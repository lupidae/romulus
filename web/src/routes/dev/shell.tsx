import { createFileRoute, Link, notFound, Outlet } from "@tanstack/react-router";

import { DatabaseIcon, GroupIcon, SettingsIcon } from "@/components/icons";
import { AppShell, PageBody } from "@/components/shell/app-shell";
import { SidebarItem, SidebarNav, SidebarSection } from "@/components/ui/sidebar";
import { toast } from "@/components/ui/toaster";
import { devRoutes } from "@/lib/dev-routes";

import { orgs, user } from "./-fixtures";

// The app shell with sample data, for building and reviewing the frame before
// sessions and organizations exist. Dev builds only. Same structure as the
// real org routes will have: a layout route renders the shell, child routes
// render pages into it.

export const Route = createFileRoute("/dev/shell")({
  beforeLoad: () => {
    if (!devRoutes) throw notFound();
  },
  component: ShellPreview,
});

function ShellPreview() {
  const nav = (
    <SidebarNav aria-label="Organization">
      <SidebarSection>
        <SidebarItem asChild icon={<DatabaseIcon />} label="Databases">
          <Link to="/dev/shell" activeOptions={{ exact: true }} />
        </SidebarItem>
        <SidebarItem asChild icon={<GroupIcon />} label="Members">
          <Link to="/dev/shell/members" />
        </SidebarItem>
        <SidebarItem asChild icon={<SettingsIcon />} label="Settings">
          <Link to="/dev/shell/settings" />
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
      source={{
        url: "https://github.com/lupidae/romulus",
        commit: "1f1ded1c0ffee5e5a1d2b3c4d5e6f708192a3b4c",
        version: "0.1.0",
      }}
    >
      <PageBody>
        <Outlet />
      </PageBody>
    </AppShell>
  );
}
