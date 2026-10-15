import { useInfiniteQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import { NavArrowRightIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";

import { type Database, versionsQuery } from "../queries";
import { Byline, OriginBadge } from "./version-meta";

/** Every version, newest first: a row each, the latest marked, older pages on demand. */
export function VersionHistory({ org, database }: { org: string; database: Database }) {
  const versions = useInfiniteQuery(versionsQuery(org, database.slug));
  const latest = database.latest?.number;
  const db = database.slug;

  if (versions.isPending) return <HistorySkeleton rows={Math.min(latest ?? 3, 6)} />;
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
              className="group grid grid-cols-[3rem_minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 px-4 py-3 outline-none hover:bg-accent/60 focus-visible:bg-accent/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset sm:grid-cols-[3rem_5.5rem_minmax(0,1fr)_auto_auto]"
            >
              <span className="font-mono text-base font-semibold text-foreground tabular-nums">
                v{v.number}
              </span>
              <span className="hidden sm:block">
                <OriginBadge origin={v.origin} />
              </span>
              <span className="flex min-w-0 items-center gap-2">
                <span
                  className={
                    v.message
                      ? "truncate text-base text-foreground"
                      : "truncate text-base text-muted-foreground"
                  }
                >
                  {v.message ?? "No message"}
                </span>
                {v.number === latest && <Badge variant="success">Latest</Badge>}
              </span>
              <Byline
                author={v.author}
                at={v.created_at}
                className="col-span-2 col-start-2 text-sm sm:col-span-1 sm:col-start-auto"
              />
              <NavArrowRightIcon
                aria-hidden
                className="row-start-1 size-4 text-faint-foreground transition-transform group-hover:translate-x-0.5 max-sm:col-start-3"
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
