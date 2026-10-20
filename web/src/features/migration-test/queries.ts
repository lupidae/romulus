import { queryOptions } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type MigrationTest = components["schemas"]["MigrationTest"];
export type MigrationTestStatus = components["schemas"]["MigrationTestStatus"];
export type TestProgress = components["schemas"]["TestProgress"];
export type TestReport = components["schemas"]["MigrationTestReport"];
export type PhaseReport = components["schemas"]["MigrationTestPhaseReport"];
export type Phase = components["schemas"]["MigrationTestPhase"];
export type CheckStatus = components["schemas"]["MigrationCheckStatus"];
export type CheckReport = components["schemas"]["MigrationTestCheck"];
export type TableCheck = components["schemas"]["MigrationTestTable"];
export type TestNote = components["schemas"]["MigrationTestNote"];
export type DataLoss = components["schemas"]["MigrationTestDataLoss"];
export type CreateMigrationTest = components["schemas"]["CreateMigrationTest"];

/** What a test is of: a draft as reviewed, two versions, a version's own migration. */
export type TestSource =
  | { kind: "draft"; draft: string; sha256: string }
  | { kind: "versions"; from: number; to: number }
  | { kind: "version"; version: number };

/** The key of a source's history (a draft's across its texts). */
function sourceKey(source: TestSource): string {
  switch (source.kind) {
    case "draft":
      return `draft:${source.draft}`;
    case "versions":
      return `versions:${source.from}:${source.to}`;
    case "version":
      return `version:${source.version}`;
  }
}

export const testKeys = {
  all: (org: string, db: string) => ["migration-tests", org, db] as const,
  of: (org: string, db: string, source: TestSource) =>
    ["migration-tests", org, db, "of", sourceKey(source)] as const,
  one: (org: string, db: string, id: string) => ["migration-tests", org, db, "one", id] as const,
};

/** Still waiting or running. */
export function isLive(test: Pick<MigrationTest, "status">): boolean {
  return test.status === "queued" || test.status === "running";
}

function listParams(source: TestSource) {
  switch (source.kind) {
    case "draft":
      return { draft: source.draft };
    case "versions":
      return { from: source.from, to: source.to };
    case "version":
      return { version: source.version };
  }
}

/** A source's tests, newest first (summaries). Polled while one runs. */
export const testsQuery = (org: string, db: string, source: TestSource) =>
  queryOptions({
    queryKey: testKeys.of(org, db, source),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/migration-tests", {
          params: { path: { org, db }, query: listParams(source) },
        }),
      ),
    refetchInterval: (query) => (query.state.data?.some(isLive) ? 2_000 : false),
  });

/** One test, its report once it ran: polled every second while it waits or runs. */
export const testQuery = (org: string, db: string, id: string) =>
  queryOptions({
    queryKey: testKeys.one(org, db, id),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/migration-tests/{id}", {
          params: { path: { org, db, id } },
        }),
      ),
    refetchInterval: (query) => (query.state.data && isLive(query.state.data) ? 1_000 : false),
  });

/**
 * How to run one: the down migration, the rows (a playground's, or fresh
 * ones), and the migration as the view shows it (renames, concurrently).
 */
export type StartOptions = Partial<
  Pick<
    CreateMigrationTest,
    "down" | "size" | "profile" | "seed" | "playground" | "renames" | "concurrently"
  >
>;

/** The body that starts a test of `source`. */
export function createBody(source: TestSource, options: StartOptions = {}): CreateMigrationTest {
  const base = { profile: "realistic" as const, ...options };
  switch (source.kind) {
    case "draft":
      return { ...base, draft_id: source.draft, sha256: source.sha256 };
    case "versions":
      return { ...base, from: source.from, to: source.to };
    case "version":
      return { ...base, version: source.version };
  }
}

/** The newest test of the draft's text as it is now (or still running), or of the source. */
export function latestFor(
  tests: MigrationTest[] | undefined,
  source: TestSource,
): MigrationTest | undefined {
  return tests?.find((t) => source.kind !== "draft" || t.current === true || isLive(t));
}
