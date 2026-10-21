import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import * as React from "react";

import { PlusIcon } from "@/components/icons";
import { PageHeader } from "@/components/shell/page-header";
import { Button } from "@/components/ui/button";
import { AccessTokens } from "@/features/agent/components/access-tokens";
import { accessTokensQuery } from "@/features/agent/queries";

export const Route = createFileRoute("/settings/tokens")({
  loader: ({ context: { queryClient } }) => {
    void queryClient.prefetchQuery(accessTokensQuery);
  },
  head: () => ({ meta: [{ title: "Access tokens – romulus" }] }),
  component: AccessTokensPage,
});

function AccessTokensPage() {
  const tokens = useQuery(accessTokensQuery);
  const [creating, setCreating] = React.useState(false);
  return (
    <div className="grid w-full max-w-3xl gap-6">
      <PageHeader
        title="Access tokens"
        description="Keys your AI agents use to act as you. Revoke one to cut it off."
        actions={
          tokens.data && tokens.data.length > 0 ? (
            <Button variant="primary" onClick={() => setCreating(true)}>
              <PlusIcon />
              New token
            </Button>
          ) : undefined
        }
      />
      <AccessTokens creating={creating} onCreatingChange={setCreating} />
    </div>
  );
}
