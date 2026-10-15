CREATE TYPE member_role AS ENUM ('admin', 'developer', 'reviewer');

COMMENT ON TYPE member_role IS
    'One role per member per organization; the capability table is in docs/MVP.md "Roles".';

CREATE TABLE organizations (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    name text NOT NULL CHECK (btrim(name) <> '' AND char_length(name) <= 100),
    slug text NOT NULL UNIQUE
        CHECK (slug ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND char_length(slug) <= 63),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE organizations IS
    'romulus owns organizations, memberships and roles; Zitadel is identity only.';

-- Only real changes: memberships_keep_an_admin rewrites the row unchanged.
CREATE TRIGGER organizations_set_updated_at BEFORE UPDATE ON organizations
    FOR EACH ROW WHEN (OLD.* IS DISTINCT FROM NEW.*) EXECUTE FUNCTION set_updated_at();

CREATE TABLE memberships (
    org_id uuid NOT NULL REFERENCES organizations ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    role member_role NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, user_id)
);

-- "Which organizations am I in?" runs on every page load.
CREATE INDEX memberships_user_id_idx ON memberships (user_id);

-- An organization always keeps at least one admin. Enforced here rather than in
-- the API so no code path (or psql session) can strand an organization.
CREATE FUNCTION memberships_keep_an_admin() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.role <> 'admin' OR (TG_OP = 'UPDATE' AND NEW.role = 'admin' AND NEW.org_id = OLD.org_id) THEN
        RETURN NULL;
    END IF;
    -- Serialises membership changes per organization by writing (unchanged)
    -- the organization row; a row lock alone is not enough. Under READ
    -- COMMITTED a concurrent change waits here, then counts admins with a
    -- fresh snapshot. Under REPEATABLE READ or SERIALIZABLE its snapshot is
    -- fixed and would still see the admin the other transaction demoted; the
    -- write makes Postgres abort it instead (40001 serialization_failure, which
    -- the caller retries). Not a key column, so inserts referencing the
    -- organization are not blocked.
    UPDATE organizations SET name = name WHERE id = OLD.org_id;
    IF NOT FOUND THEN
        -- The organization itself is being deleted (ON DELETE CASCADE).
        RETURN NULL;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM memberships WHERE org_id = OLD.org_id AND role = 'admin') THEN
        RAISE EXCEPTION 'organization % would be left without an admin', OLD.org_id
            USING ERRCODE = 'check_violation',
                  CONSTRAINT = 'organization_needs_an_admin',
                  HINT = 'Make another member an admin first.';
    END IF;
    RETURN NULL;
END
$$;

CREATE TRIGGER memberships_keep_an_admin
    AFTER UPDATE OF role, org_id OR DELETE ON memberships
    FOR EACH ROW EXECUTE FUNCTION memberships_keep_an_admin();

COMMENT ON FUNCTION memberships_keep_an_admin() IS
    'Raises check_violation with constraint organization_needs_an_admin; the API answers 409 last_admin.';

CREATE TABLE invitations (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    org_id uuid NOT NULL REFERENCES organizations ON DELETE CASCADE,
    email text NOT NULL CHECK (position('@' IN email) > 1),
    role member_role NOT NULL,
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    expires_at timestamptz NOT NULL,
    accepted_at timestamptz,
    created_by uuid REFERENCES users ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Deleting a user sets its authorship to NULL: without these, a scan per table.
CREATE INDEX invitations_created_by_idx ON invitations (created_by) WHERE created_by IS NOT NULL;

COMMENT ON TABLE invitations IS
    'Copy-link invitations (no email provider in the MVP). Accepting requires a signed-in user whose verified email equals email.';
COMMENT ON COLUMN invitations.token_hash IS 'sha256 of the token in the link; the token itself is shown once.';

-- One pending invitation per address: inviting again refreshes the link
-- (INSERT … ON CONFLICT … DO UPDATE) instead of piling up rows.
CREATE UNIQUE INDEX invitations_pending_email_key ON invitations (org_id, lower(email))
    WHERE accepted_at IS NULL;
