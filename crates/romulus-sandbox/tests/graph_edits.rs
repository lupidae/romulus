//! The SQL the web app's graph edits append to a draft is DDL the sandbox
//! takes: every statement kind, as snapshotted by
//! `web/src/features/schema/edits/edits.test.ts`, parses with Postgres's own
//! parser and passes the allowlist. No database needed.

use romulus_sandbox::{Action, ObjectKind, allowlist, catalog::Catalog, parse};

const SNAPSHOTS: &str =
    include_str!("../../../web/src/features/schema/edits/__snapshots__/edits.test.ts.snap");

/// `(case, statement)` for each `sqlFor` snapshot (the `… 1` entries; `… 2`
/// are descriptions).
fn statements() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in SNAPSHOTS.split("exports[`").skip(1) {
        let Some((name, rest)) = entry.split_once("`] = `") else {
            continue;
        };
        let Some(case) = name
            .strip_prefix("sqlFor > ")
            .and_then(|n| n.strip_suffix(" 1"))
        else {
            continue;
        };
        let value = rest.rsplit_once("`;").map_or(rest, |(v, _)| v).trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .expect("a quoted snapshot");
        let value = value
            .replace("\\`", "`")
            .replace("\\${", "${")
            .replace("\\\\", "\\");
        out.push((case.to_owned(), value));
    }
    out
}

#[test]
fn every_graph_edit_parses_and_is_allowed() {
    let all = statements();
    assert!(all.len() >= 25, "snapshots found: {}", all.len());
    let catalog = Catalog::default();
    for (case, sql) in &all {
        let parsed = parse(sql);
        assert!(parsed.errors.is_empty(), "{case}: {sql}\n{:?}", parsed.errors);
        assert_eq!(parsed.statements.len(), 1, "{case}: one statement: {sql}");
        let refused = allowlist::check(&parsed, sql, &catalog);
        assert!(refused.is_empty(), "{case}: {sql}\n{refused:?}");
    }
}

#[test]
fn graph_edits_are_keyed_by_the_object_they_touch() {
    let all = statements();
    let key = |case: &str| {
        let (_, sql) = all
            .iter()
            .find(|(c, _)| c == case)
            .unwrap_or_else(|| panic!("no snapshot {case}"));
        parse(sql).statements[0].key.clone()
    };
    let fk = key("add a foreign key");
    assert_eq!(fk.kind(), ObjectKind::ForeignKey);
    assert_eq!(fk.action(), Action::Create);
    assert_eq!(fk.to_string(), "fk:public.orders.orders_user_id_fkey");
    // Quoted names come back exactly, case and all.
    assert_eq!(
        key("add a foreign key that cascades").path(),
        ["shop", "Order", "Order_user_fkey"]
    );
    assert_eq!(key("add a table").path(), ["Sales", "order"]);
    assert_eq!(
        key("comment a column").to_string(),
        "comment:column:public.users.email"
    );
}
