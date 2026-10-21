import { createFileRoute } from "@tanstack/react-router";

import { PageHeader } from "@/components/shell/page-header";
import { ConnectAgent } from "@/features/agent/components/connect-agent";

export const Route = createFileRoute("/settings/agent")({
  head: () => ({ meta: [{ title: "Connect your agent – romulus" }] }),
  component: ConnectAgentPage,
});

function ConnectAgentPage() {
  return (
    <div className="grid w-full max-w-3xl gap-6">
      <PageHeader
        title="Connect your agent"
        description="Use romulus from Claude Code, Cursor or any MCP client."
      />
      <ConnectAgent />
    </div>
  );
}
