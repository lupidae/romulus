//! What a migration's statements become in the replay: kept, rewritten, or
//! skipped and reported.
//!
//! Migrations hold more than schema DDL. The replay keeps what shapes the
//! schema and reports the rest, per file:
//!
//! - **skipped and reported**: data (`INSERT`, `UPDATE`, `DELETE`, `MERGE`,
//!   `COPY` with its rows, `TRUNCATE`, `REFRESH`), code (`DO`, `SELECT`,
//!   `CALL`…), session settings, privileges and ownership, roles, databases
//!   and other server objects, maintenance (`VACUUM`, `REINDEX`), objects
//!   the model does not hold and the sandbox does not run (operators, casts,
//!   aggregates, languages, foreign data, text search dictionaries, whose
//!   templates read files, extensions it does not offer),
//!   psql meta-commands;
//! - **stripped**: transaction control (`BEGIN`, `COMMIT`, `SAVEPOINT`…):
//!   the replay is one transaction;
//! - **rewritten** for the replay (see [`rewrite`]): functions, procedures
//!   and triggers are created with a fixed body (their own code never runs:
//!   the sandbox's trusted extensions install as a superuser, and a function
//!   of the import's could capture one of their calls), rules do nothing,
//!   `CONCURRENTLY` is dropped, `CREATE TABLE … AS` creates the table
//!   without running its query, `AUTHORIZATION` is left out of `CREATE
//!   SCHEMA`;
//! - **kept**: everything else, then checked by the allowlist in import
//!   mode ([`crate::allowlist::check_import`]).
//!
//! Skipped statements are blanked (offsets, lines and columns of the rest
//! are unchanged), so errors still point into the file as written.

use std::ops::Range;

use pg_query::{
    NodeEnum,
    protobuf::{
        AlterTableType, CmdType, CreateTableAsStmt, FunctionParameterMode, IntoClause, Node,
        ObjectType, VariableSetKind,
    },
};
use serde::Serialize;

use super::lexer::{self, NotSql};
use crate::{
    allowlist::label,
    catalog::ALLOWED_EXTENSIONS,
    diagnostic::Diagnostic,
    names::strings,
    parse::{ParsedSql, Statement, parse},
};

/// Why a statement is not replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Rows: `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `COPY`, `TRUNCATE`,
    /// `REFRESH MATERIALIZED VIEW`.
    Data,
    /// Code and queries: `DO`, `SELECT`, `CALL`, `NOTIFY`, `LOCK`…
    Code,
    /// `BEGIN`, `COMMIT`, `SAVEPOINT`…: the replay is one transaction.
    Transaction,
    /// `SET` (other than `search_path`), `RESET`, `SET ROLE`.
    Settings,
    /// `GRANT`, `REVOKE`, `OWNER TO`, default privileges, security labels.
    Privileges,
    /// Roles, databases, tablespaces, publications, event triggers,
    /// `ALTER SYSTEM`.
    Server,
    /// `VACUUM`, `ANALYZE`, `CLUSTER`, `REINDEX`, `DETACH … FINALIZE`.
    Maintenance,
    /// Objects the schema model does not hold and the sandbox does not run:
    /// operators, casts, aggregates, languages, base types, foreign data,
    /// extensions the sandbox does not offer.
    Unsupported,
    /// psql meta-commands (`\connect`, `\restrict`…).
    Psql,
}

/// A statement of a migration that is not replayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Skipped {
    /// 1-based, in the migration's text.
    pub line: u32,
    /// `INSERT`, `DO`, `CREATE ROLE`, `\connect`…
    pub kind: String,
    pub reason: SkipReason,
    /// The statement's first line, at most 120 characters.
    pub text: String,
}

/// A migration's text, ready to replay.
#[derive(Debug, Clone)]
pub struct Analysis {
    /// The text with every skipped statement blanked (and `DO` blocks of
    /// DDL unwrapped, see `import::plpgsql`).
    pub sql: String,
    /// Statements unwrapped from a guarded `DO` block: the replay ignores
    /// "already exists" and "does not exist" errors from them.
    pub guarded: Vec<Range<usize>>,
    pub skipped: Vec<Skipped>,
    /// Syntax errors, located in the text.
    pub errors: Vec<Diagnostic>,
}

/// Classify a migration's statements (see the module documentation).
pub fn analyze(sql: &str) -> Analysis {
    let scanned = lexer::scan(sql);
    let lines = crate::source::LineIndex::new(sql);
    let mut skipped = Vec::new();
    let mut blanks: Vec<Range<usize>> = Vec::new();
    for (range, kind) in &scanned.not_sql {
        if *kind == NotSql::Psql {
            let text = sql[range.clone()].trim();
            skipped.push(Skipped {
                line: lines.position(sql, range.start).line,
                kind: text.split_whitespace().next().unwrap_or("\\").to_owned(),
                reason: SkipReason::Psql,
                text: first_line(text),
            });
        }
        blanks.push(range.clone());
    }
    // Django's sqlmigrate prints a comment where a migration runs Python
    // (`RunPython`): nothing to replay, but worth telling.
    for range in &scanned.top_level_lines {
        if sql[range.clone()].trim() == "-- THIS OPERATION CANNOT BE WRITTEN AS SQL" {
            skipped.push(Skipped {
                line: lines.position(sql, range.start).line,
                kind: "RunPython".to_owned(),
                reason: SkipReason::Code,
                text: "Raw Python operation (not exported as SQL)".to_owned(),
            });
        }
    }
    let text = lexer::blank(sql, blanks.iter().cloned());
    let parsed = parse(&text);
    let mut guarded = Vec::new();
    for statement in &parsed.statements {
        let span = statement.span;
        if let Some(unwrapped) =
            super::plpgsql::unwrap(&statement.node, &text[span.range()], span.start)
        {
            // The block goes, its statements stay where they are.
            let mut cursor = span.start;
            for kept in &unwrapped.statements {
                blanks.push(cursor..kept.start);
                cursor = kept.end;
            }
            blanks.push(cursor..semicolon_after(&text, span.end));
            if unwrapped.guarded {
                guarded.extend(unwrapped.statements);
            }
            continue;
        }
        let Some(reason) = skip_reason(&statement.node) else {
            continue;
        };
        skipped.push(Skipped {
            line: span.from.line,
            kind: label(&text, parsed.tokens_of(statement)),
            reason,
            text: first_line(&text[span.range()]),
        });
        // The statement and its `;`.
        blanks.push(span.start..semicolon_after(&text, span.end));
    }
    skipped.sort_by_key(|s| s.line);
    Analysis {
        sql: lexer::blank(sql, blanks),
        guarded,
        skipped,
        errors: parsed.errors,
    }
}

/// Just past the `;` that ends a statement ending at `end` (`end` itself
/// when none follows).
fn semicolon_after(text: &str, end: usize) -> usize {
    let after = &text[end..];
    after
        .find(|c: char| !c.is_whitespace())
        .filter(|&k| after[k..].starts_with(';'))
        .map_or(end, |k| end + k + 1)
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() > 120 {
        let cut: String = line.chars().take(119).collect();
        format!("{cut}…")
    } else {
        line.to_owned()
    }
}

/// Types only C functions take or return: such a function cannot be
/// stood in for, and only a superuser can create the real one.
const INTERNAL_TYPES: &[&str] = &[
    "any",
    "internal",
    "cstring",
    "opaque",
    "language_handler",
    "fdw_handler",
    "index_am_handler",
    "tsm_handler",
    "table_am_handler",
];

fn type_is_internal(type_name: Option<&pg_query::protobuf::TypeName>) -> bool {
    type_name
        .and_then(|t| strings(&t.names).last().cloned())
        .is_some_and(|name| INTERNAL_TYPES.contains(&name.as_str()))
}

/// Object kinds the replay skips: nothing in the model, and the sandbox
/// would refuse (or need a superuser for) them.
fn unsupported_object(objtype: ObjectType) -> bool {
    use ObjectType as O;
    matches!(
        objtype,
        O::ObjectAggregate
            | O::ObjectOperator
            | O::ObjectOpclass
            | O::ObjectOpfamily
            | O::ObjectCast
            | O::ObjectConversion
            | O::ObjectTransform
            | O::ObjectLanguage
            | O::ObjectAccessMethod
            | O::ObjectAmop
            | O::ObjectAmproc
            | O::ObjectForeignTable
            | O::ObjectForeignServer
            | O::ObjectFdw
            | O::ObjectUserMapping
            | O::ObjectTsparser
            | O::ObjectTstemplate
            // A dictionary's template runs C code reading files at creation.
            | O::ObjectTsdictionary
            | O::ObjectLargeobject
    )
}

fn server_object(objtype: ObjectType) -> bool {
    use ObjectType as O;
    matches!(
        objtype,
        O::ObjectDatabase
            | O::ObjectRole
            | O::ObjectTablespace
            | O::ObjectPublication
            | O::ObjectPublicationNamespace
            | O::ObjectPublicationRel
            | O::ObjectSubscription
            | O::ObjectEventTrigger
            | O::ObjectParameterAcl
    )
}

fn by_object(objtype: ObjectType) -> Option<SkipReason> {
    if unsupported_object(objtype) {
        Some(SkipReason::Unsupported)
    } else if server_object(objtype) {
        Some(SkipReason::Server)
    } else {
        None
    }
}

/// Why a statement is skipped; `None` when it is replayed (as written or
/// rewritten).
#[allow(clippy::too_many_lines)]
pub(crate) fn skip_reason(node: &NodeEnum) -> Option<SkipReason> {
    use NodeEnum as N;
    use SkipReason as R;
    match node {
        N::InsertStmt(_)
        | N::UpdateStmt(_)
        | N::DeleteStmt(_)
        | N::MergeStmt(_)
        | N::CopyStmt(_)
        | N::TruncateStmt(_)
        | N::RefreshMatViewStmt(_) => Some(R::Data),
        // `SELECT … INTO t` creates a table: rewritten, not skipped.
        N::SelectStmt(s) if s.into_clause.is_some() => None,
        N::SelectStmt(_)
        | N::DoStmt(_)
        | N::CallStmt(_)
        | N::ExecuteStmt(_)
        | N::PrepareStmt(_)
        | N::DeallocateStmt(_)
        | N::DeclareCursorStmt(_)
        | N::FetchStmt(_)
        | N::ClosePortalStmt(_)
        | N::ExplainStmt(_)
        | N::ListenStmt(_)
        | N::UnlistenStmt(_)
        | N::NotifyStmt(_)
        | N::LockStmt(_)
        | N::LoadStmt(_)
        | N::DiscardStmt(_)
        | N::VariableShowStmt(_)
        | N::ConstraintsSetStmt(_)
        | N::CheckPointStmt(_) => Some(R::Code),
        N::TransactionStmt(_) => Some(R::Transaction),
        N::VariableSetStmt(s) => {
            let search_path = s.name.eq_ignore_ascii_case("search_path")
                && matches!(
                    s.kind(),
                    VariableSetKind::VarSetValue
                        | VariableSetKind::VarSetDefault
                        | VariableSetKind::VarReset
                );
            (!search_path).then_some(R::Settings)
        }
        N::GrantStmt(_)
        | N::GrantRoleStmt(_)
        | N::AlterDefaultPrivilegesStmt(_)
        | N::AlterOwnerStmt(_)
        | N::ReassignOwnedStmt(_)
        | N::DropOwnedStmt(_)
        | N::SecLabelStmt(_) => Some(R::Privileges),
        N::CreateRoleStmt(_)
        | N::AlterRoleStmt(_)
        | N::AlterRoleSetStmt(_)
        | N::DropRoleStmt(_)
        | N::CreatedbStmt(_)
        | N::AlterDatabaseStmt(_)
        | N::AlterDatabaseSetStmt(_)
        | N::AlterDatabaseRefreshCollStmt(_)
        | N::DropdbStmt(_)
        | N::CreateTableSpaceStmt(_)
        | N::DropTableSpaceStmt(_)
        | N::AlterTableSpaceOptionsStmt(_)
        | N::AlterTableMoveAllStmt(_)
        | N::AlterSystemStmt(_)
        | N::CreatePublicationStmt(_)
        | N::AlterPublicationStmt(_)
        | N::CreateSubscriptionStmt(_)
        | N::AlterSubscriptionStmt(_)
        | N::DropSubscriptionStmt(_)
        | N::CreateEventTrigStmt(_)
        | N::AlterEventTrigStmt(_)
        | N::AlterCollationStmt(_) => Some(R::Server),
        N::VacuumStmt(_) | N::ClusterStmt(_) | N::ReindexStmt(_) => Some(R::Maintenance),
        N::CreateCastStmt(_)
        | N::CreateOpClassStmt(_)
        | N::CreateOpFamilyStmt(_)
        | N::AlterOpFamilyStmt(_)
        | N::AlterOperatorStmt(_)
        | N::AlterTypeStmt(_)
        | N::CreateConversionStmt(_)
        | N::CreateTransformStmt(_)
        | N::CreatePlangStmt(_)
        | N::CreateAmStmt(_)
        | N::CreateForeignTableStmt(_)
        | N::CreateFdwStmt(_)
        | N::AlterFdwStmt(_)
        | N::CreateForeignServerStmt(_)
        | N::AlterForeignServerStmt(_)
        | N::CreateUserMappingStmt(_)
        | N::AlterUserMappingStmt(_)
        | N::DropUserMappingStmt(_)
        | N::ImportForeignSchemaStmt(_)
        | N::AlterExtensionStmt(_)
        | N::AlterExtensionContentsStmt(_)
        | N::AlterTsdictionaryStmt(_) => Some(R::Unsupported),
        N::CreateExtensionStmt(s) => {
            (!ALLOWED_EXTENSIONS.contains(&s.extname.as_str())).then_some(R::Unsupported)
        }
        N::DefineStmt(s) => {
            use ObjectType as O;
            match s.kind() {
                O::ObjectCollation | O::ObjectTsconfiguration => None,
                _ => Some(R::Unsupported),
            }
        }
        N::DropStmt(s) => by_object(s.remove_type()),
        N::CommentStmt(s) => match s.objtype() {
            // A comment on an extension needs to own it (pg_dump writes one
            // for plpgsql).
            ObjectType::ObjectExtension => Some(R::Privileges),
            other => by_object(other),
        },
        N::RenameStmt(s) => by_object(s.rename_type()),
        N::AlterObjectSchemaStmt(s) => by_object(s.object_type()),
        N::AlterTableStmt(s) => {
            if s.objtype() == ObjectType::ObjectForeignTable {
                return Some(R::Unsupported);
            }
            alter_table_skip(&s.cmds)
        }
        N::CreateFunctionStmt(s) => {
            let internal = type_is_internal(s.return_type.as_ref())
                || s.parameters.iter().any(|p| match p.node.as_ref() {
                    Some(N::FunctionParameter(p)) => type_is_internal(p.arg_type.as_ref()),
                    _ => false,
                });
            internal.then_some(R::Unsupported)
        }
        N::AlterFunctionStmt(s) => {
            (kept_function_actions(&s.actions).count() == 0).then_some(R::Settings)
        }
        N::RuleStmt(s) => (s.event() == CmdType::CmdSelect).then_some(R::Unsupported),
        _ => None,
    }
}

/// An `ALTER TABLE` made only of commands the replay skips (ownership,
/// tablespaces, enabling or disabling triggers and rules, finalizing a
/// detach), and why.
fn alter_table_skip(cmds: &[Node]) -> Option<SkipReason> {
    use AlterTableType as T;
    let mut reason = None;
    for cmd in cmds {
        let Some(NodeEnum::AlterTableCmd(cmd)) = cmd.node.as_ref() else {
            return None;
        };
        let this = match cmd.subtype() {
            T::AtChangeOwner => SkipReason::Privileges,
            T::AtSetTableSpace => SkipReason::Server,
            T::AtEnableTrig
            | T::AtEnableAlwaysTrig
            | T::AtEnableReplicaTrig
            | T::AtDisableTrig
            | T::AtEnableTrigAll
            | T::AtDisableTrigAll
            | T::AtEnableTrigUser
            | T::AtDisableTrigUser
            | T::AtEnableRule
            | T::AtEnableAlwaysRule
            | T::AtEnableReplicaRule
            | T::AtDisableRule => SkipReason::Code,
            T::AtDetachPartitionFinalize => SkipReason::Maintenance,
            _ => return None,
        };
        reason.get_or_insert(this);
    }
    reason
}

/// `ALTER FUNCTION` actions the replay keeps: what decides where a
/// function may be called (volatility, strictness, parallel safety) and
/// costs. `SET`, `SECURITY`, `LEAKPROOF` (superuser only) and `SUPPORT`
/// would only matter when a real body runs.
fn kept_function_actions(actions: &[Node]) -> impl Iterator<Item = &Node> {
    actions.iter().filter(|a| {
        matches!(
            a.node.as_ref(),
            Some(NodeEnum::DefElem(d))
                if matches!(d.defname.as_str(), "volatility" | "strict" | "parallel" | "cost" | "rows")
        )
    })
}

/// The bodies the replay gives functions and procedures. A function
/// returning a value returns NULL; one returning rows, OUT parameters or
/// nothing returns at once.
pub(crate) const STUB_BODY_VALUE: &str = "BEGIN RETURN NULL; END";
pub(crate) const STUB_BODY_NOTHING: &str = "BEGIN RETURN; END";

fn string_node(value: &str) -> Node {
    Node {
        node: Some(NodeEnum::String(pg_query::protobuf::String {
            sval: value.to_owned(),
        })),
    }
}

fn def_elem(name: &str, arg: Node) -> Node {
    Node {
        node: Some(NodeEnum::DefElem(Box::new(pg_query::protobuf::DefElem {
            defname: name.to_owned(),
            arg: Some(Box::new(arg)),
            // Every enum must hold a real value: libpg_query's deparser
            // aborts the process on an undefined one.
            defaction: pg_query::protobuf::DefElemAction::DefelemUnspec.into(),
            ..Default::default()
        }))),
    }
}

/// A `CREATE FUNCTION`/`PROCEDURE` with its body replaced by a stub, in
/// PL/pgSQL, its signature and calling attributes kept (see
/// [`kept_function_actions`]).
fn stub_function(
    s: &pg_query::protobuf::CreateFunctionStmt,
) -> pg_query::protobuf::CreateFunctionStmt {
    let mut stub = s.clone();
    let returns_type = s
        .return_type
        .as_ref()
        .and_then(|t| strings(&t.names).last().cloned());
    let out_params = s.parameters.iter().any(|p| {
        matches!(
            p.node.as_ref(),
            Some(NodeEnum::FunctionParameter(p)) if matches!(
                p.mode(),
                FunctionParameterMode::FuncParamOut
                    | FunctionParameterMode::FuncParamInout
                    | FunctionParameterMode::FuncParamTable
            )
        )
    });
    let nothing = s.is_procedure
        || out_params
        || s.return_type.as_ref().is_some_and(|t| t.setof)
        || matches!(returns_type.as_deref(), Some("void" | "event_trigger"));
    let body = if nothing {
        STUB_BODY_NOTHING
    } else {
        STUB_BODY_VALUE
    };
    let mut options: Vec<Node> = kept_function_actions(&s.options).cloned().collect();
    options.push(def_elem("language", string_node("plpgsql")));
    options.push(def_elem(
        "as",
        Node {
            node: Some(NodeEnum::List(pg_query::protobuf::List {
                items: vec![string_node(body)],
            })),
        },
    ));
    stub.options = options;
    stub.sql_body = None;
    stub
}

/// A rewrite of one statement for the replay.
#[derive(Debug, Clone)]
pub(crate) struct Rewrite {
    /// Ranges of the text and what replaces them.
    pub edits: Vec<(Range<usize>, String)>,
    /// The statement as rewritten, when it is rewritten whole: what the
    /// allowlist checks instead of the original.
    pub node: Option<NodeEnum>,
}

impl Rewrite {
    fn whole(statement: &Statement, node: &NodeEnum) -> Result<Self, String> {
        let text = node
            .deparse()
            .map_err(|e| format!("could not rewrite the statement for the replay: {e}"))?;
        // What runs must be what was checked: parse the rewrite back.
        let reparsed = pg_query::parse(&text)
            .ok()
            .and_then(|p| p.protobuf.stmts.into_iter().next())
            .and_then(|s| s.stmt)
            .and_then(|s| s.node)
            .ok_or_else(|| "the statement's rewrite for the replay does not parse".to_owned())?;
        if std::mem::discriminant(&reparsed) != std::mem::discriminant(node) {
            return Err("the statement's rewrite for the replay is another statement".to_owned());
        }
        Ok(Self {
            edits: vec![(statement.span.range(), text)],
            node: Some(reparsed),
        })
    }

    /// A token-level edit; `node` is the statement as edited.
    fn token(range: Range<usize>, text: &str, node: NodeEnum) -> Self {
        Self {
            edits: vec![(range, text.to_owned())],
            node: Some(node),
        }
    }
}

/// The keyword token spelled `word` in a statement.
fn keyword(
    parsed: &ParsedSql,
    statement: &Statement,
    sql: &str,
    word: &str,
) -> Option<Range<usize>> {
    parsed
        .tokens_of(statement)
        .iter()
        .find(|t| !t.is_comment() && sql[t.start..t.end].eq_ignore_ascii_case(word))
        .map(|t| t.start..t.end)
}

/// How a kept statement is rewritten for the replay (see the module
/// documentation); `None` when it runs as written.
pub(crate) fn rewrite(
    statement: &Statement,
    parsed: &ParsedSql,
    sql: &str,
) -> Result<Option<Rewrite>, String> {
    use NodeEnum as N;
    Ok(match &statement.node {
        N::CreateFunctionStmt(s) => Some(Rewrite::whole(
            statement,
            &N::CreateFunctionStmt(Box::new(stub_function(s))),
        )?),
        N::AlterFunctionStmt(s) => {
            let mut kept = s.clone();
            kept.actions = kept_function_actions(&s.actions).cloned().collect();
            (kept.actions.len() != s.actions.len())
                .then(|| Rewrite::whole(statement, &N::AlterFunctionStmt(kept)))
                .transpose()?
        }
        N::CreateTrigStmt(s) if s.when_clause.is_some() => {
            let mut kept = s.clone();
            kept.when_clause = None;
            Some(Rewrite::whole(statement, &N::CreateTrigStmt(kept))?)
        }
        N::RuleStmt(s) if !s.actions.is_empty() || s.where_clause.is_some() => {
            let mut kept = s.clone();
            kept.actions.clear();
            kept.where_clause = None;
            Some(Rewrite::whole(statement, &N::RuleStmt(kept))?)
        }
        N::CreateSchemaStmt(s) if s.authrole.is_some() => {
            let mut kept = s.clone();
            let role = kept.authrole.take().map(|r| r.rolename).unwrap_or_default();
            if kept.schemaname.is_empty() {
                if role.is_empty() {
                    return Err("CREATE SCHEMA AUTHORIZATION needs a schema name here".to_owned());
                }
                kept.schemaname = role;
            }
            Some(Rewrite::whole(statement, &N::CreateSchemaStmt(kept))?)
        }
        N::IndexStmt(s) if s.concurrent => {
            let mut edited = s.clone();
            edited.concurrent = false;
            keyword(parsed, statement, sql, "concurrently")
                .map(|r| Rewrite::token(r, "", N::IndexStmt(edited)))
        }
        N::DropStmt(s) if s.concurrent => {
            let mut edited = s.clone();
            edited.concurrent = false;
            keyword(parsed, statement, sql, "concurrently")
                .map(|r| Rewrite::token(r, "", N::DropStmt(edited)))
        }
        N::AlterTableStmt(s) if detaches_concurrently(&s.cmds) => {
            let mut edited = s.clone();
            for cmd in &mut edited.cmds {
                if let Some(NodeEnum::AlterTableCmd(cmd)) = cmd.node.as_mut()
                    && let Some(NodeEnum::PartitionCmd(p)) =
                        cmd.def.as_mut().and_then(|d| d.node.as_mut())
                {
                    p.concurrent = false;
                }
            }
            keyword(parsed, statement, sql, "concurrently")
                .map(|r| Rewrite::token(r, "", N::AlterTableStmt(edited)))
        }
        N::CreateTableAsStmt(s)
            if s.objtype() == ObjectType::ObjectTable
                && !s.into.as_ref().is_some_and(|i| i.skip_data) =>
        {
            let mut edited = s.clone();
            if let Some(into) = edited.into.as_mut() {
                into.skip_data = true;
            }
            Some(without_data(
                statement,
                parsed,
                sql,
                N::CreateTableAsStmt(edited),
            ))
        }
        N::SelectStmt(s) if s.into_clause.is_some() => {
            let mut query = s.clone();
            let into = query.into_clause.take().map(|into| IntoClause {
                skip_data: true,
                ..*into
            });
            Some(Rewrite::whole(
                statement,
                &N::CreateTableAsStmt(Box::new(CreateTableAsStmt {
                    query: Some(Box::new(Node {
                        node: Some(N::SelectStmt(query)),
                    })),
                    into: into.map(Box::new),
                    objtype: ObjectType::ObjectTable.into(),
                    is_select_into: false,
                    if_not_exists: false,
                })),
            )?)
        }
        _ => None,
    })
}

fn detaches_concurrently(cmds: &[Node]) -> bool {
    cmds.iter().any(|cmd| {
        let Some(NodeEnum::AlterTableCmd(cmd)) = cmd.node.as_ref() else {
            return false;
        };
        matches!(
            cmd.def.as_ref().and_then(|d| d.node.as_ref()),
            Some(NodeEnum::PartitionCmd(p)) if p.concurrent
        )
    })
}

/// `CREATE TABLE … AS … [WITH DATA]` → `… WITH NO DATA`: the table gets
/// its columns, the query never runs.
fn without_data(statement: &Statement, parsed: &ParsedSql, sql: &str, edited: NodeEnum) -> Rewrite {
    let code: Vec<_> = parsed
        .tokens_of(statement)
        .iter()
        .filter(|t| !t.is_comment())
        .collect();
    if let [.., with, data] = code.as_slice()
        && sql[with.start..with.end].eq_ignore_ascii_case("with")
        && sql[data.start..data.end].eq_ignore_ascii_case("data")
    {
        return Rewrite::token(data.start..data.end, "NO DATA", edited);
    }
    let end = statement.span.end;
    Rewrite::token(end..end, " WITH NO DATA", edited)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skipped(sql: &str) -> Vec<(u32, String, SkipReason)> {
        analyze(sql)
            .skipped
            .into_iter()
            .map(|s| (s.line, s.kind, s.reason))
            .collect()
    }

    #[test]
    fn data_code_and_session_statements_are_skipped_and_reported() {
        let sql = "BEGIN;\nCREATE TABLE users (id int, email text);\nINSERT INTO users VALUES (1, 'a');\nUPDATE users SET email = lower(email);\nDO $$ BEGIN RAISE NOTICE 'x'; END $$;\nSELECT pg_catalog.set_config('search_path', '', false);\nSET statement_timeout = 0;\nSET search_path TO public;\nGRANT SELECT ON users TO app;\nALTER TABLE users OWNER TO app;\nCREATE ROLE app;\nVACUUM users;\nCOMMIT;\n";
        assert_eq!(
            skipped(sql),
            [
                (1, "BEGIN".to_owned(), SkipReason::Transaction),
                (3, "INSERT".to_owned(), SkipReason::Data),
                (4, "UPDATE".to_owned(), SkipReason::Data),
                (5, "DO".to_owned(), SkipReason::Code),
                (6, "SELECT".to_owned(), SkipReason::Code),
                (7, "SET".to_owned(), SkipReason::Settings),
                (9, "GRANT".to_owned(), SkipReason::Privileges),
                (10, "ALTER TABLE".to_owned(), SkipReason::Privileges),
                (11, "CREATE ROLE".to_owned(), SkipReason::Server),
                (12, "VACUUM".to_owned(), SkipReason::Maintenance),
                (13, "COMMIT".to_owned(), SkipReason::Transaction),
            ]
        );
        let analysis = analyze(sql);
        let kept = parse(&analysis.sql);
        let kinds: Vec<&str> = kept
            .statements
            .iter()
            .map(|s| s.text(&analysis.sql))
            .collect();
        assert_eq!(
            kinds,
            [
                "CREATE TABLE users (id int, email text)",
                "SET search_path TO public"
            ]
        );
        // Blanked, not removed: the kept statement is still on line 2.
        assert_eq!(kept.statements[0].span.from.line, 2);
    }

    #[test]
    fn copy_rows_and_psql_commands_are_skipped_with_their_statement() {
        let sql = "\\restrict k3y\nCREATE TABLE t (a int);\nCOPY public.t (a) FROM stdin;\n1\n2\n\\.\nCREATE INDEX t_a ON t (a);\n\\unrestrict k3y\n";
        let analysis = analyze(sql);
        assert!(analysis.errors.is_empty(), "{:?}", analysis.errors);
        assert_eq!(
            skipped(sql),
            [
                (1, "\\restrict".to_owned(), SkipReason::Psql),
                (3, "COPY".to_owned(), SkipReason::Data),
                (8, "\\unrestrict".to_owned(), SkipReason::Psql),
            ]
        );
        assert_eq!(parse(&analysis.sql).statements.len(), 2);
    }

    #[test]
    fn report_lines_hold_the_first_line_of_the_statement() {
        let report = analyze("\n\nINSERT INTO settings (key, value)\nVALUES ('a', 'b');").skipped;
        assert_eq!(report[0].line, 3);
        assert_eq!(report[0].text, "INSERT INTO settings (key, value)");
        let long = format!("INSERT INTO t VALUES ('{}');", "x".repeat(200));
        let report = analyze(&long).skipped;
        assert_eq!(report[0].text.chars().count(), 120);
        assert!(report[0].text.ends_with('…'));
    }

    #[test]
    fn objects_outside_the_model_are_skipped_with_their_drops_and_comments() {
        let sql = "CREATE EXTENSION IF NOT EXISTS timescaledb;\nCREATE EXTENSION IF NOT EXISTS pgcrypto;\nCREATE AGGREGATE my_sum (int) (sfunc = int4pl, stype = int);\nCREATE OPERATOR === (leftarg = int, rightarg = int, function = int4eq);\nDROP AGGREGATE my_sum (int);\nCOMMENT ON EXTENSION pgcrypto IS 'x';\nCREATE TEXT SEARCH CONFIGURATION fr (COPY = french);\nALTER TABLE t DISABLE TRIGGER ALL;\nALTER TABLE t DISABLE TRIGGER ALL, ADD COLUMN x int;\nCREATE FUNCTION h(internal) RETURNS internal LANGUAGE c AS 'x';\n";
        assert_eq!(
            skipped(sql),
            [
                (1, "CREATE EXTENSION".to_owned(), SkipReason::Unsupported),
                (3, "CREATE AGGREGATE".to_owned(), SkipReason::Unsupported),
                (4, "CREATE OPERATOR".to_owned(), SkipReason::Unsupported),
                (5, "DROP AGGREGATE".to_owned(), SkipReason::Unsupported),
                (6, "COMMENT ON EXTENSION".to_owned(), SkipReason::Privileges),
                (8, "ALTER TABLE".to_owned(), SkipReason::Code),
                (10, "CREATE FUNCTION".to_owned(), SkipReason::Unsupported),
            ]
        );
    }

    fn rewritten(sql: &str) -> Vec<String> {
        let parsed = parse(sql);
        parsed
            .statements
            .iter()
            .filter_map(|s| rewrite(s, &parsed, sql).unwrap())
            .map(|r| {
                let mut text = sql.to_owned();
                for (range, with) in r.edits.into_iter().rev() {
                    text.replace_range(range, &with);
                }
                text
            })
            .collect()
    }

    #[test]
    fn functions_get_a_stub_body_whatever_they_were_written_in() {
        let out = rewritten(
            "CREATE OR REPLACE FUNCTION app.touch() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path = app AS $$ BEGIN PERFORM pg_sleep(100); NEW.updated_at = now(); RETURN NEW; END $$",
        );
        assert_eq!(
            out,
            [
                "CREATE OR REPLACE FUNCTION app.touch() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NULL; END$$"
            ]
        );
        let out = rewritten(
            "CREATE FUNCTION slug(t text, sep text DEFAULT '-') RETURNS text IMMUTABLE STRICT PARALLEL SAFE LEAKPROOF LANGUAGE sql BEGIN ATOMIC SELECT lower(t); END",
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("IMMUTABLE"), "{}", out[0]);
        assert!(out[0].contains("RETURNS NULL ON NULL INPUT"), "{}", out[0]);
        assert!(out[0].contains("PARALLEL safe"), "{}", out[0]);
        assert!(out[0].contains("sep text = '-'"), "{}", out[0]);
        assert!(!out[0].contains("LEAKPROOF"), "{}", out[0]);
        assert!(!out[0].contains("lower"), "{}", out[0]);
        assert!(
            out[0].ends_with("LANGUAGE plpgsql AS $$BEGIN RETURN NULL; END$$"),
            "{}",
            out[0]
        );
        for sql in [
            "CREATE PROCEDURE p() LANGUAGE sql AS 'SELECT 1'",
            "CREATE FUNCTION f() RETURNS SETOF int LANGUAGE sql AS 'SELECT 1'",
            "CREATE FUNCTION f(OUT a int, OUT b int) LANGUAGE sql AS 'SELECT 1, 2'",
            "CREATE FUNCTION f() RETURNS TABLE (a int) LANGUAGE sql AS 'SELECT 1'",
            "CREATE FUNCTION f() RETURNS void LANGUAGE plpython3u AS 'import os'",
        ] {
            let out = rewritten(sql);
            assert!(
                out[0].contains("$$BEGIN RETURN; END$$"),
                "{sql}: {}",
                out[0]
            );
            assert!(out[0].contains("LANGUAGE plpgsql"), "{sql}: {}", out[0]);
        }
    }

    #[test]
    fn triggers_rules_and_concurrently_are_rewritten_to_run_nothing() {
        assert_eq!(
            rewritten(
                "CREATE TRIGGER t BEFORE UPDATE ON users FOR EACH ROW WHEN (OLD.* IS DISTINCT FROM NEW.*) EXECUTE FUNCTION touch()"
            ),
            ["CREATE TRIGGER t BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION touch()"]
        );
        assert_eq!(
            rewritten(
                "CREATE RULE r AS ON DELETE TO users WHERE OLD.id = 1 DO INSTEAD (UPDATE users SET deleted = true WHERE id = OLD.id)"
            ),
            ["CREATE RULE r AS ON DELETE TO users DO INSTEAD NOTHING"]
        );
        assert_eq!(
            rewritten("CREATE UNIQUE INDEX CONCURRENTLY users_email ON users (email)"),
            ["CREATE UNIQUE INDEX  users_email ON users (email)"]
        );
        assert_eq!(
            rewritten("DROP INDEX CONCURRENTLY IF EXISTS users_email"),
            ["DROP INDEX  IF EXISTS users_email"]
        );
        assert_eq!(
            rewritten("CREATE SCHEMA IF NOT EXISTS billing AUTHORIZATION app"),
            ["CREATE SCHEMA IF NOT EXISTS billing"]
        );
        assert_eq!(
            rewritten("CREATE TABLE archive AS SELECT * FROM users"),
            ["CREATE TABLE archive AS SELECT * FROM users WITH NO DATA"]
        );
        let out = rewritten("SELECT id, email INTO archive FROM users");
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("CREATE TABLE archive AS SELECT id, email FROM users"));
        assert!(out[0].ends_with("WITH NO DATA"));
    }
}
