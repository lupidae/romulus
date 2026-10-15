-- Enums grown over time, used right away (the whole text is one transaction).
CREATE TYPE ticket_state AS ENUM ('open', 'closed');
ALTER TYPE ticket_state ADD VALUE 'pending' BEFORE 'closed';
ALTER TYPE ticket_state ADD VALUE IF NOT EXISTS 'escalated' AFTER 'open';
ALTER TYPE ticket_state RENAME VALUE 'closed' TO 'resolved';

CREATE TYPE priority AS ENUM ('low', 'normal', 'high');

CREATE TABLE tickets (
    id        integer PRIMARY KEY,
    state     ticket_state NOT NULL DEFAULT 'pending',
    priority  priority NOT NULL DEFAULT 'normal',
    history   ticket_state[] NOT NULL DEFAULT ARRAY['open']::ticket_state[],
    CHECK (state <> 'escalated' OR priority = 'high')
);
CREATE INDEX tickets_open_idx ON tickets (priority) WHERE state IN ('open', 'escalated');
COMMENT ON TYPE ticket_state IS 'Where a ticket is';
