//! Running the statements: each one applied to the [`Catalog`] as Postgres
//! would apply it, refusing what Postgres refuses with its message, its
//! SQLSTATE and where it points. The first error stops the run, as in the
//! app's compile (one transaction).

use serde_json::Value;

use crate::{
    Compiled,
    catalog::Catalog,
    diagnostic::{Code, Diagnostic, LineIndex},
    expr::{ColumnRef, ExprError, Scope},
    tree::{self, Name},
    types::{self, Base, Ty, UserKind, builtin_modifier, builtin_named},
};

/// The largest text compiled: the app's limit for a draft's SQL.
pub const MAX_SQL: usize = 1024 * 1024;

/// An error Postgres raises, before it is located.
#[derive(Debug, Clone)]
pub struct Fail {
    pub sqlstate: &'static str,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    /// A byte offset in the text: the token it points at.
    pub location: Option<usize>,
    pub code: Code,
}

impl Fail {
    pub fn new(sqlstate: &'static str, message: impl Into<String>) -> Self {
        Self {
            sqlstate,
            message: message.into(),
            detail: None,
            hint: None,
            location: None,
            code: Code::Sql,
        }
    }

    /// This engine could not check the statement: what it relies on did not
    /// hold. Never Postgres's error, so it says so.
    pub fn internal() -> Self {
        Self {
            code: Code::Unavailable,
            hint: Some("The app compiles it in a real Postgres.".to_owned()),
            ..Self::new(
                crate::expr::INTERNAL,
                "The browser preview could not check this statement.",
            )
        }
    }

    #[must_use]
    pub fn at(mut self, location: Option<usize>) -> Self {
        if self.location.is_none() {
            self.location = location;
        }
        self
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl From<ExprError> for Fail {
    fn from(e: ExprError) -> Self {
        if e.sqlstate == crate::expr::INTERNAL {
            return Self::internal();
        }
        Self {
            sqlstate: e.sqlstate,
            message: e.message,
            detail: None,
            hint: e.hint,
            location: e.location,
            code: Code::Sql,
        }
    }
}

pub type Result<T> = std::result::Result<T, Fail>;

/// The run: the text, the catalog it builds, and the notes it makes.
pub struct Run<'a> {
    pub sql: &'a str,
    pub catalog: Catalog,
    pub notes: Vec<(usize, Diagnostic)>,
    /// The statement being run, its byte range.
    pub statement: (usize, usize),
    pub index: usize,
}

/// Compiles `sql` from its parse tree (`tree`, `libpg_query`'s JSON).
pub fn compile(sql: &str, tree_json: &str) -> Compiled {
    let lines = LineIndex::new(sql);
    if sql.len() > MAX_SQL {
        return Compiled::failed(vec![Diagnostic::error(
            Code::TooLarge,
            format!("The SQL is over {} MiB", MAX_SQL / 1024 / 1024),
        )]);
    }
    let parsed: Value = match serde_json::from_str(tree_json) {
        Ok(v) => v,
        Err(e) => {
            return Compiled::failed(vec![Diagnostic::error(
                Code::SyntaxError,
                format!("unreadable parse tree: {e}"),
            )]);
        }
    };
    let statements: Vec<(usize, usize, &Value)> = tree::list(&parsed, "stmts")
        .iter()
        .filter_map(|raw| {
            let start = usize::try_from(tree::int(raw, "stmt_location")).unwrap_or(0);
            let len = usize::try_from(tree::int(raw, "stmt_len")).unwrap_or(0);
            let end = if len == 0 {
                sql.len()
            } else {
                start.saturating_add(len).min(sql.len())
            };
            let (start, end) = trim_statement(sql, start.min(end), end);
            Some((start, end, tree::field(raw, "stmt")?))
        })
        .collect();

    // The allowlist, for every statement before any runs (as the app does).
    let mut refused = comment_lines(sql, &lines);
    for (i, (start, end, node)) in statements.iter().enumerate() {
        if let Some((message, hint)) = crate::allow::not_allowed(sql, *start, *end, node) {
            let mut d = Diagnostic::error(Code::NotAllowed, message).with_hint(hint);
            if message_is_extension(&d) {
                d.code = Code::ExtensionNotAvailable;
            }
            d.span = Some(lines.span(*start, *end));
            d.statement = Some(i);
            refused.push(d);
        }
    }
    if !refused.is_empty() {
        return Compiled::failed(refused);
    }

    let mut catalog = Catalog::new();
    catalog.schemas.extend(missing_schemas(&statements));
    let mut run = Run {
        sql,
        catalog,
        notes: Vec::new(),
        statement: (0, 0),
        index: 0,
    };
    for (i, (start, end, node)) in statements.iter().enumerate() {
        run.statement = (*start, *end);
        run.index = i;
        if let Err(fail) = run.statement(node) {
            let span = match fail.location {
                Some(at) if at >= *start && at < *end => lines.span(at, lines.token_end(at)),
                _ => lines.span(*start, *end),
            };
            let mut d = Diagnostic::error(fail.code, fail.message);
            d.sqlstate = (fail.code == Code::Sql).then(|| fail.sqlstate.to_owned());
            d.detail = fail.detail;
            d.hint = fail.hint;
            d.span = Some(span);
            d.statement = Some(i);
            return Compiled::failed(vec![d]);
        }
    }
    let notes = run
        .notes
        .into_iter()
        .map(|(i, mut d)| {
            if d.span.is_none() {
                let (start, end) = statements.get(i).map_or((0, 0), |s| (s.0, s.1));
                d.span = Some(lines.span(start, end));
            }
            d.statement = Some(i);
            d
        })
        .collect();
    Compiled {
        model: Some(run.catalog.model()),
        diagnostics: notes,
    }
}

/// Schemas the text names (in relation and type references) without
/// creating them: the app's compile creates them first, so does this.
fn missing_schemas(statements: &[(usize, usize, &Value)]) -> Vec<String> {
    let mut needed: Vec<String> = Vec::new();
    let mut created: Vec<String> = Vec::new();
    for (_, _, node) in statements {
        if let Some(f) = tree::as_kind(node, "CreateSchemaStmt") {
            created.push(tree::str(f, "schemaname").to_owned());
        }
        if let Some(f) = tree::as_kind(node, "CreateExtensionStmt")
            && tree::str(f, "extname") == "postgis_topology"
        {
            created.push("topology".to_owned());
        }
        visit_qualifiers(node, &mut |schema| {
            if !schema.is_empty() && !needed.iter().any(|n| n == schema) {
                needed.push(schema.to_owned());
            }
        });
    }
    needed
        .into_iter()
        .filter(|s| {
            !created.contains(s)
                && !matches!(s.as_str(), "public" | "information_schema")
                && !s.starts_with("pg_")
        })
        .collect()
}

/// The schema names written in relation references (objects with a
/// `relname`) and type names (objects with `names` and a `typemod`).
fn visit_qualifiers(value: &Value, f: &mut impl FnMut(&str)) {
    match value {
        Value::Object(object) => {
            if object.contains_key("relname")
                && let Some(schema) = object.get("schemaname").and_then(Value::as_str)
            {
                f(schema);
            }
            if object.contains_key("typemod")
                && let Some(names) = object.get("names").and_then(Value::as_array)
            {
                let names = tree::strings(names);
                if let [.., schema, _] = names.as_slice() {
                    f(schema);
                }
            }
            for inner in object.values() {
                visit_qualifiers(inner, f);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| visit_qualifiers(i, f)),
        _ => {}
    }
}

fn message_is_extension(d: &Diagnostic) -> bool {
    d.message.starts_with("extension \"") && d.message.ends_with("is not available")
}

/// A statement's range without the whitespace and comments before it
/// (offsets the parser gave, kept inside the text and on characters).
fn trim_statement(sql: &str, start: usize, end: usize) -> (usize, usize) {
    let floor = |mut at: usize| {
        at = at.min(sql.len());
        while !sql.is_char_boundary(at) {
            at = at.saturating_sub(1);
        }
        at
    };
    let end = floor(end);
    let mut start = floor(start).min(end);
    loop {
        let rest = sql.get(start..end).unwrap_or("");
        let trimmed = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
        start = end - trimmed.len();
        if trimmed.starts_with("--") {
            start = trimmed.find('\n').map_or(end, |n| start + n + 1);
        } else if trimmed.starts_with("/*") {
            start = trimmed.find("*/").map_or(end, |n| start + n + 2);
        } else {
            break;
        }
    }
    let kept = sql
        .get(start..end)
        .unwrap_or("")
        .trim_end_matches(|c: char| c.is_ascii_whitespace());
    (start, start + kept.len())
}

/// `--- words` lines: comments of the discussion typed and never posted (the
/// editor turns them into threads; a text keeping one does not compile).
/// The server's rule and words (romulus-api's `collab::comments`).
fn comment_lines(sql: &str, lines: &LineIndex<'_>) -> Vec<Diagnostic> {
    let is_comment_line = |line: &str| {
        line.trim_start()
            .strip_prefix("---")
            .is_some_and(|rest| !rest.starts_with('-') && !rest.trim().is_empty())
    };
    let mut out = Vec::new();
    let mut start = 0;
    for line in sql.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']);
        if is_comment_line(text) {
            let mut d = Diagnostic::error(
                Code::CommentLine,
                "This --- line is a comment that was never posted.",
            )
            .with_hint(
                "Put the caret after its words and press Enter to post it to the discussion, or write it with two dashes (-- …) to keep it in the SQL.",
            );
            d.span = Some(lines.span(start, start + text.len()));
            out.push(d);
        }
        start += line.len();
    }
    out
}

impl Run<'_> {
    pub fn note(&mut self, d: Diagnostic) {
        self.notes.push((self.index, d));
    }

    /// What this engine does not apply, said rather than dropped: `what` is
    /// the statement or clause (`ALTER SEQUENCE`, `ALTER TABLE … INHERIT`).
    pub fn not_previewed(&mut self, what: &str) {
        self.note(
            Diagnostic::info(
                Code::NotRun,
                format!("{what} is not supported in the browser preview."),
            )
            .with_hint("Here it is skipped; the app compiles it in a real Postgres."),
        );
    }

    /// The running statement's leading keywords (`ALTER TABLE`, `COMMENT ON
    /// INDEX`), for notes.
    pub fn statement_label(&self) -> String {
        crate::allow::label(
            self.sql
                .get(self.statement.0..self.statement.1)
                .unwrap_or_default(),
        )
    }

    fn statement(&mut self, node: &Value) -> Result<()> {
        let Some((kind, f)) = tree::kind(node) else {
            return Ok(());
        };
        match kind {
            "CreateSchemaStmt" => self.create_schema(f),
            "CreateStmt" => self.create_table(f),
            "AlterTableStmt" => self.alter_table(f),
            "IndexStmt" => self.create_index(f).map(|_| ()),
            "CreateEnumStmt" => self.create_enum(f),
            "AlterEnumStmt" => self.alter_enum(f),
            "CreateDomainStmt" => self.create_domain(f),
            "AlterDomainStmt" => self.alter_domain(f),
            "CompositeTypeStmt" => self.create_composite(f),
            "CreateRangeStmt" => self.create_range(f),
            "ViewStmt" => self.create_view(f),
            "CreateTableAsStmt" => self.create_matview(f),
            "CreatePolicyStmt" => self.create_policy(f),
            "AlterPolicyStmt" => self.alter_policy(f),
            "CommentStmt" => self.comment(f),
            "RenameStmt" => self.rename(f),
            "AlterObjectSchemaStmt" => self.set_schema(f),
            "DropStmt" => self.drop(f),
            "CreateExtensionStmt" => self.create_extension(f),
            "CreateSeqStmt" => self.create_sequence(f),
            "VariableSetStmt" => {
                self.set_search_path(f);
                Ok(())
            }
            // ALTER SEQUENCE, CREATE STATISTICS, CREATE COLLATION: allowed,
            // not in the model, not checked here.
            _ => {
                let label = self.statement_label();
                self.not_previewed(&label);
                Ok(())
            }
        }
    }

    // ---- Names ----------------------------------------------------------------

    /// The schema an object named `name` is created in: its own (which must
    /// exist), or the first of the path that exists.
    pub fn creation_schema(&self, schema: Option<&str>, location: Option<usize>) -> Result<String> {
        match schema {
            Some(s) if self.catalog.schemas.iter().any(|x| x == s) => Ok(s.to_owned()),
            Some(s) => {
                Err(Fail::new("3F000", format!("schema \"{s}\" does not exist")).at(location))
            }
            None => self.catalog.path().next().cloned().ok_or_else(|| {
                Fail::new("3F000", "no schema has been selected to create in").at(location)
            }),
        }
    }

    /// The relation a `RangeVar` names.
    pub fn rel_of(&self, range_var: &Value) -> Result<u32> {
        let name = Name::from_range_var(range_var);
        self.catalog
            .find_rel(name.schema.as_deref(), &name.name)
            .map(|r| r.id)
            .ok_or_else(|| {
                Fail::new(
                    "42P01",
                    format!("relation \"{}\" does not exist", display(&name)),
                )
                .at(tree::location(range_var))
            })
    }

    fn set_search_path(&mut self, f: &Value) {
        if !tree::str(f, "name").eq_ignore_ascii_case("search_path") {
            return;
        }
        if tree::str(f, "kind") != "VAR_SET_VALUE" {
            self.catalog.search_path = vec!["public".to_owned()];
            return;
        }
        let mut path = Vec::new();
        for arg in tree::list(f, "args") {
            if let Some(c) = tree::as_kind(arg, "A_Const")
                && let tree::Literal::String(s) = tree::literal(c)
            {
                for part in s.split(',') {
                    let part = part.trim().trim_matches('"');
                    if !part.is_empty() {
                        path.push(part.to_owned());
                    }
                }
            } else if let Some(c) = tree::as_kind(arg, "ColumnRef") {
                path.extend(tree::strings(tree::list(c, "fields")));
            }
        }
        self.catalog.search_path = path;
    }
}

/// `schema.name`, or `name`, as Postgres quotes names in messages (not).
pub fn display(name: &Name) -> String {
    match &name.schema {
        Some(s) => format!("{s}.{}", name.name),
        None => name.name.clone(),
    }
}

/// The type a `TypeName`'s fields resolve to.
pub fn resolve_type(catalog: &Catalog, fields: &Value) -> std::result::Result<Ty, ExprError> {
    let names = tree::strings(tree::list(fields, "names"));
    let location = tree::location(fields);
    let missing = |shown: String| ExprError {
        sqlstate: "42704",
        message: format!("type \"{shown}\" does not exist"),
        hint: None,
        location,
    };
    if tree::bool(fields, "pct_type") {
        return Err(ExprError {
            sqlstate: "0A000",
            message: "%TYPE references are not supported here".to_owned(),
            hint: Some("Write the column's type instead.".to_owned()),
            location,
        });
    }
    let array = !tree::list(fields, "arrayBounds").is_empty();
    let mut ints = Vec::new();
    let mut words = Vec::new();
    for m in tree::list(fields, "typmods") {
        if let Some(c) = tree::as_kind(m, "A_Const") {
            match tree::literal(c) {
                tree::Literal::Integer(i) => {
                    ints.push(i);
                    words.push(i.to_string());
                }
                tree::Literal::String(s) | tree::Literal::Float(s) => words.push(s),
                _ => {}
            }
        } else if let Some(c) = tree::as_kind(m, "ColumnRef") {
            words.extend(tree::strings(tree::list(c, "fields")));
        }
    }
    let builtin = |name: &str| -> Option<std::result::Result<Ty, ExprError>> {
        let canonical = builtin_named(name)?;
        let b = types::builtin(canonical)?;
        Some(
            builtin_modifier(b.name, &ints)
                .map(|modifier| Ty {
                    base: Base::Builtin(b.name),
                    modifier,
                    array,
                })
                .map_err(|message| ExprError {
                    sqlstate: "22023",
                    message,
                    hint: None,
                    location,
                }),
        )
    };
    let user = |base: Base| -> std::result::Result<Ty, ExprError> {
        let modifier = match &base {
            Base::User {
                kind: UserKind::Extension,
                ..
            } if !words.is_empty() => format!("({})", words.join(",")),
            Base::User { name, .. } if !words.is_empty() => {
                return Err(ExprError {
                    sqlstate: "42601",
                    message: format!("type modifier is not allowed for type \"{name}\""),
                    hint: None,
                    location,
                });
            }
            _ => String::new(),
        };
        Ok(Ty {
            base,
            modifier,
            array,
        })
    };
    match names.as_slice() {
        [schema, name] if schema == "pg_catalog" => {
            builtin(name).unwrap_or_else(|| Err(missing(format!("pg_catalog.{name}"))))
        }
        [name] => {
            if let Some(ty) = builtin(name) {
                return ty;
            }
            match catalog.find_type(None, name) {
                Some(base) => user(base),
                None => Err(missing(name.clone())),
            }
        }
        [.., schema, name] => match catalog.find_type(Some(schema), name) {
            Some(base) => user(base),
            None => Err(missing(format!("{schema}.{name}"))),
        },
        [] => Err(missing(String::new())),
    }
}

/// An extension function the text installed: its schema and return type.
pub fn extension_function(catalog: &Catalog, name: &str) -> Option<(String, Ty)> {
    catalog.extensions.iter().find_map(|e| {
        let info = types::extension(&e.name)?;
        let (_, ret) = info.functions.iter().find(|(f, _)| *f == name)?;
        let ty = match types::builtin(ret) {
            Some(b) => Ty::builtin(b.name),
            None => Ty {
                base: Base::User {
                    schema: e.schema.clone(),
                    name: (*ret).to_owned(),
                    kind: UserKind::Extension,
                },
                modifier: String::new(),
                array: false,
            },
        };
        Some((e.schema.clone(), ty))
    })
}

/// A table's own expressions: its columns, by bare (or table-qualified)
/// names. `columns: false` refuses them (defaults).
pub struct TableScope<'a> {
    pub catalog: &'a Catalog,
    pub rel: u32,
    pub columns: bool,
    /// Columns a generated column may not read (other generated ones).
    pub generated: &'a [u32],
}

impl Scope for TableScope<'_> {
    fn column(
        &self,
        qualifier: Option<&str>,
        name: &str,
        location: Option<usize>,
    ) -> std::result::Result<(ColumnRef, Ty), ExprError> {
        let rel = self.catalog.rel(self.rel).ok_or_else(ExprError::internal)?;
        if !self.columns {
            return Err(ExprError {
                sqlstate: "0A000",
                message: "cannot use column reference in DEFAULT expression".to_owned(),
                hint: None,
                location,
            });
        }
        if let Some(q) = qualifier
            && q != rel.name
        {
            return Err(ExprError {
                sqlstate: "42P01",
                message: format!("missing FROM-clause entry for table \"{q}\""),
                hint: None,
                location,
            });
        }
        let column = rel
            .columns
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| ExprError {
                sqlstate: "42703",
                message: format!("column \"{name}\" does not exist"),
                hint: None,
                location,
            })?;
        if self.generated.contains(&column.id) {
            return Err(ExprError {
                sqlstate: "42P17",
                message: "cannot use generated column in column generation expression".to_owned(),
                hint: None,
                location,
            });
        }
        Ok((
            ColumnRef {
                rel: rel.id,
                col: column.id,
            },
            column.ty.clone(),
        ))
    }

    fn type_name(&self, fields: &Value) -> std::result::Result<Ty, ExprError> {
        resolve_type(self.catalog, fields)
    }

    fn extension_function(&self, name: &str) -> Option<(String, Ty)> {
        extension_function(self.catalog, name)
    }
}

/// A domain's expressions: `VALUE`, no columns.
pub struct DomainScope<'a> {
    pub catalog: &'a Catalog,
    pub value: Option<Ty>,
}

impl Scope for DomainScope<'_> {
    fn column(
        &self,
        _: Option<&str>,
        name: &str,
        location: Option<usize>,
    ) -> std::result::Result<(ColumnRef, Ty), ExprError> {
        Err(ExprError {
            sqlstate: "42703",
            message: format!("column \"{name}\" does not exist"),
            hint: None,
            location,
        })
    }

    fn type_name(&self, fields: &Value) -> std::result::Result<Ty, ExprError> {
        resolve_type(self.catalog, fields)
    }

    fn extension_function(&self, name: &str) -> Option<(String, Ty)> {
        extension_function(self.catalog, name)
    }

    fn domain_value(&self) -> Option<Ty> {
        self.value.clone()
    }
}
