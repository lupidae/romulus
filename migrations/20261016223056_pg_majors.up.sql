-- Several Postgres majors per database (docs/MVP.md, "Several Postgres
-- majors"): a database targets one major; its SQL compiles on that major's
-- sandbox and its migrations are generated for it. Every database targeted
-- 17 until now (the one sandbox ran 17).
ALTER TABLE databases ADD COLUMN pg_major smallint NOT NULL DEFAULT 17
    CONSTRAINT databases_pg_major_supported CHECK (pg_major IN (15, 16, 17, 18));

COMMENT ON COLUMN databases.pg_major IS
    'The Postgres major its schema targets (domain::PgMajor): compiles run on that major''s sandbox, migrations are generated for it. Changed only once the latest version compiles on the new major (databases::major). Supporting another major is a migration of this check and a sandbox image.';

-- What each version was compiled on (uploads, drafts, imports: the
-- sandbox) or pushed from (remus JSON: the real server), as remus's model
-- says it; and the major its database targeted then, so a push from
-- another major stays flagged after the database changes major.
ALTER TABLE versions
    ADD COLUMN server_version text GENERATED ALWAYS AS (model->>'server_version') STORED,
    ADD COLUMN pg_major smallint
        GENERATED ALWAYS AS (substring(model->>'server_version' FROM '^[0-9]{1,4}')::smallint) STORED,
    ADD COLUMN database_major smallint NOT NULL DEFAULT 17;
ALTER TABLE versions ALTER COLUMN database_major DROP DEFAULT;

COMMENT ON COLUMN versions.server_version IS
    'The model''s server_version: the sandbox''s for compiled versions, the pushing server''s for remus JSON.';
COMMENT ON COLUMN versions.pg_major IS
    'The major of server_version; NULL when it does not start with one.';
COMMENT ON COLUMN versions.database_major IS
    'The database''s pg_major when the version was created (set by a trigger, never by the caller).';

-- In a BEFORE UPDATE trigger, NEW's generated columns are not computed yet
-- (NULL): the immutability check leaves them out. They follow `model`, which
-- it still compares.
CREATE OR REPLACE FUNCTION versions_are_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    -- The updates allowed: ON DELETE SET NULL on the author columns (deleting
    -- a user or token does not delete history), and removing or restoring
    -- the version. A removal itself is never edited: only undone.
    IF (to_jsonb(NEW) - 'created_by' - 'created_by_token' - 'removed_at' - 'removed_by' - 'removal_reason'
                      - 'server_version' - 'pg_major')
           IS DISTINCT FROM (to_jsonb(OLD) - 'created_by' - 'created_by_token' - 'removed_at' - 'removed_by' - 'removal_reason'
                             - 'server_version' - 'pg_major')
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

CREATE FUNCTION versions_database_major() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.database_major := (SELECT pg_major FROM databases WHERE id = NEW.database_id);
    RETURN NEW;
END
$$;

CREATE TRIGGER versions_database_major BEFORE INSERT ON versions
    FOR EACH ROW EXECUTE FUNCTION versions_database_major();

-- A draft's last compile is of its text on one major: another major's is
-- not a result for it (the room compiles again).
ALTER TABLE draft_compiles ADD COLUMN pg_major smallint NOT NULL DEFAULT 17;
ALTER TABLE draft_compiles ALTER COLUMN pg_major DROP DEFAULT;

COMMENT ON COLUMN draft_compiles.pg_major IS
    'The major the result is of (the database''s when it compiled): a room on another major compiles again.';

ALTER TYPE audit_action ADD VALUE IF NOT EXISTS 'major_changed';
