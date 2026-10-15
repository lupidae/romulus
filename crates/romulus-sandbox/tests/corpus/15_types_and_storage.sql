-- Less common column types, storage options, constraints and index kinds.
-- (No stand-alone range type: remus does not model them, so one would not
-- survive the round trip; see render_sql.)
CREATE COLLATION case_insensitive (provider = icu, locale = 'und-u-ks-level2', deterministic = false);

CREATE UNLOGGED TABLE cache_entries (
    key        text PRIMARY KEY,
    value      bytea NOT NULL,
    expires_at timestamptz NOT NULL
) WITH (fillfactor = 70);

CREATE TABLE measurements (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    sensor      text COLLATE case_insensitive NOT NULL,
    taken_at    timestamptz NOT NULL,
    reading     double precision NOT NULL,
    span        numrange,
    windows     tstzmultirange,
    ip          inet,
    mac         macaddr,
    flags       bit(8) NOT NULL DEFAULT B'00000000',
    doc         xml,
    price       money,
    duration    interval DAY TO SECOND,
    code        char(3),
    UNIQUE NULLS NOT DISTINCT (sensor, code)
);
CREATE INDEX measurements_taken_brin ON measurements USING brin (taken_at);
CREATE INDEX measurements_sensor_hash ON measurements USING hash (sensor);
CREATE INDEX measurements_recent_idx ON measurements (sensor, taken_at DESC NULLS LAST) WHERE reading IS NOT NULL;
CREATE STATISTICS measurements_stats (dependencies) ON sensor, code FROM measurements;
ALTER TABLE measurements ALTER COLUMN doc SET STORAGE EXTERNAL;
ALTER TABLE measurements ADD CONSTRAINT measurements_reading_sane CHECK (reading BETWEEN -1000 AND 1000);
