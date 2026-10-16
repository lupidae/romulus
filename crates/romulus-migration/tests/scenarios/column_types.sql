-- Widening, narrowing, crossing families and unrelated types, with
-- defaults, an index, a check, a view, a policy and a foreign key on them.
CREATE TABLE accounts (id integer PRIMARY KEY, code varchar(10) NOT NULL UNIQUE);
CREATE TABLE items (
    id integer PRIMARY KEY,
    account_id integer REFERENCES accounts (id),
    price text DEFAULT '0',
    qty bigint NOT NULL DEFAULT 1 CHECK (qty > 0),
    ref uuid,
    label varchar(40),
    seen timestamp,
    flags integer DEFAULT 0
);
CREATE INDEX items_label_idx ON items (label);
CREATE VIEW cheap_items AS SELECT id, price, qty FROM items;
ALTER TABLE items ENABLE ROW LEVEL SECURITY;
CREATE POLICY items_mine ON items USING (qty > 0);
-- ==== new ====
CREATE TABLE accounts (id bigint PRIMARY KEY, code varchar(20) NOT NULL UNIQUE);
CREATE TABLE items (
    id integer PRIMARY KEY,
    account_id bigint REFERENCES accounts (id),
    price numeric(10, 2) DEFAULT 0,
    qty integer NOT NULL DEFAULT 1 CHECK (qty > 0),
    ref bigint,
    label text,
    seen timestamptz,
    flags boolean DEFAULT false
);
CREATE INDEX items_label_idx ON items (label);
CREATE VIEW cheap_items AS SELECT id, price, qty FROM items;
ALTER TABLE items ENABLE ROW LEVEL SECURITY;
CREATE POLICY items_mine ON items USING (qty > 0);
