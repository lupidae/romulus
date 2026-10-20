COMMENT ON COLUMN migration_tests.gate_sha256 IS
    'For a draft: SHA-256 of its base version id, the up migration and the down migration as planned for publishing (renames as the draft has them, never CONCURRENTLY): publishing that exact migration counts this test.';
COMMENT ON COLUMN migration_tests.rows_checked IS
    'False when every table the migration touches was empty: nothing was checked on rows, and the publish gate does not count it.';
COMMENT ON COLUMN migration_tests.acknowledged_at IS
    'A developer acknowledged the data loss the migration does not declare (verdict needs_acknowledgement): it then counts as passed.';
