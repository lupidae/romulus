-- Enum values cannot be dropped: the type is rebuilt without it, which
-- fails while such an event exists (the audit log is append only).
ALTER TYPE audit_action RENAME TO audit_action_with_comments;
CREATE TYPE audit_action AS ENUM (
    'version_removed', 'version_restored', 'version_purged',
    'version_published', 'approvals_overridden', 'approvals_required_changed',
    'major_changed'
);
ALTER TABLE audit_events ALTER COLUMN action TYPE audit_action USING action::text::audit_action;
DROP TYPE audit_action_with_comments;
