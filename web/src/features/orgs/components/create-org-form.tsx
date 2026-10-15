import * as React from "react";

import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";

import { useCreateOrg } from "../mutations";
import type { Org } from "../queries";
import { slugify, slugProblem } from "../slug";

interface CreateOrgFormProps {
  /** Shown before the slug, e.g. `romulus.example/app/`. */
  urlPrefix: string;
  onChange?: (draft: { name: string; slug: string }) => void;
  onCreated: (org: Org) => void;
}

/**
 * Name and URL of a new organization. The URL follows the name until the
 * user edits it; both are checked as typed only after the first submit, so
 * nobody is scolded for a field they have not finished.
 */
export function CreateOrgForm({ urlPrefix, onChange, onCreated }: CreateOrgFormProps) {
  const create = useCreateOrg();
  const [name, setName] = React.useState("");
  const [slug, setSlug] = React.useState("");
  const [slugEdited, setSlugEdited] = React.useState(false);
  const [submitted, setSubmitted] = React.useState(false);
  const [serverErrors, setServerErrors] = React.useState<{ name?: string; slug?: string }>({});
  const nameRef = React.useRef<HTMLInputElement>(null);
  const slugRef = React.useRef<HTMLInputElement>(null);

  // Naming the organization is the whole page: start there.
  React.useEffect(() => nameRef.current?.focus(), []);

  const nameProblem = name.trim() ? undefined : "Give the organization a name.";
  const errors = {
    name: serverErrors.name ?? (submitted ? nameProblem : undefined),
    slug: serverErrors.slug ?? (submitted ? slugProblem(slug) : undefined),
  };

  const update = (next: { name?: string; slug?: string }) => {
    const nextName = next.name ?? name;
    const nextSlug = next.slug ?? (slugEdited ? slug : slugify(nextName));
    setName(nextName);
    setSlug(nextSlug);
    setServerErrors({});
    onChange?.({ name: nextName, slug: nextSlug });
  };

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    setSubmitted(true);
    if (nameProblem) return nameRef.current?.focus();
    if (slugProblem(slug)) return slugRef.current?.focus();
    create.mutate(
      { name: name.trim(), slug },
      {
        onSuccess: onCreated,
        onError: (error) => {
          if (error instanceof ApiError && error.code === "slug_taken") {
            setServerErrors({ slug: "That URL is taken. Try another one." });
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
    <form noValidate onSubmit={submit} className="grid gap-5">
      <Field label="Organization name" error={errors.name}>
        <Input
          ref={nameRef}
          autoComplete="organization"
          placeholder="Acme Inc."
          maxLength={100}
          value={name}
          onChange={(e) => update({ name: e.target.value })}
        />
      </Field>
      <Field
        label="URL"
        error={errors.slug}
        description="Lowercase letters, digits and hyphens. You can change it later."
      >
        <Input
          ref={slugRef}
          prefix={urlPrefix}
          autoComplete="off"
          spellCheck={false}
          autoCapitalize="none"
          placeholder="acme"
          maxLength={63}
          value={slug}
          onChange={(e) => {
            // Emptied: follow the name again.
            setSlugEdited(e.target.value !== "");
            update({ slug: e.target.value.toLowerCase().replace(/\s+/g, "-") });
          }}
          className="font-mono"
        />
      </Field>
      <Button
        type="submit"
        variant="primary"
        size="lg"
        className="mt-1 w-full"
        loading={create.isPending}
      >
        Create organization
      </Button>
    </form>
  );
}
