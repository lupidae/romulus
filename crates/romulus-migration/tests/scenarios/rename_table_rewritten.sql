-- A table renamed by rewriting its CREATE TABLE (detected): its key, its
-- serial column's sequence and its indexes are named after it, so they are
-- renamed too; the foreign key to it, the view and another table's policy
-- reading it follow by themselves.
CREATE TABLE customers (
    id serial PRIMARY KEY,
    email text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX customers_email_idx ON customers (email);
CREATE TABLE orders (
    id integer PRIMARY KEY,
    customer_id integer REFERENCES customers (id)
);
CREATE VIEW recent_customers AS SELECT id, email FROM customers WHERE created_at > '2026-01-01';
ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
CREATE POLICY own_orders ON orders USING (EXISTS (SELECT 1 FROM customers c WHERE c.id = customer_id));
-- ==== new ====
CREATE TABLE clients (
    id serial PRIMARY KEY,
    email text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX clients_email_idx ON clients (email);
CREATE TABLE orders (
    id integer PRIMARY KEY,
    customer_id integer REFERENCES clients (id)
);
CREATE VIEW recent_customers AS SELECT id, email FROM clients WHERE created_at > '2026-01-01';
ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
CREATE POLICY own_orders ON orders USING (EXISTS (SELECT 1 FROM clients c WHERE c.id = customer_id));
