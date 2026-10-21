-- Enum values cannot be dropped: each type is rebuilt without them. It fails
-- while an event holds one.
DO $$
DECLARE
    kept text;
BEGIN
    SELECT string_agg(quote_literal(enumlabel), ', ' ORDER BY enumsortorder) INTO kept
    FROM pg_enum
    WHERE enumtypid = 'audit_action'::regtype AND enumlabel NOT IN ('database_purged', 'admin_appointed');
    ALTER TYPE audit_action RENAME TO audit_action_with_retention;
    EXECUTE format('CREATE TYPE audit_action AS ENUM (%s)', kept);
    ALTER TABLE audit_events ALTER COLUMN action TYPE audit_action USING action::text::audit_action;
    DROP TYPE audit_action_with_retention;

    SELECT string_agg(quote_literal(enumlabel), ', ' ORDER BY enumsortorder) INTO kept
    FROM pg_enum
    WHERE enumtypid = 'auth_event_kind'::regtype AND enumlabel NOT IN ('account_deleted', 'organization_deleted');
    ALTER TYPE auth_event_kind RENAME TO auth_event_kind_with_retention;
    EXECUTE format('CREATE TYPE auth_event_kind AS ENUM (%s)', kept);
    ALTER TABLE auth_events ALTER COLUMN kind TYPE auth_event_kind USING kind::text::auth_event_kind;
    DROP TYPE auth_event_kind_with_retention;
END $$;

COMMENT ON TABLE databases IS 'A schema kept in romulus. Soft-deleted: deleted_at hides it, its versions stay.';
DROP INDEX invitations_pending_expires_at_idx;
DROP INDEX databases_deleted_at_idx;
DROP INDEX users_last_seen_at_idx;
ALTER TABLE users
    DROP COLUMN inactivity_retry_at,
    DROP COLUMN inactivity_failures,
    DROP COLUMN inactivity_notice_at,
    DROP COLUMN last_seen_at;

DROP TRIGGER users_set_updated_at ON users;
CREATE TRIGGER users_set_updated_at BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
