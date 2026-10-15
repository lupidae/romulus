import { Link } from "@tanstack/react-router";

import { DatabaseIcon, NavArrowRightIcon } from "@/components/icons";
import { Skeleton } from "@/components/ui/skeleton";

import type { Database } from "../queries";
import { Byline } from "./version-meta";

/**
 * The organization's databases, one row each: name and description on the
 * left edge, the latest version on the right. The whole row is the link.
 */
export function DatabaseList({ org, databases }: { org: string; databases: Database[] }) {
  return (
    <ul aria-label="Databases" className="divide-y divide-border">
      {databases.map((database) => (
        <li key={database.id}>
          <Link
            to="/$org/$db"
            params={{ org, db: database.slug }}
            className="group flex items-center gap-3 px-4 py-3 outline-none hover:bg-accent/60 focus-visible:bg-accent/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
          >
            <span className="flex size-9 shrink-0 items-center justify-center rounded-md border border-border bg-muted text-muted-foreground">
              <DatabaseIcon className="size-4" />
            </span>
            <span className="grid min-w-0 flex-1 gap-0.5 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center sm:gap-x-6">
              <span className="grid min-w-0 gap-0.5">
                <span className="flex min-w-0 items-baseline gap-2">
                  <span className="truncate text-base font-medium text-foreground">
                    {database.name}
                  </span>
                  <span className="hidden truncate font-mono text-xs text-faint-foreground sm:inline">
                    {database.slug}
                  </span>
                </span>
                {database.description && (
                  <span className="truncate text-sm text-muted-foreground">
                    {database.description}
                  </span>
                )}
              </span>
              <span className="flex min-w-0 items-center gap-2 text-sm">
                {database.latest ? (
                  <>
                    <span className="shrink-0 font-mono text-sm font-medium text-foreground tabular-nums">
                      v{database.latest.number}
                    </span>
                    <Byline author={database.latest.author} at={database.latest.created_at} />
                  </>
                ) : (
                  <span className="text-muted-foreground">No versions yet</span>
                )}
              </span>
            </span>
            <NavArrowRightIcon
              aria-hidden
              className="size-4 shrink-0 text-faint-foreground transition-transform group-hover:translate-x-0.5"
            />
          </Link>
        </li>
      ))}
    </ul>
  );
}

/** The list's shape while it loads. */
export function DatabaseListSkeleton({ rows = 3 }: { rows?: number }) {
  return (
    <div aria-hidden className="divide-y divide-border">
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className="flex items-center gap-3 px-4 py-3">
          <Skeleton className="size-9 rounded-md" />
          <div className="grid flex-1 gap-1.5">
            <Skeleton className="h-4 w-40" />
            <Skeleton className="h-3 w-64 max-w-full" />
          </div>
          <Skeleton className="hidden h-4 w-44 sm:block" />
        </div>
      ))}
    </div>
  );
}
