CREATE TYPE api_token_scope AS ENUM ('push', 'full');
CREATE TYPE version_source_kind AS ENUM ('sql', 'remus_json');
CREATE TYPE version_origin AS ENUM ('upload', 'push', 'draft');

CREATE TABLE databases (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    org_id uuid NOT NULL REFERENCES organizations ON DELETE CASCADE,
    name text NOT NULL CHECK (btrim(name) <> '' AND char_length(name) <= 100),
    slug text NOT NULL CHECK (slug ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND char_length(slug) <= 63),
    description text NOT NULL DEFAULT '',
    created_by uuid REFERENCES users ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz
);

COMMENT ON TABLE databases IS 'A schema kept in romulus. Soft-deleted: deleted_at hides it, its versions stay.';

-- docs/MVP.md says UNIQUE (org_id, slug). Deliberately partial: a deleted
-- database frees its slug, so "prod" can be created again after deleting it.
CREATE UNIQUE INDEX databases_org_id_slug_key ON databases (org_id, slug) WHERE deleted_at IS NULL;

CREATE INDEX databases_created_by_idx ON databases (created_by) WHERE created_by IS NOT NULL;

CREATE TRIGGER databases_set_updated_at BEFORE UPDATE ON databases
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE api_tokens (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    database_id uuid NOT NULL REFERENCES databases ON DELETE CASCADE,
    name text NOT NULL CHECK (btrim(name) <> '' AND char_length(name) <= 100),
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    scope api_token_scope NOT NULL,
    created_by uuid REFERENCES users ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    revoked_at timestamptz
);

COMMENT ON TABLE api_tokens IS 'Per-database bearer tokens for remus and CI. Revoked, never deleted, so versions keep their author.';
COMMENT ON COLUMN api_tokens.token_hash IS 'sha256 of the token; the token itself is shown once.';

CREATE INDEX api_tokens_database_id_idx ON api_tokens (database_id);
CREATE INDEX api_tokens_created_by_idx ON api_tokens (created_by) WHERE created_by IS NOT NULL;

CREATE TABLE versions (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    database_id uuid NOT NULL REFERENCES databases ON DELETE CASCADE,
    number integer NOT NULL CHECK (number > 0),
    model jsonb NOT NULL,
    source text NOT NULL,
    source_kind version_source_kind NOT NULL,
    origin version_origin NOT NULL,
    layout jsonb NOT NULL DEFAULT '{}',
    message text,
    created_by uuid REFERENCES users ON DELETE SET NULL,
    created_by_token uuid REFERENCES api_tokens ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- Numbers are assigned under a lock on the databases row; this is the backstop.
    CONSTRAINT versions_database_id_number_key UNIQUE (database_id, number),
    -- Lets drafts reference (database_id, version id) so a draft cannot point
    -- at another database's version.
    CONSTRAINT versions_database_id_id_key UNIQUE (database_id, id),
    CONSTRAINT versions_remus_json_only_by_push CHECK (source_kind = 'sql' OR origin = 'push')
);

COMMENT ON TABLE versions IS 'Immutable schema snapshots, numbered 1, 2, 3… per database.';
COMMENT ON COLUMN versions.model IS 'The remus JSON (remus_core::Schema). Authoritative.';
COMMENT ON COLUMN versions.source IS 'What was uploaded or published, verbatim (SQL, or remus JSON for pushes).';
COMMENT ON COLUMN versions.layout IS 'Table positions and groups as the draft held them; shape owned by the collab module.';
COMMENT ON COLUMN versions.created_by IS 'NULL for pushes, which come from a token (created_by_token), not a user.';

CREATE FUNCTION versions_are_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    -- The one update allowed: ON DELETE SET NULL on the author columns, so
    -- deleting a user or token does not have to delete history.
    IF (to_jsonb(NEW) - 'created_by' - 'created_by_token')
           IS DISTINCT FROM (to_jsonb(OLD) - 'created_by' - 'created_by_token')
       OR (NEW.created_by IS NOT NULL AND NEW.created_by IS DISTINCT FROM OLD.created_by)
       OR (NEW.created_by_token IS NOT NULL AND NEW.created_by_token IS DISTINCT FROM OLD.created_by_token) THEN
        RAISE EXCEPTION 'version % is immutable', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'versions_are_immutable';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER versions_are_immutable BEFORE UPDATE ON versions
    FOR EACH ROW EXECUTE FUNCTION versions_are_immutable();

CREATE FUNCTION versions_are_permanent() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    -- History only goes with its database (ON DELETE CASCADE, by then the
    -- databases row is gone). Databases are soft-deleted in the product.
    IF EXISTS (SELECT 1 FROM databases WHERE id = OLD.database_id) THEN
        RAISE EXCEPTION 'version % cannot be deleted', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'versions_are_permanent';
    END IF;
    RETURN OLD;
END
$$;

CREATE TRIGGER versions_are_permanent BEFORE DELETE ON versions
    FOR EACH ROW EXECUTE FUNCTION versions_are_permanent();

CREATE INDEX versions_created_by_idx ON versions (created_by) WHERE created_by IS NOT NULL;
CREATE INDEX versions_created_by_token_idx ON versions (created_by_token)
    WHERE created_by_token IS NOT NULL;
