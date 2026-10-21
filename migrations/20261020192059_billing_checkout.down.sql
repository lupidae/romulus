ALTER TABLE org_billing
    DROP COLUMN checkout_price,
    DROP COLUMN checkout_nonce,
    DROP COLUMN checkout_session_id;

-- Enum values cannot be dropped: the type is rebuilt without this one. It
-- fails while an event holds it.
DO $$
DECLARE
    kept text;
BEGIN
    SELECT string_agg(quote_literal(enumlabel), ', ' ORDER BY enumsortorder) INTO kept
    FROM pg_enum
    WHERE enumtypid = 'audit_action'::regtype AND enumlabel <> 'subscription_duplicate_cancelled';
    ALTER TYPE audit_action RENAME TO audit_action_with_duplicates;
    EXECUTE format('CREATE TYPE audit_action AS ENUM (%s)', kept);
    ALTER TABLE audit_events ALTER COLUMN action TYPE audit_action USING action::text::audit_action;
    DROP TYPE audit_action_with_duplicates;
END $$;
