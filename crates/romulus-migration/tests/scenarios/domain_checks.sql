-- A domain with two unnamed checks loses one, then gains another: the
-- checks are dropped by the name Postgres gives unnamed ones.
CREATE EXTENSION IF NOT EXISTS citext;
CREATE DOMAIN email AS citext CHECK (VALUE ~ '^[^@]+@[^@]+$') CHECK (VALUE IS NOT NULL);
CREATE DOMAIN tag AS text CHECK (VALUE <> '') CHECK (length(VALUE) < 40) CHECK (VALUE = lower(VALUE));
-- ==== new ====
CREATE EXTENSION IF NOT EXISTS citext;
CREATE DOMAIN email AS citext CHECK (VALUE ~ '^[^@]+@[^@]+$');
CREATE DOMAIN tag AS text CHECK (VALUE <> '') CHECK (VALUE = lower(VALUE)) CHECK (length(VALUE) < 80);
