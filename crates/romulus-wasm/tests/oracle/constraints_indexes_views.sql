CREATE EXTENSION citext;
CREATE SCHEMA billing;
CREATE TABLE users (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  email citext NOT NULL,
  org_id int NOT NULL,
  name text CHECK (name <> '' AND length(name) < 100),
  kind text NOT NULL DEFAULT 'person' CHECK (kind IN ('person', 'bot')),
  score numeric(5,2) CHECK (score BETWEEN 0 AND 100),
  price numeric CHECK (price > 0),
  bio text CHECK (bio LIKE '%x%' OR bio IS NULL),
  manager_id bigint REFERENCES users,
  UNIQUE NULLS NOT DISTINCT (org_id, email),
  CONSTRAINT users_name_len CHECK (char_length(name) >= 2) NOT VALID
);
CREATE TABLE billing.accounts (
  org_id int, code text, owner_id bigint,
  PRIMARY KEY (org_id, code),
  CONSTRAINT owner_fk FOREIGN KEY (owner_id) REFERENCES users (id) ON UPDATE CASCADE ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE lines (
  org_id int, code text, n int,
  FOREIGN KEY (org_id, code) REFERENCES billing.accounts MATCH FULL,
  CHECK (n > 0), CHECK (n < 1000)
);
CREATE UNIQUE INDEX users_lower_email ON users (lower(email::text));
CREATE INDEX ON users (org_id DESC NULLS LAST, name) INCLUDE (kind) WHERE kind = 'person' AND score > 1;
CREATE INDEX ON users (org_id);
CREATE INDEX users_bio_trgm ON users USING gin (to_tsvector('english', bio));
CREATE INDEX users_name_hash ON users USING hash (name);
COMMENT ON TABLE users IS 'People and bots';
COMMENT ON COLUMN users.email IS 'Lower-cased';
COMMENT ON CONSTRAINT owner_fk ON billing.accounts IS 'Who owns it';
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
ALTER TABLE users FORCE ROW LEVEL SECURITY;
CREATE POLICY own ON users FOR ALL TO PUBLIC USING (id = current_setting('app.user')::bigint) WITH CHECK (org_id = 1);
CREATE POLICY ins ON users AS RESTRICTIVE FOR INSERT WITH CHECK (kind = 'person');
CREATE POLICY upd ON users FOR UPDATE TO app_user, admin USING (true);
CREATE VIEW active_users AS SELECT u.id, u.email, a.code FROM users u JOIN billing.accounts a ON a.owner_id = u.id WHERE u.kind = 'person' ORDER BY u.id;
CREATE MATERIALIZED VIEW counts AS SELECT org_id, count(*) AS n FROM users GROUP BY org_id;
CREATE TABLE events (id bigint NOT NULL, at timestamptz NOT NULL, kind text) PARTITION BY RANGE (at);
CREATE TABLE events_2024 PARTITION OF events FOR VALUES FROM ('2024-01-01') TO ('2025-01-01');
CREATE TABLE events_default PARTITION OF events DEFAULT;
CREATE UNLOGGED TABLE cache (k text PRIMARY KEY, v jsonb);
ALTER TABLE lines ADD COLUMN note text DEFAULT 'n/a' NOT NULL;
ALTER TABLE lines ADD CONSTRAINT lines_note_check CHECK (note <> '');
ALTER TABLE lines RENAME COLUMN n TO qty;
ALTER TABLE lines ALTER COLUMN qty SET NOT NULL, ALTER COLUMN qty SET DEFAULT 1;
ALTER TABLE lines ALTER COLUMN code TYPE varchar(20);
CREATE TABLE old (x int); ALTER TABLE old RENAME TO older; DROP TABLE older;
CREATE TABLE tmp (a int, b int); ALTER TABLE tmp DROP COLUMN b; ALTER TABLE tmp ADD PRIMARY KEY (a);
