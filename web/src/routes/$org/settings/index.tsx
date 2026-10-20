import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { LogOutIcon } from "@/components/icons";
import { PageHeader } from "@/components/shell/page-header";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";
import { meQuery } from "@/features/auth/queries";
import { ApprovalsCard } from "@/features/orgs/components/approvals-card";
import { TestsCard } from "@/features/orgs/components/tests-card";
import { useLeaveOrg, useUpdateOrg } from "@/features/orgs/mutations";
import { membersQuery, type Org, orgQuery } from "@/features/orgs/queries";
import { canManageMembers } from "@/features/orgs/roles";
import { slugProblem } from "@/features/orgs/slug";

export const Route = createFileRoute("/$org/settings/")({
  head: () => ({ meta: [{ title: "Settings – romulus" }] }),
  component: General,
});

function General() {
  const { org: slug } = Route.useParams();
  const { data: org } = useSuspenseQuery(orgQuery(slug));
  const admin = canManageMembers(org.role);
  return (
    <>
      <PageHeader
        title="General"
        description={
          admin
            ? "The organization's name and its address in romulus."
            : "The organization's name and address. Only admins can change them."
        }
      />
      {/* Remounted on save, so the form starts from what was saved. */}
      <OrgForm key={`${org.id}:${org.name}:${org.slug}`} org={org} editable={admin} />
      <ApprovalsCard org={org} editable={admin} />
      <TestsCard org={org} editable={admin} />
      <LeaveCard org={org} />
    </>
  );
}

function OrgForm({ org, editable }: { org: Org; editable: boolean }) {
  const update = useUpdateOrg(org.slug);
  const navigate = useNavigate();
  const [name, setName] = React.useState(org.name);
  const [slug, setSlug] = React.useState(org.slug);
  const [errors, setErrors] = React.useState<{ name?: string; slug?: string }>({});
  const dirty = name.trim() !== org.name || slug !== org.slug;

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    const problems = {
      name: name.trim() ? undefined : "Give the organization a name.",
      slug: slugProblem(slug),
    };
    setErrors(problems);
    if (problems.name || problems.slug) return;
    update.mutate(
      {
        name: name.trim() !== org.name ? name.trim() : undefined,
        slug: slug !== org.slug ? slug : undefined,
      },
      {
        onSuccess: (saved) => {
          toast.success("Settings saved");
          if (saved.slug !== org.slug) {
            void navigate({ to: "/$org/settings", params: { org: saved.slug }, replace: true });
          }
        },
        onError: (error) => {
          if (error instanceof ApiError && error.code === "slug_taken") {
            setErrors({ slug: "That URL is taken. Try another one." });
          } else if (error instanceof ApiError && error.code === "validation") {
            setErrors(error.fields);
          } else {
            toast.error(error instanceof ApiError ? error.message : "Could not save. Try again.");
          }
        },
      },
    );
  };

  return (
    <Card className="max-w-2xl">
      <form noValidate onSubmit={submit}>
        <CardContent className="grid gap-5">
          <Field label="Name" error={errors.name}>
            <Input
              value={name}
              disabled={!editable}
              maxLength={100}
              autoComplete="organization"
              onChange={(e) => {
                setName(e.target.value);
                setErrors((all) => ({ ...all, name: undefined }));
              }}
            />
          </Field>
          <Field
            label="URL"
            error={errors.slug}
            description={
              slug !== org.slug
                ? "Links to the old address will stop working."
                : "Where the organization lives in romulus."
            }
          >
            <Input
              prefix={`${window.location.host}/app/`}
              value={slug}
              disabled={!editable}
              maxLength={63}
              spellCheck={false}
              autoComplete="off"
              className="font-mono"
              onChange={(e) => {
                setSlug(e.target.value.toLowerCase().replace(/\s+/g, "-"));
                setErrors((all) => ({ ...all, slug: undefined }));
              }}
            />
          </Field>
        </CardContent>
        {editable && (
          <div className="flex items-center justify-end gap-2 border-t border-border px-4 py-3">
            {dirty && (
              <Button
                onClick={() => {
                  setName(org.name);
                  setSlug(org.slug);
                  setErrors({});
                }}
              >
                Discard
              </Button>
            )}
            <Button type="submit" variant="primary" disabled={!dirty} loading={update.isPending}>
              Save changes
            </Button>
          </div>
        )}
      </form>
    </Card>
  );
}

/** Leaving, with the last admin told what to do first instead of a dead button. */
function LeaveCard({ org }: { org: Org }) {
  const { data: me } = useSuspenseQuery(meQuery);
  const members = useQuery(membersQuery(org.slug));
  const leave = useLeaveOrg(org.slug, me.id);
  const navigate = useNavigate();
  const [open, setOpen] = React.useState(false);
  const onlyAdmin =
    org.role === "admin" && members.data?.filter((m) => m.role === "admin").length === 1;

  return (
    <Card className="max-w-2xl">
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle>Leave {org.name}</CardTitle>
          <p className="text-sm text-muted-foreground">
            You lose access to its databases. To come back, an admin has to invite you again.
          </p>
        </div>
      </CardHeader>
      <CardContent className="flex flex-wrap items-center justify-between gap-3">
        {members.isPending ? (
          <Skeleton className="h-8 w-40" />
        ) : onlyAdmin ? (
          <p className="text-sm text-muted-foreground">
            You are the only admin.{" "}
            <Link
              to="/$org/settings/members"
              params={{ org: org.slug }}
              className="font-medium text-primary underline-offset-4 hover:underline"
            >
              Make someone else an admin
            </Link>{" "}
            before you leave.
          </p>
        ) : (
          <Button onClick={() => setOpen(true)}>
            <LogOutIcon />
            Leave organization…
          </Button>
        )}
      </CardContent>
      <ConfirmDialog
        open={open}
        onOpenChange={setOpen}
        title={`Leave ${org.name}?`}
        description="You lose access to its databases and drafts right away. To come back, an admin has to invite you again."
        confirmLabel="Leave organization"
        destructive
        onConfirm={async () => {
          try {
            await leave.mutateAsync();
          } catch (error) {
            toast.error(error instanceof ApiError ? error.message : "Could not leave. Try again.");
            throw error;
          }
          toast.success(`You left ${org.name}`);
          await navigate({ to: "/" });
        }}
      />
    </Card>
  );
}
