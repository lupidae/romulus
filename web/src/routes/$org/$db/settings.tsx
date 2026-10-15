import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { LockIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { CopyField } from "@/components/ui/copy-field";
import { EmptyState } from "@/components/ui/empty-state";
import { Field } from "@/components/ui/field";
import { Input, Textarea } from "@/components/ui/input";
import { toast } from "@/components/ui/toaster";
import { ApiTokens } from "@/features/databases/components/api-tokens";
import { useDeleteDatabase, useUpdateDatabase } from "@/features/databases/mutations";
import { type Database, databaseQuery } from "@/features/databases/queries";
import { databaseSlugProblem } from "@/features/databases/slug";
import { orgQuery } from "@/features/orgs/queries";
import { canWrite } from "@/features/orgs/roles";

// Name, address and description; API tokens; deleting. Developers and admins.
export const Route = createFileRoute("/$org/$db/settings")({
  head: () => ({ meta: [{ title: "Database settings – romulus" }] }),
  component: Settings,
});

function Settings() {
  const { org, db } = Route.useParams();
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  const { data: database } = useSuspenseQuery(databaseQuery(org, db));

  if (!canWrite(orgData.role)) {
    return (
      <div className="px-4 py-10 md:px-8">
        <EmptyState
          icon={<LockIcon />}
          title="Settings are for developers"
          description="Reviewers read every version. Ask an admin to make you a developer to rename, delete or push to this database."
        />
      </div>
    );
  }
  return (
    <div className="grid w-full max-w-3xl gap-6 px-4 py-6 md:px-8">
      {/* Remounted on save, so the form starts from what was saved. */}
      <GeneralCard
        key={`${database.name}:${database.slug}:${database.description}`}
        org={org}
        database={database}
      />
      <Card>
        <CardHeader>
          <div className="grid gap-0.5">
            <CardTitle>Database ID</CardTitle>
            <p className="text-sm text-muted-foreground">
              Pushes address the database by it, so renaming never breaks CI.
            </p>
          </div>
        </CardHeader>
        <CardContent>
          <CopyField value={database.id} label="Database ID" />
        </CardContent>
      </Card>
      <ApiTokens org={org} database={database} />
      <DeleteCard org={org} database={database} />
    </div>
  );
}

function GeneralCard({ org, database }: { org: string; database: Database }) {
  const update = useUpdateDatabase(org, database.slug);
  const navigate = useNavigate();
  const [name, setName] = React.useState(database.name);
  const [slug, setSlug] = React.useState(database.slug);
  const [description, setDescription] = React.useState(database.description);
  const [errors, setErrors] = React.useState<{
    name?: string;
    slug?: string;
    description?: string;
  }>({});
  const dirty =
    name.trim() !== database.name ||
    slug !== database.slug ||
    description.trim() !== database.description;

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    const problems = {
      name: name.trim() ? undefined : "Give the database a name.",
      slug: databaseSlugProblem(slug),
    };
    setErrors(problems);
    if (problems.name || problems.slug) return;
    update.mutate(
      {
        name: name.trim() !== database.name ? name.trim() : undefined,
        slug: slug !== database.slug ? slug : undefined,
        description: description.trim() !== database.description ? description.trim() : undefined,
      },
      {
        onSuccess: (saved) => {
          toast.success("Settings saved");
          if (saved.slug !== database.slug) {
            void navigate({
              to: "/$org/$db/settings",
              params: { org, db: saved.slug },
              replace: true,
            });
          }
        },
        onError: (error) => {
          if (error instanceof ApiError && error.code === "slug_taken") {
            setErrors({ slug: "Another database of this organization has that URL." });
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
    <Card>
      <form noValidate onSubmit={submit}>
        <CardHeader>
          <CardTitle>General</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-5">
          <Field label="Name" error={errors.name}>
            <Input
              value={name}
              maxLength={100}
              autoComplete="off"
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
              slug !== database.slug
                ? "Links to the old address will stop working. Pushes are not affected."
                : "Where the database lives in romulus."
            }
          >
            <Input
              prefix={`/${org}/`}
              value={slug}
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
          <Field label="Description" error={errors.description} description="Optional.">
            <Textarea
              rows={2}
              maxLength={1000}
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </Field>
        </CardContent>
        <div className="flex items-center justify-end gap-2 border-t border-border px-4 py-3">
          {dirty && (
            <Button
              onClick={() => {
                setName(database.name);
                setSlug(database.slug);
                setDescription(database.description);
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
      </form>
    </Card>
  );
}

function DeleteCard({ org, database }: { org: string; database: Database }) {
  const remove = useDeleteDatabase(org, database.slug);
  const navigate = useNavigate();
  const [open, setOpen] = React.useState(false);
  const versions = database.latest?.number ?? 0;
  return (
    <Card className="border-destructive/30">
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle>Delete this database</CardTitle>
          <p className="text-sm text-muted-foreground">
            It disappears from the organization with its history, and its API tokens stop working.
          </p>
        </div>
      </CardHeader>
      <CardContent>
        <Button variant="destructive" onClick={() => setOpen(true)}>
          Delete {database.name}
        </Button>
      </CardContent>
      <ConfirmDialog
        open={open}
        onOpenChange={setOpen}
        title={`Delete ${database.name}?`}
        description={
          versions > 0
            ? `${database.name} and its ${versions === 1 ? "version" : `${versions} versions`} disappear for everyone in the organization, and its API tokens stop working. You cannot undo this.`
            : `${database.name} disappears for everyone in the organization, and its API tokens stop working. You cannot undo this.`
        }
        confirmLabel="Delete database"
        destructive
        onConfirm={async () => {
          try {
            await remove.mutateAsync();
          } catch (error) {
            toast.error("Could not delete the database", {
              description: error instanceof ApiError ? error.message : "Check your connection.",
            });
            throw error;
          }
          toast.success(`${database.name} deleted`);
          void navigate({ to: "/$org", params: { org } });
        }}
      />
    </Card>
  );
}
