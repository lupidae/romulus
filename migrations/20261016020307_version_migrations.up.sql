-- remus JSON only from pushes (as before); a migration's SQL only from an
-- import, and an import only ever stores its migration.
ALTER TABLE versions DROP CONSTRAINT versions_remus_json_only_by_push;
ALTER TABLE versions ADD CONSTRAINT versions_source_matches_origin CHECK (
    CASE source_kind
        WHEN 'remus_json' THEN origin = 'push'
        WHEN 'migration' THEN origin = 'import'
        ELSE origin <> 'import'
    END
);
COMMENT ON COLUMN versions.source IS
    'What was uploaded, pushed or published, verbatim: SQL, remus JSON (pushes), or the migration''s up SQL (imports).';

CREATE TABLE version_migrations (
    version_id uuid PRIMARY KEY,
    database_id uuid NOT NULL,
    migration_id text NOT NULL CHECK (btrim(migration_id) <> '' AND char_length(migration_id) <= 500),
    path text NOT NULL CHECK (char_length(path) <= 1000),
    format migration_format NOT NULL,
    checksum bytea NOT NULL CHECK (octet_length(checksum) = 32),
    skipped jsonb NOT NULL DEFAULT '[]',
    -- Same database as the version: imports are matched per database.
    CONSTRAINT version_migrations_version_fkey FOREIGN KEY (database_id, version_id)
        REFERENCES versions (database_id, id) ON DELETE CASCADE,
    -- Importing the same files again creates nothing: a migration is
    -- imported once per database.
    CONSTRAINT version_migrations_database_id_migration_id_key UNIQUE (database_id, migration_id)
);

COMMENT ON TABLE version_migrations IS
    'The migration file each imported version came from (origin import), one per version. Immutable, like versions.';
COMMENT ON COLUMN version_migrations.migration_id IS
    'The tool''s version of the migration (20240101120000, 1.2, blog.0002_post_slug) or, for plain folders, its path: what a later import matches on.';
COMMENT ON COLUMN version_migrations.path IS 'The file it was read from, relative to the folder imported.';
COMMENT ON COLUMN version_migrations.checksum IS
    'sha256 of the migration''s up SQL (versions.source): a later import of a changed file is refused.';
COMMENT ON COLUMN version_migrations.skipped IS
    'Statements not replayed (data, code, settings…): [{"line", "kind", "reason", "text"}], line in the file.';

CREATE FUNCTION version_migrations_are_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'imported migration % is immutable', OLD.migration_id
        USING ERRCODE = 'check_violation', CONSTRAINT = 'versions_are_immutable';
END
$$;

CREATE TRIGGER version_migrations_are_immutable BEFORE UPDATE ON version_migrations
    FOR EACH ROW EXECUTE FUNCTION version_migrations_are_immutable();
