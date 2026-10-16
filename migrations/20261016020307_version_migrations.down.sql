DROP TABLE version_migrations;
DROP FUNCTION version_migrations_are_immutable();

ALTER TABLE versions DROP CONSTRAINT versions_source_matches_origin;
ALTER TABLE versions ADD CONSTRAINT versions_remus_json_only_by_push
    CHECK (source_kind = 'sql' OR origin = 'push');
COMMENT ON COLUMN versions.source IS 'What was uploaded or published, verbatim (SQL, or remus JSON for pushes).';
