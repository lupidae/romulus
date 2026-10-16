-- 20240105093000_create_organizations_and_users.rb
CREATE EXTENSION IF NOT EXISTS "pgcrypto";
CREATE EXTENSION IF NOT EXISTS "citext";
DO $$
BEGIN
    IF NOT EXISTS (
      SELECT 1
      FROM pg_type t
      JOIN pg_namespace n ON t.typnamespace = n.oid
      WHERE t.typname = 'user_role'
        AND n.nspname = ANY (current_schemas(false))
    ) THEN
        CREATE TYPE "user_role" AS ENUM ('owner', 'admin', 'member');
    END IF;
END
$$;
CREATE TABLE "organizations" ("id" uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY, "name" text NOT NULL, "slug" text NOT NULL, "created_at" timestamp(6) NOT NULL, "updated_at" timestamp(6) NOT NULL);
CREATE UNIQUE INDEX "index_organizations_on_slug" ON "organizations" ("slug");
CREATE TABLE "users" ("id" uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY, "organization_id" uuid NOT NULL, "email" citext NOT NULL, "full_name" text, "role" user_role DEFAULT 'member' NOT NULL, "created_at" timestamp(6) NOT NULL, "updated_at" timestamp(6) NOT NULL, CONSTRAINT "fk_rails_d7b9ff90af"
FOREIGN KEY ("organization_id")
  REFERENCES "organizations" ("id")
 ON DELETE CASCADE);
CREATE INDEX "index_users_on_organization_id" ON "users" ("organization_id");
CREATE UNIQUE INDEX "index_users_on_email" ON "users" ("email");
-- 20240112094300_create_projects.rb
CREATE TABLE "projects" ("id" bigserial primary key, "organization_id" uuid NOT NULL, "name" text NOT NULL, "archived" boolean DEFAULT FALSE NOT NULL, "created_at" timestamp(6) NOT NULL, "updated_at" timestamp(6) NOT NULL, CONSTRAINT "fk_rails_9aee26923d"
FOREIGN KEY ("organization_id")
  REFERENCES "organizations" ("id")
 ON DELETE CASCADE);
CREATE INDEX "index_projects_on_organization_id" ON "projects" ("organization_id");
CREATE UNIQUE INDEX "projects_org_name_key" ON "projects" (organization_id, lower(name));
-- 20240119095600_create_tasks.rb
DO $$
BEGIN
    IF NOT EXISTS (
      SELECT 1
      FROM pg_type t
      JOIN pg_namespace n ON t.typnamespace = n.oid
      WHERE t.typname = 'task_status'
        AND n.nspname = ANY (current_schemas(false))
    ) THEN
        CREATE TYPE "task_status" AS ENUM ('todo', 'doing', 'done');
    END IF;
END
$$;
CREATE TABLE "tasks" ("id" bigserial primary key, "project_id" bigint NOT NULL, "assignee_id" uuid, "title" text NOT NULL, "status" task_status DEFAULT 'todo' NOT NULL, "created_at" timestamp(6) NOT NULL, "updated_at" timestamp(6) NOT NULL, CONSTRAINT "fk_rails_02e851e3b7"
FOREIGN KEY ("project_id")
  REFERENCES "projects" ("id")
 ON DELETE CASCADE, CONSTRAINT "fk_rails_0016c50613"
FOREIGN KEY ("assignee_id")
  REFERENCES "users" ("id")
 ON DELETE SET NULL);
CREATE INDEX "index_tasks_on_project_id" ON "tasks" ("project_id");
CREATE INDEX "index_tasks_on_assignee_id" ON "tasks" ("assignee_id");
-- 20240126100900_seed_acme.rb
INSERT INTO "organizations" ("name", "slug", "created_at", "updated_at") VALUES ($1, $2, $3, $4) RETURNING "id";
INSERT INTO users (organization_id, email, full_name, role, created_at, updated_at)
VALUES ('e632ee85-878d-482d-a0f2-dd1b836e177c', 'admin@acme.test', 'Acme Admin', 'owner', now(), now());
-- 20240202102200_rename_tasks_to_issues.rb
ALTER TABLE "users" RENAME COLUMN "full_name" TO "display_name";
ALTER TABLE "tasks" RENAME TO "issues";
ALTER INDEX "tasks_pkey" RENAME TO "issues_pkey";
ALTER TABLE "public"."tasks_id_seq" RENAME TO "issues_id_seq";
ALTER INDEX "index_tasks_on_assignee_id" RENAME TO "index_issues_on_assignee_id";
ALTER INDEX "index_tasks_on_project_id" RENAME TO "index_issues_on_project_id";
-- 20240209103500_add_number_to_issues.rb
ALTER TABLE "issues" ADD "number" integer;
UPDATE issues AS i
SET number = numbered.n
FROM (SELECT id, row_number() OVER (PARTITION BY project_id ORDER BY id) AS n FROM issues) AS numbered
WHERE numbered.id = i.id;
ALTER TABLE "issues" ALTER COLUMN "number" SET NOT NULL;
CREATE UNIQUE INDEX "index_issues_on_project_id_and_number" ON "issues" ("project_id", "number");
-- 20240216104800_add_updated_at_trigger.rb
CREATE OR REPLACE FUNCTION set_updated_at() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  NEW.updated_at := now();
  RETURN NEW;
END;
$$;

CREATE TRIGGER issues_set_updated_at
  BEFORE UPDATE ON issues
  FOR EACH ROW EXECUTE FUNCTION set_updated_at();
-- 20240223110100_add_blocked_to_issue_status.rb
ALTER TYPE "task_status" ADD VALUE 'blocked' AFTER 'doing';
ALTER TYPE "task_status" RENAME TO "issue_status";
-- 20240301111400_create_events.rb
CREATE TABLE events (
  id bigint GENERATED ALWAYS AS IDENTITY,
  organization_id uuid NOT NULL,
  kind text NOT NULL,
  payload jsonb NOT NULL DEFAULT '{}',
  occurred_at timestamptz NOT NULL,
  PRIMARY KEY (id, occurred_at)
) PARTITION BY RANGE (occurred_at);
CREATE TABLE events_2024 PARTITION OF events FOR VALUES FROM ('2024-01-01') TO ('2025-01-01');
CREATE TABLE events_2025 PARTITION OF events FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');
CREATE INDEX "index_events_on_organization_id_and_occurred_at" ON "events" ("organization_id", "occurred_at" DESC);
-- 20240308112700_add_concurrent_issue_indexes.rb
CREATE INDEX CONCURRENTLY "issues_open_idx" ON "issues" ("project_id") WHERE status <> 'done';
-- 20240315114000_create_open_issues_view.rb
CREATE VIEW open_issues AS
SELECT i.id, i.project_id, i.number, i.title, i.status FROM issues AS i WHERE i.status <> 'done';
COMMENT ON TABLE "issues" IS 'Work items, numbered per project.';
COMMENT ON COLUMN "issues"."number" IS 'Per project, from 1.';
-- 20240322115300_replace_archived_with_archived_at.rb
ALTER TABLE "projects" ADD "archived_at" timestamptz;
UPDATE projects SET archived_at = now() WHERE archived;
ALTER TABLE "projects" DROP COLUMN "archived";
DROP INDEX  "projects_org_name_key";
CREATE UNIQUE INDEX "projects_org_name_key" ON "projects" (organization_id, lower(name)) WHERE archived_at IS NULL;
ALTER TABLE "users" ALTER COLUMN "display_name" TYPE character varying(200);
