-- A purged database's audit events stay with its organization
-- (retention::purge): who published what, with which approvals, outlives the
-- schema. Their database_id becomes NULL then; database_name, recorded with
-- every event about a database, says which one it was and keeps them out of
-- the other databases' activity. Personal data in them is ids and names, as
-- deleting an account already handles.
ALTER TABLE audit_events ADD COLUMN database_name text
    CONSTRAINT audit_events_database_name_length CHECK (char_length(database_name) <= 100);

-- The append-only trigger refuses updates: off for the backfill only.
ALTER TABLE audit_events DISABLE TRIGGER audit_events_are_append_only;
UPDATE audit_events e SET database_name = d.name FROM databases d WHERE d.id = e.database_id;
UPDATE audit_events SET database_name = details ->> 'database_name'
WHERE action = 'database_purged' AND details ? 'database_name';
ALTER TABLE audit_events ENABLE TRIGGER audit_events_are_append_only;

ALTER TABLE audit_events
    ADD CONSTRAINT audit_events_database_named CHECK (database_id IS NULL OR database_name IS NOT NULL),
    DROP CONSTRAINT audit_events_database_id_fkey,
    ADD CONSTRAINT audit_events_database_id_fkey FOREIGN KEY (database_id)
        REFERENCES databases ON DELETE SET NULL;

CREATE OR REPLACE FUNCTION audit_events_are_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' THEN
        -- The updates allowed: ON DELETE SET NULL on the actor (their account
        -- deleted) and on the database (purged: its name stays).
        IF (to_jsonb(NEW) - 'actor_id' - 'database_id') IS DISTINCT FROM (to_jsonb(OLD) - 'actor_id' - 'database_id')
           OR (NEW.actor_id IS NOT NULL AND NEW.actor_id IS DISTINCT FROM OLD.actor_id)
           OR (NEW.database_id IS NOT NULL AND NEW.database_id IS DISTINCT FROM OLD.database_id) THEN
            RAISE EXCEPTION 'audit event % is immutable', OLD.id
                USING ERRCODE = 'check_violation', CONSTRAINT = 'audit_events_are_append_only';
        END IF;
        RETURN NEW;
    END IF;
    -- Deleted only with its organization (ON DELETE CASCADE: by then its row
    -- is gone).
    IF EXISTS (SELECT 1 FROM organizations WHERE id = OLD.org_id) THEN
        RAISE EXCEPTION 'audit event % cannot be deleted', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'audit_events_are_append_only';
    END IF;
    RETURN OLD;
END
$$;

COMMENT ON TABLE audit_events IS
    'Who did something the organization may need to account for (audit::Action): append only, gone only with the organization. A purged database''s events stay, database_id NULL and database_name saying which.';
COMMENT ON COLUMN audit_events.database_name IS
    'The database''s name when the event was recorded (NULL for organization-level events): an event whose database_id is NULL but has a name is about a purged database, never shown in another database''s activity.';
