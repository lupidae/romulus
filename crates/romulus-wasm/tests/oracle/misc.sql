CREATE SCHEMA app;
SET search_path TO app, public;
CREATE DOMAIN app.email AS text CHECK (VALUE ~* '^[^@]+@[^@]+$');
CREATE TYPE app.money_amount AS (currency char(3), cents bigint);
CREATE TABLE orgs (id serial PRIMARY KEY, slug varchar(40) NOT NULL UNIQUE, settings jsonb NOT NULL DEFAULT '{}'::jsonb, contact email);
CREATE TABLE members (
  org_id int REFERENCES orgs (id) ON DELETE CASCADE,
  user_email email NOT NULL,
  role text NOT NULL DEFAULT 'member' CHECK (role = ANY (ARRAY['admin', 'member'])),
  joined date,
  tags text[] NOT NULL DEFAULT '{}',
  price app.money_amount,
  full_name text GENERATED ALWAYS AS (upper(user_email)) STORED,
  PRIMARY KEY (org_id, user_email)
);
CREATE INDEX members_tags_idx ON members USING gin (tags);
CREATE TABLE logs (id bigint NOT NULL, kind text NOT NULL, at timestamptz NOT NULL) PARTITION BY LIST (kind);
CREATE TABLE logs_web PARTITION OF logs FOR VALUES IN ('web', 'mobile');
CREATE TABLE logs_rest PARTITION OF logs DEFAULT PARTITION BY HASH (id);
CREATE TABLE logs_rest_0 PARTITION OF logs_rest FOR VALUES WITH (MODULUS 2, REMAINDER 0);
CREATE MATERIALIZED VIEW member_counts AS SELECT org_id, count(*) AS members FROM members GROUP BY org_id;
COMMENT ON MATERIALIZED VIEW member_counts IS 'Refreshed nightly';
COMMENT ON TYPE app.email IS 'An address';
ALTER TABLE members DROP COLUMN joined;
ALTER TABLE orgs ALTER COLUMN slug TYPE text;
ALTER TABLE orgs ALTER COLUMN settings DROP DEFAULT;
CREATE TABLE scratch (a int, b int CHECK (b > a));
DROP TABLE scratch;
