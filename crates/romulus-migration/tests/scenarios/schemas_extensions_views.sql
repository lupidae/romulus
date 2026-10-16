-- New schemas, extensions added and dropped, views over views changed in
-- the middle of the chain, a view dropped with its dependents, an unlogged
-- table logged.
CREATE EXTENSION IF NOT EXISTS citext;
CREATE TABLE base (id integer PRIMARY KEY, name citext, n integer);
CREATE VIEW level1 AS SELECT id, name, n FROM base;
CREATE VIEW level2 AS SELECT id, name FROM level1 WHERE n > 0;
CREATE VIEW level3 AS SELECT id FROM level2;
CREATE VIEW doomed AS SELECT id FROM base;
CREATE VIEW doomed_too AS SELECT id FROM doomed;
CREATE UNLOGGED TABLE cache (k text PRIMARY KEY, v text);
-- ==== new ====
CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE SCHEMA reporting;
CREATE TABLE base (id integer PRIMARY KEY, name text, n integer);
CREATE VIEW level1 AS SELECT id, name, n, n * 2 AS twice FROM base;
CREATE VIEW level2 AS SELECT id, name FROM level1 WHERE n > 0;
CREATE VIEW level3 AS SELECT id FROM level2;
CREATE TABLE reporting.runs (id uuid PRIMARY KEY DEFAULT gen_random_uuid(), base_id integer REFERENCES base (id));
CREATE VIEW reporting.latest AS SELECT r.id, b.name FROM reporting.runs r JOIN level1 b ON b.id = r.base_id;
CREATE TABLE cache (k text PRIMARY KEY, v text);
