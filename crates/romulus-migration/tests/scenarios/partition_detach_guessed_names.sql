-- A partition detached whose copy of its parent's index could not take
-- Postgres's first choice of name (an index of events holds it): the copy
-- is logs_2025_at_idx1, which the migration guesses, and drops for the new
-- table's own index. The model lists no partition's names: built from this
-- text, the copies exist under the names Postgres gave them.
CREATE TABLE events (id bigint PRIMARY KEY, logs_2025_at date);
CREATE INDEX logs_2025_at_idx ON events (logs_2025_at);
CREATE TABLE logs (
    id bigint NOT NULL,
    at date NOT NULL,
    line text,
    PRIMARY KEY (id, at)
) PARTITION BY RANGE (at);
CREATE INDEX logs_at_idx ON logs (at);
CREATE INDEX logs_line_idx ON logs (line);
CREATE TABLE logs_2025 PARTITION OF logs FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');
-- ==== new ====
CREATE TABLE events (id bigint PRIMARY KEY, logs_2025_at date);
CREATE INDEX logs_2025_at_idx ON events (logs_2025_at);
CREATE TABLE logs (
    id bigint NOT NULL,
    at date NOT NULL,
    line text,
    PRIMARY KEY (id, at)
) PARTITION BY RANGE (at);
CREATE INDEX logs_at_idx ON logs (at);
CREATE INDEX logs_line_idx ON logs (line);
CREATE TABLE logs_2025 (
    id bigint NOT NULL,
    at date NOT NULL,
    line text,
    CONSTRAINT logs_2025_pkey PRIMARY KEY (id, at)
);
CREATE INDEX logs_2025_by_day ON logs_2025 (at);
CREATE INDEX logs_2025_line_idx ON logs_2025 (line);
