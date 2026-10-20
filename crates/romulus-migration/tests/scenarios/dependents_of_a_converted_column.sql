-- A column changes type under everything that can depend on it: an
-- expression index, a partial index, a check, a unique key a foreign key
-- references, a view (and a view over it), a policy, a generated column,
-- a comment. What Postgres refuses to convert under is dropped first and
-- built again after; what it rebuilds itself is left to it.
CREATE TABLE accounts (
    id integer PRIMARY KEY,
    code varchar(10) NOT NULL UNIQUE,
    score integer NOT NULL DEFAULT 0 CHECK (score >= 0),
    label text GENERATED ALWAYS AS (code || '!') STORED
);
COMMENT ON COLUMN accounts.code IS 'Short code';
CREATE INDEX accounts_code_lower ON accounts (lower(code));
CREATE INDEX accounts_high ON accounts (score) WHERE score > 10;
CREATE TABLE payments (
    id integer PRIMARY KEY,
    account_code varchar(10) REFERENCES accounts (code),
    amount integer
);
CREATE VIEW account_codes AS SELECT id, code, score FROM accounts;
CREATE VIEW top_codes AS SELECT code FROM account_codes WHERE score > 100;
ALTER TABLE accounts ENABLE ROW LEVEL SECURITY;
CREATE POLICY own_code ON accounts USING (code <> 'hidden');
-- ==== new ====
CREATE TABLE accounts (
    id integer PRIMARY KEY,
    code text NOT NULL UNIQUE,
    score bigint NOT NULL DEFAULT 0 CHECK (score >= 0),
    label text GENERATED ALWAYS AS (code || '!') STORED
);
COMMENT ON COLUMN accounts.code IS 'Short code';
CREATE INDEX accounts_code_lower ON accounts (lower(code));
CREATE INDEX accounts_high ON accounts (score) WHERE score > 10;
CREATE TABLE payments (
    id integer PRIMARY KEY,
    account_code text REFERENCES accounts (code),
    amount integer
);
CREATE VIEW account_codes AS SELECT id, code, score FROM accounts;
CREATE VIEW top_codes AS SELECT code FROM account_codes WHERE score > 100;
ALTER TABLE accounts ENABLE ROW LEVEL SECURITY;
CREATE POLICY own_code ON accounts USING (code <> 'hidden');
