ALTER TYPE task_status ADD VALUE IF NOT EXISTS 'blocked' AFTER 'doing';
ALTER TYPE task_status RENAME TO issue_status;
