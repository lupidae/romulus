import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import { SparksIcon, XmarkIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Tooltip } from "@/components/ui/tooltip";
import { usePreference } from "@/lib/use-preference";

import { accessTokensQuery, connectedAppsQuery } from "../queries";

const isBoolean = (value: unknown): value is boolean => typeof value === "boolean";

/**
 * On an organization's home, for someone who has not connected an agent
 * yet: what one can do here, and the way to connect it. Dismissed for good
 * in this browser.
 */
export function AgentCard({ userId, writer }: { userId: string; writer: boolean }) {
  const tokens = useQuery(accessTokensQuery);
  const apps = useQuery(connectedAppsQuery);
  const [dismissed, setDismissed] = usePreference(`romulus:agent-card:${userId}`, false, isBoolean);
  if (dismissed || !tokens.data || !apps.data) return null;
  if (tokens.data.length > 0 || apps.data.length > 0) return null;
  return (
    <Card className="flex flex-col gap-4 p-4 sm:flex-row sm:items-center sm:gap-5">
      <span
        aria-hidden
        className="flex size-9 shrink-0 items-center justify-center rounded-md border border-border bg-muted text-muted-foreground [&_svg]:size-4"
      >
        <SparksIcon />
      </span>
      <div className="grid min-w-0 flex-1 gap-0.5">
        <h2 className="text-base font-semibold text-foreground">Work here from your AI agent</h2>
        <p className="text-sm text-muted-foreground">
          {writer
            ? "Claude Code, Claude Desktop or Cursor can bring a database in, draft a change, and test its migration on data, as you."
            : "Claude Code, Claude Desktop or Cursor can read these schemas, diffs and drafts for you, with your access."}
        </p>
      </div>
      <div className="flex shrink-0 items-center gap-1">
        <Button asChild>
          <Link to="/settings/agent">
            <SparksIcon />
            Connect your agent
          </Link>
        </Button>
        <Tooltip content="Dismiss">
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Dismiss the agent card"
            onClick={() => setDismissed(true)}
          >
            <XmarkIcon />
          </Button>
        </Tooltip>
      </div>
    </Card>
  );
}
