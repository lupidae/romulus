import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, notFound, Outlet, useNavigate } from "@tanstack/react-router";

import { ApiError } from "@/api/client";
import { DatabaseIcon, GroupIcon, MailOutIcon, SettingsIcon } from "@/components/icons";
import { AppShell, PageBody } from "@/components/shell/app-shell";
import { SidebarItem, SidebarNav, SidebarSection } from "@/components/ui/sidebar";
import { toast } from "@/components/ui/toaster";
import { meQuery } from "@/features/auth/queries";
import { requireUser, signOut } from "@/features/auth/session";
import { rememberLastOrg } from "@/features/orgs/last-org";
import { orgQuery, orgsQuery } from "@/features/orgs/queries";
import { canManageMembers } from "@/features/orgs/roles";

// Every page of an organization: /app/<slug>/… inside the app shell. Signed
// out → sign-in (and back); not a member (or no such slug) → not found.
export const Route = createFileRoute("/$org")({
  beforeLoad: async ({ context: { queryClient }, location }) => {
    await requireUser(queryClient, location.href);
  },
  loader: async ({ context: { queryClient }, params }) => {
    try {
      await Promise.all([
        queryClient.ensureQueryData(orgQuery(params.org)),
        queryClient.ensureQueryData(orgsQuery),
      ]);
    } catch (error) {
      if (error instanceof ApiError && error.status === 404) throw notFound();
      throw error;
    }
    rememberLastOrg(params.org);
  },
  component: OrgLayout,
});

function OrgLayout() {
  const { org: slug } = Route.useParams();
  const { data: me } = useSuspenseQuery(meQuery);
  const { data: org } = useSuspenseQuery(orgQuery(slug));
  const { data: orgs } = useSuspenseQuery(orgsQuery);
  const navigate = useNavigate();
  const params = { org: slug };

  const nav = (
    <SidebarNav aria-label={org.name}>
      <SidebarSection>
        <SidebarItem asChild>
          <Link to="/$org" params={params} activeOptions={{ exact: true }}>
            <DatabaseIcon />
            Databases
          </Link>
        </SidebarItem>
      </SidebarSection>
      <SidebarSection label="Settings">
        <SidebarItem asChild>
          <Link to="/$org/settings" params={params} activeOptions={{ exact: true }}>
            <SettingsIcon />
            General
          </Link>
        </SidebarItem>
        <SidebarItem asChild>
          <Link to="/$org/settings/members" params={params}>
            <GroupIcon />
            Members
          </Link>
        </SidebarItem>
        {canManageMembers(org.role) && (
          <SidebarItem asChild>
            <Link to="/$org/settings/invitations" params={params}>
              <MailOutIcon />
              Invitations
            </Link>
          </SidebarItem>
        )}
      </SidebarSection>
    </SidebarNav>
  );

  return (
    <AppShell
      user={me}
      org={org}
      // The list may lag a moment behind a join or a rename; never hide the current one.
      orgs={orgs.some((o) => o.id === org.id) ? orgs : [...orgs, org]}
      nav={nav}
      onSwitchOrg={(next) => void navigate({ to: "/$org", params: { org: next.slug } })}
      onCreateOrg={() => void navigate({ to: "/new" })}
      onSignOut={() =>
        void signOut().catch(() => toast.error("Could not sign out. Check your connection."))
      }
    >
      <PageBody>
        <Outlet />
      </PageBody>
    </AppShell>
  );
}
