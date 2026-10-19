-- Statements the engine skips (with a note) that do not change remus's
-- model: the model must still be the sandbox's. tests/notes.rs checks the
-- notes themselves.
CREATE SCHEMA billing;
COMMENT ON SCHEMA billing IS 'Money';
CREATE EXTENSION IF NOT EXISTS citext;
COMMENT ON EXTENSION citext IS 'Case-insensitive text';
CREATE SEQUENCE invoice_numbers;
ALTER SEQUENCE invoice_numbers INCREMENT BY 2 MINVALUE 10 START WITH 10 RESTART WITH 10;
COMMENT ON SEQUENCE invoice_numbers IS 'Printed numbers';
ALTER SEQUENCE invoice_numbers SET SCHEMA billing;
CREATE TABLE customers (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  email citext NOT NULL,
  name text NOT NULL,
  country text
) WITH (fillfactor = 90);
CREATE TABLE archive (id bigint, email text) USING heap;
CREATE INDEX customers_name_idx ON customers (name);
COMMENT ON INDEX customers_name_idx IS 'For search';
CREATE STATISTICS customers_stats (dependencies) ON email, name FROM customers;
COMMENT ON STATISTICS customers_stats IS 'Correlated';
ALTER STATISTICS customers_stats RENAME TO customers_deps;
ALTER STATISTICS customers_deps SET SCHEMA billing;
DROP STATISTICS billing.customers_deps;
CREATE COLLATION german (provider = icu, locale = 'de-DE');
COMMENT ON COLLATION german IS 'For names';
ALTER COLLATION german RENAME TO deutsch;
ALTER COLLATION deutsch SET SCHEMA billing;
DROP COLLATION billing.deutsch;
ALTER TABLE customers SET (fillfactor = 70);
ALTER TABLE customers RESET (fillfactor);
ALTER TABLE customers REPLICA IDENTITY FULL;
ALTER TABLE customers CLUSTER ON customers_name_idx;
ALTER TABLE customers SET WITHOUT CLUSTER;
ALTER TABLE customers ALTER COLUMN name SET STATISTICS 500;
ALTER TABLE customers ALTER COLUMN name SET STORAGE EXTERNAL;
ALTER TABLE customers ALTER COLUMN country SET COMPRESSION pglz;
ALTER TABLE customers ALTER COLUMN country SET (n_distinct = 200);
ALTER TABLE customers ALTER COLUMN country RESET (n_distinct);
CREATE TABLE people (id bigint, email text);
ALTER TABLE archive INHERIT people;
ALTER TABLE archive NO INHERIT people;
ALTER TABLE customers ENABLE ROW LEVEL SECURITY;
CREATE POLICY own_rows ON customers USING (country = 'FR');
COMMENT ON POLICY own_rows ON customers IS 'Each sees their own';
CREATE DOMAIN positive AS int CHECK (VALUE > 0);
COMMENT ON CONSTRAINT positive_check ON DOMAIN positive IS 'Strictly';
ALTER DOMAIN positive RENAME CONSTRAINT positive_check TO positive_is_positive;
ALTER DOMAIN positive VALIDATE CONSTRAINT positive_is_positive;
CREATE DOMAIN quantity AS int;
ALTER DOMAIN quantity SET NOT NULL;
CREATE DOMAIN label AS text NOT NULL;
ALTER DOMAIN label DROP NOT NULL;
CREATE DOMAIN code AS text;
ALTER DOMAIN code ADD CONSTRAINT code_not_null NOT NULL;
CREATE TABLE lines (id bigint, qty quantity, tag label, sku code, n positive);
