import { infiniteQueryOptions, queryOptions } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type Database = components["schemas"]["Database"];
export type VersionSummary = components["schemas"]["VersionSummary"];
export type VersionDetail = components["schemas"]["VersionDetail"];
export type VersionTexts = components["schemas"]["VersionTexts"];
export type VersionOrigin = components["schemas"]["VersionOrigin"];
export type Author = components["schemas"]["Author"];
export type ApiToken = components["schemas"]["ApiToken"];
export type CreatedToken = components["schemas"]["CreatedToken"];
export type TokenScope = components["schemas"]["TokenScope"];
export type CompileReport = components["schemas"]["CompileReport"];
export type Diagnostic = components["schemas"]["Diagnostic"];
export type RemovedVersion = components["schemas"]["RemovedVersion"];
export type AuditEvent = components["schemas"]["AuditEvent"];

export const databaseKeys = {
  list: (org: string) => ["orgs", org, "databases"] as const,
  one: (org: string, db: string) => ["orgs", org, "databases", db] as const,
  versions: (org: string, db: string) => ["orgs", org, "databases", db, "versions"] as const,
  tokens: (org: string, db: string) => ["orgs", org, "databases", db, "tokens"] as const,
  removed: (org: string, db: string) => ["orgs", org, "databases", db, "removed"] as const,
  activity: (org: string, db: string) => ["orgs", org, "databases", db, "activity"] as const,
  // Versions never change: keyed by the database's id first, so a database
  // deleted and created again under the same slug never shows the old one's
  // versions from the cache (the slugs only say where to fetch them).
  version: (org: string, db: string, databaseId: string, number: number) =>
    ["versions", databaseId, number, { org, db }] as const,
  texts: (org: string, db: string, databaseId: string, number: number) =>
    ["versions", databaseId, number, "texts", { org, db }] as const,
};

const path = (org: string, db: string) => ({ params: { path: { org, db } } });

/** The organization's databases, most recently active first. */
export const databasesQuery = (org: string) =>
  queryOptions({
    queryKey: databaseKeys.list(org),
    queryFn: () => unwrap(api.GET("/api/orgs/{org}/databases", { params: { path: { org } } })),
  });

export const databaseQuery = (org: string, db: string) =>
  queryOptions({
    queryKey: databaseKeys.one(org, db),
    queryFn: () => unwrap(api.GET("/api/orgs/{org}/databases/{db}", path(org, db))),
  });

export const VERSIONS_PAGE = 30;

/** Every version, newest first, a page at a time. */
export const versionsQuery = (org: string, db: string) =>
  infiniteQueryOptions({
    queryKey: databaseKeys.versions(org, db),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/versions", {
          params: {
            path: { org, db },
            query: { limit: VERSIONS_PAGE, before: pageParam ?? undefined },
          },
        }),
      ),
    initialPageParam: null as number | null,
    getNextPageParam: (page) => page.next_before ?? undefined,
  });

export const versionQuery = (org: string, db: string, databaseId: string, number: number) =>
  queryOptions({
    queryKey: databaseKeys.version(org, db, databaseId, number),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/versions/{number}", {
          params: { path: { org, db, number } },
        }),
      ),
    staleTime: Infinity,
  });

/** The version as SQL, JSON, DBML and Mermaid, rendered by the API. */
export const versionTextsQuery = (org: string, db: string, databaseId: string, number: number) =>
  queryOptions({
    queryKey: databaseKeys.texts(org, db, databaseId, number),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/versions/{number}/texts", {
          params: { path: { org, db, number } },
        }),
      ),
    staleTime: Infinity,
  });

/** Live API tokens (developers and admins). */
export const tokensQuery = (org: string, db: string) =>
  queryOptions({
    queryKey: databaseKeys.tokens(org, db),
    queryFn: () => unwrap(api.GET("/api/orgs/{org}/databases/{db}/tokens", path(org, db))),
  });

/** Versions an admin removed, restorable for 30 days (developers and admins). */
export const removedVersionsQuery = (org: string, db: string) =>
  queryOptions({
    queryKey: databaseKeys.removed(org, db),
    queryFn: () =>
      unwrap(api.GET("/api/orgs/{org}/databases/{db}/versions/removed", path(org, db))),
  });

export const ACTIVITY_PAGE = 20;

/** The database's audit log, newest first (developers and admins). */
export const activityQuery = (org: string, db: string) =>
  infiniteQueryOptions({
    queryKey: databaseKeys.activity(org, db),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/activity", {
          params: {
            path: { org, db },
            query: { limit: ACTIVITY_PAGE, before: pageParam ?? undefined },
          },
        }),
      ),
    initialPageParam: null as string | null,
    getNextPageParam: (page) => page.next_before ?? undefined,
  });

/**
 * The live versions next to `number` (removed and purged numbers skipped):
 * what "older" and "newer" go to.
 */
export function neighbours(
  database: Pick<Database, "latest" | "missing">,
  number: number,
): { older?: number; newer?: number } {
  const latest = database.latest?.number ?? 0;
  const gone = new Set(database.missing);
  let older: number | undefined;
  for (let n = Math.min(number - 1, latest); n >= 1; n--) {
    if (!gone.has(n)) {
      older = n;
      break;
    }
  }
  let newer: number | undefined;
  for (let n = number + 1; n <= latest; n++) {
    if (!gone.has(n)) {
      newer = n;
      break;
    }
  }
  return { older, newer };
}

/** Whether `number` was a version that is gone now (removed, or purged after). */
export function wasRemoved(
  database: Pick<Database, "latest" | "missing" | "next_number">,
  number: number,
): boolean {
  if (number < 1 || number >= database.next_number) return false;
  return number > (database.latest?.number ?? 0) || database.missing.includes(number);
}

/** Who made a version, in a few words: "Alice Martin", "GitHub Actions". */
export function authorName(author: Author | null | undefined): string {
  if (!author) return "Deleted account";
  return author.name;
}

export const originLabel: Record<VersionOrigin, string> = {
  upload: "Upload",
  push: "Push",
  draft: "Draft",
  import: "Import",
};

/** The diagnostics a 422 `compile_failed` carries in `details`. */
export function diagnosticsOf(details: unknown): Diagnostic[] {
  if (typeof details !== "object" || details === null || !("diagnostics" in details)) return [];
  const list = details.diagnostics;
  return Array.isArray(list) ? (list as Diagnostic[]) : [];
}
