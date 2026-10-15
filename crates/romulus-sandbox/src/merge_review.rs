//! The cases of the review of #13 ("Update draft"), as tests.

use super::*;

fn m(ancestor: &str, ours: &str, theirs: &str) -> Merged {
    merge(&Input {
        ancestor,
        ours,
        theirs,
        base_model_sql: ancestor,
        theirs_model_sql: theirs,
        ours_model_sql: None,
        label: "v2",
    })
    .expect("parses")
}

const BASE: &str = "\
CREATE TABLE users (
    id bigint PRIMARY KEY,
    email text NOT NULL
);

CREATE TABLE orders (
    id bigint PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id)
);
";

#[test]
fn s01_graph_edit_new_table_and_fk_order() {
    // v2 was published from a draft whose graph edits created a table and an FK to it.
    let theirs = format!(
        "{BASE}\n-- edits from the graph\nCREATE TABLE public.plans (id bigint PRIMARY KEY);\nALTER TABLE public.orders ADD COLUMN plan_id bigint REFERENCES public.plans (id);\n"
    );
    let r = m(BASE, BASE, &theirs);
    let fk = r.text.find("REFERENCES public.plans").unwrap();
    let plans = r.text.find("CREATE TABLE public.plans").unwrap();
    assert!(plans < fk, "FK before its table");
}

#[test]
fn s02_double_update_with_stale_ancestor() {
    // Two Update clicks: the second one reads the old base/base_sql, ours is already merged.
    let ours = BASE.replace(
        "email text NOT NULL\n",
        "email text NOT NULL,\n    name text\n",
    );
    let theirs = format!("{BASE}\nALTER TABLE users ADD COLUMN plan text;\n");
    let first = m(BASE, &ours, &theirs);
    let again = m(BASE, &first.text, &theirs);
    assert_eq!(again.text, first.text);
}

#[test]
fn s03_partial_twice_stale() {
    let ours = format!("{BASE}\nALTER TABLE users ALTER COLUMN email SET DEFAULT '';\n");
    let theirs = BASE.replace(
        "email text NOT NULL\n",
        "email text NOT NULL,\n    plan text\n",
    );
    let first = m(BASE, &ours, &theirs);
    let again = m(BASE, &first.text, &theirs);
    assert_eq!(again.conflicts, 0);
}

#[test]
fn s04_user_typed_marker_like_text() {
    // A comment a user might write.
    let sql = "-- <<<<<<< yours truly, this is the users table\nCREATE TABLE a (id int);\n";
    assert_eq!(conflict_diagnostics(sql), []);
    let sql2 = "CREATE TABLE a (id int); -- ======= separator\n";
    assert_eq!(conflict_diagnostics(sql2), []);
    // marker inside a dollar-quoted body / string literal
    let sql3 =
        "COMMENT ON TABLE users IS '\n-- <<<<<<< yours\nx\n-- ======= v2\ny\n-- >>>>>>> v2\n';\n";
    let (b, s) = conflicts(sql3);
    assert_eq!(
        b,
        [],
        "marker inside a string literal is treated as a block"
    );
    assert_eq!(s, []);
}

#[test]
fn s05_multi_command_alter() {
    let ours = format!("{BASE}\nALTER TABLE users ADD COLUMN a text, ADD COLUMN b text;\n");
    let theirs = BASE.replace(
        "REFERENCES users (id)\n",
        "REFERENCES users (id),\n    total numeric\n",
    );
    let r = m(BASE, &ours, &theirs);
    assert_eq!(r.conflicts, 0);
    // both edit users differently via multi-command alters
    let theirs2 =
        format!("{BASE}\nALTER TABLE users ADD COLUMN c text, ALTER COLUMN email DROP NOT NULL;\n");
    let r = m(BASE, &ours, &theirs2);
    // Different columns: both statements stay, theirs' after users.
    assert_eq!(r.conflicts, 0);
    let theirs_at = r.text.find("ADD COLUMN c text").unwrap();
    assert!(theirs_at < r.text.find("CREATE TABLE orders").unwrap());
    assert!(r.text.contains("ADD COLUMN a text, ADD COLUMN b text;"));
}

#[test]
fn s06_create_schema_with_elements() {
    let base = "CREATE SCHEMA app CREATE TABLE app.t (id int);\n";
    let theirs = "CREATE SCHEMA app CREATE TABLE app.t (id int, x int);\n";
    let r = m(base, base, theirs);
    assert_eq!(r.text, theirs);
}

#[test]
fn s07_rename_by_graph_and_theirs_edits_old_name() {
    // ours renamed users -> members (graph edit); theirs added a column to users.
    let ours =
        format!("{BASE}\n-- edits from the graph\nALTER TABLE public.users RENAME TO members;\n");
    let theirs = format!("{BASE}\nALTER TABLE users ADD COLUMN plan text;\n");
    let r = m(BASE, &ours, &theirs);
    // theirs' ALTER on users must come before the rename, else it targets a missing table.
    let rename = r.text.find("RENAME TO members").unwrap();
    let add = r.text.find("plan text").unwrap();
    assert!(add < rename, "theirs' change lands after the rename");
}

#[test]
fn s08_theirs_renames_ours_edits() {
    let ours = BASE.replace(
        "email text NOT NULL\n",
        "email text NOT NULL,\n    name text\n",
    );
    let theirs = format!("{BASE}\nALTER TABLE users RENAME TO members;\n");
    let r = m(BASE, &ours, &theirs);
    // These texts stand for their models, so theirs' rename is a statement
    // of its own (a real model would show `members` instead of `users`:
    // a dropped table ours edited, a conflict).
    assert_eq!(r.conflicts, 0);
    assert!(r.text.contains("name text") && r.text.contains("RENAME TO members"));
}

#[test]
fn s09_enum_both_add_same_label_different_place() {
    let base = "CREATE TYPE mood AS ENUM ('ok', 'sad');\n";
    let ours = "CREATE TYPE mood AS ENUM ('ok', 'sad', 'happy');\n";
    let theirs = "CREATE TYPE mood AS ENUM ('happy', 'ok', 'sad');\n";
    let r = m(base, ours, theirs);
    // The same label, added at different places: one label, where ours put
    // it, and nothing to resolve.
    assert_eq!(r.conflicts, 0);
    assert_eq!(r.text, ours);
    assert_eq!(r.ancestor, theirs);
    let ours = "CREATE TYPE mood AS ENUM ('ok', 'sad');\nALTER TYPE mood ADD VALUE 'x';\n";
    let theirs = "CREATE TYPE mood AS ENUM ('ok', 'sad');\nALTER TYPE mood ADD VALUE 'y';\n";
    let r = m(base, ours, theirs);
    assert_eq!(r.conflicts, 0);
    assert!(r.text.contains("ADD VALUE 'x'") && r.text.contains("ADD VALUE 'y'"));
}

#[test]
fn s10_index_added_by_both_differently() {
    let ours = format!("{BASE}\nCREATE INDEX orders_user_idx ON orders (user_id);\n");
    let theirs = format!("{BASE}\nCREATE INDEX orders_user_idx ON orders (user_id, id);\n");
    let r = m(BASE, &ours, &theirs);
    assert_eq!(r.conflicts, 1);
}

#[test]
fn s11_ours_drop_column_theirs_adds_fk_on_it() {
    let ours = format!("{BASE}\nALTER TABLE orders DROP COLUMN user_id;\n");
    let theirs = BASE.replace(
        "user_id bigint NOT NULL REFERENCES users (id)",
        "user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE",
    );
    let r = m(BASE, &ours, &theirs);
    assert_eq!(r.conflicts, 1);
}

#[test]
fn s12_ours_comment_on_column_theirs_drops_table() {
    let ours = format!("{BASE}\nCOMMENT ON COLUMN orders.user_id IS 'buyer';\n");
    let theirs = "CREATE TABLE users (\n    id bigint PRIMARY KEY,\n    email text NOT NULL\n);\n";
    let r = m(BASE, &ours, theirs);
    assert_eq!(r.conflicts, 1, "comment on a table theirs dropped");
}

#[test]
fn s13_pushed_theirs_unchanged_no_spurious() {
    // ancestor hand-written; theirs = render_sql of the same model; only orders gets a column.
    let ancestor = "CREATE TABLE users (id bigint PRIMARY KEY, email text NOT NULL);\nCREATE TABLE orders (id bigint PRIMARY KEY, user_id bigint REFERENCES users(id));\nCREATE INDEX ON orders (user_id);\n";
    let base_model = "CREATE TABLE public.users (\n    id bigint NOT NULL,\n    email text NOT NULL,\n    CONSTRAINT users_pkey PRIMARY KEY (id)\n);\n\nCREATE TABLE public.orders (\n    id bigint NOT NULL,\n    user_id bigint,\n    CONSTRAINT orders_pkey PRIMARY KEY (id)\n);\n\nALTER TABLE public.orders ADD CONSTRAINT orders_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id);\n\nCREATE INDEX orders_user_id_idx ON public.orders USING btree (user_id);\n";
    let theirs = base_model.replace(
        "    user_id bigint,\n",
        "    user_id bigint,\n    note text,\n",
    );
    let ours = ancestor.replace("email text NOT NULL)", "email text NOT NULL, name text)");
    let r = merge(&Input {
        ancestor,
        ours: &ours,
        theirs: &theirs,
        base_model_sql: base_model,
        theirs_model_sql: &theirs,
        ours_model_sql: None,
        label: "v2",
    })
    .unwrap();
    assert_eq!(r.conflicts, 0);
    assert!(r.text.contains("CREATE INDEX ON orders (user_id);"));
}

#[test]
fn s14_both_change_fk_inline_vs_alter() {
    // ours made the inline FK ON DELETE CASCADE, theirs (pushed) changed the column type
    let ancestor = BASE;
    let ours = BASE.replace(
        "REFERENCES users (id)\n",
        "REFERENCES users (id) ON DELETE CASCADE\n",
    );
    let theirs = BASE.replace("user_id bigint NOT NULL", "user_id integer NOT NULL");
    let r = m(ancestor, &ours, &theirs);
    // One statement, changed by both.
    assert_eq!(r.conflicts, 1);
}

#[test]
fn s15_reapply_blocks_three_times_stable() {
    let ours = BASE.replace(
        "email text NOT NULL\n",
        "email text NOT NULL,\n    name text\n",
    );
    let v2 = BASE.replace(
        "email text NOT NULL\n",
        "email text NOT NULL,\n    name varchar(40)\n",
    );
    let first = m(BASE, &ours, &v2);
    // user resolves by editing inside block but leaves markers; v3 unrelated
    let again = merge(&Input {
        ancestor: &first.ancestor,
        ours: &first.text,
        theirs: &v2,
        base_model_sql: &v2,
        theirs_model_sql: &v2,
        ours_model_sql: None,
        label: "v2",
    })
    .unwrap();
    assert_eq!(again.text, first.text);
}

#[test]
fn s16_whole_file_marker_in_dollar_function() {
    let sql = "CREATE TABLE a (id int);\nCREATE VIEW v AS SELECT $$\n-- >>>>>>> v2\n$$ AS x;\n";
    let d = conflict_diagnostics(sql);
    assert!(d.is_empty(), "marker-like line inside a literal refused");
}

#[test]
fn s17_banner_comment_refused() {
    let sql = "-- ==========================================\n-- Users\n-- ==========================================\nCREATE TABLE users (id int);\n";
    let d = conflict_diagnostics(sql);
    assert!(
        d.is_empty(),
        "a banner comment is refused as a stray conflict marker"
    );
}

#[test]
fn s18_theirs_drops_table_ours_alter_adds_column() {
    let ours = format!("{BASE}\nALTER TABLE orders ADD COLUMN note text;\n");
    let theirs = "CREATE TABLE users (\n    id bigint PRIMARY KEY,\n    email text NOT NULL\n);\n";
    let r = m(BASE, &ours, theirs);
    assert_eq!(r.conflicts, 1, "ALTER on a table theirs dropped");
}

#[test]
fn s19_ours_drops_table_theirs_adds_index() {
    let ours = "CREATE TABLE users (\n    id bigint PRIMARY KEY,\n    email text NOT NULL\n);\n";
    let theirs = format!("{BASE}\nCREATE INDEX orders_user_idx ON orders (user_id);\n");
    let r = m(BASE, ours, &theirs);
    assert_eq!(r.conflicts, 1, "index on a table ours dropped");
}

/// Every pairing of a few usual changes (drops, appended statements, edits
/// in place, new objects) merges without the merge's own edits colliding,
/// and its edits give its text.
#[test]
fn every_pairing_of_usual_changes_merges_cleanly_or_conflicts() {
    let base = format!(
        "{BASE}\nCREATE INDEX orders_user_idx ON orders (user_id);\nCOMMENT ON TABLE orders IS 'Checkouts';\n"
    );
    let variants: Vec<(&str, String)> = vec![
        ("same", base.clone()),
        ("drop orders", base.replace("CREATE TABLE orders (\n    id bigint PRIMARY KEY,\n    user_id bigint NOT NULL REFERENCES users (id)\n);\n", "")
            .replace("CREATE INDEX orders_user_idx ON orders (user_id);\n", "")
            .replace("COMMENT ON TABLE orders IS 'Checkouts';\n", "")),
        ("drop statement", format!("{base}\nDROP TABLE orders;\n")),
        ("add column", format!("{base}\n-- edits from the graph\nALTER TABLE public.orders ADD COLUMN note text;\n")),
        ("comment column", format!("{base}\nCOMMENT ON COLUMN orders.user_id IS 'buyer';\n")),
        ("edit users", base.replace("email text NOT NULL\n", "email text NOT NULL,\n    name text\n")),
        ("delete users statement", base.replace("CREATE TABLE users (\n    id bigint PRIMARY KEY,\n    email text NOT NULL\n);\n", "")),
        ("new table", format!("{base}\nCREATE TABLE coupons (code text PRIMARY KEY);\n")),
        ("index change", base.replace("ON orders (user_id);", "ON orders (user_id, id);")),
        ("rename", format!("{base}\n-- edits from the graph\nALTER TABLE public.users RENAME TO members;\n")),
    ];
    for (ours_name, ours) in &variants {
        for (theirs_name, theirs) in &variants {
            let merged = merge(&Input {
                ancestor: &base,
                ours,
                theirs,
                base_model_sql: &base,
                theirs_model_sql: theirs,
                ours_model_sql: None,
                label: "v2",
            })
            .unwrap_or_else(|err| panic!("ours {ours_name}, theirs {theirs_name}: {err:?}"));
            let mut text = ours.clone();
            for edit in merged.edits.iter().rev() {
                text.replace_range(edit.range.clone(), &edit.text);
            }
            assert_eq!(text, merged.text, "ours {ours_name}, theirs {theirs_name}");
        }
    }
}

/// One spec for conflict blocks, here and in the web app
/// (`web/src/features/draft/conflicts.ts` runs the same vectors): exact
/// marker lines only, between statements only, never in a string, a quoted
/// name, a dollar-quoted body or a comment.
#[test]
fn conflict_markers_read_as_the_shared_vectors_say() {
    let vectors: serde_json::Value = serde_json::from_str(include_str!(
        "../../../web/src/features/draft/conflict-markers.json"
    ))
    .expect("json");
    for vector in vectors.as_array().expect("a list") {
        let name = vector["name"].as_str().expect("name");
        let sql = vector["sql"].as_str().expect("sql");
        let (blocks, stray) = conflicts(sql);
        let blocks: Vec<serde_json::Value> = blocks
            .iter()
            .map(|b| {
                serde_json::json!({
                    "yours": &sql[b.yours.clone()],
                    "theirs": &sql[b.theirs.clone()],
                    "label": b.label,
                    "both": b.both,
                })
            })
            .collect();
        let stray: Vec<&str> = stray.iter().map(|r| &sql[r.clone()]).collect();
        assert_eq!(
            blocks,
            vector["blocks"].as_array().unwrap().clone(),
            "{name}"
        );
        assert_eq!(serde_json::json!(stray), vector["stray"], "{name}");
        // A text whose markers are all stray or none: diagnostics agree.
        assert_eq!(
            conflict_diagnostics(sql).len(),
            blocks.len() + stray.len(),
            "{name}"
        );
    }
}
