import * as React from "react";

import { ApiError } from "@/api/client";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

import { useCreateAccessToken } from "../mutations";
import { scopeDescription, scopeLabel } from "../labels";
import type { AccessTokenScope, CreatedAccessToken } from "../queries";

const expiries = [
  { value: "30", label: "30 days" },
  { value: "90", label: "90 days" },
  { value: "365", label: "1 year" },
  { value: "never", label: "Never" },
] as const;

type Expiry = (typeof expiries)[number]["value"];

/**
 * The fields of a new access token and its creation. `children` renders the
 * buttons (in a dialog's footer, or under a step), `submitting` says when to
 * show the primary one loading.
 */
export function TokenForm({
  defaultName = "",
  onCreated,
  children,
}: {
  defaultName?: string;
  onCreated: (created: CreatedAccessToken) => void;
  children: (submitting: boolean) => React.ReactNode;
}) {
  const create = useCreateAccessToken();
  const id = React.useId();
  const [name, setName] = React.useState(defaultName);
  const [scope, setScope] = React.useState<AccessTokenScope>("write");
  const [expiry, setExpiry] = React.useState<Expiry>("90");
  const [error, setError] = React.useState<string>();
  const [failed, setFailed] = React.useState<string>();

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    setFailed(undefined);
    if (!name.trim()) return setError("Name it after the agent: Claude Code on my laptop.");
    create.mutate(
      {
        name: name.trim(),
        scope,
        expires_in_days: expiry === "never" ? null : Number(expiry),
      },
      {
        onSuccess: onCreated,
        onError: (err) => {
          if (err instanceof ApiError && err.fields.name) setError(err.fields.name);
          else
            setFailed(
              err instanceof ApiError ? err.message : "Could not create the token. Try again.",
            );
        },
      },
    );
  };

  return (
    <form noValidate onSubmit={submit} className="grid gap-4" aria-describedby={`${id}-failed`}>
      <Field label="Name" error={error} description="Which agent, on which computer.">
        <Input
          placeholder="Claude Code on my laptop"
          maxLength={100}
          autoComplete="off"
          value={name}
          onChange={(e) => {
            setName(e.target.value);
            setError(undefined);
          }}
        />
      </Field>
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Access">
          <Choice
            value={scope}
            onValueChange={setScope}
            options={(["write", "read"] as const).map((s) => ({
              value: s,
              label: scopeLabel[s],
              description: scopeDescription[s],
            }))}
          />
        </Field>
        <Field label="Expires after">
          <Choice value={expiry} onValueChange={setExpiry} options={expiries} />
        </Field>
      </div>
      {failed && (
        <p id={`${id}-failed`} role="alert" className="text-sm text-destructive">
          {failed}
        </p>
      )}
      {children(create.isPending)}
    </form>
  );
}

/**
 * One of a few options, as a select. Inside a `Field`, the kit's `Select`
 * hands the field's id and aria attributes to its trigger, so the label
 * names it.
 */
function Choice<T extends string>({
  value,
  onValueChange,
  options,
  ...field
}: {
  value: T;
  onValueChange: (value: T) => void;
  options: readonly { value: T; label: string; description?: string }[];
} & Pick<React.ComponentProps<typeof Select>, "id" | "aria-describedby" | "aria-invalid">) {
  return (
    <Select value={value} onValueChange={(next) => onValueChange(next as T)} {...field}>
      <SelectTrigger>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {options.map((option) => (
          <SelectItem key={option.value} value={option.value} description={option.description}>
            {option.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
