-- A playground restarted to undo a role changed from inside it (its
-- password or settings): everyone in it was paused, so the audit log says
-- so. In a migration of its own: a new enum value cannot be used in the
-- transaction that adds it.
ALTER TYPE audit_action ADD VALUE 'playground_restarted';
