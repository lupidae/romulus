import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";

import { DatabaseIcon, UserPlusIcon } from "@/components/icons";
import { PageHeader } from "@/components/shell/page-header";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import { orgQuery } from "@/features/orgs/queries";
import { canManageMembers } from "@/features/orgs/roles";

// The organization's home. Databases arrive with their own module; until
// then this is their empty state, and the way to bring the team in.
export const Route = createFileRoute("/$org/")({
  head: () => ({ meta: [{ title: "Databases – romulus" }] }),
  component: OrgHome,
});

function OrgHome() {
  const { org: slug } = Route.useParams();
  const { data: org } = useSuspenseQuery(orgQuery(slug));
  return (
    <>
      <PageHeader title="Databases" description={`Schemas ${org.name} keeps in romulus.`} />
      <Card>
        <EmptyState
          icon={<DatabaseIcon />}
          title="No databases yet"
          description="Databases this organization keeps in romulus, their versions and drafts, will be listed here."
          action={
            canManageMembers(org.role) && (
              <Button variant="primary" asChild>
                <Link
                  to="/$org/settings/invitations"
                  params={{ org: slug }}
                  search={{ invite: true }}
                >
                  <UserPlusIcon />
                  Invite your team
                </Link>
              </Button>
            )
          }
        />
      </Card>
    </>
  );
}
