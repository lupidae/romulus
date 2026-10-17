CREATE OR REPLACE FUNCTION version_comments_frozen() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    thread uuid := CASE WHEN TG_OP = 'INSERT' THEN NEW.thread_id ELSE OLD.thread_id END;
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.author_id IS NULL
       AND (to_jsonb(NEW) - 'author_id') IS NOT DISTINCT FROM (to_jsonb(OLD) - 'author_id') THEN
        RETURN NEW;
    END IF;
    IF EXISTS (
        SELECT 1 FROM version_threads
        WHERE id = thread AND frozen
          AND NOT (TG_OP = 'INSERT' AND xmin = pg_current_xact_id()::xid)
    ) THEN
        RAISE EXCEPTION 'the comments of thread % are frozen', thread
            USING ERRCODE = 'check_violation', CONSTRAINT = 'version_threads_frozen';
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END
$$;

ALTER TABLE version_threads DROP CONSTRAINT version_threads_frozen_from_drafts;
ALTER TABLE version_threads ADD CONSTRAINT version_threads_frozen_from_drafts
    CHECK (frozen = (draft_thread IS NOT NULL));
