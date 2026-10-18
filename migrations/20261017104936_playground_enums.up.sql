-- What the audit log says about playgrounds (docs/playground.md). In a
-- migration of their own: a new enum value cannot be used in the
-- transaction that adds it.
ALTER TYPE audit_action ADD VALUE 'playground_created';
ALTER TYPE audit_action ADD VALUE 'playground_deleted';
ALTER TYPE audit_action ADD VALUE 'playground_reset';
ALTER TYPE audit_action ADD VALUE 'playground_migration_run';
