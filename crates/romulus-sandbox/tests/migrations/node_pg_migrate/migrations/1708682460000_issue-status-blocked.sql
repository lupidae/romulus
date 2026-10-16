-- Up Migration
ALTER TYPE task_status ADD VALUE IF NOT EXISTS 'blocked' AFTER 'doing';
ALTER TYPE task_status RENAME TO issue_status;

-- Down Migration
ALTER TYPE issue_status RENAME TO task_status;
