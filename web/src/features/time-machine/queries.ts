import { queryOptions } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { Database } from "@/features/databases/queries";

export const timelineKeys = {
  // Keyed by the database's id and what its history is made of now (the
  // latest version, how many): a version pushed, removed or restored is
  // another timeline; the same versions are the same one, for good.
  of: (database: Pick<Database, "id" | "latest" | "versions">, removed: boolean) =>
    [
      "versions",
      database.id,
      "timeline",
      { latest: database.latest?.number ?? 0, count: database.versions, removed },
    ] as const,
};

/** Every version and the history of every object across them. `removed`: admins only. */
export const timelineQuery = (
  org: string,
  database: Pick<Database, "id" | "slug" | "latest" | "versions">,
  removed = false,
) =>
  queryOptions({
    queryKey: [...timelineKeys.of(database, removed), { org }] as const,
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/timeline", {
          params: { path: { org, db: database.slug }, query: { removed } },
        }),
      ),
    staleTime: Infinity,
  });
