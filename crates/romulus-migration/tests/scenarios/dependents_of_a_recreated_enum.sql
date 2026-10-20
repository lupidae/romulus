-- An enum loses a label, so it is created again: everything naming it is
-- converted or built again around it, so the old type can go.
CREATE TYPE status AS ENUM ('new', 'paid', 'void', 'lost');
CREATE DOMAIN status_code AS status CHECK (VALUE <> 'lost');
CREATE TYPE ticket AS (state status, note text);
CREATE TABLE invoices (
    id integer PRIMARY KEY,
    state status NOT NULL DEFAULT 'new',
    history status[],
    code status_code,
    kind text DEFAULT ('paid'::status)::text,
    CONSTRAINT not_void CHECK (state <> 'void'),
    CONSTRAINT kind_check CHECK (kind::status <> 'lost')
);
CREATE INDEX invoices_paid ON invoices (id) WHERE state = 'paid';
CREATE INDEX invoices_void ON invoices ((state = 'void'));
CREATE VIEW paid_invoices AS SELECT id, state FROM invoices WHERE state = 'paid';
ALTER TABLE invoices ENABLE ROW LEVEL SECURITY;
CREATE POLICY not_new ON invoices USING (state <> 'new');
-- ==== new ====
CREATE TYPE status AS ENUM ('new', 'paid', 'void');
CREATE DOMAIN status_code AS status CHECK (VALUE <> 'void');
CREATE TYPE ticket AS (state status, note text);
CREATE TABLE invoices (
    id integer PRIMARY KEY,
    state status NOT NULL DEFAULT 'new',
    history status[],
    code status_code,
    kind text DEFAULT ('paid'::status)::text,
    CONSTRAINT not_void CHECK (state <> 'void'),
    CONSTRAINT kind_check CHECK (kind::status <> 'void')
);
CREATE INDEX invoices_paid ON invoices (id) WHERE state = 'paid';
CREATE INDEX invoices_void ON invoices ((state = 'void'));
CREATE VIEW paid_invoices AS SELECT id, state FROM invoices WHERE state = 'paid';
ALTER TABLE invoices ENABLE ROW LEVEL SECURITY;
CREATE POLICY not_new ON invoices USING (state <> 'new');
