//! "Update draft": the three-way merge of a draft's SQL with a version
//! published since the draft started (docs/MVP.md "Drafts and versions: the
//! UX").
//!
//! **Statement-level, not line-level.** The newer version may come from a
//! remus push, whose SQL `render_sql` writes in its own style; a line diff3
//! would conflict on the whole file. Here every statement of the three texts
//! is linked to the objects it defines or touches, its **atoms**: a table and
//! each of its columns, each constraint and foreign key (inline ones too,
//! named as Postgres names them), an index, a comment, an enum and each of
//! its labels, a view… The texts are:
//!
//! - the **ancestor**: what the draft's text is relative to (the base
//!   version's text, then what the last update left: [`Merged::ancestor`]);
//! - **ours**: the draft now;
//! - **theirs**: the newer version's text (its source, or `render_sql` of a
//!   pushed model).
//!
//! Statements that share an atom, and an ours statement and the ancestor
//! statement it comes from (same key, same occurrence), form a
//! **component**, merged as a whole:
//!
//! - *theirs changed it* when one of its atoms is defined differently in
//!   the base version's model and the newer one's, both rendered by
//!   `render_sql` and deparsed: formatting never counts, so a pushed version
//!   changes only what it really changes;
//! - *ours changed it* when an ours statement is new or differs from the
//!   ancestor's (deparsed: whitespace and case inside a statement do not
//!   count), or an ancestor statement is gone.
//!
//! | theirs | ours | result |
//! |---|---|---|
//! | unchanged | anything | ours, untouched: text, formatting, `--` comments |
//! | changed | unchanged | theirs' statements, where ours' first one was |
//! | changed | changed the same way | ours |
//! | changed | changed other atoms, defining nothing theirs does | theirs' statements, and ours' changed ones where they are |
//! | changed: enum labels added | changed: enum labels added | ours, then `ADD VALUE` for theirs' labels |
//! | changed | changed the same atoms | a conflict block |
//!
//! New objects of theirs go after what precedes them in theirs. A
//! statement that renames something (`… RENAME …`, `SET SCHEMA`) stands
//! alone: appended by a graph edit, it stays ours whatever theirs did to the
//! object. The graph edits' marker line ([`GRAPH_EDITS_MARKER`]) stays where
//! it is whatever happens to the statements under it. A `DROP` touches what it drops (and a dropped table, everything
//! on it): dropping what theirs changed is a conflict.
//!
//! # Conflict blocks
//!
//! ```sql
//! -- <<<<<<< yours
//! CREATE TABLE audit (id bigint, note text);
//! -- ======= v3
//! CREATE TABLE public.audit (id bigint, payload jsonb);
//! -- >>>>>>> v3
//! ```
//!
//! The markers are SQL comments, so a text with blocks still parses: a later
//! update (a newer version out while blocks remain) finds their statements,
//! and rewrites a block's theirs side when the newer version changed that
//! object again. It never compiles: [`conflict_diagnostics`] refuses it.
//! `(both can be kept)` on the first marker says that keeping both sides
//! (theirs, then yours) defines nothing twice.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt::Write as _,
    ops::Range,
};

use pg_query::{
    NodeEnum,
    protobuf::{ColumnDef, ConstrType, Constraint, RangeVar, Token as TokenKind},
};

use crate::{
    diagnostic::{Code, Diagnostic},
    key::{Action, Key, ObjectKind},
    names::{QualifiedName, quote_ident},
    parse::{ParsedSql, Statement, parse},
    source::LineIndex,
};

/// The first marker of a block, without the `(both can be kept)` note.
pub const YOURS_MARKER: &str = "-- <<<<<<< yours";
/// The note on the first marker when keeping both sides defines nothing twice.
pub const BOTH_NOTE: &str = "(both can be kept)";
/// The line graph edits write above what they append (the web app's
/// `features/schema/edits/append.ts`): appended statements merge by their
/// own keys like any other, the marker stays put.
pub const GRAPH_EDITS_MARKER: &str = "-- edits from the graph";
const MIDDLE: &str = "-- =======";
const END: &str = "-- >>>>>>>";

/// A conflict block in a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// From the first marker line to the end of the last (its line break
    /// excluded).
    pub range: Range<usize>,
    /// Between the first two markers, without the line break before the
    /// second.
    pub yours: Range<usize>,
    /// Between the last two markers.
    pub theirs: Range<usize>,
    /// How the markers name theirs: `v3`.
    pub label: String,
    /// Keeping both sides (theirs, then yours) defines nothing twice.
    pub both: bool,
}

/// A marker line of a conflict block, as the merge writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Marker {
    /// `-- <<<<<<< yours`, with whether `(both can be kept)` follows.
    Yours(bool),
    /// `-- ======= vN`.
    Theirs(String),
    /// `-- >>>>>>> vN`.
    End(String),
}

impl Marker {
    /// Only the exact lines the merge writes: a banner of `=` or a comment
    /// that happens to start like a marker is not one.
    fn read(line: &str) -> Option<Self> {
        let line = line.trim();
        let version = |label: &str| {
            let label = label.strip_prefix(' ')?;
            (label.len() > 1
                && label.starts_with('v')
                && label[1..].bytes().all(|b| b.is_ascii_digit()))
            .then(|| label.to_owned())
        };
        if line == YOURS_MARKER {
            Some(Self::Yours(false))
        } else if line.strip_prefix(YOURS_MARKER) == Some(&format!(" {BOTH_NOTE}")) {
            Some(Self::Yours(true))
        } else if let Some(label) = line.strip_prefix(MIDDLE) {
            version(label).map(Self::Theirs)
        } else {
            line.strip_prefix(END).and_then(version).map(Self::End)
        }
    }
}

/// The conflict blocks of a text, and the marker lines that belong to none
/// (a block whose other markers were deleted). See [`conflicts_in`].
pub fn conflicts(sql: &str) -> (Vec<Conflict>, Vec<Range<usize>>) {
    if !(sql.contains(YOURS_MARKER) || sql.contains(MIDDLE) || sql.contains(END)) {
        return (Vec::new(), Vec::new());
    }
    conflicts_in(sql, &parse(sql))
}

/// The conflict blocks of a parsed text. A marker is a `--` comment that
/// starts its line, outside every statement (so never in a string, a
/// dollar-quoted body or a statement's own comments), reading exactly as
/// the merge writes it ([`Marker::read`]); a block is yours, theirs and
/// end markers in that order, the last two with one label. Markers out of
/// that order are stray.
pub(crate) fn conflicts_in(sql: &str, parsed: &ParsedSql) -> (Vec<Conflict>, Vec<Range<usize>>) {
    let mut markers: Vec<(Line, Marker)> = Vec::new();
    for token in &parsed.tokens {
        if token.kind != TokenKind::SqlComment {
            continue;
        }
        let line_start = sql[..token.start].rfind('\n').map_or(0, |n| n + 1);
        let alone = sql[line_start..token.start].trim().is_empty();
        let inside = parsed
            .statements
            .iter()
            .any(|s| s.span.start < token.start && token.start < s.span.end);
        if !alone || inside {
            continue;
        }
        let end = sql[token.start..]
            .find('\n')
            .map_or(sql.len(), |n| token.start + n);
        if let Some(marker) = Marker::read(&sql[token.start..end]) {
            let line = Line {
                text: line_start..end,
                next: (end + 1).min(sql.len()),
            };
            markers.push((line, marker));
        }
    }

    let mut blocks = Vec::new();
    let mut stray = Vec::new();
    let mut open: Vec<(Line, Marker)> = Vec::new();
    for (line, marker) in markers {
        let complete = match (&marker, open.as_slice()) {
            (Marker::Yours(_), _) => {
                stray.extend(open.drain(..).map(|(l, _)| l.text));
                open.push((line, marker));
                continue;
            }
            (Marker::Theirs(_), [(_, Marker::Yours(_))]) => {
                open.push((line, marker));
                continue;
            }
            (Marker::End(label), [(start, Marker::Yours(both)), (middle, Marker::Theirs(t))])
                if t == label =>
            {
                let content = |from: usize, to: usize| {
                    // Without the line break before the next marker.
                    let to = if to > from && sql.as_bytes()[to - 1] == b'\n' {
                        to - 1
                    } else {
                        to
                    };
                    from..to.max(from)
                };
                Some(Conflict {
                    range: start.text.start..line.text.end,
                    yours: content(start.next, middle.text.start),
                    theirs: content(middle.next, line.text.start),
                    label: label.clone(),
                    both: *both,
                })
            }
            _ => None,
        };
        if let Some(block) = complete {
            open.clear();
            blocks.push(block);
        } else {
            stray.extend(open.drain(..).map(|(l, _)| l.text));
            stray.push(line.text);
        }
    }
    stray.extend(open.into_iter().map(|(l, _)| l.text));
    stray.sort_by_key(|r| r.start);
    (blocks, stray)
}

/// Why a draft's text with conflict blocks (or stray markers) does not
/// compile: one error per block, on its first marker, and one per stray
/// marker. For drafts only: an uploaded or pushed text is never refused
/// for what looks like a marker.
pub fn conflict_diagnostics(sql: &str) -> Vec<Diagnostic> {
    let (blocks, stray) = conflicts(sql);
    if blocks.is_empty() && stray.is_empty() {
        return Vec::new();
    }
    let lines = LineIndex::new(sql);
    let mut out: Vec<Diagnostic> = blocks
        .iter()
        .map(|block| {
            let first = block.range.start
                ..sql[block.range.clone()]
                    .find('\n')
                    .map_or(block.range.end, |n| block.range.start + n);
            Diagnostic::error(
                Code::Conflict,
                format!("Conflict with {}: keep yours or theirs.", block.label),
            )
            .with_span(lines.span(sql, first))
            .with_hint("Choose on the block, or edit it and delete its three marker lines.")
        })
        .chain(stray.into_iter().map(|range| {
            Diagnostic::error(
                Code::Conflict,
                "A conflict marker outside a conflict block.",
            )
            .with_span(lines.span(sql, range))
            .with_hint("Delete this line, or restore the block's other markers.")
        }))
        .collect();
    out.sort_by_key(|d| d.span.map_or(0, |s| s.start));
    out
}

/// One line of a text: its range without the line break, and where the
/// next one starts.
#[derive(Debug, Clone)]
struct Line {
    text: Range<usize>,
    next: usize,
}

/// What [`merge`] works on. See the module documentation.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    pub ancestor: &'a str,
    pub ours: &'a str,
    pub theirs: &'a str,
    /// `render_sql` of the base version's model (empty without one).
    pub base_model_sql: &'a str,
    /// `render_sql` of the newer version's model.
    pub theirs_model_sql: &'a str,
    /// `render_sql` of the draft's model, when its text compiles: two sides
    /// that made the same change, written differently, then agree.
    pub ours_model_sql: Option<&'a str>,
    /// How conflict markers name theirs: `v3`.
    pub label: &'a str,
}

/// The outcome of [`merge`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    /// Ours with [`Merged::edits`] applied.
    pub text: String,
    /// What changes in ours: sorted, never overlapping, each as small as it
    /// can be (common ends trimmed), so applying them one by one keeps
    /// everyone's cursors.
    pub edits: Vec<Edit>,
    /// The text the merged draft is relative to, for the next update:
    /// theirs' content, in ours' words where ours did not change it.
    pub ancestor: String,
    /// Conflict blocks in `text` (new ones and those still there).
    pub conflicts: usize,
}

/// Replace `range` (bytes of ours) by `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
}

/// Why [`merge`] merged nothing.
#[derive(Debug, Clone, thiserror::Error)]
pub enum MergeError {
    /// Ours does not parse: a statement-level merge needs statements.
    #[error("the draft's SQL has syntax errors")]
    Unparsable(Vec<Diagnostic>),
    /// Two of the merge's own edits overlap (a bug): nothing is applied.
    #[error("the merge's edits overlap")]
    Overlap,
}

/// The three-way merge. See the module documentation.
pub fn merge(input: &Input<'_>) -> Result<Merged, MergeError> {
    let ancestor = parse(input.ancestor);
    let ours = parse(input.ours);
    let theirs = parse(input.theirs);
    if !ours.errors.is_empty() {
        return Err(MergeError::Unparsable(ours.errors));
    }
    let (blocks, _) = conflicts_in(input.ours, &ours);
    let units = units(
        [
            (Side::Ancestor, &ancestor, input.ancestor),
            (Side::Ours, &ours, input.ours),
            (Side::Theirs, &theirs, input.theirs),
        ],
        &blocks,
    );
    let (roots, matched) = components(&units);
    let facts = Facts::of(input, &units, &matched);
    let mut components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, root) in roots.iter().enumerate() {
        components.entry(*root).or_default().push(i);
    }

    let mut plan = Plan {
        input,
        blocks: &blocks,
        units: &units,
        roots: &roots,
        result: Vec::new(),
        ancestor: Vec::new(),
        appended_ancestor: Vec::new(),
        anchored: Vec::new(),
        ends: HashMap::new(),
    };
    for (root, members) in &components {
        let c = Component::new(&units, members);
        let decision = decide(&units, &c, members, &matched, &facts);
        plan.apply(*root, &c, &decision, &matched);
    }
    plan.anchor();

    let Plan {
        result,
        ancestor: ancestor_ops,
        appended_ancestor,
        ..
    } = plan;
    let edits = minimal(input.ours, &result)?;
    let text = apply(input.ours, result)?;
    let mut new_ancestor = apply(input.ours, ancestor_ops)?;
    for block in appended_ancestor {
        new_ancestor.push_str("\n\n");
        new_ancestor.push_str(&block);
    }
    Ok(Merged {
        conflicts: conflicts(&text).0.len(),
        text,
        edits,
        ancestor: tidy(&new_ancestor),
    })
}

/// The statements of the three texts, with what links them.
fn units<'a>(sides: [(Side, &'a ParsedSql, &str); 3], blocks: &[Conflict]) -> Vec<Unit<'a>> {
    let mut units = Vec::new();
    let mut counts: HashMap<(Side, Key), usize> = HashMap::new();
    for (side, parsed, sql) in sides {
        for stmt in &parsed.statements {
            let block = if side == Side::Ours {
                blocks
                    .iter()
                    .position(|b| b.range.contains(&stmt.span.start))
                    .map(|i| (i, blocks[i].theirs.contains(&stmt.span.start)))
            } else {
                None
            };
            // The theirs side of a block is an older version's text, not ours.
            let id = (side != Side::Theirs && !matches!(block, Some((_, true)))).then(|| {
                let n = counts.entry((side, stmt.key.clone())).or_default();
                *n += 1;
                (stmt.key.clone(), *n - 1)
            });
            let links = links(stmt);
            units.push(Unit {
                side,
                refs: references(stmt, &links),
                links,
                norm: normalized(stmt, sql),
                stmt,
                block,
                id,
            });
        }
    }
    units
}

/// The atoms of the newer version and (when it compiles) of the draft;
/// those the newer version changed; and what each side dropped.
struct Facts {
    theirs: BTreeMap<Atom, String>,
    ours: Option<BTreeMap<Atom, String>>,
    changed: BTreeSet<Atom>,
    /// In the base version's model, not in the newer one's.
    theirs_dropped: BTreeSet<Atom>,
    /// Defined by the ancestor and by none of ours' statements, or dropped
    /// by a statement ours added.
    ours_dropped: BTreeSet<Atom>,
}

impl Facts {
    fn of(input: &Input<'_>, units: &[Unit<'_>], matched: &[bool]) -> Self {
        let base = facts(input.base_model_sql);
        let theirs = facts(input.theirs_model_sql);
        let changed = base
            .keys()
            .chain(theirs.keys())
            .filter(|a| base.get(*a) != theirs.get(*a))
            .cloned()
            .collect();
        let theirs_dropped = base
            .keys()
            .filter(|a| !theirs.contains_key(*a))
            .cloned()
            .collect();
        let defined = |side: Side| -> BTreeSet<&Atom> {
            units
                .iter()
                .filter(|u| u.side == side && !matches!(u.block, Some((_, true))))
                .flat_map(|u| &u.links.defines)
                .collect()
        };
        let in_ours = defined(Side::Ours);
        let mut ours_dropped: BTreeSet<Atom> = defined(Side::Ancestor)
            .into_iter()
            .filter(|a| !in_ours.contains(a))
            .cloned()
            .collect();
        for (i, unit) in units.iter().enumerate() {
            if unit.side == Side::Ours
                && !matched[i]
                && unit.stmt.keys.iter().any(|k| k.action() == Action::Drop)
            {
                ours_dropped.extend(unit.links.touches.iter().cloned());
            }
        }
        Self {
            theirs,
            ours: input.ours_model_sql.map(facts),
            changed,
            theirs_dropped,
            ours_dropped,
        }
    }
}

/// Each unit's component (its root), and whether each one is matched: an
/// ancestor statement that ours still has, an ours statement unchanged
/// from the ancestor's.
fn components(units: &[Unit<'_>]) -> (Vec<usize>, Vec<bool>) {
    let mut sets = UnionFind::new(units.len());
    let mut first: HashMap<&Atom, usize> = HashMap::new();
    let mut ancestor_ids: HashMap<&(Key, usize), usize> = HashMap::new();
    for (i, unit) in units.iter().enumerate() {
        for atom in unit.links.all() {
            match first.get(atom) {
                Some(&j) => sets.union(i, j),
                None => {
                    first.insert(atom, i);
                }
            }
        }
        if unit.side == Side::Ancestor
            && let Some(id) = &unit.id
        {
            ancestor_ids.insert(id, i);
        }
    }
    let mut matched = vec![false; units.len()];
    let mut block_first: HashMap<usize, usize> = HashMap::new();
    for (i, unit) in units.iter().enumerate() {
        if unit.side != Side::Ours {
            continue;
        }
        if let Some(id) = &unit.id
            && let Some(&a) = ancestor_ids.get(id)
        {
            sets.union(i, a);
            matched[a] = true;
            matched[i] = units[a].norm == unit.norm;
        }
        // A block is one: its sides were one object's.
        if let Some((block, _)) = unit.block {
            match block_first.get(&block) {
                Some(&j) => sets.union(i, j),
                None => {
                    block_first.insert(block, i);
                }
            }
        }
    }
    let roots = (0..units.len()).map(|i| sets.find(i)).collect();
    (roots, matched)
}

/// The table of the module documentation.
fn decide(
    units: &[Unit<'_>],
    c: &Component,
    members: &[usize],
    matched: &[bool],
    facts: &Facts,
) -> Decision {
    let atoms: BTreeSet<&Atom> = members.iter().flat_map(|&i| units[i].links.all()).collect();
    let theirs_changed = atoms.iter().any(|a| facts.changed.contains(*a));
    let ours_changed = c.has_block
        || c.ours.iter().any(|&i| !matched[i])
        || c.ancestor.iter().any(|&i| !matched[i]);
    // A change of one side to what depends on something the other side
    // dropped (a column added to a table theirs dropped, an index on a
    // table ours dropped): whatever else, a conflict. Both dropping it is
    // agreement.
    let drops = |unit: &Unit<'_>, dropped: &BTreeSet<Atom>, also: &BTreeSet<Atom>| {
        unit.refs
            .iter()
            .any(|a| dropped.contains(a) && !also.contains(a))
    };
    let ours_on_theirs_drop = c
        .ours
        .iter()
        .filter(|&&i| !matched[i])
        .any(|&i| drops(&units[i], &facts.theirs_dropped, &facts.ours_dropped));
    let theirs_on_ours_drop = theirs_changed
        && c.theirs
            .iter()
            .any(|&i| drops(&units[i], &facts.ours_dropped, &facts.theirs_dropped));
    if ours_on_theirs_drop || theirs_on_ours_drop {
        return Decision::Conflict;
    }
    match (theirs_changed, ours_changed) {
        (false, ours_changed) => {
            return Decision::Keep {
                ancestor: ours_changed,
            };
        }
        (true, false) => return Decision::Replace,
        (true, true) if c.has_block => return Decision::Conflict,
        (true, true) => {}
    }
    // With the draft's model, by atoms; without, by the statements.
    let same = if let Some(ours) = &facts.ours {
        atoms.iter().all(|a| ours.get(*a) == facts.theirs.get(*a))
    } else {
        let sorted = |side: &[usize]| {
            let mut norms: Vec<&str> = side.iter().map(|&i| units[i].norm.as_str()).collect();
            norms.sort_unstable();
            norms
        };
        sorted(&c.ours) == sorted(&c.theirs)
    };
    // Ours already has every statement of theirs (an update that ran twice,
    // or whose base did not move after it rewrote the text): keep ours, now
    // relative to theirs.
    let has_theirs = !c.theirs.is_empty() && {
        let mut mine: Vec<&str> = c.ours.iter().map(|&i| units[i].norm.as_str()).collect();
        c.theirs.iter().all(|&i| {
            mine.iter()
                .position(|m| *m == units[i].norm)
                .map(|at| mine.swap_remove(at))
                .is_some()
        })
    };
    if same {
        Decision::Same
    } else if has_theirs {
        Decision::Enum(Vec::new())
    } else if let Some(adds) = enum_merge(units, c) {
        Decision::Enum(adds)
    } else if clean(units, c, matched, &facts.changed) {
        Decision::Partial
    } else {
        Decision::Conflict
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Side {
    Ancestor,
    Ours,
    Theirs,
}

/// One statement of one of the three texts.
struct Unit<'a> {
    side: Side,
    stmt: &'a Statement,
    links: Links,
    /// Ours: the conflict block it is in, and whether on its theirs side.
    block: Option<(usize, bool)>,
    /// `(key, occurrence)` among the side's statements: how an ours
    /// statement finds the ancestor one it comes from. None on theirs.
    id: Option<(Key, usize)>,
    /// Deparsed (whitespace, case and inner comments do not count).
    norm: String,
    /// What it depends on besides what it defines ([`references`]).
    refs: Vec<Atom>,
}

/// What a statement is about. See the module documentation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Atom {
    Object(Key),
    /// An enum's label.
    Label(Key, String),
    /// A statement that stands alone (renames): its own key.
    Own(Key),
}

#[derive(Debug, Clone, Default)]
struct Links {
    defines: Vec<Atom>,
    touches: Vec<Atom>,
}

impl Links {
    fn all(&self) -> impl Iterator<Item = &Atom> {
        self.defines.iter().chain(&self.touches)
    }
}

fn object(kind: ObjectKind, path: &[String]) -> Atom {
    Atom::Object(Key::new(Action::Create, kind, path.to_vec()))
}

fn links(stmt: &Statement) -> Links {
    let mut links = Links::default();
    let key = &stmt.key;
    match stmt.node() {
        NodeEnum::CreateStmt(s)
            if key.action() == Action::Create && key.kind() == ObjectKind::Table =>
        {
            links.defines.push(Atom::Object(key.clone()));
            if let [schema, table] = key.path() {
                for element in &s.table_elts {
                    if let Some(NodeEnum::ColumnDef(column)) = element.node.as_ref() {
                        links.defines.push(object(
                            ObjectKind::Column,
                            &[schema.clone(), table.clone(), column.colname.clone()],
                        ));
                    }
                }
            }
        }
        NodeEnum::CreateEnumStmt(s) => {
            links.defines.push(Atom::Object(key.clone()));
            links.defines.extend(
                s.vals
                    .iter()
                    .filter_map(string_value)
                    .map(|label| Atom::Label(key.clone(), label)),
            );
        }
        NodeEnum::AlterEnumStmt(s) => {
            if s.old_val.is_empty() {
                let enum_key = Key::new(Action::Create, ObjectKind::Enum, key.path().to_vec());
                links.defines.push(Atom::Label(enum_key, s.new_val.clone()));
            } else {
                // RENAME VALUE stands alone, like other renames.
                links.touches.push(Atom::Own(key.clone()));
            }
        }
        NodeEnum::RenameStmt(_) | NodeEnum::AlterObjectSchemaStmt(_) => {
            links.touches.push(Atom::Own(key.clone()));
        }
        node => {
            // A multi-command ALTER TABLE keys as the table, then each
            // command: the table key only groups them.
            let skip = usize::from(
                stmt.keys.len() > 1
                    && key.action() == Action::Alter
                    && matches!(node, NodeEnum::AlterTableStmt(_)),
            );
            for key in stmt.keys.iter().skip(skip) {
                map_key(key, &mut links);
            }
        }
    }
    links
        .defines
        .extend(stmt.inline.iter().cloned().map(Atom::Object));
    links
}

fn map_key(key: &Key, links: &mut Links) {
    let path = key.path();
    match (key.action(), key.kind()) {
        (Action::Create, ObjectKind::Statement) | (Action::Rename, _) => {
            links.touches.push(Atom::Own(key.clone()));
        }
        (Action::Create | Action::Comment, _) => links.defines.push(Atom::Object(key.clone())),
        (Action::Alter | Action::Drop, ObjectKind::Type) => {
            for kind in [ObjectKind::Type, ObjectKind::Enum, ObjectKind::Domain] {
                links.touches.push(object(kind, path));
            }
        }
        (Action::Drop, ObjectKind::Constraint) => {
            for kind in [ObjectKind::Constraint, ObjectKind::ForeignKey] {
                links.touches.push(object(kind, path));
            }
        }
        (Action::Alter | Action::Drop, kind) => links.touches.push(object(kind, path)),
    }
}

/// What a statement depends on besides what it defines: what its atoms
/// belong to (a column's table, a comment's object), what it renames, the
/// table an index is on, the table a foreign key references. A side that
/// drops one of these conflicts with the other side's change to the
/// statement.
fn references(stmt: &Statement, links: &Links) -> Vec<Atom> {
    let mut out: Vec<Atom> = links.touches.clone();
    out.extend(links.all().flat_map(owners));
    for key in &stmt.keys {
        if matches!(key.action(), Action::Rename | Action::Alter) {
            let renamed = object(key.kind(), key.path());
            out.extend(owners(&renamed));
            out.push(renamed);
        }
    }
    let table = |rv: &RangeVar| {
        let (schema, name) = QualifiedName::from_range_var(rv).resolve(&stmt.search_path);
        object(ObjectKind::Table, &[schema, name])
    };
    // The elements that can hold a foreign key: columns and constraints.
    let elements: Vec<&NodeEnum> = match stmt.node() {
        NodeEnum::IndexStmt(s) => {
            out.extend(s.relation.as_ref().map(table));
            Vec::new()
        }
        NodeEnum::CreateStmt(s) => s
            .table_elts
            .iter()
            .filter_map(|e| e.node.as_ref())
            .collect(),
        NodeEnum::AlterTableStmt(s) => s
            .cmds
            .iter()
            .filter_map(|cmd| match cmd.node.as_ref()? {
                NodeEnum::AlterTableCmd(cmd) => cmd.def.as_ref()?.node.as_ref(),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut constraints: Vec<&Constraint> = Vec::new();
    for element in elements {
        match element {
            NodeEnum::ColumnDef(column) => constraints.extend(column_constraints(column)),
            NodeEnum::Constraint(c) => constraints.push(c),
            _ => {}
        }
    }
    out.extend(
        constraints
            .into_iter()
            .filter(|c| c.contype() == ConstrType::ConstrForeign)
            .filter_map(|c| c.pktable.as_ref())
            .map(table),
    );
    // What it defines is not something it depends on.
    out.retain(|a| !links.defines.contains(a));
    out.sort();
    out.dedup();
    out
}

fn column_constraints(column: &ColumnDef) -> impl Iterator<Item = &Constraint> {
    column
        .constraints
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            NodeEnum::Constraint(c) => Some(&**c),
            _ => None,
        })
}

/// What an atom belongs to: a change to one of these and to the atom
/// overlap (dropping a table and changing its column).
fn owners(atom: &Atom) -> Vec<Atom> {
    match atom {
        Atom::Label(enum_key, _) => vec![Atom::Object(enum_key.clone())],
        Atom::Own(_) => Vec::new(),
        Atom::Object(key) => {
            let path = key.path();
            let mut out = Vec::new();
            if key.action() == Action::Comment {
                let target = match key.kind() {
                    // COMMENT ON TYPE is about an enum, a composite or a domain.
                    ObjectKind::Type => vec![
                        object(ObjectKind::Type, path),
                        object(ObjectKind::Enum, path),
                        object(ObjectKind::Domain, path),
                    ],
                    kind => vec![object(kind, path)],
                };
                for target in target {
                    out.extend(owners(&target));
                    out.push(target);
                }
                return out;
            }
            match (key.kind(), path) {
                (
                    ObjectKind::Column
                    | ObjectKind::Constraint
                    | ObjectKind::ForeignKey
                    | ObjectKind::Policy
                    | ObjectKind::Trigger
                    | ObjectKind::Rule,
                    [schema, table, _],
                )
                | (ObjectKind::RowSecurity | ObjectKind::ForceRowSecurity, [schema, table]) => {
                    let owner = [schema.clone(), table.clone()];
                    out.push(object(ObjectKind::Table, &owner));
                    out.push(object(ObjectKind::Domain, &owner));
                }
                _ => {}
            }
            out
        }
    }
}

fn overlap(a: &Atom, b: &Atom) -> bool {
    a == b || owners(a).contains(b) || owners(b).contains(a)
}

fn string_value(node: &pg_query::protobuf::Node) -> Option<String> {
    match node.node.as_ref()? {
        NodeEnum::String(s) => Some(s.sval.clone()),
        _ => None,
    }
}

/// A statement as Postgres would print it back: whitespace, keyword case
/// and comments inside it do not count.
fn normalized(stmt: &Statement, sql: &str) -> String {
    stmt.node().deparse().unwrap_or_else(|_| {
        stmt.text(sql)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// Each atom of a `render_sql` text, deparsed: a table's own clauses, each
/// column, each constraint, each other statement. Equal facts, equal
/// objects, whatever the formatting of the texts they came from.
fn facts(sql: &str) -> BTreeMap<Atom, String> {
    let parsed = parse(sql);
    let mut facts: BTreeMap<Atom, String> = BTreeMap::new();
    let mut add = |atom: Atom, text: String| {
        let entry = facts.entry(atom).or_default();
        if !entry.is_empty() {
            entry.push('\n');
        }
        entry.push_str(&text);
    };
    for stmt in &parsed.statements {
        match stmt.node() {
            NodeEnum::CreateStmt(s)
                if stmt.key.action() == Action::Create && stmt.key.kind() == ObjectKind::Table =>
            {
                let deparse = |elements| {
                    let mut one = s.clone();
                    one.table_elts = elements;
                    NodeEnum::CreateStmt(one).deparse().unwrap_or_default()
                };
                add(Atom::Object(stmt.key.clone()), deparse(Vec::new()));
                let [schema, table] = stmt.key.path() else {
                    continue;
                };
                for element in &s.table_elts {
                    let atom = match element.node.as_ref() {
                        Some(NodeEnum::ColumnDef(c)) => object(
                            ObjectKind::Column,
                            &[schema.clone(), table.clone(), c.colname.clone()],
                        ),
                        Some(NodeEnum::Constraint(c)) if !c.conname.is_empty() => object(
                            if c.contype() == pg_query::protobuf::ConstrType::ConstrForeign {
                                ObjectKind::ForeignKey
                            } else {
                                ObjectKind::Constraint
                            },
                            &[schema.clone(), table.clone(), c.conname.clone()],
                        ),
                        _ => continue,
                    };
                    add(atom, deparse(vec![element.clone()]));
                }
            }
            // The enum is its whole statement (order included); a label is
            // there or not.
            NodeEnum::CreateEnumStmt(s) => {
                add(Atom::Object(stmt.key.clone()), normalized(stmt, sql));
                for label in s.vals.iter().filter_map(string_value) {
                    add(Atom::Label(stmt.key.clone(), label), String::new());
                }
            }
            _ => {
                let text = normalized(stmt, sql);
                for atom in links(stmt).all() {
                    add(atom.clone(), text.clone());
                }
            }
        }
    }
    facts
}

/// The statements of one component, per side, in text order.
struct Component {
    ours: Vec<usize>,
    ancestor: Vec<usize>,
    theirs: Vec<usize>,
    has_block: bool,
}

impl Component {
    fn new(units: &[Unit<'_>], members: &[usize]) -> Self {
        let mut c = Self {
            ours: Vec::new(),
            ancestor: Vec::new(),
            theirs: Vec::new(),
            has_block: false,
        };
        for &i in members {
            match units[i].side {
                Side::Ours => {
                    c.has_block |= units[i].block.is_some();
                    // A block's theirs side is not ours.
                    if !matches!(units[i].block, Some((_, true))) {
                        c.ours.push(i);
                    }
                }
                Side::Ancestor => c.ancestor.push(i),
                Side::Theirs => c.theirs.push(i),
            }
        }
        let start = |i: &usize| units[*i].stmt.span.start;
        c.ours.sort_by_key(start);
        c.ancestor.sort_by_key(start);
        c.theirs.sort_by_key(start);
        c
    }
}

enum Decision {
    /// Theirs changed nothing here. `ancestor`: ours did, so the next
    /// ancestor keeps the old one's statements.
    Keep {
        ancestor: bool,
    },
    /// Only theirs changed it.
    Replace,
    /// Both, the same way.
    Same,
    /// Both, different atoms: theirs' statements and ours' changed ones.
    Partial,
    /// Ours, then these statements (theirs' enum labels ours lacks), relative
    /// to theirs: both added enum labels; or ours already has all of theirs
    /// (none to add).
    Enum(Vec<String>),
    Conflict,
}

/// Both changed, but not the same atoms, and ours' changes define nothing
/// theirs' statements do.
fn clean(units: &[Unit<'_>], c: &Component, matched: &[bool], changed: &BTreeSet<Atom>) -> bool {
    let ours_changed: Vec<&Unit<'_>> = c
        .ours
        .iter()
        .chain(&c.ancestor)
        .filter(|&&i| !matched[i])
        .map(|&i| &units[i])
        .collect();
    let theirs_changed: BTreeSet<&Atom> = c
        .ours
        .iter()
        .chain(&c.ancestor)
        .chain(&c.theirs)
        .flat_map(|&i| units[i].links.all())
        .filter(|a| changed.contains(*a))
        .collect();
    let disjoint = ours_changed.iter().all(|u| {
        u.links
            .all()
            .all(|a| theirs_changed.iter().all(|b| !overlap(a, b)))
    });
    let theirs_defines: BTreeSet<&Atom> = c
        .theirs
        .iter()
        .flat_map(|&i| &units[i].links.defines)
        .collect();
    let redefined = ours_changed
        .iter()
        .filter(|u| u.side == Side::Ours)
        .flat_map(|u| &u.links.defines)
        .any(|a| theirs_defines.contains(a));
    disjoint && !redefined
}

/// When every statement of the component creates or extends one enum,
/// neither side removed a label and theirs added some: the statements that
/// add theirs' labels to ours.
fn enum_merge(units: &[Unit<'_>], c: &Component) -> Option<Vec<String>> {
    let all: Vec<&Unit<'_>> = c
        .ours
        .iter()
        .chain(&c.ancestor)
        .chain(&c.theirs)
        .map(|&i| &units[i])
        .collect();
    let path = all.first()?.stmt.key.path().to_vec();
    let enum_only = all.iter().all(|u| {
        u.stmt.key.path() == path.as_slice()
            && match u.stmt.node() {
                NodeEnum::CreateEnumStmt(_) => true,
                NodeEnum::AlterEnumStmt(s) => s.old_val.is_empty(),
                _ => false,
            }
    });
    if !enum_only {
        return None;
    }
    let labels = |side: &[usize]| -> Vec<String> {
        let mut labels: Vec<String> = Vec::new();
        for &i in side {
            match units[i].stmt.node() {
                NodeEnum::CreateEnumStmt(s) => {
                    labels = s.vals.iter().filter_map(string_value).collect();
                }
                NodeEnum::AlterEnumStmt(s) if !labels.contains(&s.new_val) => {
                    let at = labels
                        .iter()
                        .position(|l| *l == s.new_val_neighbor)
                        .map_or(labels.len(), |n| n + usize::from(s.new_val_is_after));
                    labels.insert(at, s.new_val.clone());
                }
                _ => {}
            }
        }
        labels
    };
    let (base, ours, theirs) = (labels(&c.ancestor), labels(&c.ours), labels(&c.theirs));
    if base
        .iter()
        .any(|l| !ours.contains(l) || !theirs.contains(l))
    {
        return None;
    }
    let name = format!("{}.{}", quote_ident(&path[0]), quote_ident(&path[1]));
    let mut present = ours.clone();
    let mut adds = Vec::new();
    for (n, label) in theirs.iter().enumerate() {
        if present.contains(label) {
            continue;
        }
        let place = match theirs[..n].iter().rev().find(|l| present.contains(*l)) {
            Some(previous) => format!(" AFTER {}", literal(previous)),
            None => present
                .first()
                .map(|first| format!(" BEFORE {}", literal(first)))
                .unwrap_or_default(),
        };
        adds.push(format!(
            "ALTER TYPE {name} ADD VALUE {}{place};",
            literal(label)
        ));
        present.push(label.clone());
    }
    // None to add: ours has all of theirs' labels already.
    Some(adds)
}

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A change to a text: replace `range` by `text`; `seq` orders insertions
/// at one place.
struct Op {
    range: Range<usize>,
    text: String,
    seq: usize,
}

/// Turns decisions into edits of ours (the result) and of the next ancestor.
struct Plan<'a, 'u> {
    input: &'a Input<'a>,
    blocks: &'a [Conflict],
    units: &'a [Unit<'u>],
    /// Each unit's component.
    roots: &'a [usize],
    result: Vec<Op>,
    ancestor: Vec<Op>,
    /// Ancestor content of components ours deleted entirely: appended.
    appended_ancestor: Vec<String>,
    /// Components with nothing of ours to stand on: placed after what
    /// precedes them in theirs ([`Plan::anchor`]). (component, result
    /// text, ancestor text)
    anchored: Vec<(usize, String, Option<String>)>,
    /// Where each component ends up in ours (its last byte), for anchors.
    ends: HashMap<usize, usize>,
}

/// One place of ours a component occupies: a statement, or a block.
#[derive(Clone)]
enum Slot {
    Statement(usize),
    Block(usize),
}

impl Plan<'_, '_> {
    fn ours_sql(&self) -> &str {
        self.input.ours
    }

    fn slots(&self, c: &Component) -> Vec<Slot> {
        let mut slots = Vec::new();
        let mut seen = Vec::new();
        for &i in &c.ours {
            match self.units[i].block {
                Some((block, _)) if !seen.contains(&block) => {
                    seen.push(block);
                    slots.push(Slot::Block(block));
                }
                Some(_) => {}
                None => slots.push(Slot::Statement(i)),
            }
        }
        slots.sort_by_key(|slot| self.slot_range(slot, false).start);
        slots
    }

    /// The slot's range: the statement from its first token (its comments
    /// above stay) to its `;` and same-line comment; `whole`: with those
    /// comments and the line break after, to delete it.
    fn slot_range(&self, slot: &Slot, whole: bool) -> Range<usize> {
        let sql = self.ours_sql();
        let range = match slot {
            Slot::Statement(i) => {
                let stmt = self.units[*i].stmt;
                let start = if whole {
                    lead_start(sql, stmt)
                } else {
                    stmt.span.start
                };
                start..stmt.extent.end
            }
            Slot::Block(b) => self.blocks[*b].range.clone(),
        };
        if !whole {
            return range;
        }
        // With its line break; and the blank line it would leave between
        // two others.
        let bytes = sql.as_bytes();
        let (mut start, mut end) = (range.start, range.end);
        if bytes.get(end) == Some(&b'\n') {
            end += 1;
        }
        let blank_before = start == 0 || sql[..start].ends_with("\n\n");
        if blank_before && bytes.get(end) == Some(&b'\n') {
            end += 1;
        } else if blank_before && end == sql.len() && start > 0 {
            start -= 1;
        }
        start..end
    }

    /// The text a slot moves with: a statement from its first token (`full`:
    /// with its comments above), a block's yours side.
    fn slot_text(&self, slot: &Slot, full: bool) -> &str {
        let sql = self.ours_sql();
        match slot {
            Slot::Statement(i) => {
                let stmt = self.units[*i].stmt;
                let start = if full {
                    lead_start(sql, stmt)
                } else {
                    stmt.span.start
                };
                &sql[start..stmt.extent.end]
            }
            Slot::Block(b) => &sql[self.blocks[*b].yours.clone()],
        }
    }

    fn texts(&self, indices: &[usize], sql: &str, full: bool) -> String {
        indices
            .iter()
            .map(|&i| {
                let stmt = self.units[i].stmt;
                let start = if full {
                    lead_start(sql, stmt)
                } else {
                    stmt.span.start
                };
                &sql[start..stmt.extent.end]
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn seq(&self) -> usize {
        self.result.len() + self.ancestor.len() + self.anchored.len()
    }

    /// Replace the first slot by `text` (or delete it when empty), delete
    /// the others.
    fn put(&mut self, into_ancestor: bool, slots: &[Slot], text: &str) {
        let mut ops = Vec::new();
        for (n, slot) in slots.iter().enumerate() {
            let seq = self.seq() + n;
            if n == 0 && !text.is_empty() {
                ops.push(Op {
                    range: self.slot_range(slot, false),
                    text: text.to_owned(),
                    seq,
                });
            } else {
                ops.push(Op {
                    range: self.slot_range(slot, true),
                    text: String::new(),
                    seq,
                });
            }
        }
        if into_ancestor {
            self.ancestor.extend(ops);
        } else {
            self.result.extend(ops);
        }
    }

    #[expect(clippy::too_many_lines, reason = "one arm per decision")]
    fn apply(&mut self, root: usize, c: &Component, decision: &Decision, matched: &[bool]) {
        let input = self.input;
        let slots = self.slots(c);
        let theirs_tail = self.texts(&c.theirs, input.theirs, false);
        let theirs_full = self.texts(&c.theirs, input.theirs, true);
        let ancestor_tail = self.texts(&c.ancestor, input.ancestor, false);
        let last_end = slots.iter().map(|s| self.slot_range(s, false).end).max();
        let first_end = slots.first().map(|s| self.slot_range(s, false).end);
        match decision {
            Decision::Keep { ancestor } => {
                if let Some(end) = last_end {
                    self.ends.insert(root, end);
                }
                if *ancestor {
                    if slots.is_empty() {
                        if !c.ancestor.is_empty() {
                            self.appended_ancestor.push(self.texts(
                                &c.ancestor,
                                input.ancestor,
                                true,
                            ));
                        }
                    } else {
                        self.put(true, &slots, &ancestor_tail);
                    }
                }
            }
            Decision::Same => {
                if let Some(end) = last_end {
                    self.ends.insert(root, end);
                }
            }
            Decision::Replace => {
                if slots.is_empty() {
                    if !c.theirs.is_empty() {
                        self.anchored
                            .push((root, theirs_full.clone(), Some(theirs_full)));
                    }
                } else {
                    if let Some(end) = first_end {
                        self.ends.insert(root, end);
                    }
                    self.put(false, &slots, &theirs_tail);
                    self.put(true, &slots, &theirs_tail);
                }
            }
            Decision::Partial => {
                let same: Vec<Slot> = slots
                    .iter()
                    .filter(|s| matches!(s, Slot::Statement(i) if matched[*i]))
                    .cloned()
                    .collect();
                if same.is_empty() {
                    // Before ours' first (changed) statement.
                    let at = slots.first().map_or(0, |s| self.slot_range(s, true).start);
                    let seq = self.seq();
                    self.result.push(Op {
                        range: at..at,
                        text: format!("{theirs_full}\n\n"),
                        seq,
                    });
                } else {
                    self.put(false, &same, &theirs_tail);
                }
                // What stays: theirs where ours' first unchanged statement
                // was, and ours' changed ones (the other unchanged go).
                let kept = slots
                    .iter()
                    .filter(|s| {
                        !same
                            .iter()
                            .skip(1)
                            .any(|d| self.slot_range(d, false) == self.slot_range(s, false))
                    })
                    .map(|s| self.slot_range(s, false).end)
                    .max();
                if let Some(end) = kept {
                    self.ends.insert(root, end);
                }
                self.put(true, &slots, &theirs_tail);
            }
            Decision::Enum(adds) => {
                if let Some(end) = last_end {
                    if !adds.is_empty() {
                        let seq = self.seq();
                        self.result.push(Op {
                            range: end..end,
                            text: format!("\n{}", adds.join("\n")),
                            seq,
                        });
                    }
                    self.ends.insert(root, end);
                }
                self.put(true, &slots, &theirs_tail);
            }
            Decision::Conflict => {
                let yours: Vec<&str> = slots
                    .iter()
                    .enumerate()
                    .map(|(n, slot)| self.slot_text(slot, n > 0))
                    .filter(|t| !t.trim().is_empty())
                    .collect();
                let yours = yours.join("\n");
                let both = !yours.is_empty() && !c.theirs.is_empty() && {
                    let mine: BTreeSet<&Atom> = c
                        .ours
                        .iter()
                        .flat_map(|&i| &self.units[i].links.defines)
                        .collect();
                    c.theirs
                        .iter()
                        .flat_map(|&i| self.units[i].links.all())
                        .all(|a| !mine.iter().any(|m| overlap(m, a)))
                };
                let block = block_text(&yours, &theirs_tail, input.label, both);
                if slots.is_empty() {
                    self.anchored.push((root, block, Some(theirs_full)));
                } else {
                    if let Some(end) = first_end {
                        self.ends.insert(root, end);
                    }
                    self.put(false, &slots, &block);
                    self.put(true, &slots, &theirs_tail);
                }
            }
        }
    }

    /// `at`, or the end of an edit it would fall inside (an insertion must
    /// never land in text another edit replaces).
    fn outside(&self, mut at: usize) -> usize {
        while let Some(op) = self
            .result
            .iter()
            .find(|op| op.range.start < at && at < op.range.end)
        {
            at = op.range.end;
        }
        at
    }

    /// Places components with nothing of ours to stand on: after the last
    /// place (in ours) of what precedes them in theirs; at the top when
    /// nothing does.
    fn anchor(&mut self) {
        if self.anchored.is_empty() {
            return;
        }
        let mut theirs: Vec<(usize, usize)> = Vec::new(); // (statement index, unit)
        for (i, unit) in self.units.iter().enumerate() {
            if unit.side == Side::Theirs {
                theirs.push((unit.stmt.index, i));
            }
        }
        theirs.sort_unstable();
        let anchored = std::mem::take(&mut self.anchored);
        let mut placed = vec![false; anchored.len()];
        let mut last: Option<usize> = None;
        let mut seq = self.seq() + 1_000_000;
        for (_, unit) in theirs {
            let root = self.roots[unit];
            if let Some(n) = anchored.iter().position(|(r, ..)| *r == root) {
                if !placed[n] {
                    placed[n] = true;
                    let (_, text, ancestor) = &anchored[n];
                    let at = self.outside(last.unwrap_or(0));
                    let text = if last.is_some() {
                        format!("\n\n{text}")
                    } else {
                        format!("{text}\n\n")
                    };
                    seq += 1;
                    self.result.push(Op {
                        range: at..at,
                        text,
                        seq,
                    });
                    if let Some(ancestor) = ancestor {
                        self.appended_ancestor.push(ancestor.clone());
                    }
                }
            } else if let Some(&end) = self.ends.get(&root) {
                last = Some(last.map_or(end, |l| l.max(end)));
            }
        }
        // Nothing of theirs to follow (cannot happen: anchored components
        // have theirs' statements): at the end.
        let end = self.input.ours.len();
        for (n, (_, text, ancestor)) in anchored.iter().enumerate() {
            if !placed[n] {
                seq += 1;
                self.result.push(Op {
                    range: end..end,
                    text: format!("\n\n{text}"),
                    seq,
                });
                if let Some(ancestor) = ancestor {
                    self.appended_ancestor.push(ancestor.clone());
                }
            }
        }
    }
}

/// Where a statement starts with the comments above it: its extent, but
/// after the graph edits' marker line or a conflict marker when one is
/// among them. Markers stay where they are: never moved or deleted with the
/// statement under them, never copied in from theirs.
fn lead_start(sql: &str, stmt: &Statement) -> usize {
    let mut start = stmt.extent.start;
    let mut at = stmt.extent.start;
    for line in sql[stmt.extent.start..stmt.span.start].split_inclusive('\n') {
        at += line.len();
        if line.trim() == GRAPH_EDITS_MARKER || Marker::read(line).is_some() {
            start = at;
        }
    }
    start
}

/// At most one blank line in a row, one line break at the end: the
/// ancestor is only read by the next merge, so its spacing is ours to set.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut breaks = 0;
    for c in text.trim_end().chars() {
        if c == '\n' {
            breaks += 1;
            if breaks > 2 {
                continue;
            }
        } else {
            breaks = 0;
        }
        out.push(c);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

fn block_text(yours: &str, theirs: &str, label: &str, both: bool) -> String {
    let mut out = String::from(YOURS_MARKER);
    if both {
        let _ = write!(out, " {BOTH_NOTE}");
    }
    out.push('\n');
    if !yours.is_empty() {
        out.push_str(yours);
        out.push('\n');
    }
    let _ = writeln!(out, "{MIDDLE} {label}");
    if !theirs.is_empty() {
        out.push_str(theirs);
        out.push('\n');
    }
    let _ = write!(out, "{END} {label}");
    out
}

fn apply(text: &str, mut ops: Vec<Op>) -> Result<String, MergeError> {
    ops.sort_by_key(|op| (op.range.start, op.range.end, op.seq));
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for op in ops {
        let Some(start) = clip(text, &op.range, at) else {
            return Err(MergeError::Overlap);
        };
        let op = Op {
            range: start..op.range.end.max(start),
            ..op
        };
        out.push_str(&text[at..op.range.start]);
        out.push_str(&op.text);
        at = op.range.end;
    }
    out.push_str(&text[at..]);
    Ok(out)
}

/// Where an edit may start given that the text before `at` is already
/// taken by the previous one: where it starts when they do not overlap; at
/// `at` when they share only white space (two deletions each taking the
/// blank line between them); `None` otherwise.
fn clip(text: &str, range: &Range<usize>, at: usize) -> Option<usize> {
    if range.start >= at {
        Some(range.start)
    } else if text[range.start..at.min(range.end)].trim().is_empty() {
        Some(at)
    } else {
        None
    }
}

/// The ops as edits, each trimmed of what it keeps at both ends.
fn minimal(text: &str, ops: &[Op]) -> Result<Vec<Edit>, MergeError> {
    let mut sorted: Vec<&Op> = ops.iter().collect();
    sorted.sort_by_key(|op| (op.range.start, op.range.end, op.seq));
    let mut edits: Vec<Edit> = Vec::new();
    let mut at = 0;
    for op in sorted {
        let start = clip(text, &op.range, at).ok_or(MergeError::Overlap)?;
        let op_range = start..op.range.end.max(start);
        at = op_range.end;
        let old = &text[op_range.clone()];
        let new = op.text.as_str();
        let prefix = common_prefix(old, new);
        let suffix = common_suffix(&old[prefix..], &new[prefix..]);
        let range = op_range.start + prefix..op_range.end - suffix;
        let insert = &new[prefix..new.len() - suffix];
        if range.is_empty() && insert.is_empty() {
            continue;
        }
        // Two insertions at one place: one edit.
        if let Some(last) = edits.last_mut()
            && last.range.end == range.start
            && range.is_empty()
            && last.range.is_empty()
        {
            last.text.push_str(insert);
            continue;
        }
        edits.push(Edit {
            range,
            text: insert.to_owned(),
        });
    }
    Ok(edits)
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or(a.len().min(b.len()), |((i, _), _)| i)
        .min(b.len())
}

fn common_suffix(a: &str, b: &str) -> usize {
    let mut n = 0;
    for (x, y) in a.chars().rev().zip(b.chars().rev()) {
        if x != y {
            break;
        }
        n += x.len_utf8();
    }
    n
}

/// Disjoint sets over unit indices.
struct UnionFind(Vec<usize>);

impl UnionFind {
    fn new(n: usize) -> Self {
        Self((0..n).collect())
    }

    fn find(&mut self, i: usize) -> usize {
        let mut root = i;
        while self.0[root] != root {
            root = self.0[root];
        }
        let mut i = i;
        while self.0[i] != root {
            let next = self.0[i];
            self.0[i] = root;
            i = next;
        }
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Texts written the way `render_sql` writes them stand for their own
    /// models.
    fn merge3(ancestor: &str, ours: &str, theirs: &str) -> Merged {
        merge(&Input {
            ancestor,
            ours,
            theirs,
            base_model_sql: ancestor,
            theirs_model_sql: theirs,
            ours_model_sql: None,
            label: "v2",
        })
        .expect("ours parses")
    }

    /// Applying the edits one by one gives the merged text.
    fn check_edits(ours: &str, merged: &Merged) {
        let mut text = ours.to_owned();
        for edit in merged.edits.iter().rev() {
            text.replace_range(edit.range.clone(), &edit.text);
        }
        assert_eq!(text, merged.text);
        for pair in merged.edits.windows(2) {
            assert!(
                pair[0].range.end <= pair[1].range.start,
                "{:?}",
                merged.edits
            );
        }
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
    fn nothing_changed_nothing_moves() {
        let merged = merge3(BASE, BASE, BASE);
        assert_eq!(merged.text, BASE);
        assert_eq!(merged.edits, []);
        assert_eq!(merged.ancestor, BASE);
        assert_eq!(merged.conflicts, 0);
    }

    #[test]
    fn what_only_theirs_changed_comes_in() {
        let theirs = BASE.replace(
            "user_id bigint NOT NULL REFERENCES users (id)\n",
            "user_id bigint NOT NULL REFERENCES users (id),\n    total numeric(10, 2)\n",
        );
        let merged = merge3(BASE, BASE, &theirs);
        assert_eq!(merged.text, theirs);
        assert_eq!(merged.ancestor, theirs);
        check_edits(BASE, &merged);
        // One small edit, inside orders.
        assert_eq!(merged.edits.len(), 1, "{:?}", merged.edits);
        assert!(merged.edits[0].range.start > BASE.find("CREATE TABLE orders").unwrap());
    }

    #[test]
    fn what_only_ours_changed_stays() {
        let ours = BASE.replace(
            "email text NOT NULL\n",
            "email text NOT NULL,\n    name text\n",
        );
        let merged = merge3(BASE, &ours, BASE);
        assert_eq!(merged.text, ours);
        assert_eq!(merged.edits, []);
        // Relative to theirs, which still has the old users.
        assert_eq!(merged.ancestor, BASE);
    }

    #[test]
    fn the_same_change_on_both_sides_is_no_conflict() {
        let both = BASE.replace(
            "email text NOT NULL\n",
            "email text NOT NULL,\n    name text\n",
        );
        let merged = merge3(BASE, &both, &both);
        assert_eq!(merged.text, both);
        assert_eq!(merged.edits, []);
        assert_eq!(merged.conflicts, 0);
    }

    #[test]
    fn different_changes_to_one_object_conflict() {
        let ours = BASE.replace(
            "email text NOT NULL\n",
            "email text NOT NULL,\n    name text\n",
        );
        let theirs = BASE.replace(
            "email text NOT NULL\n",
            "email text NOT NULL,\n    name varchar(40)\n",
        );
        let merged = merge3(BASE, &ours, &theirs);
        assert_eq!(merged.conflicts, 1);
        assert_eq!(
            merged.text,
            "\
-- <<<<<<< yours
CREATE TABLE users (
    id bigint PRIMARY KEY,
    email text NOT NULL,
    name text
);
-- ======= v2
CREATE TABLE users (
    id bigint PRIMARY KEY,
    email text NOT NULL,
    name varchar(40)
);
-- >>>>>>> v2

CREATE TABLE orders (
    id bigint PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id)
);
"
        );
        check_edits(&ours, &merged);
        // The next update compares with theirs.
        assert_eq!(merged.ancestor, theirs);
        let diagnostics = conflict_diagnostics(&merged.text);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, Code::Conflict);
        assert_eq!(
            diagnostics[0].message,
            "Conflict with v2: keep yours or theirs."
        );
        assert_eq!(diagnostics[0].span.unwrap().from.line, 1);
    }

    #[test]
    fn an_object_added_on_both_sides_differently_conflicts() {
        let ours = format!("{BASE}\nCREATE TABLE audit (id bigint, note text);\n");
        let theirs = format!("{BASE}\nCREATE TABLE audit (id bigint, payload jsonb);\n");
        let merged = merge3(BASE, &ours, &theirs);
        assert_eq!(merged.conflicts, 1);
        assert!(
            merged.text.ends_with(
                "\
-- <<<<<<< yours
CREATE TABLE audit (id bigint, note text);
-- ======= v2
CREATE TABLE audit (id bigint, payload jsonb);
-- >>>>>>> v2
"
            ),
            "{}",
            merged.text
        );
        // Both added the same: no conflict.
        assert_eq!(merge3(BASE, &ours, &ours).conflicts, 0);
    }

    #[test]
    fn dropping_what_the_other_side_edited_conflicts() {
        let with_audit = format!("{BASE}\nCREATE TABLE audit (id bigint);\n");
        let audit_edited = format!("{BASE}\nCREATE TABLE audit (id bigint, at timestamptz);\n");

        // Ours deleted it, theirs edited it.
        let merged = merge3(&with_audit, BASE, &audit_edited);
        assert_eq!(merged.conflicts, 1);
        assert!(merged.text.contains(
            "-- <<<<<<< yours\n-- ======= v2\nCREATE TABLE audit (id bigint, at timestamptz);\n-- >>>>>>> v2"
        ), "{}", merged.text);

        // Theirs dropped it, ours edited it.
        let merged = merge3(&with_audit, &audit_edited, BASE);
        assert_eq!(merged.conflicts, 1);
        assert!(merged.text.contains(
            "-- <<<<<<< yours\nCREATE TABLE audit (id bigint, at timestamptz);\n-- ======= v2\n-- >>>>>>> v2"
        ), "{}", merged.text);

        // Ours dropped it with a statement, theirs edited it.
        let dropped = format!("{with_audit}\nDROP TABLE audit;\n");
        assert_eq!(merge3(&with_audit, &dropped, &audit_edited).conflicts, 1);

        // Theirs dropped it, ours did not touch it: gone.
        let merged = merge3(&with_audit, &with_audit, BASE);
        assert_eq!(merged.conflicts, 0);
        assert_eq!(merged.text.trim_end(), BASE.trim_end());
    }

    #[test]
    fn statements_appended_by_graph_edits_merge_by_their_own_keys() {
        let theirs = BASE.replace(
            "email text NOT NULL\n",
            "email text NOT NULL,\n    plan text\n",
        );
        let ours = format!(
            "{BASE}\n-- edits from the graph\n\
             ALTER TABLE users RENAME TO members;\n\
             ALTER TABLE members ADD COLUMN bio text;\n"
        );
        let merged = merge3(BASE, &ours, &theirs);
        assert_eq!(merged.conflicts, 0, "{}", merged.text);
        assert_eq!(
            merged.text,
            format!(
                "{theirs}\n-- edits from the graph\n\
                 ALTER TABLE users RENAME TO members;\n\
                 ALTER TABLE members ADD COLUMN bio text;\n"
            )
        );
        check_edits(&ours, &merged);
        // Theirs, plus the marker (a comment: it stays where it is).
        assert_eq!(merged.ancestor, format!("{theirs}\n{GRAPH_EDITS_MARKER}\n"));
    }

    /// What graph edits append (web `features/schema/edits`, snapshotted
    /// there and checked against the parser in `tests/graph_edits.rs`).
    fn graph_edits(statements: &[&str]) -> String {
        format!("{BASE}\n{GRAPH_EDITS_MARKER}\n{}\n", statements.join("\n"))
    }

    #[test]
    fn the_graph_edits_marker_stays_put() {
        // The first statement under the marker goes into a conflict block
        // (theirs changed the column it alters); the marker stays.
        let ours = graph_edits(&[
            "ALTER TABLE public.users ALTER COLUMN email DROP NOT NULL;",
            "ALTER TABLE public.users ADD COLUMN nickname text;",
        ]);
        let theirs = BASE.replace("email text NOT NULL", "email varchar(320) NOT NULL");
        let merged = merge3(BASE, &ours, &theirs);
        assert_eq!(merged.conflicts, 1, "{}", merged.text);
        assert!(
            merged.text.ends_with(&format!(
                "{GRAPH_EDITS_MARKER}\nALTER TABLE public.users ADD COLUMN nickname text;\n"
            )),
            "{}",
            merged.text
        );
        let block = &conflicts(&merged.text).0[0];
        let yours = &merged.text[block.yours.clone()];
        assert!(yours.ends_with("\nALTER TABLE public.users ALTER COLUMN email DROP NOT NULL;"));
        assert!(!yours.contains(GRAPH_EDITS_MARKER));
        check_edits(&ours, &merged);
    }

    #[test]
    fn a_version_published_with_graph_edits_brings_its_statements_not_its_marker() {
        // v2 came from another draft whose graph edits added a column; ours
        // has graph edits of its own.
        let theirs = graph_edits(&["ALTER TABLE public.orders ADD COLUMN note text;"]);
        let ours = graph_edits(&["COMMENT ON TABLE public.users IS 'People';"]);
        let merged = merge3(BASE, &ours, &theirs);
        assert_eq!(merged.conflicts, 0, "{}", merged.text);
        assert_eq!(
            merged.text.matches(GRAPH_EDITS_MARKER).count(),
            1,
            "{}",
            merged.text
        );
        assert!(
            merged
                .text
                .contains("ALTER TABLE public.orders ADD COLUMN note text;")
        );
        assert!(
            merged
                .text
                .contains("COMMENT ON TABLE public.users IS 'People';")
        );
        check_edits(&ours, &merged);
    }

    #[test]
    fn ours_and_theirs_changing_different_columns_both_apply() {
        // Ours alters email (appended), theirs adds a column: theirs' table,
        // then ours' change.
        let ours = format!("{BASE}\nALTER TABLE users ALTER COLUMN email SET DEFAULT '';\n");
        let theirs = BASE.replace(
            "email text NOT NULL\n",
            "email text NOT NULL,\n    plan text\n",
        );
        let merged = merge3(BASE, &ours, &theirs);
        assert_eq!(merged.conflicts, 0, "{}", merged.text);
        assert_eq!(
            merged.text,
            format!("{theirs}\nALTER TABLE users ALTER COLUMN email SET DEFAULT '';\n")
        );
        // Theirs changes email itself: a conflict.
        let theirs = BASE.replace("email text NOT NULL", "email varchar(200) NOT NULL");
        assert_eq!(merge3(BASE, &ours, &theirs).conflicts, 1);
    }

    #[test]
    fn comments_merge_one_by_one() {
        let base = format!(
            "{BASE}\nCOMMENT ON COLUMN users.email IS 'Login';\nCOMMENT ON TABLE orders IS 'Checkouts';\n"
        );
        let ours = base.replace("'Login'", "'Where receipts go'");
        let theirs = base.replace("'Checkouts'", "'One per checkout'");
        let merged = merge3(&base, &ours, &theirs);
        assert_eq!(merged.conflicts, 0);
        assert_eq!(
            merged.text,
            ours.replace("'Checkouts'", "'One per checkout'")
        );
        // The same comment, differently: a conflict.
        let theirs = base.replace("'Login'", "'Sign-in address'");
        let merged = merge3(&base, &ours, &theirs);
        assert_eq!(merged.conflicts, 1);
        assert!(
            merged.text.contains(
                "-- <<<<<<< yours\nCOMMENT ON COLUMN users.email IS 'Where receipts go';\n\
             -- ======= v2\nCOMMENT ON COLUMN users.email IS 'Sign-in address';\n-- >>>>>>> v2"
            ),
            "{}",
            merged.text
        );
    }

    #[test]
    fn enum_labels_added_on_both_sides_are_all_kept() {
        let base = "CREATE TYPE mood AS ENUM ('ok', 'sad');\n";
        let theirs = "CREATE TYPE mood AS ENUM ('ok', 'meh', 'sad');\n";

        // Ours added one with ALTER TYPE: theirs' type, then ours' label.
        let ours = format!("{base}\nALTER TYPE mood ADD VALUE 'happy';\n");
        let merged = merge3(base, &ours, theirs);
        assert_eq!(merged.conflicts, 0);
        assert_eq!(
            merged.text,
            format!("{theirs}\nALTER TYPE mood ADD VALUE 'happy';\n")
        );

        // Ours wrote it in the CREATE: ours, then theirs' label after its
        // neighbour.
        let ours = "CREATE TYPE mood AS ENUM ('ok', 'sad', 'happy');\n";
        let merged = merge3(base, ours, theirs);
        assert_eq!(merged.conflicts, 0);
        assert_eq!(
            merged.text,
            "CREATE TYPE mood AS ENUM ('ok', 'sad', 'happy');\n\
             ALTER TYPE public.mood ADD VALUE 'meh' AFTER 'ok';\n"
        );
        assert_eq!(merged.ancestor, theirs);

        // A label removed is not an addition: a conflict.
        let ours = "CREATE TYPE mood AS ENUM ('ok', 'happy');\n";
        assert_eq!(merge3(base, ours, theirs).conflicts, 1);
    }

    /// The case statement-level merging exists for: v2 was pushed (remus
    /// JSON), so theirs is `render_sql` output, nothing like the draft's
    /// hand-written text.
    #[test]
    fn a_pushed_version_changes_only_what_it_changed() {
        let ancestor = "\
-- People who sign in.
CREATE TABLE users (
    id bigint PRIMARY KEY,
    email text NOT NULL UNIQUE -- lowercased by the app
);

CREATE TABLE orders (
    id bigint PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id),
    total numeric(10, 2)
);

CREATE INDEX ON orders (user_id);
COMMENT ON TABLE orders IS 'One per checkout';
";
        let rendered = |extra: &str| {
            format!(
                "\
-- Schema DDL generated by remus from shop (PostgreSQL 17).
--
-- This is not a migration or a backup. Use pg_dump for that.

CREATE TABLE public.users (
    id bigint NOT NULL,
    email text NOT NULL,
    CONSTRAINT users_pkey PRIMARY KEY (id),
    CONSTRAINT users_email_key UNIQUE (email)
);

CREATE TABLE public.orders (
    id bigint NOT NULL,
    user_id bigint NOT NULL,
    total numeric(10,2),{extra}
    CONSTRAINT orders_pkey PRIMARY KEY (id)
);

ALTER TABLE public.orders ADD CONSTRAINT orders_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id);

CREATE INDEX orders_user_id_idx ON public.orders USING btree (user_id);

COMMENT ON TABLE public.orders IS 'One per checkout';
"
            )
        };
        let base_model = rendered("");
        let theirs = rendered("\n    status text DEFAULT 'new'::text NOT NULL,");
        let ours = ancestor
            .replace(
                "email text NOT NULL UNIQUE",
                "email text NOT NULL UNIQUE,\n    name text",
            )
            .replace("-- lowercased by the app\n", "\n")
            + "\nCREATE TABLE coupons (code text PRIMARY KEY);\n";

        let merged = merge(&Input {
            ancestor,
            ours: &ours,
            theirs: &theirs,
            base_model_sql: &base_model,
            theirs_model_sql: &theirs,
            ours_model_sql: None,
            label: "v2",
        })
        .unwrap();
        assert_eq!(merged.conflicts, 0, "{}", merged.text);
        // Only orders comes from theirs (with its foreign key, inline in
        // ours); users, the index, the comment and coupons are ours, word
        // for word.
        let orders_start = ours.find("CREATE TABLE orders").unwrap();
        let orders_end = ours[orders_start..].find(";\n").unwrap() + orders_start + 1;
        let expected = format!(
            "{}CREATE TABLE public.orders (
    id bigint NOT NULL,
    user_id bigint NOT NULL,
    total numeric(10,2),
    status text DEFAULT 'new'::text NOT NULL,
    CONSTRAINT orders_pkey PRIMARY KEY (id)
);
ALTER TABLE public.orders ADD CONSTRAINT orders_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id);{}",
            &ours[..orders_start],
            &ours[orders_end..]
        );
        assert_eq!(merged.text, expected);
        check_edits(&ours, &merged);

        // A draft that made the same change, its own way: no conflict when
        // its model says so.
        let ours_same = ancestor.replace(
            "total numeric(10, 2)\n",
            "total numeric(10, 2),\n    status text NOT NULL DEFAULT 'new'\n",
        );
        let input = Input {
            ancestor,
            ours: &ours_same,
            theirs: &theirs,
            base_model_sql: &base_model,
            theirs_model_sql: &theirs,
            ours_model_sql: Some(&theirs),
            label: "v2",
        };
        let merged = merge(&input).unwrap();
        assert_eq!(merged.conflicts, 0);
        assert_eq!(merged.text, ours_same);
        // Without its model, written differently: a conflict.
        let merged = merge(&Input {
            ours_model_sql: None,
            ..input
        })
        .unwrap();
        assert_eq!(merged.conflicts, 1);
    }

    #[test]
    fn new_objects_of_theirs_follow_what_precedes_them() {
        let theirs = BASE.replace(
            "\nCREATE TABLE orders",
            "\nCREATE TABLE plans (id bigint PRIMARY KEY);\n\nCREATE TABLE orders",
        );
        let ours = BASE.replace("CREATE TABLE orders", "-- Checkouts.\nCREATE TABLE orders");
        let merged = merge3(BASE, &ours, &theirs);
        assert_eq!(
            merged.text,
            ours.replace(
                ");\n\n-- Checkouts.",
                ");\n\nCREATE TABLE plans (id bigint PRIMARY KEY);\n\n-- Checkouts."
            )
        );
        check_edits(&ours, &merged);
    }

    #[test]
    fn a_newer_version_updates_the_blocks_it_concerns() {
        let ours = BASE.replace(
            "email text NOT NULL\n",
            "email text NOT NULL,\n    name text\n",
        );
        let v2 = BASE.replace(
            "email text NOT NULL\n",
            "email text NOT NULL,\n    name varchar(40)\n",
        );
        let first = merge3(BASE, &ours, &v2);
        assert_eq!(first.conflicts, 1);

        // v3 changes users again, while the block is still there: its theirs
        // side becomes v3's.
        let v3 = v2.replace("varchar(40)", "varchar(80)");
        let second = merge(&Input {
            ancestor: &first.ancestor,
            ours: &first.text,
            theirs: &v3,
            base_model_sql: &v2,
            theirs_model_sql: &v3,
            ours_model_sql: None,
            label: "v3",
        })
        .unwrap();
        assert_eq!(second.conflicts, 1);
        let (blocks, stray) = conflicts(&second.text);
        assert_eq!(stray, []);
        assert_eq!(blocks[0].label, "v3");
        assert!(second.text[blocks[0].theirs.clone()].contains("varchar(80)"));
        assert!(second.text[blocks[0].yours.clone()].contains("name text"));
        assert!(!second.text.contains("varchar(40)"));
        check_edits(&first.text, &second);

        // v3 changes orders only: the block stays as it is.
        let v3 = v2.replace(
            "REFERENCES users (id)\n",
            "REFERENCES users (id) ON DELETE CASCADE\n",
        );
        let second = merge(&Input {
            ancestor: &first.ancestor,
            ours: &first.text,
            theirs: &v3,
            base_model_sql: &v2,
            theirs_model_sql: &v3,
            ours_model_sql: None,
            label: "v3",
        })
        .unwrap();
        assert_eq!(second.conflicts, 1);
        assert!(second.text.contains("-- ======= v2\n"));
        assert!(second.text.contains("ON DELETE CASCADE"));
    }

    #[test]
    fn merging_again_with_no_newer_version_changes_nothing() {
        let ours = format!(
            "{}\nCREATE TABLE coupons (code text);\n",
            BASE.replace(
                "email text NOT NULL\n",
                "email text NOT NULL,\n    name text\n"
            )
        );
        let theirs = BASE.replace(
            "REFERENCES users (id)\n",
            "REFERENCES users (id),\n    total numeric\n",
        );
        let first = merge3(BASE, &ours, &theirs);
        let again = merge(&Input {
            ancestor: &first.ancestor,
            ours: &first.text,
            theirs: &theirs,
            base_model_sql: &theirs,
            theirs_model_sql: &theirs,
            ours_model_sql: None,
            label: "v2",
        })
        .unwrap();
        assert_eq!(again.edits, []);
        assert_eq!(again.ancestor, first.ancestor);
    }

    #[test]
    fn a_draft_that_does_not_parse_is_not_merged() {
        let err = merge(&Input {
            ancestor: BASE,
            ours: "CREATE TABLE users (id bigint",
            theirs: BASE,
            base_model_sql: BASE,
            theirs_model_sql: BASE,
            ours_model_sql: None,
            label: "v2",
        })
        .unwrap_err();
        let MergeError::Unparsable(diagnostics) = err else {
            panic!("{err:?}");
        };
        assert_eq!(diagnostics[0].code, Code::SyntaxError);
    }

    #[test]
    fn conflict_blocks_and_stray_markers_are_found() {
        let sql = "\
CREATE TABLE a (id int);
-- <<<<<<< yours (both can be kept)
ALTER TABLE a ADD COLUMN x int;
-- ======= v4
-- >>>>>>> v4
-- ======= v5
";
        let (blocks, stray) = conflicts(sql);
        assert_eq!(blocks.len(), 1);
        let block = &blocks[0];
        assert_eq!(&sql[block.yours.clone()], "ALTER TABLE a ADD COLUMN x int;");
        assert!(block.theirs.is_empty());
        assert_eq!(block.label, "v4");
        assert!(block.both);
        assert!(sql[block.range.clone()].starts_with("-- <<<<<<<"));
        assert!(sql[block.range.clone()].ends_with("-- >>>>>>> v4"));
        assert_eq!(stray.len(), 1);
        assert_eq!(&sql[stray[0].clone()], "-- ======= v5");
        let diagnostics = conflict_diagnostics(sql);
        assert_eq!(
            diagnostics
                .iter()
                .map(|d| d.span.unwrap().from.line)
                .collect::<Vec<_>>(),
            [2, 6]
        );
        assert_eq!(
            conflict_diagnostics("CREATE TABLE a (id int); -- <<< not a marker"),
            []
        );
    }
}

/// The cases of the review of #13, one test each (`sNN` names the case
/// there).
#[cfg(test)]
#[path = "merge_review.rs"]
mod review;
