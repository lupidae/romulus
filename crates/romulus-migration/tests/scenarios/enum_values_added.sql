-- Labels added at the start, in the middle and at the end, one used right away.
CREATE TYPE ticket_state AS ENUM ('open', 'closed');
CREATE TABLE tickets (id integer PRIMARY KEY, state ticket_state NOT NULL DEFAULT 'open');
-- ==== new ====
CREATE TYPE ticket_state AS ENUM ('draft', 'open', 'pending', 'review', 'closed', 'archived');
CREATE TABLE tickets (id integer PRIMARY KEY, state ticket_state NOT NULL DEFAULT 'pending');
