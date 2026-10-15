import { createFileRoute } from "@tanstack/react-router";

import { DatabaseIcon, PlusIcon } from "@/components/icons";
import { PageHeader } from "@/components/shell/page-header";
import { AvatarStack } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";

import { databases } from "../-fixtures";

type State = "loading" | "empty";

export const Route = createFileRoute("/dev/shell/")({
  // ?state=loading|empty shows the page's other states.
  validateSearch: (search: Record<string, unknown>): { state?: State } => ({
    state: search.state === "loading" || search.state === "empty" ? search.state : undefined,
  }),
  head: () => ({ meta: [{ title: "Databases – romulus" }] }),
  component: Databases,
});

function Databases() {
  const { state } = Route.useSearch();
  const add = (
    <Button variant="primary" onClick={() => toast.success("Database created")}>
      <PlusIcon />
      New database
    </Button>
  );
  return (
    <>
      <PageHeader
        title="Databases"
        description="Schemas this organization keeps in romulus."
        actions={state === "empty" ? undefined : add}
      />
      {state === "empty" ? (
        <Card>
          <EmptyState
            icon={<DatabaseIcon />}
            title="No databases yet"
            description="Add a database, then upload its schema or push it from CI with remus."
            action={add}
          />
        </Card>
      ) : (
        <Card>
          <ul className="divide-y divide-border" aria-busy={state === "loading"}>
            {state === "loading"
              ? [0, 1, 2].map((i) => (
                  <li key={i} className="flex h-16 items-center gap-4 px-4">
                    <Skeleton className="size-8 rounded-md" />
                    <div className="grid flex-1 gap-1.5">
                      <Skeleton className="h-4 w-28" />
                      <Skeleton className="h-3.5 w-64 max-w-full" />
                    </div>
                  </li>
                ))
              : databases.map((db) => (
                  <li key={db.name} className="flex h-16 items-center gap-4 px-4">
                    <span className="flex size-8 shrink-0 items-center justify-center rounded-md border border-border bg-muted text-muted-foreground">
                      <DatabaseIcon className="size-4" />
                    </span>
                    <div className="grid min-w-0 flex-1">
                      <span className="truncate font-mono text-sm font-medium text-foreground">
                        {db.name}
                      </span>
                      <span className="truncate text-sm text-muted-foreground">
                        {db.description}
                      </span>
                    </div>
                    {db.editing.length > 0 && (
                      <AvatarStack people={[...db.editing]} className="hidden sm:flex" />
                    )}
                    <Badge variant="neutral" className="tabular">
                      v{db.version}
                    </Badge>
                    <span className="hidden w-24 text-right text-sm text-faint-foreground sm:block">
                      {db.updated}
                    </span>
                  </li>
                ))}
          </ul>
        </Card>
      )}
    </>
  );
}
