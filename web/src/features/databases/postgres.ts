import { queryOptions } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

import type { Database } from "./queries";

/** A Postgres major a database targets: what its SQL compiles on (docs/MVP.md, "Several Postgres majors"). */
export type PgMajor = components["schemas"]["PgMajor"];
/** A major as this server runs it: whether its compiler is up. */
export type PostgresMajor = components["schemas"]["PostgresMajor"];
export type MajorCheck = components["schemas"]["MajorCheck"];
export type VersionPostgres = components["schemas"]["VersionPostgres"];

/** Every major romulus compiles on, newest first (how pickers list them). */
export const MAJORS: readonly PgMajor[] = [18, 17, 16, 15];
/** What a new database targets. */
export const DEFAULT_MAJOR: PgMajor = 17;

export function isMajor(n: unknown): n is PgMajor {
  return typeof n === "number" && (MAJORS as readonly number[]).includes(n);
}

/** "Postgres 17": how the app names a major everywhere. */
export function majorLabel(major: number): string {
  return `Postgres ${major}`;
}

export const postgresKeys = {
  majors: ["postgres-majors"] as const,
};

/** The majors this server compiles on, and whether each one's compiler is up. */
export const postgresMajorsQuery = () =>
  queryOptions({
    queryKey: postgresKeys.majors,
    queryFn: () => unwrap(api.GET("/api/postgres-majors")),
    staleTime: 30_000,
  });

/**
 * The major a version was compiled on, as a sentence: "Compiled on
 * Postgres 17.11", or for a push "Pushed from Postgres 16.4".
 */
export function compiledOn(postgres: VersionPostgres, pushed: boolean): string {
  const version = postgres.server_version.split(" ")[0] || String(postgres.major ?? "?");
  return pushed ? `Pushed from Postgres ${version}` : `Compiled on Postgres ${version}`;
}

/** "17.11" from "17.11 (Debian 17.11-1.pgdg13+1)". */
export function shortVersion(serverVersion: string | null | undefined): string | undefined {
  return serverVersion?.split(" ")[0] || undefined;
}

/**
 * What the picker says under a major: whether this server compiles it, and
 * what to know about it (the default; 18's syntax the parser lacks).
 */
export function majorDescription(major: PgMajor, served?: PostgresMajor): string {
  if (served?.status === "off") return "Not available on this server";
  if (served?.status === "unavailable") return "Its compiler is down right now";
  return [
    major === DEFAULT_MAJOR ? "Default" : undefined,
    shortVersion(served?.server_version),
    major === 18 ? "syntax new in 18 is not read yet" : undefined,
  ]
    .filter(Boolean)
    .join(" · ");
}

/** The latest version came from a server on another major than the database targets now. */
export function pushedFromAnotherMajor(
  database: Database,
): { number: number; postgres: VersionPostgres; major: PgMajor } | null {
  const latest = database.latest;
  if (!latest || latest.origin !== "push" || !latest.postgres.other_major) return null;
  const major = latest.postgres.major;
  if (!isMajor(major) || major === database.pg_major) return null;
  return { number: latest.number, postgres: latest.postgres, major };
}
