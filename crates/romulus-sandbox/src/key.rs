//! What a statement is about: the key the statement-level merge and graph
//! edits use to find "the statement that defines `public.users`".
//!
//! A key is an optional action, an object kind and a qualified path:
//!
//! | statement | key |
//! |---|---|
//! | `CREATE TABLE users (…)` | `table:public.users` |
//! | `CREATE INDEX ON users (lower(email))` | `index:public.users_lower_idx` |
//! | `ALTER TABLE t ADD CHECK (x > 0)` (a second one) | `constraint:public.t.t_x_check1` |
//! | `ALTER TABLE orders ADD CONSTRAINT orders_user_id_fkey FOREIGN KEY …` | `fk:public.orders.orders_user_id_fkey` |
//! | `ALTER TABLE users ADD COLUMN bio text` | `column:public.users.bio` |
//! | `ALTER TABLE users ENABLE ROW LEVEL SECURITY` | `rls:public.users` |
//! | `CREATE POLICY own ON users …` | `policy:public.users.own` |
//! | `COMMENT ON COLUMN users.email IS …` | `comment:column:public.users.email` |
//! | `ALTER TABLE users RENAME TO members` | `rename:table:public.users` |
//! | `ALTER TABLE users ALTER COLUMN email SET NOT NULL` | `alter:column:public.users.email` |
//! | `DROP TABLE users` | `drop:table:public.users` |
//! | `SET search_path TO app` | `set:search_path` |
//! | `INSERT INTO …` (not allowed, still keyed) | `statement:insert` |
//!
//! # Identity rules
//!
//! - **Names, never positions.** A key is the object's qualified name.
//!   Unqualified names resolve against the `search_path` the text has set
//!   so far (`public` by default); `CREATE SCHEMA s CREATE TABLE t …` puts
//!   `t` in `s`.
//! - **Unnamed objects get Postgres's own name.** The keyer replays the
//!   text's names the way Postgres records them, and names an unnamed
//!   index or constraint exactly as Postgres would: `<table>_<columns>_idx`
//!   (`_key`, `_pkey`, `_excl`, `_fkey`, `_check`), the names truncated to
//!   63 bytes like `makeObjectName`, expression columns named like
//!   `FigureColname` (`lower`, `expr`…), a CHECK named after its column
//!   only when it mentions exactly one, and a number appended on conflict
//!   (`t_x_check1`, `t_x_idx2`), counting what the text created, renamed
//!   and dropped before (partitions' copies of their parent's indexes
//!   included). So a key matches the name in the compiled model, and the
//!   same object gets the same key in two texts that create the same
//!   names in the same order (an integration test checks the names
//!   against the sandbox).
//! - **One key per object.** [`crate::Statement::keys`] lists every object
//!   a statement defines or changes: a multi-command `ALTER TABLE` keys as
//!   the table, then one key per command (`column:…`, `constraint:…`,
//!   `fk:…`); `DROP a, b` keys each; `CREATE SCHEMA` keys the schema, then
//!   each element. [`crate::Statement::key`] is the first.
//! - **Path parts are quoted like SQL identifiers** when needed
//!   (`table:public."Order.Items"`), so a key string is unambiguous and
//!   [`Key::from_str`](std::str::FromStr) reads it back. Setting names and
//!   statement kinds are words, written as they are (`set:app.tenant`).
//!
//! Limits:
//!
//! - Statements that change the same object without naming a new one
//!   share a key (two `COMMENT ON COLUMN t.c`, two `ALTER COLUMN … SET
//!   DEFAULT`, `ENABLE` then `DISABLE ROW LEVEL SECURITY`):
//!   [`crate::Statement::occurrence`] tells them apart by order, which a
//!   merge must treat as "the n-th change to that object".
//! - Default names depend on what exists: the keyer only knows what the
//!   text created. An unnamed object that, on the real database, would
//!   collide with something the text does not create (a table of another
//!   migration) gets a different suffix there.
//! - Objects defined inline in `CREATE TABLE` (its constraints, serial
//!   sequences) belong to the table's key; they get names, for later
//!   defaults, but no keys of their own. [`crate::Statement::inline`] lists
//!   them, named (the merge matches an inline foreign key with one added by
//!   `ALTER TABLE` elsewhere).
//! - `ALTER TABLE … DROP CONSTRAINT x` keys as `drop:constraint:…` whatever
//!   kind `x` was (`fk:` or `constraint:` when created).

use std::{
    collections::{HashMap, HashSet},
    fmt,
    str::FromStr,
};

use pg_query::{
    NodeEnum,
    protobuf::{AlterTableCmd, AlterTableType, ConstrType, Constraint, Node, ObjectType, RangeVar},
};
use serde::{Serialize, Serializer};

use crate::names::{QualifiedName, SearchPath, quote_ident, string, strings};

/// What a statement does to its object. `Create` is written without a prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    Create,
    Alter,
    Comment,
    Rename,
    Drop,
}

impl Action {
    fn prefix(self) -> Option<&'static str> {
        match self {
            Self::Create => None,
            Self::Alter => Some("alter"),
            Self::Comment => Some("comment"),
            Self::Rename => Some("rename"),
            Self::Drop => Some("drop"),
        }
    }
}

/// The kind of object a key names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectKind {
    Schema,
    Extension,
    Table,
    View,
    MaterializedView,
    ForeignTable,
    Index,
    Sequence,
    Enum,
    /// Composite and range types (and `ALTER TYPE` on any type).
    Type,
    Domain,
    Column,
    /// A constraint other than a foreign key, on a table or a domain.
    Constraint,
    ForeignKey,
    Policy,
    /// `ENABLE` / `DISABLE ROW LEVEL SECURITY`.
    RowSecurity,
    /// `FORCE` / `NO FORCE ROW LEVEL SECURITY`.
    ForceRowSecurity,
    Trigger,
    Function,
    Rule,
    Statistics,
    Collation,
    /// `SET` / `RESET` of a setting; the path is the setting name.
    Setting,
    /// Any other statement; the path is its kind (`insert`, `do`, `grant`…).
    Statement,
}

const KINDS: &[(ObjectKind, &str)] = &[
    (ObjectKind::Schema, "schema"),
    (ObjectKind::Extension, "extension"),
    (ObjectKind::Table, "table"),
    (ObjectKind::View, "view"),
    (ObjectKind::MaterializedView, "matview"),
    (ObjectKind::ForeignTable, "foreign_table"),
    (ObjectKind::Index, "index"),
    (ObjectKind::Sequence, "sequence"),
    (ObjectKind::Enum, "enum"),
    (ObjectKind::Type, "type"),
    (ObjectKind::Domain, "domain"),
    (ObjectKind::Column, "column"),
    (ObjectKind::Constraint, "constraint"),
    (ObjectKind::ForeignKey, "fk"),
    (ObjectKind::Policy, "policy"),
    (ObjectKind::RowSecurity, "rls"),
    (ObjectKind::ForceRowSecurity, "force_rls"),
    (ObjectKind::Trigger, "trigger"),
    (ObjectKind::Function, "function"),
    (ObjectKind::Rule, "rule"),
    (ObjectKind::Statistics, "statistics"),
    (ObjectKind::Collation, "collation"),
    (ObjectKind::Setting, "set"),
    (ObjectKind::Statement, "statement"),
];

impl ObjectKind {
    pub fn as_str(self) -> &'static str {
        KINDS
            .iter()
            .find(|(kind, _)| *kind == self)
            .map_or("statement", |(_, word)| word)
    }

    fn parse(word: &str) -> Option<Self> {
        KINDS
            .iter()
            .find(|(_, w)| *w == word)
            .map(|(kind, _)| *kind)
    }
}

/// The identity of the object a statement defines or changes. See the module
/// documentation for the string form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    action: Action,
    kind: ObjectKind,
    path: Vec<String>,
}

impl Key {
    pub fn new(action: Action, kind: ObjectKind, path: impl IntoIterator<Item = String>) -> Self {
        Self {
            action,
            kind,
            path: path.into_iter().collect(),
        }
    }

    /// `table:<schema>.<name>`, the key of a `CREATE TABLE`.
    pub fn table(schema: &str, name: &str) -> Self {
        Self::new(
            Action::Create,
            ObjectKind::Table,
            [schema.to_owned(), name.to_owned()],
        )
    }

    pub fn action(&self) -> Action {
        self.action
    }

    pub fn kind(&self) -> ObjectKind {
        self.kind
    }

    /// Unquoted parts: `["public", "users", "email"]`.
    pub fn path(&self) -> &[String] {
        &self.path
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(prefix) = self.action.prefix() {
            write!(f, "{prefix}:")?;
        }
        write!(f, "{}:", self.kind.as_str())?;
        for (i, part) in self.path.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            // Statement kinds and setting names are words, not identifiers
            // (`statement:do`, not `statement:"do"`).
            if matches!(self.kind, ObjectKind::Statement | ObjectKind::Setting) {
                f.write_str(part)?;
            } else {
                f.write_str(&quote_ident(part))?;
            }
        }
        Ok(())
    }
}

impl Serialize for Key {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a statement key: {0}")]
pub struct KeyParseError(String);

impl FromStr for Key {
    type Err = KeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || KeyParseError(s.to_owned());
        let (first, rest) = s.split_once(':').ok_or_else(err)?;
        let action = match first {
            "alter" => Some(Action::Alter),
            "comment" => Some(Action::Comment),
            "rename" => Some(Action::Rename),
            "drop" => Some(Action::Drop),
            _ => None,
        };
        let (action, kind, path) = match action {
            Some(action) => {
                let (kind, path) = rest.split_once(':').ok_or_else(err)?;
                (action, kind, path)
            }
            None => (Action::Create, first, rest),
        };
        let kind = ObjectKind::parse(kind).ok_or_else(err)?;
        // Setting names (`app.tenant`) and statement kinds are written as
        // they are, not as identifiers.
        let path = if matches!(kind, ObjectKind::Setting | ObjectKind::Statement) {
            vec![path.to_owned()]
        } else {
            split_path(path).ok_or_else(err)?
        };
        Ok(Self { action, kind, path })
    }
}

/// `public."a.b"."say ""hi"""` → `["public", "a.b", "say \"hi\""]`.
fn split_path(path: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut chars = path.chars().peekable();
    loop {
        let mut part = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next()? {
                    '"' if chars.peek() == Some(&'"') => {
                        chars.next();
                        part.push('"');
                    }
                    '"' => break,
                    c => part.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == '.' {
                    break;
                }
                part.push(c);
                chars.next();
            }
        }
        parts.push(part);
        match chars.next() {
            None => return Some(parts),
            Some('.') => {}
            Some(_) => return None,
        }
    }
}

/// Postgres's `NAMEDATALEN - 1`: identifiers are at most 63 bytes.
const MAX_IDENTIFIER: usize = 63;

/// Keys statements one after the other, simulating the names Postgres
/// gives what a text leaves unnamed (see the module documentation).
#[derive(Debug, Clone, Default)]
pub(crate) struct Keyer {
    /// Relation names (tables, views, indexes, sequences, composite types)
    /// and constraint names per schema: what default names must avoid.
    namespaces: HashMap<String, Namespace>,
    /// Tables created so far, by schema and name.
    tables: HashMap<(String, String), Table>,
    /// Objects the statement being keyed defines inline (see
    /// [`Keyer::take_inline`]).
    inline: Vec<Key>,
}

#[derive(Debug, Clone, Default)]
struct Namespace {
    relations: HashSet<String>,
    constraints: HashSet<String>,
}

#[derive(Debug, Clone, Default)]
struct Table {
    /// Indexes (and the sequences of serial and identity columns), which
    /// go when the table goes.
    relations: Vec<String>,
    /// Constraints by name, with whether an index of the same name backs
    /// them (primary keys, unique and exclusion constraints).
    constraints: Vec<(String, bool)>,
    /// Indexes every partition gets a copy of: column names and label.
    templates: Vec<(Vec<String>, &'static str, bool)>,
    /// Partitions, by schema and name.
    partitions: Vec<(String, String)>,
}

impl Keyer {
    fn namespace(&mut self, schema: &str) -> &mut Namespace {
        self.namespaces.entry(schema.to_owned()).or_default()
    }

    fn table(&mut self, schema: &str, name: &str) -> &mut Table {
        self.tables
            .entry((schema.to_owned(), name.to_owned()))
            .or_default()
    }

    /// The constraints, foreign keys and sequences the last statement keyed
    /// defined inline, named as Postgres names them.
    pub(crate) fn take_inline(&mut self) -> Vec<Key> {
        std::mem::take(&mut self.inline)
    }

    /// `ChooseRelationName`: `name1_name2_label`, then `label1`, `label2`…
    /// until no relation (and, for a constraint's index, no constraint) of
    /// the schema has the name. The name is taken.
    fn choose_relation_name(
        &mut self,
        schema: &str,
        name1: &str,
        name2: Option<&str>,
        label: &str,
        constraint: bool,
    ) -> String {
        let namespace = self.namespace(schema);
        let mut pass = 0;
        loop {
            let label = if pass == 0 {
                label.to_owned()
            } else {
                format!("{label}{pass}")
            };
            let name = make_object_name(name1, name2, &label);
            let taken = namespace.relations.contains(&name)
                || (constraint && namespace.constraints.contains(&name));
            if !taken {
                namespace.relations.insert(name.clone());
                if constraint {
                    namespace.constraints.insert(name.clone());
                }
                return name;
            }
            pass += 1;
        }
    }

    /// `ChooseConstraintName`: like relations, against the schema's
    /// constraint names. The name is taken.
    fn choose_constraint_name(
        &mut self,
        schema: &str,
        name1: &str,
        name2: Option<&str>,
        label: &str,
    ) -> String {
        let namespace = self.namespace(schema);
        let mut pass = 0;
        loop {
            let label = if pass == 0 {
                label.to_owned()
            } else {
                format!("{label}{pass}")
            };
            let name = make_object_name(name1, name2, &label);
            if namespace.constraints.insert(name.clone()) {
                return name;
            }
            pass += 1;
        }
    }

    /// Every key of one statement, the main one first, resolving
    /// unqualified names against `path`; and the simulated namespaces
    /// updated with what it creates, renames or drops.
    #[allow(clippy::too_many_lines)] // one arm per statement kind
    pub(crate) fn keys(&mut self, node: &NodeEnum, path: &SearchPath) -> Vec<Key> {
        let k = |action, kind, parts: Vec<String>| Key::new(action, kind, parts);
        let rel = |rv: &Option<RangeVar>| -> Option<(String, String)> {
            rv.as_ref()
                .map(|rv| QualifiedName::from_range_var(rv).resolve(path))
        };
        let list = |nodes: &[Node]| -> Vec<String> {
            QualifiedName::from_list(nodes).map_or_else(Vec::new, |q| {
                let (schema, name) = q.resolve(path);
                vec![schema, name]
            })
        };
        let pair = |r: Option<(String, String)>| r.map_or_else(Vec::new, |(s, n)| vec![s, n]);
        match node {
            NodeEnum::CreateSchemaStmt(s) => {
                let name = if s.schemaname.is_empty() {
                    s.authrole
                        .as_ref()
                        .map(|r| r.rolename.clone())
                        .unwrap_or_default()
                } else {
                    s.schemaname.clone()
                };
                let mut keys = vec![k(Action::Create, ObjectKind::Schema, vec![name.clone()])];
                // `CREATE SCHEMA s CREATE TABLE t …` creates its elements in s.
                let inner = SearchPath::only(&name);
                for element in &s.schema_elts {
                    if let Some(element) = element.node.as_ref() {
                        keys.extend(self.keys(element, &inner));
                    }
                }
                keys
            }
            NodeEnum::CreateExtensionStmt(s) => {
                vec![k(
                    Action::Create,
                    ObjectKind::Extension,
                    vec![s.extname.clone()],
                )]
            }
            NodeEnum::CreateStmt(s) => {
                let Some((schema, name)) = rel(&s.relation) else {
                    return vec![statement_key(node)];
                };
                self.create_table(&schema, &name, s, path);
                vec![k(Action::Create, ObjectKind::Table, vec![schema, name])]
            }
            NodeEnum::CreateForeignTableStmt(s) => {
                let table = rel(&s.base_stmt.as_ref().and_then(|b| b.relation.clone()));
                if let Some((schema, name)) = &table {
                    self.namespace(schema).relations.insert(name.clone());
                }
                vec![k(Action::Create, ObjectKind::ForeignTable, pair(table))]
            }
            NodeEnum::ViewStmt(s) => {
                let view = rel(&s.view);
                if let Some((schema, name)) = &view {
                    self.namespace(schema).relations.insert(name.clone());
                }
                vec![k(Action::Create, ObjectKind::View, pair(view))]
            }
            NodeEnum::CreateTableAsStmt(s) => {
                let kind = if s.objtype() == ObjectType::ObjectMatview {
                    ObjectKind::MaterializedView
                } else {
                    ObjectKind::Table
                };
                let target = rel(&s.into.as_ref().and_then(|i| i.rel.clone()));
                if let Some((schema, name)) = &target {
                    self.namespace(schema).relations.insert(name.clone());
                    self.table(schema, name);
                }
                vec![k(Action::Create, kind, pair(target))]
            }
            NodeEnum::IndexStmt(s) => {
                let Some((schema, table)) = rel(&s.relation) else {
                    return vec![statement_key(node)];
                };
                let name = self.create_index(&schema, &table, s);
                vec![k(Action::Create, ObjectKind::Index, vec![schema, name])]
            }
            NodeEnum::CreateSeqStmt(s) => {
                let sequence = rel(&s.sequence);
                if let Some((schema, name)) = &sequence {
                    self.namespace(schema).relations.insert(name.clone());
                }
                vec![k(Action::Create, ObjectKind::Sequence, pair(sequence))]
            }
            NodeEnum::AlterSeqStmt(s) => {
                vec![k(
                    Action::Alter,
                    ObjectKind::Sequence,
                    pair(rel(&s.sequence)),
                )]
            }
            NodeEnum::CreateEnumStmt(s) => {
                vec![k(Action::Create, ObjectKind::Enum, list(&s.type_name))]
            }
            NodeEnum::AlterEnumStmt(s) => {
                vec![k(Action::Alter, ObjectKind::Enum, list(&s.type_name))]
            }
            NodeEnum::CompositeTypeStmt(s) => {
                // A composite type has a relation (pg_class) of its name.
                let typ = rel(&s.typevar);
                if let Some((schema, name)) = &typ {
                    self.namespace(schema).relations.insert(name.clone());
                }
                vec![k(Action::Create, ObjectKind::Type, pair(typ))]
            }
            NodeEnum::CreateRangeStmt(s) => {
                vec![k(Action::Create, ObjectKind::Type, list(&s.type_name))]
            }
            NodeEnum::CreateDomainStmt(s) => {
                let domain = list(&s.domainname);
                if let [schema, name] = domain.as_slice() {
                    self.domain_constraints(schema, name, &s.constraints);
                }
                vec![k(Action::Create, ObjectKind::Domain, domain)]
            }
            NodeEnum::AlterDomainStmt(s) => {
                let domain = list(&s.type_name);
                if let ([schema, name], "C", Some(def)) =
                    (domain.as_slice(), s.subtype.as_str(), s.def.as_deref())
                {
                    self.domain_constraints(schema, name, std::slice::from_ref(def));
                }
                vec![k(Action::Alter, ObjectKind::Domain, domain)]
            }
            NodeEnum::CreatePolicyStmt(s) => {
                let mut parts = pair(rel(&s.table));
                parts.push(s.policy_name.clone());
                vec![k(Action::Create, ObjectKind::Policy, parts)]
            }
            NodeEnum::AlterPolicyStmt(s) => {
                let mut parts = pair(rel(&s.table));
                parts.push(s.policy_name.clone());
                vec![k(Action::Alter, ObjectKind::Policy, parts)]
            }
            NodeEnum::CreateTrigStmt(s) => {
                let mut parts = pair(rel(&s.relation));
                parts.push(s.trigname.clone());
                vec![k(Action::Create, ObjectKind::Trigger, parts)]
            }
            NodeEnum::RuleStmt(s) => {
                let mut parts = pair(rel(&s.relation));
                parts.push(s.rulename.clone());
                vec![k(Action::Create, ObjectKind::Rule, parts)]
            }
            NodeEnum::CreateFunctionStmt(s) => {
                vec![k(Action::Create, ObjectKind::Function, list(&s.funcname))]
            }
            NodeEnum::CreateStatsStmt(s) => {
                vec![k(Action::Create, ObjectKind::Statistics, list(&s.defnames))]
            }
            NodeEnum::DefineStmt(s) => {
                let kind = match s.kind() {
                    ObjectType::ObjectCollation => ObjectKind::Collation,
                    ObjectType::ObjectType => ObjectKind::Type,
                    ObjectType::ObjectAggregate => ObjectKind::Function,
                    _ => return vec![statement_key(node)],
                };
                vec![k(Action::Create, kind, list(&s.defnames))]
            }
            NodeEnum::VariableSetStmt(s) => {
                let name = if s.name.is_empty() {
                    "all".to_owned()
                } else {
                    s.name.to_ascii_lowercase()
                };
                vec![k(Action::Create, ObjectKind::Setting, vec![name])]
            }
            NodeEnum::AlterTableStmt(s) => self.alter_table(s, path),
            NodeEnum::CommentStmt(s) => vec![
                object_key(
                    Action::Comment,
                    s.objtype(),
                    s.object.as_deref(),
                    None,
                    None,
                    path,
                )
                .unwrap_or_else(|| statement_key(node)),
            ],
            NodeEnum::RenameStmt(s) => {
                let key = object_key(
                    Action::Rename,
                    s.rename_type(),
                    s.object.as_deref(),
                    s.relation.as_ref(),
                    Some(&s.subname),
                    path,
                )
                .unwrap_or_else(|| statement_key(node));
                self.rename(s, &key);
                vec![key]
            }
            NodeEnum::AlterObjectSchemaStmt(s) => {
                let key = object_key(
                    Action::Alter,
                    s.object_type(),
                    s.object.as_deref(),
                    s.relation.as_ref(),
                    None,
                    path,
                )
                .unwrap_or_else(|| statement_key(node));
                if let (
                    ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView,
                    [schema, name],
                ) = (key.kind(), key.path())
                {
                    self.move_table(schema, name, &s.newschema);
                }
                vec![key]
            }
            NodeEnum::DropStmt(s) => {
                let keys: Vec<Key> = s
                    .objects
                    .iter()
                    .filter_map(|object| {
                        object_key(
                            Action::Drop,
                            s.remove_type(),
                            Some(object),
                            None,
                            None,
                            path,
                        )
                    })
                    .collect();
                for key in &keys {
                    self.drop(key);
                }
                if keys.is_empty() {
                    vec![statement_key(node)]
                } else {
                    keys
                }
            }
            _ => vec![statement_key(node)],
        }
    }

    /// A table, with the names Postgres gives what it leaves unnamed, in
    /// Postgres's order: sequences of serial and identity columns, CHECKs,
    /// then the indexes of primary keys, unique and exclusion constraints,
    /// then foreign keys.
    fn create_table(
        &mut self,
        schema: &str,
        name: &str,
        s: &pg_query::protobuf::CreateStmt,
        path: &SearchPath,
    ) {
        self.namespace(schema).relations.insert(name.to_owned());
        self.table(schema, name);
        let mut constraints: Vec<(Option<String>, &Constraint)> = Vec::new();
        for element in &s.table_elts {
            match element.node.as_ref() {
                Some(NodeEnum::ColumnDef(column)) => {
                    self.column_sequence(schema, name, column);
                    for c in &column.constraints {
                        if let Some(NodeEnum::Constraint(c)) = c.node.as_ref() {
                            constraints.push((Some(column.colname.clone()), c));
                        }
                    }
                }
                Some(NodeEnum::Constraint(c)) => constraints.push((None, c)),
                _ => {}
            }
        }
        for c in &s.constraints {
            if let Some(NodeEnum::Constraint(c)) = c.node.as_ref() {
                constraints.push((None, c));
            }
        }
        self.add_constraints(schema, name, &constraints);
        if let Some(parent) = s
            .inh_relations
            .first()
            .and_then(|p| match p.node.as_ref()? {
                NodeEnum::RangeVar(rv) => Some(QualifiedName::from_range_var(rv).resolve(path)),
                _ => None,
            })
            .filter(|_| s.partbound.is_some())
        {
            self.attach_partition(&parent, &(schema.to_owned(), name.to_owned()));
        }
    }

    /// The sequence a serial or identity column creates.
    fn column_sequence(
        &mut self,
        schema: &str,
        table: &str,
        column: &pg_query::protobuf::ColumnDef,
    ) {
        let serial = column.type_name.as_ref().is_some_and(|t| {
            matches!(
                strings(&t.names).as_slice(),
                [n] if matches!(n.as_str(), "serial" | "serial4" | "bigserial" | "serial8" | "smallserial" | "serial2")
            )
        });
        let identity = column.constraints.iter().any(|c| {
            matches!(c.node.as_ref(), Some(NodeEnum::Constraint(c)) if c.contype() == ConstrType::ConstrIdentity)
        });
        if serial || identity {
            let sequence =
                self.choose_relation_name(schema, table, Some(&column.colname), "seq", false);
            self.inline.push(Key::new(
                Action::Create,
                ObjectKind::Sequence,
                [schema.to_owned(), sequence.clone()],
            ));
            self.table(schema, table).relations.push(sequence);
        }
    }

    /// Name the constraints of a table (inline or added), in Postgres's
    /// order. Returns the name of each, in the order given.
    fn add_constraints(
        &mut self,
        schema: &str,
        table: &str,
        constraints: &[(Option<String>, &Constraint)],
    ) -> Vec<Option<String>> {
        let mut names: Vec<Option<String>> = vec![None; constraints.len()];
        let explicit = |c: &Constraint| (!c.conname.is_empty()).then(|| c.conname.clone());
        // Explicit names first: they are taken whatever the order.
        for (i, (_, c)) in constraints.iter().enumerate() {
            if let Some(name) = explicit(c) {
                let backed = matches!(
                    c.contype(),
                    ConstrType::ConstrPrimary
                        | ConstrType::ConstrUnique
                        | ConstrType::ConstrExclusion
                );
                let namespace = self.namespace(schema);
                namespace.constraints.insert(name.clone());
                if backed {
                    namespace.relations.insert(name.clone());
                }
                self.table(schema, table)
                    .constraints
                    .push((name.clone(), backed));
                names[i] = Some(name);
            }
        }
        let phases: [&[ConstrType]; 3] = [
            &[ConstrType::ConstrCheck],
            &[
                ConstrType::ConstrPrimary,
                ConstrType::ConstrUnique,
                ConstrType::ConstrExclusion,
            ],
            &[ConstrType::ConstrForeign],
        ];
        for phase in phases {
            for (i, (column, c)) in constraints.iter().enumerate() {
                if names[i].is_some() || !phase.contains(&c.contype()) {
                    continue;
                }
                let column = column.as_deref();
                let name = match c.contype() {
                    ConstrType::ConstrCheck => {
                        let single = column.map(str::to_owned).or_else(|| {
                            let columns =
                                c.raw_expr.as_deref().map(column_refs).unwrap_or_default();
                            (columns.len() == 1).then(|| columns[0].clone())
                        });
                        self.choose_constraint_name(schema, table, single.as_deref(), "check")
                    }
                    ConstrType::ConstrForeign => {
                        let columns = strings(&c.fk_attrs);
                        let columns = if columns.is_empty() {
                            column.map(|c| vec![c.to_owned()]).unwrap_or_default()
                        } else {
                            columns
                        };
                        let addition = name_addition(&columns);
                        self.choose_constraint_name(schema, table, Some(&addition), "fkey")
                    }
                    kind => {
                        let (columns, label) = match kind {
                            ConstrType::ConstrPrimary => (Vec::new(), "pkey"),
                            ConstrType::ConstrExclusion => (exclusion_columns(c), "excl"),
                            _ => {
                                let keys = strings(&c.keys);
                                let keys = if keys.is_empty() {
                                    column.map(|c| vec![c.to_owned()]).unwrap_or_default()
                                } else {
                                    keys
                                };
                                let mut all = keys;
                                all.extend(strings(&c.including));
                                (all, "key")
                            }
                        };
                        let addition = (!columns.is_empty()).then(|| name_addition(&columns));
                        let name = self.choose_relation_name(
                            schema,
                            table,
                            addition.as_deref(),
                            label,
                            true,
                        );
                        self.table(schema, table)
                            .templates
                            .push((columns, label, true));
                        name
                    }
                };
                let backed = !matches!(
                    c.contype(),
                    ConstrType::ConstrCheck | ConstrType::ConstrForeign
                );
                self.table(schema, table)
                    .constraints
                    .push((name.clone(), backed));
                names[i] = Some(name);
            }
        }
        self.record_inline(schema, table, constraints, &names);
        names
    }

    /// Keys for the constraints just named, in [`Keyer::take_inline`].
    fn record_inline(
        &mut self,
        schema: &str,
        table: &str,
        constraints: &[(Option<String>, &Constraint)],
        names: &[Option<String>],
    ) {
        for ((_, c), name) in constraints.iter().zip(names) {
            if let Some(name) = name {
                let kind = if c.contype() == ConstrType::ConstrForeign {
                    ObjectKind::ForeignKey
                } else {
                    ObjectKind::Constraint
                };
                self.inline.push(Key::new(
                    Action::Create,
                    kind,
                    [schema.to_owned(), table.to_owned(), name.clone()],
                ));
            }
        }
    }

    /// `CREATE DOMAIN d … CHECK …` names its checks `d_check`.
    fn domain_constraints(&mut self, schema: &str, domain: &str, constraints: &[Node]) {
        for c in constraints {
            let Some(NodeEnum::Constraint(c)) = c.node.as_ref() else {
                continue;
            };
            if c.contype() != ConstrType::ConstrCheck {
                continue;
            }
            if c.conname.is_empty() {
                self.choose_constraint_name(schema, domain, None, "check");
            } else {
                self.namespace(schema).constraints.insert(c.conname.clone());
            }
        }
    }

    /// An index's name, chosen like `ChooseIndexName` when not given; and
    /// the copy every partition of the table gets.
    fn create_index(
        &mut self,
        schema: &str,
        table: &str,
        s: &pg_query::protobuf::IndexStmt,
    ) -> String {
        let columns = index_column_names(s.index_params.iter().chain(&s.index_including_params));
        let label = if s.primary {
            "pkey"
        } else if !s.exclude_op_names.is_empty() {
            "excl"
        } else {
            "idx"
        };
        let name = if s.idxname.is_empty() {
            let addition = (label != "pkey").then(|| name_addition(&columns));
            self.choose_relation_name(schema, table, addition.as_deref(), label, false)
        } else {
            self.namespace(schema).relations.insert(s.idxname.clone());
            s.idxname.clone()
        };
        let info = self.table(schema, table);
        info.relations.push(name.clone());
        info.templates.push((columns.clone(), label, false));
        let partitions = info.partitions.clone();
        for (child_schema, child) in partitions {
            self.copy_index(&child_schema, &child, &columns, label, false);
        }
        name
    }

    /// The copy of a partitioned table's index a partition gets, down its
    /// own partitions.
    fn copy_index(
        &mut self,
        schema: &str,
        table: &str,
        columns: &[String],
        label: &'static str,
        constraint: bool,
    ) {
        let addition = (label != "pkey").then(|| name_addition(columns));
        let name = self.choose_relation_name(schema, table, addition.as_deref(), label, constraint);
        let info = self.table(schema, table);
        info.relations.push(name);
        info.templates.push((columns.to_vec(), label, constraint));
        for (child_schema, child) in info.partitions.clone() {
            self.copy_index(&child_schema, &child, columns, label, constraint);
        }
    }

    fn attach_partition(&mut self, parent: &(String, String), child: &(String, String)) {
        let templates = self.table(&parent.0, &parent.1).templates.clone();
        self.table(&parent.0, &parent.1)
            .partitions
            .push(child.clone());
        for (columns, label, constraint) in templates {
            self.copy_index(&child.0, &child.1, &columns, label, constraint);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn alter_table(
        &mut self,
        s: &pg_query::protobuf::AlterTableStmt,
        path: &SearchPath,
    ) -> Vec<Key> {
        let Some((schema, name)) = s
            .relation
            .as_ref()
            .map(|rv| QualifiedName::from_range_var(rv).resolve(path))
        else {
            return Vec::new();
        };
        let object_kind = match s.objtype() {
            ObjectType::ObjectView => ObjectKind::View,
            ObjectType::ObjectMatview => ObjectKind::MaterializedView,
            ObjectType::ObjectIndex => ObjectKind::Index,
            ObjectType::ObjectSequence => ObjectKind::Sequence,
            ObjectType::ObjectForeignTable => ObjectKind::ForeignTable,
            ObjectType::ObjectType => ObjectKind::Type,
            _ => ObjectKind::Table,
        };
        let table = vec![schema.clone(), name.clone()];
        let whole = Key::new(Action::Alter, object_kind, table.clone());
        let with = |sub: &str| {
            let mut parts = table.clone();
            parts.push(sub.to_owned());
            parts
        };
        let mut keys = Vec::new();
        for cmd in &s.cmds {
            let Some(NodeEnum::AlterTableCmd(cmd)) = cmd.node.as_ref() else {
                continue;
            };
            let key = match (cmd.subtype(), def(cmd)) {
                (AlterTableType::AtAddColumn, Some(NodeEnum::ColumnDef(column))) => {
                    self.column_sequence(&schema, &name, column);
                    let constraints: Vec<(Option<String>, &Constraint)> = column
                        .constraints
                        .iter()
                        .filter_map(|c| match c.node.as_ref()? {
                            NodeEnum::Constraint(c) => Some((Some(column.colname.clone()), &**c)),
                            _ => None,
                        })
                        .collect();
                    self.add_constraints(&schema, &name, &constraints);
                    Key::new(Action::Create, ObjectKind::Column, with(&column.colname))
                }
                (AlterTableType::AtAddConstraint, Some(NodeEnum::Constraint(c))) => {
                    let constraint_name = self
                        .add_constraints(&schema, &name, &[(None, c)])
                        .pop()
                        .flatten()
                        .unwrap_or_default();
                    let kind = if c.contype() == ConstrType::ConstrForeign {
                        ObjectKind::ForeignKey
                    } else {
                        ObjectKind::Constraint
                    };
                    Key::new(Action::Create, kind, with(&constraint_name))
                }
                (AlterTableType::AtAttachPartition, Some(NodeEnum::PartitionCmd(p))) => {
                    if let Some(child) = p.name.as_ref() {
                        let child = QualifiedName::from_range_var(child).resolve(path);
                        self.attach_partition(&(schema.clone(), name.clone()), &child);
                    }
                    whole.clone()
                }
                (AlterTableType::AtEnableRowSecurity | AlterTableType::AtDisableRowSecurity, _) => {
                    Key::new(Action::Create, ObjectKind::RowSecurity, table.clone())
                }
                (AlterTableType::AtForceRowSecurity | AlterTableType::AtNoForceRowSecurity, _) => {
                    Key::new(Action::Create, ObjectKind::ForceRowSecurity, table.clone())
                }
                (AlterTableType::AtDropColumn, _) => {
                    Key::new(Action::Drop, ObjectKind::Column, with(&cmd.name))
                }
                (AlterTableType::AtDropConstraint, _) => {
                    self.drop_constraint(&schema, &name, &cmd.name);
                    Key::new(Action::Drop, ObjectKind::Constraint, with(&cmd.name))
                }
                (subtype, _) if !cmd.name.is_empty() && is_column_command(subtype) => {
                    Key::new(Action::Alter, ObjectKind::Column, with(&cmd.name))
                }
                _ => whole.clone(),
            };
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        // One command: its own key. Several: the table's, then each one's.
        match keys.len() {
            0 => vec![whole],
            1 => keys,
            _ => {
                keys.retain(|k| *k != whole);
                keys.insert(0, whole);
                keys
            }
        }
    }

    fn drop_constraint(&mut self, schema: &str, table: &str, constraint: &str) {
        let info = self.table(schema, table);
        let backed = info
            .constraints
            .iter()
            .find(|(n, _)| n == constraint)
            .is_some_and(|(_, backed)| *backed);
        info.constraints.retain(|(n, _)| n != constraint);
        let namespace = self.namespace(schema);
        namespace.constraints.remove(constraint);
        if backed {
            namespace.relations.remove(constraint);
        }
    }

    fn drop(&mut self, key: &Key) {
        match (key.kind(), key.path()) {
            (
                ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::MaterializedView
                | ObjectKind::ForeignTable,
                [schema, name],
            ) => {
                self.drop_table(schema, name);
            }
            (ObjectKind::Index | ObjectKind::Sequence | ObjectKind::Type, [schema, name]) => {
                self.namespace(schema).relations.remove(name);
            }
            _ => {}
        }
    }

    fn drop_table(&mut self, schema: &str, name: &str) {
        let info = self
            .tables
            .remove(&(schema.to_owned(), name.to_owned()))
            .unwrap_or_default();
        let namespace = self.namespace(schema);
        namespace.relations.remove(name);
        for relation in &info.relations {
            namespace.relations.remove(relation);
        }
        for (constraint, _) in &info.constraints {
            namespace.constraints.remove(constraint);
        }
        for (child_schema, child) in info.partitions {
            self.drop_table(&child_schema, &child);
        }
    }

    fn rename(&mut self, s: &pg_query::protobuf::RenameStmt, key: &Key) {
        use ObjectType as O;
        match (s.rename_type(), key.path()) {
            (
                O::ObjectTable | O::ObjectView | O::ObjectMatview | O::ObjectForeignTable,
                [schema, name],
            ) => {
                let namespace = self.namespace(schema);
                namespace.relations.remove(name);
                namespace.relations.insert(s.newname.clone());
                if let Some(info) = self.tables.remove(&(schema.clone(), name.clone())) {
                    self.tables
                        .insert((schema.clone(), s.newname.clone()), info);
                }
            }
            (O::ObjectIndex | O::ObjectSequence, [schema, name]) => {
                let namespace = self.namespace(schema);
                if namespace.relations.remove(name) {
                    namespace.relations.insert(s.newname.clone());
                }
                // Renaming a constraint's index renames the constraint.
                if namespace.constraints.remove(name) {
                    namespace.constraints.insert(s.newname.clone());
                }
            }
            (O::ObjectTabconstraint, [schema, table, constraint]) => {
                let info = self.table(schema, table);
                let backed = info
                    .constraints
                    .iter_mut()
                    .find(|(n, _)| n == constraint)
                    .is_some_and(|(n, backed)| {
                        n.clone_from(&s.newname);
                        *backed
                    });
                let namespace = self.namespace(schema);
                namespace.constraints.remove(constraint);
                namespace.constraints.insert(s.newname.clone());
                if backed {
                    namespace.relations.remove(constraint);
                    namespace.relations.insert(s.newname.clone());
                }
            }
            _ => {}
        }
    }

    /// `ALTER TABLE … SET SCHEMA`: the table, its indexes, sequences and
    /// constraints move.
    fn move_table(&mut self, schema: &str, name: &str, to: &str) {
        let info = self
            .tables
            .remove(&(schema.to_owned(), name.to_owned()))
            .unwrap_or_default();
        let from = self.namespace(schema);
        from.relations.remove(name);
        for relation in &info.relations {
            from.relations.remove(relation);
        }
        for (constraint, _) in &info.constraints {
            from.constraints.remove(constraint);
        }
        let target = self.namespace(to);
        target.relations.insert(name.to_owned());
        target.relations.extend(info.relations.iter().cloned());
        target
            .constraints
            .extend(info.constraints.iter().map(|(n, _)| n.clone()));
        self.tables.insert((to.to_owned(), name.to_owned()), info);
    }
}

/// `makeObjectName`: `name1[_name2][_label]`, truncating the longer of
/// `name1` and `name2` (by whole characters) to fit 63 bytes.
fn make_object_name(name1: &str, name2: Option<&str>, label: &str) -> String {
    let overhead =
        usize::from(name2.is_some()) + if label.is_empty() { 0 } else { label.len() + 1 };
    let available = MAX_IDENTIFIER.saturating_sub(overhead);
    let (mut n1, mut n2) = (name1.len(), name2.map_or(0, str::len));
    while n1 + n2 > available {
        if n1 > n2 {
            n1 -= 1;
        } else {
            n2 -= 1;
        }
    }
    let clip = |s: &str, n: usize| crate::source::floor_char_boundary(s, n).min(s.len());
    let mut name = name1[..clip(name1, n1)].to_owned();
    if let Some(name2) = name2 {
        name.push('_');
        name.push_str(&name2[..clip(name2, n2)]);
    }
    if !label.is_empty() {
        name.push('_');
        name.push_str(label);
    }
    name
}

/// `ChooseIndexNameAddition`: names joined by `_`, stopping once past
/// the identifier limit.
fn name_addition(names: &[String]) -> String {
    let mut addition = String::new();
    for name in names {
        if !addition.is_empty() {
            addition.push('_');
        }
        let clipped = &name[..crate::source::floor_char_boundary(name, MAX_IDENTIFIER)];
        addition.push_str(clipped);
        if addition.len() > MAX_IDENTIFIER {
            break;
        }
    }
    addition
}

/// `ChooseIndexColumnNames`: each element's name (the column, or what an
/// expression is called, else `expr`), made unique with a number.
fn index_column_names<'a>(params: impl Iterator<Item = &'a Node>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for param in params {
        let Some(NodeEnum::IndexElem(e)) = param.node.as_ref() else {
            continue;
        };
        let original = if !e.indexcolname.is_empty() {
            e.indexcolname.clone()
        } else if !e.name.is_empty() {
            e.name.clone()
        } else {
            e.expr
                .as_deref()
                .and_then(figure_column_name)
                .unwrap_or_else(|| "expr".to_owned())
        };
        let mut name = original.clone();
        let mut i = 1;
        while names.contains(&name) {
            let suffix = i.to_string();
            let keep = crate::source::floor_char_boundary(&original, MAX_IDENTIFIER - suffix.len());
            name = format!("{}{suffix}", &original[..keep]);
            i += 1;
        }
        names.push(name);
    }
    names
}

/// `FigureColname` for an index expression: a function's name, a column's,
/// a cast's subject (or its type), and the keyword names of a few forms.
fn figure_column_name(node: &Node) -> Option<String> {
    use pg_query::protobuf::{MinMaxOp, SqlValueFunctionOp as Svf};
    match node.node.as_ref()? {
        NodeEnum::FuncCall(f) => strings(&f.funcname).pop(),
        NodeEnum::ColumnRef(c) => strings(&c.fields).pop(),
        NodeEnum::AIndirection(i) => i
            .indirection
            .iter()
            .rev()
            .find_map(|n| string(n).map(str::to_owned))
            .or_else(|| i.arg.as_deref().and_then(figure_column_name)),
        NodeEnum::TypeCast(c) => c
            .arg
            .as_deref()
            .and_then(figure_column_name)
            .or_else(|| c.type_name.as_ref().and_then(|t| strings(&t.names).pop())),
        NodeEnum::CollateClause(c) => c.arg.as_deref().and_then(figure_column_name),
        NodeEnum::CaseExpr(_) => Some("case".to_owned()),
        NodeEnum::AArrayExpr(_) => Some("array".to_owned()),
        NodeEnum::RowExpr(_) => Some("row".to_owned()),
        NodeEnum::CoalesceExpr(_) => Some("coalesce".to_owned()),
        NodeEnum::NullIfExpr(_) => Some("nullif".to_owned()),
        NodeEnum::MinMaxExpr(m) => Some(
            if m.op() == MinMaxOp::IsGreatest {
                "greatest"
            } else {
                "least"
            }
            .to_owned(),
        ),
        NodeEnum::SqlvalueFunction(f) => Some(
            match f.op() {
                Svf::SvfopCurrentDate => "current_date",
                Svf::SvfopCurrentTime | Svf::SvfopCurrentTimeN => "current_time",
                Svf::SvfopCurrentTimestamp | Svf::SvfopCurrentTimestampN => "current_timestamp",
                Svf::SvfopLocaltime | Svf::SvfopLocaltimeN => "localtime",
                Svf::SvfopLocaltimestamp | Svf::SvfopLocaltimestampN => "localtimestamp",
                Svf::SvfopCurrentRole => "current_role",
                Svf::SvfopCurrentUser => "current_user",
                Svf::SvfopUser => "user",
                Svf::SvfopSessionUser => "session_user",
                Svf::SvfopCurrentCatalog => "current_catalog",
                Svf::SvfopCurrentSchema => "current_schema",
                Svf::SqlvalueFunctionOpUndefined => return None,
            }
            .to_owned(),
        ),
        _ => None,
    }
}

/// The columns an exclusion constraint's elements name.
fn exclusion_columns(c: &Constraint) -> Vec<String> {
    let elements: Vec<&Node> = c
        .exclusions
        .iter()
        .filter_map(|pair| match pair.node.as_ref()? {
            NodeEnum::List(l) => l.items.first(),
            _ => None,
        })
        .collect();
    index_column_names(elements.into_iter())
}

/// The distinct columns an expression mentions, in order.
pub(crate) fn column_refs(node: &Node) -> Vec<String> {
    let Ok(value) = serde_json::to_value(node) else {
        return Vec::new();
    };
    let mut columns = Vec::new();
    crate::walk::visit(&value, &mut |kind, inner| {
        if kind == "ColumnRef"
            && let Some(name) = inner["fields"]
                .as_array()
                .and_then(|fields| fields.last())
                .and_then(|f| f["node"]["String"]["sval"].as_str())
            && !columns.iter().any(|c: &String| c == name)
        {
            columns.push(name.to_owned());
        }
    });
    columns
}

fn def(cmd: &AlterTableCmd) -> Option<&NodeEnum> {
    cmd.def.as_ref()?.node.as_ref()
}

fn is_column_command(subtype: AlterTableType) -> bool {
    use AlterTableType as T;
    matches!(
        subtype,
        T::AtColumnDefault
            | T::AtDropNotNull
            | T::AtSetNotNull
            | T::AtSetExpression
            | T::AtDropExpression
            | T::AtSetStatistics
            | T::AtSetOptions
            | T::AtResetOptions
            | T::AtSetStorage
            | T::AtSetCompression
            | T::AtAlterColumnType
            | T::AtAddIdentity
            | T::AtSetIdentity
            | T::AtDropIdentity
    )
}

/// Keys of `COMMENT ON`, `ALTER … RENAME`, `ALTER … SET SCHEMA` and `DROP`,
/// whose object is described by an object type and a name list (or a
/// relation plus a sub-name).
fn object_key(
    action: Action,
    objtype: ObjectType,
    object: Option<&Node>,
    relation: Option<&RangeVar>,
    subname: Option<&str>,
    path: &SearchPath,
) -> Option<Key> {
    use ObjectType as O;
    let kind = match objtype {
        O::ObjectTable => ObjectKind::Table,
        O::ObjectView => ObjectKind::View,
        O::ObjectMatview => ObjectKind::MaterializedView,
        O::ObjectForeignTable => ObjectKind::ForeignTable,
        O::ObjectIndex => ObjectKind::Index,
        O::ObjectSequence => ObjectKind::Sequence,
        O::ObjectType => ObjectKind::Type,
        O::ObjectDomain => ObjectKind::Domain,
        O::ObjectColumn | O::ObjectAttribute => ObjectKind::Column,
        O::ObjectTabconstraint | O::ObjectDomconstraint => ObjectKind::Constraint,
        O::ObjectPolicy => ObjectKind::Policy,
        O::ObjectTrigger => ObjectKind::Trigger,
        O::ObjectRule => ObjectKind::Rule,
        O::ObjectSchema => ObjectKind::Schema,
        O::ObjectExtension => ObjectKind::Extension,
        O::ObjectFunction | O::ObjectProcedure | O::ObjectRoutine | O::ObjectAggregate => {
            ObjectKind::Function
        }
        O::ObjectStatisticExt => ObjectKind::Statistics,
        O::ObjectCollation => ObjectKind::Collation,
        _ => return None,
    };
    let resolve = |q: QualifiedName| {
        let (schema, name) = q.resolve(path);
        vec![schema, name]
    };
    // A relation plus a sub-name: `ALTER TABLE t RENAME COLUMN c …`.
    if let Some(rv) = relation {
        let mut parts = resolve(QualifiedName::from_range_var(rv));
        if let Some(sub) = subname.filter(|s| !s.is_empty()) {
            parts.push(sub.to_owned());
        }
        return Some(Key::new(action, kind, parts));
    }
    // A bare name: schemas, extensions (and `ALTER SCHEMA s RENAME`).
    if matches!(kind, ObjectKind::Schema | ObjectKind::Extension) {
        let name = object
            .and_then(string)
            .map(str::to_owned)
            .or_else(|| subname.map(str::to_owned))?;
        return Some(Key::new(action, kind, [name]));
    }
    let parts = match object?.node.as_ref()? {
        NodeEnum::List(list) => {
            let items = &list.items;
            match objtype {
                // [schema?, table, column] / [schema?, table, name]
                O::ObjectColumn
                | O::ObjectTabconstraint
                | O::ObjectPolicy
                | O::ObjectTrigger
                | O::ObjectRule => {
                    let names = strings(items);
                    let (object_name, table) = names.split_last()?;
                    let mut parts = resolve(qualified(table)?);
                    parts.push(object_name.clone());
                    parts
                }
                // `COMMENT ON CONSTRAINT c ON DOMAIN d`: [type name, constraint
                // name]; `ALTER DOMAIN d RENAME CONSTRAINT c`: the domain's
                // name list, the constraint in `subname` (added below).
                O::ObjectDomconstraint => match items.first()?.node.as_ref()? {
                    NodeEnum::TypeName(t) => {
                        let mut parts = resolve(QualifiedName::from_list(&t.names)?);
                        parts.push(string(items.get(1)?)?.to_owned());
                        parts
                    }
                    _ => resolve(QualifiedName::from_list(items)?),
                },
                _ => resolve(QualifiedName::from_list(items)?),
            }
        }
        NodeEnum::TypeName(t) => resolve(QualifiedName::from_list(&t.names)?),
        NodeEnum::ObjectWithArgs(f) => resolve(QualifiedName::from_list(&f.objname)?),
        NodeEnum::String(s) => resolve(QualifiedName {
            schema: None,
            name: s.sval.clone(),
        }),
        _ => return None,
    };
    let mut parts = parts;
    if let Some(sub) = subname.filter(|s| !s.is_empty())
        && objtype == O::ObjectDomconstraint
    {
        parts.push(sub.to_owned());
    }
    Some(Key::new(action, kind, parts))
}

fn qualified(names: &[String]) -> Option<QualifiedName> {
    let (name, rest) = names.split_last()?;
    Some(QualifiedName {
        schema: rest.last().cloned(),
        name: name.clone(),
    })
}

/// `statement:<kind>` for statements that define nothing keyable.
fn statement_key(node: &NodeEnum) -> Key {
    Key::new(
        Action::Create,
        ObjectKind::Statement,
        [statement_kind(node)],
    )
}

/// `InsertStmt` → `insert`, `CreateTrigStmt` → `create_trig`.
pub(crate) fn statement_kind(node: &NodeEnum) -> String {
    let name = serde_json::to_value(node)
        .ok()
        .and_then(|v| v.as_object().and_then(|o| o.keys().next().cloned()))
        .unwrap_or_else(|| "Unknown".to_owned());
    let name = name.strip_suffix("Stmt").unwrap_or(&name);
    let mut snake = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                snake.push('_');
            }
            snake.push(c.to_ascii_lowercase());
        } else {
            snake.push(c);
        }
    }
    snake
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{Action, Key, ObjectKind};
    use crate::parse;

    fn keys(sql: &str) -> Vec<String> {
        let parsed = parse(sql);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        parsed
            .statements
            .iter()
            .map(|s| s.key.to_string())
            .collect()
    }

    #[test]
    fn inline_objects_are_listed_with_the_names_postgres_gives_them() {
        let parsed = parse(
            "CREATE TABLE users (id bigint PRIMARY KEY);
             CREATE TABLE orders (
                 id serial PRIMARY KEY,
                 user_id bigint REFERENCES users,
                 CHECK (id > 0)
             );
             ALTER TABLE orders ADD COLUMN code text UNIQUE;
             ALTER TABLE orders ADD CONSTRAINT own FOREIGN KEY (id) REFERENCES users;",
        );
        let inline = |i: usize| -> Vec<String> {
            parsed.statements[i]
                .inline
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        assert_eq!(inline(0), ["constraint:public.users.users_pkey"]);
        assert_eq!(
            inline(1),
            [
                "sequence:public.orders_id_seq",
                "constraint:public.orders.orders_pkey",
                "fk:public.orders.orders_user_id_fkey",
                "constraint:public.orders.orders_id_check",
            ]
        );
        assert_eq!(inline(2), ["constraint:public.orders.orders_code_key"]);
        // Its own key, not an inline one.
        assert_eq!(inline(3), Vec::<String>::new());
    }

    #[test]
    fn creates_are_keyed_by_the_object_they_define() {
        assert_eq!(
            keys(
                "CREATE SCHEMA app;
                 CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\";
                 CREATE TABLE users (id int);
                 CREATE TABLE app.orders (id int) PARTITION BY RANGE (id);
                 CREATE TABLE app.orders_1 PARTITION OF app.orders FOR VALUES FROM (1) TO (10);
                 CREATE VIEW v AS SELECT 1;
                 CREATE MATERIALIZED VIEW app.mv AS SELECT 1;
                 CREATE TYPE mood AS ENUM ('ok');
                 CREATE TYPE app.address AS (street text);
                 CREATE TYPE span AS RANGE (subtype = int4);
                 CREATE DOMAIN email AS text;
                 CREATE SEQUENCE app.seq;
                 CREATE INDEX users_email_idx ON users (email);
                 CREATE POLICY own ON app.orders USING (true);
                 CREATE STATISTICS app.st ON id, x FROM app.orders;"
            ),
            [
                "schema:app",
                "extension:\"uuid-ossp\"",
                "table:public.users",
                "table:app.orders",
                "table:app.orders_1",
                "view:public.v",
                "matview:app.mv",
                "enum:public.mood",
                "type:app.address",
                "type:public.span",
                "domain:public.email",
                "sequence:app.seq",
                "index:public.users_email_idx",
                "policy:app.orders.own",
                "statistics:app.st",
            ]
        );
    }

    #[test]
    fn unnamed_indexes_and_constraints_get_postgres_like_names() {
        assert_eq!(
            keys(
                "CREATE INDEX ON users (lower(email));
                 CREATE UNIQUE INDEX ON app.users (org_id, email);
                 ALTER TABLE orders ADD FOREIGN KEY (user_id) REFERENCES users;
                 ALTER TABLE orders ADD CONSTRAINT orders_user_fk FOREIGN KEY (user_id) REFERENCES users;
                 ALTER TABLE orders ADD PRIMARY KEY (id);
                 ALTER TABLE orders ADD UNIQUE (code);
                 ALTER TABLE orders ADD CHECK (total > 0);
                 ALTER TABLE orders ADD EXCLUDE USING gist (during WITH &&);"
            ),
            [
                "index:public.users_lower_idx",
                "index:app.users_org_id_email_idx",
                "fk:public.orders.orders_user_id_fkey",
                "fk:public.orders.orders_user_fk",
                "constraint:public.orders.orders_pkey",
                "constraint:public.orders.orders_code_key",
                "constraint:public.orders.orders_total_check",
                "constraint:public.orders.orders_during_excl",
            ]
        );
    }

    #[test]
    fn alter_table_is_keyed_by_its_single_subcommand() {
        assert_eq!(
            keys(
                "ALTER TABLE users ADD COLUMN bio text;
                 ALTER TABLE users ENABLE ROW LEVEL SECURITY;
                 ALTER TABLE users DISABLE ROW LEVEL SECURITY;
                 ALTER TABLE users FORCE ROW LEVEL SECURITY;
                 ALTER TABLE users ALTER COLUMN email SET NOT NULL;
                 ALTER TABLE users ALTER COLUMN email SET DEFAULT '';
                 ALTER TABLE users DROP COLUMN bio;
                 ALTER TABLE users DROP CONSTRAINT users_pkey;
                 ALTER TABLE users ADD COLUMN a int, ADD COLUMN b int;
                 ALTER TABLE ONLY users SET UNLOGGED;
                 ALTER VIEW v ALTER COLUMN x SET DEFAULT 1;"
            ),
            [
                "column:public.users.bio",
                "rls:public.users",
                "rls:public.users",
                "force_rls:public.users",
                "alter:column:public.users.email",
                "alter:column:public.users.email",
                "drop:column:public.users.bio",
                "drop:constraint:public.users.users_pkey",
                "alter:table:public.users",
                "alter:table:public.users",
                "alter:column:public.v.x",
            ]
        );
    }

    #[test]
    fn comments_renames_and_drops_name_their_target() {
        assert_eq!(
            keys(
                "COMMENT ON TABLE users IS 'x';
                 COMMENT ON COLUMN app.users.email IS 'x';
                 COMMENT ON COLUMN users.email IS 'x';
                 COMMENT ON CONSTRAINT users_pkey ON users IS 'x';
                 COMMENT ON CONSTRAINT positive ON DOMAIN app.amount IS 'x';
                 COMMENT ON TYPE mood IS 'x';
                 COMMENT ON DOMAIN app.amount IS 'x';
                 COMMENT ON SCHEMA app IS 'x';
                 COMMENT ON POLICY own ON users IS 'x';
                 COMMENT ON MATERIALIZED VIEW app.mv IS 'x';
                 COMMENT ON INDEX users_email_idx IS 'x';
                 COMMENT ON EXTENSION citext IS 'x';
                 ALTER TABLE users RENAME TO members;
                 ALTER TABLE users RENAME COLUMN email TO mail;
                 ALTER TABLE users RENAME CONSTRAINT a TO b;
                 ALTER INDEX users_email_idx RENAME TO x;
                 ALTER TYPE mood RENAME TO feeling;
                 ALTER SCHEMA app RENAME TO core;
                 ALTER POLICY own ON users RENAME TO mine;
                 ALTER TABLE users SET SCHEMA app;
                 DROP TABLE users, orders CASCADE;
                 DROP TYPE IF EXISTS app.mood;
                 DROP SCHEMA app;
                 DROP POLICY own ON users;
                 DROP INDEX app.users_email_idx;"
            ),
            [
                "comment:table:public.users",
                "comment:column:app.users.email",
                "comment:column:public.users.email",
                "comment:constraint:public.users.users_pkey",
                "comment:constraint:app.amount.positive",
                "comment:type:public.mood",
                "comment:domain:app.amount",
                "comment:schema:app",
                "comment:policy:public.users.own",
                "comment:matview:app.mv",
                "comment:index:public.users_email_idx",
                "comment:extension:citext",
                "rename:table:public.users",
                "rename:column:public.users.email",
                "rename:constraint:public.users.a",
                "rename:index:public.users_email_idx",
                "rename:type:public.mood",
                "rename:schema:app",
                "rename:policy:public.users.own",
                "alter:table:public.users",
                "drop:table:public.users",
                "drop:type:app.mood",
                "drop:schema:app",
                "drop:policy:public.users.own",
                "drop:index:app.users_email_idx",
            ]
        );
    }

    #[test]
    fn unqualified_names_follow_the_search_path_set_before_them() {
        assert_eq!(
            keys(
                "CREATE TABLE a (id int);
                 SET search_path TO billing, public;
                 CREATE TABLE b (id int);
                 COMMENT ON TABLE b IS 'x';
                 RESET search_path;
                 CREATE TABLE c (id int);"
            ),
            [
                "table:public.a",
                "set:search_path",
                "table:billing.b",
                "comment:table:billing.b",
                "set:search_path",
                "table:public.c",
            ]
        );
    }

    #[test]
    fn other_statements_are_keyed_by_their_kind() {
        assert_eq!(
            keys(
                "INSERT INTO t VALUES (1);
                 DO $$ BEGIN END $$;
                 GRANT SELECT ON t TO PUBLIC;
                 CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1';
                 CREATE TRIGGER trg BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();"
            ),
            [
                "statement:insert",
                "statement:do",
                "statement:grant",
                "function:public.f",
                "trigger:public.t.trg",
            ]
        );
    }

    fn all_keys(sql: &str) -> Vec<Vec<String>> {
        let parsed = parse(sql);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        parsed
            .statements
            .iter()
            .map(|s| s.keys.iter().map(ToString::to_string).collect())
            .collect()
    }

    #[test]
    fn unnamed_objects_get_postgres_s_names_with_its_numeric_suffixes() {
        assert_eq!(
            keys(
                "CREATE TABLE t (x int CHECK (x > 0), y int, z int);
                 ALTER TABLE t ADD CHECK (x < 9);
                 ALTER TABLE t ADD CHECK (x <> 5);
                 ALTER TABLE t ADD CHECK (x > y);
                 ALTER TABLE t ADD CHECK (y > z);
                 CREATE INDEX ON t (x);
                 CREATE INDEX ON t USING hash (x);
                 CREATE UNIQUE INDEX ON t (lower(x::text), lower(y::text));
                 CREATE INDEX ON t ((x || y));
                 CREATE INDEX t_y_key ON t (y);
                 ALTER TABLE t ADD UNIQUE (y);
                 ALTER TABLE t ADD FOREIGN KEY (x) REFERENCES u;
                 ALTER TABLE t ADD FOREIGN KEY (x) REFERENCES v;
                 ALTER TABLE t DROP CONSTRAINT t_x_check1;
                 ALTER TABLE t ADD CHECK (x < 100);"
            ),
            [
                "table:public.t",
                "constraint:public.t.t_x_check1",
                "constraint:public.t.t_x_check2",
                "constraint:public.t.t_check",
                "constraint:public.t.t_check1",
                "index:public.t_x_idx",
                "index:public.t_x_idx1",
                "index:public.t_lower_lower1_idx",
                "index:public.t_expr_idx",
                "index:public.t_y_key",
                // t_y_key is an index already: the constraint gets a suffix.
                "constraint:public.t.t_y_key1",
                "fk:public.t.t_x_fkey",
                "fk:public.t.t_x_fkey1",
                "drop:constraint:public.t.t_x_check1",
                // Dropped: the name is free again.
                "constraint:public.t.t_x_check1",
            ]
        );
    }

    #[test]
    fn default_names_are_truncated_like_postgres_does() {
        let table = "a".repeat(60);
        let column = "b".repeat(30);
        let sql =
            format!("CREATE TABLE {table} ({column} int); CREATE INDEX ON {table} ({column});");
        let keys = keys(&sql);
        // 63 bytes: the longer name is cut until both fit with `_` and `_idx`.
        let expected = format!("index:public.{}_{}_idx", "a".repeat(29), "b".repeat(29));
        assert_eq!(keys[1], expected);
        assert_eq!(expected.len() - "index:public.".len(), 63);
    }

    #[test]
    fn partitions_get_a_copy_of_their_parent_s_indexes() {
        assert_eq!(
            keys(
                "CREATE TABLE p (x int) PARTITION BY RANGE (x);
                 CREATE TABLE p1 PARTITION OF p FOR VALUES FROM (1) TO (10);
                 CREATE INDEX ON p (x);
                 CREATE INDEX ON p1 (x);
                 CREATE TABLE p2 PARTITION OF p FOR VALUES FROM (10) TO (20);
                 CREATE INDEX ON p2 (x);"
            ),
            [
                "table:public.p",
                "table:public.p1",
                "index:public.p_x_idx",
                // p1 got p1_x_idx from its parent's index.
                "index:public.p1_x_idx1",
                "table:public.p2",
                "index:public.p2_x_idx1",
            ]
        );
    }

    #[test]
    fn a_statement_has_a_key_per_object_it_defines() {
        assert_eq!(
            all_keys(
                "ALTER TABLE t ADD CONSTRAINT a UNIQUE (x), ADD CONSTRAINT b CHECK (x > 0), ADD COLUMN y int;
                 DROP TABLE a, b;
                 DROP INDEX i1, app.i2;
                 CREATE SCHEMA s CREATE TABLE t (id int) CREATE INDEX ON t (id) CREATE VIEW v AS SELECT 1;"
            ),
            [
                vec![
                    "alter:table:public.t",
                    "constraint:public.t.a",
                    "constraint:public.t.b",
                    "column:public.t.y",
                ],
                vec!["drop:table:public.a", "drop:table:public.b"],
                vec!["drop:index:public.i1", "drop:index:app.i2"],
                vec!["schema:s", "table:s.t", "index:s.t_id_idx", "view:s.v"],
            ]
        );
    }

    #[test]
    fn every_key_reads_back_from_its_string() {
        let parsed = parse(
            "CREATE TABLE \"x:y\" (\"a\"\"b\" int);
             SET app.tenant = 'x';
             CREATE TABLE \"a.b\".c (id int);
             COMMENT ON COLUMN \"x:y\".\"a\"\"b\" IS 'q';
             INSERT INTO t VALUES (1);",
        );
        for statement in &parsed.statements {
            for key in &statement.keys {
                assert_eq!(Key::from_str(&key.to_string()).as_ref(), Ok(key), "{key}");
            }
        }
        assert_eq!(parsed.statements[1].key.to_string(), "set:app.tenant");
    }

    #[test]
    fn quoted_names_round_trip_through_the_string_form() {
        let parsed = parse("CREATE TABLE \"Billing\".\"Order.Items\" (id int)");
        let key = &parsed.statements[0].key;
        assert_eq!(key.to_string(), "table:\"Billing\".\"Order.Items\"");
        assert_eq!(Key::from_str(&key.to_string()).unwrap(), *key);
        assert_eq!(key.path(), ["Billing", "Order.Items"]);

        let comment = Key::from_str("comment:column:public.users.\"say \"\"hi\"\"\"").unwrap();
        assert_eq!(comment.action(), Action::Comment);
        assert_eq!(comment.kind(), ObjectKind::Column);
        assert_eq!(comment.path(), ["public", "users", "say \"hi\""]);
        assert_eq!(
            Key::from_str("table:public.users").unwrap(),
            Key::table("public", "users")
        );
        assert!(Key::from_str("nonsense").is_err());
        assert!(Key::from_str("alter:nope:x").is_err());
        assert!(Key::from_str("table:\"unterminated").is_err());
    }
}
