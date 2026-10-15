-- Common extensions in use.
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE EXTENSION IF NOT EXISTS hstore;
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE EXTENSION IF NOT EXISTS btree_gist;
CREATE EXTENSION IF NOT EXISTS ltree;
CREATE EXTENSION IF NOT EXISTS citext;
CREATE EXTENSION IF NOT EXISTS earthdistance CASCADE;
CREATE EXTENSION IF NOT EXISTS fuzzystrmatch;

CREATE TABLE contacts (
    id         uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    email      citext NOT NULL UNIQUE,
    name       text NOT NULL,
    extra      hstore,
    api_key    bytea NOT NULL DEFAULT gen_random_bytes(32),
    token_hash text GENERATED ALWAYS AS (encode(digest(api_key, 'sha256'), 'hex')) STORED,
    lat        double precision,
    lon        double precision
);
CREATE INDEX contacts_name_trgm_idx ON contacts USING gin (name gin_trgm_ops);
CREATE INDEX contacts_location_idx ON contacts USING gist (ll_to_earth(lat, lon));
CREATE INDEX contacts_extra_idx ON contacts USING gin (extra);

CREATE TABLE rooms (id integer PRIMARY KEY, name text NOT NULL);
CREATE TABLE bookings (
    room_id  integer NOT NULL REFERENCES rooms (id),
    during   tstzrange NOT NULL,
    EXCLUDE USING gist (room_id WITH =, during WITH &&)
);

CREATE TABLE taxonomy (
    path   ltree PRIMARY KEY,
    label  text NOT NULL
);
CREATE INDEX taxonomy_path_idx ON taxonomy USING gist (path);
