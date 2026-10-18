-- Playgrounds (docs/playground.md): a real Postgres holding one version's
-- schema, filled with generated rows, on the playground host. What is
-- durable lives here: which version, how it was generated, its state, its
-- shared history. The rows themselves live on the host (and can always be
-- generated again from model, size, profile, seed and generator version).

CREATE TYPE playground_status AS ENUM ('building', 'ready', 'failed', 'expired');
CREATE TYPE playground_size AS ENUM ('small', 'medium');
CREATE TYPE playground_profile AS ENUM ('realistic', 'edge_cases');
CREATE TYPE playground_run_kind AS ENUM ('query', 'explain', 'migration', 'reset');
CREATE TYPE playground_run_status AS ENUM ('ok', 'error', 'cancelled');

CREATE TABLE playgrounds (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    org_id uuid NOT NULL REFERENCES organizations ON DELETE CASCADE,
    database_id uuid NOT NULL REFERENCES databases ON DELETE CASCADE,
    draft_id uuid REFERENCES drafts ON DELETE CASCADE,
    version_id uuid NOT NULL REFERENCES versions ON DELETE CASCADE,
    model_sha256 bytea NOT NULL CHECK (octet_length(model_sha256) = 32),
    size playground_size NOT NULL,
    profile playground_profile NOT NULL,
    seed bigint NOT NULL CHECK (seed >= 0),
    locale text NOT NULL DEFAULT 'en' CHECK (locale ~ '^[a-z]{2}$'),
    generator_version text CHECK (generator_version IS NULL OR char_length(generator_version) <= 100),
    status playground_status NOT NULL DEFAULT 'building',
    host text NOT NULL DEFAULT 'local' CHECK (char_length(host) <= 100),
    row_counts jsonb NOT NULL DEFAULT '[]',
    report jsonb,
    failure text CHECK (failure IS NULL OR char_length(failure) <= 2000),
    created_by uuid REFERENCES users ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    ready_at timestamptz,
    last_used_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL DEFAULT now() + interval '7 days',
    CONSTRAINT playgrounds_ready_when_ready CHECK ((status = 'ready') <= (ready_at IS NOT NULL))
);

COMMENT ON TABLE playgrounds IS
    'A real database with generated rows on the playground host (docs/playground.md), from a version or a draft''s base. One live (building or ready) per draft, and per version outside drafts.';
COMMENT ON COLUMN playgrounds.draft_id IS 'The draft it was made for (it holds the draft''s base version); NULL for a version''s own.';
COMMENT ON COLUMN playgrounds.version_id IS 'The version whose schema it holds.';
COMMENT ON COLUMN playgrounds.model_sha256 IS 'SHA-256 of that version''s model JSON: with size, profile, seed, locale and generator version, what reproduces its rows.';
COMMENT ON COLUMN playgrounds.host IS 'The playground host holding it (one in the MVP).';
COMMENT ON COLUMN playgrounds.row_counts IS 'Rows per table once generated: [{"schema", "table", "rows"}].';
COMMENT ON COLUMN playgrounds.report IS 'The generator''s report (what it could not satisfy), shown as it is.';
COMMENT ON COLUMN playgrounds.expires_at IS 'Deleted then unless used again: 7 days after the last use.';

CREATE UNIQUE INDEX playgrounds_one_per_draft ON playgrounds (draft_id)
    WHERE draft_id IS NOT NULL AND status IN ('building', 'ready');
CREATE UNIQUE INDEX playgrounds_one_per_version ON playgrounds (version_id)
    WHERE draft_id IS NULL AND status IN ('building', 'ready');
CREATE INDEX playgrounds_database_id_idx ON playgrounds (database_id, created_at DESC);
CREATE INDEX playgrounds_org_id_idx ON playgrounds (org_id) WHERE status IN ('building', 'ready');
CREATE INDEX playgrounds_version_id_idx ON playgrounds (version_id);
CREATE INDEX playgrounds_created_by_idx ON playgrounds (created_by) WHERE created_by IS NOT NULL;

CREATE TABLE playground_runs (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    playground_id uuid NOT NULL REFERENCES playgrounds ON DELETE CASCADE,
    user_id uuid REFERENCES users ON DELETE SET NULL,
    -- Their name then, as the audit log keeps it.
    user_name text NOT NULL CHECK (char_length(user_name) <= 200),
    kind playground_run_kind NOT NULL,
    sql text CHECK (sql IS NULL OR octet_length(sql) <= 65536),
    migration_input_sha256 text CHECK (migration_input_sha256 IS NULL OR migration_input_sha256 ~ '^[0-9a-f]{64}$'),
    started_at timestamptz NOT NULL DEFAULT now(),
    duration_ms bigint NOT NULL DEFAULT 0 CHECK (duration_ms >= 0),
    status playground_run_status NOT NULL,
    rows bigint CHECK (rows IS NULL OR rows >= 0),
    error jsonb,
    report jsonb
);

COMMENT ON TABLE playground_runs IS
    'A playground''s shared history: who ran what, when, how long, rows or error. Never results. Gone with the playground.';
COMMENT ON COLUMN playground_runs.sql IS 'What was run (the first 64 KiB); NULL for a reset.';
COMMENT ON COLUMN playground_runs.migration_input_sha256 IS 'For a migration run: the draft text it ran (compile::input_hash), as approvals name it.';
COMMENT ON COLUMN playground_runs.report IS 'For a migration run: the per-statement report.';

CREATE INDEX playground_runs_playground_id_idx ON playground_runs (playground_id, id DESC);
CREATE INDEX playground_runs_user_id_idx ON playground_runs (user_id) WHERE user_id IS NOT NULL;

ALTER TABLE playgrounds ADD COLUMN migrated_by_run uuid REFERENCES playground_runs ON DELETE SET NULL;
COMMENT ON COLUMN playgrounds.migrated_by_run IS
    'The migration run whose rows it holds; NULL while it holds the generated rows (after a build or a reset).';
CREATE INDEX playgrounds_migrated_by_run_idx ON playgrounds (migrated_by_run) WHERE migrated_by_run IS NOT NULL;
