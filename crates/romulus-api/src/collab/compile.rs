//! What a draft's compiles tell the people in it, and the text tooling
//! around them. Drafts compile with the API's one [`SqlCompiler`]
//! (`compile_for`: coalesced per draft, `Rejected::Superseded` when a newer
//! text replaced a waiting one).

use std::collections::HashMap;

use romulus_sandbox::ObjectKind;
use serde::Serialize;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

pub use crate::compile::{Compiled, Rejected, SqlCompiler};
use crate::domain::DraftMode;

/// Hex SHA-256 of a text: how compile results name the text they are about.
pub fn sha256_hex(text: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// Hex SHA-256 of what a compile is of, as `draft_compiles.source_hash`
/// keeps it: the source text in schema mode; in migration mode the base
/// version's SQL and the migration (one migration on another base is
/// another compile). Clients only ever see [`sha256_hex`] of the text they
/// edit.
pub fn input_hash(mode: DraftMode, base: &str, source: &str) -> String {
    use std::fmt::Write as _;
    match mode {
        DraftMode::Schema => sha256_hex(source),
        DraftMode::Migration => {
            let mut hasher = Sha256::new();
            hasher.update(b"migration\0");
            hasher.update((base.len() as u64).to_be_bytes());
            hasher.update(base.as_bytes());
            hasher.update(source.as_bytes());
            hasher
                .finalize()
                .iter()
                .fold(String::with_capacity(64), |mut hex, byte| {
                    let _ = write!(hex, "{byte:02x}");
                    hex
                })
        }
    }
}

/// Where each relation the graph draws (tables, views, materialized views,
/// foreign tables) is created: qualified name → index of its `CREATE`
/// statement. Two compiles in a row that lose a name and gain another at the
/// same index saw a rename typed in the text (docs/MVP.md "Renames").
pub fn created_at(sql: &str) -> HashMap<String, usize> {
    romulus_sandbox::parse(sql)
        .statements
        .iter()
        .filter(|statement| {
            statement.key.action() == romulus_sandbox::Action::Create
                && matches!(
                    statement.key.kind(),
                    ObjectKind::Table
                        | ObjectKind::View
                        | ObjectKind::MaterializedView
                        | ObjectKind::ForeignTable
                )
        })
        .filter_map(|statement| match statement.key.path() {
            [schema, name] => Some((format!("{schema}.{name}"), statement.index)),
            _ => None,
        })
        .collect()
}

/// Renames a compile implies: `(old name, new name)` for each relation that
/// disappeared while another appeared from the same statement.
#[expect(clippy::implicit_hasher, reason = "only ever std maps")]
pub fn renames(
    before: &HashMap<String, usize>,
    after: &HashMap<String, usize>,
) -> Vec<(String, String)> {
    let gained: HashMap<usize, &String> = after
        .iter()
        .filter(|(name, _)| !before.contains_key(*name))
        .map(|(name, &index)| (index, name))
        .collect();
    let mut out: Vec<(String, String)> = before
        .iter()
        .filter(|(name, _)| !after.contains_key(*name))
        .filter_map(|(old, index)| gained.get(index).map(|new| (old.clone(), (*new).clone())))
        .collect();
    out.sort();
    out
}

/// A compile's outcome, as everyone in the draft receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompileState {
    /// The text compiled: `model` is its model.
    Ok,
    /// The text has errors (in `diagnostics`); `model`, when sent, is the
    /// last one that compiled.
    Failed,
    /// The compiler is unavailable; the text is unchecked.
    Unavailable,
    /// Not compiled yet (just loaded); `model` is the last one that did.
    Pending,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_hex_sha256() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn a_migration_is_hashed_with_its_base() {
        assert_eq!(input_hash(DraftMode::Schema, "base", "x"), sha256_hex("x"));
        let migration = input_hash(DraftMode::Migration, "base", "x");
        assert_ne!(migration, sha256_hex("x"));
        assert_ne!(migration, input_hash(DraftMode::Migration, "other", "x"));
        // The base and the migration never run into each other.
        assert_ne!(
            input_hash(DraftMode::Migration, "ab", "c"),
            input_hash(DraftMode::Migration, "a", "bc")
        );
    }

    #[test]
    fn relations_are_found_by_their_create_statement() {
        let sql = "CREATE SCHEMA app;\nCREATE TABLE users (id int);\nCOMMENT ON TABLE users IS 'x';\n\
                   CREATE VIEW app.v AS SELECT 1;\nCREATE INDEX ON users (id);";
        let found = created_at(sql);
        assert_eq!(found.get("public.users"), Some(&1));
        assert_eq!(found.get("app.v"), Some(&3));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_name_changed_in_place_is_a_rename() {
        let before = created_at("CREATE TABLE a (id int); CREATE TABLE users (id int);");
        let after = created_at("CREATE TABLE a (id int); CREATE TABLE members (id int);");
        assert_eq!(
            renames(&before, &after),
            [("public.users".to_owned(), "public.members".to_owned())]
        );
        // Dropped, or added elsewhere: not a rename.
        let dropped = created_at("CREATE TABLE a (id int);");
        assert_eq!(renames(&before, &dropped), []);
        let moved = created_at("CREATE TABLE members (id int); CREATE TABLE a (id int);");
        assert_eq!(renames(&before, &moved), []);
    }
}
