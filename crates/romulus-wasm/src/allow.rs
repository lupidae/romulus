//! The drafts' DDL allowlist, as the app's sandbox applies it
//! (romulus-sandbox's `allowlist`): the same statements refused, with the
//! same words, before anything runs.

use serde_json::Value;

use crate::{tree, types};

const HINT_DATA: &str = "Data does not belong in a schema version: remove the statement (keep it in a migration or a seed file).";
const HINT_CODE: &str = "The sandbox only runs schema DDL; procedural code cannot run in it.";
const HINT_TRANSACTION: &str =
    "romulus runs the whole text in one transaction: remove BEGIN, COMMIT and SAVEPOINT.";
const HINT_PRIVILEGES: &str =
    "Privileges and ownership are not part of the schema model: remove the statement or clause.";
const HINT_FUNCTIONS: &str = "Functions and triggers are not part of the schema model: remove the definition. Calls to functions the text does not define are stubbed automatically.";
const HINT_CLUSTER: &str =
    "Roles, databases, tablespaces and server settings are outside a schema: remove the statement.";
const HINT_FOREIGN: &str =
    "Foreign tables need a foreign server, which the sandbox does not have: remove the statement.";

/// Why the statement at `start..end` is refused, and what to do: `None`
/// when it is allowed.
#[allow(clippy::too_many_lines)] // one arm per statement kind
pub fn not_allowed(sql: &str, start: usize, end: usize, node: &Value) -> Option<(String, String)> {
    let (kind, f) = tree::kind(node)?;
    let statement = |hint: &str| {
        Some((
            format!(
                "{} is not allowed",
                label(sql.get(start..end).unwrap_or_default())
            ),
            hint.to_owned(),
        ))
    };
    let refuse = |message: &str, hint: &str| Some((message.to_owned(), hint.to_owned()));
    match kind {
        "CreateSchemaStmt" => {
            if tree::field(f, "authrole").is_some() {
                return refuse("CREATE SCHEMA … AUTHORIZATION is not allowed", HINT_PRIVILEGES);
            }
            tree::list(f, "schemaElts")
                .iter()
                .find_map(|e| not_allowed(sql, start, end, e))
        }
        "CreateStmt" => {
            if tree::field(f, "relation").is_some_and(|r| tree::str(r, "relpersistence") == "t") {
                return refuse(
                    "temporary objects are not allowed",
                    "A temporary object is not part of a schema: drop TEMPORARY.",
                );
            }
            if tree::opt_str(f, "tablespacename").is_some() {
                return refuse(
                    "TABLESPACE is not allowed",
                    "Tablespaces are not part of the schema model: remove the clause.",
                );
            }
            None
        }
        "AlterTableStmt" => {
            if tree::str(f, "objtype") == "OBJECT_FOREIGN_TABLE" {
                return statement(HINT_FOREIGN);
            }
            tree::list(f, "cmds").iter().find_map(|cmd| {
                let cmd = tree::as_kind(cmd, "AlterTableCmd")?;
                match tree::str(cmd, "subtype") {
                    "AT_ChangeOwner" => refuse("ALTER … OWNER TO is not allowed", HINT_PRIVILEGES),
                    "AT_SetTableSpace" => refuse(
                        "SET TABLESPACE is not allowed",
                        "Tablespaces are not part of the schema model: remove the clause.",
                    ),
                    s if s.contains("Trig") || s.contains("Rule") => refuse(
                        "enabling or disabling triggers and rules is not allowed",
                        HINT_FUNCTIONS,
                    ),
                    "AT_GenericOptions" | "AT_AlterColumnGenericOptions" => {
                        refuse("OPTIONS is not allowed", HINT_FOREIGN)
                    }
                    _ => None,
                }
            })
        }
        "CreateTableAsStmt" => {
            if tree::bool(f, "is_select_into") {
                refuse(
                    "SELECT … INTO is not allowed",
                    "Create the table with its columns instead: a query cannot run in the sandbox.",
                )
            } else if tree::str(f, "objtype") != "OBJECT_MATVIEW" {
                refuse(
                    "CREATE TABLE … AS is not allowed",
                    "Create the table with its columns instead: a query cannot run in the sandbox.",
                )
            } else {
                None
            }
        }
        "IndexStmt" => {
            if tree::bool(f, "concurrent") {
                refuse(
                    "CREATE INDEX CONCURRENTLY is not allowed",
                    "Drop CONCURRENTLY: it cannot run inside the compile's transaction, and the schema is the same without it.",
                )
            } else if tree::opt_str(f, "tableSpace").is_some() {
                refuse(
                    "TABLESPACE is not allowed",
                    "Tablespaces are not part of the schema model: remove the clause.",
                )
            } else {
                None
            }
        }
        "CreateExtensionStmt" => {
            let name = tree::str(f, "extname");
            if types::extension(name).is_none() {
                let offered: Vec<&str> = types::EXTENSIONS.iter().map(|e| e.name).collect();
                return Some((
                    format!("extension \"{name}\" is not available"),
                    format!("The sandbox offers {}.", offered.join(", ")),
                ));
            }
            None
        }
        "CommentStmt" => (!object_allowed(tree::str(f, "objtype"), &COMMENTABLE))
            .then(|| statement(hint_for(tree::str(f, "objtype"))))
            .flatten(),
        "RenameStmt" => (!object_allowed(tree::str(f, "renameType"), &RENAMABLE))
            .then(|| statement(hint_for(tree::str(f, "renameType"))))
            .flatten(),
        "AlterObjectSchemaStmt" => (!object_allowed(tree::str(f, "objectType"), &MOVABLE))
            .then(|| statement(hint_for(tree::str(f, "objectType"))))
            .flatten(),
        "DropStmt" => {
            if tree::bool(f, "concurrent") {
                return refuse(
                    "DROP INDEX CONCURRENTLY is not allowed",
                    "Drop CONCURRENTLY: it cannot run inside the compile's transaction.",
                );
            }
            (!object_allowed(tree::str(f, "removeType"), &DROPPABLE))
                .then(|| statement(hint_for(tree::str(f, "removeType"))))
                .flatten()
        }
        "VariableSetStmt" => {
            let search_path = tree::str(f, "name").eq_ignore_ascii_case("search_path")
                && matches!(
                    tree::str(f, "kind"),
                    "VAR_SET_VALUE" | "VAR_SET_DEFAULT" | "VAR_RESET"
                );
            (!search_path)
                .then(|| {
                    refuse(
                        "only SET search_path is allowed",
                        "Other settings do not change the schema, and the sandbox fixes its own.",
                    )
                })
                .flatten()
        }
        "DefineStmt" => (tree::str(f, "kind") != "OBJECT_COLLATION")
            .then(|| statement(HINT_FUNCTIONS))
            .flatten(),
        "CreatePolicyStmt" | "AlterPolicyStmt" => tree::list(f, "roles").iter().find_map(|r| {
            let r = tree::as_kind(r, "RoleSpec")?;
            let word = match tree::str(r, "roletype") {
                "ROLESPEC_CURRENT_USER" => "CURRENT_USER",
                "ROLESPEC_SESSION_USER" => "SESSION_USER",
                "ROLESPEC_CURRENT_ROLE" => "CURRENT_ROLE",
                _ => return None,
            };
            refuse(
                &format!("TO {word} is not allowed in a policy"),
                "It names whoever runs the SQL, which a schema cannot record: name the role instead.",
            )
        }),
        "ViewStmt" | "CreateSeqStmt" | "AlterSeqStmt" | "CreateEnumStmt" | "AlterEnumStmt"
        | "CreateDomainStmt" | "AlterDomainStmt" | "CompositeTypeStmt" | "CreateRangeStmt"
        | "CreateStatsStmt" => None,
        "InsertStmt" | "UpdateStmt" | "DeleteStmt" | "MergeStmt" | "SelectStmt" | "CopyStmt"
        | "TruncateStmt" | "RefreshMatViewStmt" => statement(HINT_DATA),
        "DoStmt" | "CallStmt" | "ExecuteStmt" | "PrepareStmt" | "DeallocateStmt"
        | "DeclareCursorStmt" | "FetchStmt" | "ClosePortalStmt" | "ExplainStmt" | "ListenStmt"
        | "UnlistenStmt" | "NotifyStmt" | "LockStmt" | "LoadStmt" | "DiscardStmt"
        | "VariableShowStmt" | "ConstraintsSetStmt" => statement(HINT_CODE),
        "TransactionStmt" => statement(HINT_TRANSACTION),
        "GrantStmt" | "GrantRoleStmt" | "AlterDefaultPrivilegesStmt" | "AlterOwnerStmt"
        | "ReassignOwnedStmt" | "DropOwnedStmt" | "SecLabelStmt" => statement(HINT_PRIVILEGES),
        "CreateFunctionStmt" | "AlterFunctionStmt" | "CreateTrigStmt" | "CreateEventTrigStmt"
        | "AlterEventTrigStmt" | "RuleStmt" | "CreateCastStmt" | "CreateOpClassStmt"
        | "CreateOpFamilyStmt" | "AlterOpFamilyStmt" | "AlterOperatorStmt"
        | "CreateConversionStmt" | "CreateTransformStmt" | "CreatePLangStmt" | "CreateAmStmt" => {
            statement(HINT_FUNCTIONS)
        }
        "CreateForeignTableStmt" | "CreateFdwStmt" | "AlterFdwStmt" | "CreateForeignServerStmt"
        | "AlterForeignServerStmt" | "CreateUserMappingStmt" | "AlterUserMappingStmt"
        | "DropUserMappingStmt" | "ImportForeignSchemaStmt" => statement(HINT_FOREIGN),
        _ => statement(HINT_CLUSTER),
    }
}

const COMMENTABLE: [&str; 15] = [
    "TABLE",
    "COLUMN",
    "VIEW",
    "MATVIEW",
    "INDEX",
    "SEQUENCE",
    "TYPE",
    "DOMAIN",
    "DOMCONSTRAINT",
    "TABCONSTRAINT",
    "SCHEMA",
    "POLICY",
    "EXTENSION",
    "COLLATION",
    "STATISTIC_EXT",
];
const RENAMABLE: [&str; 15] = [
    "TABLE",
    "VIEW",
    "MATVIEW",
    "INDEX",
    "SEQUENCE",
    "COLUMN",
    "ATTRIBUTE",
    "TABCONSTRAINT",
    "DOMCONSTRAINT",
    "TYPE",
    "DOMAIN",
    "SCHEMA",
    "POLICY",
    "COLLATION",
    "STATISTIC_EXT",
];
const MOVABLE: [&str; 9] = [
    "TABLE",
    "VIEW",
    "MATVIEW",
    "SEQUENCE",
    "TYPE",
    "DOMAIN",
    "EXTENSION",
    "COLLATION",
    "STATISTIC_EXT",
];
const DROPPABLE: [&str; 12] = [
    "TABLE",
    "VIEW",
    "MATVIEW",
    "INDEX",
    "SEQUENCE",
    "TYPE",
    "DOMAIN",
    "SCHEMA",
    "POLICY",
    "EXTENSION",
    "COLLATION",
    "STATISTIC_EXT",
];

fn object_allowed(objtype: &str, allowed: &[&str]) -> bool {
    objtype
        .strip_prefix("OBJECT_")
        .is_some_and(|o| allowed.contains(&o))
}

fn hint_for(objtype: &str) -> &'static str {
    match objtype.strip_prefix("OBJECT_").unwrap_or("") {
        "FUNCTION" | "PROCEDURE" | "ROUTINE" | "AGGREGATE" | "TRIGGER" | "RULE"
        | "EVENT_TRIGGER" | "OPERATOR" | "CAST" | "LANGUAGE" | "TRANSFORM" => HINT_FUNCTIONS,
        "FOREIGN_TABLE" | "FOREIGN_SERVER" | "FDW" | "USER_MAPPING" => HINT_FOREIGN,
        "LARGEOBJECT" => HINT_DATA,
        _ => HINT_CLUSTER,
    }
}

/// `INSERT`, `CREATE FUNCTION`, `COMMENT ON DATABASE`…: a statement's
/// leading keywords, upper-cased, for messages.
pub fn label(statement: &str) -> String {
    const MODIFIERS: &[&str] = &[
        "OR",
        "REPLACE",
        "TEMP",
        "TEMPORARY",
        "UNLOGGED",
        "GLOBAL",
        "LOCAL",
        "UNIQUE",
        "IF",
        "NOT",
        "EXISTS",
        "ONLY",
        "CONCURRENTLY",
        "TRUSTED",
        "PROCEDURAL",
        "RECURSIVE",
    ];
    const COMPOUND: &[&str] = &[
        "MATERIALIZED",
        "FOREIGN",
        "EVENT",
        "DEFAULT",
        "TEXT",
        "USER",
        "OPERATOR",
        "ACCESS",
    ];
    let words: Vec<String> = statement
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .take(8)
        .map(str::to_uppercase)
        .collect();
    let Some(first) = words.first() else {
        return "this statement".to_owned();
    };
    let wanted = match first.as_str() {
        "CREATE" | "ALTER" | "DROP" => 1,
        "COMMENT" | "SECURITY" | "REFRESH" | "IMPORT" => 2,
        _ => 0,
    };
    let mut out = vec![first.clone()];
    let mut taken = 0;
    for w in words.iter().skip(1) {
        if taken >= wanted {
            break;
        }
        if MODIFIERS.contains(&w.as_str()) {
            continue;
        }
        out.push(w.clone());
        if !COMPOUND.contains(&w.as_str()) {
            taken += 1;
        }
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::label;

    #[test]
    fn labels_name_the_statement() {
        assert_eq!(label("insert into t values (1)"), "INSERT");
        assert_eq!(label("CREATE OR REPLACE FUNCTION f() …"), "CREATE FUNCTION");
        assert_eq!(
            label("create materialized view v as select 1"),
            "CREATE MATERIALIZED VIEW"
        );
        assert_eq!(label("comment on database x is 'y'"), "COMMENT ON DATABASE");
        assert_eq!(label("DO $$ begin end $$"), "DO");
    }
}
