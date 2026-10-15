import { createFileRoute, notFound } from "@tanstack/react-router";

import { ApiError } from "@/api/client";
import { requireUser } from "@/features/auth/session";
import { DraftEditor } from "@/features/draft/components/draft-editor";
import { draftQuery } from "@/features/draft/queries";
import { orgQuery } from "@/features/orgs/queries";
import { SchemaViewerSkeleton } from "@/features/schema/viewer/viewer-skeleton";

// A draft's editor, full screen: /app/<org>/<db>/drafts/<id>. Outside the
// shell's frame (the graph and the SQL need the room), with a bar of its own.
export const Route = createFileRoute("/$org_/$db/drafts/$draft")({
  beforeLoad: async ({ context: { queryClient }, location }) => {
    await requireUser(queryClient, location.href);
  },
  loader: async ({ context: { queryClient }, params }) => {
    try {
      const [, draft] = await Promise.all([
        queryClient.ensureQueryData(orgQuery(params.org)),
        queryClient.ensureQueryData(draftQuery(params.org, params.db, params.draft)),
      ]);
      return { name: draft.name, database: draft.database.name };
    } catch (error) {
      if (error instanceof ApiError && (error.status === 404 || error.status === 400))
        throw notFound();
      throw error;
    }
  },
  head: ({ loaderData }) => ({
    meta: [{ title: `${loaderData?.name ?? "Draft"} · ${loaderData?.database ?? ""} – romulus` }],
  }),
  pendingComponent: () => (
    <div className="flex h-dvh flex-col bg-background">
      <div className="h-12 shrink-0 border-b border-border bg-card" />
      <SchemaViewerSkeleton className="flex-1" />
    </div>
  ),
  component: DraftPage,
});

function DraftPage() {
  const { org, db, draft } = Route.useParams();
  return <DraftEditor org={org} db={db} draftId={draft} />;
}
