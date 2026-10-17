import { queryOptions } from "@tanstack/react-query";

import { ApiError, api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type SchemaMigration = components["schemas"]["SchemaMigration"];
export type MigrationTool = components["schemas"]["MigrationTool"];
export type MigrationFile = components["schemas"]["MigrationFile"];
export type MigrationStatement = components["schemas"]["MigrationStatement"];
export type MigrationWarning = components["schemas"]["MigrationWarning"];
export type MigrationHazard = components["schemas"]["MigrationHazard"];
export type MigrationMark = components["schemas"]["MigrationMark"];

/** How a migration is laid out: the tool, indexes built concurrently or not, and the first file's version. */
export interface MigrationChoice {
  format: MigrationTool;
  concurrently: boolean;
  /** Where the tool numbers its migrations: Flyway's next `V<n>`, golang-migrate's next sequence. */
  version?: string;
}

/** Tools whose versions are often the user's own numbering, not a timestamp. */
export const NUMBERED: Partial<Record<MigrationTool, { pattern: RegExp; hint: string }>> = {
  flyway: { pattern: /^\d+(\.\d+)*$/, hint: "Numbers, optionally dotted: 42 or 1.2" },
  golang_migrate: { pattern: /^\d{1,20}$/, hint: "Digits: 000042, or a timestamp" },
};

/** The tools, in the picker's order, with what their files look like. */
export const TOOLS: readonly { value: MigrationTool; label: string; description: string }[] = [
  { value: "plain", label: "Plain SQL", description: "One .sql file, in a transaction" },
  { value: "sqlx", label: "sqlx", description: "<timestamp>_<name>.sql" },
  { value: "flyway", label: "Flyway", description: "V<version>__<name>.sql" },
  { value: "prisma", label: "Prisma", description: "<timestamp>_<name>/migration.sql" },
  { value: "dbmate", label: "dbmate", description: "migrate:up and migrate:down sections" },
  { value: "golang_migrate", label: "golang-migrate", description: ".up.sql and .down.sql files" },
];

export const isTool = (value: unknown): value is MigrationTool =>
  TOOLS.some((tool) => tool.value === value);

/** Both kinds of migration query share one key type, so one view takes either. */
type MigrationKey = readonly unknown[];

/** A migration query: between two versions, or for a draft. */
export type MigrationQuery = ReturnType<typeof versionMigrationQuery>;

export const migrationKeys = {
  // Keyed by the database's id, like versions and diffs: never by slug
  // alone; and by the Postgres major it is written for (the database's).
  versions: (
    org: string,
    db: string,
    databaseId: string,
    major: number,
    from: number,
    to: number,
    choice: MigrationChoice,
  ) => ["versions", databaseId, "migration", major, from, to, choice, { org, db }] as const,
  // The text reviewed (its SHA-256): another text is another migration.
  draft: (org: string, db: string, id: string, sha256: string, choice: MigrationChoice) =>
    ["drafts", org, db, id, "migration", sha256, choice] as const,
};

/**
 * From version `from` to version `to`, laid out for a tool, for the
 * database's Postgres `major`.
 */
export const versionMigrationQuery = (
  org: string,
  db: string,
  databaseId: string,
  major: number,
  from: number,
  to: number,
  choice: MigrationChoice,
) =>
  queryOptions<SchemaMigration, Error, SchemaMigration, MigrationKey>({
    queryKey: migrationKeys.versions(org, db, databaseId, major, from, to, choice),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/migration", {
          params: { path: { org, db }, query: { from, to, ...choice } },
        }),
      ),
    // Versions never change; the files' timestamp is the first request's.
    staleTime: Infinity,
  });

/** From a draft's base to the text with this SHA-256, laid out for a tool. */
export const draftMigrationQuery = (
  org: string,
  db: string,
  id: string,
  sha256: string,
  choice: MigrationChoice,
  /** The draft moved since it was reviewed (409 `draft_changed`): time to review it again. */
  onChanged?: () => void,
) =>
  queryOptions<SchemaMigration, Error, SchemaMigration, MigrationKey>({
    queryKey: migrationKeys.draft(org, db, id, sha256, choice),
    queryFn: async () => {
      try {
        return await unwrap(
          api.GET("/api/orgs/{org}/databases/{db}/drafts/{id}/migration", {
            params: { path: { org, db, id }, query: { ...choice, sha256 } },
          }),
        );
      } catch (error) {
        if (error instanceof ApiError && error.code === "draft_changed") onChanged?.();
        throw error;
      }
    },
    staleTime: Infinity,
    gcTime: 60_000,
  });

/** The last segment of a path: what a single file downloads as. */
export function basename(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

/** Whether the files download as one .zip: several of them, or one in a folder (Prisma). */
export function isArchive(files: readonly MigrationFile[]): boolean {
  return files.length > 1 || files.some((f) => f.path.includes("/"));
}

/** Character ranges of the 1-based, inclusive line ranges, for highlighting. */
export function lineRanges(
  content: string,
  lines: readonly { from: number; to: number }[],
): { from: number; to: number }[] {
  const starts = [0];
  for (let i = 0; i < content.length; i++) if (content[i] === "\n") starts.push(i + 1);
  return lines.map(({ from, to }) => ({
    from: starts[Math.max(0, from - 1)] ?? content.length,
    to: Math.max(0, (starts[to] ?? content.length + 1) - 1),
  }));
}
