DROP TABLE draft_review_requests;
DROP TABLE draft_approvals;
DROP TABLE draft_editors;
ALTER TABLE organizations DROP COLUMN approvals_required;

DROP TABLE audit_events;
DROP FUNCTION audit_events_are_append_only();
DROP TYPE audit_action;

DROP TRIGGER versions_removal_reaches_migrations ON versions;
DROP FUNCTION versions_removal_reaches_migrations();
CREATE OR REPLACE FUNCTION version_migrations_are_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'imported migration % is immutable', OLD.migration_id
        USING ERRCODE = 'check_violation', CONSTRAINT = 'versions_are_immutable';
END
$$;
DROP INDEX version_migrations_database_id_migration_id_key;
ALTER TABLE version_migrations ADD CONSTRAINT version_migrations_database_id_migration_id_key
    UNIQUE (database_id, migration_id);
ALTER TABLE version_migrations DROP COLUMN removed;

DROP TRIGGER versions_keep_last_number ON versions;
DROP FUNCTION versions_keep_last_number();
ALTER TABLE databases DROP COLUMN last_version_number;
DROP TRIGGER databases_set_updated_at ON databases;
CREATE TRIGGER databases_set_updated_at BEFORE UPDATE ON databases
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE OR REPLACE FUNCTION versions_are_permanent() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM databases WHERE id = OLD.database_id) THEN
        RAISE EXCEPTION 'version % cannot be deleted', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'versions_are_permanent';
    END IF;
    RETURN OLD;
END
$$;

CREATE OR REPLACE FUNCTION versions_are_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF (to_jsonb(NEW) - 'created_by' - 'created_by_token')
           IS DISTINCT FROM (to_jsonb(OLD) - 'created_by' - 'created_by_token')
       OR (NEW.created_by IS NOT NULL AND NEW.created_by IS DISTINCT FROM OLD.created_by)
       OR (NEW.created_by_token IS NOT NULL AND NEW.created_by_token IS DISTINCT FROM OLD.created_by_token) THEN
        RAISE EXCEPTION 'version % is immutable', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'versions_are_immutable';
    END IF;
    RETURN NEW;
END
$$;

-- Versions removed meanwhile come back: a down migration keeps the data.
DROP INDEX versions_removed_by_idx;
DROP INDEX versions_removed_idx;
ALTER TABLE versions
    DROP CONSTRAINT versions_removal_reason_length,
    DROP CONSTRAINT versions_removal_whole,
    DROP COLUMN removal_reason,
    DROP COLUMN removed_by,
    DROP COLUMN removed_at;
