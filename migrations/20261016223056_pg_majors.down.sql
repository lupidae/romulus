-- 'major_changed' stays in audit_action: Postgres cannot drop an enum
-- value, and the events using it are append only. Every database goes back
-- to the one major there was.
ALTER TABLE draft_compiles DROP COLUMN pg_major;

DROP TRIGGER versions_database_major ON versions;
DROP FUNCTION versions_database_major();

-- As the governance migration left it.
CREATE OR REPLACE FUNCTION versions_are_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
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
ALTER TABLE versions
    DROP COLUMN database_major,
    DROP COLUMN pg_major,
    DROP COLUMN server_version;

ALTER TABLE databases DROP COLUMN pg_major;
