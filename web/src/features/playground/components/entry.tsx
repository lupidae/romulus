import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import * as React from "react";

import { healthQuery } from "@/api/health";
import { databaseQuery } from "@/features/databases/queries";
import { FlaskIcon, NavArrowRightIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { AvatarStack } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { Tooltip } from "@/components/ui/tooltip";
import { timeAgo } from "@/lib/time";
import { cn } from "@/lib/utils";

import {
  formatCount,
  type Playground,
  playgroundFor,
  playgroundsQuery,
  rowsTotal,
  timeLeft,
} from "../queries";
import { CreatePlaygroundDialog, type PlaygroundSource } from "./create-playground-dialog";

/** The Postgres major playgrounds run (playgrounds::MAJOR in the API). */
const PLAYGROUND_MAJOR = 17;

interface PlaygroundButtonProps {
  org: string;
  db: string;
  source: PlaygroundSource;
  /** Developers and admins create playgrounds; everyone opens them. */
  canCreate: boolean;
  /** Icon only below `md` (a crowded bar). */
  compact?: boolean;
  variant?: "secondary" | "ghost";
}

/**
 * "Playground": opens the draft's (or the version's) playground, or, for a
 * developer, creates one. Nothing for a reviewer when there is none yet; on
 * a server without a playground host, a disabled button saying so (to whoever
 * could create one).
 */
export function PlaygroundButton({
  org,
  db,
  source,
  canCreate,
  compact = false,
  variant = "secondary",
}: PlaygroundButtonProps) {
  const playgrounds = useQuery(playgroundsQuery(org, db));
  const health = useQuery(healthQuery);
  const database = useQuery(databaseQuery(org, db));
  const [creating, setCreating] = React.useState(false);
  const existing = playgroundFor(
    playgrounds.data,
    source.kind === "draft" ? { draft: source.draftId } : { version: source.version },
  );
  const label = (
    <>
      {existing?.status === "building" ? (
        <Spinner className="size-4" label="Building" />
      ) : (
        <FlaskIcon />
      )}
      <span className={cn(compact && "max-md:sr-only")}>Playground</span>
    </>
  );
  if (health.data?.playgrounds === "off") {
    // A self-hosted server without them: say they exist and who turns them on.
    if (!canCreate) return null;
    return (
      <Tooltip content="Playgrounds are not set up on this server. Whoever runs it can turn them on (self-host docs, Playgrounds).">
        <span>
          <Button variant={variant} disabled>
            {label}
          </Button>
        </span>
      </Tooltip>
    );
  }
  if (existing) {
    return (
      <Tooltip
        content={
          existing.status === "building"
            ? "Its playground is being built: open it"
            : `Query v${existing.version} with generated rows${source.kind === "draft" ? ", try this draft's migration on them" : ""}`
        }
      >
        <Button variant={variant} asChild>
          <Link
            to="/$org/$db/playgrounds/$playground"
            params={{ org, db, playground: existing.id }}
          >
            {label}
          </Link>
        </Button>
      </Tooltip>
    );
  }
  if (!canCreate || playgrounds.isPending) return null;
  // Playgrounds run Postgres 17 (their image): another major's migration
  // would show 17's locks and rewrites.
  const major = database.data?.pg_major;
  if (major !== undefined && major !== PLAYGROUND_MAJOR)
    return (
      <Tooltip
        content={`Playgrounds run Postgres ${PLAYGROUND_MAJOR} for now, and this database targets Postgres ${major}`}
      >
        <span>
          <Button variant={variant} disabled>
            {label}
          </Button>
        </span>
      </Tooltip>
    );
  return (
    <>
      <Tooltip
        content={`A real database holding v${source.version} with generated rows${source.kind === "draft" ? ", to try this draft's migration on" : ""}`}
      >
        <Button variant={variant} onClick={() => setCreating(true)}>
          {label}
        </Button>
      </Tooltip>
      <CreatePlaygroundDialog
        org={org}
        db={db}
        source={source}
        open={creating}
        onOpenChange={setCreating}
      />
    </>
  );
}

/**
 * The database's playgrounds, in its History tab: what each holds, how it
 * is, who is in it. Nothing while there are none (the draft and the version
 * pages offer to create one).
 */
export function PlaygroundsSection({ org, db }: { org: string; db: string }) {
  const playgrounds = useQuery(playgroundsQuery(org, db));
  const id = React.useId();
  const count = playgrounds.data?.length ?? 0;
  if (playgrounds.isSuccess && count === 0) return null;
  return (
    <section aria-labelledby={id} className="grid gap-3">
      <div className="flex min-h-8 items-center gap-2">
        <h2 id={id} className="text-base font-semibold text-foreground">
          Playgrounds
        </h2>
        {count > 0 && (
          <span className="rounded-sm bg-muted px-1 text-xs font-medium text-muted-foreground tabular">
            {count}
          </span>
        )}
      </div>
      {playgrounds.isPending ? (
        <Card className="p-0" aria-busy aria-label="Loading playgrounds">
          <div className="flex items-center gap-3 px-4 py-3">
            <Skeleton className="size-4 rounded-xs" />
            <div className="grid flex-1 gap-1.5">
              <Skeleton className="h-3.5 w-1/2" />
              <Skeleton className="h-3 w-40" />
            </div>
          </div>
        </Card>
      ) : playgrounds.isError ? (
        <Alert
          variant="destructive"
          title="Could not load the playgrounds"
          action={
            <Button size="sm" onClick={() => void playgrounds.refetch()}>
              Retry
            </Button>
          }
        >
          Check your connection and try again.
        </Alert>
      ) : (
        <Card className="p-0">
          <ul className="divide-y divide-border">
            {playgrounds.data.map((playground) => (
              <li key={playground.id}>
                <PlaygroundRow org={org} db={db} playground={playground} />
              </li>
            ))}
          </ul>
        </Card>
      )}
    </section>
  );
}

function PlaygroundRow({
  org,
  db,
  playground,
}: {
  org: string;
  db: string;
  playground: Playground;
}) {
  const people = playground.present.map((p) => ({ id: p.id, name: p.name }));
  const status =
    playground.status === "building" ? (
      <Badge variant="info">
        <Spinner className="size-3" label="Building" />
        Building
      </Badge>
    ) : playground.status === "failed" ? (
      <Badge variant="destructive">Build failed</Badge>
    ) : playground.migrated_by ? (
      <Badge variant="info">Migrated rows</Badge>
    ) : null;
  return (
    <Link
      to="/$org/$db/playgrounds/$playground"
      params={{ org, db, playground: playground.id }}
      className="group flex items-center gap-3 px-4 py-3 outline-none first:rounded-t-[inherit] last:rounded-b-[inherit] hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
    >
      <FlaskIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
      <div className="grid min-w-0 flex-1 gap-0.5">
        <p className="flex min-w-0 items-center gap-2">
          <span className="truncate text-base font-medium text-foreground">
            {playground.draft ? `“${playground.draft.name}”` : `v${playground.version}`}
          </span>
          {playground.draft && <Badge variant="outline">v{playground.version}</Badge>}
          {status}
        </p>
        <p className="truncate text-sm text-muted-foreground">
          {playground.size === "small" ? "Small" : "Medium"}
          {playground.status === "ready" && ` · ${formatCount(rowsTotal(playground))} rows`}
          {playground.created_by &&
            ` · ${playground.created_by.name}, ${timeAgo(playground.created_at)}`}
          {playground.status === "ready" &&
            ` · deleted in ${timeLeft(playground.expires_at)} unless used`}
        </p>
      </div>
      {people.length > 0 && <AvatarStack people={people} size="sm" max={4} />}
      <NavArrowRightIcon
        aria-hidden
        className="size-4 shrink-0 text-faint-foreground transition-transform group-hover:translate-x-0.5"
      />
    </Link>
  );
}
