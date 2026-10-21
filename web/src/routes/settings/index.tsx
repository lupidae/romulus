import { createFileRoute, redirect } from "@tanstack/react-router";

// The account's settings open on connecting an agent.
export const Route = createFileRoute("/settings/")({
  beforeLoad: () => {
    throw redirect({ to: "/settings/agent", replace: true });
  },
});
