ALTER TABLE organizations DROP CONSTRAINT organizations_slug_not_reserved;
ALTER TABLE organizations ADD CONSTRAINT organizations_slug_not_reserved
    CHECK (slug NOT IN ('signin', 'new', 'invite', 'dev', 'settings', 'api', 'app')) NOT VALID;
DROP TABLE oauth_refresh_tokens;
DROP TABLE oauth_access_tokens;
DROP TABLE oauth_codes;
DROP TABLE oauth_grants;
DROP TABLE oauth_requests;
DROP TABLE oauth_clients;
