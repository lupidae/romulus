-- A table becomes partitioned, a view becomes materialized, a table
-- becomes a view, with foreign keys and views on them.
CREATE TABLE events (id bigint NOT NULL, at timestamptz NOT NULL, PRIMARY KEY (id, at));
CREATE TABLE notes (id integer PRIMARY KEY, body text);
CREATE TABLE totals (n integer);
CREATE VIEW recent_events AS SELECT id, at FROM events;
CREATE VIEW note_count AS SELECT count(*) AS n FROM notes;
CREATE MATERIALIZED VIEW event_days AS SELECT date_trunc('day', at) AS day, count(*) AS n FROM events GROUP BY 1;
CREATE UNIQUE INDEX event_days_day ON event_days (day);
-- ==== new ====
CREATE TABLE events (id bigint NOT NULL, at timestamptz NOT NULL, PRIMARY KEY (id, at)) PARTITION BY RANGE (at);
CREATE TABLE events_2026 PARTITION OF events FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
CREATE TABLE notes (id integer PRIMARY KEY, body text);
CREATE VIEW totals AS SELECT 1 AS n;
CREATE MATERIALIZED VIEW recent_events AS SELECT id, at FROM events;
CREATE VIEW note_count AS SELECT count(*) AS n, max(id) AS last FROM notes;
CREATE MATERIALIZED VIEW event_days AS SELECT date_trunc('day', at) AS day, count(*) AS n FROM events GROUP BY 1;
CREATE UNIQUE INDEX event_days_day ON event_days (day);
CREATE INDEX event_days_n ON event_days (n);
