DROP INDEX invitations_pending_email_key;
CREATE UNIQUE INDEX invitations_pending_email_key ON invitations (org_id, lower(email))
    WHERE accepted_at IS NULL;
DROP INDEX users_lower_email_idx;
CREATE INDEX users_lower_email_idx ON users (lower(email));
ALTER TABLE invitations DROP CONSTRAINT invitations_email_nfc;
ALTER TABLE users DROP CONSTRAINT users_email_nfc;
ALTER TABLE oidc_flows ADD COLUMN next text NOT NULL DEFAULT '/app/';
