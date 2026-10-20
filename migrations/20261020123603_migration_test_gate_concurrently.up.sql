-- What the gate key and rows_checked mean now: the key is of the
-- migration as the test ran it (CONCURRENTLY or not: publishing accepts
-- either form), rows_checked is about the tables the migration changes,
-- and an acknowledged up-only test stays up only.

COMMENT ON COLUMN migration_tests.gate_sha256 IS
    'For a draft: SHA-256 of its base version id, the up migration and the down migration as planned for publishing (renames as the draft has them, CONCURRENTLY as the test ran it; the gate accepts either form): publishing that exact migration counts this test.';
COMMENT ON COLUMN migration_tests.rows_checked IS
    'False when every table the migration changes (something rows matter to: data changed, a constraint validated...) was empty: nothing was checked on rows, and the publish gate does not count it. Renames, nullable columns and plain indexes need no rows.';
COMMENT ON COLUMN migration_tests.acknowledged_at IS
    'A developer acknowledged the data loss the migration does not declare (verdict needs_acknowledgement): it then counts as passed, an up-only test as passed_up_only (never enough for the gate). Where the organization requires passing tests, not the draft''s author or editors unless an admin.';
