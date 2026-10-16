-- A label removed: the type is recreated, columns (scalar and array),
-- defaults, checks, partial indexes and views converted.
CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy', 'ecstatic');
CREATE TABLE people (
    id integer PRIMARY KEY,
    mood mood NOT NULL DEFAULT 'ok',
    history mood[] NOT NULL DEFAULT ARRAY['ok']::mood[],
    CHECK (mood <> 'sad' OR id > 0)
);
CREATE INDEX people_happy_idx ON people (id) WHERE mood IN ('happy', 'ecstatic');
CREATE VIEW happy_people AS SELECT id, mood FROM people WHERE mood = 'happy';
CREATE DOMAIN good_mood AS mood CHECK (VALUE <> 'sad');
CREATE TABLE days (day date PRIMARY KEY, overall good_mood);
-- ==== new ====
CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
CREATE TABLE people (
    id integer PRIMARY KEY,
    mood mood NOT NULL DEFAULT 'ok',
    history mood[] NOT NULL DEFAULT ARRAY['ok']::mood[],
    CHECK (mood <> 'sad' OR id > 0)
);
CREATE INDEX people_happy_idx ON people (id) WHERE mood IN ('happy');
CREATE VIEW happy_people AS SELECT id, mood FROM people WHERE mood = 'happy';
CREATE DOMAIN good_mood AS mood CHECK (VALUE <> 'sad');
CREATE TABLE days (day date PRIMARY KEY, overall good_mood);
