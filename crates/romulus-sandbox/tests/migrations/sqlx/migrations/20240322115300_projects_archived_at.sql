ALTER TABLE projects ADD COLUMN archived_at timestamptz;
UPDATE projects SET archived_at = now() WHERE archived;
ALTER TABLE projects DROP COLUMN archived;

DROP INDEX IF EXISTS projects_org_name_key;
CREATE UNIQUE INDEX projects_org_name_key
    ON projects (organization_id, lower(name))
    WHERE archived_at IS NULL;

ALTER TABLE users ALTER COLUMN display_name TYPE varchar(200);
