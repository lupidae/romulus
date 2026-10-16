DROP TYPE migration_format;

-- Enum values cannot be dropped: the types are rebuilt without them, which
-- fails while an imported version exists (versions are permanent). The
-- check comparing both columns goes while their types change.
ALTER TABLE versions DROP CONSTRAINT versions_remus_json_only_by_push;

ALTER TYPE version_origin RENAME TO version_origin_with_import;
CREATE TYPE version_origin AS ENUM ('upload', 'push', 'draft');
ALTER TABLE versions ALTER COLUMN origin TYPE version_origin USING origin::text::version_origin;
DROP TYPE version_origin_with_import;

ALTER TYPE version_source_kind RENAME TO version_source_kind_with_migration;
CREATE TYPE version_source_kind AS ENUM ('sql', 'remus_json');
ALTER TABLE versions ALTER COLUMN source_kind TYPE version_source_kind USING source_kind::text::version_source_kind;
DROP TYPE version_source_kind_with_migration;

ALTER TABLE versions ADD CONSTRAINT versions_remus_json_only_by_push
    CHECK (source_kind = 'sql' OR origin = 'push');
