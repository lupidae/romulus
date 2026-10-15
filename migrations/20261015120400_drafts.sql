CREATE TYPE draft_status AS ENUM ('open', 'published', 'deleted');

CREATE TABLE drafts (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    database_id uuid NOT NULL REFERENCES databases ON DELETE CASCADE,
    name text NOT NULL CHECK (btrim(name) <> '' AND char_length(name) <= 200),
    base_version_id uuid,
    status draft_status NOT NULL DEFAULT 'open',
    published_version_id uuid,
    created_by uuid REFERENCES users ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT drafts_base_version_fkey FOREIGN KEY (database_id, base_version_id)
        REFERENCES versions (database_id, id),
    CONSTRAINT drafts_published_version_fkey FOREIGN KEY (database_id, published_version_id)
        REFERENCES versions (database_id, id),
    CONSTRAINT drafts_published_has_version CHECK ((status = 'published') = (published_version_id IS NOT NULL))
);

COMMENT ON TABLE drafts IS
    'Branches off the version line. The content lives in a Y.Doc: draft_updates + draft_snapshots.';
COMMENT ON COLUMN drafts.base_version_id IS
    'The version the draft started from (moves on "Update draft"). NULL: the database had no version yet.';

-- The database page lists open drafts.
CREATE INDEX drafts_open_database_id_idx ON drafts (database_id) WHERE status = 'open';
CREATE INDEX drafts_created_by_idx ON drafts (created_by) WHERE created_by IS NOT NULL;

CREATE TRIGGER drafts_set_updated_at BEFORE UPDATE ON drafts
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE draft_compiles (
    draft_id uuid PRIMARY KEY REFERENCES drafts ON DELETE CASCADE,
    compiled_at timestamptz NOT NULL DEFAULT now(),
    source_hash bytea NOT NULL CHECK (octet_length(source_hash) = 32),
    model jsonb,
    errors jsonb,
    CONSTRAINT draft_compiles_model_xor_errors CHECK ((model IS NULL) <> (errors IS NULL))
);

COMMENT ON TABLE draft_compiles IS
    'The last compile result per draft, so a reconnecting client gets the graph at once.';
COMMENT ON COLUMN draft_compiles.source_hash IS
    'sha256 of the SQL that was compiled: an unchanged text is not compiled again.';

CREATE TABLE draft_updates (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    draft_id uuid NOT NULL REFERENCES drafts ON DELETE CASCADE,
    update bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE draft_updates IS
    'Append-only Yjs updates (y-sync v1 encoding). Folded into draft_snapshots by compaction.';

-- Loading a doc reads the updates after its snapshot, in order.
CREATE INDEX draft_updates_draft_id_id_idx ON draft_updates (draft_id, id);

CREATE TABLE draft_snapshots (
    draft_id uuid PRIMARY KEY REFERENCES drafts ON DELETE CASCADE,
    state bytea NOT NULL,
    upto_update_id bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE draft_snapshots IS 'Compacted Y.Doc state; one per draft, replaced on each compaction.';
COMMENT ON COLUMN draft_snapshots.upto_update_id IS
    'Every draft_updates row with id <= this is folded into state and may be deleted.';
