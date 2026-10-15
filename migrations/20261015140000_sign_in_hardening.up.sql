-- Where a sign-in lands can carry a secret (an invitation link's token):
-- the flow cookie holds it now, so the database never stores it in clear.
ALTER TABLE oidc_flows DROP COLUMN next;

-- Email addresses are compared case-insensitively (lower() on both sides,
-- including the pending-invitation unique index). Unicode has two spellings
-- of "é" (one code point, or "e" plus a combining accent) that lower() keeps
-- apart; the API stores the composed one (NFC), and these keep it so.
ALTER TABLE users ADD CONSTRAINT users_email_nfc CHECK (email IS NFC NORMALIZED);
ALTER TABLE invitations ADD CONSTRAINT invitations_email_nfc CHECK (email IS NFC NORMALIZED);

-- Compared with Unicode case mapping whatever the database's locale (a
-- C-locale database would only fold ASCII): the indexes follow the queries.
DROP INDEX users_lower_email_idx;
CREATE INDEX users_lower_email_idx ON users (lower(email COLLATE pg_unicode_fast));
DROP INDEX invitations_pending_email_key;
CREATE UNIQUE INDEX invitations_pending_email_key
    ON invitations (org_id, lower(email COLLATE pg_unicode_fast)) WHERE accepted_at IS NULL;
