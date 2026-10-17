-- A frozen thread's comments never change, not even in the transaction
-- that froze it (the last migration let that transaction add comments, by
-- xmin). Publishing now writes a draft's threads unfrozen, their comments,
-- then freezes them (versions::discussion::freeze), all in one transaction:
-- once frozen, nothing is added, changed or removed.

ALTER TABLE version_threads DROP CONSTRAINT version_threads_frozen_from_drafts;
ALTER TABLE version_threads ADD CONSTRAINT version_threads_frozen_from_drafts
    CHECK (NOT frozen OR draft_thread IS NOT NULL);

CREATE OR REPLACE FUNCTION version_comments_frozen() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    thread uuid := CASE WHEN TG_OP = 'INSERT' THEN NEW.thread_id ELSE OLD.thread_id END;
BEGIN
    -- The one update allowed: the author's account gone (ON DELETE SET NULL).
    IF TG_OP = 'UPDATE' AND NEW.author_id IS NULL
       AND (to_jsonb(NEW) - 'author_id') IS NOT DISTINCT FROM (to_jsonb(OLD) - 'author_id') THEN
        RETURN NEW;
    END IF;
    -- A frozen thread's comments are history. (They go with the thread,
    -- when its version is purged: by then the thread's row is gone.)
    IF EXISTS (SELECT 1 FROM version_threads WHERE id = thread AND frozen) THEN
        RAISE EXCEPTION 'the comments of thread % are frozen', thread
            USING ERRCODE = 'check_violation', CONSTRAINT = 'version_threads_frozen';
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END
$$;
