import { createFileRoute, Link, notFound, Outlet } from "@tanstack/react-router";

import { DatabaseIcon, GroupIcon, SettingsIcon } from "@/components/icons";
import { AppShell, PageBody } from "@/components/shell/app-shell";
import { SidebarItem, SidebarNav, SidebarSection } from "@/components/ui/sidebar";
import { toast } from "@/components/ui/toaster";

import { orgs, user } from "./-fixtures";

// The app shell with sample data, for building and reviewing the frame before
// sessions and organizations exist. Dev builds only. Same structure as the
// real org routes will have: a layout route renders the shell, child routes
// render pages into it.

export const Route = createFileRoute("/dev/shell")({
  beforeLoad: () => {
    if (!import.meta.env.DEV) throw notFound();
  },
  component: ShellPreview,
});

function ShellPreview() {
  const nav = (
    <SidebarNav aria-label="Organization">
      <SidebarSection>
        <SidebarItem asChild>
          <Link to="/dev/shell" activeOptions={{ exact: true }}>
            <DatabaseIcon />
            Databases
          </Link>
        </SidebarItem>
        <SidebarItem asChild>
          <Link to="/dev/shell/members">
            <GroupIcon />
            Members
          </Link>
        </SidebarItem>
        <SidebarItem asChild>
          <Link to="/dev/shell/settings">
            <SettingsIcon />
            Settings
          </Link>
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
      onSwitchOrg={(o) => toast(`Switched to ${o.name}`, { description: "Preview only." })}
      onCreateOrg={() => toast("Create organization", { description: "Preview only." })}
      onSignOut={() => toast("Signed out", { description: "Preview only." })}
    >
      <PageBody>
        <Outlet />
      </PageBody>
    </AppShell>
  );
}
