-- Keys, an index, a policy and a composite's attribute renamed alone
-- (detected: the same definitions under other names).
CREATE TYPE pair AS (left_value integer, right_value integer);
CREATE TABLE items (
    id integer CONSTRAINT items_pk PRIMARY KEY,
    sku text CONSTRAINT items_sku_unique UNIQUE,
    qty integer CONSTRAINT qty_positive CHECK (qty > 0)
);
CREATE INDEX items_by_qty ON items (qty);
CREATE TABLE stock (id integer PRIMARY KEY, item_id integer CONSTRAINT stock_item REFERENCES items (id));
ALTER TABLE items ENABLE ROW LEVEL SECURITY;
CREATE POLICY items_read ON items FOR SELECT USING (qty > 0);
-- ==== new ====
CREATE TYPE pair AS (left_value integer, right_value_2 integer);
CREATE TABLE items (
    id integer CONSTRAINT items_pkey PRIMARY KEY,
    sku text CONSTRAINT items_sku_key UNIQUE,
    qty integer CONSTRAINT items_qty_check CHECK (qty > 0)
);
CREATE INDEX items_qty_idx ON items (qty);
CREATE TABLE stock (id integer PRIMARY KEY, item_id integer CONSTRAINT stock_item_id_fkey REFERENCES items (id));
ALTER TABLE items ENABLE ROW LEVEL SECURITY;
CREATE POLICY items_select ON items FOR SELECT USING (qty > 0);
