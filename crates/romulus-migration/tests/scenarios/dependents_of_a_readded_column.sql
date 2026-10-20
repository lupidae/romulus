-- A column becomes generated, so it is dropped and added again: its
-- comment, its unique key and the foreign key to it from another table,
-- its index, the view over it and the policy naming it come back after.
CREATE TABLE products (
    id integer PRIMARY KEY,
    price integer NOT NULL,
    sku text,
    CONSTRAINT products_sku_key UNIQUE (sku)
);
COMMENT ON COLUMN products.sku IS 'Stock keeping unit';
CREATE INDEX products_sku_lower ON products (lower(sku));
CREATE TABLE stock (id integer PRIMARY KEY, sku text REFERENCES products (sku));
CREATE VIEW skus AS SELECT id, sku FROM products;
ALTER TABLE products ENABLE ROW LEVEL SECURITY;
CREATE POLICY visible ON products USING (sku IS NOT NULL);
-- ==== new ====
CREATE TABLE products (
    id integer PRIMARY KEY,
    price integer NOT NULL,
    sku text GENERATED ALWAYS AS ('P' || id::text) STORED,
    CONSTRAINT products_sku_key UNIQUE (sku)
);
COMMENT ON COLUMN products.sku IS 'Stock keeping unit';
CREATE INDEX products_sku_lower ON products (lower(sku));
CREATE TABLE stock (id integer PRIMARY KEY, sku text REFERENCES products (sku));
CREATE VIEW skus AS SELECT id, sku FROM products;
ALTER TABLE products ENABLE ROW LEVEL SECURITY;
CREATE POLICY visible ON products USING (sku IS NOT NULL);
