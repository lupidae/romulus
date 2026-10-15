import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, redirect } from "@tanstack/react-router";

import { GitCompareIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { databaseQuery } from "@/features/databases/queries";

// /app/<org>/<db>/diff: the latest change, the newest version against the one before.
export const Route = createFileRoute("/$org/$db/diff/")({
  loader: async ({ context: { queryClient }, params: { org, db } }) => {
    const database = await queryClient.ensureQueryData(databaseQuery(org, db));
    const latest = database.latest?.number ?? 0;
    if (latest >= 2) {
      throw redirect({
        to: "/$org/$db/diff/$range",
        params: { org, db, range: { from: latest - 1, to: latest } },
        replace: true,
      });
    }
  },
  head: () => ({ meta: [{ title: "Compare – romulus" }] }),
  component: NothingToCompare,
});

function NothingToCompare() {
  const { org, db } = Route.useParams();
  const { data: database } = useSuspenseQuery(databaseQuery(org, db));
  return (
    <div className="px-4 py-10 md:px-8">
      <EmptyState
        icon={<GitCompareIcon />}
        title="Nothing to compare yet"
        description={
          database.latest
            ? "A comparison needs two versions. The next upload, push or published draft makes v2."
            : "A comparison needs two versions, and this database has none yet."
        }
        action={
          <Button asChild>
            <Link to={database.latest ? "/$org/$db" : "/$org/$db/history"} params={{ org, db }}>
              {database.latest ? "Open v1" : "See the history"}
            </Link>
          </Button>
        }
      />
    </div>
  );
}
