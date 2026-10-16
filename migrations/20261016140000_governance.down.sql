-- Reverting would bring removed versions back as live ones, and number
-- the next version after the highest live one, which can give a purged
-- version's number again. So it is refused while either happened: restore
-- or (after 30 days) accept the purge, and decide by hand.
DO $$
DECLARE
    removed bigint := (SELECT count(*) FROM versions WHERE removed_at IS NOT NULL);
    purged bigint := (SELECT count(*) FROM audit_events WHERE action = 'version_purged');
BEGIN
    IF removed > 0 OR purged > 0 THEN
        RAISE EXCEPTION 'governance down: % removed and % purged versions would come back or have their numbers given again; restore them (or delete them) first', removed, purged;
    END IF;
END
$$;

ALTER TABLE drafts DROP COLUMN editors_unknown;

DROP TABLE version_approvals;
DROP FUNCTION version_approvals_are_immutable();

DROP TABLE draft_review_requests;
DROP TABLE draft_approvals;
DROP TABLE draft_editors;
ALTER TABLE organizations DROP COLUMN approvals_required;

DROP TABLE audit_events;
DROP FUNCTION audit_events_are_append_only();
DROP FUNCTION audit_events_are_not_truncated();
DROP TYPE audit_action;

DROP TABLE purged_migrations;

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

DROP INDEX versions_removed_by_idx;
DROP INDEX versions_removed_idx;
ALTER TABLE versions
    DROP CONSTRAINT versions_removal_reason_length,
    DROP CONSTRAINT versions_removal_whole,
    DROP COLUMN removal_reason,
    DROP COLUMN removed_by,
    DROP COLUMN removed_at;
