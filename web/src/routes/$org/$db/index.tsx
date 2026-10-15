import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";

import { GetStarted } from "@/features/databases/components/get-started";
import { VersionView } from "@/features/databases/components/version-view";
import { databaseQuery, versionQuery, versionTextsQuery } from "@/features/databases/queries";
import { useOpenUpload } from "@/features/databases/upload-context";

// The database's schema as it is now: its latest version, or the ways in.
export const Route = createFileRoute("/$org/$db/")({
  loader: async ({ context: { queryClient }, params: { org, db } }) => {
    const database = await queryClient.ensureQueryData(databaseQuery(org, db));
    const latest = database.latest?.number;
    if (latest) {
      void queryClient.prefetchQuery(versionQuery(org, db, database.id, latest));
      void queryClient.prefetchQuery(versionTextsQuery(org, db, database.id, latest));
    }
  },
  component: Latest,
});

function Latest() {
  const { org, db } = Route.useParams();
  const { data: database } = useSuspenseQuery(databaseQuery(org, db));
  const openUpload = useOpenUpload();
  if (!database.latest) {
    return (
      <GetStarted
        org={org}
        database={database}
        writer={openUpload !== null}
        onUpload={() => openUpload?.()}
      />
    );
  }
  return <VersionView org={org} database={database} number={database.latest.number} />;
}
