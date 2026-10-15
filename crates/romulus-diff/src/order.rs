//! Matching items by key, and telling a real move from a shift.
//!
//! Both are written so the result does not depend on which side is old: that
//! is what lets `diff(b, a)` be exactly `diff(a, b).reversed()`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Pairs items of both sides by key, sorted by key. A duplicate key keeps its
/// first item: Postgres never produces one, and guessing a pairing would be
/// worse than ignoring it.
pub(crate) fn by_key<'a, T, K: Ord>(
    old: &'a [T],
    new: &'a [T],
    key: impl Fn(&'a T) -> K,
) -> Vec<(K, Option<&'a T>, Option<&'a T>)> {
    let mut map: BTreeMap<K, (Option<&'a T>, Option<&'a T>)> = BTreeMap::new();
    for item in old {
        map.entry(key(item)).or_default().0.get_or_insert(item);
    }
    for item in new {
        map.entry(key(item)).or_default().1.get_or_insert(item);
    }
    map.into_iter().map(|(k, (b, a))| (k, b, a)).collect()
}

/// An item of an ordered list (columns, labels, attributes), matched by name.
pub(crate) struct Placed<'a, T> {
    pub name: &'a str,
    pub before: Option<(usize, &'a T)>,
    pub after: Option<(usize, &'a T)>,
    /// Present on both sides and its order relative to the others changed.
    pub moved: bool,
}

/// Matches two ordered lists by name. The result is sorted by smallest
/// position on either side, then largest, then name: close to the table's own
/// order, and the same whichever side is old.
pub(crate) fn placed<'a, T>(
    old: &'a [T],
    new: &'a [T],
    name: impl Fn(&'a T) -> &'a str,
) -> Vec<Placed<'a, T>> {
    let old = first_by_name(old, &name);
    let new = first_by_name(new, &name);
    let old_order: Vec<&str> = old.iter().map(|(n, _, _)| *n).collect();
    let new_order: Vec<&str> = new.iter().map(|(n, _, _)| *n).collect();
    let moved = moved(&old_order, &new_order);

    let mut items: BTreeMap<&str, Placed<'a, T>> = BTreeMap::new();
    for (n, position, item) in old {
        items
            .entry(n)
            .or_insert_with(|| Placed::new(n))
            .before
            .replace((position, item));
    }
    for (n, position, item) in new {
        items
            .entry(n)
            .or_insert_with(|| Placed::new(n))
            .after
            .replace((position, item));
    }
    let mut items: Vec<Placed<'a, T>> = items
        .into_values()
        .map(|mut p| {
            p.moved = moved.contains(p.name);
            p
        })
        .collect();
    items.sort_by_key(|p| {
        let positions = [p.before.map(|(i, _)| i), p.after.map(|(i, _)| i)];
        let lowest = positions.iter().flatten().min().copied();
        let highest = positions.iter().flatten().max().copied();
        (lowest, highest, p.name)
    });
    items
}

impl<'a, T> Placed<'a, T> {
    fn new(name: &'a str) -> Self {
        Self {
            name,
            before: None,
            after: None,
            moved: false,
        }
    }
}

/// Items with their position in the original list, duplicates after the first
/// dropped (see [`by_key`]).
fn first_by_name<'a, T>(
    items: &'a [T],
    name: &impl Fn(&'a T) -> &'a str,
) -> Vec<(&'a str, usize, &'a T)> {
    let mut seen = BTreeSet::new();
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| seen.insert(name(item)))
        .map(|(i, item)| (name(item), i, item))
        .collect()
}

/// Names on both sides whose order relative to the other shared names changed.
///
/// A name is unmoved when it belongs to every longest common subsequence of
/// the two orders. Comparing raw positions would call every column after an
/// insertion moved; picking one longest subsequence would make the answer
/// depend on tie-breaking, and so on which side is old. "In every one" is a
/// property of the set of subsequences, which is the same both ways: swapping
/// two columns marks both, moving one column to the front marks only it.
pub(crate) fn moved<'a>(old: &[&'a str], new: &[&'a str]) -> BTreeSet<&'a str> {
    let in_new: BTreeSet<&str> = new.iter().copied().collect();
    let shared: Vec<&'a str> = old.iter().copied().filter(|n| in_new.contains(n)).collect();
    let rank: HashMap<&str, usize> = shared.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    // The new order of the shared names, as old ranks: a longest common
    // subsequence is a longest increasing subsequence of this.
    let seq: Vec<usize> = new.iter().filter_map(|n| rank.get(n).copied()).collect();
    let len = seq.len();

    let mut ending = vec![1usize; len];
    for i in 0..len {
        for j in 0..i {
            if seq[j] < seq[i] {
                ending[i] = ending[i].max(ending[j] + 1);
            }
        }
    }
    let mut starting = vec![1usize; len];
    for i in (0..len).rev() {
        for j in i + 1..len {
            if seq[j] > seq[i] {
                starting[i] = starting[i].max(starting[j] + 1);
            }
        }
    }
    let longest = ending.iter().copied().max().unwrap_or(0);
    // Every longest subsequence takes exactly one element per `ending` level
    // from the elements on some longest subsequence; an element is on all of
    // them when it is alone at its level.
    let on_some: Vec<bool> = (0..len)
        .map(|i| ending[i] + starting[i] - 1 == longest)
        .collect();
    let mut per_level = vec![0usize; longest + 1];
    for i in (0..len).filter(|&i| on_some[i]) {
        per_level[ending[i]] += 1;
    }
    (0..len)
        .filter(|&i| !(on_some[i] && per_level[ending[i]] == 1))
        .map(|i| shared[seq[i]])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{by_key, moved, placed};

    fn moved_of(old: &[&'static str], new: &[&'static str]) -> Vec<&'static str> {
        moved(old, new).into_iter().collect()
    }

    #[test]
    fn same_order_moves_nothing() {
        assert!(moved_of(&["a", "b", "c"], &["a", "b", "c"]).is_empty());
    }

    #[test]
    fn insertions_and_removals_do_not_move_the_rest() {
        assert!(moved_of(&["a", "b", "c"], &["x", "a", "c", "y"]).is_empty());
    }

    #[test]
    fn moving_one_to_the_front_marks_only_it() {
        assert_eq!(
            moved_of(&["a", "b", "c", "d"], &["d", "a", "b", "c"]),
            ["d"]
        );
        assert_eq!(
            moved_of(&["d", "a", "b", "c"], &["a", "b", "c", "d"]),
            ["d"]
        );
    }

    #[test]
    fn a_swap_marks_both() {
        assert_eq!(moved_of(&["a", "b"], &["b", "a"]), ["a", "b"]);
        assert_eq!(moved_of(&["a", "b", "c"], &["a", "c", "b"]), ["b", "c"]);
    }

    #[test]
    fn moves_are_the_same_both_ways() {
        let old = ["a", "b", "c", "d", "e", "f"];
        let new = ["c", "a", "f", "b", "e", "d"];
        assert_eq!(moved(&old, &new), moved(&new, &old));
    }

    #[test]
    fn duplicate_keys_keep_the_first() {
        let old = [("a", 1), ("a", 2)];
        let new = [("a", 3)];
        let pairs = by_key(&old, &new, |(k, _)| *k);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1, Some(&("a", 1)));
    }

    #[test]
    fn placed_items_follow_table_order() {
        let old = ["id", "a", "b"];
        let new = ["id", "x", "a"];
        let order: Vec<&str> = placed(&old, &new, |s| *s).iter().map(|p| p.name).collect();
        assert_eq!(order, ["id", "x", "a", "b"]);
    }
}
