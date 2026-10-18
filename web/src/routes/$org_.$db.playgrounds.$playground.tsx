import { createFileRoute, notFound } from "@tanstack/react-router";

import { ApiError } from "@/api/client";
import { Skeleton } from "@/components/ui/skeleton";
import { meQuery } from "@/features/auth/queries";
import { requireUser } from "@/features/auth/session";
import { orgQuery } from "@/features/orgs/queries";
import { PlaygroundPage } from "@/features/playground/components/playground-page";
import { playgroundQuery } from "@/features/playground/queries";

// A playground, full screen: /app/<org>/<db>/playgrounds/<id>. Outside the
// shell's frame like the draft editor: the console needs the room.
export const Route = createFileRoute("/$org_/$db/playgrounds/$playground")({
  beforeLoad: async ({ context: { queryClient }, location }) => {
    await requireUser(queryClient, location.href);
  },
  loader: async ({ context: { queryClient }, params }) => {
    try {
      const [, , playground] = await Promise.all([
        queryClient.ensureQueryData(orgQuery(params.org)),
        queryClient.ensureQueryData(meQuery),
        queryClient.ensureQueryData(playgroundQuery(params.org, params.db, params.playground)),
      ]);
      return {
        title: playground.draft ? `“${playground.draft.name}”` : `v${playground.version}`,
      };
    } catch (error) {
      if (error instanceof ApiError && (error.status === 404 || error.status === 400))
        throw notFound();
      throw error;
    }
  },
  head: ({ loaderData }) => ({
    meta: [{ title: `Playground · ${loaderData?.title ?? ""} – romulus` }],
  }),
  pendingComponent: () => (
    <div className="flex h-dvh flex-col bg-background">
      <div className="h-12 shrink-0 border-b border-border bg-card" />
      <div className="flex min-h-0 flex-1">
        <div className="hidden w-64 shrink-0 border-r border-border bg-card p-3 md:grid md:content-start md:gap-2">
          <Skeleton className="h-5 w-24" />
          <Skeleton className="h-7 w-full" />
          <Skeleton className="h-7 w-full" />
        </div>
        <div className="flex-1 p-4">
          <Skeleton className="h-32 w-full" />
        </div>
      </div>
    </div>
  ),
  component: Page,
});

function Page() {
  const { org, db, playground } = Route.useParams();
  return <PlaygroundPage org={org} db={db} id={playground} />;
}
