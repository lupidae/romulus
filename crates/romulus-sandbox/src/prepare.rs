//! Turning the user's text into the script the sandbox runs, and mapping
//! the sandbox's error positions back.
//!
//! The script is the text's statements, in order, each preceded by a
//! progress marker and followed by `;`, with these changes:
//!
//! - **progress markers**: `setval` on a session sequence before each
//!   statement. Sequences ignore rollbacks, so after a failure the compiler
//!   reads which statement was running, for errors Postgres reports without
//!   a position (`relation "t" already exists`);
//! - **missing schemas**: schemas the text uses but never creates (a schema
//!   holding only functions, which remus does not render) are created first;
//! - **stubs** for functions the text calls but does not define, created
//!   right before the first statement that calls them (see [`crate::stubs`]);
//! - **policy roles**: roles are cluster-wide and the sandbox role cannot
//!   create them, so `TO app_user, admin` becomes `TO PUBLIC`, and the
//!   written roles are recorded against the policy's OID, which follows any
//!   later rename; the compiler puts them back into the model;
//! - **materialized views** are created `WITH NO DATA`: their query is
//!   checked, never run;
//! - **objects an extension already created** are skipped: remus lists
//!   PostGIS's `spatial_ref_sys` like any table, and SQL rendered from such
//!   a model creates it again right after `CREATE EXTENSION postgis`.
//!
//! Every byte of the script belongs to a segment that says where it came
//! from, so a Postgres error position maps back to the user's text.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    ops::Range,
};

use pg_query::{
    NodeEnum,
    protobuf::{ObjectType, RoleSpecType},
};
use remus_core::emit::ident::quote_literal;

use crate::{
    catalog::{Catalog, ProvidedObject},
    diagnostic::{Code, Diagnostic},
    key::{Action, ObjectKind},
    names::{QualifiedName, quote_ident},
    parse::{ParsedSql, Statement},
    source::location,
    stubs::{self, Call, Operand, Overrides, Stub},
    walk,
};

/// The session sequence the progress markers set (created per connection).
pub(crate) const PROGRESS: &str = "pg_temp.romulus_sandbox_progress";
/// The session table policy roles are recorded in (created per connection).
pub(crate) const POLICY_ROLES: &str = "pg_temp.romulus_sandbox_policy_roles";

/// The script for one compile attempt.
#[derive(Debug, Clone)]
pub(crate) struct Prepared {
    pub script: String,
    segments: Vec<Segment>,
    pub stubs: Vec<Stub>,
    pub calls: Vec<Call>,
    pub operands: Vec<Operand>,
    /// Info diagnostics: statements skipped because an extension provides
    /// their object.
    pub notes: Vec<Diagnostic>,
    /// Schemas created because the text uses them without creating them.
    pub created_schemas: Vec<String>,
    /// Whether any policy roles were recorded (worth reading back).
    pub records_policy_roles: bool,
    /// Where each statement's part of the script starts (its progress
    /// marker), by statement index: an import runs the script a migration
    /// at a time.
    pub statement_starts: Vec<usize>,
}

/// What a migration import changes in the text it replays (see
/// [`crate::import`]); nothing for a compile.
#[derive(Debug, Clone, Default)]
pub(crate) struct Edits {
    /// Ranges of a statement replaced for the replay, by statement index.
    pub replace: HashMap<usize, Vec<(Range<usize>, String)>>,
    /// Statements not run at all (a repeated `CREATE EXTENSION IF NOT
    /// EXISTS`).
    pub omit: HashSet<usize>,
    /// Functions the text defines, `(schema, name)`: never stubbed.
    pub defined: HashSet<(String, String)>,
}

#[derive(Debug, Clone)]
struct Segment {
    script: Range<usize>,
    origin: Origin,
}

#[derive(Debug, Clone)]
enum Origin {
    /// Copied verbatim from the text, starting at this byte offset.
    Source(usize),
    /// Stands in for this range of the text.
    Replaced(Range<usize>),
    /// Added by the compiler, on behalf of a statement (or of none).
    Injected(Option<usize>, Injection),
}

/// What an injected segment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Injection {
    Prelude,
    Schema,
    Marker,
    Stub(usize),
    Terminator,
    PolicyRoles,
}

/// Where a script position comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Located {
    pub statement: Option<usize>,
    /// The range of the text it maps to: a point inside a verbatim
    /// statement, a rewritten range, or the call a stub was made for.
    pub source: Option<Range<usize>>,
    pub injection: Option<Injection>,
}

/// The progress value meaning "statement `index` is running". Sequence
/// values start at 1, and 1 means "before any statement".
pub(crate) fn progress_value(index: usize) -> i64 {
    i64::try_from(index).unwrap_or(i64::MAX - 2) + 2
}

/// The statement a progress value says was running.
pub(crate) fn statement_of_progress(value: i64) -> Option<usize> {
    usize::try_from(value.checked_sub(2)?).ok()
}

/// Build the script for a text that parsed and passed the allowlist.
pub(crate) fn prepare(
    sql: &str,
    parsed: &ParsedSql,
    catalog: &Catalog,
    overrides: &Overrides,
) -> Prepared {
    prepare_with(sql, parsed, catalog, overrides, &Edits::default())
}

/// [`prepare`] with an import's [`Edits`].
pub(crate) fn prepare_with(
    sql: &str,
    parsed: &ParsedSql,
    catalog: &Catalog,
    overrides: &Overrides,
    edits: &Edits,
) -> Prepared {
    let inferred = stubs::infer(&parsed.statements, catalog, overrides, &edits.defined);
    let skipped = provided_by_extensions(&parsed.statements, catalog);
    let created_schemas = missing_schemas(&parsed.statements, catalog, &inferred.stubs);

    let mut script = Script::default();
    script.inject(
        &format!("SELECT pg_catalog.setval('{PROGRESS}', 1, false);\n"),
        None,
        Injection::Prelude,
    );
    for schema in &created_schemas {
        script.inject(
            &format!("CREATE SCHEMA {};\n", quote_ident(schema)),
            None,
            Injection::Schema,
        );
    }

    let mut notes = Vec::new();
    let mut records_policy_roles = false;
    let mut statement_starts = Vec::with_capacity(parsed.statements.len());
    for statement in &parsed.statements {
        let i = statement.index;
        statement_starts.push(script.text.len());
        script.inject(
            &format!(
                "SELECT pg_catalog.setval('{PROGRESS}', {}, true);\n",
                progress_value(i)
            ),
            Some(i),
            Injection::Marker,
        );
        for (k, stub) in inferred.stubs.iter().enumerate() {
            if stub.statement == i {
                script.inject(&format!("{};\n", stub.ddl()), Some(i), Injection::Stub(k));
            }
        }
        if let Some((extension, object)) = skipped
            .iter()
            .find(|(s, _, _)| *s == i)
            .map(|(_, e, o)| (e, o))
        {
            notes.push(
                Diagnostic::info(
                    Code::ProvidedByExtension,
                    format!(
                        "{object} is created by extension {extension}; this statement was skipped"
                    ),
                )
                .with_span(statement.span)
                .with_statement(i),
            );
            continue;
        }
        if edits.omit.contains(&i) {
            continue;
        }

        let mut rewrites: Vec<(Range<usize>, String)> =
            edits.replace.get(&i).cloned().unwrap_or_default();
        let roles = policy_roles(statement, parsed);
        if let Some(range) = roles.as_ref().and_then(|r| r.rewrite.clone()) {
            rewrites.push((range, "PUBLIC".to_owned()));
        }
        if let Some(rewrite) = materialized_view_without_data(statement, parsed, sql) {
            rewrites.push(rewrite);
        }
        rewrites.sort_by_key(|(range, _)| range.start);

        let mut cursor = statement.span.start;
        for (range, text) in rewrites {
            script.source(sql, cursor..range.start);
            script.replace(&text, range.clone());
            cursor = range.end;
        }
        script.source(sql, cursor..statement.span.end);
        script.inject(";\n", Some(i), Injection::Terminator);

        if let Some(written) = roles.and_then(|r| r.record)
            && let Some(capture) = record_policy_roles(statement, &written)
        {
            records_policy_roles = true;
            script.inject(&capture, Some(i), Injection::PolicyRoles);
        }
    }

    Prepared {
        script: script.text,
        segments: script.segments,
        stubs: inferred.stubs,
        calls: inferred.calls,
        operands: inferred.operands,
        notes,
        created_schemas,
        records_policy_roles,
        statement_starts,
    }
}

impl Prepared {
    /// Where a byte offset of the script comes from.
    pub fn locate(&self, offset: usize, parsed: &ParsedSql) -> Located {
        let segment = self
            .segments
            .iter()
            .find(|s| s.script.start <= offset && offset < s.script.end)
            .or_else(|| self.segments.last());
        let Some(segment) = segment else {
            return Located {
                statement: None,
                source: None,
                injection: None,
            };
        };
        let statement_at = |at: usize| {
            parsed
                .statements
                .iter()
                .find(|s| s.span.start <= at && at <= s.span.end)
                .map(|s| s.index)
        };
        match &segment.origin {
            Origin::Source(start) => {
                let at = start + offset.saturating_sub(segment.script.start);
                Located {
                    statement: statement_at(at),
                    source: Some(at..at),
                    injection: None,
                }
            }
            Origin::Replaced(range) => Located {
                statement: statement_at(range.start),
                source: Some(range.clone()),
                injection: None,
            },
            Origin::Injected(statement, injection) => Located {
                statement: *statement,
                source: match injection {
                    Injection::Stub(k) => self.stubs.get(*k).map(|s| s.call..s.call),
                    _ => None,
                },
                injection: Some(*injection),
            },
        }
    }
}

#[derive(Default)]
struct Script {
    text: String,
    segments: Vec<Segment>,
}

impl Script {
    fn push(&mut self, text: &str, origin: Origin) {
        if text.is_empty() && !matches!(origin, Origin::Replaced(_)) {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        self.segments.push(Segment {
            script: start..self.text.len(),
            origin,
        });
    }

    fn inject(&mut self, text: &str, statement: Option<usize>, injection: Injection) {
        self.push(text, Origin::Injected(statement, injection));
    }

    fn source(&mut self, sql: &str, range: Range<usize>) {
        if range.start < range.end {
            self.push(&sql[range.clone()], Origin::Source(range.start));
        }
    }

    fn replace(&mut self, text: &str, range: Range<usize>) {
        self.push(text, Origin::Replaced(range));
    }
}

/// What a `CREATE`/`ALTER POLICY … TO …` needs.
struct PolicyRoles {
    /// The role list, to rewrite as `PUBLIC` (the roles do not exist in the
    /// sandbox); `None` when it already is just `PUBLIC`.
    rewrite: Option<Range<usize>>,
    /// The roles to record for the model; `Some(vec![])` resets them to
    /// PUBLIC (an `ALTER POLICY … TO PUBLIC`).
    record: Option<Vec<String>>,
}

/// The `TO` list of a policy statement. Postgres keeps only PUBLIC when the
/// list names it alongside other roles (with a warning), and remus reports
/// PUBLIC as no roles: the model must say the same. `CURRENT_USER` and the
/// like are refused by the allowlist (no name to restore).
fn policy_roles(statement: &Statement, parsed: &ParsedSql) -> Option<PolicyRoles> {
    let (roles, alter) = match &statement.node {
        NodeEnum::CreatePolicyStmt(s) => (&s.roles, false),
        NodeEnum::AlterPolicyStmt(s) => (&s.roles, true),
        _ => return None,
    };
    let specs: Vec<&pg_query::protobuf::RoleSpec> = roles
        .iter()
        .filter_map(|r| match r.node.as_ref()? {
            NodeEnum::RoleSpec(spec) => Some(spec),
            _ => None,
        })
        .collect();
    if specs.is_empty() {
        return None;
    }
    let public = specs
        .iter()
        .any(|spec| spec.roletype() == RoleSpecType::RolespecPublic);
    let named: Vec<String> = specs
        .iter()
        .filter(|spec| spec.roletype() != RoleSpecType::RolespecPublic)
        .map(|spec| spec.rolename.clone())
        .collect();
    let rewrite = if named.is_empty() {
        None
    } else {
        let start = specs.iter().filter_map(|s| location(s.location)).min()?;
        let last = specs.iter().filter_map(|s| location(s.location)).max()?;
        let end = parsed.token_at(last).map_or(last, |t| t.end);
        Some(start..end)
    };
    let record = if public {
        alter.then(Vec::new)
    } else {
        let mut unique = Vec::new();
        for role in named {
            if !unique.contains(&role) {
                unique.push(role);
            }
        }
        Some(unique)
    };
    Some(PolicyRoles { rewrite, record })
}

/// The statement that records a policy's written roles against its OID.
fn record_policy_roles(statement: &Statement, roles: &[String]) -> Option<String> {
    let (table, name) = match &statement.node {
        NodeEnum::CreatePolicyStmt(s) => (s.table.as_ref()?, &s.policy_name),
        NodeEnum::AlterPolicyStmt(s) => (s.table.as_ref()?, &s.policy_name),
        _ => return None,
    };
    let relation = QualifiedName::from_range_var(table).to_sql();
    let roles: Vec<String> = roles.iter().map(|r| quote_literal(r)).collect();
    Some(format!(
        "INSERT INTO {POLICY_ROLES} (policy, roles) SELECT p.oid, ARRAY[{}]::text[] \
         FROM pg_catalog.pg_policy p WHERE p.polrelid = {}::regclass AND p.polname = {} \
         ON CONFLICT (policy) DO UPDATE SET roles = excluded.roles;\n",
        roles.join(", "),
        quote_literal(&relation),
        quote_literal(name)
    ))
}

/// `CREATE MATERIALIZED VIEW … [WITH DATA]` → `… WITH NO DATA`.
fn materialized_view_without_data(
    statement: &Statement,
    parsed: &ParsedSql,
    sql: &str,
) -> Option<(Range<usize>, String)> {
    let NodeEnum::CreateTableAsStmt(s) = &statement.node else {
        return None;
    };
    if s.objtype() != ObjectType::ObjectMatview || s.into.as_ref().is_some_and(|i| i.skip_data) {
        return None;
    }
    let code: Vec<_> = parsed
        .tokens_of(statement)
        .iter()
        .filter(|t| !t.is_comment())
        .collect();
    if let [.., with, data] = code.as_slice()
        && sql[with.start..with.end].eq_ignore_ascii_case("with")
        && sql[data.start..data.end].eq_ignore_ascii_case("data")
    {
        return Some((data.start..data.end, "NO DATA".to_owned()));
    }
    let end = statement.span.end;
    Some((end..end, " WITH NO DATA".to_owned()))
}

/// Statements that create (or comment on, alter, index…) an object an
/// extension created earlier in the text provides: (statement, extension,
/// object name).
fn provided_by_extensions(
    statements: &[Statement],
    catalog: &Catalog,
) -> Vec<(usize, String, String)> {
    let mut provided: Vec<(String, String, String)> = Vec::new();
    let mut skipped = Vec::new();
    for statement in statements {
        if let NodeEnum::CreateExtensionStmt(s) = &statement.node {
            let mut schema = None;
            let mut cascade = false;
            for option in &s.options {
                if let Some(NodeEnum::DefElem(def)) = option.node.as_ref() {
                    match def.defname.as_str() {
                        "schema" => {
                            schema = def
                                .arg
                                .as_deref()
                                .and_then(crate::names::string)
                                .map(str::to_owned);
                        }
                        "cascade" => cascade = true,
                        _ => {}
                    }
                }
            }
            let names = if cascade {
                catalog.with_requirements(&s.extname)
            } else {
                vec![s.extname.clone()]
            };
            for name in names {
                let Some(extension) = catalog.extension(&name) else {
                    continue;
                };
                let install = extension
                    .fixed_schema
                    .clone()
                    .or_else(|| schema.clone())
                    .unwrap_or_else(|| statement.search_path.creation_schema().to_owned());
                for ProvidedObject {
                    schema,
                    name: object,
                    ..
                } in &extension.objects
                {
                    let schema = schema.clone().unwrap_or_else(|| install.clone());
                    provided.push((schema, object.clone(), name.clone()));
                }
            }
            continue;
        }
        let Some((schema, name)) = target(statement) else {
            continue;
        };
        if let Some((_, _, extension)) =
            provided.iter().find(|(s, n, _)| *s == schema && *n == name)
        {
            skipped.push((
                statement.index,
                extension.clone(),
                format!("{}.{}", quote_ident(&schema), quote_ident(&name)),
            ));
        }
    }
    skipped
}

/// The relation or type a statement creates or changes, for the extension
/// check.
fn target(statement: &Statement) -> Option<(String, String)> {
    if let NodeEnum::IndexStmt(s) = &statement.node {
        let rv = s.relation.as_ref()?;
        return Some(QualifiedName::from_range_var(rv).resolve(&statement.search_path));
    }
    let key = &statement.key;
    let relevant_action = matches!(
        key.action(),
        Action::Create | Action::Comment | Action::Alter
    );
    let relevant_kind = matches!(
        key.kind(),
        ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::MaterializedView
            | ObjectKind::Sequence
            | ObjectKind::Type
            | ObjectKind::Enum
            | ObjectKind::Domain
            | ObjectKind::Column
            | ObjectKind::Constraint
            | ObjectKind::ForeignKey
            | ObjectKind::Policy
            | ObjectKind::RowSecurity
            | ObjectKind::ForceRowSecurity
    );
    match key.path() {
        [schema, name, ..] if relevant_action && relevant_kind => {
            Some((schema.clone(), name.clone()))
        }
        _ => None,
    }
}

/// Schemas the text uses (for its objects, the relations and types it
/// names, and the stubs it needs) without creating them.
fn missing_schemas(statements: &[Statement], catalog: &Catalog, stubs: &[Stub]) -> Vec<String> {
    let mut needed = BTreeSet::new();
    let mut created = HashSet::new();
    for statement in statements {
        let key = &statement.key;
        match (key.action(), key.kind(), key.path()) {
            (Action::Create, ObjectKind::Schema, [name]) => {
                created.insert(name.clone());
            }
            (
                Action::Create,
                ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::MaterializedView
                | ObjectKind::Sequence
                | ObjectKind::Enum
                | ObjectKind::Type
                | ObjectKind::Domain
                | ObjectKind::Statistics
                | ObjectKind::Collation
                | ObjectKind::Function,
                [schema, ..],
            ) => {
                needed.insert(schema.clone());
            }
            _ => {}
        }
        if let NodeEnum::CreateExtensionStmt(s) = &statement.node {
            for name in catalog.with_requirements(&s.extname) {
                if let Some(schema) = catalog
                    .extension(&name)
                    .and_then(|e| e.fixed_schema.clone())
                {
                    created.insert(schema);
                }
            }
        }
        if let NodeEnum::CreateSchemaStmt(s) = &statement.node {
            for element in &s.schema_elts {
                if let Some(value) = element
                    .node
                    .as_ref()
                    .and_then(|n| serde_json::to_value(n).ok())
                {
                    collect_qualifiers(&value, &mut needed);
                }
            }
            continue;
        }
        if let Ok(value) = serde_json::to_value(&statement.node) {
            collect_qualifiers(&value, &mut needed);
        }
    }
    needed.extend(stubs.iter().map(|s| s.schema.clone()));
    needed
        .into_iter()
        .filter(|s| {
            !created.contains(s)
                && !matches!(s.as_str(), "public" | "information_schema")
                && !s.starts_with("pg_")
        })
        .collect()
}

/// Schema names written in relation and type references.
fn collect_qualifiers(value: &serde_json::Value, out: &mut BTreeSet<String>) {
    walk::visit_objects(value, &mut |object| {
        if object.contains_key("relname")
            && let Some(schema) = object.get("schemaname").and_then(|s| s.as_str())
            && !schema.is_empty()
        {
            out.insert(schema.to_owned());
        }
        if object.contains_key("typemod")
            && let Some(names) = object.get("names").and_then(|n| n.as_array())
            && names.len() >= 2
            && let Some(schema) = names[names.len() - 2]["node"]["String"]["sval"].as_str()
        {
            out.insert(schema.to_owned());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        Injection, POLICY_ROLES, PROGRESS, prepare, progress_value, statement_of_progress,
    };
    use crate::{
        catalog::{Catalog, Extension, ProvidedKind, ProvidedObject},
        parse,
        stubs::Overrides,
    };

    fn catalog() -> Catalog {
        Catalog::new(
            ["now", "lower"].map(str::to_owned),
            [
                (
                    "postgis".to_owned(),
                    Extension {
                        relocatable: false,
                        functions: ["st_makepoint"].map(str::to_owned).into_iter().collect(),
                        objects: vec![
                            ProvidedObject {
                                schema: None,
                                name: "spatial_ref_sys".to_owned(),
                                kind: ProvidedKind::Relation,
                            },
                            ProvidedObject {
                                schema: None,
                                name: "geometry_dump".to_owned(),
                                kind: ProvidedKind::Type,
                            },
                        ],
                        ..Extension::default()
                    },
                ),
                (
                    "postgis_topology".to_owned(),
                    Extension {
                        requires: vec!["postgis".to_owned()],
                        fixed_schema: Some("topology".to_owned()),
                        objects: vec![ProvidedObject {
                            schema: Some("topology".to_owned()),
                            name: "layer".to_owned(),
                            kind: ProvidedKind::Relation,
                        }],
                        ..Extension::default()
                    },
                ),
            ],
        )
    }

    fn script(sql: &str) -> super::Prepared {
        let parsed = parse(sql);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        prepare(sql, &parsed, &catalog(), &Overrides::default())
    }

    /// The script without the progress markers, for readable assertions.
    fn statements(prepared: &super::Prepared) -> Vec<&str> {
        prepared
            .script
            .lines()
            .filter(|l| !l.contains(PROGRESS))
            .collect()
    }

    #[test]
    fn statements_run_in_order_between_progress_markers() {
        let prepared = script("CREATE TABLE a (id int); -- note\nCOMMENT ON TABLE a IS 'x'");
        assert_eq!(
            prepared.script,
            format!(
                "SELECT pg_catalog.setval('{PROGRESS}', 1, false);\n\
             SELECT pg_catalog.setval('{PROGRESS}', 2, true);\n\
             CREATE TABLE a (id int);\n\
             SELECT pg_catalog.setval('{PROGRESS}', 3, true);\n\
             COMMENT ON TABLE a IS 'x';\n"
            )
        );
        assert_eq!(statement_of_progress(progress_value(7)), Some(7));
        assert_eq!(statement_of_progress(1), None);
    }

    #[test]
    fn policy_roles_become_public_and_are_recorded() {
        let prepared = script(
            "CREATE POLICY own ON app.docs AS RESTRICTIVE FOR SELECT TO app_user, \"Admin\", app_user USING (true);
             CREATE POLICY pub ON docs TO PUBLIC USING (true);
             ALTER POLICY own ON app.docs TO auditor;
             CREATE POLICY mixed ON docs TO app_user, PUBLIC USING (true);
             ALTER POLICY own ON app.docs TO PUBLIC;",
        );
        assert_eq!(
            statements(&prepared),
            [
                // `app` is used but never created: the compile creates it.
                "CREATE SCHEMA app;",
                "CREATE POLICY own ON app.docs AS RESTRICTIVE FOR SELECT TO PUBLIC USING (true);",
                &format!(
                    "INSERT INTO {POLICY_ROLES} (policy, roles) SELECT p.oid, ARRAY['app_user', 'Admin']::text[] FROM pg_catalog.pg_policy p WHERE p.polrelid = 'app.docs'::regclass AND p.polname = 'own' ON CONFLICT (policy) DO UPDATE SET roles = excluded.roles;"
                ),
                "CREATE POLICY pub ON docs TO PUBLIC USING (true);",
                "ALTER POLICY own ON app.docs TO PUBLIC;",
                &format!(
                    "INSERT INTO {POLICY_ROLES} (policy, roles) SELECT p.oid, ARRAY['auditor']::text[] FROM pg_catalog.pg_policy p WHERE p.polrelid = 'app.docs'::regclass AND p.polname = 'own' ON CONFLICT (policy) DO UPDATE SET roles = excluded.roles;"
                ),
                // PUBLIC among other roles is PUBLIC, as Postgres keeps it:
                // rewritten (the others do not exist), nothing to restore.
                "CREATE POLICY mixed ON docs TO PUBLIC USING (true);",
                // Back to PUBLIC: the roles recorded earlier are reset.
                "ALTER POLICY own ON app.docs TO PUBLIC;",
                &format!(
                    "INSERT INTO {POLICY_ROLES} (policy, roles) SELECT p.oid, ARRAY[]::text[] FROM pg_catalog.pg_policy p WHERE p.polrelid = 'app.docs'::regclass AND p.polname = 'own' ON CONFLICT (policy) DO UPDATE SET roles = excluded.roles;"
                ),
            ]
        );
        assert!(prepared.records_policy_roles);
    }

    #[test]
    fn materialized_views_are_created_without_data() {
        let prepared = script(
            "CREATE MATERIALIZED VIEW a AS SELECT 1 -- inline\n;
             CREATE MATERIALIZED VIEW b AS SELECT 1 WITH DATA;
             CREATE MATERIALIZED VIEW c AS SELECT 1 WITH NO DATA;",
        );
        assert_eq!(
            statements(&prepared),
            [
                "CREATE MATERIALIZED VIEW a AS SELECT 1 WITH NO DATA;",
                "CREATE MATERIALIZED VIEW b AS SELECT 1 WITH NO DATA;",
                "CREATE MATERIALIZED VIEW c AS SELECT 1 WITH NO DATA;",
            ]
        );
    }

    #[test]
    fn stubs_come_right_before_their_first_caller_and_missing_schemas_first() {
        let prepared = script(
            "CREATE SCHEMA app;
             CREATE TABLE app.t (id bigint DEFAULT app.next_id(), owner uuid DEFAULT auth.uid());
             CREATE TABLE u (id bigint DEFAULT app.next_id());",
        );
        assert_eq!(prepared.created_schemas, ["auth"]);
        let lines = statements(&prepared);
        assert_eq!(lines[0], "CREATE SCHEMA auth;");
        assert_eq!(lines[1], "CREATE SCHEMA app;");
        assert!(lines[2].starts_with("CREATE FUNCTION app.next_id() RETURNS pg_catalog.int8"));
        assert!(lines[3].starts_with("CREATE FUNCTION auth.uid() RETURNS uuid"));
        assert!(lines[4].starts_with("CREATE TABLE app.t"));
        assert!(lines[5].starts_with("CREATE TABLE u"));
        assert_eq!(lines.len(), 6);
    }

    #[test]
    fn objects_an_extension_provides_are_skipped_after_it() {
        let prepared = script(
            "CREATE TABLE spatial_ref_sys (srid int);
             CREATE EXTENSION IF NOT EXISTS postgis WITH SCHEMA public;
             CREATE TYPE public.geometry_dump AS (path int[]);
             CREATE TABLE public.spatial_ref_sys (srid int);
             COMMENT ON TABLE public.spatial_ref_sys IS 'x';
             CREATE TABLE places (id int DEFAULT st_makepoint(1, 2));
             CREATE SCHEMA IF NOT EXISTS topology;
             CREATE EXTENSION postgis_topology CASCADE;
             CREATE TABLE topology.layer (id int);",
        );
        let notes: Vec<&str> = prepared.notes.iter().map(|n| n.message.as_str()).collect();
        assert_eq!(
            notes,
            [
                "public.geometry_dump is created by extension postgis; this statement was skipped",
                "public.spatial_ref_sys is created by extension postgis; this statement was skipped",
                "public.spatial_ref_sys is created by extension postgis; this statement was skipped",
                "topology.layer is created by extension postgis_topology; this statement was skipped",
            ]
        );
        // The first CREATE TABLE runs: nothing provides it yet. No stub for
        // an extension's function, no schema created for a fixed one.
        let lines = statements(&prepared);
        assert_eq!(lines.len(), 5);
        assert_eq!(prepared.stubs, []);
        assert_eq!(prepared.created_schemas, Vec::<String>::new());
    }

    #[test]
    fn script_positions_map_back_to_the_text() {
        let sql = "CREATE TABLE a (id int);\nCREATE POLICY p ON a TO admin USING (oops = 1);\nCREATE TABLE b (x int DEFAULT f());";
        let parsed = parse(sql);
        let prepared = prepare(sql, &parsed, &catalog(), &Overrides::default());

        let oops = prepared.script.find("oops").unwrap();
        let located = prepared.locate(oops, &parsed);
        assert_eq!(located.statement, Some(1));
        assert_eq!(
            located.source,
            Some(sql.find("oops").unwrap()..sql.find("oops").unwrap())
        );

        let public = prepared.script.find("TO PUBLIC").unwrap() + 3;
        let located = prepared.locate(public, &parsed);
        let admin = sql.find("admin").unwrap();
        assert_eq!(located.source, Some(admin..admin + "admin".len()));

        let stub = prepared.script.find("CREATE FUNCTION").unwrap();
        let located = prepared.locate(stub + 20, &parsed);
        assert_eq!(located.injection, Some(Injection::Stub(0)));
        assert_eq!(located.statement, Some(2));
        let call = sql.find("f()").unwrap();
        assert_eq!(located.source, Some(call..call));

        let marker = prepared.script.find("setval").unwrap();
        assert_eq!(
            prepared.locate(marker, &parsed).injection,
            Some(Injection::Prelude)
        );
    }
}
