import { createFileRoute } from "@tanstack/react-router";

import { PageHeader } from "@/components/shell/page-header";
import { ConnectedApps } from "@/features/agent/components/connected-apps";
import { connectedAppsQuery } from "@/features/agent/queries";

export const Route = createFileRoute("/settings/apps")({
  loader: ({ context: { queryClient } }) => {
    void queryClient.prefetchQuery(connectedAppsQuery);
  },
  head: () => ({ meta: [{ title: "Connected apps – romulus" }] }),
  component: ConnectedAppsPage,
});

function ConnectedAppsPage() {
  return (
    <div className="grid w-full max-w-3xl gap-6">
      <PageHeader
        title="Connected apps"
        description="AI agents you approved. Each acts as you until you disconnect it."
      />
      <ConnectedApps />
    </div>
  );
}
