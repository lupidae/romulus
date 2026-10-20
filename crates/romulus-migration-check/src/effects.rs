//! What each statement does to the rows, read from its parse tree, and who
//! each table and column is through the migration.
//!
//! A [`Walk`] starts from a model's tables, columns and enum types and
//! follows the statements in order: renames and moves (`ALTER … RENAME`,
//! `SET SCHEMA`), tables and columns created and dropped, conversions
//! (`ALTER COLUMN … TYPE … USING`), rows deleted, inserted or updated,
//! enum labels renamed, and what it cannot read (`DO`, `CALL`, `MERGE`:
//! [`Effect::Opaque`]). Every [`Effect`] names the walk's own table and
//! column (indexes that never move), each of which knows the base object it
//! is, if any: so a check follows a renamed column to its values, and a
//! value that did not come back is explained by the statement that dropped
//! or converted it.
//!
//! The renames come from the statements themselves, never from a guess: a
//! generated migration runs them as `ALTER … RENAME` (romulus-migration
//! puts them first), a written one as written. Unqualified names resolve
//! against the `search_path` the statements set (`public` by default).
//!
//! A partitioned table's column changes and row statements reach its
//! partitions, as Postgres applies them. User types (enums, domains,
//! composites) are followed through renames too: a conversion names the
//! types of its moment, resolved to their names at the walk's end
//! ([`Conversion::resolved`]).

use std::ops::Range;

use remus_core::{Schema, model::EntityKind};
use romulus_sandbox::pg_query::{
    self, NodeEnum,
    protobuf::{
        self, AlterTableType, DropBehavior, KeywordKind, Node, ObjectType, RangeVar, Token,
        VariableSetKind,
    },
};

/// A table (or partitioned table, foreign table) as the walk sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The base table it is (`schema`, `name` in the base model); `None`
    /// for one the migration created (even under a base table's name).
    pub base: Option<(String, String)>,
    pub schema: String,
    pub name: String,
    pub alive: bool,
    /// The partitioned table it is a partition of (walk index).
    pub parent: Option<usize>,
    /// Every partitioned table it has been a partition of, in this walk:
    /// its rows were theirs while it was.
    pub parents: Vec<usize>,
    pub columns: Vec<Column>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// Its name in the base model; `None` for one the migration added.
    pub base: Option<String>,
    pub name: String,
    pub alive: bool,
    /// The user types it depends on (walk indexes): its own, and a
    /// domain's base. `DROP TYPE … CASCADE` of one drops it.
    pub types: Vec<usize>,
}

/// A user type (enum, domain, composite).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeEntry {
    /// The base type it is; `None` for one the migration created.
    pub base: Option<(String, String)>,
    pub schema: String,
    pub name: String,
    pub alive: bool,
}

/// A conversion of a column, as the statement wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion {
    /// The type it converts to, as the statement names it.
    pub to: String,
    /// The `USING` expression, deparsed; `None`: the column, cast.
    pub using: Option<String>,
    /// The table's columns when it ran: (name then, walk column index).
    pub names: Vec<(String, usize)>,
    /// The walk's type `to` names, if a user type, and its array depth.
    pub to_type: Option<(usize, usize)>,
    /// The user types the `USING` casts to (`::name`): where in `using`,
    /// and the walk's type.
    pub using_types: Vec<(Range<usize>, usize)>,
}

impl Conversion {
    /// `to` and `using` with the user types they name under their names at
    /// the end of `walk` (a type created as `x_new` and renamed `x` later
    /// reads `x`): what Postgres resolves after the migration.
    pub fn resolved(&self, walk: &Walk) -> (String, Option<String>) {
        let name = |i: usize| {
            walk.types
                .get(i)
                .map(|t| crate::session::qualified(&t.schema, &t.name))
        };
        let to = match self.to_type.and_then(|(i, depth)| Some((name(i)?, depth))) {
            Some((type_name, depth)) => format!("{type_name}{}", "[]".repeat(depth)),
            None => self.to.clone(),
        };
        let using = self.using.as_ref().map(|using| {
            let mut out = using.clone();
            // From the end: earlier ranges stay where they are.
            let mut refs = self.using_types.clone();
            refs.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
            for (range, i) in refs {
                if let Some(type_name) = name(i)
                    && out.get(range.clone()).is_some()
                {
                    out.replace_range(range, &type_name);
                }
            }
            out
        });
        (to, using)
    }
}

/// What a statement does to the rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    TableCreated {
        table: usize,
    },
    TableDropped {
        table: usize,
    },
    TableRenamed {
        table: usize,
    },
    /// `rows`: existing rows can make it fail (`NOT NULL` without a
    /// default, a constraint, a generated value, a type that may be a
    /// domain): a test on an empty table cannot tell.
    ColumnAdded {
        table: usize,
        column: usize,
        rows: bool,
    },
    ColumnDropped {
        table: usize,
        column: usize,
    },
    ColumnRenamed {
        table: usize,
        column: usize,
    },
    ColumnConverted {
        table: usize,
        column: usize,
        conversion: Conversion,
    },
    /// `DELETE` (`all`: without `WHERE`), `TRUNCATE`.
    RowsDeleted {
        table: usize,
        all: bool,
    },
    /// `INSERT`, `COPY … FROM`.
    RowsAdded {
        table: usize,
    },
    /// `UPDATE … SET` these columns (`None`: it cannot tell which).
    ValuesChanged {
        table: usize,
        columns: Option<Vec<usize>>,
    },
    /// `ALTER TYPE … RENAME VALUE`: the enum's values in every column of it.
    LabelRenamed {
        enum_type: usize,
        from: String,
        to: String,
    },
    /// The table altered otherwise (a constraint, a default, an index): it
    /// is touched, its rows are not changed. `rows`: existing rows can make
    /// it fail (a constraint validated, `NOT NULL` set, a unique or
    /// expression index): a test on an empty table cannot tell.
    Altered {
        table: usize,
        rows: bool,
    },
    /// `DETACH PARTITION`: the child's rows leave the parent (moved, not
    /// lost: they are the child's own now).
    Detached {
        parent: usize,
        child: usize,
    },
    /// `ATTACH PARTITION`: the child's rows become the parent's too.
    Attached {
        parent: usize,
        child: usize,
    },
    /// Code the check cannot read (`DO`, `CALL`, `MERGE`): it may have
    /// changed anything, which cannot be verified.
    Opaque,
}

/// Whether existing rows can make adding the column `def` fail: `NOT NULL`
/// with nothing to fill it, a validated constraint, a stored generated
/// value, or a user type (a domain may refuse NULL) without a default.
fn column_needs_rows(def: &protobuf::ColumnDef, user_type: bool) -> bool {
    use protobuf::ConstrType;
    let constraints: Vec<&protobuf::Constraint> = def
        .constraints
        .iter()
        .filter_map(|c| match c.node.as_ref() {
            Some(NodeEnum::Constraint(c)) => Some(c.as_ref()),
            _ => None,
        })
        .collect();
    let has = |kind: ConstrType| constraints.iter().any(|c| c.contype() == kind);
    let filled = def.raw_default.is_some()
        || has(ConstrType::ConstrDefault)
        || has(ConstrType::ConstrIdentity)
        || has(ConstrType::ConstrGenerated);
    let validated = constraints.iter().any(|c| {
        matches!(
            c.contype(),
            ConstrType::ConstrCheck
                | ConstrType::ConstrPrimary
                | ConstrType::ConstrUnique
                | ConstrType::ConstrExclusion
                | ConstrType::ConstrForeign
        ) && !c.skip_validation
    });
    (has(ConstrType::ConstrNotnull) && !filled)
        || validated
        || has(ConstrType::ConstrGenerated)
        || (user_type && !filled)
}

/// Whether existing rows can make `ALTER TABLE … <subtype>` fail or change
/// what it does. What only changes the catalog (defaults, `DROP NOT NULL`,
/// storage, owners, triggers on or off, row security…) cannot; anything
/// else is taken to.
fn alter_needs_rows(subtype: AlterTableType, cmd: &protobuf::AlterTableCmd) -> bool {
    use AlterTableType as A;
    match subtype {
        A::AtAddConstraint | A::AtReAddConstraint => !matches!(
            cmd.def.as_ref().and_then(|d| d.node.as_ref()),
            Some(NodeEnum::Constraint(c)) if c.skip_validation
        ),
        A::AtColumnDefault
        | A::AtCookedColumnDefault
        | A::AtDropNotNull
        | A::AtDropExpression
        | A::AtSetStatistics
        | A::AtSetOptions
        | A::AtResetOptions
        | A::AtSetStorage
        | A::AtSetCompression
        | A::AtDropConstraint
        | A::AtReAddComment
        | A::AtAlterConstraint
        | A::AtAlterColumnGenericOptions
        | A::AtChangeOwner
        | A::AtClusterOn
        | A::AtDropCluster
        | A::AtSetLogged
        | A::AtSetUnLogged
        | A::AtDropOids
        | A::AtSetAccessMethod
        | A::AtSetTableSpace
        | A::AtSetRelOptions
        | A::AtResetRelOptions
        | A::AtReplaceRelOptions
        | A::AtEnableTrig
        | A::AtEnableAlwaysTrig
        | A::AtEnableReplicaTrig
        | A::AtDisableTrig
        | A::AtEnableTrigAll
        | A::AtDisableTrigAll
        | A::AtEnableTrigUser
        | A::AtDisableTrigUser
        | A::AtEnableRule
        | A::AtEnableAlwaysRule
        | A::AtEnableReplicaRule
        | A::AtDisableRule
        | A::AtDropInherit
        | A::AtDropOf
        | A::AtReplicaIdentity
        | A::AtEnableRowSecurity
        | A::AtDisableRowSecurity
        | A::AtForceRowSecurity
        | A::AtNoForceRowSecurity
        | A::AtGenericOptions
        | A::AtAddIdentity
        | A::AtSetIdentity
        | A::AtDropIdentity
        | A::AtReAddStatistics
        | A::AtDetachPartitionFinalize => false,
        _ => true,
    }
}

impl Effect {
    /// Whether what it does depends on the rows: data changed or lost, or
    /// a statement rows can make fail. Renames, catalog changes, nullable
    /// columns and plain indexes do not: a test of them needs no rows.
    pub fn needs_rows(&self) -> bool {
        match self {
            Self::TableCreated { .. }
            | Self::TableRenamed { .. }
            | Self::ColumnRenamed { .. }
            | Self::LabelRenamed { .. }
            | Self::Detached { .. } => false,
            Self::ColumnAdded { rows, .. } | Self::Altered { rows, .. } => *rows,
            Self::TableDropped { .. }
            | Self::ColumnDropped { .. }
            | Self::ColumnConverted { .. }
            | Self::RowsDeleted { .. }
            | Self::RowsAdded { .. }
            | Self::ValuesChanged { .. }
            | Self::Attached { .. }
            | Self::Opaque => true,
        }
    }
}

/// The tables, columns and user types through the statements.
#[derive(Debug, Clone)]
pub struct Walk {
    pub tables: Vec<Table>,
    pub types: Vec<TypeEntry>,
    search_path: Vec<String>,
}

/// A name list (`[schema,] name`) from string nodes.
fn names(nodes: &[Node]) -> Vec<String> {
    nodes
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(NodeEnum::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// A type's name parts from a `DROP TYPE` / `RENAME` object node (a type
/// name, or a list of names).
fn type_parts(node: &Node) -> Vec<String> {
    match node.node.as_ref() {
        Some(NodeEnum::TypeName(t)) => names(&t.names),
        Some(NodeEnum::List(l)) => names(&l.items),
        _ => Vec::new(),
    }
}

/// An expression's text, as Postgres's deparser prints it.
fn deparse_expr(expr: &Node) -> Option<String> {
    let select = NodeEnum::SelectStmt(Box::new(protobuf::SelectStmt {
        target_list: vec![Node {
            node: Some(NodeEnum::ResTarget(Box::new(protobuf::ResTarget {
                val: Some(Box::new(expr.clone())),
                ..protobuf::ResTarget::default()
            }))),
        }],
        // libpg_query refuses its enums' undefined (0) values.
        op: protobuf::SetOperation::SetopNone.into(),
        limit_option: protobuf::LimitOption::Default.into(),
        ..protobuf::SelectStmt::default()
    }));
    let text = select.deparse().ok()?;
    text.strip_prefix("SELECT ").map(str::to_owned)
}

/// A type name's text (`numeric(10,2)`, `public.mood[]`).
fn deparse_type(type_name: &protobuf::TypeName) -> Option<String> {
    let cast = Node {
        node: Some(NodeEnum::TypeCast(Box::new(protobuf::TypeCast {
            arg: Some(Box::new(Node {
                node: Some(NodeEnum::AConst(protobuf::AConst {
                    isnull: true,
                    ..protobuf::AConst::default()
                })),
            })),
            type_name: Some(type_name.clone()),
            location: 0,
        }))),
    };
    let text = deparse_expr(&cast)?;
    text.strip_prefix("NULL::").map(str::to_owned)
}

impl Walk {
    /// The tables (of every kind holding rows), columns and enums of
    /// `model`, each its own base.
    pub fn new(model: &Schema) -> Self {
        let entry = |schema: &str, name: &str| TypeEntry {
            base: Some((schema.to_owned(), name.to_owned())),
            schema: schema.to_owned(),
            name: name.to_owned(),
            alive: true,
        };
        let types: Vec<TypeEntry> = model
            .enums
            .iter()
            .map(|e| entry(&e.schema, &e.name))
            .chain(model.domains.iter().map(|d| entry(&d.schema, &d.name)))
            .chain(model.composites.iter().map(|c| entry(&c.schema, &c.name)))
            .collect();
        let index = |schema: &str, name: &str| {
            types
                .iter()
                .position(|t| t.schema == schema && t.name == name)
        };
        // A column's types: its own, and a domain's base (`schema.name`,
        // maybe an array of it).
        let column_types = |schema: &str, name: &str| -> Vec<usize> {
            let mut out: Vec<usize> = index(schema, name).into_iter().collect();
            if let Some(domain) = model
                .domains
                .iter()
                .find(|d| d.schema == schema && d.name == name)
                && let Some((s, n)) = domain.base_type.trim_end_matches("[]").split_once('.')
                && let Some(base) = index(s.trim_matches('"'), n.trim_matches('"'))
            {
                out.push(base);
            }
            out
        };
        let mut tables: Vec<Table> = model
            .entities
            .iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    EntityKind::Table | EntityKind::PartitionedTable | EntityKind::ForeignTable
                )
            })
            .map(|e| Table {
                base: Some((e.schema.clone(), e.name.clone())),
                schema: e.schema.clone(),
                name: e.name.clone(),
                alive: true,
                parent: None,
                parents: Vec::new(),
                columns: e
                    .columns
                    .iter()
                    .map(|c| Column {
                        base: Some(c.name.clone()),
                        name: c.name.clone(),
                        alive: true,
                        types: column_types(&c.type_schema, &c.type_name),
                    })
                    .collect(),
            })
            .collect();
        // remus lists a partition under its parent only: it is a table of
        // the walk all the same, with its parent's columns (parents first:
        // a sub-partition's parent is a partition listed before it).
        for p in &model.partitions {
            if tables
                .iter()
                .any(|t| t.schema == p.child_schema && t.name == p.child)
            {
                continue;
            }
            let columns = tables
                .iter()
                .find(|t| t.schema == p.parent_schema && t.name == p.parent)
                .map(|t| t.columns.clone())
                .unwrap_or_default();
            tables.push(Table {
                base: Some((p.child_schema.clone(), p.child.clone())),
                schema: p.child_schema.clone(),
                name: p.child.clone(),
                alive: true,
                parent: None,
                parents: Vec::new(),
                columns,
            });
        }
        for p in &model.partitions {
            let parent = tables
                .iter()
                .position(|t| t.schema == p.parent_schema && t.name == p.parent);
            if let Some(child) = tables
                .iter_mut()
                .find(|t| t.schema == p.child_schema && t.name == p.child)
            {
                child.parent = parent;
                child.parents.extend(parent);
            }
        }
        Self {
            tables,
            types,
            search_path: vec!["public".to_owned()],
        }
    }

    /// The user types a column definition's type names (its own).
    fn types_of(&self, type_name: Option<&protobuf::TypeName>) -> Vec<usize> {
        type_name
            .and_then(|t| self.resolve_type(&names(&t.names)))
            .into_iter()
            .collect()
    }

    /// `table` and every table that has been a partition of it (or of one
    /// of those) in this walk: whose rows were its rows at some point.
    pub fn family_ever(&self, table: usize) -> Vec<usize> {
        let mut out = vec![table];
        let mut at = 0;
        while at < out.len() {
            let parent = out[at];
            for i in 0..self.tables.len() {
                if self.tables[i].parents.contains(&parent) && !out.contains(&i) {
                    out.push(i);
                }
            }
            at += 1;
        }
        out
    }

    /// `table` and its partitions (theirs too), live ones.
    pub fn family(&self, table: usize) -> Vec<usize> {
        let mut out = vec![table];
        let mut at = 0;
        while at < out.len() {
            let parent = out[at];
            out.extend(
                (0..self.tables.len())
                    .filter(|&i| self.tables[i].alive && self.tables[i].parent == Some(parent)),
            );
            at += 1;
        }
        out
    }

    /// The tables a statement on `rv` reaches: the table, and its
    /// partitions unless it says `ONLY`.
    fn reach(&self, rv: &RangeVar) -> Vec<usize> {
        match self.range(rv) {
            Some(table) if rv.inh => self.family(table),
            Some(table) => vec![table],
            None => Vec::new(),
        }
    }

    /// The live table now called `schema.name`.
    pub fn find(&self, schema: &str, name: &str) -> Option<usize> {
        self.tables
            .iter()
            .position(|t| t.alive && t.schema == schema && t.name == name)
    }

    /// The live table that is base table `schema.name`.
    pub fn of_base(&self, schema: &str, name: &str) -> Option<usize> {
        self.tables.iter().position(|t| {
            t.alive
                && t.base
                    .as_ref()
                    .is_some_and(|(s, n)| s == schema && n == name)
        })
    }

    /// The live column now called `name` of table `table`.
    pub fn column(&self, table: usize, name: &str) -> Option<usize> {
        self.tables[table]
            .columns
            .iter()
            .position(|c| c.alive && c.name == name)
    }

    /// The live column that is base column `name` of table `table`.
    pub fn column_of_base(&self, table: usize, name: &str) -> Option<usize> {
        self.tables[table]
            .columns
            .iter()
            .position(|c| c.alive && c.base.as_deref() == Some(name))
    }

    fn schema_for(&self, given: &str) -> String {
        if given.is_empty() {
            self.search_path
                .first()
                .cloned()
                .unwrap_or_else(|| "public".to_owned())
        } else {
            given.to_owned()
        }
    }

    /// The live table a name refers to: qualified, or the first schema of
    /// the `search_path` holding it, or the only table of that name.
    fn resolve(&self, schema: &str, name: &str) -> Option<usize> {
        if !schema.is_empty() {
            return self.find(schema, name);
        }
        for s in &self.search_path {
            if let Some(t) = self.find(s, name) {
                return Some(t);
            }
        }
        let mut alike = self
            .tables
            .iter()
            .enumerate()
            .filter(|(_, t)| t.alive && t.name == name);
        match (alike.next(), alike.next()) {
            (Some((i, _)), None) => Some(i),
            _ => None,
        }
    }

    fn range(&self, rv: &RangeVar) -> Option<usize> {
        self.resolve(&rv.schemaname, &rv.relname)
    }

    /// The live user type a name refers to (as a table's name resolves).
    pub fn resolve_type(&self, parts: &[String]) -> Option<usize> {
        let (schema, name) = match parts {
            [schema, name] => (schema.as_str(), name.as_str()),
            [name] => ("", name.as_str()),
            _ => return None,
        };
        let found = |s: &str| {
            self.types
                .iter()
                .position(|e| e.alive && e.schema == s && e.name == name)
        };
        if !schema.is_empty() {
            return found(schema);
        }
        self.search_path.iter().find_map(|s| found(s)).or_else(|| {
            let mut alike = self
                .types
                .iter()
                .enumerate()
                .filter(|(_, e)| e.alive && e.name == name);
            match (alike.next(), alike.next()) {
                (Some((i, _)), None) => Some(i),
                _ => None,
            }
        })
    }

    fn create_type(&mut self, parts: &[String]) {
        let (schema, name) = match parts {
            [schema, name] => (schema.clone(), name.clone()),
            [name] => (self.schema_for(""), name.clone()),
            _ => return,
        };
        self.types.push(TypeEntry {
            base: None,
            schema,
            name,
            alive: true,
        });
    }

    /// The user types a deparsed expression casts to (`::name`,
    /// `::schema.name`): where, and which.
    fn cast_types(&self, expr: &str) -> Vec<(Range<usize>, usize)> {
        let Ok(scanned) = pg_query::scan(expr) else {
            return Vec::new();
        };
        let tokens = scanned.tokens;
        let at = |i: usize| -> Option<(Range<usize>, String)> {
            let t = tokens.get(i)?;
            let word =
                t.token == Token::Ident as i32 || t.keyword_kind != KeywordKind::NoKeyword as i32;
            let range = usize::try_from(t.start).ok()?..usize::try_from(t.end).ok()?;
            let text = expr.get(range.clone())?;
            word.then(|| (range, text.trim_matches('"').to_owned()))
        };
        let mut out = Vec::new();
        for (i, token) in tokens.iter().enumerate() {
            if token.token != Token::Typecast as i32 {
                continue;
            }
            let Some((first, first_name)) = at(i + 1) else {
                continue;
            };
            let dot = tokens
                .get(i + 2)
                .is_some_and(|t| t.token == Token::Ascii46 as i32);
            let (range, parts) = match (dot, at(i + 3)) {
                (true, Some((second, second_name))) => {
                    (first.start..second.end, vec![first_name, second_name])
                }
                _ => (first, vec![first_name]),
            };
            if let Some(found) = self.resolve_type(&parts) {
                out.push((range, found));
            }
        }
        out
    }

    /// The columns a `SET` list writes (all of them when it cannot tell).
    fn changed(&self, table: usize, targets: &[Node]) -> Effect {
        let mut columns = Vec::new();
        let mut every = false;
        for target in targets {
            match target.node.as_ref() {
                Some(NodeEnum::ResTarget(t)) => match self.column(table, &t.name) {
                    Some(c) => columns.push(c),
                    None => every = true,
                },
                _ => every = true,
            }
        }
        Effect::ValuesChanged {
            table,
            columns: (!every).then_some(columns),
        }
    }

    /// One `ALTER TABLE` command on one table (the statement's, or one of
    /// its partitions: they share column names).
    #[allow(clippy::too_many_lines, reason = "one arm per kind of change")]
    fn alter(&mut self, table: usize, cmd: &protobuf::AlterTableCmd, effects: &mut Vec<Effect>) {
        match cmd.subtype() {
            AlterTableType::AtAddColumn => {
                let Some(NodeEnum::ColumnDef(def)) = cmd.def.as_ref().and_then(|d| d.node.as_ref())
                else {
                    return;
                };
                if self.column(table, &def.colname).is_some() {
                    // `IF NOT EXISTS` on one there (a partition gets the
                    // parent's anyway).
                    return;
                }
                let types = self.types_of(def.type_name.as_ref());
                let rows = column_needs_rows(def, !types.is_empty());
                self.tables[table].columns.push(Column {
                    base: None,
                    name: def.colname.clone(),
                    alive: true,
                    types,
                });
                effects.push(Effect::ColumnAdded {
                    table,
                    column: self.tables[table].columns.len() - 1,
                    rows,
                });
            }
            AlterTableType::AtDropColumn => {
                if let Some(column) = self.column(table, &cmd.name) {
                    self.tables[table].columns[column].alive = false;
                    effects.push(Effect::ColumnDropped { table, column });
                }
            }
            AlterTableType::AtAlterColumnType => {
                let Some(column) = self.column(table, &cmd.name) else {
                    return;
                };
                let Some(NodeEnum::ColumnDef(def)) = cmd.def.as_ref().and_then(|d| d.node.as_ref())
                else {
                    return;
                };
                let to = def
                    .type_name
                    .as_ref()
                    .and_then(deparse_type)
                    .unwrap_or_default();
                let to_type = def.type_name.as_ref().and_then(|t| {
                    let found = self.resolve_type(&names(&t.names))?;
                    Some((found, t.array_bounds.len()))
                });
                let using = def.raw_default.as_ref().and_then(|e| deparse_expr(e));
                let using_types = using
                    .as_deref()
                    .map(|u| self.cast_types(u))
                    .unwrap_or_default();
                let names = self.tables[table]
                    .columns
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.alive)
                    .map(|(i, c)| (c.name.clone(), i))
                    .collect();
                // Of its new type now.
                self.tables[table].columns[column].types =
                    to_type.map(|(t, _)| t).into_iter().collect();
                effects.push(Effect::ColumnConverted {
                    table,
                    column,
                    conversion: Conversion {
                        to,
                        using,
                        names,
                        to_type,
                        using_types,
                    },
                });
            }
            AlterTableType::AtDetachPartition => {
                if let Some(Some(NodeEnum::PartitionCmd(p))) =
                    cmd.def.as_ref().map(|d| d.node.as_ref())
                    && let Some(child) = p.name.as_ref().and_then(|rv| self.range(rv))
                {
                    self.tables[child].parent = None;
                    effects.push(Effect::Detached {
                        parent: table,
                        child,
                    });
                }
            }
            AlterTableType::AtAttachPartition => {
                if let Some(Some(NodeEnum::PartitionCmd(p))) =
                    cmd.def.as_ref().map(|d| d.node.as_ref())
                    && let Some(child) = p.name.as_ref().and_then(|rv| self.range(rv))
                {
                    self.tables[child].parent = Some(table);
                    if !self.tables[child].parents.contains(&table) {
                        self.tables[child].parents.push(table);
                    }
                    effects.push(Effect::Attached {
                        parent: table,
                        child,
                    });
                }
            }
            subtype => effects.push(Effect::Altered {
                table,
                rows: alter_needs_rows(subtype, cmd),
            }),
        }
    }

    fn drop_table(&mut self, table: usize, effects: &mut Vec<Effect>) {
        if !self.tables[table].alive {
            return;
        }
        self.tables[table].alive = false;
        effects.push(Effect::TableDropped { table });
        // Its partitions go with it.
        let children: Vec<usize> = (0..self.tables.len())
            .filter(|&i| self.tables[i].parent == Some(table) && self.tables[i].alive)
            .collect();
        for child in children {
            self.drop_table(child, effects);
        }
    }

    /// Follows one statement (`sql` holds exactly one): what it does.
    pub fn statement(&mut self, sql: &str) -> Vec<Effect> {
        let parsed = romulus_sandbox::parse(sql);
        let mut effects = Vec::new();
        for statement in &parsed.statements {
            self.node(statement.node(), &mut effects);
        }
        effects
    }

    #[allow(clippy::too_many_lines, reason = "one arm per kind of statement")]
    fn node(&mut self, node: &NodeEnum, effects: &mut Vec<Effect>) {
        match node {
            NodeEnum::CreateStmt(s) => {
                let Some(rv) = &s.relation else { return };
                let mut columns: Vec<Column> = s
                    .table_elts
                    .iter()
                    .filter_map(|e| match e.node.as_ref() {
                        Some(NodeEnum::ColumnDef(c)) => Some(Column {
                            base: None,
                            name: c.colname.clone(),
                            alive: true,
                            types: self.types_of(c.type_name.as_ref()),
                        }),
                        _ => None,
                    })
                    .collect();
                let parent = s.inh_relations.iter().find_map(|n| match n.node.as_ref() {
                    Some(NodeEnum::RangeVar(rv)) if s.partbound.is_some() => self.range(rv),
                    _ => None,
                });
                // A partition has its parent's columns.
                if let Some(parent) = parent {
                    for c in self.tables[parent].columns.iter().filter(|c| c.alive) {
                        if !columns.iter().any(|own| own.name == c.name) {
                            columns.push(Column {
                                base: None,
                                name: c.name.clone(),
                                alive: true,
                                types: c.types.clone(),
                            });
                        }
                    }
                }
                let schema = self.schema_for(&rv.schemaname);
                if s.if_not_exists && self.find(&schema, &rv.relname).is_some() {
                    return;
                }
                self.tables.push(Table {
                    base: None,
                    schema,
                    name: rv.relname.clone(),
                    alive: true,
                    parent,
                    parents: parent.into_iter().collect(),
                    columns,
                });
                effects.push(Effect::TableCreated {
                    table: self.tables.len() - 1,
                });
            }
            NodeEnum::CreateTableAsStmt(s) => {
                let Some(rv) = s.into.as_ref().and_then(|i| i.rel.as_ref()) else {
                    return;
                };
                if s.objtype() != ObjectType::ObjectTable {
                    return;
                }
                self.tables.push(Table {
                    base: None,
                    schema: self.schema_for(&rv.schemaname),
                    name: rv.relname.clone(),
                    alive: true,
                    parent: None,
                    parents: Vec::new(),
                    columns: Vec::new(),
                });
                effects.push(Effect::TableCreated {
                    table: self.tables.len() - 1,
                });
            }
            NodeEnum::RenameStmt(s) => match s.rename_type() {
                ObjectType::ObjectTable
                | ObjectType::ObjectForeignTable
                | ObjectType::ObjectView
                | ObjectType::ObjectMatview
                | ObjectType::ObjectIndex
                | ObjectType::ObjectSequence => {
                    if let Some(table) = s.relation.as_ref().and_then(|rv| self.range(rv)) {
                        self.tables[table].name.clone_from(&s.newname);
                        effects.push(Effect::TableRenamed { table });
                    }
                }
                ObjectType::ObjectColumn => {
                    // Partitions' columns are renamed with their parent's.
                    let reached = s
                        .relation
                        .as_ref()
                        .map(|rv| self.reach(rv))
                        .unwrap_or_default();
                    for table in reached {
                        if let Some(column) = self.column(table, &s.subname) {
                            self.tables[table].columns[column]
                                .name
                                .clone_from(&s.newname);
                            effects.push(Effect::ColumnRenamed { table, column });
                        }
                    }
                }
                ObjectType::ObjectType | ObjectType::ObjectDomain => {
                    let parts = s.object.as_deref().map(type_parts).unwrap_or_default();
                    if let Some(e) = self.resolve_type(&parts) {
                        self.types[e].name.clone_from(&s.newname);
                    }
                }
                _ => {}
            },
            NodeEnum::AlterObjectSchemaStmt(s) => match s.object_type() {
                ObjectType::ObjectTable
                | ObjectType::ObjectForeignTable
                | ObjectType::ObjectView
                | ObjectType::ObjectMatview => {
                    if let Some(table) = s.relation.as_ref().and_then(|rv| self.range(rv)) {
                        self.tables[table].schema.clone_from(&s.newschema);
                        effects.push(Effect::TableRenamed { table });
                    }
                }
                ObjectType::ObjectType | ObjectType::ObjectDomain => {
                    let parts = s.object.as_deref().map(type_parts).unwrap_or_default();
                    if let Some(e) = self.resolve_type(&parts) {
                        self.types[e].schema.clone_from(&s.newschema);
                    }
                }
                _ => {}
            },
            NodeEnum::AlterTableStmt(s) => {
                let Some(rv) = s.relation.as_ref() else {
                    return;
                };
                let Some(table) = self.range(rv) else { return };
                for cmd in &s.cmds {
                    let Some(NodeEnum::AlterTableCmd(cmd)) = cmd.node.as_ref() else {
                        continue;
                    };
                    // Column changes reach the partitions (`ONLY` aside);
                    // attaching and detaching is the parent's alone.
                    let reached = match cmd.subtype() {
                        AlterTableType::AtAttachPartition | AlterTableType::AtDetachPartition => {
                            vec![table]
                        }
                        _ => self.reach(rv),
                    };
                    for t in reached {
                        self.alter(t, cmd, effects);
                    }
                }
            }
            NodeEnum::IndexStmt(s) => {
                // Duplicates fail a unique index; an expression or a
                // predicate is evaluated on every row.
                let rows = s.unique
                    || s.where_clause.is_some()
                    || s.index_params.iter().any(|p| {
                        matches!(p.node.as_ref(), Some(NodeEnum::IndexElem(e)) if e.expr.is_some())
                    });
                for table in s
                    .relation
                    .as_ref()
                    .map(|rv| self.reach(rv))
                    .unwrap_or_default()
                {
                    effects.push(Effect::Altered { table, rows });
                }
            }
            NodeEnum::DropStmt(s) => match s.remove_type() {
                ObjectType::ObjectTable | ObjectType::ObjectForeignTable => {
                    for object in &s.objects {
                        let Some(NodeEnum::List(list)) = object.node.as_ref() else {
                            continue;
                        };
                        let parts = names(&list.items);
                        let found = match parts.as_slice() {
                            [schema, name] => self.resolve(schema, name),
                            [name] => self.resolve("", name),
                            _ => None,
                        };
                        if let Some(table) = found {
                            self.drop_table(table, effects);
                        }
                    }
                }
                ObjectType::ObjectSchema if s.behavior() == DropBehavior::DropCascade => {
                    for object in &s.objects {
                        let Some(NodeEnum::String(schema)) = object.node.as_ref() else {
                            continue;
                        };
                        let in_it: Vec<usize> = (0..self.tables.len())
                            .filter(|&i| {
                                self.tables[i].alive && self.tables[i].schema == schema.sval
                            })
                            .collect();
                        for table in in_it {
                            self.drop_table(table, effects);
                        }
                    }
                }
                ObjectType::ObjectType | ObjectType::ObjectDomain => {
                    for object in &s.objects {
                        let parts = type_parts(object);
                        let Some(e) = self.resolve_type(&parts) else {
                            continue;
                        };
                        self.types[e].alive = false;
                        // Its columns go too (and those of domains over
                        // it): exactly those, nothing else.
                        if s.behavior() == DropBehavior::DropCascade {
                            for table in 0..self.tables.len() {
                                if !self.tables[table].alive {
                                    continue;
                                }
                                for column in 0..self.tables[table].columns.len() {
                                    let c = &mut self.tables[table].columns[column];
                                    if c.alive && c.types.contains(&e) {
                                        c.alive = false;
                                        effects.push(Effect::ColumnDropped { table, column });
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            },
            // Row statements on a partitioned table reach its partitions.
            NodeEnum::TruncateStmt(s) => {
                for relation in &s.relations {
                    if let Some(NodeEnum::RangeVar(rv)) = relation.node.as_ref() {
                        for table in self.reach(rv) {
                            effects.push(Effect::RowsDeleted { table, all: true });
                        }
                    }
                }
            }
            NodeEnum::DeleteStmt(s) => {
                for table in s
                    .relation
                    .as_ref()
                    .map(|rv| self.reach(rv))
                    .unwrap_or_default()
                {
                    effects.push(Effect::RowsDeleted {
                        table,
                        all: s.where_clause.is_none(),
                    });
                }
            }
            NodeEnum::UpdateStmt(s) => {
                for table in s
                    .relation
                    .as_ref()
                    .map(|rv| self.reach(rv))
                    .unwrap_or_default()
                {
                    let effect = self.changed(table, &s.target_list);
                    effects.push(effect);
                }
            }
            NodeEnum::InsertStmt(s) => {
                for table in s
                    .relation
                    .as_ref()
                    .map(|rv| self.reach(rv))
                    .unwrap_or_default()
                {
                    effects.push(Effect::RowsAdded { table });
                    // `ON CONFLICT DO UPDATE` changes the rows it meets.
                    if let Some(conflict) = &s.on_conflict_clause
                        && conflict.action() == protobuf::OnConflictAction::OnconflictUpdate
                    {
                        let effect = self.changed(table, &conflict.target_list);
                        effects.push(effect);
                    }
                }
            }
            NodeEnum::CopyStmt(s) if s.is_from => {
                for table in s
                    .relation
                    .as_ref()
                    .map(|rv| self.reach(rv))
                    .unwrap_or_default()
                {
                    effects.push(Effect::RowsAdded { table });
                }
            }
            NodeEnum::AlterEnumStmt(s) if !s.old_val.is_empty() => {
                if let Some(enum_type) = self.resolve_type(&names(&s.type_name)) {
                    effects.push(Effect::LabelRenamed {
                        enum_type,
                        from: s.old_val.clone(),
                        to: s.new_val.clone(),
                    });
                }
            }
            NodeEnum::CreateEnumStmt(s) => self.create_type(&names(&s.type_name)),
            NodeEnum::CreateDomainStmt(s) => self.create_type(&names(&s.domainname)),
            NodeEnum::CompositeTypeStmt(s) => {
                if let Some(rv) = &s.typevar {
                    let parts = if rv.schemaname.is_empty() {
                        vec![rv.relname.clone()]
                    } else {
                        vec![rv.schemaname.clone(), rv.relname.clone()]
                    };
                    self.create_type(&parts);
                }
            }
            NodeEnum::VariableSetStmt(s)
                if s.name == "search_path" && s.kind() == VariableSetKind::VarSetValue =>
            {
                let path: Vec<String> = s
                    .args
                    .iter()
                    .filter_map(|a| match a.node.as_ref() {
                        Some(NodeEnum::AConst(c)) => match c.val.as_ref() {
                            Some(protobuf::a_const::Val::Sval(s)) => Some(s.sval.clone()),
                            _ => None,
                        },
                        _ => None,
                    })
                    .filter(|s| s != "$user" && s != "pg_catalog")
                    .collect();
                if !path.is_empty() {
                    self.search_path = path;
                }
            }
            NodeEnum::DoStmt(_) | NodeEnum::CallStmt(_) | NodeEnum::MergeStmt(_) => {
                effects.push(Effect::Opaque);
            }
            _ => {}
        }
    }
}

/// Every statement's effects, as a walk from `model` reads them, and the
/// walk where it ends.
pub fn walk<'a>(
    model_walk: Walk,
    statements: impl IntoIterator<Item = &'a str>,
) -> (Walk, Vec<Vec<Effect>>) {
    let mut walk = model_walk;
    let effects = statements
        .into_iter()
        .map(|sql| walk.statement(sql))
        .collect();
    (walk, effects)
}

#[cfg(test)]
mod tests {
    use remus_core::Schema;

    use super::*;

    fn model() -> Schema {
        let column = |name: &str, ty: &str| {
            serde_json::json!({
                "name": name, "type": ty, "type_schema": "pg_catalog", "type_name": ty,
                "type_kind": "b", "not_null": false, "default": null, "identity": null,
                "generated": null, "comment": null
            })
        };
        let table = |name: &str, columns: Vec<serde_json::Value>| {
            serde_json::json!({
                "schema": "public", "name": name, "kind": "r", "comment": null,
                "partition_key": null, "view_definition": null, "columns": columns
            })
        };
        Schema::from_json(
            &serde_json::json!({
                "remus_version": "0.2", "database": "t", "server_version": "17",
                "entities": [
                    table("users", vec![column("id", "integer"), column("email", "text"), column("legacy", "text")]),
                    table("orders", vec![column("id", "integer"), column("total", "numeric")]),
                ],
                "enums": [{"schema": "public", "name": "mood", "labels": ["ok", "sad"], "comment": null}]
            })
            .to_string(),
        )
        .unwrap()
    }

    fn run(statements: &[&str]) -> (Walk, Vec<Vec<Effect>>) {
        walk(Walk::new(&model()), statements.iter().copied())
    }

    #[test]
    fn what_rows_matter_to_is_told_apart() {
        let (_, effects) = run(&[
            "CREATE INDEX orders_total_idx ON orders (total);",
            "CREATE UNIQUE INDEX orders_total_key ON orders (total);",
            "CREATE INDEX orders_half_idx ON orders ((total / 2));",
            "ALTER TABLE orders ADD COLUMN memo text;",
            "ALTER TABLE orders ADD COLUMN code text NOT NULL;",
            "ALTER TABLE orders ADD COLUMN paid boolean NOT NULL DEFAULT false;",
            "ALTER TABLE orders ADD COLUMN feeling mood;",
            "ALTER TABLE orders ALTER COLUMN total SET NOT NULL;",
            "ALTER TABLE orders ALTER COLUMN total DROP NOT NULL, ALTER COLUMN total SET DEFAULT 0;",
            "ALTER TABLE orders ADD CONSTRAINT positive CHECK (total > 0) NOT VALID;",
            "ALTER TABLE orders ADD CONSTRAINT positive_too CHECK (total > 0);",
            "ALTER TABLE orders RENAME COLUMN total TO amount;",
        ]);
        let needs: Vec<bool> = effects
            .iter()
            .map(|e| e.iter().any(Effect::needs_rows))
            .collect();
        assert_eq!(
            needs,
            [
                false, true, true, false, true, false, true, true, false, false, true, false
            ]
        );
    }

    #[test]
    fn renames_are_followed_to_the_base_objects() {
        let (walk, effects) = run(&[
            "ALTER TABLE public.users RENAME COLUMN email TO email_address;",
            "ALTER TABLE users RENAME TO members;",
            "ALTER TABLE members RENAME COLUMN email_address TO contact;",
        ]);
        let members = walk.find("public", "members").unwrap();
        assert_eq!(walk.of_base("public", "users"), Some(members));
        let contact = walk.column_of_base(members, "email").unwrap();
        assert_eq!(walk.tables[members].columns[contact].name, "contact");
        assert_eq!(effects[1], [Effect::TableRenamed { table: members }]);
        assert_eq!(
            effects[2],
            [Effect::ColumnRenamed {
                table: members,
                column: contact
            }]
        );
    }

    #[test]
    fn conversions_keep_their_using_and_the_names_of_the_moment() {
        let (walk, effects) = run(&[
            "ALTER TABLE orders RENAME COLUMN total TO amount;",
            "ALTER TABLE public.orders ALTER COLUMN amount DROP DEFAULT, ALTER COLUMN amount TYPE integer USING amount::integer;",
            "ALTER TABLE orders ALTER COLUMN id TYPE bigint;",
        ]);
        let orders = walk.find("public", "orders").unwrap();
        // `DROP DEFAULT` alters the table (rows do not matter to it); the
        // conversion follows.
        assert_eq!(
            effects[1][0],
            Effect::Altered {
                table: orders,
                rows: false
            }
        );
        let Effect::ColumnConverted {
            column, conversion, ..
        } = &effects[1][1]
        else {
            panic!("a conversion: {:?}", effects[1]);
        };
        assert_eq!(
            walk.tables[orders].columns[*column].base.as_deref(),
            Some("total")
        );
        assert_eq!(conversion.to, "int");
        assert_eq!(conversion.using.as_deref(), Some("amount::int"));
        assert!(conversion.names.contains(&("amount".to_owned(), *column)));
        let Effect::ColumnConverted { conversion, .. } = &effects[2][0] else {
            panic!("a conversion");
        };
        assert_eq!(
            (conversion.to.as_str(), conversion.using.as_ref()),
            ("bigint", None)
        );
    }

    #[test]
    fn a_table_dropped_and_created_again_is_another_table() {
        let (walk, effects) = run(&[
            "DROP TABLE orders;",
            "CREATE TABLE orders (id bigint PRIMARY KEY, total numeric);",
        ]);
        assert!(matches!(effects[0][..], [Effect::TableDropped { .. }]));
        let now = walk.find("public", "orders").unwrap();
        assert_eq!(walk.tables[now].base, None);
        assert_eq!(walk.of_base("public", "orders"), None);
    }

    #[test]
    fn data_statements_say_what_they_touch() {
        let (walk, effects) = run(&[
            "SET search_path TO public;",
            "UPDATE users SET legacy = upper(legacy) WHERE id > 1;",
            "DELETE FROM orders WHERE total < 0;",
            "TRUNCATE users;",
            "ALTER TABLE users DROP COLUMN legacy;",
            "ALTER TYPE mood RENAME VALUE 'sad' TO 'blue';",
            "DO $$ BEGIN PERFORM 1; END $$;",
        ]);
        let users = walk.find("public", "users").unwrap();
        let orders = walk.find("public", "orders").unwrap();
        assert_eq!(
            effects[1],
            [Effect::ValuesChanged {
                table: users,
                columns: Some(vec![2])
            }]
        );
        assert_eq!(
            effects[2],
            [Effect::RowsDeleted {
                table: orders,
                all: false
            }]
        );
        assert_eq!(
            effects[3],
            [Effect::RowsDeleted {
                table: users,
                all: true
            }]
        );
        assert_eq!(
            effects[4],
            [Effect::ColumnDropped {
                table: users,
                column: 2
            }]
        );
        assert_eq!(walk.column_of_base(users, "legacy"), None);
        assert_eq!(
            effects[5],
            [Effect::LabelRenamed {
                enum_type: 0,
                from: "sad".into(),
                to: "blue".into()
            }]
        );
        assert_eq!(effects[6], [Effect::Opaque]);
    }

    fn typed_model() -> Schema {
        let column = |name: &str, ty: &str, kind: &str| {
            serde_json::json!({
                "name": name, "type": ty, "type_schema": if kind == "b" { "pg_catalog" } else { "public" },
                "type_name": ty, "type_kind": kind, "not_null": false, "default": null,
                "identity": null, "generated": null, "comment": null
            })
        };
        let table = |name: &str, kind: &str, columns: Vec<serde_json::Value>| {
            serde_json::json!({
                "schema": "public", "name": name, "kind": kind, "comment": null,
                "partition_key": if kind == "p" { serde_json::json!("RANGE (at)") } else { serde_json::Value::Null },
                "view_definition": null, "columns": columns
            })
        };
        Schema::from_json(
            &serde_json::json!({
                "remus_version": "0.2", "database": "t", "server_version": "17",
                "entities": [
                    table("people", "r", vec![column("id", "integer", "b"), column("feeling", "mood", "e"), column("note", "text", "b")]),
                    table("events", "p", vec![column("id", "integer", "b"), column("at", "date", "b"), column("kind", "text", "b")]),
                    table("events_2026", "r", vec![column("id", "integer", "b"), column("at", "date", "b"), column("kind", "text", "b")]),
                ],
                "partitions": [{
                    "parent_schema": "public", "parent": "events",
                    "child_schema": "public", "child": "events_2026",
                    "bound": "FOR VALUES FROM ('2026-01-01') TO ('2027-01-01')"
                }],
                "enums": [{"schema": "public", "name": "mood", "labels": ["ok", "sad"], "comment": null}]
            })
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn a_conversion_reads_its_types_under_their_names_at_the_end() {
        let (walk, effects) = walk(
            Walk::new(&typed_model()),
            [
                "CREATE TYPE mood_new AS ENUM ('ok', 'blue');",
                "ALTER TABLE people ALTER COLUMN feeling TYPE mood_new USING feeling::text::mood_new;",
                "DROP TYPE mood;",
                "ALTER TYPE mood_new RENAME TO mood;",
            ],
        );
        let Effect::ColumnConverted { conversion, .. } = &effects[1][0] else {
            panic!("a conversion: {:?}", effects[1]);
        };
        assert_eq!(conversion.to, "mood_new");
        let (to, using) = conversion.resolved(&walk);
        assert_eq!(to, r#""public"."mood""#);
        assert_eq!(using.as_deref(), Some(r#"feeling::text::"public"."mood""#));
    }

    #[test]
    fn a_partitioned_table_s_changes_reach_its_partitions() {
        let (walk, effects) = walk(
            Walk::new(&typed_model()),
            [
                "ALTER TABLE events RENAME COLUMN kind TO category;",
                "DELETE FROM events WHERE at < '2026-06-01';",
                "ALTER TABLE events DROP COLUMN category;",
                "CREATE TABLE events_2027 PARTITION OF events FOR VALUES FROM ('2027-01-01') TO ('2028-01-01');",
            ],
        );
        let child = walk.find("public", "events_2026").unwrap();
        assert!(effects[0].contains(&Effect::ColumnRenamed {
            table: child,
            column: 2
        }));
        assert!(effects[1].contains(&Effect::RowsDeleted {
            table: child,
            all: false
        }));
        assert!(effects[2].contains(&Effect::ColumnDropped {
            table: child,
            column: 2
        }));
        let new = walk.find("public", "events_2027").unwrap();
        let names: Vec<&str> = walk.tables[new]
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["id", "at"]);
    }

    #[test]
    fn a_type_dropped_with_cascade_takes_its_columns_only() {
        let (walk, effects) = walk(Walk::new(&typed_model()), ["DROP TYPE mood CASCADE;"]);
        let people = walk.find("public", "people").unwrap();
        assert_eq!(
            effects[0],
            [Effect::ColumnDropped {
                table: people,
                column: 1
            }]
        );
        assert!(walk.column(people, "note").is_some());
    }

    #[test]
    fn a_swap_through_a_temporary_name_lands_each_on_its_own() {
        let (walk, _) = run(&[
            "ALTER TABLE users RENAME COLUMN email TO romulus_swap;",
            "ALTER TABLE users RENAME COLUMN legacy TO email;",
            "ALTER TABLE users RENAME COLUMN romulus_swap TO legacy;",
        ]);
        let users = walk.find("public", "users").unwrap();
        let email = walk.column(users, "email").unwrap();
        assert_eq!(
            walk.tables[users].columns[email].base.as_deref(),
            Some("legacy")
        );
        let legacy = walk.column(users, "legacy").unwrap();
        assert_eq!(
            walk.tables[users].columns[legacy].base.as_deref(),
            Some("email")
        );
    }
}
