-- The up migration removes a label (the type is recreated); its down
-- migration adds it back and uses it right away, so the value must be
-- committed before the rest of the down migration, in every tool.
CREATE TYPE mood AS ENUM ('ok', 'sad');
CREATE TABLE days (d date PRIMARY KEY, mood mood NOT NULL DEFAULT 'sad');
-- ==== new ====
CREATE TYPE mood AS ENUM ('ok');
CREATE TABLE days (d date PRIMARY KEY, mood mood NOT NULL DEFAULT 'ok');
