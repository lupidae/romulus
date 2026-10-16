-- migrate:up
ALTER TABLE users RENAME COLUMN full_name TO display_name;
ALTER TABLE tasks RENAME TO issues;
ALTER INDEX tasks_project_id_idx RENAME TO issues_project_id_idx;

-- migrate:down
ALTER INDEX issues_project_id_idx RENAME TO tasks_project_id_idx;
ALTER TABLE issues RENAME TO tasks;
ALTER TABLE users RENAME COLUMN display_name TO full_name;
