//! Where to land after signing in: `?next=` comes from the URL, which anyone
//! can craft, so it is an open redirect unless checked here, server-side.

/// The default landing: the app, which routes to the last organization.
pub const HOME: &str = "/app/";

/// `next` if it is a path into the SPA on this origin (`/app` or below),
/// normalised; otherwise `None`.
///
/// Refused: absolute and scheme-relative URLs (`https://…`, `//evil.test`),
/// backslashes (browsers read `/\evil.test` as `//evil.test`), control
/// characters (stripped differently by different URL parsers), and paths
/// that leave `/app` once `..` and `.` are resolved.
pub fn safe(next: &str) -> Option<String> {
    if !next.starts_with('/')
        || next.starts_with("//")
        || next.contains('\\')
        || next.chars().any(char::is_control)
        || next.len() > 2048
    {
        return None;
    }
    let base = url::Url::parse("http://romulus.invalid/").expect("a valid URL");
    let url = base.join(next).ok()?;
    if url.origin() != base.origin() {
        return None;
    }
    let path = &url[url::Position::BeforePath..];
    let under_app = url.path() == "/app" || url.path().starts_with("/app/");
    (under_app && !path.starts_with("//")).then(|| path.to_owned())
}

/// [`safe`], or the app's home page.
pub fn or_home(next: Option<&str>) -> String {
    next.and_then(safe).unwrap_or_else(|| HOME.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_into_the_app_are_kept() {
        for next in [
            "/app",
            "/app/",
            "/app/acme/settings/members",
            "/app/invite/abc?x=1#top",
        ] {
            assert_eq!(safe(next).as_deref(), Some(next), "{next}");
        }
    }

    #[test]
    fn paths_are_normalised() {
        assert_eq!(safe("/app/acme/../beta").as_deref(), Some("/app/beta"));
        assert_eq!(safe("/app/./x").as_deref(), Some("/app/x"));
    }

    #[test]
    fn anything_leaving_the_app_is_refused() {
        for next in [
            "",
            "app/acme",
            "https://evil.test/app/",
            "//evil.test/app/",
            "/\\evil.test",
            "/app\\..\\..\\evil",
            "/app/\t/evil",
            "/app/\n",
            "/api/auth/logout",
            "/application",
            "/",
            "/app/../api/me",
            "/app/..//evil.test",
            "/app/../../evil.test",
        ] {
            assert_eq!(safe(next), None, "{next:?}");
        }
    }

    #[test]
    fn a_refused_target_falls_back_to_home() {
        assert_eq!(or_home(Some("//evil.test")), HOME);
        assert_eq!(or_home(None), HOME);
        assert_eq!(or_home(Some("/app/acme")), "/app/acme");
    }
}
