DROP INDEX migration_tests_acknowledged_by_idx;
DROP INDEX migration_tests_data_playground_idx;
DROP INDEX migration_tests_by_gate;
ALTER TABLE migration_tests
    DROP CONSTRAINT migration_tests_acknowledged_with_who,
    DROP COLUMN acknowledged_at,
    DROP COLUMN acknowledged_by,
    DROP COLUMN rows_checked,
    DROP COLUMN gate_sha256,
    DROP COLUMN verdict;
DROP TYPE migration_test_verdict;

-- Enum values cannot be dropped: the types are rebuilt without the ones this
-- migration added. It fails while a row or an event holds one.
DO $$
DECLARE
    kept text;
BEGIN
    SELECT string_agg(quote_literal(enumlabel), ', ' ORDER BY enumsortorder) INTO kept
    FROM pg_enum
    WHERE enumtypid = 'audit_action'::regtype
      AND enumlabel NOT IN ('migration_test_acknowledged', 'passing_test_overridden');
    ALTER TYPE audit_action RENAME TO audit_action_with_test_gate;
    EXECUTE format('CREATE TYPE audit_action AS ENUM (%s)', kept);
    ALTER TABLE audit_events ALTER COLUMN action TYPE audit_action USING action::text::audit_action;
    DROP TYPE audit_action_with_test_gate;

    -- What compares the status goes while its type changes.
    DROP INDEX migration_tests_live_idx;
    ALTER TABLE migration_tests DROP CONSTRAINT migration_tests_finished_when_finished;
    ALTER TABLE migration_tests ALTER COLUMN status DROP DEFAULT;
    ALTER TYPE migration_test_status RENAME TO migration_test_status_with_ack;
    CREATE TYPE migration_test_status AS ENUM ('queued', 'running', 'passed', 'failed', 'error', 'cancelled');
    ALTER TABLE migration_tests ALTER COLUMN status TYPE migration_test_status
        USING status::text::migration_test_status;
    ALTER TABLE migration_tests ALTER COLUMN status SET DEFAULT 'queued';
    DROP TYPE migration_test_status_with_ack;
    ALTER TABLE migration_tests ADD CONSTRAINT migration_tests_finished_when_finished CHECK (
        (status IN ('queued', 'running')) = (finished_at IS NULL)
    );
    CREATE INDEX migration_tests_live_idx ON migration_tests (status) WHERE status IN ('queued', 'running');
END $$;
