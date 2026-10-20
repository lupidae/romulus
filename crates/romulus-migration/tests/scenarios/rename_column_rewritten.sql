-- A column renamed by rewriting the CREATE TABLE (detected): its unique key
-- and check are named after it by Postgres, so they are renamed too; its
-- index, the foreign key to it, the views and the policy over it follow by
-- themselves. Views that print it under its new name are built again.
CREATE TABLE users (
    id integer PRIMARY KEY,
    email text NOT NULL UNIQUE CHECK (email <> ''),
    name text
);
CREATE INDEX users_email_lower ON users (lower(email));
CREATE TABLE invites (
    id integer PRIMARY KEY,
    sender_email text REFERENCES users (email)
);
CREATE VIEW user_emails AS SELECT id, email FROM users;
CREATE VIEW invite_senders AS
    SELECT i.id, u.email FROM invites i JOIN users u ON u.email = i.sender_email;
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
CREATE POLICY has_email ON users USING (email IS NOT NULL);
-- ==== new ====
CREATE TABLE users (
    id integer PRIMARY KEY,
    email_address text NOT NULL UNIQUE CHECK (email_address <> ''),
    name text
);
CREATE INDEX users_email_lower ON users (lower(email_address));
CREATE TABLE invites (
    id integer PRIMARY KEY,
    sender_email text REFERENCES users (email_address)
);
CREATE VIEW user_emails AS SELECT id, email_address FROM users;
CREATE VIEW invite_senders AS
    SELECT i.id, u.email_address FROM invites i JOIN users u ON u.email_address = i.sender_email;
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
CREATE POLICY has_email ON users USING (email_address IS NOT NULL);
