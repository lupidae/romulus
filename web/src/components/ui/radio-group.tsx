import { RadioGroup as RadioGroupPrimitive } from "radix-ui";
import type * as React from "react";

import { cn } from "@/lib/utils";

/**
 * One choice among a few, each worth a sentence (a size, a profile): cards
 * with a label and what it means. Arrow keys move between them. Use a
 * ToggleGroup for short labels, a Select for many options.
 */
export function RadioGroup({
  className,
  ...props
}: React.ComponentProps<typeof RadioGroupPrimitive.Root>) {
  return (
    <RadioGroupPrimitive.Root
      data-slot="radio-group"
      className={cn("grid gap-2", className)}
      {...props}
    />
  );
}

interface RadioCardProps extends Omit<
  React.ComponentProps<typeof RadioGroupPrimitive.Item>,
  "children"
> {
  label: React.ReactNode;
  description?: React.ReactNode;
}

export function RadioCard({ label, description, className, ...props }: RadioCardProps) {
  return (
    <RadioGroupPrimitive.Item
      data-slot="radio-card"
      className={cn(
        "group flex cursor-pointer items-start gap-2.5 rounded-lg border border-border bg-card px-3 py-2.5 text-left transition-colors outline-none",
        "hover:border-border-strong focus-visible:ring-2 focus-visible:ring-ring",
        "data-[state=checked]:border-primary data-[state=checked]:bg-primary/5",
        "disabled:cursor-not-allowed disabled:opacity-50",
        className,
      )}
      {...props}
    >
      <span
        aria-hidden
        className="mt-0.5 flex size-4 shrink-0 items-center justify-center rounded-full border border-border-strong group-data-[state=checked]:border-primary"
      >
        <RadioGroupPrimitive.Indicator className="size-2 rounded-full bg-primary" />
      </span>
      <span className="grid gap-0.5">
        <span className="text-sm font-medium text-foreground">{label}</span>
        {description && <span className="text-sm text-muted-foreground">{description}</span>}
      </span>
    </RadioGroupPrimitive.Item>
  );
}
