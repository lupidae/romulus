-- Append-only events, partitioned three ways.
CREATE SCHEMA analytics;

CREATE TABLE analytics.events (
    id          bigint GENERATED ALWAYS AS IDENTITY,
    occurred_at timestamptz NOT NULL,
    tenant_id   integer NOT NULL,
    kind        text NOT NULL,
    payload     jsonb NOT NULL DEFAULT '{}',
    PRIMARY KEY (id, occurred_at)
) PARTITION BY RANGE (occurred_at);
CREATE TABLE analytics.events_2026_09 PARTITION OF analytics.events
    FOR VALUES FROM ('2026-09-01 00:00:00+00') TO ('2026-10-01 00:00:00+00');
CREATE TABLE analytics.events_2026_10 PARTITION OF analytics.events
    FOR VALUES FROM ('2026-10-01 00:00:00+00') TO ('2026-11-01 00:00:00+00');
CREATE TABLE analytics.events_default PARTITION OF analytics.events DEFAULT;
CREATE INDEX events_tenant_idx ON analytics.events (tenant_id, occurred_at);
CREATE INDEX events_kind_idx ON analytics.events (kind) WHERE kind <> 'heartbeat';

CREATE TABLE analytics.regional_sales (
    region  text NOT NULL,
    day     date NOT NULL,
    total   numeric(14, 2) NOT NULL DEFAULT 0
) PARTITION BY LIST (region);
CREATE TABLE analytics.regional_sales_eu PARTITION OF analytics.regional_sales FOR VALUES IN ('fr', 'de', 'es', 'it');
CREATE TABLE analytics.regional_sales_us PARTITION OF analytics.regional_sales FOR VALUES IN ('us');
CREATE TABLE analytics.regional_sales_rest PARTITION OF analytics.regional_sales DEFAULT;

CREATE TABLE analytics.sessions (
    id       uuid NOT NULL,
    user_id  bigint NOT NULL,
    started  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id)
) PARTITION BY HASH (id);
CREATE TABLE analytics.sessions_0 PARTITION OF analytics.sessions FOR VALUES WITH (MODULUS 4, REMAINDER 0);
CREATE TABLE analytics.sessions_1 PARTITION OF analytics.sessions FOR VALUES WITH (MODULUS 4, REMAINDER 1);
CREATE TABLE analytics.sessions_2 PARTITION OF analytics.sessions FOR VALUES WITH (MODULUS 4, REMAINDER 2);
CREATE TABLE analytics.sessions_3 PARTITION OF analytics.sessions FOR VALUES WITH (MODULUS 4, REMAINDER 3);
COMMENT ON TABLE analytics.sessions IS 'Spread evenly across four partitions';

-- Two levels: by region, then each region by month.
CREATE TABLE analytics.orders (
    id        bigint NOT NULL,
    region    text NOT NULL,
    placed_on date NOT NULL,
    total     numeric(12, 2) NOT NULL,
    PRIMARY KEY (id, region, placed_on)
) PARTITION BY LIST (region);
CREATE TABLE analytics.orders_eu PARTITION OF analytics.orders FOR VALUES IN ('eu')
    PARTITION BY RANGE (placed_on);
CREATE TABLE analytics.orders_eu_2026 PARTITION OF analytics.orders_eu
    FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
CREATE TABLE analytics.orders_eu_old PARTITION OF analytics.orders_eu DEFAULT;
CREATE TABLE analytics.orders_us PARTITION OF analytics.orders FOR VALUES IN ('us')
    PARTITION BY RANGE (placed_on);
CREATE TABLE analytics.orders_us_2026 PARTITION OF analytics.orders_us
    FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
CREATE INDEX orders_placed_idx ON analytics.orders (placed_on);
