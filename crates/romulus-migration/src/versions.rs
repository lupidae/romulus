//! What a migration's statements may use that older Postgres majors lack:
//! the guards of [`Options::major`](crate::Options::major).
//!
//! The planner avoids what it can (a generated column's new expression is
//! dropped and added again before 17, where `SET EXPRESSION` is missing);
//! what the new model itself needs is kept, noted on its statement and
//! warned about ([`WarningCode::NewerPostgres`](crate::WarningCode)): the
//! model came from a newer major (a version compiled on 18, a push from a
//! newer server) than the database is now on.

use crate::Kind;

/// Syntax that came with a Postgres major.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Feature {
    /// The major that brought it.
    pub since: u32,
    /// What it is, for notes and warnings.
    pub what: &'static str,
}

/// Version-specific syntax found in a statement's code (outside string
/// literals, quoted identifiers and comments), uppercase, words joined by
/// one space.
const SYNTAX: &[(&str, Feature)] = &[
    (
        "NULLS NOT DISTINCT",
        Feature {
            since: 15,
            what: "NULLS NOT DISTINCT",
        },
    ),
    (
        "ON DELETE SET NULL (",
        Feature {
            since: 15,
            what: "a column list in ON DELETE SET NULL",
        },
    ),
    (
        "ON DELETE SET DEFAULT (",
        Feature {
            since: 15,
            what: "a column list in ON DELETE SET DEFAULT",
        },
    ),
    (
        "SET EXPRESSION AS",
        Feature {
            since: 17,
            what: "ALTER COLUMN … SET EXPRESSION",
        },
    ),
    (
        ") VIRTUAL",
        Feature {
            since: 18,
            what: "virtual generated columns",
        },
    ),
    (
        "NOT ENFORCED",
        Feature {
            since: 18,
            what: "NOT ENFORCED constraints",
        },
    ),
    (
        "WITHOUT OVERLAPS",
        Feature {
            since: 18,
            what: "temporal keys (WITHOUT OVERLAPS)",
        },
    ),
    (
        ", PERIOD ",
        Feature {
            since: 18,
            what: "temporal foreign keys (PERIOD)",
        },
    ),
    (
        "(PERIOD ",
        Feature {
            since: 18,
            what: "temporal foreign keys (PERIOD)",
        },
    ),
];

/// An exclusion constraint on a partitioned table.
pub(crate) const PARTITIONED_EXCLUSION: Feature = Feature {
    since: 17,
    what: "exclusion constraints on partitioned tables",
};

/// The features of [`SYNTAX`] a statement uses, newest first. Only
/// statements that define structure are read: a comment's text or a
/// policy's expression never is.
pub(crate) fn features(kind: Kind, sql: &str) -> Vec<Feature> {
    if !matches!(
        kind,
        Kind::CreateTable
            | Kind::AddColumn
            | Kind::AlterColumn
            | Kind::AddConstraint
            | Kind::CreateIndex
            | Kind::CreatePartition
    ) {
        return Vec::new();
    }
    let code = code(sql);
    let mut found: Vec<Feature> = SYNTAX
        .iter()
        .filter(|(needle, _)| code.contains(needle))
        .map(|(_, feature)| *feature)
        .collect();
    found.sort_by(|a, b| b.cmp(a));
    found.dedup();
    found
}

/// `sql` uppercase, with string literals, quoted identifiers and comments
/// blanked and whitespace runs as one space: what keywords are searched in.
fn code(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut space = false;
    let push = |out: &mut String, c: char, space: &mut bool| {
        if c.is_whitespace() {
            *space = true;
        } else {
            if *space && !out.is_empty() {
                out.push(' ');
            }
            *space = false;
            out.extend(c.to_uppercase());
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                // Doubled quotes stay inside: skip to the closing one.
                while let Some(d) = chars.next() {
                    if d == c {
                        if chars.peek() == Some(&c) {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
                push(&mut out, 'X', &mut space);
            }
            '-' if chars.peek() == Some(&'-') => {
                for d in chars.by_ref() {
                    if d == '\n' {
                        break;
                    }
                }
                space = true;
            }
            _ => push(&mut out, c, &mut space),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_found_outside_literals_identifiers_and_comments() {
        let found = |sql| {
            features(Kind::AddConstraint, sql)
                .iter()
                .map(|f| f.since)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            found("ALTER TABLE t ADD CONSTRAINT k UNIQUE NULLS  NOT\n DISTINCT (a);"),
            [15]
        );
        assert_eq!(
            found("ALTER TABLE t ADD CONSTRAINT k CHECK (a <> 'nulls not distinct');"),
            [] as [u32; 0]
        );
        assert_eq!(
            found(
                "ALTER TABLE \"NULLS NOT DISTINCT\" ADD CONSTRAINT k CHECK (a > 0); -- NOT ENFORCED"
            ),
            [] as [u32; 0]
        );
        assert_eq!(
            found("ALTER TABLE t ADD CONSTRAINT k CHECK (a > 0) NOT ENFORCED;"),
            [18]
        );
        assert_eq!(
            found(
                "ALTER TABLE t ADD CONSTRAINT k FOREIGN KEY (a, PERIOD b) REFERENCES u (a, PERIOD b) ON DELETE SET NULL (a);"
            ),
            [18, 15]
        );
        // A comment's text is never read.
        assert_eq!(
            features(
                Kind::Comment,
                "COMMENT ON TABLE t IS 'x' -- NULLS NOT DISTINCT"
            ),
            []
        );
    }

    #[test]
    fn a_virtual_column_is_18() {
        let found = features(
            Kind::AddColumn,
            "ALTER TABLE t ADD COLUMN b integer GENERATED ALWAYS AS ((a * 2)) VIRTUAL;",
        );
        assert_eq!(found[0].since, 18);
        assert_eq!(
            features(
                Kind::AddColumn,
                "ALTER TABLE t ADD COLUMN b integer GENERATED ALWAYS AS ((a * 2)) STORED;"
            ),
            []
        );
    }
}
