-- Domains: base type changed (recreated), checks added and dropped,
-- default and NOT NULL toggled. Composites: attributes added, dropped,
-- converted. A domain over an enum that is recreated.
CREATE DOMAIN email AS text CHECK (VALUE ~ '@');
CREATE DOMAIN cents AS integer NOT NULL DEFAULT 0 CHECK (VALUE >= 0);
CREATE DOMAIN code AS varchar(5);
CREATE TYPE address AS (street text, city text, zip integer);
CREATE TYPE color AS ENUM ('red', 'green', 'blue');
CREATE DOMAIN warm AS color CHECK (VALUE <> 'blue');
CREATE TABLE contacts (id integer PRIMARY KEY, email email, balance cents, code code, tint warm);
-- ==== new ====
CREATE DOMAIN email AS text CHECK (VALUE ~ '@') CHECK (length(VALUE) < 255);
CREATE DOMAIN cents AS bigint DEFAULT 0 CHECK (VALUE >= 0);
CREATE DOMAIN code AS varchar(5) NOT NULL DEFAULT 'none';
CREATE TYPE address AS (street text, city varchar(80), country text);
CREATE TYPE color AS ENUM ('red', 'green');
CREATE DOMAIN warm AS color CHECK (VALUE <> 'green');
CREATE TABLE contacts (id integer PRIMARY KEY, email email, balance cents, code code, tint warm);
