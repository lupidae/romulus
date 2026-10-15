//! Organization slugs: the `<slug>` of `/app/<slug>/…`.

/// Top-level routes of the web app (and the API's prefix). An organization
/// named like one would be unreachable. The database refuses them too
/// (`organizations_slug_not_reserved`); keep both lists equal.
pub const RESERVED: [&str; 7] = ["signin", "new", "invite", "dev", "settings", "api", "app"];

pub const MAX_LEN: usize = 63;

/// Why `slug` cannot be an organization's address, in a sentence for the
/// form, or `None` when it can.
pub fn problem(slug: &str) -> Option<&'static str> {
    if slug.is_empty() {
        return Some("Choose a URL for the organization.");
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
        return Some("This URL is reserved by romulus. Pick another one.");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_slugs_are_fine() {
        for slug in ["acme", "acme-labs", "a", "42", "x1-y2-z3"] {
            assert_eq!(problem(slug), None, "{slug}");
        }
        assert_eq!(problem(&"a".repeat(63)), None);
    }

    #[test]
    fn malformed_slugs_are_refused() {
        for slug in [
            "",
            "Acme",
            "acme_labs",
            "-acme",
            "acme-",
            "ac--me",
            "café",
            "a b",
            &"a".repeat(64),
        ] {
            assert!(problem(slug).is_some(), "{slug:?}");
        }
    }

    #[test]
    fn app_routes_are_reserved() {
        for slug in RESERVED {
            assert!(problem(slug).unwrap().contains("reserved"), "{slug}");
        }
    }
}
