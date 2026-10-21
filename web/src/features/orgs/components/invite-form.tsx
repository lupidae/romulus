import * as React from "react";

import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { PlanLimitNotice } from "@/features/billing/components/upgrade-prompt";
import { isPlanLimit } from "@/features/billing/plan-limit";
import { cn } from "@/lib/utils";

import { useInvite } from "../mutations";
import type { CreatedInvitation, Invitation, Role } from "../queries";
import { RoleSelect } from "./role-select";

interface InviteFormProps {
  slug: string;
  /** Pending invitations, to warn that inviting an address again replaces its link. */
  pending?: Invitation[];
  onCreated: (created: CreatedInvitation) => void;
  /** Laid out in a row (onboarding) rather than stacked (dialog). */
  inline?: boolean;
  /** Rendered next to the submit button (Cancel, in a dialog). */
  secondaryAction?: React.ReactNode;
}

const EMAIL = /^[^\s@]+@[^\s@]+$/;

/** An email and a role → a one-time invitation link. */
export function InviteForm({
  slug,
  pending,
  onCreated,
  inline = false,
  secondaryAction,
}: InviteFormProps) {
  const invite = useInvite(slug);
  const [email, setEmail] = React.useState("");
  const [role, setRole] = React.useState<Role>("developer");
  const [error, setError] = React.useState<string>();
  const [refused, setRefused] = React.useState<unknown>(null);
  const emailRef = React.useRef<HTMLInputElement>(null);

  const trimmed = email.trim();
  const replaces = pending?.find((i) => i.email.toLowerCase() === trimmed.toLowerCase());

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    if (!EMAIL.test(trimmed)) {
      setError("Enter an email address, like name@company.com.");
      emailRef.current?.focus();
      return;
    }
    setError(undefined);
    setRefused(null);
    invite.mutate(
      { email: trimmed, role },
      {
        onSuccess: (created) => {
          setEmail("");
          onCreated(created);
        },
        onError: (err) => {
          if (isPlanLimit(err)) return setRefused(err);
          setError(
            err instanceof ApiError ? (err.fields.email ?? err.message) : "Something went wrong.",
          );
          emailRef.current?.focus();
        },
      },
    );
  };

  return (
    <form
      noValidate
      onSubmit={submit}
      className={cn(
        "grid gap-4",
        inline && "sm:grid-cols-[minmax(0,1fr)_8rem_auto] sm:items-start sm:gap-2",
      )}
    >
      <Field
        label="Email"
        error={error}
        description={
          replaces
            ? `${replaces.email} already has a pending invitation: a new link replaces it.`
            : undefined
        }
      >
        <Input
          ref={emailRef}
          type="email"
          inputMode="email"
          autoComplete="off"
          spellCheck={false}
          placeholder="name@company.com"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
        />
      </Field>
      <Field label="Role">
        <RoleSelect value={role} onValueChange={setRole} className="w-full" />
      </Field>
      <PlanLimitNotice error={refused} org={slug} className={cn(inline && "sm:col-span-full")} />
      <div
        className={cn(
          "flex flex-col-reverse gap-2 sm:flex-row sm:justify-end",
          inline && "sm:pt-[1.375rem]",
        )}
      >
        {secondaryAction}
        <Button type="submit" variant="primary" loading={invite.isPending}>
          Create invite link
        </Button>
      </div>
    </form>
  );
}
