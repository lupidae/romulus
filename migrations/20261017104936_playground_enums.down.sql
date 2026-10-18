-- Enum values cannot be dropped: the type is rebuilt without the four this
-- migration added, keeping every other value whoever added it (another
-- migration may have since). It fails while a playground event exists (the
-- audit log is append only).
DO $$
DECLARE
    kept text;
BEGIN
    SELECT string_agg(quote_literal(enumlabel), ', ' ORDER BY enumsortorder) INTO kept
    FROM pg_enum
    WHERE enumtypid = 'audit_action'::regtype
      AND enumlabel NOT IN ('playground_created', 'playground_deleted', 'playground_reset',
                            'playground_migration_run');
    ALTER TYPE audit_action RENAME TO audit_action_with_playgrounds;
    EXECUTE format('CREATE TYPE audit_action AS ENUM (%s)', kept);
    ALTER TABLE audit_events ALTER COLUMN action TYPE audit_action USING action::text::audit_action;
    DROP TYPE audit_action_with_playgrounds;
END $$;
