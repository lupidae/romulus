import { cva, type VariantProps } from "class-variance-authority";
import type * as React from "react";

import { cn } from "@/lib/utils";

export const badgeVariants = cva(
  "inline-flex h-5 shrink-0 items-center gap-1 rounded-sm px-1.5 text-xs font-medium whitespace-nowrap [&_svg]:size-3",
  {
    variants: {
      variant: {
        neutral: "bg-muted text-muted-foreground ring-1 ring-border ring-inset",
        primary: "bg-primary/10 text-primary ring-1 ring-primary/20 ring-inset",
        success: "bg-success/10 text-success ring-1 ring-success/20 ring-inset",
        warning: "bg-warning/10 text-warning ring-1 ring-warning/25 ring-inset",
        destructive: "bg-destructive/10 text-destructive ring-1 ring-destructive/20 ring-inset",
        info: "bg-info/10 text-info ring-1 ring-info/20 ring-inset",
        outline: "text-muted-foreground ring-1 ring-border-strong ring-inset",
      },
    },
    defaultVariants: { variant: "neutral" },
  },
);

/** Status, role and count pills. Status colors always carry a word, never color alone. */
export function Badge({
  className,
  variant,
  ...props
}: React.ComponentProps<"span"> & VariantProps<typeof badgeVariants>) {
  return <span data-slot="badge" className={cn(badgeVariants({ variant }), className)} {...props} />;
}
