-- Several schemas, quoted names, reserved words, comments everywhere.
CREATE SCHEMA auth;
CREATE SCHEMA billing;
CREATE SCHEMA "Reporting";

CREATE TABLE auth.accounts (
    id        uuid PRIMARY KEY,
    "user"    text NOT NULL,
    "order"   integer NOT NULL DEFAULT 0,
    "Email"   text NOT NULL
);
COMMENT ON SCHEMA auth IS 'Identity';
COMMENT ON TABLE auth.accounts IS 'Sign-in accounts';
COMMENT ON COLUMN auth.accounts."user" IS 'Login name; "user" is a reserved word';

CREATE TYPE billing.currency AS ENUM ('EUR', 'USD', 'GBP');
COMMENT ON TYPE billing.currency IS 'ISO 4217';

CREATE TABLE billing."Invoice" (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    account_id  uuid NOT NULL REFERENCES auth.accounts (id),
    currency    billing.currency NOT NULL DEFAULT 'EUR',
    "Total"     numeric(12, 2) NOT NULL,
    CONSTRAINT "Invoice_total_positive" CHECK ("Total" >= 0)
);
COMMENT ON CONSTRAINT "Invoice_total_positive" ON billing."Invoice" IS 'No credit notes here';

CREATE TABLE billing."LineItem" (
    invoice_id  bigint NOT NULL REFERENCES billing."Invoice" (id) ON DELETE CASCADE,
    position    integer NOT NULL,
    label       text NOT NULL,
    PRIMARY KEY (invoice_id, position)
);
CREATE INDEX "LineItem_label_idx" ON billing."LineItem" (lower(label));
COMMENT ON INDEX billing."LineItem_label_idx" IS 'Case-insensitive search';

CREATE VIEW "Reporting"."Revenue" AS
    SELECT i.currency, sum(i."Total") AS total FROM billing."Invoice" i GROUP BY i.currency;
COMMENT ON VIEW "Reporting"."Revenue" IS 'Per currency';
