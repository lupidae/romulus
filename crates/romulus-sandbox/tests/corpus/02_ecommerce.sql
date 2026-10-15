-- A shop: enums, domains, a composite type, money as integers of cents.
CREATE EXTENSION IF NOT EXISTS citext;

CREATE TYPE order_status AS ENUM ('cart', 'placed', 'paid', 'shipped', 'delivered', 'cancelled');
CREATE TYPE address AS (line1 text, line2 text, city text, postcode text, country char(2));

CREATE DOMAIN email AS citext CHECK (VALUE ~ '^[^@]+@[^@]+$');
CREATE DOMAIN cents AS bigint NOT NULL DEFAULT 0 CHECK (VALUE >= 0);
CREATE DOMAIN sku AS text CHECK (VALUE ~ '^[A-Z0-9-]{4,32}$');
COMMENT ON DOMAIN cents IS 'Amounts are integers of cents';

CREATE TABLE customers (
    id            bigserial PRIMARY KEY,
    email         email NOT NULL UNIQUE,
    name          text NOT NULL,
    shipping      address,
    marketing_ok  boolean NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE products (
    id          serial PRIMARY KEY,
    sku         sku NOT NULL UNIQUE,
    title       text NOT NULL,
    price       cents,
    stock       integer NOT NULL DEFAULT 0 CHECK (stock >= 0),
    tags        text[] NOT NULL DEFAULT '{}',
    attributes  jsonb
);
CREATE INDEX products_tags_idx ON products USING gin (tags);
CREATE INDEX products_attributes_idx ON products USING gin (attributes jsonb_path_ops);

CREATE TABLE orders (
    id           bigserial PRIMARY KEY,
    customer_id  bigint NOT NULL REFERENCES customers (id) ON DELETE RESTRICT,
    status       order_status NOT NULL DEFAULT 'cart',
    placed_at    timestamptz,
    shipping     address,
    note         text,
    CONSTRAINT placed_orders_have_a_date CHECK (status = 'cart' OR placed_at IS NOT NULL)
);
CREATE INDEX orders_customer_idx ON orders (customer_id, placed_at DESC);
CREATE INDEX orders_open_idx ON orders (status) WHERE status IN ('placed', 'paid');

CREATE TABLE order_items (
    order_id    bigint NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    product_id  integer NOT NULL REFERENCES products (id),
    quantity    integer NOT NULL CHECK (quantity > 0),
    unit_price  cents,
    line_total  bigint GENERATED ALWAYS AS (quantity * unit_price) STORED,
    PRIMARY KEY (order_id, product_id)
);
COMMENT ON COLUMN order_items.unit_price IS 'Price when ordered, not today''s';
