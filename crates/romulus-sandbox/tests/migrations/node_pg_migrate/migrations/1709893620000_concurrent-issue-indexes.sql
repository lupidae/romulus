-- Up Migration
CREATE INDEX CONCURRENTLY IF NOT EXISTS issues_open_idx
    ON issues (project_id) WHERE status <> 'done';
CREATE INDEX CONCURRENTLY issues_assignee_idx ON issues (assignee_id);

-- Down Migration
DROP INDEX CONCURRENTLY issues_assignee_idx;
DROP INDEX CONCURRENTLY issues_open_idx;
