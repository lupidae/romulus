//! Sign-in through Zitadel (OIDC code flow + PKCE, done by the API as a
//! backend-for-frontend) and the session cookie.
//!
//! Planned: `GET /auth/login` (redirect to the IdP), `GET /auth/callback`
//! (the redirect URI the Zitadel provisioner registers), `POST /auth/logout`,
//! `GET /auth/me`, and an extractor (`CurrentUser`) other modules use.
//! Tables: `users`, `sessions`. The cookie name is `Config::session_cookie`.
//! Requests with `Authorization: Bearer` authenticate by API token only;
//! never fall back to the cookie (see `crate::csrf`).

use utoipa_axum::router::OpenApiRouter;

use crate::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
