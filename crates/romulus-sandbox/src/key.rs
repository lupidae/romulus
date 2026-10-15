//! What a statement is about: the key the statement-level merge and graph
//! edits use to find "the statement that defines `public.users`".
//!
//! A key is an optional action, an object kind and a qualified path:
//!
//! | statement | key |
//! |---|---|
//! | `CREATE TABLE users (…)` | `table:public.users` |
//! | `CREATE INDEX ON users (lower(email))` | `index:public.users_lower_idx` |
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
//! Unqualified names resolve against the `search_path` the text has set so
//! far (`public` by default). Path parts are quoted like SQL identifiers when
//! needed (`table:public."Order.Items"`), so a key string is unambiguous and
//! [`Key::from_str`](std::str::FromStr) reads it back. Unnamed indexes and
//! constraints get the name Postgres would most likely choose
//! (`<table>_<columns>_idx`, `_key`, `_pkey`, `_fkey`, `_check`, `_excl`);
//! that only has to be stable, not exact. Two statements can share a key (two
//! `ALTER TABLE users …`); [`crate::Statement::occurrence`] tells them apart.

use std::{fmt, str::FromStr};

use pg_query::{
    NodeEnum,
    protobuf::{
        AlterTableCmd, AlterTableType, ConstrType, Constraint, IndexElem, Node, ObjectType,
        RangeVar,
    },
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
        let path = split_path(path).ok_or_else(err)?;
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

/// The key of one statement, resolving unqualified names against `path`.
#[allow(clippy::too_many_lines)] // one arm per statement kind
pub(crate) fn key_of(node: &NodeEnum, path: &SearchPath) -> Key {
    let k = |action, kind, parts: Vec<String>| Key::new(action, kind, parts);
    let rel = |rv: &Option<RangeVar>| -> Vec<String> {
        rv.as_ref().map_or_else(Vec::new, |rv| {
            let (schema, name) = QualifiedName::from_range_var(rv).resolve(path);
            vec![schema, name]
        })
    };
    let list = |nodes: &[Node]| -> Vec<String> {
        QualifiedName::from_list(nodes).map_or_else(Vec::new, |q| {
            let (schema, name) = q.resolve(path);
            vec![schema, name]
        })
    };
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
            k(Action::Create, ObjectKind::Schema, vec![name])
        }
        NodeEnum::CreateExtensionStmt(s) => k(
            Action::Create,
            ObjectKind::Extension,
            vec![s.extname.clone()],
        ),
        NodeEnum::CreateStmt(s) => k(Action::Create, ObjectKind::Table, rel(&s.relation)),
        NodeEnum::CreateForeignTableStmt(s) => k(
            Action::Create,
            ObjectKind::ForeignTable,
            rel(&s.base_stmt.as_ref().and_then(|b| b.relation.clone())),
        ),
        NodeEnum::ViewStmt(s) => k(Action::Create, ObjectKind::View, rel(&s.view)),
        NodeEnum::CreateTableAsStmt(s) => {
            let kind = if s.objtype() == ObjectType::ObjectMatview {
                ObjectKind::MaterializedView
            } else {
                ObjectKind::Table
            };
            k(
                Action::Create,
                kind,
                rel(&s.into.as_ref().and_then(|i| i.rel.clone())),
            )
        }
        NodeEnum::IndexStmt(s) => {
            let table = rel(&s.relation);
            let schema = table.first().cloned().unwrap_or_default();
            let name = if s.idxname.is_empty() {
                let table = table.get(1).map_or("", String::as_str);
                let suffix = if s.primary {
                    "pkey"
                } else if !s.exclude_op_names.is_empty() {
                    "excl"
                } else {
                    "idx"
                };
                default_name(table, &index_columns(&s.index_params), suffix)
            } else {
                s.idxname.clone()
            };
            k(Action::Create, ObjectKind::Index, vec![schema, name])
        }
        NodeEnum::CreateSeqStmt(s) => k(Action::Create, ObjectKind::Sequence, rel(&s.sequence)),
        NodeEnum::AlterSeqStmt(s) => k(Action::Alter, ObjectKind::Sequence, rel(&s.sequence)),
        NodeEnum::CreateEnumStmt(s) => k(Action::Create, ObjectKind::Enum, list(&s.type_name)),
        NodeEnum::AlterEnumStmt(s) => k(Action::Alter, ObjectKind::Enum, list(&s.type_name)),
        NodeEnum::CompositeTypeStmt(s) => k(Action::Create, ObjectKind::Type, rel(&s.typevar)),
        NodeEnum::CreateRangeStmt(s) => k(Action::Create, ObjectKind::Type, list(&s.type_name)),
        NodeEnum::CreateDomainStmt(s) => k(Action::Create, ObjectKind::Domain, list(&s.domainname)),
        NodeEnum::AlterDomainStmt(s) => k(Action::Alter, ObjectKind::Domain, list(&s.type_name)),
        NodeEnum::CreatePolicyStmt(s) => {
            let mut parts = rel(&s.table);
            parts.push(s.policy_name.clone());
            k(Action::Create, ObjectKind::Policy, parts)
        }
        NodeEnum::AlterPolicyStmt(s) => {
            let mut parts = rel(&s.table);
            parts.push(s.policy_name.clone());
            k(Action::Alter, ObjectKind::Policy, parts)
        }
        NodeEnum::CreateTrigStmt(s) => {
            let mut parts = rel(&s.relation);
            parts.push(s.trigname.clone());
            k(Action::Create, ObjectKind::Trigger, parts)
        }
        NodeEnum::RuleStmt(s) => {
            let mut parts = rel(&s.relation);
            parts.push(s.rulename.clone());
            k(Action::Create, ObjectKind::Rule, parts)
        }
        NodeEnum::CreateFunctionStmt(s) => {
            k(Action::Create, ObjectKind::Function, list(&s.funcname))
        }
        NodeEnum::CreateStatsStmt(s) => {
            k(Action::Create, ObjectKind::Statistics, list(&s.defnames))
        }
        NodeEnum::DefineStmt(s) => {
            let kind = match s.kind() {
                ObjectType::ObjectCollation => ObjectKind::Collation,
                ObjectType::ObjectType => ObjectKind::Type,
                ObjectType::ObjectAggregate => ObjectKind::Function,
                _ => return statement_key(node),
            };
            k(Action::Create, kind, list(&s.defnames))
        }
        NodeEnum::VariableSetStmt(s) => {
            let name = if s.name.is_empty() {
                "all".to_owned()
            } else {
                s.name.to_ascii_lowercase()
            };
            k(Action::Create, ObjectKind::Setting, vec![name])
        }
        NodeEnum::AlterTableStmt(s) => alter_table_key(s, path),
        NodeEnum::CommentStmt(s) => object_key(
            Action::Comment,
            s.objtype(),
            s.object.as_deref(),
            None,
            None,
            path,
        )
        .unwrap_or_else(|| statement_key(node)),
        NodeEnum::RenameStmt(s) => object_key(
            Action::Rename,
            s.rename_type(),
            s.object.as_deref(),
            s.relation.as_ref(),
            Some(&s.subname),
            path,
        )
        .unwrap_or_else(|| statement_key(node)),
        NodeEnum::AlterObjectSchemaStmt(s) => object_key(
            Action::Alter,
            s.object_type(),
            s.object.as_deref(),
            s.relation.as_ref(),
            None,
            path,
        )
        .unwrap_or_else(|| statement_key(node)),
        NodeEnum::DropStmt(s) => s
            .objects
            .first()
            .and_then(|object| {
                object_key(
                    Action::Drop,
                    s.remove_type(),
                    Some(object),
                    None,
                    None,
                    path,
                )
            })
            .unwrap_or_else(|| statement_key(node)),
        _ => statement_key(node),
    }
}

fn alter_table_key(s: &pg_query::protobuf::AlterTableStmt, path: &SearchPath) -> Key {
    let table = s.relation.as_ref().map_or_else(Vec::new, |rv| {
        let (schema, name) = QualifiedName::from_range_var(rv).resolve(path);
        vec![schema, name]
    });
    let object_kind = match s.objtype() {
        ObjectType::ObjectView => ObjectKind::View,
        ObjectType::ObjectMatview => ObjectKind::MaterializedView,
        ObjectType::ObjectIndex => ObjectKind::Index,
        ObjectType::ObjectSequence => ObjectKind::Sequence,
        ObjectType::ObjectForeignTable => ObjectKind::ForeignTable,
        ObjectType::ObjectType => ObjectKind::Type,
        _ => ObjectKind::Table,
    };
    let whole = || Key::new(Action::Alter, object_kind, table.clone());
    let [cmd] = s.cmds.as_slice() else {
        return whole();
    };
    let Some(NodeEnum::AlterTableCmd(cmd)) = cmd.node.as_ref() else {
        return whole();
    };
    let with = |name: String| {
        let mut parts = table.clone();
        parts.push(name);
        parts
    };
    let table_name = table.get(1).map_or("", String::as_str);
    match cmd.subtype() {
        AlterTableType::AtAddColumn => match def(cmd) {
            Some(NodeEnum::ColumnDef(column)) => Key::new(
                Action::Create,
                ObjectKind::Column,
                with(column.colname.clone()),
            ),
            _ => whole(),
        },
        AlterTableType::AtAddConstraint => match def(cmd) {
            Some(NodeEnum::Constraint(c)) => {
                let kind = if c.contype() == ConstrType::ConstrForeign {
                    ObjectKind::ForeignKey
                } else {
                    ObjectKind::Constraint
                };
                Key::new(
                    Action::Create,
                    kind,
                    with(constraint_name(c, table_name, None)),
                )
            }
            _ => whole(),
        },
        AlterTableType::AtEnableRowSecurity | AlterTableType::AtDisableRowSecurity => {
            Key::new(Action::Create, ObjectKind::RowSecurity, table.clone())
        }
        AlterTableType::AtForceRowSecurity | AlterTableType::AtNoForceRowSecurity => {
            Key::new(Action::Create, ObjectKind::ForceRowSecurity, table.clone())
        }
        AlterTableType::AtDropColumn => {
            Key::new(Action::Drop, ObjectKind::Column, with(cmd.name.clone()))
        }
        AlterTableType::AtDropConstraint => {
            Key::new(Action::Drop, ObjectKind::Constraint, with(cmd.name.clone()))
        }
        _ if !cmd.name.is_empty() && is_column_command(cmd.subtype()) => {
            Key::new(Action::Alter, ObjectKind::Column, with(cmd.name.clone()))
        }
        _ => whole(),
    }
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

/// The name of a constraint, or the one Postgres would give it.
pub(crate) fn constraint_name(c: &Constraint, table: &str, column: Option<&str>) -> String {
    if !c.conname.is_empty() {
        return c.conname.clone();
    }
    let keys = |nodes: &[Node]| strings(nodes);
    let (columns, suffix) = match c.contype() {
        ConstrType::ConstrPrimary => return format!("{table}_pkey"),
        ConstrType::ConstrForeign => {
            let columns = keys(&c.fk_attrs);
            let columns = if columns.is_empty() {
                column.map(|c| vec![c.to_owned()]).unwrap_or_default()
            } else {
                columns
            };
            (columns, "fkey")
        }
        ConstrType::ConstrUnique => {
            let columns = keys(&c.keys);
            let columns = if columns.is_empty() {
                column.map(|c| vec![c.to_owned()]).unwrap_or_default()
            } else {
                columns
            };
            (columns, "key")
        }
        ConstrType::ConstrExclusion => {
            let columns = c
                .exclusions
                .iter()
                .filter_map(|pair| match pair.node.as_ref()? {
                    NodeEnum::List(l) => match l.items.first()?.node.as_ref()? {
                        NodeEnum::IndexElem(e) => Some(index_column(e)),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            (columns, "excl")
        }
        ConstrType::ConstrCheck => {
            let first = column
                .map(str::to_owned)
                .or_else(|| c.raw_expr.as_deref().and_then(first_column_ref));
            (first.into_iter().collect(), "check")
        }
        _ => (
            column.map(|c| vec![c.to_owned()]).unwrap_or_default(),
            "not_null",
        ),
    };
    default_name(table, &columns, suffix)
}

/// `<table>_<col>_<col>_<suffix>`, Postgres's `ChooseRelationName` minus the
/// truncation and de-duplication.
fn default_name(table: &str, columns: &[String], suffix: &str) -> String {
    let mut name = table.to_owned();
    for column in columns {
        name.push('_');
        name.push_str(column);
    }
    name.push('_');
    name.push_str(suffix);
    name
}

fn index_columns(params: &[Node]) -> Vec<String> {
    params
        .iter()
        .filter_map(|p| match p.node.as_ref()? {
            NodeEnum::IndexElem(e) => Some(index_column(e)),
            _ => None,
        })
        .collect()
}

/// What Postgres names an index column: the column, or the function an
/// expression calls, else `expr`.
fn index_column(e: &IndexElem) -> String {
    if !e.name.is_empty() {
        return e.name.clone();
    }
    if !e.indexcolname.is_empty() {
        return e.indexcolname.clone();
    }
    let mut expr = e.expr.as_deref().and_then(|n| n.node.as_ref());
    while let Some(NodeEnum::TypeCast(cast)) = expr {
        expr = cast.arg.as_deref().and_then(|n| n.node.as_ref());
    }
    match expr {
        Some(NodeEnum::FuncCall(f)) => strings(&f.funcname)
            .pop()
            .unwrap_or_else(|| "expr".to_owned()),
        Some(NodeEnum::ColumnRef(c)) => strings(&c.fields)
            .pop()
            .unwrap_or_else(|| "expr".to_owned()),
        _ => "expr".to_owned(),
    }
}

/// The first column an expression mentions, for a CHECK's default name.
fn first_column_ref(node: &Node) -> Option<String> {
    let value = serde_json::to_value(node).ok()?;
    let mut found = None;
    crate::walk::visit(&value, &mut |kind, inner| {
        if found.is_none() && kind == "ColumnRef" {
            found = inner["fields"]
                .as_array()
                .and_then(|fields| fields.last())
                .and_then(|f| f["node"]["String"]["sval"].as_str())
                .map(str::to_owned);
        }
    });
    found
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
