import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import * as React from "react";

import { GitCompareIcon, NavArrowRightIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

import { type Database, databaseKeys, versionsQuery } from "../queries";
import { Byline, OriginBadge } from "./version-meta";

/**
 * Every version, newest first: a row each, the latest marked, older pages on
 * demand. Each row compares with the one before it; ticking two compares them.
 */
export function VersionHistory({ org, database }: { org: string; database: Database }) {
  const versions = useInfiniteQuery(versionsQuery(org, database.slug));
  // At most two, in the order picked: a third replaces the first.
  const [picked, setPicked] = React.useState<number[]>([]);
  const toggle = (n: number, on: boolean) =>
    setPicked((current) =>
      on ? [...current.filter((p) => p !== n), n].slice(-2) : current.filter((p) => p !== n),
    );
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
  const pair = picked.length === 2 ? ([Math.min(...picked), Math.max(...picked)] as const) : null;
  return (
    <Card>
      {latest > 1 && (
        <div className="flex min-h-12 items-center gap-3 border-b border-border px-4 py-2">
          <p className="min-w-0 flex-1 text-sm text-muted-foreground" aria-live="polite">
            {picked.length === 0
              ? "Tick two versions to compare them, or compare one with the version before it."
              : picked.length === 1
                ? `v${picked[0]} ticked: tick another one to compare with it.`
                : `v${pair![0]} and v${pair![1]} ticked.`}
          </p>
          {picked.length > 0 && (
            <Button variant="ghost" size="sm" onClick={() => setPicked([])}>
              Clear
            </Button>
          )}
          {pair ? (
            <Button variant="primary" size="sm" asChild>
              <Link
                to="/$org/$db/diff/$range"
                params={{ org, db, range: { from: pair[0], to: pair[1] } }}
              >
                <GitCompareIcon />
                Compare v{pair[0]} → v{pair[1]}
              </Link>
            </Button>
          ) : (
            <Button variant="primary" size="sm" disabled>
              <GitCompareIcon />
              Compare
            </Button>
          )}
        </div>
      )}
      <ol aria-label="Versions" className="divide-y divide-border">
        {rows.map((v) => (
          <li
            key={v.id}
            className={cn(
              "group relative flex items-center gap-3 px-4 py-3 hover:bg-accent/60",
              picked.includes(v.number) && "bg-accent/40",
            )}
          >
            {latest > 1 && (
              <Checkbox
                aria-label={`Tick v${v.number} to compare`}
                checked={picked.includes(v.number)}
                onCheckedChange={(on) => toggle(v.number, on === true)}
                className="z-1"
              />
            )}
            {/* The row is the link (its ::after covers it); the box and the
                compare button sit above it. */}
            <Link
              to={v.number === latest ? "/$org/$db" : "/$org/$db/v/$number"}
              params={{ org, db, number: v.number }}
              className={cn(
                "flex min-w-0 flex-1 items-center gap-3 outline-none",
                "after:absolute after:inset-0 after:content-['']",
                "focus-visible:after:ring-2 focus-visible:after:ring-ring focus-visible:after:ring-inset",
              )}
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
            </Link>
            {v.number > 1 ? (
              <Tooltip content={`Compare v${v.number - 1} → v${v.number}`}>
                <Button variant="ghost" size="sm" asChild className="z-1 max-sm:px-2">
                  <Link
                    to="/$org/$db/diff/$range"
                    params={{ org, db, range: { from: v.number - 1, to: v.number } }}
                    aria-label={`Compare v${v.number - 1} → v${v.number}`}
                  >
                    <GitCompareIcon />
                    <span className="max-sm:hidden">Compare</span>
                  </Link>
                </Button>
              </Tooltip>
            ) : (
              // Keeps the column: v1 has nothing before it.
              <span aria-hidden className="w-7 shrink-0 sm:w-23" />
            )}
            <NavArrowRightIcon
              aria-hidden
              className="size-4 shrink-0 text-faint-foreground transition-transform group-hover:translate-x-0.5"
            />
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
