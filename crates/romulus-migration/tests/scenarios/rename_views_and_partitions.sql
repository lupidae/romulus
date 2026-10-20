-- A view (read by another view) and a materialized view renamed, and a
-- partition (detected: the same query, the same parent and bound).
CREATE TABLE events (id integer NOT NULL, at date NOT NULL) PARTITION BY RANGE (at);
CREATE TABLE events_y2026 PARTITION OF events FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
CREATE VIEW event_days AS SELECT DISTINCT at FROM events;
CREATE VIEW event_day_count AS SELECT count(*) AS n FROM event_days;
CREATE MATERIALIZED VIEW event_stats AS SELECT count(*) AS n FROM events;
-- ==== new ====
CREATE TABLE events (id integer NOT NULL, at date NOT NULL) PARTITION BY RANGE (at);
CREATE TABLE events_2026 PARTITION OF events FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
CREATE VIEW days_with_events AS SELECT DISTINCT at FROM events;
CREATE VIEW event_day_count AS SELECT count(*) AS n FROM days_with_events;
CREATE MATERIALIZED VIEW event_totals AS SELECT count(*) AS n FROM events;
