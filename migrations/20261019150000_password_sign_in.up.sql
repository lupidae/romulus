-- Built-in email + password sign-in (ROMULUS_AUTH=password), for installs
-- without an identity provider (docs/self-host). Accounts are users like any
-- other: `oidc_subject` is `password|<uuid>` for them, so the OIDC upsert can
-- never reach one, and an address has at most one password account.

COMMENT ON TABLE users IS
    'A person: identity from the OIDC provider (this row mirrors its claims at the last sign-in), or a password account (oidc_subject `password|…`).';
COMMENT ON COLUMN users.oidc_subject IS
    'The IdP''s `sub` claim: the only stable identity (emails change). `password|<uuid>` for password accounts, `dev|<email>` for the dev sign-in.';

CREATE UNIQUE INDEX users_password_email_key ON users (lower(email COLLATE pg_unicode_fast))
    WHERE oidc_subject LIKE 'password|%';

CREATE TABLE password_credentials (
    user_id uuid PRIMARY KEY REFERENCES users ON DELETE CASCADE,
    hash text NOT NULL CHECK (hash LIKE '$argon2id$%'),
    changed_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE password_credentials IS
    'Password accounts'' secrets: an Argon2id PHC string (parameters and salt included). A password user without a row has not set one yet (setup).';

-- Sign-in attempts per address since its last success, whether an account
-- exists or not: the answers (and the lockout) are the same either way, so
-- they tell nobody which addresses have one. Each attempt counts before its
-- password is checked, so concurrent guesses cannot outrun the lock.
CREATE TABLE sign_in_failures (
    -- lower() can lengthen an address (İ → i̇): bounded, not to 254.
    email_key text PRIMARY KEY CHECK (char_length(email_key) <= 1024),
    -- Attempts in the current series (a success clears the row; a series
    -- starts again once a lock has run out).
    failures integer NOT NULL DEFAULT 0 CHECK (failures >= 0),
    -- Locks since the last success, within a day: an address locked again
    -- and again is someone's target (logged).
    locks integer NOT NULL DEFAULT 0 CHECK (locks >= 0),
    -- The latest attempt of the series.
    last_failure_at timestamptz NOT NULL DEFAULT now(),
    locked_until timestamptz
);
COMMENT ON TABLE sign_in_failures IS
    'Password sign-in attempts per address (lower(email COLLATE pg_unicode_fast)), existing account or not: the 10th in a series locks it 15 minutes, and a new series starts when the lock runs out. Cleared by a success, a password reset or `romulus-api unlock`; forgotten a day after the last attempt.';
COMMENT ON COLUMN sign_in_failures.locked_until IS
    'Sign-ins with this address are refused until then, the password unchecked.';
CREATE INDEX sign_in_failures_last_failure_at_idx ON sign_in_failures (last_failure_at);

-- Browsers that signed in to an account with its password (a device cookie,
-- OWASP's defence against lockout abuse): an attempt from one counts against
-- the device, not the address, so guesses from elsewhere cannot lock the
-- account's owner out.
CREATE TABLE sign_in_devices (
    token_hash bytea PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    failures integer NOT NULL DEFAULT 0 CHECK (failures >= 0),
    locked_until timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE sign_in_devices IS
    'Browsers that signed in to a password account (the sha256 of their device cookie): their attempts skip the address''s lock and count their own failures. At most 10 per account, forgotten after 180 days unused.';
CREATE INDEX sign_in_devices_user_id_idx ON sign_in_devices (user_id, last_used_at DESC);
CREATE INDEX sign_in_devices_last_used_at_idx ON sign_in_devices (last_used_at);

CREATE TYPE auth_link_purpose AS ENUM ('sign_up', 'reset_password');

-- One-time links sent by email or printed by the CLI. Only the sha256 of the
-- token is stored; the token rides in the link's fragment (never in a
-- server log or a Referer).
CREATE TABLE auth_links (
    token_hash bytea PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    purpose auth_link_purpose NOT NULL,
    -- reset_password: the account. sign_up: none yet.
    user_id uuid REFERENCES users ON DELETE CASCADE,
    email text NOT NULL CHECK (email IS NFC NORMALIZED AND char_length(email) <= 254),
    -- sign_up: the name given, and the invitation it came from (joined once
    -- the account exists).
    name text CHECK (name IS NULL OR char_length(name) <= 100),
    invitation_id uuid REFERENCES invitations ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_at timestamptz,
    CONSTRAINT auth_links_reset_has_a_user CHECK ((purpose = 'reset_password') = (user_id IS NOT NULL))
);
COMMENT ON TABLE auth_links IS
    'One-time links: finishing a sign-up (the address proved by receiving it) and setting or resetting a password. Single use, short-lived.';
CREATE INDEX auth_links_user_id_idx ON auth_links (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX auth_links_invitation_id_idx ON auth_links (invitation_id) WHERE invitation_id IS NOT NULL;
CREATE INDEX auth_links_expires_at_idx ON auth_links (expires_at);

CREATE TYPE auth_event_kind AS ENUM (
    'signed_in', 'sign_in_failed', 'sign_in_locked',
    'sign_up_requested', 'account_created',
    'password_reset_requested', 'password_reset', 'password_changed',
    'password_link_created', 'unlocked', 'email_verified', 'admin_created'
);

CREATE TABLE auth_events (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    kind auth_event_kind NOT NULL,
    user_id uuid REFERENCES users ON DELETE SET NULL,
    -- The address concerned, as given (a failed sign-in's may have no account).
    email text CHECK (email IS NULL OR char_length(email) <= 254),
    -- The client, as the API saw it (behind ROMULUS_TRUSTED_PROXIES: the
    -- forwarded address). NULL for the CLI.
    ip inet,
    details jsonb NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE auth_events IS
    'Password sign-in''s security log: sign-ins, failures, lockouts, resets, admin commands. Append only; rows older than 90 days are purged.';
CREATE INDEX auth_events_created_at_idx ON auth_events (created_at DESC);
CREATE INDEX auth_events_user_id_idx ON auth_events (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX auth_events_email_idx ON auth_events (lower(email COLLATE pg_unicode_fast)) WHERE email IS NOT NULL;

CREATE FUNCTION auth_events_are_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' THEN
        -- The one update allowed: ON DELETE SET NULL on the user.
        IF (to_jsonb(NEW) - 'user_id') IS DISTINCT FROM (to_jsonb(OLD) - 'user_id')
           OR NEW.user_id IS NOT NULL THEN
            RAISE EXCEPTION 'auth event % is immutable', OLD.id
                USING ERRCODE = 'check_violation', CONSTRAINT = 'auth_events_are_append_only';
        END IF;
        RETURN NEW;
    END IF;
    -- Deleted only by the retention purge.
    IF OLD.created_at > now() - interval '90 days' THEN
        RAISE EXCEPTION 'auth event % is kept for 90 days', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'auth_events_are_append_only';
    END IF;
    RETURN OLD;
END
$$;

CREATE TRIGGER auth_events_are_append_only BEFORE UPDATE OR DELETE ON auth_events
    FOR EACH ROW EXECUTE FUNCTION auth_events_are_append_only();

CREATE FUNCTION auth_events_are_not_truncated() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'auth_events cannot be truncated'
        USING ERRCODE = 'check_violation', CONSTRAINT = 'auth_events_are_append_only';
END
$$;

CREATE TRIGGER auth_events_are_not_truncated BEFORE TRUNCATE ON auth_events
    FOR EACH STATEMENT EXECUTE FUNCTION auth_events_are_not_truncated();
