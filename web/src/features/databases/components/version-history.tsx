import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import * as React from "react";

import { NavArrowRightIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

import { type Database, databaseKeys, versionsQuery } from "../queries";
import { Byline, OriginBadge } from "./version-meta";

/** Every version, newest first: a row each, the latest marked, older pages on demand. */
export function VersionHistory({ org, database }: { org: string; database: Database }) {
  const versions = useInfiniteQuery(versionsQuery(org, database.slug));
  const queryClient = useQueryClient();
  const db = database.slug;
  // The list may know of a newer version (pushed by CI) than the page's
  // database: trust it, and bring the rest of the page up to date.
  const newest = versions.data?.pages[0]?.versions[0]?.number;
  const latest = Math.max(database.latest?.number ?? 0, newest ?? 0);
  React.useEffect(() => {
    if (newest !== undefined && newest > (database.latest?.number ?? 0)) {
      void queryClient.invalidateQueries({ queryKey: databaseKeys.one(org, db), exact: true });
    }
  }, [newest, database.latest?.number, queryClient, org, db]);

  if (versions.isPending) return <HistorySkeleton rows={Math.min(latest || 3, 6)} />;
  if (versions.isError) {
    return (
      <Alert
        variant="destructive"
        title="Could not load the versions"
        action={
          <Button size="sm" loading={versions.isFetching} onClick={() => void versions.refetch()}>
            Retry
          </Button>
        }
      >
        {versions.error.message}
      </Alert>
    );
  }
  const rows = versions.data.pages.flatMap((page) => page.versions);
  return (
    <Card>
      <ol aria-label="Versions" className="divide-y divide-border">
        {rows.map((v) => (
          <li key={v.id}>
            <Link
              to={v.number === latest ? "/$org/$db" : "/$org/$db/v/$number"}
              params={{ org, db, number: v.number }}
              className="group flex items-center gap-3 px-4 py-3 outline-none hover:bg-accent/60 focus-visible:bg-accent/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
            >
              <span className="w-10 shrink-0 font-mono text-base font-semibold text-foreground tabular-nums">
                v{v.number}
              </span>
              <span className="hidden w-20 shrink-0 sm:block">
                <OriginBadge origin={v.origin} />
              </span>
              <span className="grid min-w-0 flex-1 gap-0.5">
                <span className="flex min-w-0 items-center gap-2">
                  <span
                    className={cn(
                      "truncate text-base",
                      v.message ? "text-foreground" : "text-muted-foreground",
                    )}
                  >
                    {v.message ?? "No message"}
                  </span>
                  {v.number === latest && <Badge variant="success">Latest</Badge>}
                </span>
                <Byline author={v.author} at={v.created_at} className="text-sm sm:hidden" />
              </span>
              <Byline
                author={v.author}
                at={v.created_at}
                className="hidden shrink-0 text-sm sm:inline-flex"
              />
              <NavArrowRightIcon
                aria-hidden
                className="size-4 shrink-0 text-faint-foreground transition-transform group-hover:translate-x-0.5"
              />
            </Link>
          </li>
        ))}
      </ol>
      {versions.hasNextPage && (
        <div className="border-t border-border px-4 py-3">
          <Button
            size="sm"
            loading={versions.isFetchingNextPage}
            onClick={() => void versions.fetchNextPage()}
          >
            Show older versions
          </Button>
        </div>
      )}
    </Card>
  );
}

function HistorySkeleton({ rows }: { rows: number }) {
  return (
    <Card aria-hidden>
      <div className="divide-y divide-border">
        {Array.from({ length: rows }, (_, i) => (
          <div key={i} className="flex items-center gap-3 px-4 py-3.5">
            <Skeleton className="h-4 w-8" />
            <Skeleton className="hidden h-5 w-16 sm:block" />
            <Skeleton className="h-4 flex-1" />
            <Skeleton className="h-4 w-36" />
          </div>
        ))}
      </div>
    </Card>
  );
}
