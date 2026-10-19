DROP TABLE auth_events;
DROP FUNCTION auth_events_are_not_truncated();
DROP FUNCTION auth_events_are_append_only();
DROP TYPE auth_event_kind;
DROP TABLE auth_links;
DROP TYPE auth_link_purpose;
DROP TABLE sign_in_devices;
DROP TABLE sign_in_failures;
DROP TABLE password_credentials;
DROP INDEX users_password_email_key;
COMMENT ON TABLE users IS
    'Identity comes from Zitadel (OIDC); this row mirrors the claims at the last sign-in.';
COMMENT ON COLUMN users.oidc_subject IS 'The IdP''s `sub` claim: the only stable identity. Emails change.';
