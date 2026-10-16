-- An import's models while it replays: each is written as it is read, then
-- moved into versions by the transaction that stores the import, and the
-- rows deleted. The API holds one model at a time, never a whole history.
-- Unlogged: an import in flight does not survive a crash anyway.
CREATE UNLOGGED TABLE import_staging (
    import_id uuid NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    database_id uuid NOT NULL REFERENCES databases (id) ON DELETE CASCADE,
    model jsonb NOT NULL,
    layout jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (import_id, ordinal)
);
CREATE INDEX import_staging_database_id_idx ON import_staging (database_id);
CREATE INDEX import_staging_created_at_idx ON import_staging (created_at);

COMMENT ON TABLE import_staging IS
    'Models of imports in flight (imports::run), one row per new migration in order (ordinal); moved into versions when the import is stored, deleted when it ends. Rows older than an hour are leftovers of a stopped process.';
