-- Enum values cannot be dropped: the type is rebuilt without this one,
-- keeping every other value whoever added it. It fails while such an event
-- exists (the audit log is append only).
DO $$
DECLARE
    kept text;
BEGIN
    SELECT string_agg(quote_literal(enumlabel), ', ' ORDER BY enumsortorder) INTO kept
    FROM pg_enum
    WHERE enumtypid = 'audit_action'::regtype AND enumlabel <> 'playground_restarted';
    ALTER TYPE audit_action RENAME TO audit_action_with_restarts;
    EXECUTE format('CREATE TYPE audit_action AS ENUM (%s)', kept);
    ALTER TABLE audit_events ALTER COLUMN action TYPE audit_action USING action::text::audit_action;
    DROP TYPE audit_action_with_restarts;
END $$;
