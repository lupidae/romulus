//! Database slugs: the `<database>` of `/app/<org>/<database>/…`.

/// The organization's own pages under `/app/<org>/`. A database named like
/// one would be unreachable. The database refuses them too
/// (`databases_slug_not_reserved`); keep both lists equal.
pub const RESERVED: [&str; 6] = [
    "settings",
    "new",
    "members",
    "invitations",
    "search",
    "activity",
];

pub const MAX_LEN: usize = 63;

/// Why `slug` cannot be a database's address, in a sentence for the form, or
/// `None` when it can.
pub fn problem(slug: &str) -> Option<&'static str> {
    if slug.is_empty() {
        return Some("Choose a URL for the database.");
    }
    if slug.len() > MAX_LEN {
        return Some("Keep the URL to 63 characters or fewer.");
    }
    let well_formed = slug.split('-').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    });
    if !well_formed {
        return Some("Use lowercase letters, digits and single hyphens, not at the start or end.");
    }
    if RESERVED.contains(&slug) {
        return Some("This URL is used by the organization's own pages. Pick another one.");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_slugs_are_fine() {
        for slug in ["billing", "billing-prod", "db2", "dev", "api", "app"] {
            assert_eq!(problem(slug), None, "{slug}");
        }
    }

    #[test]
    fn malformed_slugs_are_refused() {
        for slug in ["", "Billing", "billing_db", "-a", "a-", "a--b", &"a".repeat(64)] {
            assert!(problem(slug).is_some(), "{slug:?}");
        }
    }

    #[test]
    fn organization_pages_are_reserved() {
        for slug in RESERVED {
            assert!(problem(slug).unwrap().contains("organization"), "{slug}");
        }
    }
}
