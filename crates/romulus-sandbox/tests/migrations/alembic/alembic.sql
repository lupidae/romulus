BEGIN;

CREATE TABLE alembic_version (
    version_num VARCHAR(32) NOT NULL, 
    CONSTRAINT alembic_version_pkc PRIMARY KEY (version_num)
);

-- Running upgrade  -> 1975ea83b712, create organizations and users

CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE EXTENSION IF NOT EXISTS citext;

CREATE TABLE organizations (
    id UUID DEFAULT gen_random_uuid() NOT NULL, 
    name TEXT NOT NULL, 
    slug TEXT NOT NULL, 
    created_at TIMESTAMP WITH TIME ZONE DEFAULT now() NOT NULL, 
    PRIMARY KEY (id), 
    UNIQUE (slug)
);

CREATE TYPE user_role AS ENUM ('owner', 'admin', 'member');

CREATE TABLE users (
    id UUID DEFAULT gen_random_uuid() NOT NULL, 
    organization_id UUID NOT NULL, 
    email CITEXT NOT NULL, 
    full_name TEXT, 
    role user_role DEFAULT 'member' NOT NULL, 
    created_at TIMESTAMP WITH TIME ZONE DEFAULT now() NOT NULL, 
    PRIMARY KEY (id), 
    FOREIGN KEY(organization_id) REFERENCES organizations (id) ON DELETE CASCADE, 
    UNIQUE (email)
);

CREATE INDEX users_organization_id_idx ON users (organization_id);

INSERT INTO alembic_version (version_num) VALUES ('1975ea83b712') RETURNING alembic_version.version_num;

-- Running upgrade 1975ea83b712 -> ae1027a6acf0, create projects

CREATE TABLE projects (
    id BIGINT GENERATED ALWAYS AS IDENTITY, 
    organization_id UUID NOT NULL, 
    name TEXT NOT NULL, 
    archived BOOLEAN DEFAULT false NOT NULL, 
    created_at TIMESTAMP WITH TIME ZONE DEFAULT now() NOT NULL, 
    PRIMARY KEY (id), 
    FOREIGN KEY(organization_id) REFERENCES organizations (id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX projects_org_name_key ON projects (organization_id, lower(name));

UPDATE alembic_version SET version_num='ae1027a6acf0' WHERE alembic_version.version_num = '1975ea83b712';

-- Running upgrade ae1027a6acf0 -> 27c6a30d7c24, create tasks

CREATE TYPE task_status AS ENUM ('todo', 'doing', 'done');

CREATE TABLE tasks (
    id BIGINT GENERATED ALWAYS AS IDENTITY, 
    project_id BIGINT NOT NULL, 
    assignee_id UUID, 
    title TEXT NOT NULL, 
    status task_status DEFAULT 'todo' NOT NULL, 
    created_at TIMESTAMP WITH TIME ZONE DEFAULT now() NOT NULL, 
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT now() NOT NULL, 
    PRIMARY KEY (id), 
    FOREIGN KEY(assignee_id) REFERENCES users (id) ON DELETE SET NULL, 
    FOREIGN KEY(project_id) REFERENCES projects (id) ON DELETE CASCADE
);

CREATE INDEX tasks_project_id_idx ON tasks (project_id);

UPDATE alembic_version SET version_num='27c6a30d7c24' WHERE alembic_version.version_num = 'ae1027a6acf0';

-- Running upgrade 27c6a30d7c24 -> 3b8d4f1e9a20, seed the acme organization

INSERT INTO organizations (name, slug) VALUES ('Acme', 'acme');

INSERT INTO users (organization_id, email, full_name, role) SELECT id, 'admin@acme.test', 'Acme Admin', 'owner' FROM organizations WHERE slug = 'acme';

UPDATE alembic_version SET version_num='3b8d4f1e9a20' WHERE alembic_version.version_num = '27c6a30d7c24';

-- Running upgrade 3b8d4f1e9a20 -> 4c2e8a7b1d35, rename tasks to issues

ALTER TABLE users RENAME full_name TO display_name;

ALTER TABLE tasks RENAME TO issues;

ALTER INDEX tasks_project_id_idx RENAME TO issues_project_id_idx;

UPDATE alembic_version SET version_num='4c2e8a7b1d35' WHERE alembic_version.version_num = '3b8d4f1e9a20';

-- Running upgrade 4c2e8a7b1d35 -> 5d9f0b2c6e41, number issues per project

ALTER TABLE issues ADD COLUMN number INTEGER;

UPDATE issues AS i SET number = numbered.n
        FROM (SELECT id, row_number() OVER (PARTITION BY project_id ORDER BY id) AS n FROM issues) AS numbered
        WHERE numbered.id = i.id;

ALTER TABLE issues ALTER COLUMN number SET NOT NULL;

ALTER TABLE issues ADD CONSTRAINT issues_project_number_key UNIQUE (project_id, number);

UPDATE alembic_version SET version_num='5d9f0b2c6e41' WHERE alembic_version.version_num = '4c2e8a7b1d35';

-- Running upgrade 5d9f0b2c6e41 -> 6e1a3c4d7f52, touch updated_at on update

CREATE OR REPLACE FUNCTION set_updated_at() RETURNS trigger
        LANGUAGE plpgsql AS $$
        BEGIN
            NEW.updated_at := now();
            RETURN NEW;
        END;
        $$;

CREATE TRIGGER issues_set_updated_at BEFORE UPDATE ON issues FOR EACH ROW WHEN (OLD.* IS DISTINCT FROM NEW.*) EXECUTE FUNCTION set_updated_at();

UPDATE alembic_version SET version_num='6e1a3c4d7f52' WHERE alembic_version.version_num = '5d9f0b2c6e41';

-- Running upgrade 6e1a3c4d7f52 -> 7f2b4d5e8a63, issue status blocked

ALTER TYPE task_status ADD VALUE IF NOT EXISTS 'blocked' AFTER 'doing';

ALTER TYPE task_status RENAME TO issue_status;

UPDATE alembic_version SET version_num='7f2b4d5e8a63' WHERE alembic_version.version_num = '6e1a3c4d7f52';

-- Running upgrade 7f2b4d5e8a63 -> 8a3c5e6f9b74, partitioned events

CREATE TABLE events (
            id bigint GENERATED ALWAYS AS IDENTITY,
            organization_id uuid NOT NULL,
            kind text NOT NULL,
            payload jsonb NOT NULL DEFAULT '{}',
            occurred_at timestamptz NOT NULL,
            PRIMARY KEY (id, occurred_at)
        ) PARTITION BY RANGE (occurred_at);

CREATE TABLE events_2024 PARTITION OF events FOR VALUES FROM ('2024-01-01') TO ('2025-01-01');

CREATE TABLE events_2025 PARTITION OF events FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');

CREATE INDEX events_organization_idx ON events (organization_id, occurred_at DESC);

UPDATE alembic_version SET version_num='8a3c5e6f9b74' WHERE alembic_version.version_num = '7f2b4d5e8a63';

-- Running upgrade 8a3c5e6f9b74 -> 9b4d6f7a0c85, concurrent issue indexes

COMMIT;

CREATE INDEX CONCURRENTLY IF NOT EXISTS issues_open_idx ON issues (project_id) WHERE status <> 'done';

CREATE INDEX CONCURRENTLY issues_assignee_idx ON issues (assignee_id);

BEGIN;

UPDATE alembic_version SET version_num='9b4d6f7a0c85' WHERE alembic_version.version_num = '8a3c5e6f9b74';

-- Running upgrade 9b4d6f7a0c85 -> a0c5e7f8b196, open issues view

CREATE VIEW open_issues AS SELECT i.id, i.project_id, i.number, i.title, i.status FROM issues AS i WHERE i.status <> 'done';

COMMENT ON TABLE issues IS 'Work items, numbered per project.';

COMMENT ON COLUMN issues.number IS 'Per project, from 1.';

ALTER TABLE issues ENABLE ROW LEVEL SECURITY;

CREATE POLICY issues_by_organization ON issues USING (project_id IN (SELECT id FROM projects WHERE organization_id = current_setting('app.organization_id')::uuid));

GRANT SELECT ON open_issues TO app_reader;

UPDATE alembic_version SET version_num='a0c5e7f8b196' WHERE alembic_version.version_num = '9b4d6f7a0c85';

-- Running upgrade a0c5e7f8b196 -> b1d6f8a9c2a7, projects archived at

ALTER TABLE projects ADD COLUMN archived_at TIMESTAMP WITH TIME ZONE;

UPDATE projects SET archived_at = now() WHERE archived;

ALTER TABLE projects DROP COLUMN archived;

DROP INDEX IF EXISTS projects_org_name_key;

CREATE UNIQUE INDEX projects_org_name_key ON projects (organization_id, lower(name)) WHERE archived_at IS NULL;

ALTER TABLE users ALTER COLUMN display_name TYPE VARCHAR(200);

UPDATE alembic_version SET version_num='b1d6f8a9c2a7' WHERE alembic_version.version_num = 'a0c5e7f8b196';

COMMIT;

