ALTER TABLE organizations DROP COLUMN require_passing_test;

-- Enum values cannot be dropped: the type is rebuilt without the one this
-- migration added, keeping every other value. It fails while such an event
-- exists (the audit log is append only).
DO $$
DECLARE
    kept text;
BEGIN
    SELECT string_agg(quote_literal(enumlabel), ', ' ORDER BY enumsortorder) INTO kept
    FROM pg_enum
    WHERE enumtypid = 'audit_action'::regtype
      AND enumlabel <> 'passing_test_required_changed';
    ALTER TYPE audit_action RENAME TO audit_action_with_passing_test;
    EXECUTE format('CREATE TYPE audit_action AS ENUM (%s)', kept);
    ALTER TABLE audit_events ALTER COLUMN action TYPE audit_action USING action::text::audit_action;
    DROP TYPE audit_action_with_passing_test;
END $$;
