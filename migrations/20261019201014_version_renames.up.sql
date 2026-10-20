-- The renames a version was published with (task MIG2): which removed and
-- added objects its migration from the version before it treated as one
-- object renamed (`ALTER … RENAME`), and which it left as a drop and an add.
-- The migration between the two versions is planned again with them, so it
-- is the one the person saw when publishing.
ALTER TABLE versions ADD COLUMN renames jsonb NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE versions ADD CONSTRAINT versions_renames_is_an_object
    CHECK (jsonb_typeof(renames) = 'object');

COMMENT ON COLUMN versions.renames IS
    'romulus_migration::RenameChoices of the migration from the version before: `known` (the renames it ran, old name to new, in order) and `decisions` (the detected renames left off, by key, false). {} for a version not published from a draft. Immutable, like the rest of the row.';
