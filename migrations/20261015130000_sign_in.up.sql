-- OIDC sign-ins in flight, between /api/auth/login and its callback.
CREATE TABLE oidc_flows (
    state_hash bytea PRIMARY KEY CHECK (octet_length(state_hash) = 32),
    pkce_verifier text NOT NULL,
    nonce text NOT NULL,
    next text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE oidc_flows IS
    'Authorization code flows in progress: the PKCE verifier and nonce of a redirect to the IdP, until its callback (one use, 10 minutes at most).';
COMMENT ON COLUMN oidc_flows.state_hash IS
    'sha256 of the state parameter, which the browser also holds in an httpOnly cookie: the callback must come back to the browser that started it.';
COMMENT ON COLUMN oidc_flows.next IS 'Where to land after signing in: a path under /app, checked before it was stored.';

-- Every new sign-in sweeps the abandoned ones.
CREATE INDEX oidc_flows_created_at_idx ON oidc_flows (created_at);

-- Organization pages live at /app/<slug>, next to the app's own top-level
-- routes: a slug must not shadow one of them.
ALTER TABLE organizations ADD CONSTRAINT organizations_slug_not_reserved
    CHECK (slug NOT IN ('signin', 'new', 'invite', 'dev', 'settings', 'api', 'app'));
