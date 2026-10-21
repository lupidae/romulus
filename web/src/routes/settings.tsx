import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, Outlet, redirect, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { AppWindowIcon, DatabaseIcon, KeyIcon, SparksIcon } from "@/components/icons";
import { AppShell, PageBody } from "@/components/shell/app-shell";
import { SidebarItem, SidebarNav, SidebarSection } from "@/components/ui/sidebar";
import { toast } from "@/components/ui/toaster";
import { ChangePasswordDialog } from "@/features/auth/components/change-password-dialog";
import { instanceQuery, meQuery } from "@/features/auth/queries";
import { requireUser, signOut } from "@/features/auth/session";
import { readLastOrg } from "@/features/orgs/last-org";
import { orgsQuery } from "@/features/orgs/queries";

// The signed-in person's own settings (/app/settings/…): their AI agents, the
// apps they connected and their access tokens, in every organization at once.
// The shell keeps the last organization visited in its switcher, so getting
// back is one click.
export const Route = createFileRoute("/settings")({
  beforeLoad: async ({ context: { queryClient }, location }) => {
    await requireUser(queryClient, location.href);
    const orgs = await queryClient.ensureQueryData(orgsQuery);
    if (orgs.length === 0) throw redirect({ to: "/new", replace: true });
  },
  component: AccountLayout,
});

function AccountLayout() {
  const { data: me } = useSuspenseQuery(meQuery);
  const { data: orgs } = useSuspenseQuery(orgsQuery);
  const instance = useQuery(instanceQuery);
  const navigate = useNavigate();
  const [changingPassword, setChangingPassword] = React.useState(false);
  const last = readLastOrg();
  const org = orgs.find((o) => o.slug === last) ?? orgs[0];

  const nav = (
    <SidebarNav aria-label="Your account">
      <SidebarSection label="Your account">
        <SidebarItem asChild icon={<SparksIcon />} label="Connect your agent">
          <Link to="/settings/agent" />
        </SidebarItem>
        <SidebarItem asChild icon={<AppWindowIcon />} label="Connected apps">
          <Link to="/settings/apps" />
        </SidebarItem>
        <SidebarItem asChild icon={<KeyIcon />} label="Access tokens">
          <Link to="/settings/tokens" />
        </SidebarItem>
      </SidebarSection>
      {org && (
        <SidebarSection label={org.name}>
          <SidebarItem asChild icon={<DatabaseIcon />} label="Databases">
            <Link to="/$org" params={{ org: org.slug }} />
          </SidebarItem>
        </SidebarSection>
      )}
    </SidebarNav>
  );

  if (!org) return null;
  return (
    <AppShell
      user={me}
      org={org}
      orgs={orgs}
      nav={nav}
      onSwitchOrg={(next) => void navigate({ to: "/$org", params: { org: next.slug } })}
      onCreateOrg={
        instance.data && !instance.data.single_org ? () => void navigate({ to: "/new" }) : undefined
      }
      onSignOut={() =>
        void signOut().catch(() => toast.error("Could not sign out. Check your connection."))
      }
      onChangePassword={me.password_account ? () => setChangingPassword(true) : undefined}
    >
      <PageBody>
        <Outlet />
      </PageBody>
      {me.password_account && (
        <ChangePasswordDialog
          email={me.email}
          open={changingPassword}
          onOpenChange={setChangingPassword}
        />
      )}
    </AppShell>
  );
}
