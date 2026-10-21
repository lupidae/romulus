import type * as React from "react";

import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";

/**
 * A box to tick and what ticking it says, its error under it. Never ticked
 * in advance: what it says is the person's own statement.
 */
export function Consent({
  id,
  checked,
  onCheckedChange,
  error,
  invalid,
  children,
}: {
  id: string;
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  error?: string;
  /** Marked invalid without its own message (a group's message says why). */
  invalid?: boolean;
  children: React.ReactNode;
}) {
  const errorId = `${id}-error`;
  return (
    <div className="grid gap-1">
      <div className="flex items-start gap-2.5">
        <Checkbox
          id={id}
          checked={checked}
          onCheckedChange={(value) => onCheckedChange(value === true)}
          aria-invalid={error || invalid ? true : undefined}
          aria-describedby={error ? errorId : undefined}
          className="mt-0.5"
        />
        <Label htmlFor={id} className="leading-snug font-normal">
          {children}
        </Label>
      </div>
      {error && (
        <p id={errorId} role="alert" className="pl-6.5 text-sm text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}

/** A link to a legal page, opening in a new tab (the form keeps its state). */
export function DocumentLink({
  href,
  children,
  newTab,
}: {
  href: string;
  children: React.ReactNode;
  /** "(opens in a new tab)", in the page's language. */
  newTab: string;
}) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      className="font-medium text-primary underline-offset-4 hover:underline"
    >
      {children}
      <span className="sr-only"> {newTab}</span>
    </a>
  );
}
