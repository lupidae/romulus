-- Up Migration
CREATE TYPE task_status AS ENUM ('todo', 'doing', 'done');

CREATE TABLE tasks (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    assignee_id uuid REFERENCES users (id) ON DELETE SET NULL,
    title text NOT NULL,
    status task_status NOT NULL DEFAULT 'todo',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX tasks_project_id_idx ON tasks (project_id);

-- Down Migration
DROP TABLE tasks;
DROP TYPE task_status;
