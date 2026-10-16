-- A generated column becomes plain while the column it reads changes type:
-- its expression must go before the conversion Postgres would refuse.
CREATE TABLE t (id integer PRIMARY KEY, a integer, g integer GENERATED ALWAYS AS (a * 2) STORED);
-- ==== new ====
CREATE TABLE t (id integer PRIMARY KEY, a bigint, g integer);
