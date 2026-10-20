-- A table becomes partitioned, so it is dropped and created again: the
-- foreign keys to it, the views over it and the policies of other tables
-- that read it are dropped first and come back after.
CREATE TABLE events (
    id integer NOT NULL,
    at date NOT NULL,
    PRIMARY KEY (id, at)
);
CREATE TABLE notes (
    id integer PRIMARY KEY,
    event_id integer,
    event_at date,
    FOREIGN KEY (event_id, event_at) REFERENCES events (id, at)
);
CREATE VIEW recent_events AS SELECT id, at FROM events WHERE at > '2026-01-01';
ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
CREATE POLICY with_event ON notes USING (EXISTS (SELECT 1 FROM events e WHERE e.id = event_id));
-- ==== new ====
CREATE TABLE events (
    id integer NOT NULL,
    at date NOT NULL,
    PRIMARY KEY (id, at)
) PARTITION BY RANGE (at);
CREATE TABLE events_2026 PARTITION OF events FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
CREATE TABLE notes (
    id integer PRIMARY KEY,
    event_id integer,
    event_at date,
    FOREIGN KEY (event_id, event_at) REFERENCES events (id, at)
);
CREATE VIEW recent_events AS SELECT id, at FROM events WHERE at > '2026-01-01';
ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
CREATE POLICY with_event ON notes USING (EXISTS (SELECT 1 FROM events e WHERE e.id = event_id));
