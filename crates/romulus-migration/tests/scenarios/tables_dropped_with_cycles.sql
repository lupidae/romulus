-- Tables referencing each other in a cycle, all dropped; one kept table
-- referencing a dropped one; a table likely renamed.
CREATE TABLE a (id integer PRIMARY KEY, b_id integer);
CREATE TABLE b (id integer PRIMARY KEY, a_id integer REFERENCES a (id));
ALTER TABLE a ADD CONSTRAINT a_b_fkey FOREIGN KEY (b_id) REFERENCES b (id);
CREATE TABLE c (id integer PRIMARY KEY, a_id integer REFERENCES a (id));
CREATE TABLE kept (id integer PRIMARY KEY, c_id integer REFERENCES c (id));
CREATE TABLE customers (id integer PRIMARY KEY, name text NOT NULL);
-- ==== new ====
CREATE TABLE kept (id integer PRIMARY KEY, c_id integer);
CREATE TABLE clients (id integer PRIMARY KEY, name text NOT NULL);
