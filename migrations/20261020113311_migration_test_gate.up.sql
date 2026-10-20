-- A migration test's verdict as the engine gave it, the exact migration a
-- draft's test ran (what the publish gate compares), whether it checked
-- anything on rows, and a developer's acknowledgement of data loss the
-- migration does not declare.
CREATE TYPE migration_test_verdict AS ENUM (
    'passed', 'passed_up_only', 'needs_acknowledgement', 'failed'
);
ALTER TYPE migration_test_status ADD VALUE 'needs_acknowledgement' AFTER 'failed';

ALTER TABLE migration_tests
    ADD COLUMN verdict migration_test_verdict,
    ADD COLUMN gate_sha256 bytea CONSTRAINT migration_tests_gate_sha256_length
        CHECK (octet_length(gate_sha256) = 32),
    ADD COLUMN rows_checked boolean NOT NULL DEFAULT true,
    ADD COLUMN acknowledged_by uuid REFERENCES users ON DELETE SET NULL,
    ADD COLUMN acknowledged_at timestamptz,
    ADD CONSTRAINT migration_tests_acknowledged_with_who CHECK (
        acknowledged_at IS NOT NULL OR acknowledged_by IS NULL
    );

CREATE INDEX migration_tests_by_gate ON migration_tests (draft_id, gate_sha256)
    WHERE draft_id IS NOT NULL;
-- A playground's page says when a test last copied it.
CREATE INDEX migration_tests_data_playground_idx ON migration_tests (data_playground_id, started_at DESC)
    WHERE data_playground_id IS NOT NULL;
CREATE INDEX migration_tests_acknowledged_by_idx ON migration_tests (acknowledged_by)
    WHERE acknowledged_by IS NOT NULL;

COMMENT ON COLUMN migration_tests.gate_sha256 IS
    'For a draft: SHA-256 of its base version id, the up migration and the down migration as planned for publishing (renames as the draft has them, never CONCURRENTLY): publishing that exact migration counts this test.';
COMMENT ON COLUMN migration_tests.rows_checked IS
    'False when every table the migration touches was empty: nothing was checked on rows, and the publish gate does not count it.';
COMMENT ON COLUMN migration_tests.acknowledged_at IS
    'A developer acknowledged the data loss the migration does not declare (verdict needs_acknowledgement): it then counts as passed.';

ALTER TYPE audit_action ADD VALUE 'migration_test_acknowledged';
ALTER TYPE audit_action ADD VALUE 'passing_test_overridden';
