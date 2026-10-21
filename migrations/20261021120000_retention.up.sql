-- Retention (docs/MVP.md "Retention and deletion"): when each account was
-- last used and whether it was told it is about to be deleted for
-- inactivity, what the purges look up by, and the events deleting and
-- purging write.

-- Only the identity columns mark a row changed: last_seen_at moves on every
-- hour of use and is not a change of the account.
DROP TRIGGER users_set_updated_at ON users;
CREATE TRIGGER users_set_updated_at BEFORE UPDATE OF oidc_subject, email, email_verified, name ON users
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

ALTER TABLE users
    ADD COLUMN last_seen_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN inactivity_notice_at timestamptz,
    ADD COLUMN inactivity_failures integer NOT NULL DEFAULT 0
        CONSTRAINT users_inactivity_failures_positive CHECK (inactivity_failures >= 0),
    ADD COLUMN inactivity_retry_at timestamptz;
-- Accounts that exist: their latest use known. A session outlives its last
-- use by a week at most; updated_at moves at each OIDC sign-in; tokens used
-- by the person's agent or by the CI of a database they made count as use.
UPDATE users u SET last_seen_at = greatest(
    u.created_at, u.updated_at,
    (SELECT max(s.last_seen_at) FROM sessions s WHERE s.user_id = u.id),
    (SELECT max(t.last_used_at) FROM personal_access_tokens t WHERE t.user_id = u.id),
    (SELECT max(t.last_used_at) FROM api_tokens t WHERE t.created_by = u.id)
);
COMMENT ON COLUMN users.last_seen_at IS
    'When the account was last used (retention::activity): at each sign-in, and at most once an hour on the use of a session, of a personal access token, or of an API token the account made. With ROMULUS_DELETE_INACTIVE_ACCOUNTS, an account unused for 3 years is deleted (retention::inactive).';
COMMENT ON COLUMN users.inactivity_notice_at IS
    'When the mail server took the email saying the account is about to be deleted for inactivity (deleted 30 days later at the earliest); cleared when it is used again.';
COMMENT ON COLUMN users.inactivity_failures IS
    'Inactivity notices or deletions of this account that failed in a row (mail refused, identity provider down): each doubles the wait before the next try.';
COMMENT ON COLUMN users.inactivity_retry_at IS
    'No inactivity notice or deletion of this account is tried before then: a notice being sent (a claim), or a failure''s back-off. NULL: whenever due.';
CREATE INDEX users_last_seen_at_idx ON users (last_seen_at);

-- The purges (retention::purge): deleted databases 30 days on, pending
-- invitations 30 days after they expire (a revoked one is deleted at once).
CREATE INDEX databases_deleted_at_idx ON databases (deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX invitations_pending_expires_at_idx ON invitations (expires_at) WHERE accepted_at IS NULL;
COMMENT ON TABLE databases IS
    'A schema kept in romulus. Soft-deleted: deleted_at hides it at once, and 30 days later it is purged with its versions (retention::purge).';

ALTER TYPE audit_action ADD VALUE 'database_purged';
ALTER TYPE audit_action ADD VALUE 'admin_appointed';
ALTER TYPE auth_event_kind ADD VALUE 'account_deleted';
ALTER TYPE auth_event_kind ADD VALUE 'organization_deleted';
