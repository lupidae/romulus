-- An organization may require a passing migration test (Test on data) of a
-- draft's text before publishing it. Off by default; changing it is audited.
ALTER TYPE audit_action ADD VALUE 'passing_test_required_changed';

ALTER TABLE organizations ADD COLUMN require_passing_test boolean NOT NULL DEFAULT false;
COMMENT ON COLUMN organizations.require_passing_test IS
    'Publishing a draft that changes the schema needs a passing migration test (migration_tests) of its text as it is. Pushes and imports are never gated.';
