import { createFileRoute, Link, notFound } from "@tanstack/react-router";
import * as React from "react";

import { ClockRotateRightIcon, DatabaseIcon } from "@/components/icons";
import { AppShell } from "@/components/shell/app-shell";
import { Badge } from "@/components/ui/badge";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { SidebarItem, SidebarNav, SidebarSection } from "@/components/ui/sidebar";
import { toast } from "@/components/ui/toaster";
import type { VersionLink } from "@/features/time-machine/components/history-panel";
import { preloadAutoLayout } from "@/features/schema/graph/use-auto-layout";
import { TimeMachine } from "@/features/time-machine/components/time-machine";
import { TimeMachineSkeleton } from "@/features/time-machine/components/time-machine-skeleton";
import { largeHistory, shopHistory } from "@/features/time-machine/fixtures";
import { parseHistory } from "@/features/time-machine/history";
import { devRoutes } from "@/lib/dev-routes";

import { orgs, user } from "./-fixtures";

// The time machine on sample histories, without the API. Dev builds only.
//   ?fixture=shop|large   eight versions of a shop, or 220 tables over 120 versions
//   ?removed=1            the shop with a removed version (an admin's view)
//   ?state=loading        the loading state

const FIXTURES = ["shop", "large"] as const;
type FixtureId = (typeof FIXTURES)[number];

interface Search {
  fixture?: FixtureId;
  removed?: 1;
  state?: "loading";
}

export const Route = createFileRoute("/dev/time-machine")({
  validateSearch: (search: Record<string, unknown>): Search => ({
    fixture: FIXTURES.includes(search.fixture as FixtureId)
      ? (search.fixture as FixtureId)
      : undefined,
    removed: search.removed === 1 || search.removed === "1" ? 1 : undefined,
    state: search.state === "loading" ? "loading" : undefined,
  }),
  beforeLoad: () => {
    if (!devRoutes) throw notFound();
  },
  loader: () => preloadAutoLayout(),
  head: () => ({ meta: [{ title: "Time machine – romulus" }] }),
  component: Preview,
});

const DevVersionLink: VersionLink = ({ className, children, label }) => (
  <a
    href="#diff"
    aria-label={label}
    className={className}
    onClick={(event) => {
      event.preventDefault();
      toast("The diff opens here", { description: "Preview only." });
    }}
  >
    {children}
  </a>
);

function Preview() {
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  const id = search.fixture ?? "shop";
  const history = React.useMemo(
    () =>
      parseHistory(
        id === "large" ? largeHistory() : shopHistory({ removed: search.removed === 1 }),
      ),
    [id, search.removed],
  );
  const last = history.versions.length - 1;
  const nav = (
    <SidebarNav aria-label="Organization">
      <SidebarSection>
        <SidebarItem asChild icon={<DatabaseIcon />} label="Databases">
          <Link to="/dev/time-machine" />
        </SidebarItem>
      </SidebarSection>
    </SidebarNav>
  );
  return (
    <AppShell
      user={user}
      org={orgs[0]!}
      orgs={orgs}
      nav={nav}
      breadcrumb={<span className="truncate font-mono text-sm text-foreground">time machine</span>}
      onSwitchOrg={(o) => toast(`Switched to ${o.name}`, { description: "Preview only." })}
      onCreateOrg={() => toast("Create organization", { description: "Preview only." })}
      onSignOut={() => toast("Signed out", { description: "Preview only." })}
    >
      <div className="flex h-full min-h-0 flex-col">
        <div className="flex min-h-14 shrink-0 flex-wrap items-center gap-x-3 gap-y-2 border-b border-border px-4 py-2.5">
          <span className="flex size-8 items-center justify-center rounded-md border border-border bg-muted text-muted-foreground">
            <ClockRotateRightIcon className="size-4" />
          </span>
          <h1 className="text-lg font-semibold text-foreground">Time machine</h1>
          <Badge variant="outline">Dev fixture</Badge>
          <Select
            value={id}
            onValueChange={(v) => void navigate({ search: { fixture: v as FixtureId } })}
          >
            <SelectTrigger aria-label="Sample history" className="ml-auto w-56">
              <SelectValue />
            </SelectTrigger>
            <SelectContent align="end">
              <SelectItem value="shop">A shop, 8 versions</SelectItem>
              <SelectItem value="large">220 tables, 120 versions</SelectItem>
            </SelectContent>
          </Select>
        </div>
        <div className="min-h-0 flex-1">
          {search.state === "loading" ? (
            <TimeMachineSkeleton />
          ) : (
            <TimeMachine
              key={`${id}:${search.removed ?? 0}`}
              history={history}
              initial={{
                range: { from: 0, to: last },
                at: last,
                mode: "superposed",
                changedOnly: false,
                group: null,
                query: "",
                table: null,
              }}
              VersionLink={DevVersionLink}
              layoutKey={`dev-${id}`}
            />
          )}
        </div>
      </div>
    </AppShell>
  );
}
