-- The same column renamed with ALTER TABLE … RENAME COLUMN, as a graph edit
-- does (known): Postgres keeps every name and every view's output column,
-- so the rename is all there is to run.
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
-- edits from the graph
ALTER TABLE users RENAME COLUMN email TO email_address;
