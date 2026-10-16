//! Placeholders for the functions a text calls but does not define.
//!
//! Real schemas call their own functions in defaults, checks, generated
//! columns, indexes, policies and views (`DEFAULT app.next_id()`, `USING
//! (owner = auth.uid())`), and remus's SQL output never defines them. Before
//! the first statement that calls one, the compiler creates a stub with the
//! same name: `LANGUAGE sql IMMUTABLE`, returning `NULL` of the inferred
//! type, so Postgres accepts the call wherever the real function would be
//! accepted (immutable is required by indexes and generated columns).
//!
//! A stub goes where Postgres would look for the real function: the schema
//! the call names, else the schema unqualified names resolve to (the first
//! on the `search_path`, `public` by default). So the compiled model prints
//! the call exactly as remus would on the real database (`public.f(id)`),
//! and since remus does not model functions, the stub itself never reaches
//! the model. A schema that only holds functions is created for the compile.
//!
//! Argument types come from the call: casts (`f(x::int)`), literals, and
//! columns of tables the text created (`f(owner_id)`). The return type comes
//! from where the call sits: the column it defaults, the cast around it, the
//! boolean a CHECK or policy needs, the other side of a comparison
//! (`owner_id = auth.uid()` returns `owner_id`'s type), `jsonb` under `->>`.
//! Anything else defaults to `text`. When Postgres still disagrees (`function
//! f(uuid) does not exist`, `operator does not exist: uuid = text`), the
//! compiler reads the types from its error, corrects the stub and retries
//! (see [`crate::Compiler`]).
//!
//! Built-in functions, and functions of extensions the text creates, are
//! never stubbed. Aggregates and window functions cannot be.

use std::collections::{HashMap, HashSet};

use pg_query::{
    NodeEnum,
    protobuf::{
        AExprKind, AlterTableType, ConstrType, Node, SelectStmt, SqlValueFunctionOp, SubLinkType,
        a_const::Val,
    },
};
use remus_core::emit::ident::quote_literal;
use serde::Serialize;

use crate::{
    catalog::Catalog,
    names::{QualifiedName, SearchPath, SqlType, quote_ident, strings},
    parse::Statement,
    source::location,
};

/// A function created for the compile only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Stub {
    pub schema: String,
    pub name: String,
    /// Argument types, as SQL (`pg_catalog.int4`, `public.mood`, `text[]`).
    #[cfg_attr(feature = "utoipa", schema(value_type = Vec<String>))]
    args: Vec<SqlType>,
    /// Names of named arguments (`f(p => 1)`), aligned with `args`.
    pub arg_names: Vec<Option<String>>,
    #[cfg_attr(feature = "utoipa", schema(value_type = String))]
    returns: SqlType,
    /// The first statement calling it; the stub is created just before it.
    pub statement: usize,
    /// Byte offset of that first call in the text.
    pub call: usize,
}

impl Stub {
    /// Argument types, as SQL.
    pub fn args(&self) -> impl Iterator<Item = &str> {
        self.args.iter().map(SqlType::as_str)
    }

    /// The return type, as SQL.
    pub fn returns(&self) -> &str {
        self.returns.as_str()
    }

    pub(crate) fn arity(&self) -> usize {
        self.args.len()
    }

    pub(crate) fn has_args(&self, args: &[SqlType]) -> bool {
        self.args == args
    }

    pub(crate) fn returns_type(&self) -> &SqlType {
        &self.returns
    }

    /// How a person would write the signature: `auth.uid() → uuid`,
    /// `public.slugify(text, integer) → text`.
    pub fn signature(&self) -> String {
        let args: Vec<String> = self.args().map(readable_type).collect();
        format!(
            "{}.{}({}) → {}",
            quote_ident(&self.schema),
            quote_ident(&self.name),
            args.join(", "),
            readable_type(self.returns())
        )
    }

    /// The `CREATE FUNCTION` that stands in for it. Names are quoted, types
    /// are [`SqlType`]s (written from a parse tree), and the body is a
    /// quoted literal, so nothing in it can end a quote early.
    pub(crate) fn ddl(&self) -> String {
        let args: Vec<String> = self
            .args
            .iter()
            .zip(&self.arg_names)
            .map(|(ty, name)| match name {
                Some(name) => format!("{} {}", quote_ident(name), ty.as_str()),
                None => ty.as_str().to_owned(),
            })
            .collect();
        format!(
            "CREATE FUNCTION {}.{}({}) RETURNS {} LANGUAGE sql IMMUTABLE PARALLEL SAFE AS {}",
            quote_ident(&self.schema),
            quote_ident(&self.name),
            args.join(", "),
            self.returns.as_str(),
            quote_literal(&format!("SELECT NULL::{}", self.returns.as_str()))
        )
    }

    fn identity(&self) -> (String, String, Vec<SqlType>) {
        (self.schema.clone(), self.name.clone(), self.args.clone())
    }
}

/// Corrections learnt from Postgres's errors, applied on the next attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Overrides {
    map: HashMap<(String, String, usize), Override>,
    /// Stubs Postgres could not create: let the call fail on its own.
    skipped: HashSet<(String, String, Vec<SqlType>)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Override {
    args: Option<Vec<SqlType>>,
    returns: Option<SqlType>,
}

impl Overrides {
    /// Use these argument types for `schema.name` with this many arguments.
    /// Returns whether anything changed.
    pub fn set_args(&mut self, schema: &str, name: &str, args: Vec<SqlType>) -> bool {
        let entry = self
            .map
            .entry((schema.to_owned(), name.to_owned(), args.len()))
            .or_default();
        let changed = entry.args.as_ref() != Some(&args);
        entry.args = Some(args);
        changed
    }

    /// Use this return type for `schema.name` with this many arguments.
    pub fn set_returns(
        &mut self,
        schema: &str,
        name: &str,
        arity: usize,
        returns: SqlType,
    ) -> bool {
        let entry = self
            .map
            .entry((schema.to_owned(), name.to_owned(), arity))
            .or_default();
        let changed = entry.returns.as_ref() != Some(&returns);
        entry.returns = Some(returns);
        changed
    }

    /// Do not create this stub again. Returns whether it was new.
    pub fn skip(&mut self, stub: &Stub) -> bool {
        self.skipped.insert(stub.identity())
    }

    fn get(&self, schema: &str, name: &str, arity: usize) -> Option<&Override> {
        self.map.get(&(schema.to_owned(), name.to_owned(), arity))
    }
}

/// A call to a stubbed function, for mapping an error back to its stub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Call {
    /// Byte offset of the call (its function name) in the text.
    pub location: usize,
    /// Index into the stubs.
    pub stub: usize,
}

/// A stubbed call that is one side of an operator, for `operator does not
/// exist` errors: the operator's location, and the stub on that side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Operand {
    pub operator: usize,
    pub stub: usize,
    /// The stub is the left operand.
    pub left: bool,
}

/// Every stub a text needs, and where they are called.
#[derive(Debug, Clone, Default)]
pub(crate) struct Inferred {
    pub stubs: Vec<Stub>,
    pub calls: Vec<Call>,
    pub operands: Vec<Operand>,
}

/// Find the functions to stub in a parsed text.
pub(crate) fn infer(
    statements: &[Statement],
    catalog: &Catalog,
    overrides: &Overrides,
    defined: &HashSet<(String, String)>,
) -> Inferred {
    let mut extension_functions = HashSet::new();
    for statement in statements {
        if let NodeEnum::CreateExtensionStmt(s) = &statement.node {
            for name in catalog.with_requirements(&s.extname) {
                if let Some(extension) = catalog.extension(&name) {
                    extension_functions.extend(extension.functions.iter().cloned());
                }
            }
        }
    }
    let mut inference = Inference {
        catalog,
        overrides,
        defined,
        extension_functions,
        tables: HashMap::new(),
        domains: HashMap::new(),
        out: Inferred::default(),
        statement: 0,
        path: SearchPath::default(),
    };
    for statement in statements {
        inference.statement = statement.index;
        inference.path = statement.search_path.clone();
        inference.statement_node(&statement.node);
    }
    inference.out
}

/// What the context of an expression needs it to be.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expect {
    Type(SqlType),
    Bool,
    Unknown,
}

impl Expect {
    fn returns(&self) -> SqlType {
        match self {
            Self::Type(t) => t.clone(),
            Self::Bool => SqlType::builtin("boolean"),
            Self::Unknown => SqlType::builtin("text"),
        }
    }
}

type Columns = Vec<(String, Option<SqlType>)>;

/// Column names and types visible to an expression, per table alias.
#[derive(Debug, Clone, Default)]
struct Scope {
    tables: Vec<(String, Columns)>,
}

impl Scope {
    fn table(name: &str, columns: Columns) -> Self {
        Self {
            tables: vec![(name.to_owned(), columns)],
        }
    }

    fn column(&self, fields: &[String]) -> Option<SqlType> {
        match fields {
            [column] => self
                .tables
                .iter()
                .find_map(|(_, cols)| cols.iter().find(|(n, _)| n == column))
                .and_then(|(_, ty)| ty.clone()),
            [.., table, column] => self
                .tables
                .iter()
                .find(|(t, _)| t == table)
                .and_then(|(_, cols)| cols.iter().find(|(n, _)| n == column))
                .and_then(|(_, ty)| ty.clone()),
            [] => None,
        }
    }
}

struct Inference<'a> {
    catalog: &'a Catalog,
    overrides: &'a Overrides,
    /// Functions the text itself defines (an import's migrations).
    defined: &'a HashSet<(String, String)>,
    extension_functions: HashSet<String>,
    /// Columns of the tables created so far.
    tables: HashMap<(String, String), Columns>,
    /// Base types of the domains created so far.
    domains: HashMap<(String, String), Option<SqlType>>,
    out: Inferred,
    statement: usize,
    path: SearchPath,
}

impl Inference<'_> {
    fn resolve(&self, q: &QualifiedName) -> (String, String) {
        q.resolve(&self.path)
    }

    fn columns_of(&self, q: &QualifiedName) -> Columns {
        self.tables
            .get(&self.resolve(q))
            .cloned()
            .unwrap_or_default()
    }

    #[allow(clippy::too_many_lines)]
    fn statement_node(&mut self, node: &NodeEnum) {
        use NodeEnum as N;
        match node {
            N::CreateSchemaStmt(s) => {
                for element in &s.schema_elts {
                    if let Some(element) = element.node.as_ref() {
                        let saved = self.path.clone();
                        self.path = SearchPath::default();
                        if !s.schemaname.is_empty() {
                            self.path.apply(&schema_path(&s.schemaname));
                        }
                        self.statement_node(element);
                        self.path = saved;
                    }
                }
            }
            N::CreateStmt(s) => {
                let Some(rv) = &s.relation else { return };
                let q = QualifiedName::from_range_var(rv);
                let mut columns: Columns = Vec::new();
                for parent in &s.inh_relations {
                    if let Some(N::RangeVar(parent)) = parent.node.as_ref() {
                        columns.extend(self.columns_of(&QualifiedName::from_range_var(parent)));
                    }
                }
                for element in &s.table_elts {
                    match element.node.as_ref() {
                        Some(N::ColumnDef(c)) => {
                            if let Some(slot) = columns.iter_mut().find(|(n, _)| *n == c.colname) {
                                if c.type_name.is_some() {
                                    slot.1 = c.type_name.as_ref().and_then(SqlType::from_type_name);
                                }
                            } else {
                                columns.push((
                                    c.colname.clone(),
                                    c.type_name.as_ref().and_then(SqlType::from_type_name),
                                ));
                            }
                        }
                        Some(N::TableLikeClause(like)) => {
                            if let Some(source) = &like.relation {
                                columns.extend(
                                    self.columns_of(&QualifiedName::from_range_var(source)),
                                );
                            }
                        }
                        _ => {}
                    }
                }
                let key = self.resolve(&q);
                self.tables.insert(key, columns.clone());
                let scope = Scope::table(&q.name, columns.clone());
                for element in &s.table_elts {
                    match element.node.as_ref() {
                        Some(N::ColumnDef(c)) => {
                            let ty = columns
                                .iter()
                                .find(|(n, _)| *n == c.colname)
                                .and_then(|(_, t)| t.clone());
                            self.column_constraints(&c.constraints, ty.as_ref(), &scope);
                        }
                        Some(N::Constraint(c)) => self.table_constraint(c, &scope),
                        _ => {}
                    }
                }
                for constraint in &s.constraints {
                    if let Some(N::Constraint(c)) = constraint.node.as_ref() {
                        self.table_constraint(c, &scope);
                    }
                }
                if let Some(spec) = &s.partspec {
                    for param in &spec.part_params {
                        if let Some(N::PartitionElem(e)) = param.node.as_ref()
                            && let Some(expr) = &e.expr
                        {
                            self.expr(expr, &Expect::Unknown, &scope);
                        }
                    }
                }
            }
            N::AlterTableStmt(s) => {
                let Some(rv) = &s.relation else { return };
                let q = QualifiedName::from_range_var(rv);
                let key = self.resolve(&q);
                for cmd in &s.cmds {
                    let Some(N::AlterTableCmd(cmd)) = cmd.node.as_ref() else {
                        continue;
                    };
                    let def = cmd.def.as_deref().and_then(|d| d.node.as_ref());
                    let scope =
                        Scope::table(&q.name, self.tables.get(&key).cloned().unwrap_or_default());
                    match (cmd.subtype(), def) {
                        (AlterTableType::AtAddColumn, Some(N::ColumnDef(c))) => {
                            let ty = c.type_name.as_ref().and_then(SqlType::from_type_name);
                            let columns = self.tables.entry(key.clone()).or_default();
                            columns.push((c.colname.clone(), ty.clone()));
                            let scope = Scope::table(&q.name, columns.clone());
                            self.column_constraints(&c.constraints, ty.as_ref(), &scope);
                        }
                        (AlterTableType::AtColumnDefault, Some(_)) => {
                            let ty = scope.column(std::slice::from_ref(&cmd.name));
                            if let Some(expr) = &cmd.def {
                                self.expr(expr, &ty.map_or(Expect::Unknown, Expect::Type), &scope);
                            }
                        }
                        (AlterTableType::AtAddConstraint, Some(N::Constraint(c))) => {
                            self.table_constraint(c, &scope);
                        }
                        (AlterTableType::AtAlterColumnType, Some(N::ColumnDef(c))) => {
                            let ty = c.type_name.as_ref().and_then(SqlType::from_type_name);
                            if let Some(using) = &c.raw_default {
                                let expect = ty.clone().map_or(Expect::Unknown, Expect::Type);
                                self.expr(using, &expect, &scope);
                            }
                            if let Some(columns) = self.tables.get_mut(&key)
                                && let Some(slot) = columns.iter_mut().find(|(n, _)| *n == cmd.name)
                            {
                                slot.1 = ty;
                            }
                        }
                        (AlterTableType::AtDropColumn, _) => {
                            if let Some(columns) = self.tables.get_mut(&key) {
                                columns.retain(|(n, _)| *n != cmd.name);
                            }
                        }
                        _ => {}
                    }
                }
            }
            N::IndexStmt(s) => {
                let Some(rv) = &s.relation else { return };
                let q = QualifiedName::from_range_var(rv);
                let scope = Scope::table(&q.name, self.columns_of(&q));
                for param in s.index_params.iter().chain(&s.index_including_params) {
                    if let Some(N::IndexElem(e)) = param.node.as_ref()
                        && let Some(expr) = &e.expr
                    {
                        self.expr(expr, &Expect::Unknown, &scope);
                    }
                }
                if let Some(predicate) = &s.where_clause {
                    self.expr(predicate, &Expect::Bool, &scope);
                }
            }
            N::CreatePolicyStmt(s) => {
                let Some(rv) = &s.table else { return };
                let q = QualifiedName::from_range_var(rv);
                let scope = Scope::table(&q.name, self.columns_of(&q));
                for expr in [&s.qual, &s.with_check].into_iter().flatten() {
                    self.expr(expr, &Expect::Bool, &scope);
                }
            }
            N::AlterPolicyStmt(s) => {
                let Some(rv) = &s.table else { return };
                let q = QualifiedName::from_range_var(rv);
                let scope = Scope::table(&q.name, self.columns_of(&q));
                for expr in [&s.qual, &s.with_check].into_iter().flatten() {
                    self.expr(expr, &Expect::Bool, &scope);
                }
            }
            N::CreateDomainStmt(s) => {
                let base = s.type_name.as_ref().and_then(SqlType::from_type_name);
                if let Some(q) = QualifiedName::from_list(&s.domainname) {
                    let key = self.resolve(&q);
                    self.domains.insert(key, base.clone());
                }
                let scope = Scope::table("", vec![("value".to_owned(), base.clone())]);
                self.column_constraints(&s.constraints, base.as_ref(), &scope);
            }
            N::AlterDomainStmt(s) => {
                let base = QualifiedName::from_list(&s.type_name)
                    .and_then(|q| self.domains.get(&self.resolve(&q)).cloned())
                    .flatten();
                let scope = Scope::table("", vec![("value".to_owned(), base.clone())]);
                match (s.subtype.as_str(), s.def.as_deref()) {
                    ("C", Some(def)) => {
                        self.column_constraints(std::slice::from_ref(def), base.as_ref(), &scope);
                    }
                    ("T", Some(def)) => {
                        self.expr(def, &base.map_or(Expect::Unknown, Expect::Type), &scope);
                    }
                    _ => {}
                }
            }
            N::ViewStmt(s) => {
                if let Some(query) = &s.query {
                    self.query(query, &Scope::default());
                }
            }
            N::CreateTableAsStmt(s) => {
                if let Some(query) = &s.query {
                    self.query(query, &Scope::default());
                }
            }
            N::CreateStatsStmt(s) => {
                let scope = s
                    .relations
                    .first()
                    .and_then(|r| match r.node.as_ref()? {
                        N::RangeVar(rv) => Some(QualifiedName::from_range_var(rv)),
                        _ => None,
                    })
                    .map(|q| Scope::table(&q.name, self.columns_of(&q)))
                    .unwrap_or_default();
                for expr in &s.exprs {
                    if let Some(N::StatsElem(e)) = expr.node.as_ref()
                        && let Some(expr) = &e.expr
                    {
                        self.expr(expr, &Expect::Unknown, &scope);
                    }
                }
            }
            N::RenameStmt(s) => self.rename(s),
            N::AlterObjectSchemaStmt(s) => {
                if let Some(rv) = &s.relation {
                    let from = self.resolve(&QualifiedName::from_range_var(rv));
                    if let Some(columns) = self.tables.remove(&from) {
                        self.tables.insert((s.newschema.clone(), from.1), columns);
                    }
                }
            }
            _ => {}
        }
    }

    fn rename(&mut self, s: &pg_query::protobuf::RenameStmt) {
        use pg_query::protobuf::ObjectType as O;
        let Some(rv) = &s.relation else { return };
        let key = self.resolve(&QualifiedName::from_range_var(rv));
        match s.rename_type() {
            O::ObjectTable | O::ObjectView | O::ObjectMatview => {
                if let Some(columns) = self.tables.remove(&key) {
                    self.tables.insert((key.0, s.newname.clone()), columns);
                }
            }
            O::ObjectColumn => {
                if let Some(columns) = self.tables.get_mut(&key)
                    && let Some(slot) = columns.iter_mut().find(|(n, _)| *n == s.subname)
                {
                    slot.0.clone_from(&s.newname);
                }
            }
            _ => {}
        }
    }

    /// DEFAULT, GENERATED and CHECK constraints of a column (or a domain).
    fn column_constraints(&mut self, constraints: &[Node], ty: Option<&SqlType>, scope: &Scope) {
        for constraint in constraints {
            let Some(NodeEnum::Constraint(c)) = constraint.node.as_ref() else {
                continue;
            };
            let Some(expr) = &c.raw_expr else { continue };
            match c.contype() {
                ConstrType::ConstrDefault | ConstrType::ConstrGenerated => {
                    let expect = ty.map_or(Expect::Unknown, |t| Expect::Type(t.clone()));
                    self.expr(expr, &expect, scope);
                }
                ConstrType::ConstrCheck => self.expr(expr, &Expect::Bool, scope),
                _ => {}
            }
        }
    }

    fn table_constraint(&mut self, c: &pg_query::protobuf::Constraint, scope: &Scope) {
        match c.contype() {
            ConstrType::ConstrCheck => {
                if let Some(expr) = &c.raw_expr {
                    self.expr(expr, &Expect::Bool, scope);
                }
            }
            ConstrType::ConstrExclusion => {
                for pair in &c.exclusions {
                    if let Some(NodeEnum::List(list)) = pair.node.as_ref()
                        && let Some(NodeEnum::IndexElem(e)) =
                            list.items.first().and_then(|n| n.node.as_ref())
                        && let Some(expr) = &e.expr
                    {
                        self.expr(expr, &Expect::Unknown, scope);
                    }
                }
                if let Some(predicate) = &c.where_clause {
                    self.expr(predicate, &Expect::Bool, scope);
                }
            }
            _ => {}
        }
    }

    /// A SELECT (views, materialized views, sub-queries).
    fn query(&mut self, node: &Node, outer: &Scope) {
        if let Some(NodeEnum::SelectStmt(select)) = node.node.as_ref() {
            self.select(select, outer, &Expect::Unknown);
        }
    }

    /// `target` is what the select list must produce (for sub-queries).
    fn select(&mut self, s: &SelectStmt, outer: &Scope, target: &Expect) {
        if let Some(with) = &s.with_clause {
            for cte in &with.ctes {
                if let Some(NodeEnum::CommonTableExpr(cte)) = cte.node.as_ref()
                    && let Some(query) = &cte.ctequery
                {
                    self.query(query, outer);
                }
            }
        }
        if let Some(left) = &s.larg {
            self.select(left, outer, target);
        }
        if let Some(right) = &s.rarg {
            self.select(right, outer, target);
        }
        let mut scope = outer.clone();
        for item in &s.from_clause {
            self.range_item(item, &mut scope);
        }
        for item in &s.target_list {
            if let Some(NodeEnum::ResTarget(t)) = item.node.as_ref()
                && let Some(value) = &t.val
            {
                self.expr(value, target, &scope);
            }
        }
        for predicate in [&s.where_clause, &s.having_clause].into_iter().flatten() {
            self.expr(predicate, &Expect::Bool, &scope);
        }
        for item in s.group_clause.iter().chain(&s.sort_clause) {
            self.expr(item, &Expect::Unknown, &scope);
        }
        for row in &s.values_lists {
            self.expr(row, &Expect::Unknown, &scope);
        }
    }

    fn range_item(&mut self, item: &Node, scope: &mut Scope) {
        match item.node.as_ref() {
            Some(NodeEnum::RangeVar(rv)) => {
                let q = QualifiedName::from_range_var(rv);
                let alias = rv
                    .alias
                    .as_ref()
                    .map_or_else(|| q.name.clone(), |a| a.aliasname.clone());
                scope.tables.push((alias, self.columns_of(&q)));
            }
            Some(NodeEnum::JoinExpr(join)) => {
                for side in [&join.larg, &join.rarg].into_iter().flatten() {
                    self.range_item(side, scope);
                }
                if let Some(quals) = &join.quals {
                    self.expr(quals, &Expect::Bool, scope);
                }
            }
            Some(NodeEnum::RangeSubselect(sub)) => {
                if let Some(query) = &sub.subquery {
                    self.query(query, scope);
                }
            }
            Some(NodeEnum::RangeFunction(f)) => {
                // Set-returning functions cannot be stubbed; look inside.
                for list in &f.functions {
                    if let Some(NodeEnum::List(items)) = list.node.as_ref()
                        && let Some(NodeEnum::FuncCall(call)) =
                            items.items.first().and_then(|n| n.node.as_ref())
                    {
                        for arg in &call.args {
                            self.expr(arg, &Expect::Unknown, scope);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_lines)]
    fn expr(&mut self, node: &Node, expect: &Expect, scope: &Scope) {
        use NodeEnum as N;
        let Some(inner) = node.node.as_ref() else {
            return;
        };
        match inner {
            N::FuncCall(call) => {
                self.call(call, expect, scope);
                for arg in &call.args {
                    let arg = match arg.node.as_ref() {
                        Some(N::NamedArgExpr(named)) => named.arg.as_deref(),
                        _ => Some(arg),
                    };
                    if let Some(arg) = arg {
                        self.expr(arg, &Expect::Unknown, scope);
                    }
                }
                for item in &call.agg_order {
                    self.expr(item, &Expect::Unknown, scope);
                }
                if let Some(filter) = &call.agg_filter {
                    self.expr(filter, &Expect::Bool, scope);
                }
            }
            N::TypeCast(cast) => {
                let expect = cast
                    .type_name
                    .as_ref()
                    .and_then(SqlType::from_type_name)
                    .map_or(Expect::Unknown, Expect::Type);
                if let Some(arg) = &cast.arg {
                    self.expr(arg, &expect, scope);
                }
            }
            N::AExpr(e) => self.a_expr(e, expect, scope),
            N::BoolExpr(b) => {
                for arg in &b.args {
                    self.expr(arg, &Expect::Bool, scope);
                }
            }
            N::NullTest(t) => {
                if let Some(arg) = &t.arg {
                    self.expr(arg, &Expect::Unknown, scope);
                }
            }
            N::BooleanTest(t) => {
                if let Some(arg) = &t.arg {
                    self.expr(arg, &Expect::Bool, scope);
                }
            }
            N::CaseExpr(c) => {
                let subject = c.arg.as_deref();
                if let Some(arg) = subject {
                    self.expr(arg, &Expect::Unknown, scope);
                }
                let when_expect = subject
                    .and_then(|a| type_of(a, scope))
                    .map_or(Expect::Bool, Expect::Type);
                for when in &c.args {
                    if let Some(N::CaseWhen(w)) = when.node.as_ref() {
                        if let Some(condition) = &w.expr {
                            self.expr(condition, &when_expect, scope);
                        }
                        if let Some(result) = &w.result {
                            self.expr(result, expect, scope);
                        }
                    }
                }
                if let Some(default) = &c.defresult {
                    self.expr(default, expect, scope);
                }
            }
            N::CoalesceExpr(c) => {
                let sibling = c.args.iter().find_map(|a| type_of(a, scope));
                let expect = sibling.map_or_else(|| expect.clone(), Expect::Type);
                for arg in &c.args {
                    self.expr(arg, &expect, scope);
                }
            }
            N::MinMaxExpr(m) => {
                let sibling = m.args.iter().find_map(|a| type_of(a, scope));
                let expect = sibling.map_or_else(|| expect.clone(), Expect::Type);
                for arg in &m.args {
                    self.expr(arg, &expect, scope);
                }
            }
            N::SubLink(link) => {
                if let Some(test) = &link.testexpr {
                    self.expr(test, &Expect::Unknown, scope);
                }
                // `x IN (SELECT f())` compares f's result with x; a scalar
                // sub-query stands where its value is expected.
                let target = match link.sub_link_type() {
                    SubLinkType::AnySublink | SubLinkType::AllSublink => link
                        .testexpr
                        .as_deref()
                        .and_then(|t| type_of(t, scope))
                        .map_or(Expect::Unknown, Expect::Type),
                    SubLinkType::ExprSublink | SubLinkType::ArraySublink => expect.clone(),
                    _ => Expect::Unknown,
                };
                if let Some(NodeEnum::SelectStmt(select)) =
                    link.subselect.as_deref().and_then(|q| q.node.as_ref())
                {
                    self.select(select, scope, &target);
                }
            }
            N::AArrayExpr(a) => {
                let element = match expect {
                    Expect::Type(t) => t.element().map_or(Expect::Unknown, Expect::Type),
                    _ => Expect::Unknown,
                };
                for item in &a.elements {
                    self.expr(item, &element, scope);
                }
            }
            N::RowExpr(r) => {
                for arg in &r.args {
                    self.expr(arg, &Expect::Unknown, scope);
                }
            }
            N::CollateClause(c) => {
                if let Some(arg) = &c.arg {
                    self.expr(arg, expect, scope);
                }
            }
            N::AIndirection(i) => {
                if let Some(arg) = &i.arg {
                    self.expr(arg, &Expect::Unknown, scope);
                }
            }
            N::List(list) => {
                for item in &list.items {
                    self.expr(item, expect, scope);
                }
            }
            N::SortBy(s) => {
                if let Some(inner) = &s.node {
                    self.expr(inner, &Expect::Unknown, scope);
                }
            }
            N::ResTarget(t) => {
                if let Some(value) = &t.val {
                    self.expr(value, expect, scope);
                }
            }
            _ => {}
        }
    }

    fn a_expr(&mut self, e: &pg_query::protobuf::AExpr, expect: &Expect, scope: &Scope) {
        let operator = strings(&e.name).pop().unwrap_or_default();
        let (left, right) = (e.lexpr.as_deref(), e.rexpr.as_deref());
        let left_type = left.and_then(|n| type_of(n, scope));
        let right_type = right.and_then(|n| type_of(n, scope));
        let (left_expect, right_expect) = match e.kind() {
            AExprKind::AexprOp if is_json_operator(&operator) => {
                (Expect::Type(SqlType::builtin("jsonb")), Expect::Unknown)
            }
            AExprKind::AexprOp if is_comparison(&operator) => (
                right_type.clone().map_or(Expect::Unknown, Expect::Type),
                left_type.clone().map_or(Expect::Unknown, Expect::Type),
            ),
            AExprKind::AexprOp if operator == "||" => (
                Expect::Type(
                    left_type
                        .clone()
                        .or(right_type.clone())
                        .unwrap_or_else(|| SqlType::builtin("text")),
                ),
                Expect::Type(
                    right_type
                        .clone()
                        .or(left_type.clone())
                        .unwrap_or_else(|| SqlType::builtin("text")),
                ),
            ),
            AExprKind::AexprOp => {
                let fallback = || match expect {
                    Expect::Type(t) => Expect::Type(t.clone()),
                    _ => Expect::Unknown,
                };
                (
                    right_type.clone().map_or_else(fallback, Expect::Type),
                    left_type.clone().map_or_else(fallback, Expect::Type),
                )
            }
            AExprKind::AexprOpAny | AExprKind::AexprOpAll => (
                Expect::Unknown,
                left_type
                    .clone()
                    .map_or(Expect::Unknown, |t| Expect::Type(t.array())),
            ),
            AExprKind::AexprDistinct
            | AExprKind::AexprNotDistinct
            | AExprKind::AexprNullif
            | AExprKind::AexprIn
            | AExprKind::AexprBetween
            | AExprKind::AexprNotBetween
            | AExprKind::AexprBetweenSym
            | AExprKind::AexprNotBetweenSym => (
                right_type.clone().map_or(Expect::Unknown, Expect::Type),
                left_type.clone().map_or(Expect::Unknown, Expect::Type),
            ),
            AExprKind::AexprLike | AExprKind::AexprIlike | AExprKind::AexprSimilar => (
                Expect::Type(SqlType::builtin("text")),
                Expect::Type(SqlType::builtin("text")),
            ),
            AExprKind::Undefined => (Expect::Unknown, Expect::Unknown),
        };
        let operator_at = location(e.location);
        if let Some(left) = left {
            let before = self.out.calls.len();
            self.expr(left, &left_expect, scope);
            self.note_operand(left, before, operator_at, true);
        }
        if let Some(right) = right {
            let before = self.out.calls.len();
            self.expr(right, &right_expect, scope);
            self.note_operand(right, before, operator_at, false);
        }
    }

    /// When an operand is itself a stubbed call, remember it next to the
    /// operator, so `operator does not exist: uuid = text` can fix it.
    fn note_operand(&mut self, operand: &Node, calls_before: usize, at: Option<usize>, left: bool) {
        let Some(at) = at else { return };
        let Some(NodeEnum::FuncCall(call)) = operand.node.as_ref() else {
            return;
        };
        let Some(location) = location(call.location) else {
            return;
        };
        if let Some(c) = self.out.calls[calls_before..]
            .iter()
            .find(|c| c.location == location)
        {
            self.out.operands.push(Operand {
                operator: at,
                stub: c.stub,
                left,
            });
        }
    }

    /// Record a call to a function to stub; `None` when it needs no stub.
    fn call(
        &mut self,
        call: &pg_query::protobuf::FuncCall,
        expect: &Expect,
        scope: &Scope,
    ) -> Option<usize> {
        let names = strings(&call.funcname);
        let (schema, name) = match names.as_slice() {
            [name] => (None, name.clone()),
            [.., schema, name] => (Some(schema.clone()), name.clone()),
            [] => return None,
        };
        let aggregate = call.agg_star
            || call.agg_distinct
            || call.agg_within_group
            || call.func_variadic
            || call.over.is_some()
            || call.agg_filter.is_some()
            || !call.agg_order.is_empty();
        let builtin = match &schema {
            Some(s) => s == "pg_catalog" || s == "information_schema",
            None => self.catalog.is_builtin_function(&name),
        };
        if aggregate || builtin || self.extension_functions.contains(&name) {
            return None;
        }
        // A function the text defines is called as itself; unqualified,
        // wherever the path finds it.
        let defined = match &schema {
            Some(schema) => self.defined.contains(&(schema.clone(), name.clone())),
            None => self.defined.iter().any(|(_, n)| *n == name),
        };
        if defined {
            return None;
        }
        let schema = schema.unwrap_or_else(|| self.path.creation_schema().to_owned());

        let mut args = Vec::new();
        let mut arg_names = Vec::new();
        for arg in &call.args {
            if let Some(NodeEnum::NamedArgExpr(named_arg)) = arg.node.as_ref() {
                let ty = named_arg.arg.as_deref().and_then(|a| type_of(a, scope));
                args.push(ty.unwrap_or_else(|| SqlType::builtin("text")));
                arg_names.push(Some(named_arg.name.clone()));
            } else {
                args.push(type_of(arg, scope).unwrap_or_else(|| SqlType::builtin("text")));
                arg_names.push(None);
            }
        }
        let mut returns = expect.returns();
        if let Some(fix) = self.overrides.get(&schema, &name, args.len()) {
            if let Some(fixed) = &fix.args {
                args.clone_from(fixed);
            }
            if let Some(fixed) = &fix.returns {
                returns.clone_from(fixed);
            }
        }
        let at = location(call.location).unwrap_or(0);
        let stub = Stub {
            schema,
            name,
            args,
            arg_names,
            returns,
            statement: self.statement,
            call: at,
        };
        let identity = stub.identity();
        if self.overrides.skipped.contains(&identity) {
            return None;
        }
        let index =
            if let Some(index) = self.out.stubs.iter().position(|s| s.identity() == identity) {
                index
            } else {
                self.out.stubs.push(stub);
                self.out.stubs.len() - 1
            };
        self.out.calls.push(Call {
            location: at,
            stub: index,
        });
        Some(index)
    }
}

/// The type of an expression, when the text makes it obvious.
fn type_of(node: &Node, scope: &Scope) -> Option<SqlType> {
    use NodeEnum as N;
    match node.node.as_ref()? {
        N::AConst(c) => match c.val.as_ref()? {
            Val::Ival(_) => Some(SqlType::builtin("integer")),
            Val::Fval(_) => Some(SqlType::builtin("numeric")),
            Val::Boolval(_) => Some(SqlType::builtin("boolean")),
            Val::Sval(_) => Some(SqlType::builtin("text")),
            Val::Bsval(_) => Some(SqlType::builtin("bit varying")),
        },
        N::TypeCast(cast) => cast.type_name.as_ref().and_then(SqlType::from_type_name),
        N::ColumnRef(c) => scope.column(&strings(&c.fields)),
        N::BoolExpr(_) | N::NullTest(_) | N::BooleanTest(_) => Some(SqlType::builtin("boolean")),
        N::SqlvalueFunction(f) => Some(SqlType::builtin(match f.op() {
            SqlValueFunctionOp::SvfopCurrentDate => "date",
            SqlValueFunctionOp::SvfopCurrentTime | SqlValueFunctionOp::SvfopCurrentTimeN => {
                "time with time zone"
            }
            SqlValueFunctionOp::SvfopCurrentTimestamp
            | SqlValueFunctionOp::SvfopCurrentTimestampN => "timestamp with time zone",
            SqlValueFunctionOp::SvfopLocaltime | SqlValueFunctionOp::SvfopLocaltimeN => {
                "time without time zone"
            }
            SqlValueFunctionOp::SvfopLocaltimestamp | SqlValueFunctionOp::SvfopLocaltimestampN => {
                "timestamp without time zone"
            }
            _ => "name",
        })),
        N::AExpr(e) if e.kind() == AExprKind::AexprOp => {
            let operator = strings(&e.name).pop().unwrap_or_default();
            if is_comparison(&operator) {
                return Some(SqlType::builtin("boolean"));
            }
            if operator == "->>" || operator == "#>>" {
                return Some(SqlType::builtin("text"));
            }
            if operator == "||" {
                return Some(SqlType::builtin("text"));
            }
            e.lexpr
                .as_deref()
                .and_then(|l| type_of(l, scope))
                .or_else(|| e.rexpr.as_deref().and_then(|r| type_of(r, scope)))
        }
        N::CollateClause(c) => c.arg.as_deref().and_then(|a| type_of(a, scope)),
        _ => None,
    }
}

/// A `VariableSetStmt` equivalent of `SET search_path TO <schema>`, for the
/// elements of `CREATE SCHEMA s CREATE TABLE …` (created in `s`).
fn schema_path(schema: &str) -> pg_query::protobuf::VariableSetStmt {
    use pg_query::protobuf::{AConst, String as PgString, VariableSetKind, VariableSetStmt};
    VariableSetStmt {
        kind: VariableSetKind::VarSetValue as i32,
        name: "search_path".to_owned(),
        args: vec![Node {
            node: Some(NodeEnum::AConst(AConst {
                isnull: false,
                location: -1,
                val: Some(Val::Sval(PgString {
                    sval: schema.to_owned(),
                })),
            })),
        }],
        is_local: false,
    }
}

fn is_comparison(operator: &str) -> bool {
    matches!(operator, "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=")
}

fn is_json_operator(operator: &str) -> bool {
    matches!(
        operator,
        "->" | "->>" | "#>" | "#>>" | "?" | "?|" | "?&" | "@?" | "@@"
    )
}

/// `pg_catalog.int4` → `integer`, for people.
fn readable_type(sql: &str) -> String {
    let (base, arrays) = match sql.find("[]") {
        Some(at) => (&sql[..at], &sql[at..]),
        None => (sql, ""),
    };
    let base = base.strip_prefix("pg_catalog.").unwrap_or(base);
    let readable = match base {
        "int2" => "smallint",
        "int4" => "integer",
        "int8" => "bigint",
        "float4" => "real",
        "float8" => "double precision",
        "bool" => "boolean",
        "varbit" => "bit varying",
        "varchar" => "character varying",
        "bpchar" => "character",
        "timestamptz" => "timestamp with time zone",
        "timestamp" => "timestamp without time zone",
        "timetz" => "time with time zone",
        "time" => "time without time zone",
        "interval" => "interval",
        "numeric" => "numeric",
        other => other,
    };
    format!("{readable}{arrays}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{Overrides, infer};
    use crate::{
        catalog::{Catalog, Extension},
        names::SqlType,
        parse,
    };

    fn catalog() -> Catalog {
        Catalog::new(
            [
                "now",
                "lower",
                "count",
                "coalesce",
                "current_setting",
                "gen_random_uuid",
            ]
            .map(str::to_owned),
            [(
                "pgcrypto".to_owned(),
                Extension {
                    functions: ["crypt", "gen_salt"]
                        .map(str::to_owned)
                        .into_iter()
                        .collect(),
                    ..Extension::default()
                },
            )],
        )
    }

    fn signatures(sql: &str) -> Vec<String> {
        signatures_with(sql, &Overrides::default())
    }

    fn signatures_with(sql: &str, overrides: &Overrides) -> Vec<String> {
        let parsed = parse(sql);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        infer(&parsed.statements, &catalog(), overrides, &HashSet::new())
            .stubs
            .iter()
            .map(super::Stub::signature)
            .collect()
    }

    #[test]
    fn defaults_and_generated_columns_return_the_column_type() {
        assert_eq!(
            signatures(
                "CREATE TABLE t (
                    id bigint DEFAULT app.next_id(),
                    code text DEFAULT short_code(8),
                    slug text GENERATED ALWAYS AS (slugify(code)) STORED,
                    created timestamptz DEFAULT now(),
                    tags text[] DEFAULT default_tags()
                );"
            ),
            [
                "app.next_id() → bigint",
                "public.short_code(integer) → text",
                "public.slugify(text) → text",
                "public.default_tags() → text[]",
            ]
        );
    }

    #[test]
    fn policies_compare_against_the_column_type() {
        assert_eq!(
            signatures(
                "CREATE TABLE docs (owner_id uuid, org_id bigint, data jsonb);
                 CREATE POLICY own ON docs TO authenticated USING (owner_id = auth.uid());
                 CREATE POLICY org ON docs USING (org_id IN (SELECT app.my_orgs()) OR app.is_admin());
                 CREATE POLICY claims ON docs USING ((auth.jwt() ->> 'role') = 'admin');"
            ),
            [
                "auth.uid() → uuid",
                "app.my_orgs() → bigint",
                "app.is_admin() → boolean",
                "auth.jwt() → jsonb",
            ]
        );
    }

    #[test]
    fn casts_literals_and_columns_type_the_arguments() {
        assert_eq!(
            signatures(
                "CREATE TABLE t (a int, b text, CHECK (valid(a, b, 'x', 1.5, true, now()::date)));
                 CREATE INDEX ON t (canon(b::varchar(10)));
                 CREATE INDEX ON t (normalize(b));
                 CREATE INDEX ON t (lower(b)) WHERE is_live(a);"
            ),
            [
                "public.valid(integer, text, text, numeric, boolean, date) → boolean",
                // NORMALIZE is SQL syntax for a built-in: not stubbed.
                "public.canon(character varying) → text",
                "public.is_live(integer) → boolean",
            ]
        );
    }

    #[test]
    fn builtins_extension_functions_and_aggregates_are_not_stubbed() {
        assert_eq!(
            signatures(
                "CREATE EXTENSION pgcrypto;
                 CREATE TABLE t (a text DEFAULT crypt('x', gen_salt('bf')), b timestamptz DEFAULT pg_catalog.now(), c text DEFAULT lower('X'));
                 CREATE VIEW v AS SELECT count(*), my_agg(a ORDER BY a), rank() OVER () FROM t;"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn calls_are_stubbed_once_per_signature_in_the_schema_they_resolve_to() {
        let parsed = parse(
            "CREATE TABLE a (x int DEFAULT f());
             SET search_path TO app, public;
             CREATE TABLE b (x int DEFAULT f(), y int DEFAULT f(), z text DEFAULT f(1));",
        );
        let inferred = infer(
            &parsed.statements,
            &catalog(),
            &Overrides::default(),
            &HashSet::new(),
        );
        let stubs: Vec<(String, usize)> = inferred
            .stubs
            .iter()
            .map(|s| (s.signature(), s.statement))
            .collect();
        assert_eq!(
            stubs,
            [
                ("public.f() → integer".to_owned(), 0),
                ("app.f() → integer".to_owned(), 2),
                ("app.f(integer) → text".to_owned(), 2),
            ]
        );
        assert_eq!(inferred.calls.len(), 4);
    }

    #[test]
    fn views_resolve_columns_through_their_from_clause() {
        assert_eq!(
            signatures(
                "CREATE TABLE users (id bigint, email text);
                 CREATE VIEW v AS SELECT u.id, mask(u.email) AS email FROM users u WHERE visible(u.id);"
            ),
            ["public.mask(text) → text", "public.visible(bigint) → boolean"]
        );
    }

    #[test]
    fn domains_type_value_and_their_default() {
        assert_eq!(
            signatures("CREATE DOMAIN app.slug AS text DEFAULT gen_slug() CHECK (is_slug(VALUE));"),
            ["public.gen_slug() → text", "public.is_slug(text) → boolean"]
        );
    }

    #[test]
    fn overrides_correct_the_inferred_types() {
        let mut overrides = Overrides::default();
        let uuid = || SqlType::parse("uuid").unwrap();
        assert!(overrides.set_args("public", "f", vec![uuid()]));
        assert!(!overrides.set_args("public", "f", vec![uuid()]));
        assert!(overrides.set_returns("public", "f", 1, SqlType::parse("bigint").unwrap()));
        assert_eq!(
            signatures_with("CREATE TABLE t (a int DEFAULT f('x'));", &overrides),
            ["public.f(uuid) → bigint"]
        );
    }

    #[test]
    fn stub_ddl_is_an_immutable_null_returning_sql_function() {
        let parsed = parse("CREATE TABLE t (a int DEFAULT \"My\".f(p => 1::bigint));");
        let stubs = infer(
            &parsed.statements,
            &catalog(),
            &Overrides::default(),
            &HashSet::new(),
        )
        .stubs;
        assert_eq!(
            stubs[0].ddl(),
            "CREATE FUNCTION \"My\".f(p pg_catalog.int8) RETURNS pg_catalog.int4 LANGUAGE sql IMMUTABLE PARALLEL SAFE AS 'SELECT NULL::pg_catalog.int4'"
        );
        assert_eq!(stubs[0].signature(), "\"My\".f(bigint) → integer");
    }
}
