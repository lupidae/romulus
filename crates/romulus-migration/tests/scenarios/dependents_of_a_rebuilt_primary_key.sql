-- A primary key changes (another column): every foreign key to it, from
-- two tables, one of them in another schema, goes first and comes back
-- after, as do their comments.
CREATE SCHEMA billing;
CREATE TABLE customers (
    id integer NOT NULL,
    region integer NOT NULL,
    CONSTRAINT customers_pkey PRIMARY KEY (id)
);
CREATE TABLE orders (
    id integer PRIMARY KEY,
    customer_id integer CONSTRAINT orders_customer_fkey REFERENCES customers (id) ON DELETE CASCADE
);
COMMENT ON CONSTRAINT orders_customer_fkey ON orders IS 'Who ordered';
CREATE TABLE billing.invoices (
    id integer PRIMARY KEY,
    customer_id integer REFERENCES customers (id)
);
-- ==== new ====
CREATE SCHEMA billing;
CREATE TABLE customers (
    id integer NOT NULL,
    region integer NOT NULL,
    CONSTRAINT customers_pkey PRIMARY KEY (id, region),
    UNIQUE (id)
);
CREATE TABLE orders (
    id integer PRIMARY KEY,
    customer_id integer CONSTRAINT orders_customer_fkey REFERENCES customers (id) ON DELETE CASCADE
);
COMMENT ON CONSTRAINT orders_customer_fkey ON orders IS 'Who ordered';
CREATE TABLE billing.invoices (
    id integer PRIMARY KEY,
    customer_id integer REFERENCES customers (id)
);
