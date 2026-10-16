-- Keys moved, checks and uniques changed, a unique index replaced by a
-- unique constraint of the same name, foreign keys rebuilt around the key
-- they reference, deferrable changed, exclusion constraints.
CREATE EXTENSION IF NOT EXISTS btree_gist;
CREATE TABLE parents (
    id integer NOT NULL,
    code text NOT NULL,
    CONSTRAINT parents_pkey PRIMARY KEY (id)
);
CREATE UNIQUE INDEX parents_code_key ON parents (code);
CREATE TABLE children (
    id integer PRIMARY KEY,
    parent_id integer REFERENCES parents (id),
    parent_code text,
    amount integer CONSTRAINT children_amount_check CHECK (amount > 0),
    during tstzrange,
    room integer,
    CONSTRAINT children_code_fkey FOREIGN KEY (parent_code) REFERENCES parents (code)
);
CREATE INDEX children_amount_idx ON children (amount);
CREATE INDEX children_parent_idx ON children (parent_id);
-- ==== new ====
CREATE EXTENSION IF NOT EXISTS btree_gist;
CREATE TABLE parents (
    id integer NOT NULL,
    code text NOT NULL,
    CONSTRAINT parents_pkey PRIMARY KEY (id, code),
    CONSTRAINT parents_code_key UNIQUE (code),
    CONSTRAINT parents_id_key UNIQUE (id)
);
CREATE TABLE children (
    id integer PRIMARY KEY,
    parent_id integer REFERENCES parents (id) ON DELETE CASCADE DEFERRABLE,
    parent_code text,
    amount integer CONSTRAINT children_amount_check CHECK (amount >= 0),
    during tstzrange,
    room integer,
    CONSTRAINT children_code_fkey FOREIGN KEY (parent_code) REFERENCES parents (code),
    CONSTRAINT children_no_overlap EXCLUDE USING gist (room WITH =, during WITH &&)
);
CREATE INDEX children_amount_idx ON children (amount) WHERE amount > 10;
CREATE INDEX children_code_idx ON children (parent_code);
