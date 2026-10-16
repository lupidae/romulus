//! Temporary objects in migrations: a migration's scaffolding (a backfill
//! that copies rows into a temporary table, joins it, drops it), never
//! part of the schema. An import skips and reports them, and every later
//! statement of the same migration that only concerns them (an index on
//! one, `ALTER TABLE`, `DROP TABLE`, a comment, a trigger…); a draft or an
//! upload still refuses them (`allowlist`).
//!
//! Names are resolved as Postgres resolves them: `pg_temp` comes first in
//! the search path, so an unqualified name finds the temporary relation
//! even when a real table has the same name, until the temporary one is
//! dropped; a name qualified with another schema is the real one. A
//! migration's temporary objects are its own (each migration starts as a
//! fresh session would).
//!
//! Statements that build a lasting object from a temporary one are kept:
//! a view over one is temporary itself (Postgres makes it so), but a
//! `CREATE TABLE … AS SELECT` from one is a real table, which the replay
//! cannot build without its source (it fails, naming the skipped helper).

use std::collections::HashSet;

use pg_query::{
    NodeEnum,
    protobuf::{Node, ObjectType, RangeVar},
};
use serde_json::Value;

use crate::{names::strings, walk};

/// The temporary relation a statement creates, if it creates one: a
/// `TEMPORARY` (or `pg_temp.`) table, `CREATE TABLE … AS`, `SELECT … INTO`,
/// view or sequence. Its name, as an unqualified reference finds it.
pub(crate) fn created(node: &NodeEnum) -> Option<String> {
    use NodeEnum as N;
    let rel = match node {
        N::CreateStmt(s) => s.relation.as_ref(),
        N::CreateTableAsStmt(s) => s.into.as_ref().and_then(|i| i.rel.as_ref()),
        N::SelectStmt(s) => s.into_clause.as_ref().and_then(|i| i.rel.as_ref()),
        N::CreateSeqStmt(s) => s.sequence.as_ref(),
        N::ViewStmt(s) => s.view.as_ref(),
        _ => None,
    }?;
    is_temporary(rel).then(|| rel.relname.clone())
}

fn is_temporary(rel: &RangeVar) -> bool {
    rel.relpersistence == "t" || rel.schemaname == "pg_temp"
}

/// The temporary relations a migration created so far (tables, views,
/// sequences, and the indexes on them), by the name an unqualified
/// reference finds.
#[derive(Debug, Default)]
pub(crate) struct Temporaries(HashSet<String>);

impl Temporaries {
    /// Follows a statement of the migration: `true` when it creates a
    /// temporary object or only concerns ones created before (it is
    /// skipped). Keeps track of what it creates, renames and drops.
    pub(crate) fn follow(&mut self, node: &NodeEnum) -> bool {
        use NodeEnum as N;
        if let Some(name) = created(node) {
            self.0.insert(name);
            return true;
        }
        match node {
            // A view over a temporary relation is temporary (Postgres
            // says so, and makes it one).
            N::ViewStmt(s) if s.query.as_deref().is_some_and(|q| self.used_in(q)) => {
                if let Some(view) = &s.view {
                    self.0.insert(view.relname.clone());
                }
                true
            }
            N::IndexStmt(s) if self.targets(s.relation.as_ref()) => {
                if !s.idxname.is_empty() {
                    self.0.insert(s.idxname.clone());
                }
                true
            }
            N::AlterTableStmt(s) => self.targets(s.relation.as_ref()),
            N::AlterSeqStmt(s) => self.targets(s.sequence.as_ref()),
            N::CreateTrigStmt(s) => self.targets(s.relation.as_ref()),
            N::RuleStmt(s) => self.targets(s.relation.as_ref()),
            N::CreatePolicyStmt(s) => self.targets(s.table.as_ref()),
            N::AlterPolicyStmt(s) => self.targets(s.table.as_ref()),
            N::CreateStatsStmt(s) => {
                !s.relations.is_empty()
                    && s.relations.iter().all(|r| match r.node.as_ref() {
                        Some(N::RangeVar(r)) => self.targets(Some(r)),
                        _ => false,
                    })
            }
            N::RenameStmt(s) if self.targets(s.relation.as_ref()) => {
                if relation_kind(s.rename_type())
                    && let Some(rel) = &s.relation
                {
                    self.0.remove(&rel.relname);
                    self.0.insert(s.newname.clone());
                }
                true
            }
            N::DropStmt(s) if relation_kind(s.remove_type()) => {
                let names: Vec<Vec<String>> = s.objects.iter().map(list).collect();
                let temporary: Vec<bool> = names.iter().map(|n| self.named(n)).collect();
                // What a drop finds first is gone: an unqualified name
                // is the real table again afterwards.
                for (name, temporary) in names.iter().zip(&temporary) {
                    if *temporary && let Some(last) = name.last() {
                        self.0.remove(last);
                    }
                }
                !temporary.is_empty() && temporary.iter().all(|t| *t)
            }
            N::CommentStmt(s) => {
                let Some(object) = s.object.as_deref() else {
                    return false;
                };
                let name = list(object);
                match s.objtype() {
                    o if relation_kind(o) => self.named(&name),
                    ObjectType::ObjectColumn => {
                        name.split_last().is_some_and(|(_, rel)| self.named(rel))
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// A relation reference that finds a temporary relation.
    fn targets(&self, rel: Option<&RangeVar>) -> bool {
        rel.is_some_and(|r| {
            r.schemaname == "pg_temp" || (r.schemaname.is_empty() && self.0.contains(&r.relname))
        })
    }

    /// A name (`[schema.]name`) that finds a temporary relation.
    fn named(&self, name: &[String]) -> bool {
        match name {
            [name] => self.0.contains(name),
            [schema, _] => schema == "pg_temp",
            _ => false,
        }
    }

    /// A query reading a temporary relation.
    fn used_in(&self, query: &Node) -> bool {
        let Ok(value) = serde_json::to_value(query) else {
            return false;
        };
        let mut found = false;
        walk::visit_objects(&value, &mut |object| {
            let Some(Value::String(relname)) = object.get("relname") else {
                return;
            };
            let schema = object
                .get("schemaname")
                .and_then(Value::as_str)
                .unwrap_or("");
            found |= schema == "pg_temp" || (schema.is_empty() && self.0.contains(relname));
        });
        found
    }
}

/// Objects whose names live among relations (what `pg_temp` holds).
fn relation_kind(kind: ObjectType) -> bool {
    use ObjectType as O;
    matches!(
        kind,
        O::ObjectTable | O::ObjectView | O::ObjectSequence | O::ObjectIndex | O::ObjectMatview
    )
}

/// A `List` of names (`["pg_temp", "t"]`).
fn list(node: &Node) -> Vec<String> {
    match node.node.as_ref() {
        Some(NodeEnum::List(list)) => strings(&list.items),
        Some(NodeEnum::String(s)) => vec![s.sval.clone()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use crate::import::{SkipReason, analyze};

    /// What `sql`'s analysis skips: (line, reason).
    fn skipped(sql: &str) -> Vec<(u32, SkipReason)> {
        analyze(sql)
            .skipped
            .iter()
            .map(|s| (s.line, s.reason))
            .collect()
    }

    /// The pattern of a real migration: columns added, backfilled from a
    /// temporary table built from another, the table dropped, twice.
    #[test]
    fn a_backfill_through_a_temporary_table_is_skipped_around_the_ddl() {
        use SkipReason::{Data, Maintenance, Temporary};
        let sql = "\
ALTER TABLE share_links ADD COLUMN granted_at timestamptz;
CREATE TEMP TABLE link_first_seen AS
SELECT resource_type, resource_id, min(created_at) AS first_seen
FROM activity_log GROUP BY resource_type, resource_id;
CREATE INDEX ON link_first_seen (resource_type, resource_id);
ANALYZE link_first_seen;
UPDATE share_links l SET granted_at = d.first_seen
FROM link_first_seen d WHERE d.resource_id = l.id;
DROP TABLE link_first_seen;
CREATE TEMPORARY TABLE link_first_seen ON COMMIT DROP AS SELECT 1 AS id;
UPDATE share_links SET granted_at = now() FROM link_first_seen;
DROP TABLE IF EXISTS link_first_seen;
ALTER TABLE share_links ALTER COLUMN granted_at SET NOT NULL;
";
        assert_eq!(
            skipped(sql),
            [
                (2, Temporary),
                (5, Temporary),
                (6, Maintenance),
                (7, Data),
                (9, Temporary),
                (10, Temporary),
                (11, Data),
                (12, Temporary),
            ]
        );
        let kept = analyze(sql).sql;
        assert!(kept.contains("ADD COLUMN granted_at"));
        assert!(kept.contains("SET NOT NULL"));
        assert!(!kept.contains("link_first_seen"));
    }

    /// An unqualified name finds the temporary table first (`pg_temp` is
    /// searched first), until it is dropped; another schema's is real.
    #[test]
    fn a_temporary_table_shadows_a_real_one_until_dropped() {
        use SkipReason::Temporary;
        let sql = "\
CREATE TEMP TABLE users AS SELECT 1 AS id;
ALTER TABLE users ADD COLUMN x int;
ALTER TABLE public.users ADD COLUMN y int;
COMMENT ON TABLE public.users IS 'real';
DROP TABLE users;
ALTER TABLE users ADD COLUMN z int;
DROP TABLE users;
";
        assert_eq!(
            skipped(sql),
            [(1, Temporary), (2, Temporary), (5, Temporary)]
        );
        let kept = analyze(sql).sql;
        assert!(kept.contains("ALTER TABLE public.users ADD COLUMN y int"));
        assert!(kept.contains("ALTER TABLE users ADD COLUMN z int"));
        // The second drop is the real table's.
        assert_eq!(kept.matches("DROP TABLE users").count(), 1);
    }

    #[test]
    fn every_temporary_form_and_what_only_concerns_it_is_skipped() {
        let sql = "\
SELECT id INTO TEMP picked FROM users;
SELECT id INTO TEMPORARY TABLE picked_too FROM users;
CREATE TEMP SEQUENCE counter;
ALTER SEQUENCE counter RESTART;
CREATE TEMP VIEW recent AS SELECT * FROM users;
CREATE VIEW over_picked AS SELECT * FROM picked;
CREATE TABLE pg_temp.scratch (id int);
CREATE INDEX scratch_id ON scratch (id);
ALTER INDEX scratch_id RENAME TO scratch_by_id;
DROP INDEX scratch_by_id;
COMMENT ON TABLE scratch IS 'helper';
COMMENT ON COLUMN scratch.id IS 'helper';
CREATE TRIGGER t BEFORE INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION f();
ALTER TABLE scratch RENAME TO scratch2;
ALTER TABLE scratch2 ADD COLUMN x int;
DROP TABLE scratch2, picked, picked_too;
";
        let reasons: Vec<SkipReason> = skipped(sql).into_iter().map(|(_, r)| r).collect();
        assert_eq!(reasons, [SkipReason::Temporary; 16]);
        assert_eq!(analyze(sql).sql.trim(), "");
    }

    #[test]
    fn lasting_objects_stay_even_beside_temporary_ones() {
        let sql = "\
CREATE TEMP TABLE scratch (id int);
SELECT id INTO archived_users FROM users;
DROP TABLE scratch, users;
CREATE TABLE copy AS SELECT * FROM scratch;
";
        // A real table from SELECT INTO; a drop of a real and a temporary
        // table (it fails on replay, naming the helper); a real table
        // built from the temporary one (the same).
        assert_eq!(skipped(sql), [(1, SkipReason::Temporary)]);
    }

    /// A `DO` block creating a temporary table is not unwrapped: it is
    /// code, skipped as such.
    #[test]
    fn a_do_block_making_a_temporary_table_stays_code() {
        assert_eq!(
            skipped("DO $$ BEGIN CREATE TEMP TABLE t (id int); END $$;"),
            [(1, SkipReason::Code)]
        );
    }
}
