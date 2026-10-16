ALTER TABLE projects DROP COLUMN archived_at;
ALTER TABLE projects ADD COLUMN archived boolean NOT NULL DEFAULT false;
