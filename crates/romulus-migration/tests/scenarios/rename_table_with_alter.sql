-- The same table renamed with ALTER TABLE … RENAME TO (known): Postgres
-- keeps its key's, sequence's and indexes' names.
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
-- edits from the graph
ALTER TABLE customers RENAME TO clients;
ALTER TABLE clients RENAME COLUMN email TO contact_email;
