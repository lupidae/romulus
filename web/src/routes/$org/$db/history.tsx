import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";

import { ClockIcon, ImportIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import { DraftsSlot } from "@/features/databases/components/drafts-slot";
import { VersionHistory } from "@/features/databases/components/version-history";
import { databaseQuery, versionsQuery } from "@/features/databases/queries";
import { orgQuery } from "@/features/orgs/queries";
import { canWrite } from "@/features/orgs/roles";

// Open drafts, then every version of the database, newest first.
export const Route = createFileRoute("/$org/$db/history")({
  loader: ({ context: { queryClient }, params: { org, db } }) => {
    void queryClient.prefetchInfiniteQuery(versionsQuery(org, db));
  },
  head: () => ({ meta: [{ title: "History – romulus" }] }),
  component: History,
});

function History() {
  const { org, db } = Route.useParams();
  const { data: database } = useSuspenseQuery(databaseQuery(org, db));
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  const importLink = canWrite(orgData.role) && (
    <Button size="sm" asChild>
      <Link to="/$org/$db/import" params={{ org, db }}>
        <ImportIcon />
        Import migrations
      </Link>
    </Button>
  );
  return (
    <div className="grid w-full max-w-5xl gap-6 px-4 py-6 md:px-8">
      <DraftsSlot org={org} db={db} />
      <section aria-labelledby="versions-title" className="grid gap-3">
        <div className="flex flex-wrap items-end justify-between gap-3">
          <div className="grid gap-0.5">
            <h2 id="versions-title" className="text-base font-semibold text-foreground">
              Versions
            </h2>
            <p className="text-sm text-muted-foreground">
              Each one is an immutable snapshot of the schema. Open one to read it.
            </p>
          </div>
          {importLink}
        </div>
        {database.latest ? (
          <VersionHistory org={org} database={database} />
        ) : (
          <Card>
            <EmptyState
              icon={<ClockIcon />}
              title="No versions yet"
              description="Uploads, pushes from CI, imported migrations and published drafts appear here."
              action={importLink || undefined}
            />
          </Card>
        )}
      </section>
    </div>
  );
}
