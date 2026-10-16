import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";

import { LockIcon } from "@/components/icons";
import { EmptyState } from "@/components/ui/empty-state";
import { databaseQuery } from "@/features/databases/queries";
import { ImportFlow } from "@/features/imports/components/import-flow";
import { orgQuery } from "@/features/orgs/queries";
import { canWrite } from "@/features/orgs/roles";

// Migration files in, one version per migration out. Developers and admins.
export const Route = createFileRoute("/$org/$db/import")({
  head: () => ({ meta: [{ title: "Import migrations – romulus" }] }),
  component: Import,
});

function Import() {
  const { org, db } = Route.useParams();
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  const { data: database } = useSuspenseQuery(databaseQuery(org, db));
  if (!canWrite(orgData.role)) {
    return (
      <div className="px-4 py-10 md:px-8">
        <EmptyState
          icon={<LockIcon />}
          title="Imports are for developers"
          description="Reviewers read every version. Ask an admin to make you a developer to import migrations."
        />
      </div>
    );
  }
  return <ImportFlow org={org} db={db} databaseId={database.id} />;
}
