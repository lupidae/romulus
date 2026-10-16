-- Drafts have two modes, one source of truth at a time (docs/MVP.md "Review
-- round 1", item 2): the schema SQL, or a migration written by hand from the
-- draft's base version, from which the schema is derived.
CREATE TYPE draft_mode AS ENUM ('schema', 'migration');

ALTER TABLE drafts ADD COLUMN mode draft_mode NOT NULL DEFAULT 'schema';

COMMENT ON COLUMN drafts.mode IS
    'What the draft''s Y.Doc holds as its source: the schema SQL (`sql`), or a migration from the base version (`migration`), compiled after the base version''s SQL. Written by the draft''s room (collab), which mirrors it in the doc''s meta.mode for clients.';

-- A version published from a draft in migration mode keeps the migration it
-- was written as: its model is what that migration made of the version
-- before it.
ALTER TABLE versions ADD COLUMN migration_sql text;
ALTER TABLE versions ADD CONSTRAINT versions_migration_only_from_drafts
    CHECK (migration_sql IS NULL OR origin = 'draft');

COMMENT ON COLUMN versions.migration_sql IS
    'The migration written by hand in the draft it was published from (migration mode), from the version before it; NULL otherwise. Immutable, like the rest of the row.';
