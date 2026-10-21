import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";

import { ClockIcon, ClockRotateRightIcon, ImportIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import { DraftsSlot } from "@/features/databases/components/drafts-slot";
import { ExportLink } from "@/features/databases/components/export-card";
import { OtherMajorAlert } from "@/features/databases/components/postgres";
import { VersionHistory } from "@/features/databases/components/version-history";
import { databaseExportUrl } from "@/features/databases/export";
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
  const writer = canWrite(orgData.role);
  const importLink = writer && (
    <Button size="sm" asChild>
      <Link to="/$org/$db/import" params={{ org, db }}>
        <ImportIcon />
        Import migrations
      </Link>
    </Button>
  );
  return (
    <div className="grid w-full max-w-5xl gap-6 px-4 py-6 md:px-8">
      <OtherMajorAlert org={org} database={database} writer={writer} />
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
          <div className="flex items-center gap-2">
            {database.versions > 1 && (
              <Button size="sm" asChild>
                <Link to="/$org/$db/time-machine" params={{ org, db }}>
                  <ClockRotateRightIcon />
                  Time machine
                </Link>
              </Button>
            )}
            {/* Every member may take everything with them (the settings are developers'). */}
            {database.latest && (
              <ExportLink size="sm" href={databaseExportUrl(org, db)} label="Export (ZIP)" />
            )}
            {importLink}
          </div>
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
