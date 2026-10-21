-- OAuth for the MCP endpoint (the oauth module, docs/mcp.md): romulus's API
-- is the authorization server of its own MCP resource, /api/mcp. An agent
-- registers itself (dynamic client registration), sends its person here to
-- sign in and approve, and gets access tokens (`rmls_oat_`, an hour) that
-- act as that person exactly as a personal access token does, with refresh
-- tokens (`rmls_ort_`) that rotate on every use. Only sha256 hashes of the
-- secrets are kept. Zitadel stays the identity provider: signing in is the
-- usual sign-in.

CREATE TABLE oauth_clients (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    name text CHECK (name IS NULL OR (btrim(name) <> '' AND char_length(name) <= 100)),
    redirect_uris text[] NOT NULL CHECK (cardinality(redirect_uris) BETWEEN 1 AND 10),
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz
);

COMMENT ON TABLE oauth_clients IS
    'MCP clients registered with POST /api/oauth/register (RFC 7591), open to anyone: public clients (PKCE, no secret). Its id is the client_id. Swept 30 days after its last use when no grant of it is left.';
COMMENT ON COLUMN oauth_clients.name IS 'client_name as the client registered it (unverified): shown on the consent screen next to its redirect URI''s host.';
COMMENT ON COLUMN oauth_clients.redirect_uris IS 'https URIs, or http on a loopback host (127.0.0.1, [::1], localhost), whose port is not compared (RFC 8252).';
COMMENT ON COLUMN oauth_clients.last_used_at IS 'Its last authorization, code exchange or refresh.';

-- Authorization requests waiting for their person's decision: GET
-- /api/oauth/authorize checks one and keeps it here, the consent page
-- shows it, and the decision consumes it.
CREATE TABLE oauth_requests (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    binding_hash bytea NOT NULL CHECK (octet_length(binding_hash) = 32),
    client_id uuid NOT NULL REFERENCES oauth_clients ON DELETE CASCADE,
    redirect_uri text NOT NULL CHECK (char_length(redirect_uri) <= 2000),
    redirect_uri_given boolean NOT NULL,
    state text NOT NULL CHECK (char_length(state) BETWEEN 1 AND 1024),
    code_challenge text NOT NULL CHECK (char_length(code_challenge) = 43),
    scope access_token_scope NOT NULL,
    resource text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE oauth_requests IS
    'Authorization requests waiting for consent (20 minutes at most). The browser that started one holds a cookie with the secret whose sha256 is binding_hash: the request is shown and decided in that browser only.';
COMMENT ON COLUMN oauth_requests.redirect_uri_given IS 'The request named its redirect_uri (rather than the client''s only one): the token request must then name the same.';

CREATE INDEX oauth_requests_client_id_idx ON oauth_requests (client_id);
CREATE INDEX oauth_requests_created_at_idx ON oauth_requests (created_at);

-- A person's approval of a client: what its tokens may do, until when.
CREATE TABLE oauth_grants (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    client_id uuid NOT NULL REFERENCES oauth_clients ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    scope access_token_scope NOT NULL,
    resource text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL CHECK (expires_at > created_at),
    idle_expires_at timestamptz NOT NULL CHECK (idle_expires_at <= expires_at),
    last_used_at timestamptz,
    last_client text CHECK (last_client IS NULL OR char_length(last_client) <= 200),
    revoked_at timestamptz
);

COMMENT ON TABLE oauth_grants IS
    'One approval (a code exchanged): the family of its access and refresh tokens. Revoking it (Disconnect, a refresh token replayed, a code exchanged twice, a password reset or change) ends every token of it at once.';
COMMENT ON COLUMN oauth_grants.scope IS '`read`: reading tools only (romulus.read); `write`: everything the person''s roles allow (romulus.write).';
COMMENT ON COLUMN oauth_grants.resource IS 'The resource its tokens are for (RFC 8707): the MCP endpoint''s URL when it was approved. A token is refused anywhere else.';
COMMENT ON COLUMN oauth_grants.expires_at IS 'At most 90 days after the approval, refreshed or not.';
COMMENT ON COLUMN oauth_grants.idle_expires_at IS 'Its refresh token''s expiry: 30 days after the last refresh, never past expires_at.';
COMMENT ON COLUMN oauth_grants.last_client IS 'The MCP client that last said who it is with it (its clientInfo), as it said it.';

CREATE INDEX oauth_grants_user_id_idx ON oauth_grants (user_id, client_id);
CREATE INDEX oauth_grants_client_id_idx ON oauth_grants (client_id);

CREATE TABLE oauth_codes (
    code_hash bytea PRIMARY KEY CHECK (octet_length(code_hash) = 32),
    client_id uuid NOT NULL REFERENCES oauth_clients ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    redirect_uri text NOT NULL,
    redirect_uri_given boolean NOT NULL,
    code_challenge text NOT NULL CHECK (char_length(code_challenge) = 43),
    scope access_token_scope NOT NULL,
    resource text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    used_at timestamptz,
    grant_id uuid REFERENCES oauth_grants ON DELETE SET NULL
);

COMMENT ON TABLE oauth_codes IS
    'Authorization codes: 60 seconds, single use (PKCE S256). A code presented again revokes the grant its first exchange made (grant_id).';

CREATE INDEX oauth_codes_client_id_idx ON oauth_codes (client_id);
CREATE INDEX oauth_codes_user_id_idx ON oauth_codes (user_id);
CREATE INDEX oauth_codes_grant_id_idx ON oauth_codes (grant_id);
CREATE INDEX oauth_codes_created_at_idx ON oauth_codes (created_at);

CREATE TABLE oauth_access_tokens (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    grant_id uuid NOT NULL REFERENCES oauth_grants ON DELETE CASCADE,
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL CHECK (expires_at > created_at),
    revoked_at timestamptz
);

COMMENT ON TABLE oauth_access_tokens IS '`rmls_oat_` + a random token, an hour, only its sha256 stored. Accepted at /api/mcp only, as its grant''s person.';

CREATE INDEX oauth_access_tokens_grant_id_idx ON oauth_access_tokens (grant_id);
CREATE INDEX oauth_access_tokens_expires_at_idx ON oauth_access_tokens (expires_at);

CREATE TABLE oauth_refresh_tokens (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    grant_id uuid NOT NULL REFERENCES oauth_grants ON DELETE CASCADE,
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL CHECK (expires_at > created_at),
    used_at timestamptz
);

COMMENT ON TABLE oauth_refresh_tokens IS
    '`rmls_ort_` + a random token, only its sha256 stored. Each use replaces it (used_at set); a used one presented again is a replay, which revokes its grant.';

CREATE INDEX oauth_refresh_tokens_grant_id_idx ON oauth_refresh_tokens (grant_id);
CREATE INDEX oauth_refresh_tokens_expires_at_idx ON oauth_refresh_tokens (expires_at);

-- The consent page is /app/oauth/authorize, a top-level route of the web app:
-- an organization may not be called that (orgs::slug::RESERVED). NOT VALID:
-- an organization that already took the name keeps it.
ALTER TABLE organizations DROP CONSTRAINT organizations_slug_not_reserved;
ALTER TABLE organizations ADD CONSTRAINT organizations_slug_not_reserved
    CHECK (slug NOT IN ('signin', 'new', 'invite', 'dev', 'settings', 'api', 'app', 'oauth')) NOT VALID;
