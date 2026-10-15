import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";

import { LockIcon, MailOutIcon, UserPlusIcon } from "@/components/icons";
import { PageHeader } from "@/components/shell/page-header";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import {
  InvitationsList,
  InvitationsListSkeleton,
} from "@/features/orgs/components/invitations-list";
import { InviteDialog } from "@/features/orgs/components/invite-dialog";
import { invitationsQuery, orgQuery } from "@/features/orgs/queries";
import { canManageMembers } from "@/features/orgs/roles";

interface InvitationsSearch {
  /** Opens the invite dialog (links from elsewhere: "Invite your team"). */
  invite?: boolean;
}

export const Route = createFileRoute("/$org/settings/invitations")({
  validateSearch: (search: Record<string, unknown>): InvitationsSearch => ({
    invite: search.invite === true || search.invite === "true" ? true : undefined,
  }),
  head: () => ({ meta: [{ title: "Invitations – romulus" }] }),
  component: Invitations,
});

function Invitations() {
  const { org: slug } = Route.useParams();
  const { invite } = Route.useSearch();
  const navigate = useNavigate({ from: Route.fullPath });
  const { data: org } = useSuspenseQuery(orgQuery(slug));
  const admin = canManageMembers(org.role);
  const invitations = useQuery({ ...invitationsQuery(slug), enabled: admin });
  const setInviting = (open: boolean) =>
    void navigate({ search: { invite: open || undefined }, replace: true });

  if (!admin) {
    return (
      <>
        <PageHeader title="Invitations" />
        <Card>
          <EmptyState
            icon={<LockIcon />}
            title="Only admins manage invitations"
            description={`Ask an admin of ${org.name} to invite the people you work with.`}
          />
        </Card>
      </>
    );
  }

  const inviteButton = (
    <Button variant="primary" onClick={() => setInviting(true)}>
      <UserPlusIcon />
      Invite
    </Button>
  );
  const empty = invitations.data?.length === 0;

  return (
    <>
      <PageHeader
        title="Invitations"
        description="Links waiting to be used. Each works once, for one email address, for 7 days."
        actions={!empty && inviteButton}
      />
      <Card>
        {invitations.isPending ? (
          <InvitationsListSkeleton />
        ) : invitations.isError ? (
          <div className="p-4">
            <Alert
              variant="destructive"
              title="Could not load the invitations"
              action={
                <Button
                  size="sm"
                  loading={invitations.isFetching}
                  onClick={() => void invitations.refetch()}
                >
                  Retry
                </Button>
              }
            >
              {invitations.error.message}
            </Alert>
          </div>
        ) : empty ? (
          <EmptyState
            icon={<MailOutIcon />}
            title="No pending invitations"
            description="Invite someone by email: you get a link to send them, and they join with the role you pick."
            action={inviteButton}
          />
        ) : (
          <InvitationsList slug={slug} orgName={org.name} invitations={invitations.data} />
        )}
      </Card>
      <InviteDialog
        open={invite === true}
        onOpenChange={setInviting}
        slug={slug}
        orgName={org.name}
        pending={invitations.data}
      />
    </>
  );
}
