import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, notFound, useParams } from "@tanstack/react-router";

import { ClockIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { VersionView } from "@/features/databases/components/version-view";
import { databaseQuery, versionQuery, versionTextsQuery } from "@/features/databases/queries";

// One version of the database, read-only, at /app/<org>/<db>/v/<number>.
export const Route = createFileRoute("/$org/$db/v/$number")({
  params: {
    parse: ({ number }) => {
      const n = Number(number);
      if (!Number.isSafeInteger(n) || n < 1 || String(n) !== number) throw notFound();
      return { number: n };
    },
    stringify: ({ number }) => ({ number: String(number) }),
  },
  loader: async ({ context: { queryClient }, params: { org, db, number } }) => {
    let database = await queryClient.ensureQueryData(databaseQuery(org, db));
    if (number > (database.latest?.number ?? 0)) {
      // Newer than what this page knew: maybe pushed since. Ask again before
      // saying it does not exist.
      database = await queryClient.fetchQuery({ ...databaseQuery(org, db), staleTime: 0 });
    }
    if (number <= (database.latest?.number ?? 0)) {
      void queryClient.prefetchQuery(versionQuery(org, db, database.id, number));
      void queryClient.prefetchQuery(versionTextsQuery(org, db, database.id, number));
    }
  },
  head: ({ params }) => ({ meta: [{ title: `v${params.number} – romulus` }] }),
  notFoundComponent: NoSuchVersion,
  component: Version,
});

function Version() {
  const { org, db, number } = Route.useParams();
  const { data: database } = useSuspenseQuery(databaseQuery(org, db));
  if (number > (database.latest?.number ?? 0)) return <NoSuchVersion />;
  return <VersionView org={org} database={database} number={number} />;
}

function NoSuchVersion() {
  // Not Route.useParams: this also renders when the number did not parse.
  const { org = "", db = "" } = useParams({ strict: false });
  return (
    <div className="px-4 py-10 md:px-8">
      <EmptyState
        icon={<ClockIcon />}
        title="No such version"
        description="This database has no version with that number (yet)."
        action={
          <Button asChild>
            <Link to="/$org/$db/history" params={{ org, db }}>
              See every version
            </Link>
          </Button>
        }
      />
    </div>
  );
}
