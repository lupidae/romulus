-- Comment threads (docs/MVP.md "Review round 1", item 6). A draft's threads
-- live in its Y.Doc (`threads` and `comments`, collab::comments), checked and
-- stamped by its room; publishing freezes them into the version, here, as
-- review history (read-only). A version also takes new threads on its page.
-- Never into COMMENT ON: database comments stay what they are.

CREATE TABLE version_threads (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    version_id uuid NOT NULL REFERENCES versions ON DELETE CASCADE,
    -- The draft it was discussed in, and its id in that draft's doc (a
    -- thread from the review); NULL for one started on the version's page.
    draft_id uuid REFERENCES drafts ON DELETE SET NULL,
    draft_thread uuid,
    frozen boolean NOT NULL,
    anchor jsonb NOT NULL CHECK (jsonb_typeof(anchor) = 'object' AND pg_column_size(anchor) <= 4096),
    created_by uuid REFERENCES users ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz,
    resolved_by uuid REFERENCES users ON DELETE SET NULL,
    -- Their name then (the history reads the same once they rename or leave).
    resolved_by_name text,
    CONSTRAINT version_threads_resolved_whole CHECK ((resolved_at IS NULL) = (resolved_by_name IS NULL)),
    CONSTRAINT version_threads_frozen_from_drafts CHECK (frozen = (draft_thread IS NOT NULL))
);

COMMENT ON TABLE version_threads IS
    'Comment threads of a version: its draft''s review, frozen when it was published (read-only), and those started on its page since.';
COMMENT ON COLUMN version_threads.anchor IS
    'What the thread is pinned to, as in the draft''s doc: {"kind": "table", "table"}, {"kind": "column", "table", "column"}, {"kind": "point", "x", "y"} (graph coordinates) or {"kind": "statement", "statement"} (the head of a statement); table and column ones may carry the statement they were written above.';
COMMENT ON COLUMN version_threads.frozen IS
    'From the draft''s review: kept as it was when the draft was published, never changed (a trigger refuses it).';

CREATE INDEX version_threads_version_id_idx ON version_threads (version_id, created_at);
CREATE INDEX version_threads_draft_id_idx ON version_threads (draft_id) WHERE draft_id IS NOT NULL;
CREATE INDEX version_threads_created_by_idx ON version_threads (created_by) WHERE created_by IS NOT NULL;
CREATE INDEX version_threads_resolved_by_idx ON version_threads (resolved_by) WHERE resolved_by IS NOT NULL;

CREATE TABLE version_comments (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    thread_id uuid NOT NULL REFERENCES version_threads ON DELETE CASCADE,
    author_id uuid REFERENCES users ON DELETE SET NULL,
    -- Their name then, like the audit log's.
    author_name text NOT NULL,
    body text NOT NULL CONSTRAINT version_comments_body_length
        CHECK (btrim(body) <> '' AND char_length(body) <= 4000),
    -- Members @mentioned in the body.
    mentions uuid[] NOT NULL DEFAULT '{}' CHECK (cardinality(mentions) <= 20),
    created_at timestamptz NOT NULL DEFAULT now(),
    edited_at timestamptz
);

COMMENT ON TABLE version_comments IS
    'The comments of a version''s threads, oldest first; those of a frozen thread are never changed.';

CREATE INDEX version_comments_thread_id_idx ON version_comments (thread_id, created_at);
CREATE INDEX version_comments_author_id_idx ON version_comments (author_id) WHERE author_id IS NOT NULL;

-- A frozen thread is history: its row changes only by ON DELETE SET NULL
-- (an account, the draft gone), and goes only with its version.
CREATE FUNCTION version_threads_frozen() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' THEN
        IF OLD.frozen AND (
               (to_jsonb(NEW) - 'created_by' - 'resolved_by' - 'draft_id')
                   IS DISTINCT FROM (to_jsonb(OLD) - 'created_by' - 'resolved_by' - 'draft_id')
            OR (NEW.created_by IS NOT NULL AND NEW.created_by IS DISTINCT FROM OLD.created_by)
            OR (NEW.resolved_by IS NOT NULL AND NEW.resolved_by IS DISTINCT FROM OLD.resolved_by)
            OR (NEW.draft_id IS NOT NULL AND NEW.draft_id IS DISTINCT FROM OLD.draft_id)) THEN
            RAISE EXCEPTION 'thread % is frozen', OLD.id
                USING ERRCODE = 'check_violation', CONSTRAINT = 'version_threads_frozen';
        END IF;
        RETURN NEW;
    END IF;
    IF OLD.frozen AND EXISTS (SELECT 1 FROM versions WHERE id = OLD.version_id) THEN
        RAISE EXCEPTION 'thread % is frozen', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'version_threads_frozen';
    END IF;
    RETURN OLD;
END
$$;

CREATE TRIGGER version_threads_frozen BEFORE UPDATE OR DELETE ON version_threads
    FOR EACH ROW EXECUTE FUNCTION version_threads_frozen();

CREATE FUNCTION version_comments_frozen() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    thread uuid := CASE WHEN TG_OP = 'INSERT' THEN NEW.thread_id ELSE OLD.thread_id END;
BEGIN
    -- The one update allowed: the author's account gone (ON DELETE SET NULL).
    IF TG_OP = 'UPDATE' AND NEW.author_id IS NULL
       AND (to_jsonb(NEW) - 'author_id') IS NOT DISTINCT FROM (to_jsonb(OLD) - 'author_id') THEN
        RETURN NEW;
    END IF;
    -- A frozen thread's comments are written with it, in the transaction
    -- publishing the draft (which inserted the thread), and never after.
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

CREATE TRIGGER version_comments_frozen BEFORE INSERT OR UPDATE OR DELETE ON version_comments
    FOR EACH ROW EXECUTE FUNCTION version_comments_frozen();

-- Unread markers: where each person has read a thread up to. A draft's
-- threads are keys of its doc; a version's are rows above.
CREATE TABLE draft_thread_reads (
    draft_id uuid NOT NULL REFERENCES drafts ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    thread uuid NOT NULL,
    read_at timestamptz NOT NULL,
    PRIMARY KEY (draft_id, user_id, thread)
);
COMMENT ON TABLE draft_thread_reads IS
    'Up to when each person read each thread of a draft (its key in the doc): comments after that, by others, are unread.';
CREATE INDEX draft_thread_reads_user_id_idx ON draft_thread_reads (user_id);

CREATE TABLE version_thread_reads (
    thread_id uuid NOT NULL REFERENCES version_threads ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    read_at timestamptz NOT NULL,
    PRIMARY KEY (thread_id, user_id)
);
COMMENT ON TABLE version_thread_reads IS
    'Up to when each person read each thread of a version (carried over from the draft''s when it was published).';
CREATE INDEX version_thread_reads_user_id_idx ON version_thread_reads (user_id);
