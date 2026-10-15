//! The human side of a diff: one line per change and a one-sentence summary.
//!
//! Lines are derived from the structured diff alone (never from the schemas),
//! so a reversed diff re-describes itself. Names are in backticks and leave
//! out the `public.` schema; long values are shortened to one line.

use std::fmt::Write;

use crate::{
    model::{
        AttributeChange, AttributeDiff, Category, ColumnChange, ColumnDiff, CompositeChange,
        CompositeDiff, ConstraintChange, ConstraintDiff, ConstraintKind, Count, DomainChange,
        DomainDiff, EntityChange, EntityDiff, EntityKind, EnumChange, EnumDiff, ExtensionChange,
        ExtensionDiff, Generated, Identity, IndexChange, IndexDiff, LabelDiff, PartitionChange,
        PartitionDiff, PolicyChange, PolicyCommand, PolicyDiff, Relation, SchemaDiff, Status,
        Summary, TypeKind,
    },
    sql::squash,
};

/// Values longer than this are cut with an ellipsis in a line; the full text
/// is in the structured change.
const MAX_VALUE: usize = 80;

/// Shared words kept on each side of the difference when a long `X → Y` is
/// shortened, so the reader sees where in the value it is.
const CONTEXT: usize = 2;

impl SchemaDiff {
    /// Fills in everything derived: each object's lines and the summary.
    pub(crate) fn finish(&mut self) {
        for e in &mut self.entities {
            e.lines = entity_lines(e);
        }
        for e in &mut self.enums {
            e.lines = enum_lines(e);
        }
        for d in &mut self.domains {
            d.lines = domain_lines(d);
        }
        for c in &mut self.composites {
            c.lines = composite_lines(c);
        }
        for x in &mut self.extensions {
            x.lines = extension_lines(x);
        }
        let orphans = &mut self.orphan_partitions;
        orphans.lines = orphans
            .partitions
            .iter()
            .flat_map(partition_lines)
            .collect();
        self.summary = summary(self);
    }
}

fn display(schema: &str, name: &str) -> String {
    if schema == "public" {
        name.to_owned()
    } else {
        format!("{schema}.{name}")
    }
}

fn code(name: &str) -> String {
    format!("`{name}`")
}

fn flat(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cut(value: &str) -> String {
    if value.chars().count() <= MAX_VALUE {
        value.to_owned()
    } else {
        let kept: String = value.chars().take(MAX_VALUE - 1).collect();
        format!("{}…", kept.trim_end())
    }
}

/// A value on one line: whitespace runs collapsed, cut at [`MAX_VALUE`].
fn short(value: &str) -> String {
    cut(&flat(value))
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// `X → Y` on one line. When either side is long, the words both share at
/// the start and the end are elided (keeping [`CONTEXT`] of them), so cutting
/// can never hide the part that changed:
/// `…ON DELETE CASCADE → …ON DELETE RESTRICT`.
fn arrow(before: &str, after: &str) -> String {
    let (before, after) = (flat(before), flat(after));
    if before.chars().count() <= MAX_VALUE && after.chars().count() <= MAX_VALUE {
        return format!("{before} → {after}");
    }
    let b: Vec<&str> = before.split(' ').collect();
    let a: Vec<&str> = after.split(' ').collect();
    let prefix = b.iter().zip(&a).take_while(|(x, y)| x == y).count();
    let suffix = b[prefix..]
        .iter()
        .rev()
        .zip(a[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let start = prefix.saturating_sub(CONTEXT);
    let end = suffix.saturating_sub(CONTEXT);
    let side = |words: &[&str]| {
        let mut out = String::new();
        if start > 0 {
            out.push('…');
        }
        out.push_str(&words[start..words.len() - end].join(" "));
        if end > 0 {
            out.push('…');
        }
        cut(&out)
    };
    format!("{} → {}", side(&b), side(&a))
}

/// `subject: what X added`, `… X removed`, `… X → Y`, each value passed
/// through `show` first.
fn optional(
    subject: &str,
    what: &str,
    before: &Option<String>,
    after: &Option<String>,
    show: fn(&str) -> String,
) -> String {
    match (before, after) {
        (None, Some(a)) => format!("{subject}: {what} {} added", short(&show(a))),
        (Some(b), None) => format!("{subject}: {what} {} removed", short(&show(b))),
        (Some(b), Some(a)) => format!("{subject}: {what} {}", arrow(&show(b), &show(a))),
        (None, None) => format!("{subject}: {what} changed"),
    }
}

fn comment(subject: &str, before: &Option<String>, after: &Option<String>) -> String {
    optional(subject, "comment", before, after, literal)
}

fn not_null(subject: &str, after: bool) -> String {
    if after {
        format!("{subject}: now NOT NULL")
    } else {
        format!("{subject}: now nullable")
    }
}

fn entity_kind(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Table => "table",
        EntityKind::PartitionedTable => "partitioned table",
        EntityKind::View => "view",
        EntityKind::MaterializedView => "materialized view",
        EntityKind::ForeignTable => "foreign table",
    }
}

fn type_kind(kind: TypeKind) -> &'static str {
    match kind {
        TypeKind::Base => "base type",
        TypeKind::Enum => "enum",
        TypeKind::Domain => "domain",
        TypeKind::Composite => "composite type",
        TypeKind::Range => "range",
        TypeKind::Multirange => "multirange",
        TypeKind::Pseudo => "pseudo-type",
    }
}

fn identity(identity: Identity) -> &'static str {
    match identity {
        Identity::Always => "GENERATED ALWAYS AS IDENTITY",
        Identity::ByDefault => "GENERATED BY DEFAULT AS IDENTITY",
    }
}

fn generated(generated: Generated) -> &'static str {
    match generated {
        Generated::Stored => "STORED",
        Generated::Virtual => "VIRTUAL",
    }
}

fn constraint_kind(kind: ConstraintKind) -> &'static str {
    match kind {
        ConstraintKind::PrimaryKey => "primary key",
        ConstraintKind::Unique => "unique constraint",
        ConstraintKind::ForeignKey => "foreign key",
        ConstraintKind::Check => "check constraint",
        ConstraintKind::Exclusion => "exclusion constraint",
    }
}

fn command(command: PolicyCommand) -> &'static str {
    match command {
        PolicyCommand::All => "ALL",
        PolicyCommand::Select => "SELECT",
        PolicyCommand::Insert => "INSERT",
        PolicyCommand::Update => "UPDATE",
        PolicyCommand::Delete => "DELETE",
    }
}

fn roles(roles: &[String]) -> String {
    if roles.is_empty() {
        "PUBLIC".to_owned()
    } else {
        roles.join(", ")
    }
}

/// Positions are 0-based in the data and 1-based for people.
fn moved(subject: &str, before: usize, after: usize) -> String {
    format!(
        "{subject}: moved from position {} to {}",
        before + 1,
        after + 1
    )
}

fn entity_lines(e: &EntityDiff) -> Vec<String> {
    let table = display(&e.schema, &e.name);
    let me = code(&table);
    match e.status {
        Status::Added => return vec![format!("{} {me} added", entity_kind(e.kind))],
        Status::Removed => return vec![format!("{} {me} removed", entity_kind(e.kind))],
        Status::Changed | Status::Unchanged => {}
    }
    let mut out: Vec<String> = e.changes.iter().map(|c| entity_change(&me, c)).collect();
    for c in &e.columns {
        out.extend(column_lines(&table, c));
    }
    for c in &e.constraints {
        out.extend(constraint_lines(&me, c));
    }
    for i in &e.indexes {
        out.extend(index_lines(&me, i));
    }
    for p in &e.policies {
        out.extend(policy_lines(&me, p));
    }
    for p in &e.partitions {
        out.extend(partition_lines(p));
    }
    out
}

fn entity_change(me: &str, change: &EntityChange) -> String {
    match change {
        EntityChange::Kind { before, after } => {
            format!("{me}: {} → {}", entity_kind(*before), entity_kind(*after))
        }
        EntityChange::Comment { before, after } => comment(me, before, after),
        EntityChange::Rls { after: true, .. } => format!("{me}: row level security enabled"),
        EntityChange::Rls { after: false, .. } => format!("{me}: row level security disabled"),
        EntityChange::RlsForced { after: true, .. } => format!("{me}: row level security forced"),
        EntityChange::RlsForced { after: false, .. } => {
            format!("{me}: row level security no longer forced")
        }
        EntityChange::Unlogged { after: true, .. } => format!("{me}: now unlogged"),
        EntityChange::Unlogged { after: false, .. } => format!("{me}: now logged"),
        EntityChange::PartitionKey { before, after } => match (before, after) {
            (None, Some(a)) => format!("{me}: now partitioned by {}", short(a)),
            (Some(_), None) => format!("{me}: no longer partitioned"),
            _ => optional(me, "partition key", before, after, str::to_owned),
        },
        EntityChange::ViewDefinition { before, after } => match (before, after) {
            (None, Some(_)) => format!("{me}: definition added"),
            (Some(_), None) => format!("{me}: definition removed"),
            _ => format!("{me}: definition changed"),
        },
        EntityChange::DependsOn { before, after } => {
            let name = |r: &Relation| code(&display(&r.schema, &r.name));
            let reads: Vec<String> = after
                .iter()
                .filter(|r| !before.contains(r))
                .map(name)
                .collect();
            let unread: Vec<String> = before
                .iter()
                .filter(|r| !after.contains(r))
                .map(name)
                .collect();
            let mut parts = Vec::new();
            if !reads.is_empty() {
                parts.push(format!("now reads {}", reads.join(", ")));
            }
            if !unread.is_empty() {
                parts.push(format!("no longer reads {}", unread.join(", ")));
            }
            if parts.is_empty() {
                parts.push("dependencies changed".to_owned());
            }
            format!("{me}: {}", parts.join("; "))
        }
    }
}

fn column_lines(table: &str, c: &ColumnDiff) -> Vec<String> {
    let me = code(&format!("{table}.{}", c.name));
    match c.status {
        Status::Added => {
            let definition = c.definition.after.as_deref().unwrap_or_default();
            return vec![format!("column {me} added: {}", short(definition))];
        }
        Status::Removed => return vec![format!("column {me} removed")],
        Status::Changed | Status::Unchanged => {}
    }
    // A new type usually is a new kind of type too; one line says it.
    let retyped = c.changes.iter().any(|change| {
        matches!(
            change,
            ColumnChange::Type { .. } | ColumnChange::TypeName { .. }
        )
    });
    c.changes
        .iter()
        .filter(|change| !(retyped && matches!(change, ColumnChange::TypeKind { .. })))
        .map(|change| match change {
            ColumnChange::Type { before, after } | ColumnChange::TypeName { before, after } => {
                format!("{me}: type {}", arrow(before, after))
            }
            ColumnChange::TypeKind { before, after } => {
                format!(
                    "{me}: type kind {} → {}",
                    type_kind(*before),
                    type_kind(*after)
                )
            }
            ColumnChange::NotNull { after, .. } => not_null(&me, *after),
            ColumnChange::Default { before, after } => {
                optional(&me, "default", before, after, str::to_owned)
            }
            ColumnChange::Expression { before, after } => {
                optional(&me, "generation expression", before, after, str::to_owned)
            }
            ColumnChange::Identity { before, after } => match (before, after) {
                (None, Some(a)) => format!("{me}: now {}", identity(*a)),
                (Some(_), None) => format!("{me}: no longer an identity column"),
                (Some(b), Some(a)) => format!("{me}: {} → {}", identity(*b), identity(*a)),
                (None, None) => format!("{me}: identity changed"),
            },
            ColumnChange::Generated { before, after } => match (before, after) {
                (None, Some(a)) => format!("{me}: now generated ({})", generated(*a)),
                (Some(_), None) => format!("{me}: no longer generated"),
                (Some(b), Some(a)) => {
                    format!("{me}: generated {} → {}", generated(*b), generated(*a))
                }
                (None, None) => format!("{me}: generation changed"),
            },
            ColumnChange::Comment { before, after } => comment(&me, before, after),
            ColumnChange::Position { before, after } => moved(&me, *before, *after),
        })
        .collect()
}

fn constraint_lines(table: &str, c: &ConstraintDiff) -> Vec<String> {
    let me = format!("{} {} on {table}", constraint_kind(c.kind), code(&c.name));
    match c.status {
        Status::Added => {
            let definition = c.definition.after.as_deref().unwrap_or_default();
            return vec![format!("{me} added: {}", short(definition))];
        }
        Status::Removed => return vec![format!("{me} removed")],
        Status::Changed | Status::Unchanged => {}
    }
    c.changes
        .iter()
        .map(|change| match change {
            ConstraintChange::Kind { before, after } => format!(
                "{me}: {} → {}",
                constraint_kind(*before),
                constraint_kind(*after)
            ),
            ConstraintChange::Definition { before, after } => {
                format!("{me}: {}", definition_change(c.kind, before, after))
            }
            ConstraintChange::Comment { before, after } => comment(&me, before, after),
        })
        .collect()
}

/// A constraint's new definition, said in its own terms when the rest of it
/// stays: a foreign key on the same columns and target names the clauses
/// that moved (`on delete set null → cascade`), a check its condition
/// (`length(note) < 500 → length(note) <= 500`). Anything else is
/// `X → Y`.
fn definition_change(kind: ConstraintKind, before: &str, after: &str) -> String {
    match kind {
        ConstraintKind::ForeignKey => {
            if let (Some(b), Some(a)) = (ForeignKey::parse(before), ForeignKey::parse(after))
                && b.head == a.head
            {
                let moved = b.changes_to(&a);
                if !moved.is_empty() {
                    return moved.join(", ");
                }
            }
        }
        ConstraintKind::Check => {
            if let (Some(b), Some(a)) = (check_condition(before), check_condition(after)) {
                return arrow(b, a);
            }
        }
        _ => {}
    }
    arrow(before, after)
}

/// A foreign key as `pg_get_constraintdef` prints it, `FOREIGN KEY (a)
/// REFERENCES s.t(id)` then its clauses, with Postgres's defaults for the
/// ones it leaves out.
#[derive(Debug, PartialEq, Eq)]
struct ForeignKey<'a> {
    /// Up to the target's columns: what must stay for a clause-only change.
    head: &'a str,
    matching: String,
    on_update: String,
    on_delete: String,
    deferrable: bool,
    deferred: bool,
    valid: bool,
}

impl<'a> ForeignKey<'a> {
    fn parse(definition: &'a str) -> Option<Self> {
        let definition = definition.trim();
        let references = definition.find(" REFERENCES ")?;
        let close = references + definition[references..].find(')')? + 1;
        let mut fk = Self {
            head: &definition[..close],
            matching: "simple".into(),
            on_update: "no action".into(),
            on_delete: "no action".into(),
            deferrable: false,
            deferred: false,
            valid: true,
        };
        let words: Vec<&str> = definition[close..].split_whitespace().collect();
        let keyword = |w: &str| matches!(w, "ON" | "MATCH" | "DEFERRABLE" | "NOT" | "INITIALLY");
        let mut i = 0;
        while i < words.len() {
            match (words[i], words.get(i + 1).copied()) {
                ("MATCH", Some(kind)) => {
                    fk.matching = kind.to_lowercase();
                    i += 2;
                }
                ("ON", Some(event @ ("UPDATE" | "DELETE"))) => {
                    let end = (i + 2..words.len())
                        .find(|&j| keyword(words[j]))
                        .unwrap_or(words.len());
                    if end == i + 2 {
                        return None;
                    }
                    let action = words[i + 2..end].join(" ").to_lowercase();
                    if event == "UPDATE" {
                        fk.on_update = action;
                    } else {
                        fk.on_delete = action;
                    }
                    i = end;
                }
                ("DEFERRABLE", _) => {
                    fk.deferrable = true;
                    i += 1;
                }
                ("NOT", Some("DEFERRABLE")) => {
                    fk.deferrable = false;
                    i += 2;
                }
                ("NOT", Some("VALID")) => {
                    fk.valid = false;
                    i += 2;
                }
                ("INITIALLY", Some(when @ ("DEFERRED" | "IMMEDIATE"))) => {
                    fk.deferred = when == "DEFERRED";
                    i += 2;
                }
                // A clause this does not know: say the whole definition.
                _ => return None,
            }
        }
        Some(fk)
    }

    /// `on delete set null → cascade`, `now deferrable`…: what moved.
    fn changes_to(&self, new: &Self) -> Vec<String> {
        let mut out = Vec::new();
        for (what, before, after) in [
            ("match", &self.matching, &new.matching),
            ("on update", &self.on_update, &new.on_update),
            ("on delete", &self.on_delete, &new.on_delete),
        ] {
            if before != after {
                out.push(format!("{what} {before} → {after}"));
            }
        }
        if self.deferrable != new.deferrable {
            out.push(
                if new.deferrable {
                    "now deferrable"
                } else {
                    "no longer deferrable"
                }
                .into(),
            );
        }
        if self.deferred != new.deferred {
            out.push(
                if new.deferred {
                    "now initially deferred"
                } else {
                    "now initially immediate"
                }
                .into(),
            );
        }
        if self.valid != new.valid {
            out.push(
                if new.valid {
                    "now validated"
                } else {
                    "now NOT VALID"
                }
                .into(),
            );
        }
        out
    }
}

/// The condition of `CHECK ((a > 0))`: `a > 0`. `None` when anything
/// follows it (`NOT VALID`, `NO INHERIT`), which a change may be about.
fn check_condition(definition: &str) -> Option<&str> {
    let inner = definition
        .trim()
        .strip_prefix("CHECK (")?
        .strip_suffix(')')?;
    if !balanced(inner) {
        return None;
    }
    // Postgres wraps the expression once more: drop that pair too.
    match inner.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        Some(bare) if balanced(bare) => Some(bare),
        _ => Some(inner),
    }
}

/// Parentheses outside quotes pair up, none closing before it opens.
fn balanced(text: &str) -> bool {
    let mut depth = 0i32;
    let mut quote = None;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0 && quote.is_none()
}

fn index_lines(table: &str, i: &IndexDiff) -> Vec<String> {
    let me = format!("index {} on {table}", code(&i.name));
    match i.status {
        Status::Added => {
            let definition = i.definition.after.as_deref().unwrap_or_default();
            return vec![format!("{me} added: {}", short(definition))];
        }
        Status::Removed => return vec![format!("{me} removed")],
        Status::Changed | Status::Unchanged => {}
    }
    i.changes
        .iter()
        .map(|change| match change {
            IndexChange::Definition { before, after } => {
                format!("{me}: {}", arrow(before, after))
            }
        })
        .collect()
}

fn policy_lines(table: &str, p: &PolicyDiff) -> Vec<String> {
    let me = format!("policy {} on {table}", code(&p.name));
    match p.status {
        Status::Added => {
            let definition = p.definition.after.as_deref().unwrap_or_default();
            return vec![format!("{me} added: {}", short(definition))];
        }
        Status::Removed => return vec![format!("{me} removed")],
        Status::Changed | Status::Unchanged => {}
    }
    p.changes
        .iter()
        .map(|change| match change {
            PolicyChange::Command { before, after } => {
                format!("{me}: command {} → {}", command(*before), command(*after))
            }
            PolicyChange::Permissive { after: true, .. } => format!("{me}: now permissive"),
            PolicyChange::Permissive { after: false, .. } => format!("{me}: now restrictive"),
            PolicyChange::Roles { before, after } => {
                format!("{me}: roles {} → {}", roles(before), roles(after))
            }
            PolicyChange::Using { before, after } => {
                optional(&me, "USING", before, after, str::to_owned)
            }
            PolicyChange::Check { before, after } => {
                optional(&me, "WITH CHECK", before, after, str::to_owned)
            }
        })
        .collect()
}

/// Partitions name their own parent, which under a sub-partitioned tree is not
/// the entity they are listed under.
fn partition_lines(p: &PartitionDiff) -> Vec<String> {
    let parent = |r: &Relation| code(&display(&r.schema, &r.name));
    let me = format!(
        "partition {} of {}",
        code(&display(&p.schema, &p.name)),
        parent(&p.parent)
    );
    match p.status {
        Status::Added => {
            let definition = p.definition.after.as_deref().unwrap_or_default();
            return vec![format!("{me} added: {}", short(definition))];
        }
        Status::Removed => return vec![format!("{me} removed")],
        Status::Changed | Status::Unchanged => {}
    }
    p.changes
        .iter()
        .map(|change| match change {
            PartitionChange::Parent { before, .. } => {
                format!("{me}: moved from {}", parent(before))
            }
            PartitionChange::Bound { before, after } => {
                let bound = |b: &Option<String>| b.as_deref().unwrap_or("DEFAULT").to_owned();
                format!("{me}: bound {}", arrow(&bound(before), &bound(after)))
            }
            PartitionChange::PartitionKey { before, after } => {
                let key = |k: &Option<String>| k.as_deref().unwrap_or("none").to_owned();
                format!("{me}: partitioned by {}", arrow(&key(before), &key(after)))
            }
        })
        .collect()
}

fn enum_lines(e: &EnumDiff) -> Vec<String> {
    let me = code(&display(&e.schema, &e.name));
    match e.status {
        Status::Added => return vec![format!("enum {me} added")],
        Status::Removed => return vec![format!("enum {me} removed")],
        Status::Changed | Status::Unchanged => {}
    }
    let mut out: Vec<String> = e
        .changes
        .iter()
        .map(|change| match change {
            EnumChange::Comment { before, after } => comment(&me, before, after),
        })
        .collect();
    out.extend(e.labels.iter().map(|l| label_line(&me, l)));
    out
}

fn label_line(me: &str, l: &LabelDiff) -> String {
    let label = short(&literal(&l.label));
    match (l.status, l.position.before, l.position.after) {
        (Status::Added, ..) => format!("{me}: label {label} added"),
        (Status::Removed, ..) => format!("{me}: label {label} removed"),
        (_, Some(before), Some(after)) => format!(
            "{me}: label {label} moved from position {} to {}",
            before + 1,
            after + 1
        ),
        _ => format!("{me}: label {label} changed"),
    }
}

fn domain_lines(d: &DomainDiff) -> Vec<String> {
    let me = code(&display(&d.schema, &d.name));
    match d.status {
        Status::Added => return vec![format!("domain {me} added")],
        Status::Removed => return vec![format!("domain {me} removed")],
        Status::Changed | Status::Unchanged => {}
    }
    let mut out = Vec::new();
    for change in &d.changes {
        match change {
            DomainChange::BaseType { before, after } => {
                out.push(format!("{me}: base type {}", arrow(before, after)));
            }
            DomainChange::NotNull { after, .. } => out.push(not_null(&me, *after)),
            DomainChange::Default { before, after } => {
                out.push(optional(&me, "default", before, after, str::to_owned));
            }
            DomainChange::Constraints { before, after } => {
                let start = out.len();
                let squashed = |checks: &[String]| -> Vec<String> {
                    checks.iter().map(|c| squash(c)).collect()
                };
                let (old, new) = (squashed(before), squashed(after));
                for check in after.iter().filter(|c| !old.contains(&squash(c))) {
                    out.push(format!("{me}: check added: {}", short(check)));
                }
                for check in before.iter().filter(|c| !new.contains(&squash(c))) {
                    out.push(format!("{me}: check removed: {}", short(check)));
                }
                if out.len() == start {
                    out.push(format!("{me}: checks changed"));
                }
            }
            DomainChange::Comment { before, after } => out.push(comment(&me, before, after)),
        }
    }
    out
}

fn composite_lines(c: &CompositeDiff) -> Vec<String> {
    let name = display(&c.schema, &c.name);
    let me = code(&name);
    match c.status {
        Status::Added => return vec![format!("composite type {me} added")],
        Status::Removed => return vec![format!("composite type {me} removed")],
        Status::Changed | Status::Unchanged => {}
    }
    let mut out: Vec<String> = c
        .changes
        .iter()
        .map(|change| match change {
            CompositeChange::Comment { before, after } => comment(&me, before, after),
        })
        .collect();
    for a in &c.attributes {
        out.extend(attribute_lines(&name, a));
    }
    out
}

fn attribute_lines(composite: &str, a: &AttributeDiff) -> Vec<String> {
    let me = code(&format!("{composite}.{}", a.name));
    match a.status {
        Status::Added => {
            let definition = a.definition.after.as_deref().unwrap_or_default();
            return vec![format!("attribute {me} added: {}", short(definition))];
        }
        Status::Removed => return vec![format!("attribute {me} removed")],
        Status::Changed | Status::Unchanged => {}
    }
    a.changes
        .iter()
        .map(|change| match change {
            AttributeChange::Type { before, after } => {
                format!("{me}: type {}", arrow(before, after))
            }
            AttributeChange::Position { before, after } => moved(&me, *before, *after),
        })
        .collect()
}

fn extension_lines(x: &ExtensionDiff) -> Vec<String> {
    let me = code(&x.name);
    match x.status {
        Status::Added => return vec![format!("extension {me} added")],
        Status::Removed => return vec![format!("extension {me} removed")],
        Status::Changed | Status::Unchanged => {}
    }
    x.changes
        .iter()
        .map(|change| match change {
            ExtensionChange::Schema { before, after } => {
                format!("{me}: schema {before} → {after}")
            }
            ExtensionChange::Version { before, after } => {
                format!("{me}: version {before} → {after}")
            }
        })
        .collect()
}

fn category(kind: EntityKind) -> Category {
    match kind {
        EntityKind::Table | EntityKind::PartitionedTable => Category::Table,
        EntityKind::View => Category::View,
        EntityKind::MaterializedView => Category::MaterializedView,
        EntityKind::ForeignTable => Category::ForeignTable,
    }
}

fn nouns(category: Category) -> (&'static str, &'static str) {
    match category {
        Category::Table => ("table", "tables"),
        Category::View => ("view", "views"),
        Category::MaterializedView => ("materialized view", "materialized views"),
        Category::ForeignTable => ("foreign table", "foreign tables"),
        Category::Partition => ("partition", "partitions"),
        Category::Enum => ("enum", "enums"),
        Category::Domain => ("domain", "domains"),
        Category::Composite => ("composite type", "composite types"),
        Category::Extension => ("extension", "extensions"),
    }
}

const CATEGORIES: [Category; 9] = [
    Category::Table,
    Category::View,
    Category::MaterializedView,
    Category::ForeignTable,
    Category::Partition,
    Category::Enum,
    Category::Domain,
    Category::Composite,
    Category::Extension,
];

fn summary(diff: &SchemaDiff) -> Summary {
    let mut counts: Vec<Count> = CATEGORIES
        .iter()
        .map(|&category| Count {
            category,
            added: 0,
            removed: 0,
            changed: 0,
        })
        .collect();
    let mut tally = |category: Category, status: Status| {
        let Some(count) = counts.iter_mut().find(|c| c.category == category) else {
            return;
        };
        match status {
            Status::Added => count.added += 1,
            Status::Removed => count.removed += 1,
            Status::Changed => count.changed += 1,
            Status::Unchanged => {}
        }
    };
    for e in &diff.entities {
        tally(category(e.kind), e.status);
        for p in &e.partitions {
            tally(Category::Partition, p.status);
        }
    }
    for p in &diff.orphan_partitions.partitions {
        tally(Category::Partition, p.status);
    }
    for e in &diff.enums {
        tally(Category::Enum, e.status);
    }
    for d in &diff.domains {
        tally(Category::Domain, d.status);
    }
    for c in &diff.composites {
        tally(Category::Composite, c.status);
    }
    for x in &diff.extensions {
        tally(Category::Extension, x.status);
    }
    counts.retain(|c| c.added + c.removed + c.changed > 0);
    let text = if counts.is_empty() {
        "no changes".to_owned()
    } else {
        counts.iter().map(sentence).collect::<Vec<_>>().join("; ")
    };
    Summary { counts, text }
}

/// `3 tables added, 1 changed`: the noun goes with the first number.
fn sentence(count: &Count) -> String {
    let (one, many) = nouns(count.category);
    let mut parts = [
        (count.added, "added"),
        (count.changed, "changed"),
        (count.removed, "removed"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0);
    let Some((n, verb)) = parts.next() else {
        return String::new();
    };
    let noun = if n == 1 { one } else { many };
    let mut out = format!("{n} {noun} {verb}");
    for (n, verb) in parts {
        let _ = write!(out, ", {n} {verb}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FK: &str = "FOREIGN KEY (author_id) REFERENCES public.users(id)";

    fn fk(clauses: &str) -> String {
        format!("{FK}{clauses}")
    }

    #[test]
    fn a_foreign_key_on_the_same_columns_says_which_clauses_moved() {
        let change =
            |b: &str, a: &str| definition_change(ConstraintKind::ForeignKey, &fk(b), &fk(a));
        assert_eq!(
            change(" ON DELETE SET NULL", " ON DELETE CASCADE"),
            "on delete set null → cascade"
        );
        // Postgres leaves the defaults out.
        assert_eq!(
            change("", " ON UPDATE CASCADE"),
            "on update no action → cascade"
        );
        assert_eq!(
            change(
                " MATCH FULL ON DELETE RESTRICT",
                " ON DELETE SET DEFAULT DEFERRABLE"
            ),
            "match full → simple, on delete restrict → set default, now deferrable"
        );
        assert_eq!(
            change(" DEFERRABLE INITIALLY DEFERRED", " NOT VALID"),
            "no longer deferrable, now initially immediate, now NOT VALID"
        );
        assert_eq!(
            change(" ON DELETE SET NULL (author_id)", " ON DELETE SET NULL"),
            "on delete set null (author_id) → set null"
        );
    }

    #[test]
    fn a_foreign_key_to_another_target_says_both_definitions() {
        let before = fk(" ON DELETE CASCADE");
        let after = "FOREIGN KEY (author_id) REFERENCES public.people(id) ON DELETE CASCADE";
        assert_eq!(
            definition_change(ConstraintKind::ForeignKey, &before, after),
            format!("{before} → {after}")
        );
        // A clause it does not know is no reason to guess.
        assert_eq!(
            definition_change(ConstraintKind::ForeignKey, &fk(" ENFORCED"), &fk("")),
            format!("{} → {FK}", fk(" ENFORCED"))
        );
    }

    #[test]
    fn a_check_says_its_condition() {
        let change = |b: &str, a: &str| definition_change(ConstraintKind::Check, b, a);
        assert_eq!(
            change(
                "CHECK ((length(note) < 500))",
                "CHECK ((length(note) <= 500))"
            ),
            "length(note) < 500 → length(note) <= 500"
        );
        // Two groups are not one pair of parentheses to drop.
        assert_eq!(
            change(
                "CHECK (((a > 0) AND (b > 0)))",
                "CHECK ((a > 0) AND (b > 1))"
            ),
            "(a > 0) AND (b > 0) → (a > 0) AND (b > 1)"
        );
        // Becoming NOT VALID is part of the change: both said whole.
        assert_eq!(
            change("CHECK ((a > 0))", "CHECK ((a > 0)) NOT VALID"),
            "CHECK ((a > 0)) → CHECK ((a > 0)) NOT VALID"
        );
        assert_eq!(
            check_condition("CHECK ((note <> ')'::text))"),
            Some("note <> ')'::text")
        );
    }
}
