-- A column renamed and widened at once: offered unsure (off), turned on
-- here, so it is renamed then converted, never dropped and added.
-- decide: column/public/accounts/code/account_code on
CREATE TABLE accounts (
    id integer PRIMARY KEY,
    code varchar(10) NOT NULL DEFAULT 'none',
    balance integer
);
CREATE INDEX accounts_code ON accounts (code);
-- ==== new ====
CREATE TABLE accounts (
    id integer PRIMARY KEY,
    account_code varchar(64) NOT NULL DEFAULT 'unknown',
    balance integer
);
CREATE INDEX accounts_code ON accounts (account_code);
