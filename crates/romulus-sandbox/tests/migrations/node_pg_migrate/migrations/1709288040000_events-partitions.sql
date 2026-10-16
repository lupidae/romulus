-- Up Migration
CREATE TABLE events (
    id bigint GENERATED ALWAYS AS IDENTITY,
    organization_id uuid NOT NULL,
    kind text NOT NULL,
    payload jsonb NOT NULL DEFAULT '{}',
    occurred_at timestamptz NOT NULL,
    PRIMARY KEY (id, occurred_at)
) PARTITION BY RANGE (occurred_at);

CREATE TABLE events_2024 PARTITION OF events
    FOR VALUES FROM ('2024-01-01') TO ('2025-01-01');
CREATE TABLE events_2025 PARTITION OF events
    FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');

CREATE INDEX events_organization_idx ON events (organization_id, occurred_at DESC);

-- Down Migration
DROP TABLE events;
