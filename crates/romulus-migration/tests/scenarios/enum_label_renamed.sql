-- One label out, one in, at the same place: possibly a rename, the names
-- unalike (unsure: off by default), turned on.
-- decide: enum_value/public/size/medium/regular on
CREATE TYPE size AS ENUM ('small', 'medium', 'large');
CREATE TABLE shirts (id integer PRIMARY KEY, size size);
-- ==== new ====
CREATE TYPE size AS ENUM ('small', 'regular', 'large');
CREATE TABLE shirts (id integer PRIMARY KEY, size size);
