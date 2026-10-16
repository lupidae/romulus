import type { IgnoreReason, MigrationFormat, SkipReason } from "./api";

/** Every format, as people write them, with the files each one reads. */
export const formats: { value: MigrationFormat; label: string; files: string }[] = [
  { value: "sqlx", label: "sqlx", files: "<version>_<name>.sql, or .up.sql / .down.sql" },
  { value: "flyway", label: "Flyway", files: "V<version>__<name>.sql" },
  { value: "prisma", label: "Prisma", files: "<timestamp>_<name>/migration.sql" },
  { value: "dbmate", label: "dbmate", files: "<version>_<name>.sql with -- migrate:up" },
  { value: "golang_migrate", label: "golang-migrate", files: "<version>_<name>.up.sql" },
  {
    value: "node_pg_migrate",
    label: "node-pg-migrate",
    files: "<timestamp>_<name>.sql with -- Up Migration",
  },
  { value: "plain", label: "Plain SQL", files: "Any .sql files, in name order" },
  { value: "alembic", label: "Alembic", files: "The output of alembic.sh" },
  { value: "django", label: "Django", files: "The output of django.sh" },
  { value: "rails", label: "Rails", files: "The output of rails.sh" },
];

export function formatLabel(format: MigrationFormat): string {
  return formats.find((f) => f.value === format)?.label ?? format;
}

/** Why a statement was not replayed, as a short label. */
export const skipReasonLabel: Record<SkipReason, string> = {
  data: "Data",
  code: "Code",
  transaction: "Transaction",
  settings: "Setting",
  privileges: "Privileges",
  server: "Server",
  maintenance: "Maintenance",
  unsupported: "Not in the schema",
  psql: "psql command",
};

/** What each reason means, for a tooltip. */
export const skipReasonHint: Record<SkipReason, string> = {
  data: "Rows are not part of the schema; the statement is not run.",
  code: "Procedural code and queries are not run.",
  transaction: "The import runs in one transaction of its own.",
  settings: "Session settings do not change the schema.",
  privileges: "Grants and owners are not part of the schema model.",
  server: "Roles, databases and server objects are outside a schema.",
  maintenance: "Maintenance does not change the schema.",
  unsupported: "Operators, casts, aggregates and foreign data are not in the schema model.",
  psql: "A psql command, not SQL.",
};

export const ignoreReasonLabel: Record<IgnoreReason, string> = {
  not_sql: "Not SQL",
  down: "Down migration",
  repeatable: "Repeatable migration",
  baseline: "Baseline migration",
  unrecognized: "Name not recognized",
  tool: "The tool's own file",
};
