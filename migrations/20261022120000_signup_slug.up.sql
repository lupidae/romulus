-- The subscribe link of the homepage's plans is /app/signup, a top-level
-- route of the web app: an organization may not be called that
-- (orgs::slug::RESERVED). NOT VALID: an organization that already took the
-- name keeps it.
ALTER TABLE organizations DROP CONSTRAINT organizations_slug_not_reserved;
ALTER TABLE organizations ADD CONSTRAINT organizations_slug_not_reserved
    CHECK (slug NOT IN ('signin', 'signup', 'new', 'invite', 'dev', 'settings', 'api', 'app', 'oauth')) NOT VALID;
