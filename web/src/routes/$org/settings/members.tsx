import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import * as React from "react";

import { UserPlusIcon } from "@/components/icons";
import { PageHeader } from "@/components/shell/page-header";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { meQuery } from "@/features/auth/queries";
import { InviteDialog } from "@/features/orgs/components/invite-dialog";
import { MembersTable, MembersTableSkeleton } from "@/features/orgs/components/members-table";
import { invitationsQuery, membersQuery, orgQuery } from "@/features/orgs/queries";
import { canManageMembers } from "@/features/orgs/roles";

export const Route = createFileRoute("/$org/settings/members")({
  head: () => ({ meta: [{ title: "Members – romulus" }] }),
  component: Members,
});

function Members() {
  const { org: slug } = Route.useParams();
  const { data: org } = useSuspenseQuery(orgQuery(slug));
  const { data: me } = useSuspenseQuery(meQuery);
  const admin = canManageMembers(org.role);
  const members = useQuery(membersQuery(slug));
  const invitations = useQuery({ ...invitationsQuery(slug), enabled: admin });
  const [inviting, setInviting] = React.useState(false);
  const pending = invitations.data?.length ?? 0;

  return (
    <>
      <PageHeader
        title="Members"
        description="Who can read, edit and manage this organization's databases."
        actions={
          admin && (
            <Button variant="primary" onClick={() => setInviting(true)}>
              <UserPlusIcon />
              Invite
            </Button>
          )
        }
      />
      <Card>
        {members.isPending ? (
          <MembersTableSkeleton />
        ) : members.isError ? (
          <div className="p-4">
            <Alert
              variant="destructive"
              title="Could not load the members"
              action={
                <Button
                  size="sm"
                  loading={members.isFetching}
                  onClick={() => void members.refetch()}
                >
                  Retry
                </Button>
              }
            >
              {members.error.message}
            </Alert>
          </div>
        ) : (
          <MembersTable org={org} members={members.data} myId={me.id} />
        )}
      </Card>
      {admin && pending > 0 && (
        <p className="text-sm text-muted-foreground">
          {pending === 1 ? "1 invitation is" : `${pending} invitations are`} waiting to be accepted.{" "}
          <Link
            to="/$org/settings/invitations"
            params={{ org: slug }}
            className="font-medium text-primary underline-offset-4 hover:underline"
          >
            See invitations
          </Link>
        </p>
      )}
      <InviteDialog
        open={inviting}
        onOpenChange={setInviting}
        slug={slug}
        orgName={org.name}
        pending={invitations.data}
      />
    </>
  );
}
