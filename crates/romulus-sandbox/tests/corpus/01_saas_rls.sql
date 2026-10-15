-- A multi-tenant SaaS: organizations, members, projects, all behind row
-- level security. Policies are written for roles the sandbox does not have.

CREATE SCHEMA app;
COMMENT ON SCHEMA app IS 'Everything the product stores';

CREATE TYPE app.member_role AS ENUM ('owner', 'admin', 'member', 'viewer');

CREATE TABLE app.organizations (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    slug        text NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9-]{3,40}$'),
    name        text NOT NULL,
    plan        text NOT NULL DEFAULT 'free',
    created_at  timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE app.organizations IS 'A customer account';

CREATE TABLE app.users (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    email       text NOT NULL,
    full_name   text,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX users_email_lower_idx ON app.users (lower(email));

CREATE TABLE app.memberships (
    org_id   uuid NOT NULL REFERENCES app.organizations (id) ON DELETE CASCADE,
    user_id  uuid NOT NULL REFERENCES app.users (id) ON DELETE CASCADE,
    role     app.member_role NOT NULL DEFAULT 'member',
    PRIMARY KEY (org_id, user_id)
);
CREATE INDEX memberships_user_idx ON app.memberships (user_id);

CREATE TABLE app.projects (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    org_id      uuid NOT NULL REFERENCES app.organizations (id) ON DELETE CASCADE,
    name        text NOT NULL,
    archived_at timestamptz,
    settings    jsonb NOT NULL DEFAULT '{}'::jsonb,
    UNIQUE (org_id, name)
);
CREATE INDEX projects_active_idx ON app.projects (org_id) WHERE archived_at IS NULL;

ALTER TABLE app.organizations ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.memberships ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.projects ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.projects FORCE ROW LEVEL SECURITY;

CREATE POLICY org_members ON app.organizations FOR SELECT TO app_user
    USING (id IN (SELECT m.org_id FROM app.memberships m
                  WHERE m.user_id = current_setting('app.user_id')::uuid));
CREATE POLICY org_admins ON app.organizations FOR UPDATE TO app_user, app_admin
    USING (app.is_org_admin(id)) WITH CHECK (app.is_org_admin(id));
CREATE POLICY own_memberships ON app.memberships FOR SELECT TO app_user
    USING (user_id = current_setting('app.user_id')::uuid);
CREATE POLICY project_members ON app.projects TO app_user
    USING (org_id = app.current_org_id());
CREATE POLICY project_no_archived ON app.projects AS RESTRICTIVE FOR SELECT TO app_user
    USING (archived_at IS NULL);
CREATE POLICY service_all ON app.projects TO service_role USING (true) WITH CHECK (true);
