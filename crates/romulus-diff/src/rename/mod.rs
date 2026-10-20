//! Renames between two models.
//!
//! The diff matches objects by name, so a rename reads as a removal and an
//! addition. A migration should rename instead (`ALTER … RENAME` keeps the
//! rows), and [`resolve`] says which pairs to treat as renames:
//!
//! - **known** ones ([`RenameChoices::known`]): the `ALTER … RENAME` and
//!   `ALTER … SET SCHEMA` statements a draft's text runs (graph edits append
//!   them, a migration written by hand has them), or the renames a version
//!   was published with. They are certain ([`Confidence::Certain`]) and
//!   always applied; renames that exchange names (a swap, given as one pair
//!   each way) apply together, as a permutation;
//! - **detected** ones: a removed and an added object alike enough to be
//!   one object renamed, scored ([`Candidate::score`]) and applied by
//!   default when the score says [`Confidence::High`], left off when it
//!   says [`Confidence::Medium`]; [`RenameChoices::decisions`] turns them
//!   on or off by [`Rename::key`]. Two candidates about as good as each
//!   other for one object are never chosen between: both are offered, off,
//!   as rivals ([`Candidate::rivals`]).
//!
//! Detection runs in levels, each on the old model with the renames chosen
//! so far applied: types, tables, their columns, enum labels and composite
//! attributes, the sequences serial columns draw from, constraints,
//! indexes, policies, partitions, views. So a constraint whose definition
//! names a renamed column compares equal to its new self, and its own
//! rename (Postgres keeps `users_email_key` when the column becomes
//! `email_address`; a fresh `CREATE TABLE` names it `users_email_address_key`)
//! follows the column's ([`Candidate::follows`]): applied with it, no
//! choice of its own.
//!
//! [`Resolved::schema`] is the old model as Postgres has it once the renames
//! ran: names changed, and every dependent object printed as Postgres
//! prints it after a rename (`sql.rs`): indexes, keys, checks, policies,
//! views, defaults and casts follow by OID. The migration is then planned
//! from it.
//!
//! # Scores
//!
//! Out of 100. A pair is [`Confidence::High`] when its structure matches
//! exactly, it scores at least 72, **and something besides structure and
//! place says it is the same object**: a similar name (0.5 or more), a
//! comment both share, foreign keys that now reference it (tables), columns
//! that switch to it (types). Same-shaped data (a `password_hash text`
//! where a `bio text` now stands, a label in the place of another) is never
//! renamed by default: a wrong rename puts data under the wrong name,
//! silently, where a wrong drop and add at least says it loses it. It is
//! [`Confidence::Medium`] then, offered and off, as is anything from 45;
//! below that it is not offered. Keys, indexes, policies and views (no
//! data of their own under the name) need no such evidence.
//!
//! - **Columns** (within one table): the same type, nullability, default,
//!   identity, generation and keys (every constraint and index naming the
//!   column, compared with the column's name blanked) is 60, and 15 more
//!   in the same place (the same position, or the same place between the
//!   columns kept on both sides). Not alike but in the same place with a
//!   compatible type (`varchar(50)` and `text`, `integer` and `bigint`) is
//!   35 + 15: medium at best.
//! - **Tables** (within one schema, of the same kind): the same columns
//!   (names and types), keys and indexes, compared with the table's name
//!   blanked, is 75 (60 for a single column); most of the columns (from
//!   three quarters) 30 to 52, medium at best. 10 more when a foreign key
//!   kept on both sides now references the new name.
//! - **Types**: enums with the same labels, domains with the same base,
//!   default and checks, composites with the same attributes: 75; 20 more
//!   when columns kept on both sides switch from one to the other; an enum
//!   that only gained labels, or kept them all in place but one, 55 (high
//!   when its columns switch to it).
//! - **Enum labels**: one label out and one in at the same place 75;
//!   several between the same kept labels, paired in order, 50.
//! - **Composite attributes**: as columns, on their type alone.
//! - **Constraints, indexes, policies, views, partitions**: the same
//!   definition (bound and parent for a partition) under another name, 75.
//! - **Sequences** a serial column draws from: renamed when the column
//!   draws from another sequence that the old side did not have, 80.
//! - **Moves** (`ALTER … SET SCHEMA`): a table of the same name and the
//!   same columns, keys and indexes in another schema, 75; the same name is
//!   its evidence.
//!
//! The names add up to 25 (their similarity: shared words, or the edit
//! distance, whichever is closer), and a comment both sides share 10.
//! Pairs whose scores are within 5 of each other for one object are rivals.

mod apply;
mod detect;
mod known;
pub(crate) mod sql;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use remus_core::Schema;
use serde::{Deserialize, Serialize};

/// What is renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RenameKind {
    /// A table or a partitioned table.
    Table,
    View,
    MaterializedView,
    /// A partition (a table attached to another).
    Partition,
    Column,
    Constraint,
    Index,
    Policy,
    /// The sequence a serial column draws from.
    Sequence,
    Enum,
    Domain,
    Composite,
    /// One label of an enum (`ALTER TYPE … RENAME VALUE`).
    EnumValue,
    /// One attribute of a composite type.
    Attribute,
}

impl RenameKind {
    /// The word in keys and sentences: `table`, `enum_value`.
    pub fn word(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::View => "view",
            Self::MaterializedView => "materialized_view",
            Self::Partition => "partition",
            Self::Column => "column",
            Self::Constraint => "constraint",
            Self::Index => "index",
            Self::Policy => "policy",
            Self::Sequence => "sequence",
            Self::Enum => "enum",
            Self::Domain => "domain",
            Self::Composite => "composite",
            Self::EnumValue => "enum_value",
            Self::Attribute => "attribute",
        }
    }

    /// The kind [`RenameKind::word`] names.
    pub fn from_word(word: &str) -> Option<Self> {
        Some(match word {
            "table" => Self::Table,
            "view" => Self::View,
            "materialized_view" => Self::MaterializedView,
            "partition" => Self::Partition,
            "column" => Self::Column,
            "constraint" => Self::Constraint,
            "index" => Self::Index,
            "policy" => Self::Policy,
            "sequence" => Self::Sequence,
            "enum" => Self::Enum,
            "domain" => Self::Domain,
            "composite" => Self::Composite,
            "enum_value" => Self::EnumValue,
            "attribute" => Self::Attribute,
            _ => return None,
        })
    }

    /// Relations share one namespace per schema (`pg_class`): a table can
    /// be renamed with `ALTER INDEX` as well as an index can with `ALTER
    /// TABLE`. Known renames of one kind match an object of any kind of
    /// the same family.
    pub(crate) fn is_relation(self) -> bool {
        matches!(
            self,
            Self::Table
                | Self::View
                | Self::MaterializedView
                | Self::Partition
                | Self::Index
                | Self::Sequence
        )
    }

    pub(crate) fn is_type(self) -> bool {
        matches!(self, Self::Enum | Self::Domain | Self::Composite)
    }
}

/// One rename: `from` becomes `to`, in `schema`, under `parent`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Rename {
    pub kind: RenameKind,
    pub schema: String,
    /// The table of a column, constraint, index, policy or sequence; the
    /// type of an enum value or an attribute; `null` for the rest. Named
    /// as it is when the rename runs: after its own rename, if any.
    pub parent: Option<String>,
    pub from: String,
    pub to: String,
    /// The schema a table, view or type moves to (`ALTER … SET SCHEMA`),
    /// when it moves; `null` otherwise. A move may come with a rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_schema: Option<String>,
}

impl Rename {
    /// What identifies it across requests, for [`RenameChoices::decisions`]:
    /// `column/public/users/email/email_address`, each part escaped
    /// (`\` and `/` with a backslash).
    pub fn key(&self) -> String {
        let escape = |s: &str| s.replace('\\', "\\\\").replace('/', "\\/");
        let mut key = format!(
            "{}/{}/{}/{}/{}",
            self.kind.word(),
            escape(&self.schema),
            escape(self.parent.as_deref().unwrap_or("")),
            escape(&self.from),
            escape(&self.to)
        );
        if let Some(to_schema) = &self.to_schema {
            key.push('/');
            key.push_str(&escape(to_schema));
        }
        key
    }

    /// The rename a [`Rename::key`] names, or `None` for text that is not
    /// one.
    pub fn from_key(key: &str) -> Option<Self> {
        let mut parts: Vec<String> = vec![String::new()];
        let mut chars = key.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => parts.last_mut()?.push(chars.next()?),
                '/' => parts.push(String::new()),
                c => parts.last_mut()?.push(c),
            }
        }
        let (kind, schema, parent, from, to, to_schema) = match parts.as_slice() {
            [k, s, p, f, t] => (k, s, p, f, t, None),
            [k, s, p, f, t, m] => (k, s, p, f, t, Some(m.clone())),
            _ => return None,
        };
        Some(Self {
            kind: RenameKind::from_word(kind)?,
            schema: schema.clone(),
            parent: (!parent.is_empty()).then(|| parent.clone()),
            from: from.clone(),
            to: to.clone(),
            to_schema,
        })
    }

    /// This rename as it reads after `later` ran (a rename after it): its
    /// parent renamed or moved, what it leads to renamed or moved again.
    /// What it comes from is gone by then, so a later rename of that name
    /// is about something else.
    #[must_use]
    pub fn after(&self, later: &Rename) -> Self {
        let mut out = self.clone();
        let parent_kind = |k: RenameKind| match out.kind {
            RenameKind::EnumValue | RenameKind::Attribute => k.is_type(),
            _ => k.is_relation(),
        };
        if later.parent.is_none()
            && out.parent.as_deref() == Some(later.from.as_str())
            && out.schema == later.schema
            && parent_kind(later.kind)
        {
            out.parent = Some(later.to.clone());
            later.final_schema().clone_into(&mut out.schema);
        }
        let family = |k: RenameKind| {
            k == out.kind
                || (k.is_relation() && out.kind.is_relation())
                || (k.is_type() && out.kind.is_type())
        };
        if family(later.kind)
            && later.parent == out.parent
            && later.schema == out.final_schema()
            && later.from == out.to
        {
            later.to.clone_into(&mut out.to);
            let to_schema = later.final_schema();
            out.to_schema = (to_schema != out.schema).then(|| to_schema.to_owned());
        }
        out
    }

    /// The schema it ends in.
    pub fn final_schema(&self) -> &str {
        self.to_schema.as_deref().unwrap_or(&self.schema)
    }

    /// The rename back: `to` becomes `from` (under the same parent, which
    /// a down migration renames back after its children).
    #[must_use]
    pub fn reversed(&self) -> Self {
        Self {
            from: self.to.clone(),
            to: self.from.clone(),
            schema: self.final_schema().to_owned(),
            to_schema: self.to_schema.as_ref().map(|_| self.schema.clone()),
            ..self.clone()
        }
    }

    /// `public.users.email → email_address`, for sentences.
    pub fn describe(&self) -> String {
        if let Some(to_schema) = &self.to_schema {
            return format!("{}.{} → {to_schema}.{}", self.schema, self.from, self.to);
        }
        let at = match &self.parent {
            Some(parent) => format!("{}.{parent}.", self.schema),
            None => format!("{}.", self.schema),
        };
        format!("{at}{} → {}", self.from, self.to)
    }
}

/// How sure the rename is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = RenameConfidence))]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Known: an `ALTER … RENAME` ran, or a person chose it at publish.
    /// Always applied.
    Certain,
    /// Alike in structure and in name or place: applied unless turned off.
    High,
    /// Plausible: offered, off unless turned on.
    Medium,
}

/// A rename, known or detected, and whether the migration applies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema), schema(as = RenameCandidate))]
pub struct Candidate {
    /// [`Rename::key`]: what [`RenameChoices::decisions`] turn on or off.
    pub key: String,
    pub rename: Rename,
    pub confidence: Confidence,
    /// Out of 100 (see the module documentation); 100 when known.
    pub score: u8,
    pub applied: bool,
    /// The candidate this one comes with (a constraint renamed with its
    /// column, a table's sequence with the table): applied with it, no
    /// choice of its own.
    pub follows: Option<String>,
    /// Candidates about as good for one of the same objects: applying this
    /// one leaves them off. Never applied by default.
    pub rivals: Vec<String>,
    /// Why it looks like a rename, as facts: `Same type, nullability,
    /// default and keys; same place; similar name.`
    pub reason: String,
}

/// What [`resolve`] goes by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct RenameChoices {
    /// Renames known to have happened, in the order they ran, each named
    /// as it was when it ran (a column under its table's name of that
    /// moment). Chains are followed (`a` → `b` then `b` → `c` is `a` →
    /// `c`); one about something the old model lacks, or whose result the
    /// new one lacks, is ignored.
    #[serde(default)]
    pub known: Vec<Rename>,
    /// Where `known` starts another batch (indexes into it, ascending): a
    /// range of versions, each version's renames applied as a whole, its
    /// swaps with it, before the next version's (a swap held back to the
    /// end would let a later rename land on the wrong object). Empty: one
    /// batch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub batches: Vec<usize>,
    /// Detected renames turned on (`true`) or off, by [`Rename::key`].
    /// Keys that match no candidate are ignored.
    #[serde(default)]
    pub decisions: BTreeMap<String, bool>,
    /// Look for renames beyond the known ones (the default). Off for a
    /// down migration, which mirrors its up migration's renames exactly.
    #[serde(default = "yes")]
    pub detect: bool,
}

fn yes() -> bool {
    true
}

impl Default for RenameChoices {
    fn default() -> Self {
        Self {
            known: Vec::new(),
            batches: Vec::new(),
            decisions: BTreeMap::new(),
            detect: true,
        }
    }
}

impl RenameChoices {
    /// Exactly these renames, nothing detected: what mirrors a migration
    /// that applied `renames` (in reverse: the down migration).
    pub fn exactly(renames: Vec<Rename>) -> Self {
        Self {
            known: renames,
            batches: Vec::new(),
            decisions: BTreeMap::new(),
            detect: false,
        }
    }

    /// The choices that undo a migration which applied `renames` (in their
    /// order): the same renames reversed, last first.
    pub fn reverse_of(renames: &[Rename]) -> Self {
        Self::exactly(renames.iter().rev().map(Rename::reversed).collect())
    }

    /// The choices of consecutive steps (versions, oldest first) as one:
    /// each step's known renames a batch of their own; each step's
    /// decisions as they read after the later steps' renames (a column's
    /// table renamed later, what it was left off for renamed again), and
    /// off for a key two steps decided differently (one turned it off on
    /// purpose: detection never turns it back on).
    pub fn compose(steps: impl IntoIterator<Item = RenameChoices>) -> Self {
        let steps: Vec<RenameChoices> = steps.into_iter().collect();
        let mut out = Self::default();
        let mut conflicting: BTreeSet<String> = BTreeSet::new();
        for (n, step) in steps.iter().enumerate() {
            let offset = out.known.len();
            if offset > 0 && !step.known.is_empty() {
                out.batches.push(offset);
            }
            out.batches.extend(step.batches.iter().map(|b| b + offset));
            out.known.extend(step.known.iter().cloned());
            let later: Vec<&Rename> = steps[n + 1..].iter().flat_map(|s| &s.known).collect();
            for (key, &on) in &step.decisions {
                let key = match Rename::from_key(key) {
                    Some(rename) => later.iter().fold(rename, |r, l| r.after(l)).key(),
                    None => key.clone(),
                };
                if out
                    .decisions
                    .insert(key.clone(), on)
                    .is_some_and(|was| was != on)
                {
                    conflicting.insert(key);
                }
            }
        }
        for key in conflicting {
            out.decisions.insert(key, false);
        }
        out
    }

    /// The way back: the known renames reversed, last first, batch by
    /// batch (the last one first); the decisions, about the way forward,
    /// dropped.
    #[must_use]
    pub fn reversed(&self) -> Self {
        let n = self.known.len();
        let mut batches: Vec<usize> = self
            .batches
            .iter()
            .filter(|&&b| b > 0 && b < n)
            .map(|b| n - b)
            .collect();
        batches.sort_unstable();
        batches.dedup();
        Self {
            known: self.known.iter().rev().map(Rename::reversed).collect(),
            batches,
            decisions: BTreeMap::new(),
            detect: self.detect,
        }
    }
}

/// What [`resolve`] found and chose.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// Known ones first, then detected ones by level (see the module
    /// documentation), applied or not.
    pub candidates: Vec<Candidate>,
    /// The renames applied, each from the old name to the final one, in an
    /// order they can run in (types, relations, then what belongs to them).
    pub renames: Vec<Rename>,
    /// What to run for them: [`Resolved::renames`], except that names two
    /// objects swap go through a temporary one first.
    pub steps: Vec<Rename>,
    /// The old model once the renames ran, as Postgres would describe it.
    pub schema: Schema,
    /// Views whose definition reads a renamed column in a way the rewrite
    /// cannot follow (a `UNION`, a subquery): build them again rather than
    /// trust a guess of how Postgres prints them now.
    pub rebuild_views: BTreeSet<(String, String)>,
}

impl Resolved {
    /// No rename at all.
    pub fn is_empty(&self) -> bool {
        self.renames.is_empty()
    }
}

/// The renames from `old` to `new` (see the module documentation), and the
/// old model with the applied ones done. Give both sides as
/// [`crate::normalize`] leaves them.
pub fn resolve(old: &Schema, new: &Schema, choices: &RenameChoices) -> Resolved {
    let mut resolved = Resolved {
        candidates: Vec::new(),
        renames: Vec::new(),
        steps: Vec::new(),
        schema: old.clone(),
        rebuild_views: BTreeSet::new(),
    };
    let known = known::resolve(old, new, &choices.known, &choices.batches);
    for step in &known.steps {
        apply::apply(&mut resolved.schema, step, &mut resolved.rebuild_views);
    }
    resolved.steps.extend(known.steps);
    for rename in known.renames {
        resolved.candidates.push(Candidate {
            key: rename.key(),
            rename: rename.clone(),
            confidence: Confidence::Certain,
            score: 100,
            applied: true,
            follows: None,
            rivals: Vec::new(),
            reason: "Renamed with ALTER … RENAME.".to_owned(),
        });
        resolved.renames.push(rename);
    }
    if choices.detect {
        detect::detect(&mut resolved, new, &choices.decisions);
    }
    resolved
}
