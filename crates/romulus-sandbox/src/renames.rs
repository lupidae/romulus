//! The renames a text runs: its `ALTER … RENAME` statements, as known
//! renames for a migration (`romulus_diff::RenameChoices::known`).
//!
//! A draft's graph edits append them (`ALTER TABLE … RENAME COLUMN`), and a
//! migration written by hand has them: those renames are certain, where
//! comparing the two models could only guess. What a draft's text renames
//! is what it runs beyond its base's text: a statement the base already had
//! (a version published without Tidy keeps its graph edits) renamed
//! something before the base version, not since.

use pg_query::{NodeEnum, protobuf::ObjectType};
use romulus_migration::{Rename, RenameKind};

use crate::{
    key::{Action, ObjectKind},
    parse::parse,
};

/// The renames `sql` runs that `base` does not, in order, named as the
/// statements name them (resolved through `search_path` as the text sets
/// it). A text that does not parse gives the renames of the statements
/// that do.
pub fn known_renames(base: &str, sql: &str) -> Vec<Rename> {
    let mut seen: Vec<String> = renames_in(base).into_iter().map(|(text, _)| text).collect();
    let mut out = Vec::new();
    for (text, rename) in renames_in(sql) {
        if let Some(i) = seen.iter().position(|s| *s == text) {
            seen.remove(i);
            continue;
        }
        out.push(rename);
    }
    out
}

/// Each rename statement of `sql`: its text, whitespace squashed, and the
/// rename.
fn renames_in(sql: &str) -> Vec<(String, Rename)> {
    let parsed = parse(sql);
    let mut out = Vec::new();
    for statement in &parsed.statements {
        let text = squash(statement.text(sql));
        let key = &statement.key;
        let path = key.path();
        let rename = match statement.node() {
            NodeEnum::RenameStmt(s) if key.action() == Action::Rename => {
                let kind = match s.rename_type() {
                    ObjectType::ObjectTable | ObjectType::ObjectForeignTable => RenameKind::Table,
                    ObjectType::ObjectView => RenameKind::View,
                    ObjectType::ObjectMatview => RenameKind::MaterializedView,
                    ObjectType::ObjectIndex => RenameKind::Index,
                    ObjectType::ObjectSequence => RenameKind::Sequence,
                    ObjectType::ObjectColumn => RenameKind::Column,
                    ObjectType::ObjectAttribute => RenameKind::Attribute,
                    ObjectType::ObjectTabconstraint => RenameKind::Constraint,
                    ObjectType::ObjectPolicy => RenameKind::Policy,
                    ObjectType::ObjectType => RenameKind::Enum,
                    ObjectType::ObjectDomain => RenameKind::Domain,
                    _ => continue,
                };
                let child = matches!(
                    kind,
                    RenameKind::Column
                        | RenameKind::Attribute
                        | RenameKind::Constraint
                        | RenameKind::Policy
                );
                match (child, path) {
                    (false, [schema, name]) => Rename {
                        kind,
                        schema: schema.clone(),
                        parent: None,
                        from: name.clone(),
                        to: s.newname.clone(),
                        to_schema: None,
                    },
                    (true, [schema, parent, name]) => Rename {
                        kind,
                        schema: schema.clone(),
                        parent: Some(parent.clone()),
                        from: name.clone(),
                        to: s.newname.clone(),
                        to_schema: None,
                    },
                    _ => continue,
                }
            }
            // A move: `ALTER TABLE … SET SCHEMA`.
            NodeEnum::AlterObjectSchemaStmt(s) if key.action() == Action::Alter => {
                let kind = match s.object_type() {
                    ObjectType::ObjectTable => RenameKind::Table,
                    ObjectType::ObjectView => RenameKind::View,
                    ObjectType::ObjectMatview => RenameKind::MaterializedView,
                    ObjectType::ObjectType => RenameKind::Enum,
                    ObjectType::ObjectDomain => RenameKind::Domain,
                    _ => continue,
                };
                let [schema, name] = path else { continue };
                if s.newschema.is_empty() || s.newschema == *schema {
                    continue;
                }
                Rename {
                    kind,
                    schema: schema.clone(),
                    parent: None,
                    from: name.clone(),
                    to: name.clone(),
                    to_schema: Some(s.newschema.clone()),
                }
            }
            NodeEnum::AlterEnumStmt(s) if !s.old_val.is_empty() => {
                let ([schema, name], ObjectKind::Enum) = (path, key.kind()) else {
                    continue;
                };
                Rename {
                    kind: RenameKind::EnumValue,
                    schema: schema.clone(),
                    parent: Some(name.clone()),
                    from: s.old_val.clone(),
                    to: s.new_val.clone(),
                    to_schema: None,
                }
            }
            _ => continue,
        };
        out.push((text, rename));
    }
    out
}

fn squash(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rename(kind: RenameKind, parent: Option<&str>, from: &str, to: &str) -> Rename {
        Rename {
            kind,
            schema: "public".into(),
            parent: parent.map(str::to_owned),
            from: from.into(),
            to: to.into(),
            to_schema: None,
        }
    }

    #[test]
    fn every_kind_of_rename_statement_is_read() {
        let sql = "
            ALTER TABLE users RENAME TO accounts;
            ALTER TABLE accounts RENAME COLUMN email TO email_address;
            ALTER TABLE shop.orders RENAME CONSTRAINT orders_pkey TO orders_key;
            ALTER INDEX users_email_idx RENAME TO accounts_email_idx;
            ALTER SEQUENCE users_id_seq RENAME TO accounts_id_seq;
            ALTER VIEW v RENAME TO w;
            ALTER MATERIALIZED VIEW m RENAME TO n;
            ALTER POLICY p ON accounts RENAME TO q;
            ALTER TYPE mood RENAME TO feeling;
            ALTER TYPE feeling RENAME VALUE 'ok' TO 'fine';
            ALTER TYPE pair RENAME ATTRIBUTE a TO b;
            ALTER DOMAIN email RENAME TO mail;
            ALTER TYPE feeling ADD VALUE 'new';
            CREATE TABLE x (id int);
        ";
        let mut shop = rename(
            RenameKind::Constraint,
            Some("orders"),
            "orders_pkey",
            "orders_key",
        );
        shop.schema = "shop".into();
        assert_eq!(
            known_renames("", sql),
            [
                rename(RenameKind::Table, None, "users", "accounts"),
                rename(
                    RenameKind::Column,
                    Some("accounts"),
                    "email",
                    "email_address"
                ),
                shop,
                rename(
                    RenameKind::Index,
                    None,
                    "users_email_idx",
                    "accounts_email_idx"
                ),
                rename(
                    RenameKind::Sequence,
                    None,
                    "users_id_seq",
                    "accounts_id_seq"
                ),
                rename(RenameKind::View, None, "v", "w"),
                rename(RenameKind::MaterializedView, None, "m", "n"),
                rename(RenameKind::Policy, Some("accounts"), "p", "q"),
                rename(RenameKind::Enum, None, "mood", "feeling"),
                rename(RenameKind::EnumValue, Some("feeling"), "ok", "fine"),
                rename(RenameKind::Attribute, Some("pair"), "a", "b"),
                rename(RenameKind::Domain, None, "email", "mail"),
            ]
        );
    }

    #[test]
    fn a_move_to_another_schema_is_read() {
        let renames = known_renames(
            "",
            "ALTER TABLE users SET SCHEMA archive;\nALTER TYPE mood SET SCHEMA archive;",
        );
        assert_eq!(renames.len(), 2);
        assert_eq!(renames[0].to_schema.as_deref(), Some("archive"));
        assert_eq!(
            (renames[0].from.as_str(), renames[0].to.as_str()),
            ("users", "users")
        );
        assert_eq!(renames[1].kind, RenameKind::Enum);
    }

    #[test]
    fn renames_the_base_already_ran_are_not_the_drafts() {
        let base = "CREATE TABLE t (a int);\nALTER TABLE t RENAME COLUMN a TO b;\n";
        let draft = format!("{base}-- edits from the graph\nALTER TABLE t RENAME COLUMN b TO c;\n");
        assert_eq!(
            known_renames(base, &draft),
            [rename(RenameKind::Column, Some("t"), "b", "c")]
        );
        assert_eq!(known_renames(base, base), []);
    }

    #[test]
    fn the_search_path_the_text_sets_names_the_schema() {
        let sql = "SET search_path TO shop;\nALTER TABLE orders RENAME TO purchases;";
        let renames = known_renames("", sql);
        assert_eq!(renames[0].schema, "shop");
    }
}
