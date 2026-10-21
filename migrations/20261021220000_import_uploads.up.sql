-- Migration files sent a few at a time, then imported (imports::uploads):
-- what an agent sends over MCP, which cannot read a folder in one request
-- (4 MiB per message). The person who started an upload adds files to it,
-- then imports them with the import's own engine; the row then follows
-- that import (its progress, then its report or its error), which is how
-- a client polls an import that outlasts its request.

CREATE TYPE import_upload_status AS ENUM ('open', 'importing', 'checked', 'failed', 'imported');

CREATE TABLE import_uploads (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    database_id uuid NOT NULL REFERENCES databases (id) ON DELETE CASCADE,
    created_by uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    status import_upload_status NOT NULL DEFAULT 'open',
    dry_run boolean NOT NULL DEFAULT false,
    progress jsonb,
    outcome jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL CHECK (expires_at > created_at)
);

COMMENT ON TABLE import_uploads IS
    'Migration files staged for an import, then that import as it runs. Private to created_by. Files can be added while open (and after a dry run or a failure, to fix one); importing keeps them until the import stores its versions. Swept a day after expires_at.';
COMMENT ON COLUMN import_uploads.status IS
    'open: taking files. importing: an import of them runs. checked: a dry run replayed them (files kept). failed: the last import was refused or stopped (files kept, outcome is the error). imported: done (files deleted, outcome is the report).';
COMMENT ON COLUMN import_uploads.dry_run IS 'The last import asked was a dry run.';
COMMENT ON COLUMN import_uploads.progress IS 'While importing: imports::uploads::ImportProgress (stage, migrations replayed of how many).';
COMMENT ON COLUMN import_uploads.outcome IS 'The last import''s imports::ImportReport (checked, imported) or its error body (failed).';
COMMENT ON COLUMN import_uploads.updated_at IS 'The last file added, or the import''s last progress: an import silent for 30 minutes was lost with its process.';
COMMENT ON COLUMN import_uploads.expires_at IS 'An hour after the last file or import ended (a day once imported): its files are deleted then, the row a day later.';

CREATE INDEX import_uploads_database_id_idx ON import_uploads (database_id);
CREATE INDEX import_uploads_created_by_idx ON import_uploads (created_by);
CREATE INDEX import_uploads_expires_at_idx ON import_uploads (expires_at);

CREATE TABLE import_upload_files (
    upload_id uuid NOT NULL REFERENCES import_uploads (id) ON DELETE CASCADE,
    path text NOT NULL CHECK (char_length(path) BETWEEN 1 AND 1000),
    part integer NOT NULL CHECK (part BETWEEN 1 AND 100),
    content text NOT NULL,
    PRIMARY KEY (upload_id, path, part)
);

COMMENT ON TABLE import_upload_files IS
    'An upload''s files, by their path in the repository. A file too large for one request comes in parts (1, 2…), joined in order when imported; sending a path and part again replaces it, so a retried request changes nothing.';
