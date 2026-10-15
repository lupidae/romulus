//! The smallest edit that turns a draft's text into another, applied to its
//! `Y.Text` in one transaction: what the server writes when it rewrites the
//! SQL (Tidy; the three-way merge of "Update draft" can use it too).
//!
//! Never delete-all then insert-all: that moves every collaborator's caret
//! to an end of the text and turns everything they type meanwhile into a
//! conflict with the whole file. Only the changed ranges are touched, so a
//! caret in text that stays keeps its place (Yjs positions are relative to
//! the characters around them), and concurrent typing elsewhere merges.
//!
//! Lines are diffed first (patience: statements that moved stay whole), then
//! each changed run of lines character by character, so `users` becoming
//! `public.users` inserts `public.` rather than replacing the line. Both
//! diffs have a deadline: past it the result is coarser, never wrong.

use std::{
    ops::Range,
    time::{Duration, Instant},
};

use similar::{Algorithm, DiffTag, TextDiff};
use yrs::{Text, TextRef, TransactionMut};

/// A run of changed lines longer than this (in bytes) is replaced whole
/// rather than diffed character by character.
const REFINE_LIMIT: usize = 20_000;

/// Each of the two diffs gives up refining after this long.
const DEADLINE: Duration = Duration::from_millis(250);

/// One change to the old text: replace `delete` with `insert`. Ranges are in
/// bytes of the old text, in order, never overlapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub delete: Range<usize>,
    pub insert: String,
}

/// The changes that turn `old` into `new`.
pub fn changes(old: &str, new: &str) -> Vec<Change> {
    if old == new {
        return Vec::new();
    }
    let deadline = Instant::now() + DEADLINE;
    let lines = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .deadline(deadline)
        .diff_lines(old, new);
    let old_starts = starts(lines.old_slices());
    let new_starts = starts(lines.new_slices());
    let mut out = Vec::new();
    // Consecutive non-equal ops form one changed run.
    let mut run: Option<(Range<usize>, Range<usize>)> = None;
    let flush = |run: &mut Option<(Range<usize>, Range<usize>)>, out: &mut Vec<Change>| {
        if let Some((o, n)) = run.take() {
            let o = old_starts[o.start]..old_starts[o.end];
            let n = new_starts[n.start]..new_starts[n.end];
            refine(old, new, o, n, deadline, out);
        }
    };
    for op in lines.ops() {
        let (tag, o, n) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            flush(&mut run, &mut out);
            continue;
        }
        run = Some(match run {
            Some((ro, rn)) => (ro.start..o.end, rn.start..n.end),
            None => (o, n),
        });
    }
    flush(&mut run, &mut out);
    out
}

/// Byte offset of each slice, plus the end.
fn starts(slices: &[&str]) -> Vec<usize> {
    let mut out = Vec::with_capacity(slices.len() + 1);
    let mut at = 0;
    out.push(0);
    for slice in slices {
        at += slice.len();
        out.push(at);
    }
    out
}

/// The changes within one changed run of lines, by character.
fn refine(
    old: &str,
    new: &str,
    o: Range<usize>,
    n: Range<usize>,
    deadline: Instant,
    out: &mut Vec<Change>,
) {
    let (old_part, new_part) = (&old[o.clone()], &new[n.clone()]);
    if old_part.len() + new_part.len() > REFINE_LIMIT {
        out.push(Change {
            delete: o,
            insert: new_part.to_owned(),
        });
        return;
    }
    let chars = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .deadline(deadline)
        .diff_chars(old_part, new_part);
    let old_starts = starts(chars.old_slices());
    let new_starts = starts(chars.new_slices());
    let mut pending: Option<Change> = None;
    for op in chars.ops() {
        let (tag, co, cn) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            out.extend(pending.take());
            continue;
        }
        let delete = o.start + old_starts[co.start]..o.start + old_starts[co.end];
        let insert = &new_part[new_starts[cn.start]..new_starts[cn.end]];
        match &mut pending {
            Some(change) if change.delete.end == delete.start => {
                change.delete.end = delete.end;
                change.insert.push_str(insert);
            }
            _ => {
                out.extend(pending.take());
                pending = Some(Change {
                    delete,
                    insert: insert.to_owned(),
                });
            }
        }
    }
    out.extend(pending);
}

/// UTF-16 length of a string: Y.Text offsets in a doc made by
/// `doc::new_doc`.
fn utf16_len(text: &str) -> u32 {
    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

/// Applies `changes` (computed against `old`, the text's content now) to
/// the text, last first so earlier offsets stay valid. Whether anything
/// changed.
pub fn apply(text: &TextRef, txn: &mut TransactionMut, old: &str, changes: &[Change]) -> bool {
    for change in changes.iter().rev() {
        let at = utf16_len(&old[..change.delete.start]);
        let len = utf16_len(&old[change.delete.clone()]);
        if len > 0 {
            text.remove_range(txn, at, len);
        }
        if !change.insert.is_empty() {
            text.insert(txn, at, &change.insert);
        }
    }
    !changes.is_empty()
}

#[cfg(test)]
mod tests {
    use yrs::{
        Assoc, GetString, IndexedSequence, ReadTxn, StateVector, Transact, Update,
        updates::decoder::Decode,
    };

    use super::*;
    use crate::collab::doc::{Roots, new_doc};

    /// `old` with `changes` applied, the plain way.
    fn patched(old: &str, changes: &[Change]) -> String {
        let mut out = String::new();
        let mut at = 0;
        for change in changes {
            out.push_str(&old[at..change.delete.start]);
            out.push_str(&change.insert);
            at = change.delete.end;
        }
        out.push_str(&old[at..]);
        out
    }

    fn round_trip(old: &str, new: &str) -> Vec<Change> {
        let changes = changes(old, new);
        assert_eq!(patched(old, &changes), new, "{changes:?}");
        // And through a Y.Text.
        let doc = new_doc();
        let roots = Roots::of(&doc);
        roots.sql.insert(&mut doc.transact_mut(), 0, old);
        apply(&roots.sql, &mut doc.transact_mut(), old, &changes);
        assert_eq!(roots.sql.get_string(&doc.transact()), new);
        changes
    }

    #[test]
    fn the_same_text_changes_nothing() {
        assert_eq!(round_trip("a;\nb;\n", "a;\nb;\n"), []);
    }

    #[test]
    fn a_word_inserted_is_an_insert_of_that_word() {
        let changes = round_trip(
            "CREATE TABLE users (id int);\nCREATE TABLE orders (id int);\n",
            "CREATE TABLE public.users (id int);\nCREATE TABLE orders (id int);\n",
        );
        assert_eq!(
            changes,
            [Change {
                delete: 13..13,
                insert: "public.".into()
            }]
        );
    }

    #[test]
    fn statements_appended_at_the_end_are_folded_in() {
        let old = "CREATE TABLE users (\n    id int,\n    email text\n);\n\n-- edits from the graph\nALTER TABLE public.users RENAME COLUMN email TO mail;\n";
        let new = "CREATE TABLE public.users (\n    id integer,\n    mail text\n);\n";
        let changes = round_trip(old, new);
        // Nothing replaced whole: the unchanged lines are untouched.
        assert!(
            changes
                .iter()
                .all(|c| !old[c.delete.clone()].contains("id int,"))
        );
    }

    #[test]
    fn non_ascii_text_lands_where_it_should() {
        round_trip(
            "COMMENT ON TABLE t IS 'café ☕';\nCREATE TABLE ü (x int);\n",
            "COMMENT ON TABLE public.t IS 'café ☕ 🎉';\nCREATE TABLE public.ü (x integer);\n",
        );
        round_trip("😀\n", "");
        round_trip("", "😀 é\n");
    }

    #[test]
    fn large_texts_still_patch_exactly() {
        let old = (0..3000)
            .map(|i| format!("CREATE TABLE t{i} (id int);\n"))
            .collect::<Vec<_>>()
            .concat();
        let new = (0..3000)
            .rev()
            .map(|i| format!("CREATE TABLE public.t{i} (id integer);\n"))
            .collect::<Vec<_>>()
            .concat();
        round_trip(&old, &new);
    }

    #[test]
    fn a_caret_in_untouched_text_keeps_its_place_and_concurrent_typing_merges() {
        let old = "CREATE TABLE a (id int);\n\nCREATE TABLE b (id int);\n";
        let new = "CREATE TABLE public.a (id integer);\n\nCREATE TABLE public.b (id integer);\n";
        // Two replicas of one draft.
        let server = new_doc();
        let roots = Roots::of(&server);
        roots.sql.insert(&mut server.transact_mut(), 0, old);
        let client = new_doc();
        let client_roots = Roots::of(&client);
        let state = server
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        client
            .transact_mut()
            .apply_update(Update::decode_v1(&state).unwrap())
            .unwrap();
        // The client's caret sits before "(id int)" of b…
        let caret_at = u32::try_from(old.rfind("(id").unwrap()).unwrap();
        let caret = client_roots
            .sql
            .sticky_index(&client.transact_mut(), caret_at, Assoc::After)
            .unwrap();
        // …and the client types in a's line while the server rewrites.
        client_roots
            .sql
            .insert(&mut client.transact_mut(), 15, " /* x */");
        let before = server.transact().state_vector();
        {
            let mut txn = server.transact_mut();
            apply(&roots.sql, &mut txn, old, &changes(old, new));
        }
        let rewrite = server.transact().encode_state_as_update_v1(&before);
        client
            .transact_mut()
            .apply_update(Update::decode_v1(&rewrite).unwrap())
            .unwrap();
        let text = client_roots.sql.get_string(&client.transact());
        assert!(text.contains("/* x */"), "{text}");
        assert!(
            text.contains("CREATE TABLE public.b (id integer);"),
            "{text}"
        );
        let txn = client.transact();
        let offset = caret.get_offset(&txn).unwrap().index as usize;
        assert!(
            text[offset..].starts_with("(id integer);"),
            "{text} at {offset}"
        );
    }
}
