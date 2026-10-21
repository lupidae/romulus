-- Events of purged databases would have gone with them: they go now.
ALTER TABLE audit_events DISABLE TRIGGER audit_events_are_append_only;
DELETE FROM audit_events WHERE database_id IS NULL AND database_name IS NOT NULL AND action <> 'database_purged';
ALTER TABLE audit_events ENABLE TRIGGER audit_events_are_append_only;

CREATE OR REPLACE FUNCTION audit_events_are_append_only() RETURNS trigger
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

ALTER TABLE audit_events
    DROP CONSTRAINT audit_events_database_id_fkey,
    ADD CONSTRAINT audit_events_database_id_fkey FOREIGN KEY (database_id)
        REFERENCES databases ON DELETE CASCADE,
    DROP CONSTRAINT audit_events_database_named,
    DROP COLUMN database_name;

COMMENT ON TABLE audit_events IS
    'Who did what that the organization may need to account for (audit::Action): append only, gone only with the organization or the database.';
