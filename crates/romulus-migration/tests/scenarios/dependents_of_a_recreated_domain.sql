-- A domain changes its base type, so it is created again: its columns are
-- converted, and the index, check and view over them follow.
CREATE DOMAIN money_cents AS integer CHECK (VALUE >= 0);
CREATE TABLE lines (
    id integer PRIMARY KEY,
    amount money_cents NOT NULL,
    CONSTRAINT small CHECK (amount < 1000000)
);
CREATE INDEX lines_amount ON lines (amount);
CREATE VIEW big_lines AS SELECT id, amount FROM lines WHERE amount > 1000;
-- ==== new ====
CREATE DOMAIN money_cents AS bigint CHECK (VALUE >= 0);
CREATE TABLE lines (
    id integer PRIMARY KEY,
    amount money_cents NOT NULL,
    CONSTRAINT small CHECK (amount < 1000000)
);
CREATE INDEX lines_amount ON lines (amount);
CREATE VIEW big_lines AS SELECT id, amount FROM lines WHERE amount > 1000;
