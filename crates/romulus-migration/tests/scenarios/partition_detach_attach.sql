-- A partition detached and kept as a table of its own (its rows stay), and
-- a table of its own attached as a partition: never dropped and created.
CREATE TABLE logs (
    id bigint NOT NULL,
    at date NOT NULL,
    line text,
    CONSTRAINT logs_line_check CHECK (length(line) < 1000),
    PRIMARY KEY (id, at)
) PARTITION BY RANGE (at);
CREATE INDEX logs_at_idx ON logs (at);
CREATE TABLE logs_2025 PARTITION OF logs FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');
CREATE TABLE logs_2026 PARTITION OF logs FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
CREATE TABLE logs_2024 (id bigint NOT NULL, at date NOT NULL, line text, source text);
-- ==== new ====
CREATE TABLE logs (
    id bigint NOT NULL,
    at date NOT NULL,
    line text,
    CONSTRAINT logs_line_check CHECK (length(line) < 1000),
    PRIMARY KEY (id, at)
) PARTITION BY RANGE (at);
CREATE INDEX logs_at_idx ON logs (at);
CREATE TABLE logs_2024 PARTITION OF logs FOR VALUES FROM ('2024-01-01') TO ('2025-01-01');
CREATE TABLE logs_2026 PARTITION OF logs FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
CREATE TABLE logs_2025 (
    id bigint NOT NULL,
    at date NOT NULL,
    line text,
    reviewed boolean NOT NULL DEFAULT false,
    CONSTRAINT logs_line_check CHECK (length(line) < 1000),
    CONSTRAINT logs_2025_pkey PRIMARY KEY (id, at)
);
CREATE INDEX logs_2025_at_idx ON logs_2025 (at);
