-- Review round 1 (docs/MVP.md): removing a version, approvals before
-- publishing, and the audit log both write to.

-- ---- Removing a version ------------------------------------------------------

-- A removed version is hidden (history, diffs, imports, the latest version),
-- restorable for 30 days, then purged (versions::removal).
ALTER TABLE versions
    ADD COLUMN removed_at timestamptz,
    ADD COLUMN removed_by uuid REFERENCES users ON DELETE SET NULL,
    ADD COLUMN removal_reason text,
    ADD CONSTRAINT versions_removal_whole CHECK (
        (removed_at IS NULL) = (removal_reason IS NULL) AND (removed_at IS NOT NULL OR removed_by IS NULL)
    ),
    ADD CONSTRAINT versions_removal_reason_length CHECK (
        removal_reason IS NULL OR (btrim(removal_reason) <> '' AND char_length(removal_reason) <= 1000)
    );

COMMENT ON COLUMN versions.removed_at IS
    'Set when an admin removed the version: hidden everywhere, restorable for 30 days, then purged (deleted). NULL: live.';
COMMENT ON COLUMN versions.removal_reason IS 'Why it was removed, as the admin wrote it (also in audit_events).';

-- The settings page lists them; the purge looks for the old ones.
CREATE INDEX versions_removed_idx ON versions (database_id, removed_at) WHERE removed_at IS NOT NULL;
CREATE INDEX versions_removed_by_idx ON versions (removed_by) WHERE removed_by IS NOT NULL;

CREATE OR REPLACE FUNCTION versions_are_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    -- The updates allowed: ON DELETE SET NULL on the author columns (deleting
    -- a user or token does not delete history), and removing or restoring
    -- the version. A removal itself is never edited: only undone.
    IF (to_jsonb(NEW) - 'created_by' - 'created_by_token' - 'removed_at' - 'removed_by' - 'removal_reason')
           IS DISTINCT FROM (to_jsonb(OLD) - 'created_by' - 'created_by_token' - 'removed_at' - 'removed_by' - 'removal_reason')
       OR (NEW.created_by IS NOT NULL AND NEW.created_by IS DISTINCT FROM OLD.created_by)
       OR (NEW.created_by_token IS NOT NULL AND NEW.created_by_token IS DISTINCT FROM OLD.created_by_token)
       OR (OLD.removed_at IS NOT NULL AND NEW.removed_at IS NOT NULL AND (
               NEW.removed_at IS DISTINCT FROM OLD.removed_at
            OR NEW.removal_reason IS DISTINCT FROM OLD.removal_reason
            OR (NEW.removed_by IS NOT NULL AND NEW.removed_by IS DISTINCT FROM OLD.removed_by))) THEN
        RAISE EXCEPTION 'version % is immutable', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'versions_are_immutable';
    END IF;
    RETURN NEW;
END
$$;

CREATE OR REPLACE FUNCTION versions_are_permanent() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    -- History only goes with its database (ON DELETE CASCADE, by then the
    -- databases row is gone), or once removed for 30 days (the purge).
    IF EXISTS (SELECT 1 FROM databases WHERE id = OLD.database_id)
       AND (OLD.removed_at IS NULL OR OLD.removed_at > now() - interval '30 days') THEN
        RAISE EXCEPTION 'version % cannot be deleted', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'versions_are_permanent';
    END IF;
    RETURN OLD;
END
$$;

-- Numbers are never given twice, even once a removed version is purged:
-- the database keeps the highest one given, and the next version is that
-- plus one (versions::store).
DROP TRIGGER databases_set_updated_at ON databases;
-- updated_at is the database's own edits (name, address, description),
-- never this counter moving.
CREATE TRIGGER databases_set_updated_at BEFORE UPDATE OF name, slug, description, deleted_at ON databases
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

ALTER TABLE databases ADD COLUMN last_version_number integer NOT NULL DEFAULT 0
    CONSTRAINT databases_last_version_number_check CHECK (last_version_number >= 0);
UPDATE databases d
SET last_version_number = coalesce((SELECT max(number) FROM versions v WHERE v.database_id = d.id), 0);

COMMENT ON COLUMN databases.last_version_number IS
    'The highest version number given (a trigger on versions keeps it): the next version gets this plus one, so a purged version''s number is never given again.';

CREATE FUNCTION versions_keep_last_number() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE databases SET last_version_number = NEW.number
    WHERE id = NEW.database_id AND last_version_number < NEW.number;
    RETURN NULL;
END
$$;

CREATE TRIGGER versions_keep_last_number AFTER INSERT ON versions
    FOR EACH ROW EXECUTE FUNCTION versions_keep_last_number();

-- An imported version removed on purpose stays out of the history: a later
-- import of the same files skips its migration ("removed in romulus")
-- rather than creating it again or refusing the files as out of order.
-- Once purged, its migration is remembered here, without its SQL.
CREATE TABLE purged_migrations (
    database_id uuid NOT NULL REFERENCES databases ON DELETE CASCADE,
    migration_id text NOT NULL,
    path text NOT NULL,
    checksum bytea NOT NULL CHECK (octet_length(checksum) = 32),
    number integer NOT NULL CHECK (number > 0),
    purged_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (database_id, migration_id)
);
COMMENT ON TABLE purged_migrations IS
    'Migrations whose imported version was removed, then purged (versions::removal): later imports skip them. Written by the purge, never changed.';

-- ---- The audit log -------------------------------------------------------------

CREATE TYPE audit_action AS ENUM (
    'version_removed', 'version_restored', 'version_purged',
    'version_published', 'approvals_overridden', 'approvals_required_changed'
);

CREATE TABLE audit_events (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    org_id uuid NOT NULL REFERENCES organizations ON DELETE CASCADE,
    database_id uuid REFERENCES databases ON DELETE CASCADE,
    action audit_action NOT NULL,
    actor_id uuid REFERENCES users ON DELETE SET NULL,
    -- Their name then: the event reads the same once they rename or leave.
    actor_name text,
    version_number integer CHECK (version_number > 0),
    reason text CHECK (reason IS NULL OR (btrim(reason) <> '' AND char_length(reason) <= 1000)),
    details jsonb NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE audit_events IS
    'Who did what that the organization may need to account for (audit::Action): append only, gone only with the organization or the database.';
COMMENT ON COLUMN audit_events.actor_id IS 'NULL for what romulus did itself (the purge), or once the account is gone.';
COMMENT ON COLUMN audit_events.actor_name IS 'The actor''s name when it happened (NULL for romulus itself).';
COMMENT ON COLUMN audit_events.version_number IS 'The version concerned (removed, restored, purged, published).';
COMMENT ON COLUMN audit_events.details IS
    'What the action needs to be read later, by action: the version''s message; the draft, who approved it and the number required; the old and new number of approvals required.';

CREATE INDEX audit_events_database_id_idx ON audit_events (database_id, created_at DESC) WHERE database_id IS NOT NULL;
CREATE INDEX audit_events_org_id_idx ON audit_events (org_id, created_at DESC);
CREATE INDEX audit_events_actor_id_idx ON audit_events (actor_id) WHERE actor_id IS NOT NULL;

CREATE FUNCTION audit_events_are_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' THEN
        -- The one update allowed: ON DELETE SET NULL on the actor.
        IF (to_jsonb(NEW) - 'actor_id') IS DISTINCT FROM (to_jsonb(OLD) - 'actor_id')
           OR NEW.actor_id IS NOT NULL THEN
            RAISE EXCEPTION 'audit event % is immutable', OLD.id
                USING ERRCODE = 'check_violation', CONSTRAINT = 'audit_events_are_append_only';
        END IF;
        RETURN NEW;
    END IF;
    -- Deleted only with its organization or database (ON DELETE CASCADE: by
    -- then their row is gone).
    IF EXISTS (SELECT 1 FROM organizations WHERE id = OLD.org_id)
       AND (OLD.database_id IS NULL OR EXISTS (SELECT 1 FROM databases WHERE id = OLD.database_id)) THEN
        RAISE EXCEPTION 'audit event % cannot be deleted', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'audit_events_are_append_only';
    END IF;
    RETURN OLD;
END
$$;

CREATE TRIGGER audit_events_are_append_only BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION audit_events_are_append_only();

CREATE FUNCTION audit_events_are_not_truncated() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_events cannot be truncated'
        USING ERRCODE = 'check_violation', CONSTRAINT = 'audit_events_are_append_only';
END
$$;

CREATE TRIGGER audit_events_are_not_truncated BEFORE TRUNCATE ON audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION audit_events_are_not_truncated();

-- ---- Approvals ---------------------------------------------------------------------

ALTER TABLE organizations ADD COLUMN approvals_required smallint NOT NULL DEFAULT 0
    CONSTRAINT organizations_approvals_required_range CHECK (approvals_required BETWEEN 0 AND 10);
COMMENT ON COLUMN organizations.approvals_required IS
    'Approvals a draft needs before it is published, from members who did not edit it; 0: none. Pushes and imports are never gated.';

CREATE TABLE draft_editors (
    draft_id uuid NOT NULL REFERENCES drafts ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    first_edit_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (draft_id, user_id)
);
COMMENT ON TABLE draft_editors IS
    'Who changed a draft''s SQL (the collab room records it on their first edit): they cannot approve it.';
CREATE INDEX draft_editors_user_id_idx ON draft_editors (user_id);

CREATE TABLE draft_approvals (
    draft_id uuid NOT NULL REFERENCES drafts ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    sql_sha256 bytea NOT NULL CHECK (octet_length(sql_sha256) = 32),
    approved_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (draft_id, user_id)
);
COMMENT ON TABLE draft_approvals IS
    'Each member''s approval of a draft, of one text: it counts while the SQL hashes to sql_sha256 (any edit resets it).';
CREATE INDEX draft_approvals_user_id_idx ON draft_approvals (user_id);

CREATE TABLE draft_review_requests (
    draft_id uuid PRIMARY KEY REFERENCES drafts ON DELETE CASCADE,
    requested_by uuid REFERENCES users ON DELETE SET NULL,
    requested_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE draft_review_requests IS 'A draft whose editors asked for a review: shown in the drafts list.';
CREATE INDEX draft_review_requests_requested_by_idx ON draft_review_requests (requested_by)
    WHERE requested_by IS NOT NULL;

-- Who approved a version, frozen when its draft was published (the
-- draft's own approvals may go: a purge deletes published drafts). The
-- audit log keeps the same, and outlives a purge of the version.
CREATE TABLE version_approvals (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    version_id uuid NOT NULL,
    database_id uuid NOT NULL,
    user_id uuid REFERENCES users ON DELETE SET NULL,
    name text NOT NULL,
    approved_at timestamptz NOT NULL,
    sql_sha256 bytea NOT NULL CHECK (octet_length(sql_sha256) = 32),
    CONSTRAINT version_approvals_version_fkey FOREIGN KEY (database_id, version_id)
        REFERENCES versions (database_id, id) ON DELETE CASCADE
);
COMMENT ON TABLE version_approvals IS
    'The approvals a draft had when it was published as the version: name as it was, when, of which text. Immutable; gone only with the version.';
CREATE INDEX version_approvals_version_id_idx ON version_approvals (version_id);
CREATE INDEX version_approvals_user_id_idx ON version_approvals (user_id) WHERE user_id IS NOT NULL;

CREATE FUNCTION version_approvals_are_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' THEN
        -- The one update allowed: ON DELETE SET NULL on the user.
        IF (to_jsonb(NEW) - 'user_id') IS DISTINCT FROM (to_jsonb(OLD) - 'user_id')
           OR NEW.user_id IS NOT NULL THEN
            RAISE EXCEPTION 'version approval % is immutable', OLD.id
                USING ERRCODE = 'check_violation', CONSTRAINT = 'versions_are_immutable';
        END IF;
        RETURN NEW;
    END IF;
    IF EXISTS (SELECT 1 FROM versions WHERE id = OLD.version_id) THEN
        RAISE EXCEPTION 'version approval % cannot be deleted', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'versions_are_permanent';
    END IF;
    RETURN OLD;
END
$$;

CREATE TRIGGER version_approvals_are_immutable BEFORE UPDATE OR DELETE ON version_approvals
    FOR EACH ROW EXECUTE FUNCTION version_approvals_are_immutable();

-- Drafts open before editors were recorded: romulus does not know who
-- edited them, so only reviewers (who never could) may approve them.
ALTER TABLE drafts ADD COLUMN editors_unknown boolean NOT NULL DEFAULT false;
COMMENT ON COLUMN drafts.editors_unknown IS
    'Edited before draft_editors existed: its editors are unknown, so developers and admins cannot approve it (reviewers can).';
-- Not an edit of theirs: updated_at stays.
ALTER TABLE drafts DISABLE TRIGGER drafts_set_updated_at;
UPDATE drafts SET editors_unknown = true WHERE status = 'open';
ALTER TABLE drafts ENABLE TRIGGER drafts_set_updated_at;
