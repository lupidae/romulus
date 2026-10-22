ALTER TABLE organizations DROP CONSTRAINT organizations_slug_not_reserved;
ALTER TABLE organizations ADD CONSTRAINT organizations_slug_not_reserved
    CHECK (slug NOT IN ('signin', 'new', 'invite', 'dev', 'settings', 'api', 'app', 'oauth')) NOT VALID;
