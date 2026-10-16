-- One label out, one in, at the same place: probably a rename.
CREATE TYPE size AS ENUM ('small', 'medium', 'large');
CREATE TABLE shirts (id integer PRIMARY KEY, size size);
-- ==== new ====
CREATE TYPE size AS ENUM ('small', 'regular', 'large');
CREATE TABLE shirts (id integer PRIMARY KEY, size size);
