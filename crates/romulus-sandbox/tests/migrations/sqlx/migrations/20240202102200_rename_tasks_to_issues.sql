ALTER TABLE users RENAME COLUMN full_name TO display_name;
ALTER TABLE tasks RENAME TO issues;
ALTER INDEX tasks_project_id_idx RENAME TO issues_project_id_idx;
