import { queryOptions } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type VersionDiff = components["schemas"]["VersionDiff"];

export const diffKeys = {
  // Like versions, keyed by the database's id: two versions never change, so
  // neither does their diff, but a database created again under the same slug
  // is another database.
  pair: (org: string, db: string, databaseId: string, from: number, to: number) =>
    ["versions", databaseId, "diff", from, to, { org, db }] as const,
};

/** `from` compared with `to`: the change list, both models, their SQL and the union's layout. */
export const diffQuery = (org: string, db: string, databaseId: string, from: number, to: number) =>
  queryOptions({
    queryKey: diffKeys.pair(org, db, databaseId, from, to),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/diff", {
          params: { path: { org, db }, query: { from, to } },
        }),
      ),
    staleTime: Infinity,
  });

/** The address of a comparison: `/app/<org>/<db>/diff/4...5`. */
export function rangeParam(from: number, to: number): string {
  return `${from}...${to}`;
}

/** `4...5` → `{ from: 4, to: 5 }`; anything else is `null`. */
export function parseRange(range: string): { from: number; to: number } | null {
  const match = /^([1-9]\d{0,8})\.\.\.([1-9]\d{0,8})$/.exec(range);
  if (!match) return null;
  return { from: Number(match[1]), to: Number(match[2]) };
}
