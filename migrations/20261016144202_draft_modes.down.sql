-- Drops the drafts' modes and the migrations versions were written as: a
-- draft in migration mode comes back in schema mode, its migration text left
-- unused in its doc (its schema SQL as it was before the switch), and the
-- versions keep only their schema.
ALTER TABLE versions DROP CONSTRAINT versions_migration_only_from_drafts;
ALTER TABLE versions DROP COLUMN migration_sql;
ALTER TABLE drafts DROP COLUMN mode;
DROP TYPE draft_mode;
