import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";

import { ShownTest } from "@/features/migration-test/components/test-sheet";
import { testQuery } from "@/features/migration-test/queries";
import { orgQuery } from "@/features/orgs/queries";
import { canWrite } from "@/features/orgs/roles";

// One migration test, on a page of its own: what a link to it opens.
export const Route = createFileRoute("/$org/$db/tests/$id")({
  loader: ({ context: { queryClient }, params: { org, db, id } }) =>
    queryClient.ensureQueryData(testQuery(org, db, id)),
  head: () => ({ meta: [{ title: "Migration test – romulus" }] }),
  component: TestPage,
});

function TestPage() {
  const { org, db, id } = Route.useParams();
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  return (
    <div className="grid w-full max-w-3xl gap-6 px-4 py-6 md:px-8">
      <ShownTest org={org} db={db} id={id} canRun={canWrite(orgData.role)} />
    </div>
  );
}
