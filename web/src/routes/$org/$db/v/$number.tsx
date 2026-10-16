import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, notFound, useParams } from "@tanstack/react-router";

import { ClockIcon, EyeClosedIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { VersionView } from "@/features/databases/components/version-view";
import {
  databaseQuery,
  versionQuery,
  versionTextsQuery,
  wasRemoved,
} from "@/features/databases/queries";
import { orgQuery } from "@/features/orgs/queries";
import { canWrite } from "@/features/orgs/roles";

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
    if (number <= (database.latest?.number ?? 0) && !database.missing.includes(number)) {
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
  if (wasRemoved(database, number)) return <RemovedVersion org={org} db={db} number={number} />;
  if (number > (database.latest?.number ?? 0)) return <NoSuchVersion />;
  return <VersionView org={org} database={database} number={number} />;
}

/** A number that was given, to a version removed since. */
function RemovedVersion({ org, db, number }: { org: string; db: string; number: number }) {
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  return (
    <div className="px-4 py-10 md:px-8">
      <EmptyState
        icon={<EyeClosedIcon />}
        title={`v${number} was removed`}
        description="An admin removed it from the history. It can be restored for 30 days after its removal, from the database's settings."
        action={
          <div className="flex flex-wrap justify-center gap-2">
            <Button asChild>
              <Link to="/$org/$db" params={{ org, db }}>
                Go to the latest version
              </Link>
            </Button>
            {canWrite(orgData.role) && (
              <Button variant="ghost" asChild>
                <Link to="/$org/$db/settings" params={{ org, db }} hash="removed-versions">
                  Removed versions
                </Link>
              </Button>
            )}
          </div>
        }
      />
    </div>
  );
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
