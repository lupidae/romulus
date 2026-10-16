ALTER INDEX issues_project_id_idx RENAME TO tasks_project_id_idx;
ALTER TABLE issues RENAME TO tasks;
ALTER TABLE users RENAME COLUMN display_name TO full_name;
