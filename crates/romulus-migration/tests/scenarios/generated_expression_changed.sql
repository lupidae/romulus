-- A generated column's expression changes, an index reading it: in place
-- from 17 on (SET EXPRESSION); before, dropped and added again, the index
-- built again after it.
CREATE TABLE t (id integer PRIMARY KEY, a integer, g integer GENERATED ALWAYS AS (a * 2) STORED);
CREATE INDEX t_g_idx ON t (g);
-- ==== new ====
CREATE TABLE t (id integer PRIMARY KEY, a integer, g integer GENERATED ALWAYS AS (a * 3) STORED);
CREATE INDEX t_g_idx ON t (g);
