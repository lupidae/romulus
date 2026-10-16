-- A relocatable extension moved to its own schema, a column of its type
-- following it. The sandbox's compile role cannot move an extension: the
-- tool runs (as the cluster's superuser) check this one.
CREATE EXTENSION IF NOT EXISTS citext;
CREATE TABLE users (id integer PRIMARY KEY, email citext NOT NULL);
-- ==== new ====
CREATE SCHEMA ext;
CREATE EXTENSION IF NOT EXISTS citext WITH SCHEMA ext;
CREATE TABLE users (id integer PRIMARY KEY, email ext.citext NOT NULL);
