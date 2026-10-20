import { queryOptions } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type Playground = components["schemas"]["Playground"];
export type PlaygroundStatus = components["schemas"]["PlaygroundStatus"];
export type PlaygroundSize = components["schemas"]["PlaygroundSize"];
export type PlaygroundProfile = components["schemas"]["PlaygroundProfile"];
export type PlaygroundStage = components["schemas"]["PlaygroundStage"];
export type BuildProgress = components["schemas"]["BuildProgress"];
export type TableRows = components["schemas"]["PlaygroundTableRows"];
export type PlaygroundRun = components["schemas"]["PlaygroundRun"];
export type MigrationEvent = components["schemas"]["PlaygroundMigrationEvent"];
export type MigrationReport = components["schemas"]["MigrationRunReport"];
export type MigrationStatementResult = components["schemas"]["MigrationRunStatement"];

/** The Postgres major playgrounds run (playgrounds::MAJOR in the API). */
export const PLAYGROUND_MAJOR = 17;

export const playgroundKeys = {
  list: (org: string, db: string) => ["playgrounds", org, db] as const,
  one: (org: string, db: string, id: string) => ["playgrounds", org, db, id] as const,
  runs: (org: string, db: string, id: string) => ["playgrounds", org, db, id, "runs"] as const,
};

/** The database's playgrounds. Polled while one builds. */
export const playgroundsQuery = (org: string, db: string) =>
  queryOptions({
    queryKey: playgroundKeys.list(org, db),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/playgrounds", { params: { path: { org, db } } }),
      ),
    refetchInterval: (query) =>
      query.state.data?.some((p) => p.status === "building") ? 2_000 : 30_000,
  });

/** One playground: polled every second while it builds, every two while
 *  its host is busy (a reset, a migration: the buttons wait for it), then
 *  now and then (who is in it, whether it runs). */
export const playgroundQuery = (org: string, db: string, id: string) =>
  queryOptions({
    queryKey: playgroundKeys.one(org, db, id),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/playgrounds/{id}", {
          params: { path: { org, db, id } },
        }),
      ),
    refetchInterval: (query) => {
      const data = query.state.data;
      if (data?.status === "building") return 1_000;
      return data?.live?.busy ? 2_000 : 30_000;
    },
  });

/** The shared history, newest first (the first page; live entries arrive on the console). */
export const runsQuery = (org: string, db: string, id: string) =>
  queryOptions({
    queryKey: playgroundKeys.runs(org, db, id),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/playgrounds/{id}/runs", {
          params: { path: { org, db, id }, query: { limit: 50 } },
        }),
      ),
  });

/** The live playground made for a draft, or for a version outside drafts. */
export function playgroundFor(
  playgrounds: Playground[] | undefined,
  source: { draft: string } | { version: number },
): Playground | undefined {
  return playgrounds?.find((p) => {
    if (p.status !== "building" && p.status !== "ready") return false;
    return "draft" in source
      ? p.draft?.id === source.draft
      : !p.draft && p.version === source.version;
  });
}

/** "Small · 42 300 rows", the size and the rows it holds. */
export function rowsTotal(playground: Playground): number {
  return playground.tables.reduce((sum, t) => sum + t.rows, 0);
}

const numberFormat = new Intl.NumberFormat("en-US");

/** 24 000 → "24,000". */
export function formatCount(n: number): string {
  return numberFormat.format(n);
}

/** Days and hours until `iso`, in words: "6 days", "5 hours", "less than an hour". */
export function timeLeft(iso: string, now = Date.now()): string {
  const ms = new Date(iso).getTime() - now;
  if (ms <= 0) return "now";
  const hours = Math.floor(ms / 3_600_000);
  if (hours >= 36) return `${Math.round(hours / 24)} days`;
  if (hours >= 1) return `${hours} hour${hours === 1 ? "" : "s"}`;
  return "less than an hour";
}
