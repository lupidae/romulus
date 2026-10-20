-- Test migration (docs/playground.md, "Test migration"): a migration run up
-- and down on a short-lived playground with generated rows, checked by
-- romulus-migration-check, its report kept for the organization to see.

CREATE TYPE migration_test_status AS ENUM ('queued', 'running', 'passed', 'failed', 'error', 'cancelled');
CREATE TYPE migration_test_source AS ENUM ('draft', 'versions', 'version');
CREATE TYPE migration_test_down AS ENUM ('generated', 'written', 'none');

CREATE TABLE migration_tests (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    org_id uuid NOT NULL REFERENCES organizations ON DELETE CASCADE,
    database_id uuid NOT NULL REFERENCES databases ON DELETE CASCADE,
    source migration_test_source NOT NULL,
    draft_id uuid REFERENCES drafts ON DELETE CASCADE,
    from_version_id uuid NOT NULL REFERENCES versions ON DELETE CASCADE,
    to_version_id uuid REFERENCES versions ON DELETE CASCADE,
    input_sha256 bytea CHECK (input_sha256 IS NULL OR octet_length(input_sha256) = 32),
    up_sql text NOT NULL CHECK (octet_length(up_sql) <= 4194304),
    down_sql text CHECK (down_sql IS NULL OR octet_length(down_sql) <= 4194304),
    down migration_test_down NOT NULL,
    size playground_size NOT NULL,
    profile playground_profile NOT NULL,
    seed bigint NOT NULL CHECK (seed >= 0),
    data_playground_id uuid REFERENCES playgrounds ON DELETE SET NULL,
    data_label text CHECK (data_label IS NULL OR char_length(data_label) <= 200),
    status migration_test_status NOT NULL DEFAULT 'queued',
    report jsonb,
    failure text CHECK (failure IS NULL OR char_length(failure) <= 2000),
    playground_id uuid,
    created_by uuid REFERENCES users ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz,
    finished_at timestamptz,
    CONSTRAINT migration_tests_source_shape CHECK (
        CASE source
            WHEN 'draft' THEN draft_id IS NOT NULL AND input_sha256 IS NOT NULL
            ELSE draft_id IS NULL AND to_version_id IS NOT NULL
        END
    ),
    CONSTRAINT migration_tests_down_shape CHECK ((down = 'none') = (down_sql IS NULL)),
    CONSTRAINT migration_tests_finished_when_finished CHECK (
        (status IN ('queued', 'running')) = (finished_at IS NULL)
    )
);

COMMENT ON TABLE migration_tests IS
    'A migration tested on rows: up and down on a playground made for it (deleted after), checked by romulus-migration-check. Visible to the organization; the history of a draft, a pair of versions or a version''s own migration.';
COMMENT ON COLUMN migration_tests.source IS
    'draft: the draft''s migration from its base (input_sha256 = what was reviewed, compile::input_hash); versions: the migration planned from from_version to to_version; version: the migration to_version was written or imported as, from from_version.';
COMMENT ON COLUMN migration_tests.input_sha256 IS
    'For a draft: the reviewed hash of the text tested (publish::reviewed), so the publish dialog can say "passed for this text".';
COMMENT ON COLUMN migration_tests.down IS
    'Where the down migration came from: generated (romulus-migration, mirroring up), written (with the migration), none (up only).';
COMMENT ON COLUMN migration_tests.report IS
    'romulus_migration_check::Report as it serializes, once the test ran.';
COMMENT ON COLUMN migration_tests.data_playground_id IS
    'The playground whose rows the test ran on (a copy of them: it is never changed); NULL for generated rows (size, profile, seed), or once that playground is deleted (data_label keeps what it was).';
COMMENT ON COLUMN migration_tests.data_label IS
    'What the report calls the rows of a playground: "Alice''s playground (small, seed 7f3a91)".';
COMMENT ON COLUMN migration_tests.playground_id IS
    'The short-lived playground it runs on, while it lives (the playground sweeper spares it); NULL before and after.';

CREATE INDEX migration_tests_database_id_idx ON migration_tests (database_id, created_at DESC);
CREATE INDEX migration_tests_draft_id_idx ON migration_tests (draft_id, created_at DESC) WHERE draft_id IS NOT NULL;
CREATE INDEX migration_tests_live_idx ON migration_tests (status) WHERE status IN ('queued', 'running');
CREATE INDEX migration_tests_from_version_id_idx ON migration_tests (from_version_id);
CREATE INDEX migration_tests_to_version_id_idx ON migration_tests (to_version_id) WHERE to_version_id IS NOT NULL;
