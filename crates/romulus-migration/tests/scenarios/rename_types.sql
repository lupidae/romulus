-- An enum, a domain and a composite renamed (detected: the same
-- definitions, their columns switch to them); a label renamed in place
-- (unsure, the names unalike: turned on);
-- casts, defaults and checks naming them follow.
CREATE TYPE mood AS ENUM ('happy', 'ok', 'sad');
CREATE DOMAIN email AS text CHECK (VALUE LIKE '%@%');
CREATE TYPE point2 AS (x integer, y integer);
CREATE TABLE people (
    id integer PRIMARY KEY,
    mood mood NOT NULL DEFAULT 'ok',
    contact email,
    home point2,
    CONSTRAINT not_sad CHECK (mood <> 'sad')
);
CREATE INDEX people_happy ON people (id) WHERE mood = 'happy';
-- decide: enum_value/public/feeling/ok/fine on
-- ==== new ====
CREATE TYPE feeling AS ENUM ('happy', 'fine', 'sad');
CREATE DOMAIN email_address AS text CHECK (VALUE LIKE '%@%');
CREATE TYPE location AS (x integer, y integer);
CREATE TABLE people (
    id integer PRIMARY KEY,
    mood feeling NOT NULL DEFAULT 'fine',
    contact email_address,
    home location,
    CONSTRAINT not_sad CHECK (mood <> 'sad')
);
CREATE INDEX people_happy ON people (id) WHERE mood = 'happy';
