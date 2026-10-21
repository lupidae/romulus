DROP INDEX versions_created_idx;
DROP INDEX drafts_open_base_idx;
DROP FUNCTION version_kept_visible(uuid, uuid);
DROP TABLE billing_notices;
DROP TABLE playground_usage;
DROP TABLE billing_events;
DROP TABLE org_billing;
DROP TYPE billing_interval;
DROP TYPE billing_plan;

-- Enum values cannot be dropped: the type is rebuilt without the ones this
-- migration added. It fails while an event holds one.
DO $$
DECLARE
    kept text;
BEGIN
    SELECT string_agg(quote_literal(enumlabel), ', ' ORDER BY enumsortorder) INTO kept
    FROM pg_enum
    WHERE enumtypid = 'audit_action'::regtype
      AND enumlabel NOT IN ('terms_accepted', 'subscription_changed', 'payment_failed',
                            'payment_succeeded', 'plan_downgraded', 'playground_hours_used_up');
    ALTER TYPE audit_action RENAME TO audit_action_with_billing;
    EXECUTE format('CREATE TYPE audit_action AS ENUM (%s)', kept);
    ALTER TABLE audit_events ALTER COLUMN action TYPE audit_action USING action::text::audit_action;
    DROP TYPE audit_action_with_billing;
END $$;
