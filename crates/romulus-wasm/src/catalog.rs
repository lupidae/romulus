//! Postgres's catalogs in miniature: what the statements created so far,
//! with the ids Postgres keys things by (so a rename reaches every
//! definition that mentions the renamed object), and the remus model it
//! amounts to, as `introspect.sql` would read it.

use std::{collections::BTreeSet, fmt::Write as _};

use remus_core::{
    Schema,
    emit::ident::{quote_ident, quote_literal},
    model::{
        self, ConstraintKind, EntityKind, Generated, Identity, PolicyCommand, ReferentialAction,
        RelationRef,
    },
};

use crate::{
    ddl::Fail,
    expr::{ColumnRef, Expr, Names, Printer},
    types::{Base, Ty, UserKind},
    view::{Query, print_view},
};

/// What the model says it was compiled on: the app's default major.
pub const SERVER_VERSION: &str = "17.7";
/// The model's `database` before the caller names it (as the sandbox's).
pub const DATABASE: &str = "sandbox";

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// Schemas that exist: `public`, then those created.
    pub schemas: Vec<String>,
    /// Where unqualified names resolve, then are created (`SET search_path`).
    pub search_path: Vec<String>,
    pub enums: Vec<EnumType>,
    pub domains: Vec<Domain>,
    pub composites: Vec<Composite>,
    /// Range types: no remus object, but columns may use them.
    pub ranges: Vec<(String, String)>,
    pub extensions: Vec<Extension>,
    pub rels: Vec<Rel>,
    /// Sequences (serial and identity columns' too): names only.
    pub sequences: Vec<(String, String)>,
    next_id: u32,
}

#[derive(Debug, Clone)]
pub struct Extension {
    pub name: String,
    pub schema: String,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct EnumType {
    pub schema: String,
    pub name: String,
    pub labels: Vec<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Domain {
    pub schema: String,
    pub name: String,
    pub base: Ty,
    pub not_null: bool,
    pub default: Option<Expr>,
    /// Name and body of each CHECK.
    pub checks: Vec<(String, Expr)>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Composite {
    pub schema: String,
    pub name: String,
    pub attributes: Vec<(String, Ty)>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelKind {
    Table,
    PartitionedTable,
    View,
    MaterializedView,
}

impl RelKind {
    pub fn is_table(self) -> bool {
        matches!(self, Self::Table | Self::PartitionedTable)
    }

    /// The word Postgres uses for it in messages.
    pub fn word(self) -> &'static str {
        match self {
            Self::Table | Self::PartitionedTable => "table",
            Self::View => "view",
            Self::MaterializedView => "materialized view",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Rel {
    pub id: u32,
    pub schema: String,
    pub name: String,
    pub kind: RelKind,
    pub columns: Vec<Column>,
    pub constraints: Vec<Constraint>,
    pub indexes: Vec<Index>,
    pub policies: Vec<Policy>,
    pub comment: Option<String>,
    pub rls: bool,
    pub rls_forced: bool,
    pub unlogged: bool,
    pub partition_key: Option<PartitionKey>,
    /// A partition: its parent and its bound (`FOR VALUES …`, `DEFAULT`).
    pub partition_of: Option<(u32, String)>,
    pub view: Option<Query>,
}

#[derive(Debug, Clone)]
pub struct PartitionKey {
    /// `RANGE`, `LIST`, `HASH`.
    pub strategy: &'static str,
    pub elements: Vec<PartitionElem>,
}

#[derive(Debug, Clone)]
pub enum PartitionElem {
    Column(u32),
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct Column {
    pub id: u32,
    pub name: String,
    pub ty: Ty,
    pub not_null: bool,
    pub default: Option<Expr>,
    pub identity: Option<Identity>,
    pub generated: bool,
    pub comment: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // It mirrors pg_constraint's flags.
pub struct Constraint {
    pub name: String,
    pub kind: ConstraintKind,
    pub columns: Vec<u32>,
    pub check: Option<Expr>,
    pub foreign: Option<ForeignKey>,
    pub include: Vec<u32>,
    pub nulls_not_distinct: bool,
    pub deferrable: bool,
    pub initially_deferred: bool,
    pub not_valid: bool,
    pub no_inherit: bool,
    /// An exclusion constraint's definition, as written.
    pub exclusion: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ForeignKey {
    pub target: u32,
    pub columns: Vec<u32>,
    pub on_update: ReferentialAction,
    pub on_delete: ReferentialAction,
    pub match_full: bool,
}

#[derive(Debug, Clone)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    pub method: String,
    pub elements: Vec<IndexElem>,
    pub include: Vec<u32>,
    pub predicate: Option<Expr>,
    pub nulls_not_distinct: bool,
    /// `WITH (…)`, as written.
    pub options: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IndexElem {
    pub key: PartitionElem,
    pub collation: Option<String>,
    pub opclass: Option<String>,
    pub desc: bool,
    /// `Some(true)`: `NULLS FIRST`; `Some(false)`: `NULLS LAST`.
    pub nulls_first: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub name: String,
    pub command: PolicyCommand,
    pub permissive: bool,
    pub roles: Vec<String>,
    pub using: Option<Expr>,
    pub check: Option<Expr>,
}

impl Catalog {
    pub fn new() -> Self {
        Self {
            schemas: vec!["public".to_owned()],
            search_path: vec!["public".to_owned()],
            extensions: Vec::new(),
            ..Self::default()
        }
    }

    pub fn next_id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    pub fn rel(&self, id: u32) -> Option<&Rel> {
        self.rels.iter().find(|r| r.id == id)
    }

    pub fn rel_mut(&mut self, id: u32) -> Option<&mut Rel> {
        self.rels.iter_mut().find(|r| r.id == id)
    }

    /// The relation `id`, which the run found or made: its absence is this
    /// engine's failure, not Postgres's error.
    pub fn get(&self, id: u32) -> Result<&Rel, Fail> {
        self.rel(id).ok_or_else(Fail::internal)
    }

    pub fn get_mut(&mut self, id: u32) -> Result<&mut Rel, Fail> {
        self.rel_mut(id).ok_or_else(Fail::internal)
    }

    /// The enum, domain or composite at `index`, which the run found.
    pub fn enum_mut(&mut self, index: usize) -> Result<&mut EnumType, Fail> {
        self.enums.get_mut(index).ok_or_else(Fail::internal)
    }

    pub fn domain_mut(&mut self, index: usize) -> Result<&mut Domain, Fail> {
        self.domains.get_mut(index).ok_or_else(Fail::internal)
    }

    pub fn composite_mut(&mut self, index: usize) -> Result<&mut Composite, Fail> {
        self.composites.get_mut(index).ok_or_else(Fail::internal)
    }

    /// Schemas searched for an unqualified name: the path's that exist.
    pub fn path(&self) -> impl Iterator<Item = &String> {
        self.search_path.iter().filter(|s| self.schemas.contains(s))
    }

    /// The relation `schema.name`, or `name` on the path.
    pub fn find_rel(&self, schema: Option<&str>, name: &str) -> Option<&Rel> {
        match schema {
            Some(schema) => self
                .rels
                .iter()
                .find(|r| r.schema == schema && r.name == name),
            None => self
                .path()
                .find_map(|s| self.rels.iter().find(|r| &r.schema == s && r.name == name)),
        }
    }

    /// Whether a relation-like name (table, view, index, sequence,
    /// composite type) is taken in `schema`.
    pub fn relation_name_taken(&self, schema: &str, name: &str) -> bool {
        self.rels.iter().any(|r| {
            r.schema == schema
                && (r.name == name
                    || r.indexes.iter().any(|i| i.name == name)
                    || r.constraints.iter().any(|c| {
                        c.name == name
                            && matches!(
                                c.kind,
                                ConstraintKind::PrimaryKey
                                    | ConstraintKind::Unique
                                    | ConstraintKind::Exclusion
                            )
                    }))
        }) || self.sequences.iter().any(|(s, n)| s == schema && n == name)
            || self
                .composites
                .iter()
                .any(|c| c.schema == schema && c.name == name)
    }

    /// Whether a constraint of any table of `schema` has the name: what a
    /// default constraint name must avoid.
    pub fn constraint_name_taken(&self, schema: &str, name: &str) -> bool {
        self.rels
            .iter()
            .any(|r| r.schema == schema && r.constraints.iter().any(|c| c.name == name))
            || self
                .domains
                .iter()
                .any(|d| d.schema == schema && d.checks.iter().any(|(n, _)| n == name))
    }

    /// Whether a type of that name exists in `schema` (tables have one too).
    pub fn type_name_taken(&self, schema: &str, name: &str) -> bool {
        self.enums
            .iter()
            .any(|t| t.schema == schema && t.name == name)
            || self
                .domains
                .iter()
                .any(|t| t.schema == schema && t.name == name)
            || self
                .composites
                .iter()
                .any(|t| t.schema == schema && t.name == name)
            || self.ranges.iter().any(|(s, n)| s == schema && n == name)
            || self
                .rels
                .iter()
                .any(|r| r.schema == schema && r.name == name)
    }

    /// A user type, by schema (or the path) and name.
    pub fn find_type(&self, schema: Option<&str>, name: &str) -> Option<Base> {
        let in_schema = |s: &str| -> Option<Base> {
            let user = |kind| {
                Some(Base::User {
                    schema: s.to_owned(),
                    name: name.to_owned(),
                    kind,
                })
            };
            if self.enums.iter().any(|t| t.schema == s && t.name == name) {
                return user(UserKind::Enum);
            }
            if self.domains.iter().any(|t| t.schema == s && t.name == name) {
                return user(UserKind::Domain);
            }
            if self
                .composites
                .iter()
                .any(|t| t.schema == s && t.name == name)
            {
                return user(UserKind::Composite);
            }
            if self.ranges.iter().any(|(rs, n)| rs == s && n == name) {
                return user(UserKind::Range);
            }
            if self.extensions.iter().any(|e| {
                e.schema == s
                    && crate::types::extension(&e.name).is_some_and(|x| x.types.contains(&name))
            }) {
                return user(UserKind::Extension);
            }
            None
        };
        match schema {
            Some(s) => in_schema(s),
            None => self.path().find_map(|s| in_schema(s)),
        }
    }

    pub fn domain(&self, schema: &str, name: &str) -> Option<&Domain> {
        self.domains
            .iter()
            .find(|d| d.schema == schema && d.name == name)
    }

    /// The type a column of type `ty` behaves as (a domain's base type).
    pub fn base_of(&self, ty: &Ty) -> Ty {
        match &ty.base {
            Base::User {
                schema,
                name,
                kind: UserKind::Domain,
            } if !ty.array => self
                .domain(schema, name)
                .map_or_else(|| ty.clone(), |d| self.base_of(&d.base)),
            _ => ty.clone(),
        }
    }

    /// The column of `rel` named so.
    pub fn column(&self, rel: u32, name: &str) -> Option<&Column> {
        self.rel(rel)?.columns.iter().find(|c| c.name == name)
    }

    pub fn column_name(&self, column: ColumnRef) -> String {
        self.rel(column.rel)
            .and_then(|r| r.columns.iter().find(|c| c.id == column.col))
            .map_or_else(|| "?".to_owned(), |c| c.name.clone())
    }

    /// `schema.name`, quoted where needed: how definitions name relations.
    pub fn qualified(&self, rel: u32) -> String {
        self.rel(rel).map_or_else(
            || "?".to_owned(),
            |r| format!("{}.{}", quote_ident(&r.schema), quote_ident(&r.name)),
        )
    }

    // ---- The model --------------------------------------------------------------

    /// The remus model of everything created, as `introspect.sql` reads a
    /// database: entities by schema and name (partitions under their
    /// parent), constraints by kind then name, indexes and policies by name.
    pub fn model(&self) -> Schema {
        let mut entities: Vec<model::Entity> = self
            .rels
            .iter()
            .filter(|r| r.partition_of.is_none())
            .map(|r| self.entity(r))
            .collect();
        entities.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        let mut enums: Vec<model::EnumType> = self
            .enums
            .iter()
            .map(|e| model::EnumType {
                schema: e.schema.clone(),
                name: e.name.clone(),
                labels: e.labels.clone(),
                comment: e.comment.clone(),
            })
            .collect();
        enums.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        let mut domains: Vec<model::DomainType> =
            self.domains.iter().map(Self::domain_model).collect();
        domains.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        let mut composites: Vec<model::CompositeType> = self
            .composites
            .iter()
            .map(|c| model::CompositeType {
                schema: c.schema.clone(),
                name: c.name.clone(),
                attributes: c
                    .attributes
                    .iter()
                    .map(|(name, ty)| model::CompositeAttribute {
                        name: name.clone(),
                        r#type: ty.display(),
                    })
                    .collect(),
                comment: c.comment.clone(),
            })
            .collect();
        composites.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        let mut partitions: Vec<model::Partition> = self
            .rels
            .iter()
            .filter_map(|r| {
                let (parent, bound) = r.partition_of.as_ref()?;
                let parent = self.rel(*parent)?;
                Some(model::Partition {
                    parent_schema: parent.schema.clone(),
                    parent: parent.name.clone(),
                    child_schema: r.schema.clone(),
                    child: r.name.clone(),
                    bound: Some(bound.clone()),
                    partition_key: r.partition_key.as_ref().map(|k| self.partition_key(r, k)),
                })
            })
            .collect();
        partitions.sort_by(|a, b| {
            (&a.parent_schema, &a.parent, &a.child_schema, &a.child).cmp(&(
                &b.parent_schema,
                &b.parent,
                &b.child_schema,
                &b.child,
            ))
        });
        let mut extensions: Vec<model::Extension> = self
            .extensions
            .iter()
            .filter(|e| e.name != "plpgsql")
            .map(|e| model::Extension {
                name: e.name.clone(),
                schema: e.schema.clone(),
                version: e.version.clone(),
            })
            .collect();
        extensions.sort_by(|a, b| a.name.cmp(&b.name));
        Schema {
            remus_version: "0.1".to_owned(),
            generated_at: None,
            database: DATABASE.to_owned(),
            server_version: SERVER_VERSION.to_owned(),
            entities,
            enums,
            domains,
            composites,
            partitions,
            extensions,
        }
    }

    fn domain_model(d: &Domain) -> model::DomainType {
        let names = NoColumns;
        let printer = Printer {
            names: &names,
            pretty: false,
        };
        let mut constraints: Vec<(String, String)> = d
            .checks
            .iter()
            .map(|(name, e)| (name.clone(), format!("CHECK ({})", printer.print(e))))
            .collect();
        constraints.sort();
        model::DomainType {
            schema: d.schema.clone(),
            name: d.name.clone(),
            base_type: d.base.display(),
            not_null: d.not_null,
            default: d.default.as_ref().map(|e| printer.print(e)),
            constraints: constraints.into_iter().map(|(_, c)| c).collect(),
            comment: d.comment.clone(),
        }
    }

    fn entity(&self, rel: &Rel) -> model::Entity {
        let names = TableNames { catalog: self, rel };
        let printer = Printer {
            names: &names,
            pretty: false,
        };
        let kind = match rel.kind {
            RelKind::Table => EntityKind::Table,
            RelKind::PartitionedTable => EntityKind::PartitionedTable,
            RelKind::View => EntityKind::View,
            RelKind::MaterializedView => EntityKind::MaterializedView,
        };
        let columns = rel
            .columns
            .iter()
            .map(|c| {
                let element = c.ty.element();
                let (type_schema, type_name) = element.catalog_name();
                model::Column {
                    name: c.name.clone(),
                    r#type: c.ty.display(),
                    type_schema,
                    type_name,
                    type_kind: element.kind(),
                    is_array: c.ty.array,
                    not_null: c.not_null,
                    default: c.default.as_ref().map(|e| printer.print(e)),
                    identity: c.identity.clone(),
                    generated: c.generated.then_some(Generated::Stored),
                    comment: c.comment.clone(),
                }
            })
            .collect();
        let mut constraints: Vec<model::Constraint> = rel
            .constraints
            .iter()
            .map(|c| self.constraint(rel, c, &printer))
            .collect();
        constraints
            .sort_by(|a, b| (kind_code(&a.kind), &a.name).cmp(&(kind_code(&b.kind), &b.name)));
        let mut indexes: Vec<model::Index> = rel
            .indexes
            .iter()
            .map(|i| model::Index {
                name: i.name.clone(),
                unique: i.unique,
                partial: i.predicate.is_some(),
                definition: self.index_definition(rel, i, &printer),
            })
            .collect();
        indexes.sort_by(|a, b| a.name.cmp(&b.name));
        let mut policies: Vec<model::Policy> = rel
            .policies
            .iter()
            .map(|p| model::Policy {
                name: p.name.clone(),
                command: p.command.clone(),
                permissive: p.permissive,
                roles: p.roles.clone(),
                using: p.using.as_ref().map(|e| printer.print(e)),
                check: p.check.as_ref().map(|e| printer.print(e)),
            })
            .collect();
        policies.sort_by(|a, b| a.name.cmp(&b.name));
        let (view_definition, depends_on) = match &rel.view {
            Some(query) => {
                let mut depends: Vec<RelationRef> = query
                    .relations()
                    .into_iter()
                    .filter_map(|id| self.rel(id))
                    .map(|r| RelationRef {
                        schema: r.schema.clone(),
                        name: r.name.clone(),
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                depends.sort();
                (Some(print_view(self, query)), depends)
            }
            None => (None, Vec::new()),
        };
        model::Entity {
            schema: rel.schema.clone(),
            name: rel.name.clone(),
            kind,
            comment: rel.comment.clone(),
            rls: rel.rls,
            rls_forced: rel.rls_forced,
            unlogged: rel.unlogged,
            partition_key: rel
                .partition_key
                .as_ref()
                .map(|k| self.partition_key(rel, k)),
            view_definition,
            depends_on,
            columns,
            constraints,
            indexes,
            policies,
        }
    }

    fn column_list(rel: &Rel, ids: &[u32]) -> String {
        ids.iter()
            .map(|id| {
                rel.columns
                    .iter()
                    .find(|c| c.id == *id)
                    .map_or_else(|| "?".to_owned(), |c| quote_ident(&c.name).into_owned())
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn column_names(rel: &Rel, ids: &[u32]) -> Vec<String> {
        ids.iter()
            .filter_map(|id| rel.columns.iter().find(|c| c.id == *id))
            .map(|c| c.name.clone())
            .collect()
    }

    /// `pg_get_constraintdef`.
    fn constraint(&self, rel: &Rel, c: &Constraint, printer: &Printer<'_>) -> model::Constraint {
        let mut definition = match c.kind {
            ConstraintKind::PrimaryKey => {
                format!("PRIMARY KEY ({})", Self::column_list(rel, &c.columns))
            }
            ConstraintKind::Unique => format!(
                "UNIQUE {}({})",
                if c.nulls_not_distinct {
                    "NULLS NOT DISTINCT "
                } else {
                    ""
                },
                Self::column_list(rel, &c.columns)
            ),
            ConstraintKind::Check => format!(
                "CHECK ({}){}",
                c.check
                    .as_ref()
                    .map(|e| printer.print(e))
                    .unwrap_or_default(),
                if c.no_inherit { " NO INHERIT" } else { "" }
            ),
            ConstraintKind::ForeignKey => match c.foreign.as_ref() {
                None => "FOREIGN KEY".to_owned(),
                Some(fk) => {
                    let target = self.rel(fk.target);
                    let mut def = format!(
                        "FOREIGN KEY ({}) REFERENCES {}({})",
                        Self::column_list(rel, &c.columns),
                        self.qualified(fk.target),
                        target
                            .map(|t| Self::column_list(t, &fk.columns))
                            .unwrap_or_default()
                    );
                    if fk.match_full {
                        def.push_str(" MATCH FULL");
                    }
                    if let Some(action) = action_words(&fk.on_update) {
                        def.push_str(" ON UPDATE ");
                        def.push_str(action);
                    }
                    if let Some(action) = action_words(&fk.on_delete) {
                        def.push_str(" ON DELETE ");
                        def.push_str(action);
                    }
                    def
                }
            },
            ConstraintKind::Exclusion => c.exclusion.clone().unwrap_or_default(),
        };
        if !c.include.is_empty() {
            let _ = write!(
                definition,
                " INCLUDE ({})",
                Self::column_list(rel, &c.include)
            );
        }
        if c.deferrable {
            definition.push_str(" DEFERRABLE");
            if c.initially_deferred {
                definition.push_str(" INITIALLY DEFERRED");
            }
        }
        if c.not_valid {
            definition.push_str(" NOT VALID");
        }
        let (ref_schema, ref_table, ref_columns, on_update, on_delete) = match &c.foreign {
            Some(fk) => {
                let target = self.rel(fk.target);
                (
                    target.map(|t| t.schema.clone()),
                    target.map(|t| t.name.clone()),
                    target
                        .map(|t| Self::column_names(t, &fk.columns))
                        .unwrap_or_default(),
                    Some(fk.on_update.clone()),
                    Some(fk.on_delete.clone()),
                )
            }
            None => (None, None, Vec::new(), None, None),
        };
        model::Constraint {
            name: c.name.clone(),
            kind: c.kind.clone(),
            columns: Self::constraint_columns(rel, c),
            ref_schema,
            ref_table,
            ref_columns,
            on_update,
            on_delete,
            deferrable: c.deferrable,
            definition,
            comment: c.comment.clone(),
        }
    }

    /// `conkey`, by name. A check's are the columns it reads, in the order
    /// the expression names them (`pull_var_clause`).
    fn constraint_columns(rel: &Rel, c: &Constraint) -> Vec<String> {
        if c.kind != ConstraintKind::Check {
            return Self::column_names(rel, &c.columns);
        }
        let mut read = Vec::new();
        if let Some(e) = &c.check {
            e.columns(&mut read);
        }
        let ids: Vec<u32> = read
            .iter()
            .filter(|r| r.rel == rel.id)
            .map(|r| r.col)
            .collect();
        Self::column_names(rel, &ids)
    }

    /// `pg_get_indexdef`.
    fn index_definition(&self, rel: &Rel, index: &Index, printer: &Printer<'_>) -> String {
        let elements = index
            .elements
            .iter()
            .map(|e| {
                let mut out = match &e.key {
                    PartitionElem::Column(id) => rel
                        .columns
                        .iter()
                        .find(|c| c.id == *id)
                        .map_or_else(|| "?".to_owned(), |c| quote_ident(&c.name).into_owned()),
                    PartitionElem::Expr(expr) => {
                        let text = printer.print(expr);
                        if matches!(expr, Expr::Func { .. }) {
                            text
                        } else {
                            format!("({text})")
                        }
                    }
                };
                if let Some(collation) = &e.collation {
                    out.push_str(" COLLATE ");
                    out.push_str(collation);
                }
                if let Some(opclass) = &e.opclass {
                    out.push(' ');
                    out.push_str(opclass);
                }
                if e.desc {
                    out.push_str(" DESC");
                }
                match (e.nulls_first, e.desc) {
                    (Some(true), false) => out.push_str(" NULLS FIRST"),
                    (Some(false), true) => out.push_str(" NULLS LAST"),
                    _ => {}
                }
                out
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut def = format!(
            "CREATE {}INDEX {} ON {}{} USING {} ({elements})",
            if index.unique { "UNIQUE " } else { "" },
            quote_ident(&index.name),
            if rel.kind == RelKind::PartitionedTable {
                "ONLY "
            } else {
                ""
            },
            self.qualified(rel.id),
            index.method,
        );
        if !index.include.is_empty() {
            let _ = write!(def, " INCLUDE ({})", Self::column_list(rel, &index.include));
        }
        if index.nulls_not_distinct {
            def.push_str(" NULLS NOT DISTINCT");
        }
        if let Some(options) = &index.options {
            def.push_str(" WITH (");
            def.push_str(options);
            def.push(')');
        }
        if let Some(predicate) = &index.predicate {
            def.push_str(" WHERE ");
            def.push_str(&printer.print(predicate));
        }
        def
    }

    /// `pg_get_partkeydef`: `RANGE (created_at)`.
    fn partition_key(&self, rel: &Rel, key: &PartitionKey) -> String {
        let names = TableNames { catalog: self, rel };
        let printer = Printer {
            names: &names,
            pretty: false,
        };
        let elements = key
            .elements
            .iter()
            .map(|e| match e {
                PartitionElem::Column(id) => rel
                    .columns
                    .iter()
                    .find(|c| c.id == *id)
                    .map_or_else(|| "?".to_owned(), |c| quote_ident(&c.name).into_owned()),
                PartitionElem::Expr(expr) => {
                    let text = printer.print(expr);
                    if matches!(expr, Expr::Func { .. }) {
                        text
                    } else {
                        format!("({text})")
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} ({elements})", key.strategy)
    }

    /// A partition bound's value, typed by its key column (`'2024-01-01'`
    /// read as a `timestamptz`): as `pg_get_expr` prints a bound.
    pub fn bound_literal(value: &Expr) -> String {
        match value {
            Expr::Const { value: Some(v), ty }
                if ty.category() == crate::types::Category::Numeric =>
            {
                v.clone()
            }
            Expr::Const { value: Some(v), ty } if ty.is("bool") => v.clone(),
            Expr::Const { value: Some(v), .. } => quote_literal(v),
            Expr::Const { value: None, .. } => "NULL".to_owned(),
            other => {
                let names = NoColumns;
                Printer {
                    names: &names,
                    pretty: false,
                }
                .print(other)
            }
        }
    }
}

/// `introspect.sql` orders constraints by `contype`, the catalog letter.
fn kind_code(kind: &ConstraintKind) -> char {
    match kind {
        ConstraintKind::Check => 'c',
        ConstraintKind::ForeignKey => 'f',
        ConstraintKind::PrimaryKey => 'p',
        ConstraintKind::Unique => 'u',
        ConstraintKind::Exclusion => 'x',
    }
}

fn action_words(action: &ReferentialAction) -> Option<&'static str> {
    match action {
        ReferentialAction::NoAction => None,
        ReferentialAction::Restrict => Some("RESTRICT"),
        ReferentialAction::Cascade => Some("CASCADE"),
        ReferentialAction::SetNull => Some("SET NULL"),
        ReferentialAction::SetDefault => Some("SET DEFAULT"),
    }
}

/// A table's own expressions: columns by their bare names.
pub struct TableNames<'a> {
    pub catalog: &'a Catalog,
    pub rel: &'a Rel,
}

impl Names for TableNames<'_> {
    fn column(&self, column: ColumnRef) -> String {
        let name = if column.rel == self.rel.id {
            self.rel
                .columns
                .iter()
                .find(|c| c.id == column.col)
                .map_or_else(|| "?".to_owned(), |c| c.name.clone())
        } else {
            self.catalog.column_name(column)
        };
        quote_ident(&name).into_owned()
    }
}

/// Expressions without columns (a domain's, a bound).
pub struct NoColumns;

impl Names for NoColumns {
    fn column(&self, _: ColumnRef) -> String {
        "?".to_owned()
    }
}
