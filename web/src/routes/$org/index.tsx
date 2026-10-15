import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { DatabaseIcon, PlusIcon } from "@/components/icons";
import { PageHeader } from "@/components/shell/page-header";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import { Kbd } from "@/components/ui/kbd";
import { Tooltip } from "@/components/ui/tooltip";
import { CreateDatabaseDialog } from "@/features/databases/components/create-database-dialog";
import { DatabaseList, DatabaseListSkeleton } from "@/features/databases/components/database-list";
import { databasesQuery } from "@/features/databases/queries";
import { orgQuery } from "@/features/orgs/queries";
import { canWrite } from "@/features/orgs/roles";
import { useShortcut } from "@/lib/use-shortcut";

// The organization's home: its databases.
export const Route = createFileRoute("/$org/")({
  loader: ({ context: { queryClient }, params }) => {
    // Started here, awaited by nobody: the header shows at once, the list
    // shows its skeleton until it lands.
    void queryClient.prefetchQuery(databasesQuery(params.org));
  },
  head: () => ({ meta: [{ title: "Databases – romulus" }] }),
  component: OrgHome,
});

function OrgHome() {
  const { org: slug } = Route.useParams();
  const { data: org } = useSuspenseQuery(orgQuery(slug));
  const databases = useQuery(databasesQuery(slug));
  const navigate = useNavigate();
  const writer = canWrite(org.role);
  const [creating, setCreating] = React.useState(false);
  useShortcut("c", () => setCreating(true), writer);

  const createButton = (
    <Tooltip content="New database" shortcut={["C"]}>
      <Button variant="primary" onClick={() => setCreating(true)}>
        <PlusIcon />
        New database
      </Button>
    </Tooltip>
  );

  return (
    <>
      <PageHeader
        title="Databases"
        description={`The schemas ${org.name} keeps in romulus, each with its versions.`}
        actions={writer && databases.data?.length ? createButton : undefined}
      />
      <Card>
        {databases.isPending ? (
          <DatabaseListSkeleton />
        ) : databases.isError ? (
          <div className="p-4">
            <Alert
              variant="destructive"
              title="Could not load the databases"
              action={
                <Button
                  size="sm"
                  loading={databases.isFetching}
                  onClick={() => void databases.refetch()}
                >
                  Retry
                </Button>
              }
            >
              {databases.error.message}
            </Alert>
          </div>
        ) : databases.data.length === 0 ? (
          <EmptyState
            icon={<DatabaseIcon />}
            title="No databases yet"
            description={
              writer
                ? "A database holds one schema and its history. Create one, then upload its SQL or push it from CI with remus."
                : `A database holds one schema and its history. Developers and admins of ${org.name} can add one.`
            }
            action={
              writer && (
                <Button variant="primary" onClick={() => setCreating(true)}>
                  <PlusIcon />
                  New database
                  <Kbd className="ml-1 border-primary-foreground/25 bg-primary-foreground/10 text-primary-foreground shadow-none">
                    C
                  </Kbd>
                </Button>
              )
            }
          />
        ) : (
          <DatabaseList org={slug} databases={databases.data} />
        )}
      </Card>
      {writer && (
        <CreateDatabaseDialog
          org={slug}
          open={creating}
          onOpenChange={setCreating}
          onCreated={(database) => {
            setCreating(false);
            void navigate({ to: "/$org/$db", params: { org: slug, db: database.slug } });
          }}
        />
      )}
    </>
  );
}
