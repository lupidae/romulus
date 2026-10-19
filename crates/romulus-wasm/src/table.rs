//! Tables: `CREATE TABLE` (columns, inline and table constraints,
//! partitions), `CREATE INDEX` and `ALTER TABLE`.

use std::fmt::Write as _;

use remus_core::model::{ConstraintKind, Identity, ReferentialAction};
use serde_json::Value;

use crate::{
    catalog::{
        Column, Constraint, ForeignKey, Index, IndexElem, PartitionElem, PartitionKey, Rel, RelKind,
    },
    ddl::{Fail, Result, Run, TableScope, display, resolve_type},
    expr::{self, Analyzer, Expr},
    names::{choose_constraint_name, choose_relation_name, index_column_names, name_addition},
    tree::{self, Name},
    types::{Ty, serial},
};

/// A constraint met in a statement, with the column it was written on.
struct Pending<'a> {
    column: Option<String>,
    fields: &'a Value,
    /// Attributes written after it (`DEFERRABLE`, `INITIALLY DEFERRED`).
    deferrable: bool,
    deferred: bool,
}

impl Run<'_> {
    #[allow(clippy::too_many_lines)] // the statement, clause by clause
    pub fn create_table(&mut self, f: &Value) -> Result<()> {
        let relation = tree::field(f, "relation").unwrap_or(&Value::Null);
        let name = Name::from_range_var(relation);
        let location = tree::location(relation);
        let schema = self.creation_schema(name.schema.as_deref(), location)?;
        if self.catalog.find_rel(Some(&schema), &name.name).is_some()
            || self.catalog.relation_name_taken(&schema, &name.name)
        {
            if tree::bool(f, "if_not_exists") {
                return Ok(());
            }
            return Err(Fail::new(
                "42P07",
                format!("relation \"{}\" already exists", name.name),
            )
            .at(location));
        }
        if self.catalog.type_name_taken(&schema, &name.name) {
            return Err(
                Fail::new("42710", format!("type \"{}\" already exists", name.name)).at(location),
            );
        }

        for (present, clause) in [
            (tree::field(f, "ofTypename").is_some(), "OF"),
            (!tree::list(f, "options").is_empty(), "WITH (…)"),
            (tree::opt_str(f, "accessMethod").is_some(), "USING"),
        ] {
            if present {
                self.not_previewed(&format!("CREATE TABLE … {clause}"));
            }
        }
        let parents = tree::list(f, "inhRelations");
        let bound = tree::field(f, "partbound");
        let mut parent_id = None;
        let mut inherited: Vec<Column> = Vec::new();
        for parent in parents {
            let Some(rv) = tree::as_kind(parent, "RangeVar") else {
                continue;
            };
            let id = self.rel_of(rv)?;
            let parent_rel = self.catalog.get(id)?;
            if bound.is_some() && parent_rel.kind != RelKind::PartitionedTable {
                return Err(Fail::new(
                    "42809",
                    format!("table \"{}\" is not partitioned", parent_rel.name),
                )
                .at(tree::location(rv)));
            }
            parent_id = Some(id);
            for c in &parent_rel.columns {
                if !inherited.iter().any(|i| i.name == c.name) {
                    inherited.push(c.clone());
                }
            }
        }

        let id = self.catalog.next_id();
        let kind = if tree::field(f, "partspec").is_some() {
            RelKind::PartitionedTable
        } else {
            RelKind::Table
        };
        let mut rel = Rel {
            id,
            schema: schema.clone(),
            name: name.name.clone(),
            kind,
            columns: Vec::new(),
            constraints: Vec::new(),
            indexes: Vec::new(),
            policies: Vec::new(),
            comment: None,
            rls: false,
            rls_forced: false,
            unlogged: tree::str(relation, "relpersistence") == "u",
            partition_key: None,
            partition_of: None,
            view: None,
        };
        for mut c in inherited {
            c.id = self.catalog.next_id();
            c.identity = None;
            rel.columns.push(c);
        }
        self.catalog.rels.push(rel);

        let mut pending: Vec<Pending<'_>> = Vec::new();
        let mut generated: Vec<(u32, &Value)> = Vec::new();
        let mut defaults: Vec<(u32, &Value)> = Vec::new();
        for element in tree::list(f, "tableElts") {
            let Some((kind, e)) = tree::kind(element) else {
                continue;
            };
            match kind {
                "ColumnDef" => {
                    let colname = tree::str(e, "colname");
                    if let Some(existing) = self
                        .catalog
                        .rel(id)
                        .and_then(|r| r.columns.iter().find(|c| c.name == colname))
                    {
                        // A partition's (or child's) clauses on an inherited column.
                        let existing = existing.id;
                        if tree::field(e, "typeName").is_some() && parent_id.is_none() {
                            return Err(Fail::new(
                                "42701",
                                format!("column \"{colname}\" specified more than once"),
                            )
                            .at(tree::location(e)));
                        }
                        self.column_clauses(
                            id,
                            existing,
                            e,
                            &mut pending,
                            &mut defaults,
                            &mut generated,
                        )?;
                        continue;
                    }
                    self.add_column(id, e, &mut pending, &mut defaults, &mut generated)?;
                }
                "Constraint" => pending.push(Pending {
                    column: None,
                    fields: e,
                    deferrable: false,
                    deferred: false,
                }),
                "TableLikeClause" => {
                    if tree::int(e, "options") != 0 {
                        self.not_previewed("CREATE TABLE … LIKE … INCLUDING");
                    }
                    let source = tree::field(e, "relation").unwrap_or(&Value::Null);
                    let from = self.rel_of(source)?;
                    let columns = self.catalog.get(from)?.columns.clone();
                    for mut c in columns {
                        if self
                            .catalog
                            .rel(id)
                            .is_some_and(|r| r.columns.iter().any(|x| x.name == c.name))
                        {
                            return Err(Fail::new(
                                "42701",
                                format!("column \"{}\" specified more than once", c.name),
                            ));
                        }
                        c.id = self.catalog.next_id();
                        c.default = None;
                        c.identity = None;
                        c.generated = false;
                        c.comment = None;
                        self.catalog.get_mut(id)?.columns.push(c);
                    }
                }
                _ => self.not_previewed("This CREATE TABLE element"),
            }
        }
        for c in tree::list(f, "constraints") {
            if let Some(c) = tree::as_kind(c, "Constraint") {
                pending.push(Pending {
                    column: None,
                    fields: c,
                    deferrable: false,
                    deferred: false,
                });
            }
        }
        self.defaults(id, &defaults, &generated)?;

        if let Some(spec) = tree::field(f, "partspec") {
            let key = self.partition_key(id, spec)?;
            self.catalog.get_mut(id)?.partition_key = Some(key);
        }
        if let (Some(parent), Some(bound)) = (parent_id, bound) {
            let text = self.bound_text(parent, bound)?;
            self.catalog.get_mut(id)?.partition_of = Some((parent, text));
        }
        self.add_constraints(id, &pending, false)
    }

    /// A column of a `CREATE TABLE` or `ADD COLUMN`; its constraints are
    /// added to `pending`, its default and generation expression analysed
    /// once every column exists.
    fn add_column<'a>(
        &mut self,
        rel: u32,
        e: &'a Value,
        pending: &mut Vec<Pending<'a>>,
        defaults: &mut Vec<(u32, &'a Value)>,
        generated: &mut Vec<(u32, &'a Value)>,
    ) -> Result<u32> {
        let colname = tree::str(e, "colname").to_owned();
        let type_name = tree::field(e, "typeName").unwrap_or(&Value::Null);
        let names = tree::strings(tree::list(type_name, "names"));
        let (ty, sequence) = match names.as_slice() {
            [n] if let Some(base) = serial(n)
                && tree::list(type_name, "arrayBounds").is_empty() =>
            {
                (Ty::builtin(base), true)
            }
            _ => (
                resolve_type(&self.catalog, type_name).map_err(Fail::from)?,
                false,
            ),
        };
        let id = self.catalog.next_id();
        let (schema, table) = {
            let r = self.catalog.get(rel)?;
            (r.schema.clone(), r.name.clone())
        };
        let mut column = Column {
            id,
            name: colname.clone(),
            ty,
            not_null: false,
            default: None,
            identity: None,
            generated: false,
            comment: None,
        };
        if sequence {
            let seq = choose_relation_name(
                &self.catalog,
                &schema,
                &table,
                Some(&colname),
                "seq",
                false,
                &[],
            );
            column.not_null = true;
            column.default = Some(nextval(&schema, &seq));
            self.catalog.sequences.push((schema.clone(), seq));
        }
        self.catalog.get_mut(rel)?.columns.push(column);
        self.column_clauses(rel, id, e, pending, defaults, generated)?;
        Ok(id)
    }

    /// `NOT NULL`, `DEFAULT`, identity, generation and the constraints
    /// written on a column.
    fn column_clauses<'a>(
        &mut self,
        rel: u32,
        id: u32,
        e: &'a Value,
        pending: &mut Vec<Pending<'a>>,
        defaults: &mut Vec<(u32, &'a Value)>,
        generated: &mut Vec<(u32, &'a Value)>,
    ) -> Result<()> {
        let colname = tree::str(e, "colname").to_owned();
        for c in tree::list(e, "constraints") {
            let Some(c) = tree::as_kind(c, "Constraint") else {
                continue;
            };
            match tree::str(c, "contype") {
                "CONSTR_NOTNULL" => self.column_mut(rel, id)?.not_null = true,
                "CONSTR_NULL" => self.column_mut(rel, id)?.not_null = false,
                "CONSTR_DEFAULT" => {
                    if let Some(raw) = tree::field(c, "raw_expr") {
                        defaults.push((id, raw));
                    }
                }
                "CONSTR_IDENTITY" => {
                    let (schema, table) = {
                        let r = self.catalog.get(rel)?;
                        (r.schema.clone(), r.name.clone())
                    };
                    let seq = choose_relation_name(
                        &self.catalog,
                        &schema,
                        &table,
                        Some(&colname),
                        "seq",
                        false,
                        &[],
                    );
                    self.catalog.sequences.push((schema, seq));
                    let column = self.column_mut(rel, id)?;
                    if !column.ty.array && column.ty.category() != crate::types::Category::Numeric {
                        return Err(Fail::new(
                            "22023",
                            "identity column type must be smallint, integer, or bigint",
                        )
                        .at(tree::location(c)));
                    }
                    column.identity = Some(if tree::str(c, "generated_when") == "a" {
                        Identity::Always
                    } else {
                        Identity::ByDefault
                    });
                    column.not_null = true;
                }
                "CONSTR_GENERATED" => {
                    if let Some(raw) = tree::field(c, "raw_expr") {
                        generated.push((id, raw));
                    }
                }
                "CONSTR_ATTR_DEFERRABLE" | "CONSTR_ATTR_DEFERRED" => {
                    if let Some(last) = pending.last_mut() {
                        last.deferrable = true;
                        last.deferred |= tree::str(c, "contype") == "CONSTR_ATTR_DEFERRED";
                    }
                }
                "CONSTR_ATTR_NOT_DEFERRABLE" | "CONSTR_ATTR_IMMEDIATE" => {}
                _ => pending.push(Pending {
                    column: Some(colname.clone()),
                    fields: c,
                    deferrable: false,
                    deferred: false,
                }),
            }
        }
        Ok(())
    }

    fn column_mut(&mut self, rel: u32, id: u32) -> Result<&mut Column> {
        self.catalog
            .rel_mut(rel)
            .and_then(|r| r.columns.iter_mut().find(|c| c.id == id))
            .ok_or_else(Fail::internal)
    }

    /// Defaults (no column references) and generation expressions (other
    /// columns, but not generated ones), coerced to their column's type.
    fn defaults(
        &mut self,
        rel: u32,
        defaults: &[(u32, &Value)],
        generated: &[(u32, &Value)],
    ) -> Result<()> {
        let generated_ids: Vec<u32> = generated.iter().map(|(id, _)| *id).collect();
        for (id, raw) in defaults {
            let expr = {
                let scope = TableScope {
                    catalog: &self.catalog,
                    rel,
                    columns: false,
                    generated: &[],
                };
                Analyzer {
                    sql: self.sql,
                    scope: &scope,
                }
                .expr(raw)?
            };
            let expr = self.coerce_to_column(rel, *id, expr, "default expression", raw)?;
            let column = self.column_mut(rel, *id)?;
            if column.identity.is_some() {
                let column = column.name.clone();
                let table = &self.catalog.get(rel)?.name;
                return Err(Fail::new(
                    "42601",
                    format!(
                        "both default and identity specified for column \"{column}\" of table \"{table}\""
                    ),
                ));
            }
            // `DEFAULT NULL` stores no default.
            column.default = (!matches!(expr, Expr::Const { value: None, .. })).then_some(expr);
        }
        for (id, raw) in generated {
            let expr = {
                let scope = TableScope {
                    catalog: &self.catalog,
                    rel,
                    columns: true,
                    generated: &generated_ids,
                };
                Analyzer {
                    sql: self.sql,
                    scope: &scope,
                }
                .expr(raw)?
            };
            if expr.is_volatile() {
                return Err(Fail::new("42P17", "generation expression is not immutable")
                    .at(location_of(raw)));
            }
            let expr = self.coerce_to_column(rel, *id, expr, "generation expression", raw)?;
            let column = self.column_mut(rel, *id)?;
            column.default = Some(expr);
            column.generated = true;
        }
        Ok(())
    }

    /// A value stored in a column: coerced to its type, or Postgres's error.
    fn coerce_to_column(
        &self,
        rel: u32,
        id: u32,
        expr: Expr,
        what: &str,
        raw: &Value,
    ) -> Result<Expr> {
        let column = self
            .catalog
            .rel(rel)
            .and_then(|r| r.columns.iter().find(|c| c.id == id))
            .ok_or_else(Fail::internal)?;
        let target = self.catalog.base_of(&column.ty);
        let from = self.catalog.base_of(&expr.ty());
        if !expr::assignable(&from, &target) {
            return Err(Fail::new(
                "42804",
                format!(
                    "column \"{}\" is of type {} but {what} is of type {}",
                    column.name,
                    column.ty.display(),
                    expr.ty().display()
                ),
            )
            .hint("You will need to rewrite or cast the expression.")
            .at(location_of(raw)));
        }
        Ok(expr::coerce_assignment(expr, &column.ty))
    }

    fn partition_key(&self, rel: u32, spec: &Value) -> Result<PartitionKey> {
        let strategy = match tree::str(spec, "strategy") {
            "PARTITION_STRATEGY_LIST" => "LIST",
            "PARTITION_STRATEGY_HASH" => "HASH",
            _ => "RANGE",
        };
        let mut elements = Vec::new();
        for p in tree::list(spec, "partParams") {
            let Some(p) = tree::as_kind(p, "PartitionElem") else {
                continue;
            };
            if let Some(name) = tree::opt_str(p, "name") {
                let column = self.catalog.column(rel, name).ok_or_else(|| {
                    Fail::new(
                        "42703",
                        format!("column \"{name}\" named in partition key does not exist"),
                    )
                    .at(tree::location(p))
                })?;
                elements.push(PartitionElem::Column(column.id));
            } else if let Some(e) = tree::field(p, "expr") {
                let scope = TableScope {
                    catalog: &self.catalog,
                    rel,
                    columns: true,
                    generated: &[],
                };
                let expr = Analyzer {
                    sql: self.sql,
                    scope: &scope,
                }
                .expr(e)?;
                elements.push(match expr {
                    Expr::Column { column, .. } => PartitionElem::Column(column.col),
                    other => PartitionElem::Expr(other),
                });
            }
        }
        Ok(PartitionKey { strategy, elements })
    }

    /// `FOR VALUES …` of a partition, its values typed by the parent's key.
    fn bound_text(&self, parent: u32, bound: &Value) -> Result<String> {
        if tree::bool(bound, "is_default") {
            return Ok("DEFAULT".to_owned());
        }
        let parent_rel = self.catalog.get(parent)?;
        let key_types: Vec<Ty> = parent_rel
            .partition_key
            .as_ref()
            .map(|k| {
                k.elements
                    .iter()
                    .map(|e| match e {
                        PartitionElem::Column(id) => parent_rel
                            .columns
                            .iter()
                            .find(|c| c.id == *id)
                            .map_or_else(Ty::unknown, |c| c.ty.clone()),
                        PartitionElem::Expr(e) => e.ty(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let value = |node: &Value, i: usize| -> String {
            if let Some(c) = tree::as_kind(node, "ColumnRef") {
                let word = tree::strings(tree::list(c, "fields"))
                    .pop()
                    .unwrap_or_default();
                return word.to_uppercase();
            }
            let ty = key_types.get(i).cloned().unwrap_or_else(Ty::unknown);
            let analyzed = match tree::kind(node) {
                Some(("A_Const", c)) => expr::constant(&tree::literal(c)),
                Some(("TypeCast", c)) => {
                    let inner = tree::field(c, "arg").and_then(|a| tree::as_kind(a, "A_Const"));
                    inner.map_or_else(
                        || Expr::Raw {
                            text: expr::source_of(self.sql, node),
                            ty: Ty::unknown(),
                        },
                        |a| expr::constant(&tree::literal(a)),
                    )
                }
                _ => Expr::Raw {
                    text: expr::source_of(self.sql, node),
                    ty: Ty::unknown(),
                },
            };
            let typed = expr::resolve_unknown(analyzed, &self.catalog.base_of(&ty));
            crate::catalog::Catalog::bound_literal(&typed)
        };
        let list = |name: &str| -> Vec<String> {
            tree::list(bound, name)
                .iter()
                .enumerate()
                .map(|(i, n)| value(n, i))
                .collect()
        };
        Ok(match tree::str(bound, "strategy") {
            "l" => {
                let items: Vec<String> = tree::list(bound, "listdatums")
                    .iter()
                    .map(|n| value(n, 0))
                    .collect();
                format!("FOR VALUES IN ({})", items.join(", "))
            }
            "h" => format!(
                "FOR VALUES WITH (modulus {}, remainder {})",
                tree::int(bound, "modulus"),
                tree::int(bound, "remainder")
            ),
            _ => format!(
                "FOR VALUES FROM ({}) TO ({})",
                list("lowerdatums").join(", "),
                list("upperdatums").join(", ")
            ),
        })
    }

    /// Names, then adds, a statement's constraints in Postgres's order:
    /// explicit names first, then CHECKs, then the keys backed by an index,
    /// then foreign keys (which may reference the keys just made).
    #[allow(clippy::too_many_lines)] // one kind of constraint after the other
    fn add_constraints(&mut self, rel: u32, pending: &[Pending<'_>], alter: bool) -> Result<()> {
        let (schema, table) = {
            let r = self.catalog.get(rel)?;
            (r.schema.clone(), r.name.clone())
        };
        let mut names: Vec<Option<String>> = vec![None; pending.len()];
        let mut chosen: Vec<String> = Vec::new();
        for (i, p) in pending.iter().enumerate() {
            if let Some(name) = tree::opt_str(p.fields, "conname") {
                let r = self.catalog.get(rel)?;
                if r.constraints.iter().any(|c| c.name == name) || chosen.iter().any(|c| c == name)
                {
                    return Err(Fail::new(
                        "42710",
                        format!("constraint \"{name}\" for relation \"{table}\" already exists"),
                    )
                    .at(tree::location(p.fields)));
                }
                let backed = matches!(
                    tree::str(p.fields, "contype"),
                    "CONSTR_PRIMARY" | "CONSTR_UNIQUE" | "CONSTR_EXCLUSION"
                );
                if backed && self.catalog.relation_name_taken(&schema, name) {
                    return Err(
                        Fail::new("42P07", format!("relation \"{name}\" already exists"))
                            .at(tree::location(p.fields)),
                    );
                }
                chosen.push(name.to_owned());
                if let Some(slot) = names.get_mut(i) {
                    *slot = Some(name.to_owned());
                }
            }
        }
        let phases: [&[&str]; 3] = [
            &["CONSTR_CHECK"],
            &["CONSTR_PRIMARY", "CONSTR_UNIQUE", "CONSTR_EXCLUSION"],
            &["CONSTR_FOREIGN"],
        ];
        for phase in phases {
            for (i, p) in pending.iter().enumerate() {
                let contype = tree::str(p.fields, "contype");
                if !phase.contains(&contype) {
                    continue;
                }
                let name = names.get(i).cloned().flatten().unwrap_or_else(|| {
                    let n = self.default_name(&schema, &table, p, &chosen);
                    chosen.push(n.clone());
                    n
                });
                let constraint = match contype {
                    "CONSTR_CHECK" => self.check_constraint(rel, name, p, alter)?,
                    "CONSTR_PRIMARY" | "CONSTR_UNIQUE" => self.key_constraint(rel, name, p)?,
                    "CONSTR_EXCLUSION" => self.exclusion(rel, name, p)?,
                    _ => self.foreign_key(rel, name, p, alter)?,
                };
                self.catalog.get_mut(rel)?.constraints.push(constraint);
            }
        }
        Ok(())
    }

    fn default_name(
        &self,
        schema: &str,
        table: &str,
        p: &Pending<'_>,
        chosen: &[String],
    ) -> String {
        let column = p.column.as_deref();
        match tree::str(p.fields, "contype") {
            "CONSTR_CHECK" => {
                let single = column.map(str::to_owned).or_else(|| {
                    let mut read = Vec::new();
                    tree::visit(
                        tree::field(p.fields, "raw_expr").unwrap_or(&Value::Null),
                        &mut |k, f| {
                            if k == "ColumnRef"
                                && let Some(n) = tree::strings(tree::list(f, "fields")).pop()
                                && !read.contains(&n)
                            {
                                read.push(n);
                            }
                        },
                    );
                    (read.len() == 1).then(|| read.remove(0))
                });
                choose_constraint_name(
                    &self.catalog,
                    schema,
                    table,
                    single.as_deref(),
                    "check",
                    chosen,
                )
            }
            "CONSTR_FOREIGN" => {
                let mut columns = tree::strings(tree::list(p.fields, "fk_attrs"));
                if columns.is_empty() {
                    columns.extend(column.map(str::to_owned));
                }
                choose_constraint_name(
                    &self.catalog,
                    schema,
                    table,
                    Some(&name_addition(&columns)),
                    "fkey",
                    chosen,
                )
            }
            kind => {
                let (columns, label) = match kind {
                    "CONSTR_PRIMARY" => (Vec::new(), "pkey"),
                    "CONSTR_EXCLUSION" => {
                        let elems: Vec<Value> = tree::list(p.fields, "exclusions")
                            .iter()
                            .filter_map(|pair| tree::list_items(pair).first().cloned())
                            .collect();
                        (index_column_names(&elems), "excl")
                    }
                    _ => {
                        let mut keys = tree::strings(tree::list(p.fields, "keys"));
                        if keys.is_empty() {
                            keys.extend(column.map(str::to_owned));
                        }
                        keys.extend(tree::strings(tree::list(p.fields, "including")));
                        (keys, "key")
                    }
                };
                let addition = (!columns.is_empty()).then(|| name_addition(&columns));
                choose_relation_name(
                    &self.catalog,
                    schema,
                    table,
                    addition.as_deref(),
                    label,
                    true,
                    chosen,
                )
            }
        }
    }

    fn check_constraint(
        &self,
        rel: u32,
        name: String,
        p: &Pending<'_>,
        alter: bool,
    ) -> Result<Constraint> {
        let raw = tree::field(p.fields, "raw_expr").unwrap_or(&Value::Null);
        let scope = TableScope {
            catalog: &self.catalog,
            rel,
            columns: true,
            generated: &[],
        };
        let expr = Analyzer {
            sql: self.sql,
            scope: &scope,
        }
        .expr(raw)?;
        let ty = self.catalog.base_of(&expr.ty());
        if !ty.is_unknown() && !ty.is("bool") {
            return Err(Fail::new(
                "42804",
                format!(
                    "argument of CHECK must be type boolean, not type {}",
                    ty.display()
                ),
            )
            .at(location_of(raw)));
        }
        Ok(Constraint {
            name,
            kind: ConstraintKind::Check,
            columns: Vec::new(),
            check: Some(expr::resolve_unknown(
                expr,
                &crate::types::Ty::builtin("bool"),
            )),
            foreign: None,
            include: Vec::new(),
            nulls_not_distinct: false,
            deferrable: false,
            initially_deferred: false,
            not_valid: alter && tree::bool(p.fields, "skip_validation"),
            no_inherit: tree::bool(p.fields, "is_no_inherit"),
            exclusion: None,
            comment: None,
        })
    }

    fn column_ids(
        &self,
        rel: u32,
        names: &[String],
        what: &str,
        location: Option<usize>,
    ) -> Result<Vec<u32>> {
        names
            .iter()
            .map(|n| {
                self.catalog.column(rel, n).map(|c| c.id).ok_or_else(|| {
                    Fail::new("42703", format!("column \"{n}\" {what} does not exist")).at(location)
                })
            })
            .collect()
    }

    fn key_constraint(&mut self, rel: u32, name: String, p: &Pending<'_>) -> Result<Constraint> {
        let primary = tree::str(p.fields, "contype") == "CONSTR_PRIMARY";
        let mut keys = tree::strings(tree::list(p.fields, "keys"));
        if keys.is_empty() {
            keys.extend(p.column.clone());
        }
        let location = tree::location(p.fields);
        let columns = self.column_ids(rel, &keys, "named in key", location)?;
        let include = self.column_ids(
            rel,
            &tree::strings(tree::list(p.fields, "including")),
            "named in key",
            location,
        )?;
        let r = self.catalog.get(rel)?;
        if primary
            && r.constraints
                .iter()
                .any(|c| c.kind == ConstraintKind::PrimaryKey)
        {
            return Err(Fail::new(
                "42P16",
                format!(
                    "multiple primary keys for table \"{}\" are not allowed",
                    r.name
                ),
            )
            .at(location));
        }
        if primary {
            let table = self.catalog.get_mut(rel)?;
            for c in &mut table.columns {
                if columns.contains(&c.id) {
                    c.not_null = true;
                }
            }
        }
        Ok(Constraint {
            name,
            kind: if primary {
                ConstraintKind::PrimaryKey
            } else {
                ConstraintKind::Unique
            },
            columns,
            check: None,
            foreign: None,
            include,
            nulls_not_distinct: tree::bool(p.fields, "nulls_not_distinct"),
            deferrable: p.deferrable || tree::bool(p.fields, "deferrable"),
            initially_deferred: p.deferred || tree::bool(p.fields, "initdeferred"),
            not_valid: false,
            no_inherit: false,
            exclusion: None,
            comment: None,
        })
    }

    /// `EXCLUDE USING gist (room WITH =, during WITH &&)`, as written.
    fn exclusion(&self, rel: u32, name: String, p: &Pending<'_>) -> Result<Constraint> {
        let method = tree::opt_str(p.fields, "access_method").unwrap_or("btree");
        let mut parts = Vec::new();
        let mut columns = Vec::new();
        for pair in tree::list(p.fields, "exclusions") {
            let items = tree::list_items(pair);
            let (Some(elem), Some(ops)) = (items.first(), items.get(1)) else {
                continue;
            };
            let Some(e) = tree::as_kind(elem, "IndexElem") else {
                continue;
            };
            let target = match tree::opt_str(e, "name") {
                Some(n) => {
                    let c = self.catalog.column(rel, n).ok_or_else(|| {
                        Fail::new(
                            "42703",
                            format!("column \"{n}\" named in key does not exist"),
                        )
                    })?;
                    columns.push(c.id);
                    remus_core::emit::ident::quote_ident(n).into_owned()
                }
                None => format!(
                    "({})",
                    expr::squash(&expr::source_of(
                        self.sql,
                        tree::field(e, "expr").unwrap_or(&Value::Null)
                    ))
                ),
            };
            let op = tree::strings(tree::list_items(ops))
                .pop()
                .unwrap_or_default();
            parts.push(format!("{target} WITH {op}"));
        }
        let mut definition = format!("EXCLUDE USING {method} ({})", parts.join(", "));
        if let Some(w) = tree::field(p.fields, "where_clause") {
            let _ = write!(
                definition,
                " WHERE ({})",
                expr::squash(&expr::source_of(self.sql, w))
            );
        }
        Ok(Constraint {
            name,
            kind: ConstraintKind::Exclusion,
            columns,
            check: None,
            foreign: None,
            include: Vec::new(),
            nulls_not_distinct: false,
            deferrable: p.deferrable || tree::bool(p.fields, "deferrable"),
            initially_deferred: p.deferred || tree::bool(p.fields, "initdeferred"),
            not_valid: false,
            no_inherit: false,
            exclusion: Some(definition),
            comment: None,
        })
    }

    fn foreign_key(
        &self,
        rel: u32,
        name: String,
        p: &Pending<'_>,
        alter: bool,
    ) -> Result<Constraint> {
        let location = tree::location(p.fields);
        let mut fk_names = tree::strings(tree::list(p.fields, "fk_attrs"));
        if fk_names.is_empty() {
            fk_names.extend(p.column.clone());
        }
        let columns = self.column_ids(
            rel,
            &fk_names,
            "referenced in foreign key constraint",
            location,
        )?;
        let pktable = tree::field(p.fields, "pktable").unwrap_or(&Value::Null);
        let target = self.rel_of(pktable)?;
        let target_rel = self.catalog.get(target)?;
        if !target_rel.kind.is_table() {
            return Err(Fail::new(
                "42809",
                format!("referenced relation \"{}\" is not a table", target_rel.name),
            )
            .at(tree::location(pktable)));
        }
        let ref_columns = self.referenced_columns(target, p, pktable, location)?;
        if ref_columns.len() != columns.len() {
            return Err(Fail::new(
                "42830",
                "number of referencing and referenced columns for foreign key disagree",
            )
            .at(location));
        }
        let table = self.catalog.get(rel)?;
        for (from, to) in columns.iter().zip(&ref_columns) {
            let a = table
                .columns
                .iter()
                .find(|c| c.id == *from)
                .ok_or_else(Fail::internal)?;
            let b = target_rel
                .columns
                .iter()
                .find(|c| c.id == *to)
                .ok_or_else(Fail::internal)?;
            let (ta, tb) = (self.catalog.base_of(&a.ty), self.catalog.base_of(&b.ty));
            if !compatible_keys(&ta, &tb) {
                return Err(Fail::new("42804", format!("foreign key constraint \"{name}\" cannot be implemented"))
                    .detail(format!(
                        "Key columns \"{}\" of the referencing table and \"{}\" of the referenced table are of incompatible types: {} and {}.",
                        a.name,
                        b.name,
                        a.ty.display(),
                        b.ty.display()
                    ))
                    .at(location));
            }
        }
        let action = |field: &str| match tree::str(p.fields, field) {
            "r" => ReferentialAction::Restrict,
            "c" => ReferentialAction::Cascade,
            "n" => ReferentialAction::SetNull,
            "d" => ReferentialAction::SetDefault,
            _ => ReferentialAction::NoAction,
        };
        Ok(Constraint {
            name,
            kind: ConstraintKind::ForeignKey,
            columns,
            check: None,
            foreign: Some(ForeignKey {
                target,
                columns: ref_columns,
                on_update: action("fk_upd_action"),
                on_delete: action("fk_del_action"),
                match_full: tree::str(p.fields, "fk_matchtype") == "f",
            }),
            include: Vec::new(),
            nulls_not_distinct: false,
            deferrable: p.deferrable || tree::bool(p.fields, "deferrable"),
            initially_deferred: p.deferred || tree::bool(p.fields, "initdeferred"),
            not_valid: alter && tree::bool(p.fields, "skip_validation"),
            no_inherit: false,
            exclusion: None,
            comment: None,
        })
    }

    /// The columns a foreign key references: the primary key's when it
    /// names none, else those it names, which a key or a unique index of
    /// the referenced table must have exactly.
    fn referenced_columns(
        &self,
        target: u32,
        p: &Pending<'_>,
        pktable: &Value,
        location: Option<usize>,
    ) -> Result<Vec<u32>> {
        let target_rel = self.catalog.get(target)?;
        let pk_names = tree::strings(tree::list(p.fields, "pk_attrs"));
        if pk_names.is_empty() {
            return target_rel
                .constraints
                .iter()
                .find(|c| c.kind == ConstraintKind::PrimaryKey)
                .map(|c| c.columns.clone())
                .ok_or_else(|| {
                    Fail::new(
                        "42830",
                        format!(
                            "there is no primary key for referenced table \"{}\"",
                            target_rel.name
                        ),
                    )
                    .at(tree::location(pktable))
                });
        }
        let ids = self.column_ids(
            target,
            &pk_names,
            "referenced in foreign key constraint",
            location,
        )?;
        let unique = target_rel.constraints.iter().any(|c| {
            matches!(c.kind, ConstraintKind::PrimaryKey | ConstraintKind::Unique)
                && c.columns.len() == ids.len()
                && ids.iter().all(|i| c.columns.contains(i))
        }) || target_rel.indexes.iter().any(|i| {
            i.unique
                && i.predicate.is_none()
                && i.elements.len() == ids.len()
                && i.elements
                    .iter()
                    .all(|e| matches!(e.key, PartitionElem::Column(c) if ids.contains(&c)))
        });
        if !unique {
            return Err(Fail::new(
                "42830",
                format!(
                    "there is no unique constraint matching given keys for referenced table \"{}\"",
                    target_rel.name
                ),
            )
            .at(tree::location(pktable)));
        }
        Ok(ids)
    }

    /// `CREATE INDEX`: its name (chosen as Postgres chooses it when not
    /// given), elements and predicate.
    pub fn create_index(&mut self, f: &Value) -> Result<Option<String>> {
        let relation = tree::field(f, "relation").unwrap_or(&Value::Null);
        let rel = self.rel_of(relation)?;
        let r = self.catalog.get(rel)?;
        if !(r.kind.is_table() || r.kind == RelKind::MaterializedView) {
            return Err(Fail::new(
                "42809",
                format!("cannot create index on relation \"{}\"", r.name),
            )
            .detail("This operation is not supported for views.")
            .at(tree::location(relation)));
        }
        let (schema, table) = (r.schema.clone(), r.name.clone());
        let params = tree::list(f, "indexParams");
        let elements = params
            .iter()
            .filter_map(|p| tree::as_kind(p, "IndexElem"))
            .map(|e| self.index_elem(rel, relation, e))
            .collect::<Result<Vec<_>>>()?;
        let include = self.column_ids(
            rel,
            &tree::strings(
                &tree::list(f, "indexIncludingParams")
                    .iter()
                    .filter_map(|p| tree::as_kind(p, "IndexElem"))
                    .map(|e| serde_json::json!({"String": {"sval": tree::str(e, "name")}}))
                    .collect::<Vec<_>>(),
            ),
            "named in INCLUDE",
            tree::location(relation),
        )?;
        let scope = TableScope {
            catalog: &self.catalog,
            rel,
            columns: true,
            generated: &[],
        };
        let predicate = tree::field(f, "whereClause")
            .map(|w| {
                Analyzer {
                    sql: self.sql,
                    scope: &scope,
                }
                .expr(w)
            })
            .transpose()?;
        let name = if let Some(n) = tree::opt_str(f, "idxname") {
            if self.catalog.relation_name_taken(&schema, n) {
                if tree::bool(f, "if_not_exists") {
                    return Ok(None);
                }
                return Err(Fail::new(
                    "42P07",
                    format!("relation \"{n}\" already exists"),
                ));
            }
            n.to_owned()
        } else {
            let mut all = params.to_vec();
            all.extend(tree::list(f, "indexIncludingParams").iter().cloned());
            let columns = index_column_names(&all);
            choose_relation_name(
                &self.catalog,
                &schema,
                &table,
                Some(&name_addition(&columns)),
                "idx",
                false,
                &[],
            )
        };
        let options: Vec<String> = tree::list(f, "options")
            .iter()
            .filter_map(|o| tree::as_kind(o, "DefElem"))
            .map(|d| {
                let value = tree::field(d, "arg")
                    .map(|a| expr::source_of(self.sql, a))
                    .unwrap_or_default();
                format!("{}='{}'", tree::str(d, "defname"), value.trim_matches('\''))
            })
            .collect();
        self.catalog.get_mut(rel)?.indexes.push(Index {
            name: name.clone(),
            unique: tree::bool(f, "unique"),
            method: tree::opt_str(f, "accessMethod")
                .unwrap_or("btree")
                .to_owned(),
            elements,
            include,
            predicate,
            nulls_not_distinct: tree::bool(f, "nulls_not_distinct"),
            options: (!options.is_empty()).then(|| options.join(", ")),
        });
        Ok(Some(name))
    }

    /// An element of `CREATE INDEX`: a column or an expression, with its
    /// collation, operator class and ordering.
    fn index_elem(&self, rel: u32, relation: &Value, e: &Value) -> Result<IndexElem> {
        let key = if let Some(name) = tree::opt_str(e, "name") {
            let c = self.catalog.column(rel, name).ok_or_else(|| {
                Fail::new("42703", format!("column \"{name}\" does not exist"))
                    .at(tree::location(relation))
            })?;
            PartitionElem::Column(c.id)
        } else {
            let raw = tree::field(e, "expr").unwrap_or(&Value::Null);
            let scope = TableScope {
                catalog: &self.catalog,
                rel,
                columns: true,
                generated: &[],
            };
            match (Analyzer {
                sql: self.sql,
                scope: &scope,
            })
            .expr(raw)?
            {
                Expr::Column { column, .. } => PartitionElem::Column(column.col),
                other => PartitionElem::Expr(expr::resolve_unknown(other, &Ty::builtin("text"))),
            }
        };
        let opclass = tree::strings(tree::list(e, "opclass"));
        let collation = tree::strings(tree::list(e, "collation"));
        Ok(IndexElem {
            key,
            collation: (!collation.is_empty()).then(|| {
                collation
                    .iter()
                    .map(|c| remus_core::emit::ident::quote_ident(c).into_owned())
                    .collect::<Vec<_>>()
                    .join(".")
            }),
            opclass: (!opclass.is_empty()).then(|| opclass.join(".")),
            desc: tree::str(e, "ordering") == "SORTBY_DESC",
            nulls_first: match tree::str(e, "nulls_ordering") {
                "SORTBY_NULLS_FIRST" => Some(true),
                "SORTBY_NULLS_LAST" => Some(false),
                _ => None,
            },
        })
    }

    // ---- ALTER TABLE --------------------------------------------------------------

    #[allow(clippy::too_many_lines)] // one arm per subcommand
    pub fn alter_table(&mut self, f: &Value) -> Result<()> {
        let relation = tree::field(f, "relation").unwrap_or(&Value::Null);
        let objtype = tree::str(f, "objtype");
        if objtype == "OBJECT_TYPE" {
            return self.alter_composite(f);
        }
        if matches!(objtype, "OBJECT_INDEX" | "OBJECT_SEQUENCE") {
            let label = self.statement_label();
            self.not_previewed(&label);
            return Ok(());
        }
        let rel = match self.rel_of(relation) {
            Ok(rel) => rel,
            Err(_) if tree::bool(f, "missing_ok") => return Ok(()),
            Err(e) => return Err(e),
        };
        let table = self.catalog.get(rel)?.name.clone();
        for cmd in tree::list(f, "cmds") {
            let Some(cmd) = tree::as_kind(cmd, "AlterTableCmd") else {
                continue;
            };
            let column_name = tree::str(cmd, "name");
            let column =
                |run: &Self| -> Result<u32> {
                    run.catalog
                        .column(rel, column_name)
                        .map(|c| c.id)
                        .ok_or_else(|| {
                            Fail::new(
                        "42703",
                        format!("column \"{column_name}\" of relation \"{table}\" does not exist"),
                    )
                    .at(tree::location(relation))
                        })
                };
            match tree::str(cmd, "subtype") {
                "AT_AddColumn" => {
                    let def = tree::field(cmd, "def")
                        .and_then(|d| tree::as_kind(d, "ColumnDef"))
                        .unwrap_or(&Value::Null);
                    let colname = tree::str(def, "colname");
                    if self.catalog.column(rel, colname).is_some() {
                        if tree::bool(cmd, "missing_ok") {
                            continue;
                        }
                        return Err(Fail::new(
                            "42701",
                            format!("column \"{colname}\" of relation \"{table}\" already exists"),
                        )
                        .at(tree::location(def)));
                    }
                    let mut pending = Vec::new();
                    let mut defaults = Vec::new();
                    let mut generated = Vec::new();
                    self.add_column(rel, def, &mut pending, &mut defaults, &mut generated)?;
                    self.defaults(rel, &defaults, &generated)?;
                    self.add_constraints(rel, &pending, true)?;
                }
                "AT_DropColumn" => {
                    let id = match column(self) {
                        Ok(id) => id,
                        Err(_) if tree::bool(cmd, "missing_ok") => continue,
                        Err(e) => return Err(e),
                    };
                    self.drop_column(rel, id, tree::str(cmd, "behavior") == "DROP_CASCADE")?;
                }
                "AT_AlterColumnType" => {
                    let id = column(self)?;
                    let def = tree::field(cmd, "def")
                        .and_then(|d| tree::as_kind(d, "ColumnDef"))
                        .unwrap_or(&Value::Null);
                    let ty = resolve_type(
                        &self.catalog,
                        tree::field(def, "typeName").unwrap_or(&Value::Null),
                    )?;
                    if let Some(view) = self.views_reading(rel, Some(id)).first() {
                        return Err(Fail::new(
                            "0A000",
                            "cannot alter type of a column used by a view or rule",
                        )
                        .detail(format!(
                            "rule _RETURN on view {view} depends on column \"{column_name}\""
                        )));
                    }
                    self.column_mut(rel, id)?.ty = ty;
                }
                "AT_ColumnDefault" => {
                    let id = column(self)?;
                    if let Some(raw) = tree::field(cmd, "def") {
                        let expr = {
                            let scope = TableScope {
                                catalog: &self.catalog,
                                rel,
                                columns: false,
                                generated: &[],
                            };
                            Analyzer {
                                sql: self.sql,
                                scope: &scope,
                            }
                            .expr(raw)?
                        };
                        let expr =
                            self.coerce_to_column(rel, id, expr, "default expression", raw)?;
                        self.column_mut(rel, id)?.default =
                            (!matches!(expr, Expr::Const { value: None, .. })).then_some(expr);
                    } else {
                        let c = self.column_mut(rel, id)?;
                        if !c.generated {
                            c.default = None;
                        }
                    }
                }
                "AT_SetNotNull" => {
                    let id = column(self)?;
                    self.column_mut(rel, id)?.not_null = true;
                }
                "AT_DropNotNull" => {
                    let id = column(self)?;
                    let r = self.catalog.get(rel)?;
                    if r.constraints
                        .iter()
                        .any(|c| c.kind == ConstraintKind::PrimaryKey && c.columns.contains(&id))
                    {
                        return Err(Fail::new(
                            "42P16",
                            format!("column \"{column_name}\" is in a primary key"),
                        ));
                    }
                    if r.columns.iter().any(|c| c.id == id && c.identity.is_some()) {
                        return Err(Fail::new(
                            "42P16",
                            format!(
                                "column \"{column_name}\" of relation \"{table}\" is an identity column"
                            ),
                        ));
                    }
                    self.column_mut(rel, id)?.not_null = false;
                }
                "AT_AddConstraint" => {
                    let Some(c) =
                        tree::field(cmd, "def").and_then(|d| tree::as_kind(d, "Constraint"))
                    else {
                        continue;
                    };
                    if tree::opt_str(c, "indexname").is_some() {
                        let what =
                            format!("{} … ADD CONSTRAINT … USING INDEX", self.statement_label());
                        self.not_previewed(&what);
                        continue;
                    }
                    self.add_constraints(
                        rel,
                        &[Pending {
                            column: None,
                            fields: c,
                            deferrable: false,
                            deferred: false,
                        }],
                        true,
                    )?;
                }
                "AT_DropConstraint" => {
                    self.drop_constraint(
                        rel,
                        column_name,
                        tree::bool(cmd, "missing_ok"),
                        tree::str(cmd, "behavior") == "DROP_CASCADE",
                    )?;
                }
                "AT_ValidateConstraint" => {
                    let r = self.catalog.get_mut(rel)?;
                    match r.constraints.iter_mut().find(|c| c.name == column_name) {
                        Some(c) => c.not_valid = false,
                        None => {
                            return Err(Fail::new(
                                "42704",
                                format!(
                                    "constraint \"{column_name}\" of relation \"{table}\" does not exist"
                                ),
                            ));
                        }
                    }
                }
                "AT_EnableRowSecurity" => self.catalog.get_mut(rel)?.rls = true,
                "AT_DisableRowSecurity" => self.catalog.get_mut(rel)?.rls = false,
                "AT_ForceRowSecurity" => {
                    self.catalog.get_mut(rel)?.rls_forced = true;
                }
                "AT_NoForceRowSecurity" => {
                    self.catalog.get_mut(rel)?.rls_forced = false;
                }
                "AT_SetLogged" => self.catalog.get_mut(rel)?.unlogged = false,
                "AT_SetUnLogged" => self.catalog.get_mut(rel)?.unlogged = true,
                "AT_AddIdentity" => {
                    let id = column(self)?;
                    let generated_when = tree::field(cmd, "def")
                        .and_then(|d| tree::as_kind(d, "Constraint"))
                        .map_or("a", |c| tree::str(c, "generated_when"));
                    let c = self.column_mut(rel, id)?;
                    c.identity = Some(if generated_when == "a" {
                        Identity::Always
                    } else {
                        Identity::ByDefault
                    });
                    c.not_null = true;
                }
                "AT_DropIdentity" => {
                    let id = column(self)?;
                    self.column_mut(rel, id)?.identity = None;
                }
                "AT_DropExpression" => {
                    let id = column(self)?;
                    let c = self.column_mut(rel, id)?;
                    c.generated = false;
                    c.default = None;
                }
                "AT_AttachPartition" => {
                    let Some(p) =
                        tree::field(cmd, "def").and_then(|d| tree::as_kind(d, "PartitionCmd"))
                    else {
                        continue;
                    };
                    let child = self.rel_of(tree::field(p, "name").unwrap_or(&Value::Null))?;
                    let text =
                        self.bound_text(rel, tree::field(p, "bound").unwrap_or(&Value::Null))?;
                    self.catalog.get_mut(child)?.partition_of = Some((rel, text));
                }
                "AT_DetachPartition" => {
                    let Some(p) =
                        tree::field(cmd, "def").and_then(|d| tree::as_kind(d, "PartitionCmd"))
                    else {
                        continue;
                    };
                    let child = self.rel_of(tree::field(p, "name").unwrap_or(&Value::Null))?;
                    self.catalog.get_mut(child)?.partition_of = None;
                }
                other => {
                    let what = match subcommand_words(other) {
                        Some(words) => format!("{} … {words}", self.statement_label()),
                        None => format!("This {} action", self.statement_label()),
                    };
                    self.not_previewed(&what);
                }
            }
        }
        Ok(())
    }

    /// The views reading `rel` (or one of its columns), by name.
    pub fn views_reading(&self, rel: u32, column: Option<u32>) -> Vec<String> {
        self.catalog
            .rels
            .iter()
            .filter(|r| {
                r.view.as_ref().is_some_and(|q| match column {
                    Some(col) => q.column_deps().iter().any(|d| d.rel == rel && d.col == col),
                    None => q.relations().contains(&rel),
                })
            })
            .map(|r| r.name.clone())
            .collect()
    }

    fn drop_column(&mut self, rel: u32, id: u32, cascade: bool) -> Result<()> {
        let r = self.catalog.get(rel)?;
        let column = r
            .columns
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(Fail::internal)?
            .name
            .clone();
        let table = r.name.clone();
        // Foreign keys of other tables pointing at it, and views reading it.
        let mut dependents = Vec::new();
        let mut foreign = Vec::new();
        for other in &self.catalog.rels {
            for c in &other.constraints {
                if let Some(fk) = &c.foreign
                    && fk.target == rel
                    && fk.columns.contains(&id)
                    && other.id != rel
                {
                    dependents.push(format!(
                        "constraint {} on table {} depends on column {column} of table {table}",
                        c.name, other.name
                    ));
                    foreign.push((other.id, c.name.clone()));
                }
            }
        }
        let views: Vec<String> = self.views_reading(rel, Some(id));
        for v in &views {
            dependents.push(format!(
                "view {v} depends on column {column} of table {table}"
            ));
        }
        if !dependents.is_empty() && !cascade {
            return Err(Fail::new(
                "2BP01",
                format!("cannot drop column {column} of table {table} because other objects depend on it"),
            )
            .detail(dependents.join("\n"))
            .hint("Use DROP ... CASCADE to drop the dependent objects too."));
        }
        for (other, name) in foreign {
            if let Some(o) = self.catalog.rel_mut(other) {
                o.constraints.retain(|c| c.name != name);
            }
        }
        for v in views {
            self.drop_rel_named(&v);
        }
        let r = self.catalog.get_mut(rel)?;
        r.columns.retain(|c| c.id != id);
        // Constraints and indexes over the column go with it.
        r.constraints.retain(|c| {
            let mut read = Vec::new();
            if let Some(e) = &c.check {
                e.columns(&mut read);
            }
            !c.columns.contains(&id)
                && !c.include.contains(&id)
                && !read.iter().any(|x| x.col == id)
        });
        r.indexes.retain(|i| {
            let mut read = Vec::new();
            for e in &i.elements {
                if let PartitionElem::Expr(x) = &e.key {
                    x.columns(&mut read);
                }
            }
            if let Some(p) = &i.predicate {
                p.columns(&mut read);
            }
            !i.elements
                .iter()
                .any(|e| matches!(e.key, PartitionElem::Column(c) if c == id))
                && !i.include.contains(&id)
                && !read.iter().any(|x| x.col == id)
        });
        r.policies.retain(|p| {
            let mut read = Vec::new();
            p.using
                .iter()
                .chain(&p.check)
                .for_each(|e| e.columns(&mut read));
            !read.iter().any(|x| x.col == id)
        });
        Ok(())
    }

    fn drop_rel_named(&mut self, name: &str) {
        self.catalog
            .rels
            .retain(|r| r.name != name || r.view.is_none());
    }

    fn drop_constraint(
        &mut self,
        rel: u32,
        name: &str,
        missing_ok: bool,
        cascade: bool,
    ) -> Result<()> {
        let r = self.catalog.get(rel)?;
        let table = r.name.clone();
        let Some(constraint) = r.constraints.iter().find(|c| c.name == name).cloned() else {
            if missing_ok {
                return Ok(());
            }
            return Err(Fail::new(
                "42704",
                format!("constraint \"{name}\" of relation \"{table}\" does not exist"),
            ));
        };
        if matches!(
            constraint.kind,
            ConstraintKind::PrimaryKey | ConstraintKind::Unique
        ) {
            let mut dependents = Vec::new();
            for other in &self.catalog.rels {
                for c in &other.constraints {
                    if let Some(fk) = &c.foreign
                        && fk.target == rel
                        && fk.columns.len() == constraint.columns.len()
                        && fk.columns.iter().all(|x| constraint.columns.contains(x))
                        && !(other.id == rel && c.name == name)
                    {
                        dependents.push((other.id, c.name.clone(), other.name.clone()));
                    }
                }
            }
            if !dependents.is_empty() {
                if !cascade {
                    return Err(Fail::new(
                        "2BP01",
                        format!("cannot drop constraint {name} on table {table} because other objects depend on it"),
                    )
                    .detail(
                        dependents
                            .iter()
                            .map(|(_, c, t)| format!("constraint {c} on table {t} depends on index {name}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                    .hint("Use DROP ... CASCADE to drop the dependent objects too."));
                }
                for (other, c, _) in dependents {
                    if let Some(o) = self.catalog.rel_mut(other) {
                        o.constraints.retain(|x| x.name != c);
                    }
                }
            }
        }
        self.catalog
            .get_mut(rel)?
            .constraints
            .retain(|c| c.name != name);
        Ok(())
    }

    /// `ALTER TYPE composite ADD/DROP/ALTER ATTRIBUTE`.
    fn alter_composite(&mut self, f: &Value) -> Result<()> {
        let relation = tree::field(f, "relation").unwrap_or(&Value::Null);
        let name = Name::from_range_var(relation);
        let Some(index) = self.catalog.composites.iter().position(|c| {
            c.name == name.name && name.schema.as_ref().is_none_or(|s| *s == c.schema)
        }) else {
            return Err(Fail::new(
                "42704",
                format!("type \"{}\" does not exist", display(&name)),
            )
            .at(tree::location(relation)));
        };
        for cmd in tree::list(f, "cmds") {
            let Some(cmd) = tree::as_kind(cmd, "AlterTableCmd") else {
                continue;
            };
            match tree::str(cmd, "subtype") {
                "AT_AddColumn" => {
                    let def = tree::field(cmd, "def")
                        .and_then(|d| tree::as_kind(d, "ColumnDef"))
                        .unwrap_or(&Value::Null);
                    let ty = resolve_type(
                        &self.catalog,
                        tree::field(def, "typeName").unwrap_or(&Value::Null),
                    )?;
                    self.catalog
                        .composite_mut(index)?
                        .attributes
                        .push((tree::str(def, "colname").to_owned(), ty));
                }
                "AT_DropColumn" => {
                    let attr = tree::str(cmd, "name");
                    self.catalog
                        .composite_mut(index)?
                        .attributes
                        .retain(|(n, _)| n != attr);
                }
                "AT_AlterColumnType" => {
                    let def = tree::field(cmd, "def")
                        .and_then(|d| tree::as_kind(d, "ColumnDef"))
                        .unwrap_or(&Value::Null);
                    let ty = resolve_type(
                        &self.catalog,
                        tree::field(def, "typeName").unwrap_or(&Value::Null),
                    )?;
                    let attr = tree::str(cmd, "name");
                    if let Some(a) = self
                        .catalog
                        .composite_mut(index)?
                        .attributes
                        .iter_mut()
                        .find(|(n, _)| n == attr)
                    {
                        a.1 = ty;
                    }
                }
                _ => {
                    let what = format!("This {} action", self.statement_label());
                    self.not_previewed(&what);
                }
            }
        }
        Ok(())
    }
}

/// An `ALTER TABLE` action this engine skips, as SQL names it.
fn subcommand_words(subtype: &str) -> Option<&'static str> {
    Some(match subtype {
        "AT_AlterConstraint" => "ALTER CONSTRAINT",
        "AT_SetRelOptions" | "AT_ReplaceRelOptions" => "SET (…)",
        "AT_ResetRelOptions" => "RESET (…)",
        "AT_ReplicaIdentity" => "REPLICA IDENTITY",
        "AT_ClusterOn" => "CLUSTER ON",
        "AT_DropCluster" => "SET WITHOUT CLUSTER",
        "AT_SetStatistics" => "ALTER COLUMN … SET STATISTICS",
        "AT_SetStorage" => "ALTER COLUMN … SET STORAGE",
        "AT_SetCompression" => "ALTER COLUMN … SET COMPRESSION",
        "AT_SetOptions" => "ALTER COLUMN … SET (…)",
        "AT_ResetOptions" => "ALTER COLUMN … RESET (…)",
        "AT_AddInherit" => "INHERIT",
        "AT_DropInherit" => "NO INHERIT",
        "AT_AddOf" => "OF",
        "AT_DropOf" => "NOT OF",
        "AT_SetAccessMethod" => "SET ACCESS METHOD",
        "AT_SetIdentity" => "ALTER COLUMN … SET GENERATED / identity options",
        "AT_SetExpression" => "ALTER COLUMN … SET EXPRESSION",
        "AT_DetachPartitionFinalize" => "DETACH PARTITION … FINALIZE",
        _ => return None,
    })
}

/// `nextval('schema.seq'::regclass)`: a serial column's default.
fn nextval(schema: &str, sequence: &str) -> Expr {
    use remus_core::emit::ident::quote_ident;
    Expr::Func {
        name: "nextval".to_owned(),
        schema: None,
        args: vec![Expr::Const {
            value: Some(format!("{}.{}", quote_ident(schema), quote_ident(sequence))),
            ty: Ty::builtin("regclass"),
        }],
        star: false,
        distinct: false,
        ty: Ty::builtin("int8"),
        sql_syntax: None,
    }
}

/// Whether a foreign key can compare the two types (an equality operator
/// exists between them, implicit casts allowed).
fn compatible_keys(a: &Ty, b: &Ty) -> bool {
    use crate::types::Category;
    if a.plain() == b.plain() {
        return true;
    }
    if a.array != b.array {
        return false;
    }
    match (a.category(), b.category()) {
        (Category::Numeric, Category::Numeric)
        | (Category::String, Category::String)
        | (Category::DateTime, Category::DateTime) => true,
        _ => {
            matches!(a.base, crate::types::Base::User { .. })
                && matches!(b.base, crate::types::Base::User { .. })
        }
    }
}

/// The first location under a raw node.
pub fn location_of(node: &Value) -> Option<usize> {
    let mut first = None;
    tree::visit(node, &mut |_, f| {
        if let Some(l) = tree::location(f) {
            first = Some(first.map_or(l, |x: usize| x.min(l)));
        }
    });
    first
}
