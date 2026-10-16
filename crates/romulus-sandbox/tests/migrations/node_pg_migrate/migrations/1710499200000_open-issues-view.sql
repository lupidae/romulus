-- Up Migration
CREATE VIEW open_issues AS
SELECT i.id, i.project_id, i.number, i.title, i.status
FROM issues AS i
WHERE i.status <> 'done';

COMMENT ON TABLE issues IS 'Work items, numbered per project.';
COMMENT ON COLUMN issues.number IS 'Per project, from 1.';

ALTER TABLE issues ENABLE ROW LEVEL SECURITY;
CREATE POLICY issues_by_organization ON issues
    USING (project_id IN (
        SELECT id FROM projects
        WHERE organization_id = current_setting('app.organization_id')::uuid
    ));

GRANT SELECT ON open_issues TO app_reader;

-- Down Migration
DROP POLICY issues_by_organization ON issues;
DROP VIEW open_issues;
