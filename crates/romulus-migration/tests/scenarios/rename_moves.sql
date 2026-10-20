-- Moves to another schema. Known: ALTER … SET SCHEMA in the text (a table
-- with its serial sequence, keys and indexes, a view over it, a key to it,
-- an enum), one moved then renamed. Detected: a table written again, the
-- same, in another schema.
CREATE SCHEMA archive;
CREATE TYPE mood AS ENUM ('sad', 'happy');
CREATE TABLE customers (
    id serial PRIMARY KEY,
    email text NOT NULL UNIQUE,
    mood mood NOT NULL DEFAULT 'happy'
);
CREATE INDEX customers_mood_idx ON customers (mood);
CREATE TABLE orders (
    id integer PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES customers (id)
);
CREATE VIEW happy_customers AS SELECT id, email FROM customers WHERE mood = 'happy';
CREATE TABLE audit_log (id bigint PRIMARY KEY, happened_at timestamptz NOT NULL, what text NOT NULL);
CREATE TABLE notes (id bigint PRIMARY KEY, body text NOT NULL, written_at timestamptz NOT NULL);
-- ==== new ====
CREATE SCHEMA archive;
CREATE TYPE mood AS ENUM ('sad', 'happy');
CREATE TABLE customers (
    id serial PRIMARY KEY,
    email text NOT NULL UNIQUE,
    mood mood NOT NULL DEFAULT 'happy'
);
CREATE INDEX customers_mood_idx ON customers (mood);
CREATE TABLE orders (
    id integer PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES customers (id)
);
CREATE VIEW happy_customers AS SELECT id, email FROM customers WHERE mood = 'happy';
CREATE TABLE archive.audit_log (id bigint PRIMARY KEY, happened_at timestamptz NOT NULL, what text NOT NULL);
CREATE TABLE notes (id bigint PRIMARY KEY, body text NOT NULL, written_at timestamptz NOT NULL);
ALTER TABLE customers SET SCHEMA archive;
ALTER TYPE mood SET SCHEMA archive;
ALTER TABLE notes SET SCHEMA archive;
ALTER TABLE archive.notes RENAME TO old_notes;
