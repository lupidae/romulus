CREATE TABLE users (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    oidc_subject text NOT NULL UNIQUE,
    email text NOT NULL,
    email_verified boolean NOT NULL DEFAULT false,
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE users IS
    'Identity comes from Zitadel (OIDC); this row mirrors the claims at the last sign-in.';
COMMENT ON COLUMN users.oidc_subject IS 'The IdP''s `sub` claim: the only stable identity. Emails change.';
COMMENT ON COLUMN users.email_verified IS
    'The `email_verified` claim. Accepting an invitation requires it to be true.';

-- Invitations are matched by email, case-insensitively.
CREATE INDEX users_lower_email_idx ON users (lower(email));

CREATE TRIGGER users_set_updated_at BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE sessions (
    id_hash bytea PRIMARY KEY CHECK (octet_length(id_hash) = 32),
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    oidc_refresh_token bytea
);

COMMENT ON TABLE sessions IS
    'Browser sessions (backend-for-frontend): the SPA holds only an httpOnly cookie.';
COMMENT ON COLUMN sessions.id_hash IS
    'sha256 of the cookie value. The raw session id is never stored, so a database leak yields no usable cookie.';
COMMENT ON COLUMN sessions.oidc_refresh_token IS
    'Encrypted by the API (AEAD, key from its config). NULL when the IdP issued none.';

CREATE INDEX sessions_user_id_idx ON sessions (user_id);
-- The expiry sweep deletes by expires_at.
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);
