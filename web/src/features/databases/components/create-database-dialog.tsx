import * as React from "react";

import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field } from "@/components/ui/field";
import { Input, Textarea } from "@/components/ui/input";

import { useCreateDatabase } from "../mutations";
import type { Database } from "../queries";
import { databaseSlugProblem, slugify } from "../slug";

interface CreateDatabaseDialogProps {
  org: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (database: Database) => void;
}

type Errors = { name?: string; slug?: string; description?: string };

/**
 * Name, URL and description of a new database. The URL follows the name
 * until edited; fields are checked as typed only after the first submit.
 */
export function CreateDatabaseDialog({
  org,
  open,
  onOpenChange,
  onCreated,
}: CreateDatabaseDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>New database</DialogTitle>
          <DialogDescription>
            One schema and its versions. Upload its SQL or push it from CI next.
          </DialogDescription>
        </DialogHeader>
        {/* Remounted on open: a cancelled draft does not come back. */}
        {open && (
          <CreateDatabaseForm
            org={org}
            onCreated={onCreated}
            onCancel={() => onOpenChange(false)}
          />
        )}
      </DialogContent>
    </Dialog>
  );
}

function CreateDatabaseForm({
  org,
  onCreated,
  onCancel,
}: {
  org: string;
  onCreated: (database: Database) => void;
  onCancel: () => void;
}) {
  const create = useCreateDatabase(org);
  const [name, setName] = React.useState("");
  const [slug, setSlug] = React.useState("");
  const [description, setDescription] = React.useState("");
  const [slugEdited, setSlugEdited] = React.useState(false);
  const [submitted, setSubmitted] = React.useState(false);
  const [serverErrors, setServerErrors] = React.useState<Errors>({});
  const nameRef = React.useRef<HTMLInputElement>(null);
  const slugRef = React.useRef<HTMLInputElement>(null);

  const nameProblem = name.trim() ? undefined : "Give the database a name.";
  const errors: Errors = {
    name: serverErrors.name ?? (submitted ? nameProblem : undefined),
    slug: serverErrors.slug ?? (submitted ? databaseSlugProblem(slug) : undefined),
    description: serverErrors.description,
  };

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    setSubmitted(true);
    if (nameProblem) return nameRef.current?.focus();
    if (databaseSlugProblem(slug)) return slugRef.current?.focus();
    create.mutate(
      { name: name.trim(), slug, description: description.trim() },
      {
        onSuccess: onCreated,
        onError: (error) => {
          if (error instanceof ApiError && error.code === "slug_taken") {
            setServerErrors({ slug: "This organization already has a database at that URL." });
            slugRef.current?.focus();
          } else if (error instanceof ApiError && error.code === "validation") {
            setServerErrors(error.fields);
          } else {
            setServerErrors({
              name: error instanceof ApiError ? error.message : "Something went wrong. Try again.",
            });
          }
        },
      },
    );
  };

  return (
    <form noValidate onSubmit={submit} className="grid gap-4">
      <Field label="Name" error={errors.name}>
        <Input
          ref={nameRef}
          autoComplete="off"
          placeholder="Billing"
          maxLength={100}
          value={name}
          onChange={(e) => {
            setName(e.target.value);
            if (!slugEdited) setSlug(slugify(e.target.value));
            setServerErrors({});
          }}
        />
      </Field>
      <Field label="URL" error={errors.slug}>
        <Input
          ref={slugRef}
          prefix={`/${org}/`}
          autoComplete="off"
          spellCheck={false}
          autoCapitalize="none"
          placeholder="billing"
          maxLength={63}
          value={slug}
          className="font-mono"
          onChange={(e) => {
            setSlugEdited(e.target.value !== "");
            setSlug(
              e.target.value === ""
                ? slugify(name)
                : e.target.value.toLowerCase().replace(/\s+/g, "-"),
            );
            setServerErrors({});
          }}
        />
      </Field>
      <Field
        label="Description"
        description="Optional. What lives in it."
        error={errors.description}
      >
        <Textarea
          rows={2}
          maxLength={1000}
          placeholder="Invoices, plans and payments"
          value={description}
          onChange={(e) => setDescription(e.target.value)}
        />
      </Field>
      <DialogFooter>
        <Button onClick={onCancel}>Cancel</Button>
        <Button type="submit" variant="primary" loading={create.isPending}>
          Create database
        </Button>
      </DialogFooter>
    </form>
  );
}
