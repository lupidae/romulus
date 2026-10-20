-- The column a materialized view reads changes type: the view (and its
-- indexes, and its comment) are dropped first and built again.
CREATE TABLE readings (id integer PRIMARY KEY, value integer NOT NULL);
CREATE MATERIALIZED VIEW reading_stats AS SELECT count(*) AS n, max(value) AS top FROM readings;
CREATE UNIQUE INDEX reading_stats_n ON reading_stats (n);
COMMENT ON MATERIALIZED VIEW reading_stats IS 'Refreshed nightly';
-- ==== new ====
CREATE TABLE readings (id integer PRIMARY KEY, value numeric NOT NULL);
CREATE MATERIALIZED VIEW reading_stats AS SELECT count(*) AS n, max(value) AS top FROM readings;
CREATE UNIQUE INDEX reading_stats_n ON reading_stats (n);
COMMENT ON MATERIALIZED VIEW reading_stats IS 'Refreshed nightly';
