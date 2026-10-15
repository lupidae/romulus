-- Places on a map.
CREATE EXTENSION IF NOT EXISTS postgis;

CREATE TABLE places (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name      text NOT NULL,
    location  geometry(Point, 4326) NOT NULL,
    area      geography(Polygon, 4326),
    CONSTRAINT places_location_valid CHECK (ST_IsValid(location))
);
CREATE INDEX places_location_idx ON places USING gist (location);
CREATE INDEX places_area_idx ON places USING gist (area);

CREATE TABLE routes (
    id     bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    path   geometry(LineString, 4326) NOT NULL,
    length_m double precision GENERATED ALWAYS AS (ST_Length(path::geography)) STORED
);

CREATE VIEW places_near_paris AS
    SELECT id, name, ST_Distance(location::geography, ST_SetSRID(ST_MakePoint(2.35, 48.85), 4326)::geography) AS meters
    FROM places;
