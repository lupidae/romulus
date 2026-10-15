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

export const databaseKeys = {
  list: (org: string) => ["orgs", org, "databases"] as const,
  one: (org: string, db: string) => ["orgs", org, "databases", db] as const,
  versions: (org: string, db: string) => ["orgs", org, "databases", db, "versions"] as const,
  tokens: (org: string, db: string) => ["orgs", org, "databases", db, "tokens"] as const,
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

/** Who made a version, in a few words: "Alice Martin", "GitHub Actions". */
export function authorName(author: Author | null | undefined): string {
  if (!author) return "Deleted account";
  return author.name;
}

export const originLabel: Record<VersionOrigin, string> = {
  upload: "Upload",
  push: "Push",
  draft: "Draft",
};

/** The diagnostics a 422 `compile_failed` carries in `details`. */
export function diagnosticsOf(details: unknown): Diagnostic[] {
  if (typeof details !== "object" || details === null || !("diagnostics" in details)) return [];
  const list = details.diagnostics;
  return Array.isArray(list) ? (list as Diagnostic[]) : [];
}
