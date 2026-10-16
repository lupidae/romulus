-- "Update draft" rewrites the draft's text (in its Y.Doc) and then moves its
-- base here. Between the two, the new base and the text it is relative to
-- wait in the pending columns; the rewrite notes in the doc the version it
-- merged. An update that failed after its rewrite is finished by the next
-- one from those, rather than merged again against the old base (which
-- would turn edits made since into conflicts).
ALTER TABLE drafts
    ADD COLUMN pending_base_version_id uuid,
    ADD COLUMN pending_base_sql text,
    ADD CONSTRAINT drafts_pending_base_fkey FOREIGN KEY (database_id, pending_base_version_id)
        REFERENCES versions (database_id, id),
    ADD CONSTRAINT drafts_pending_base_whole
        CHECK ((pending_base_version_id IS NULL) = (pending_base_sql IS NULL));

COMMENT ON COLUMN drafts.pending_base_version_id IS
    'The base an "Update draft" in progress moves to, once its rewrite of the text is written; NULL when none is.';
COMMENT ON COLUMN drafts.pending_base_sql IS
    'The base_sql that goes with pending_base_version_id.';
