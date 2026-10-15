//! Sign-in through Zitadel (OIDC code flow + PKCE, done by the API as a
//! backend-for-frontend) and the session cookie.
//!
//! Planned: `GET /auth/login` (redirect to the IdP), `GET /auth/callback`
//! (the redirect URI the Zitadel provisioner registers), `POST /auth/logout`,
//! `GET /auth/me`, and an extractor (`CurrentUser`) other modules use.
//! Tables: `users`, `sessions`. The cookie name is `Config::session_cookie`.
//! Requests with `Authorization: Bearer` authenticate by API token only;
//! never fall back to the cookie (see `crate::csrf`).
//!
//! Dev sign-in: when `Config::dev_signin` is true (env `ROMULUS_DEV_SIGNIN`,
//! which config refuses unless the public URL is loopback), a route such as
//! `POST /auth/dev-signin {email}` creates the session for a test user
//! without Zitadel, for QA and the Playwright suite. Register it only when
//! the flag is set, so it does not exist (404) anywhere else.

use utoipa_axum::router::OpenApiRouter;

use crate::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
