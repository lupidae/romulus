import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import {
  createFileRoute,
  Link,
  notFound,
  Outlet,
  useMatches,
  useNavigate,
  useParams,
} from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import {
  CreditCardIcon,
  DatabaseIcon,
  GroupIcon,
  MailOutIcon,
  SettingsIcon,
} from "@/components/icons";
import { AppShell, PageBody } from "@/components/shell/app-shell";
import { SidebarItem, SidebarNav, SidebarSection } from "@/components/ui/sidebar";
import { toast } from "@/components/ui/toaster";
import { ChangePasswordDialog } from "@/features/auth/components/change-password-dialog";
import { instanceQuery, meQuery } from "@/features/auth/queries";
import { requireUser, signOut } from "@/features/auth/session";
import { databaseQuery } from "@/features/databases/queries";
import { rememberLastOrg } from "@/features/orgs/last-org";
import { orgQuery, orgsQuery } from "@/features/orgs/queries";
import { ContractFooter } from "@/features/legal/components/contract-footer";
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
  },
  component: OrgLayout,
});

function OrgLayout() {
  const { org: slug } = Route.useParams();
  const { data: me } = useSuspenseQuery(meQuery);
  const { data: org } = useSuspenseQuery(orgQuery(slug));
  const { data: orgs } = useSuspenseQuery(orgsQuery);
  const instance = useQuery(instanceQuery);
  const navigate = useNavigate();
  const [changingPassword, setChangingPassword] = React.useState(false);
  const params = { org: slug };
  // Database pages lay themselves out edge to edge (the schema viewer).
  const { db } = useParams({ strict: false });
  const fullBleed = useMatches({
    select: (matches) => matches.some((m) => m.staticData.fullBleed),
  });

  // Here, not in the loader: hovering another org in the switcher preloads it.
  React.useEffect(() => rememberLastOrg(slug), [slug]);

  const nav = (
    <SidebarNav aria-label={org.name}>
      <SidebarSection>
        <SidebarItem asChild icon={<DatabaseIcon />} label="Databases">
          <Link to="/$org" params={params} activeOptions={{ exact: true }} />
        </SidebarItem>
      </SidebarSection>
      <SidebarSection label="Settings">
        <SidebarItem
          asChild
          icon={<SettingsIcon />}
          label="General"
          railLabel="Organization settings"
        >
          <Link to="/$org/settings" params={params} activeOptions={{ exact: true }} />
        </SidebarItem>
        <SidebarItem asChild icon={<GroupIcon />} label="Members">
          <Link to="/$org/settings/members" params={params} />
        </SidebarItem>
        {canManageMembers(org.role) && (
          <SidebarItem asChild icon={<MailOutIcon />} label="Invitations">
            <Link to="/$org/settings/invitations" params={params} />
          </SidebarItem>
        )}
        {/* Hosted only: a server without billing has no plans. */}
        {instance.data?.billing && (
          <SidebarItem asChild icon={<CreditCardIcon />} label="Billing">
            <Link to="/$org/settings/billing" params={params} />
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
      breadcrumb={db ? <DatabaseCrumb org={slug} db={db} /> : undefined}
      onSwitchOrg={(next) => void navigate({ to: "/$org", params: { org: next.slug } })}
      // Hidden until the server says organizations can be created.
      onCreateOrg={
        instance.data && !instance.data.single_org ? () => void navigate({ to: "/new" }) : undefined
      }
      onSignOut={() =>
        void signOut().catch(() => toast.error("Could not sign out. Check your connection."))
      }
      onChangePassword={me.password_account ? () => setChangingPassword(true) : undefined}
      source={instance.data?.source}
    >
      {fullBleed ? (
        <Outlet />
      ) : (
        <PageBody>
          <Outlet />
          <ContractFooter org={slug} />
        </PageBody>
      )}
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

/** The database being looked at, after the organization in the top bar. */
function DatabaseCrumb({ org, db }: { org: string; db: string }) {
  const database = useQuery(databaseQuery(org, db));
  return (
    <Link
      to="/$org/$db"
      params={{ org, db }}
      className="truncate rounded-md px-1.5 py-1 text-sm font-medium text-foreground outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
    >
      {database.data?.name ?? db}
    </Link>
  );
}
