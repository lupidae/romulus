//! Views: their query analysed against the catalog (relations, columns,
//! output types) and printed as `pg_get_viewdef(view, true)` prints it.
//! `SELECT [DISTINCT] … FROM … [JOIN …] WHERE … GROUP BY … HAVING …
//! ORDER BY … LIMIT … OFFSET …`; anything else (set operations, `WITH`,
//! subqueries, window functions) keeps the text as written.

use remus_core::emit::ident::quote_ident;
use serde_json::Value;

use crate::{
    catalog::Catalog,
    expr::{Analyzer, ColumnRef, Expr, ExprError, Names, Printer, Scope, squash},
    tree,
    types::Ty,
};

/// In a view's expressions, `ColumnRef::rel` is an index into its range
/// table (a relation may appear twice, under two aliases).
#[derive(Debug, Clone)]
pub struct Query {
    pub distinct: bool,
    pub targets: Vec<Target>,
    pub from: Vec<FromItem>,
    pub filter: Option<Expr>,
    pub group: Vec<Expr>,
    pub having: Option<Expr>,
    pub order: Vec<(Expr, bool, Option<bool>)>,
    pub limit: Option<Expr>,
    pub offset: Option<Expr>,
    pub rtes: Vec<Rte>,
    /// The query as written, when this engine does not analyse it.
    pub raw: Option<String>,
    /// The relations a raw query reads.
    pub raw_relations: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct Rte {
    pub rel: u32,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub enum FromItem {
    Rel(usize),
    Join {
        kind: &'static str,
        left: Box<FromItem>,
        right: Box<FromItem>,
        on: Option<Box<Expr>>,
        using: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Target {
    pub expr: Expr,
    pub name: String,
    /// The name the expression has by itself (a column's): `AS` is left out
    /// when the target is called that.
    pub natural: Option<String>,
}

impl Query {
    /// The relations the view reads, by id.
    pub fn relations(&self) -> Vec<u32> {
        if self.raw.is_some() {
            return self.raw_relations.clone();
        }
        self.rtes.iter().map(|r| r.rel).collect()
    }

    /// The columns of other relations it reads, by relation and column id.
    pub fn column_deps(&self) -> Vec<ColumnRef> {
        fn joins(item: &FromItem, refs: &mut Vec<ColumnRef>) {
            if let FromItem::Join {
                left, right, on, ..
            } = item
            {
                joins(left, refs);
                joins(right, refs);
                if let Some(on) = on {
                    on.columns(refs);
                }
            }
        }
        let mut refs = Vec::new();
        for t in &self.targets {
            t.expr.columns(&mut refs);
        }
        let mut more = |e: &Expr| e.columns(&mut refs);
        self.filter.iter().for_each(&mut more);
        self.having.iter().for_each(&mut more);
        self.group.iter().for_each(&mut more);
        self.order.iter().for_each(|(e, _, _)| more(e));
        for item in &self.from {
            joins(item, &mut refs);
        }
        refs.into_iter()
            .filter_map(|r| {
                let rte = self.rtes.get(usize::try_from(r.rel).ok()?)?;
                Some(ColumnRef {
                    rel: rte.rel,
                    col: r.col,
                })
            })
            .collect()
    }
}

/// The view's columns: name and type of each output.
pub fn output_columns(query: &Query) -> Vec<(String, Ty)> {
    query
        .targets
        .iter()
        .map(|t| (t.name.clone(), t.expr.ty()))
        .collect()
}

struct ViewScope<'a> {
    catalog: &'a Catalog,
    rtes: &'a [Rte],
}

impl Scope for ViewScope<'_> {
    fn column(
        &self,
        qualifier: Option<&str>,
        name: &str,
        location: Option<usize>,
    ) -> Result<(ColumnRef, Ty), ExprError> {
        let mut found = Vec::new();
        for (i, rte) in self.rtes.iter().enumerate() {
            let Some(rel) = self.catalog.rel(rte.rel) else {
                continue;
            };
            let refname = rte.alias.as_deref().unwrap_or(&rel.name);
            if qualifier.is_some_and(|q| q != refname) {
                continue;
            }
            if let Some(c) = rel.columns.iter().find(|c| c.name == name) {
                found.push((
                    ColumnRef {
                        rel: u32::try_from(i).unwrap_or(u32::MAX),
                        col: c.id,
                    },
                    c.ty.clone(),
                ));
            }
        }
        match found.len() {
            1 => Ok(found.remove(0)),
            0 if qualifier.is_some()
                && !self.rtes.iter().any(|r| {
                    self.catalog.rel(r.rel).is_some_and(|rel| {
                        r.alias.as_deref().unwrap_or(&rel.name) == qualifier.unwrap_or("")
                    })
                }) =>
            {
                Err(ExprError {
                    sqlstate: "42P01",
                    message: format!(
                        "missing FROM-clause entry for table \"{}\"",
                        qualifier.unwrap_or("")
                    ),
                    hint: None,
                    location,
                })
            }
            0 => Err(ExprError {
                sqlstate: "42703",
                message: match qualifier {
                    Some(q) => format!("column {q}.{name} does not exist"),
                    None => format!("column \"{name}\" does not exist"),
                },
                hint: None,
                location,
            }),
            _ => Err(ExprError {
                sqlstate: "42702",
                message: format!("column reference \"{name}\" is ambiguous"),
                hint: None,
                location,
            }),
        }
    }

    fn type_name(&self, fields: &Value) -> Result<Ty, ExprError> {
        crate::ddl::resolve_type(self.catalog, fields)
    }

    fn extension_function(&self, name: &str) -> Option<(String, Ty)> {
        crate::ddl::extension_function(self.catalog, name)
    }
}

/// Why a query is kept as written.
struct Unsupported;

/// Analyses a view's `SelectStmt`. Errors are Postgres's (a relation or a
/// column that does not exist); constructs this engine does not analyse
/// give a raw query.
pub fn analyse(catalog: &Catalog, sql: &str, select: &Value) -> Result<Query, ExprError> {
    match analyse_select(catalog, sql, select) {
        Ok(Ok(query)) => Ok(query),
        Ok(Err(Unsupported)) => Ok(raw_query(catalog, sql, select)),
        Err(e) => Err(e),
    }
}

fn raw_query(catalog: &Catalog, sql: &str, select: &Value) -> Query {
    let mut relations = Vec::new();
    tree::visit(select, &mut |kind, fields| {
        if kind == "RangeVar" {
            let name = tree::Name::from_range_var(fields);
            if let Some(rel) = catalog.find_rel(name.schema.as_deref(), &name.name)
                && !relations.contains(&rel.id)
            {
                relations.push(rel.id);
            }
        }
    });
    let text = statement_text(sql, select);
    Query {
        distinct: false,
        targets: Vec::new(),
        from: Vec::new(),
        filter: None,
        group: Vec::new(),
        having: None,
        order: Vec::new(),
        limit: None,
        offset: None,
        rtes: Vec::new(),
        raw: Some(text),
        raw_relations: relations,
    }
}

/// The text of a query: from its first location to the statement's end.
fn statement_text(sql: &str, select: &Value) -> String {
    let mut start = usize::MAX;
    tree::visit(select, &mut |_, fields| {
        if let Some(l) = tree::location(fields) {
            start = start.min(l);
        }
    });
    let Some(rest) = sql.get(start..) else {
        return String::new();
    };
    // Back to the SELECT keyword before the first located node.
    let before = sql.get(..start).unwrap_or_default();
    let select_at = before.to_ascii_lowercase().rfind("select").unwrap_or(start);
    let end = start + rest.find(';').unwrap_or(rest.len());
    squash(sql.get(select_at..end).unwrap_or(rest))
}

#[allow(clippy::too_many_lines)] // one clause after the other, as transformSelectStmt
fn analyse_select(
    catalog: &Catalog,
    sql: &str,
    select: &Value,
) -> Result<Result<Query, Unsupported>, ExprError> {
    let Some(s) = tree::as_kind(select, "SelectStmt") else {
        return Ok(Err(Unsupported));
    };
    let unsupported = tree::str(s, "op") != "SETOP_NONE" && !tree::str(s, "op").is_empty()
        || tree::field(s, "withClause").is_some()
        || !tree::list(s, "valuesLists").is_empty()
        || tree::list(s, "distinctClause")
            .iter()
            .any(|d| !d.as_object().is_some_and(serde_json::Map::is_empty))
        || !tree::list(s, "windowClause").is_empty()
        || tree::field(s, "intoClause").is_some();
    if unsupported {
        return Ok(Err(Unsupported));
    }
    let mut has_sublink = false;
    tree::visit(select, &mut |kind, fields| {
        has_sublink |= matches!(kind, "SubLink" | "RangeSubselect" | "RangeFunction")
            || (kind == "FuncCall" && tree::field(fields, "over").is_some());
    });
    if has_sublink {
        return Ok(Err(Unsupported));
    }

    let mut rtes = Vec::new();
    let mut pending_joins = Vec::new();
    let mut from = Vec::new();
    for item in tree::list(s, "fromClause") {
        match from_item(catalog, item, &mut rtes, &mut pending_joins)? {
            Some(item) => from.push(item),
            None => return Ok(Err(Unsupported)),
        }
    }
    let scope = ViewScope {
        catalog,
        rtes: &rtes,
    };
    let analyzer = Analyzer { sql, scope: &scope };

    // Join conditions, now that every relation is known.
    for item in &mut from {
        resolve_joins(item, &analyzer, &mut pending_joins.iter().copied())?;
    }

    let mut targets = Vec::new();
    for t in tree::list(s, "targetList") {
        let Some(t) = tree::as_kind(t, "ResTarget") else {
            continue;
        };
        let val = tree::field(t, "val").unwrap_or(&Value::Null);
        let alias = tree::opt_str(t, "name");
        if let Some(c) = tree::as_kind(val, "ColumnRef")
            && tree::list(c, "fields")
                .iter()
                .any(|f| tree::kind(f).is_some_and(|(k, _)| k == "A_Star"))
        {
            // `*` and `t.*`: every column, expanded now as Postgres does.
            let qualifier = tree::strings(tree::list(c, "fields")).pop();
            for (i, rte) in rtes.iter().enumerate() {
                let Some(rel) = catalog.rel(rte.rel) else {
                    continue;
                };
                let refname = rte.alias.as_deref().unwrap_or(&rel.name);
                if qualifier.as_deref().is_some_and(|q| q != refname) {
                    continue;
                }
                for col in &rel.columns {
                    targets.push(Target {
                        expr: Expr::Column {
                            column: ColumnRef {
                                rel: u32::try_from(i).unwrap_or(u32::MAX),
                                col: col.id,
                            },
                            ty: col.ty.clone(),
                        },
                        name: col.name.clone(),
                        natural: Some(col.name.clone()),
                    });
                }
            }
            continue;
        }
        let expr = crate::expr::resolve_unknown(analyzer.expr(val)?, &Ty::builtin("text"));
        let natural = match &expr {
            Expr::Column { column, .. } => Some(
                rtes.get(usize::try_from(column.rel).unwrap_or(usize::MAX))
                    .and_then(|rte| catalog.rel(rte.rel))
                    .and_then(|r| r.columns.iter().find(|c| c.id == column.col))
                    .map_or_else(String::new, |c| c.name.clone()),
            ),
            _ => None,
        };
        let name = alias
            .map(str::to_owned)
            .or_else(|| natural.clone())
            .unwrap_or_else(|| figure_name(val));
        targets.push(Target {
            expr,
            name,
            natural,
        });
    }

    let filter = tree::field(s, "whereClause")
        .map(|w| analyzer.expr(w))
        .transpose()?;
    let by_position = |node: &Value, targets: &[Target]| -> Option<Expr> {
        let c = tree::as_kind(node, "A_Const")?;
        let tree::Literal::Integer(n) = tree::literal(c) else {
            return None;
        };
        let n = usize::try_from(n).ok()?.checked_sub(1)?;
        targets.get(n).map(|t| t.expr.clone())
    };
    let by_alias = |node: &Value, targets: &[Target]| -> Option<Expr> {
        let c = tree::as_kind(node, "ColumnRef")?;
        let [name] = tree::strings(tree::list(c, "fields")).try_into().ok()?;
        targets
            .iter()
            .find(|t| t.name == name && t.natural.as_deref() != Some(&name))
            .map(|t| t.expr.clone())
    };
    let mut group = Vec::new();
    for g in tree::list(s, "groupClause") {
        match by_position(g, &targets) {
            Some(e) => group.push(e),
            None => group.push(analyzer.expr(g)?),
        }
    }
    let having = tree::field(s, "havingClause")
        .map(|h| analyzer.expr(h))
        .transpose()?;
    let mut order = Vec::new();
    for o in tree::list(s, "sortClause") {
        let Some(o) = tree::as_kind(o, "SortBy") else {
            continue;
        };
        let node = tree::field(o, "node").unwrap_or(&Value::Null);
        let expr = match by_position(node, &targets).or_else(|| by_alias(node, &targets)) {
            Some(e) => e,
            None => analyzer.expr(node)?,
        };
        let desc = tree::str(o, "sortby_dir") == "SORTBY_DESC";
        let nulls = match tree::str(o, "sortby_nulls") {
            "SORTBY_NULLS_FIRST" => Some(true),
            "SORTBY_NULLS_LAST" => Some(false),
            _ => None,
        };
        order.push((expr, desc, nulls));
    }
    let limit = tree::field(s, "limitCount")
        .map(|l| analyzer.expr(l))
        .transpose()?;
    let offset = tree::field(s, "limitOffset")
        .map(|l| analyzer.expr(l))
        .transpose()?;
    Ok(Ok(Query {
        distinct: !tree::list(s, "distinctClause").is_empty(),
        targets,
        from,
        filter,
        group,
        having,
        order,
        limit,
        offset,
        rtes,
        raw: None,
        raw_relations: Vec::new(),
    }))
}

/// A `FROM` item: relations become range table entries; joins keep their
/// raw condition until every relation is known.
fn from_item<'a>(
    catalog: &Catalog,
    item: &'a Value,
    rtes: &mut Vec<Rte>,
    joins: &mut Vec<Option<&'a Value>>,
) -> Result<Option<FromItem>, ExprError> {
    let Some((kind, f)) = tree::kind(item) else {
        return Ok(None);
    };
    match kind {
        "RangeVar" => {
            let name = tree::Name::from_range_var(f);
            let Some(rel) = catalog.find_rel(name.schema.as_deref(), &name.name) else {
                return Err(ExprError {
                    sqlstate: "42P01",
                    message: format!("relation \"{}\" does not exist", display_name(&name)),
                    hint: None,
                    location: tree::location(f),
                });
            };
            let alias = tree::field(f, "alias").map(|a| tree::str(a, "aliasname").to_owned());
            rtes.push(Rte { rel: rel.id, alias });
            Ok(Some(FromItem::Rel(rtes.len() - 1)))
        }
        "JoinExpr" => {
            if tree::bool(f, "isNatural") || tree::field(f, "alias").is_some() {
                return Ok(None);
            }
            let kind = match tree::str(f, "jointype") {
                "JOIN_LEFT" => "LEFT JOIN",
                "JOIN_RIGHT" => "RIGHT JOIN",
                "JOIN_FULL" => "FULL JOIN",
                _ if tree::field(f, "quals").is_none()
                    && tree::list(f, "usingClause").is_empty() =>
                {
                    "CROSS JOIN"
                }
                _ => "JOIN",
            };
            let Some(left) = from_item(
                catalog,
                tree::field(f, "larg").unwrap_or(&Value::Null),
                rtes,
                joins,
            )?
            else {
                return Ok(None);
            };
            let Some(right) = from_item(
                catalog,
                tree::field(f, "rarg").unwrap_or(&Value::Null),
                rtes,
                joins,
            )?
            else {
                return Ok(None);
            };
            joins.push(tree::field(f, "quals"));
            Ok(Some(FromItem::Join {
                kind,
                left: Box::new(left),
                right: Box::new(right),
                on: None,
                using: tree::strings(tree::list(f, "usingClause")),
            }))
        }
        _ => Ok(None),
    }
}

fn resolve_joins<'a>(
    item: &mut FromItem,
    analyzer: &Analyzer<'_>,
    quals: &mut impl Iterator<Item = Option<&'a Value>>,
) -> Result<(), ExprError> {
    if let FromItem::Join {
        left, right, on, ..
    } = item
    {
        resolve_joins(left, analyzer, quals)?;
        resolve_joins(right, analyzer, quals)?;
        if let Some(Some(q)) = quals.next() {
            *on = Some(Box::new(analyzer.expr(q)?));
        }
    }
    Ok(())
}

fn display_name(name: &tree::Name) -> String {
    match &name.schema {
        Some(s) => format!("{s}.{}", name.name),
        None => name.name.clone(),
    }
}

/// `FigureColname`: what an unnamed output column is called.
fn figure_name(node: &Value) -> String {
    let Some((kind, f)) = tree::kind(node) else {
        return "?column?".to_owned();
    };
    match kind {
        "FuncCall" => tree::strings(tree::list(f, "funcname"))
            .pop()
            .unwrap_or_else(|| "?column?".to_owned()),
        "ColumnRef" => tree::strings(tree::list(f, "fields"))
            .pop()
            .unwrap_or_else(|| "?column?".to_owned()),
        "TypeCast" => {
            let inner = tree::field(f, "arg").map(figure_name);
            match inner {
                Some(name) if name != "?column?" => name,
                _ => tree::field(f, "typeName")
                    .and_then(|t| tree::strings(tree::list(t, "names")).pop())
                    .unwrap_or_else(|| "?column?".to_owned()),
            }
        }
        "CaseExpr" => "case".to_owned(),
        "CoalesceExpr" => "coalesce".to_owned(),
        "MinMaxExpr" => {
            if tree::str(f, "op") == "IS_GREATEST" {
                "greatest".to_owned()
            } else {
                "least".to_owned()
            }
        }
        "A_ArrayExpr" => "array".to_owned(),
        _ => "?column?".to_owned(),
    }
}

struct ViewNames<'a> {
    catalog: &'a Catalog,
    query: &'a Query,
    qualify: bool,
}

impl Names for ViewNames<'_> {
    fn column(&self, column: ColumnRef) -> String {
        let Some(rte) = usize::try_from(column.rel)
            .ok()
            .and_then(|i| self.query.rtes.get(i))
        else {
            return "?".to_owned();
        };
        let Some(rel) = self.catalog.rel(rte.rel) else {
            return "?".to_owned();
        };
        let name = rel
            .columns
            .iter()
            .find(|c| c.id == column.col)
            .map_or("?", |c| c.name.as_str());
        if self.qualify {
            let refname = rte.alias.as_deref().unwrap_or(&rel.name);
            format!("{}.{}", quote_ident(refname), quote_ident(name))
        } else {
            quote_ident(name).into_owned()
        }
    }
}

/// `pg_get_viewdef(view, true)`.
pub fn print_view(catalog: &Catalog, query: &Query) -> String {
    if let Some(raw) = &query.raw {
        return format!(" {raw};");
    }
    let names = ViewNames {
        catalog,
        query,
        qualify: query.rtes.len() > 1,
    };
    let printer = Printer {
        names: &names,
        pretty: true,
    };
    let mut out = String::from(" SELECT");
    if query.distinct {
        out.push_str(" DISTINCT");
    }
    for (i, t) in query.targets.iter().enumerate() {
        out.push_str(if i == 0 { " " } else { ",\n    " });
        out.push_str(&printer.print(&t.expr));
        if t.natural.as_deref() != Some(t.name.as_str()) {
            out.push_str(" AS ");
            out.push_str(&quote_ident(&t.name));
        }
    }
    if !query.from.is_empty() {
        out.push_str("\n   FROM ");
        for (i, item) in query.from.iter().enumerate() {
            if i > 0 {
                out.push_str(",\n    ");
            }
            from_text(catalog, query, &printer, item, &mut out);
        }
    }
    if let Some(filter) = &query.filter {
        out.push_str("\n  WHERE ");
        out.push_str(&printer.print(filter));
    }
    if !query.group.is_empty() {
        out.push_str("\n  GROUP BY ");
        out.push_str(
            &query
                .group
                .iter()
                .map(|g| printer.print(g))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if let Some(having) = &query.having {
        out.push_str("\n HAVING ");
        out.push_str(&printer.print(having));
    }
    if !query.order.is_empty() {
        out.push_str("\n  ORDER BY ");
        let items: Vec<String> = query
            .order
            .iter()
            .map(|(e, desc, nulls)| {
                let mut s = printer.print(e);
                if *desc {
                    s.push_str(" DESC");
                }
                match (nulls, desc) {
                    (Some(true), false) => s.push_str(" NULLS FIRST"),
                    (Some(false), true) => s.push_str(" NULLS LAST"),
                    _ => {}
                }
                s
            })
            .collect();
        out.push_str(&items.join(", "));
    }
    if let Some(offset) = &query.offset {
        out.push_str("\n OFFSET ");
        out.push_str(&printer.print(offset));
    }
    if let Some(limit) = &query.limit {
        out.push_str("\n LIMIT ");
        out.push_str(&printer.print(limit));
    }
    out.push(';');
    out
}

fn from_text(
    catalog: &Catalog,
    query: &Query,
    printer: &Printer<'_>,
    item: &FromItem,
    out: &mut String,
) {
    match item {
        FromItem::Rel(i) => {
            let Some(rte) = query.rtes.get(*i) else {
                return;
            };
            out.push_str(&catalog.qualified(rte.rel));
            if let Some(alias) = &rte.alias {
                out.push(' ');
                out.push_str(&quote_ident(alias));
            }
        }
        FromItem::Join {
            kind,
            left,
            right,
            on,
            using,
        } => {
            from_text(catalog, query, printer, left, out);
            out.push_str("\n     ");
            out.push_str(kind);
            out.push(' ');
            from_text(catalog, query, printer, right, out);
            if !using.is_empty() {
                out.push_str(" USING (");
                out.push_str(
                    &using
                        .iter()
                        .map(|u| quote_ident(u).into_owned())
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                out.push(')');
            } else if let Some(on) = on {
                out.push_str(" ON ");
                out.push_str(&printer.print(on));
            }
        }
    }
}
