import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, redirect, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { LogoMark } from "@/components/brand/logo";
import { ArrowLeftIcon, CheckCircleIcon } from "@/components/icons";
import { SchemaPreview } from "@/components/shell/schema-preview";
import { UserMenu } from "@/components/shell/user-menu";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/toaster";
import { ChangePasswordDialog } from "@/features/auth/components/change-password-dialog";
import { instanceQuery, meQuery } from "@/features/auth/queries";
import { requireUser, signOut } from "@/features/auth/session";
import { CreateOrgForm } from "@/features/orgs/components/create-org-form";
import { InviteForm } from "@/features/orgs/components/invite-form";
import { InviteLink } from "@/features/orgs/components/invite-link";
import { OrgPreview } from "@/features/orgs/components/org-preview";
import { type CreatedInvitation, orgsQuery } from "@/features/orgs/queries";
import { roleLabel } from "@/features/orgs/roles";

interface NewSearch {
  /** Set once the organization exists: the second step, inviting the team. */
  org?: string;
}

export const Route = createFileRoute("/new")({
  validateSearch: (search: Record<string, unknown>): NewSearch => ({
    org: typeof search.org === "string" ? search.org : undefined,
  }),
  beforeLoad: async ({ context: { queryClient }, location }) => {
    await requireUser(queryClient, location.href);
    // One organization per install: a member has nothing to create here.
    const [instance, orgs] = await Promise.all([
      queryClient.ensureQueryData(instanceQuery),
      queryClient.ensureQueryData(orgsQuery),
    ]);
    if (instance.single_org && orgs.length > 0) throw redirect({ to: "/", replace: true });
  },
  loader: ({ context: { queryClient } }) => queryClient.ensureQueryData(orgsQuery),
  head: () => ({ meta: [{ title: "Create an organization – romulus" }] }),
  component: NewOrganization,
});

function NewOrganization() {
  const { org: createdSlug } = Route.useSearch();
  const { data: me } = useSuspenseQuery(meQuery);
  const orgs = useQuery(orgsQuery).data ?? [];
  const instance = useQuery(instanceQuery);
  const singleOrg = instance.data?.single_org === true;
  const [changingPassword, setChangingPassword] = React.useState(false);
  const navigate = useNavigate();
  const created = orgs.find((o) => o.slug === createdSlug);
  const [draft, setDraft] = React.useState({ name: "", slug: "" });
  const host = window.location.host;
  // Someone who already has organizations can go back to one.
  const back = orgs.find((o) => o.slug !== createdSlug);

  const preview = created ? { name: created.name, slug: created.slug } : draft;

  return (
    <div className="relative grid min-h-dvh bg-background lg:grid-cols-[minmax(0,1fr)_minmax(0,1.1fr)]">
      {/* Top right of the whole page, over the preview on wide screens. */}
      <div className="absolute top-4 right-4 z-(--z-sticky) sm:top-5 sm:right-6">
        <UserMenu
          user={me}
          onSignOut={() =>
            void signOut().catch(() => toast.error("Could not sign out. Try again."))
          }
          onChangePassword={me.password_account ? () => setChangingPassword(true) : undefined}
          source={instance.data?.source}
        />
        {me.password_account && (
          <ChangePasswordDialog
            email={me.email}
            open={changingPassword}
            onOpenChange={setChangingPassword}
          />
        )}
      </div>
      <div className="flex flex-col px-6 py-5 sm:px-10">
        <header className="flex h-8 items-center gap-4 pr-12 lg:pr-0">
          <div className="flex min-w-0 items-center gap-3">
            <LogoMark className="size-8" />
            {!singleOrg && (
              <span className="text-sm text-muted-foreground tabular">
                Step {created ? 2 : 1} of 2
              </span>
            )}
            {back && !created && (
              <Button variant="ghost" size="sm" asChild>
                <Link to="/$org" params={{ org: back.slug }}>
                  <ArrowLeftIcon />
                  Back to {back.name}
                </Link>
              </Button>
            )}
          </div>
        </header>

        <main className="flex flex-1 items-center justify-center py-10">
          <div className="w-full max-w-md">
            {created ? (
              <InviteStep slug={created.slug} name={created.name} />
            ) : singleOrg ? (
              <NotInvitedYet email={me.email} />
            ) : (
              <>
                <h1 className="text-2xl font-semibold text-foreground">Create your organization</h1>
                <p className="mt-2 text-base text-muted-foreground">
                  {orgs.length === 0 && `Welcome to romulus, ${me.name.split(/\s+/)[0]}. `}
                  An organization holds your team&rsquo;s databases, versions and drafts.
                </p>
                <div className="mt-8">
                  <CreateOrgForm
                    urlPrefix={`${host}/app/`}
                    onChange={setDraft}
                    onCreated={(org) =>
                      void navigate({ to: "/new", search: { org: org.slug }, replace: true })
                    }
                  />
                </div>
                <p className="mt-6 text-sm text-muted-foreground">
                  Joining a team? Open the invitation link you were sent instead.
                </p>
              </>
            )}
          </div>
        </main>
      </div>

      <aside
        aria-hidden
        className="hidden items-center justify-center overflow-hidden border-l border-border bg-muted p-12 lg:flex"
      >
        {singleOrg && !created ? (
          <SchemaPreview />
        ) : (
          <OrgPreview name={preview.name} slug={preview.slug} user={me} host={host} />
        )}
      </aside>
    </div>
  );
}

/**
 * A single-organization install (`ROMULUS_SINGLE_ORG`): nothing to create,
 * someone signed in but not a member waits for an invitation.
 */
function NotInvitedYet({ email }: { email: string }) {
  return (
    <>
      <h1 className="text-2xl font-semibold text-foreground">
        You are not in the organization yet
      </h1>
      <p className="mt-2 text-base text-muted-foreground">
        This romulus server keeps one organization. Its admins invite people by email address: ask
        one of them to invite <span className="font-medium text-foreground">{email}</span>, then
        open the link they send.
      </p>
      <p className="mt-6 text-sm text-muted-foreground">
        Invited with another address? Sign out from the menu at the top right, then open the
        invitation link.
      </p>
    </>
  );
}

function InviteStep({ slug, name }: { slug: string; name: string }) {
  const [links, setLinks] = React.useState<CreatedInvitation[]>([]);
  const heading = React.useRef<HTMLHeadingElement>(null);

  // A new step is a new screen: move focus to its heading.
  React.useEffect(() => heading.current?.focus(), []);

  return (
    <>
      <h1
        ref={heading}
        tabIndex={-1}
        className="text-2xl font-semibold text-foreground outline-none"
      >
        Invite your team
      </h1>
      <p className="mt-2 flex items-start gap-1.5 text-base text-success">
        <CheckCircleIcon className="mt-1 size-4 shrink-0" />
        <span>
          <span className="font-medium">{name}</span> is ready.
        </span>
      </p>
      <p className="mt-1 text-base text-muted-foreground">
        Each person gets a link that works once, for their email address only, for 7 days. Send it
        however you like.
      </p>

      <div className="mt-8">
        <InviteForm
          slug={slug}
          pending={links.map((l) => l.invitation)}
          onCreated={(created) =>
            setLinks((all) => [
              created,
              ...all.filter((l) => l.invitation.id !== created.invitation.id),
            ])
          }
        />
      </div>

      {links.length > 0 && (
        <ul className="mt-6 grid gap-4" aria-label="Invitation links">
          {links.map((created, i) => (
            <li
              key={created.invitation.id}
              className="grid gap-2 rounded-lg border border-border bg-card p-3"
            >
              <div className="flex items-center justify-between gap-2">
                <span className="truncate text-sm font-medium text-foreground">
                  {created.invitation.email}
                </span>
                <Badge variant="neutral">{roleLabel[created.invitation.role]}</Badge>
              </div>
              <InviteLink created={created} autoCopy={i === 0} />
            </li>
          ))}
        </ul>
      )}

      <div className="mt-8 flex flex-col-reverse items-stretch gap-3 sm:flex-row sm:items-center sm:justify-between">
        <p className="text-sm text-muted-foreground">
          {links.length === 0
            ? "You can also invite people later, from Settings."
            : `${links.length} ${links.length === 1 ? "link" : "links"} ready to send.`}
        </p>
        <Button asChild>
          <Link to="/$org" params={{ org: slug }}>
            {links.length > 0 ? `Continue to ${name}` : "Skip for now"}
          </Link>
        </Button>
      </div>
    </>
  );
}
