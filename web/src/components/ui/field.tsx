import { Slot } from "radix-ui";
import * as React from "react";

import { cn } from "@/lib/utils";

import { Label } from "./label";

interface FieldProps {
  label: React.ReactNode;
  /** One short line under the label, when the label alone is not enough. */
  description?: React.ReactNode;
  /** Replaces the description and marks the control invalid. Say how to fix it. */
  error?: React.ReactNode;
  className?: string;
  /** Exactly one control: Input, Textarea, a Select trigger… It receives id and aria-*. */
  children: React.ReactElement;
}

/**
 * A labelled form control. Wires the label, description and error to the
 * control (id, aria-describedby, aria-invalid) so screen readers announce them.
 */
export function Field({ label, description, error, className, children }: FieldProps) {
  const id = React.useId();
  const hintId = `${id}-hint`;
  const hint = error ?? description;
  return (
    <div data-slot="field" className={cn("grid gap-1.5", className)}>
      <Label htmlFor={id}>{label}</Label>
      <Slot.Root
        id={id}
        aria-describedby={hint ? hintId : undefined}
        aria-invalid={error ? true : undefined}
      >
        {children}
      </Slot.Root>
      {hint && (
        <p
          id={hintId}
          className={cn("text-sm", error ? "text-destructive" : "text-muted-foreground")}
        >
          {hint}
        </p>
      )}
    </div>
  );
}
