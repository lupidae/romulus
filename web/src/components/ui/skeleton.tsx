import type * as React from "react";

import { cn } from "@/lib/utils";

/**
 * A placeholder with the exact size of what is loading, so nothing jumps when
 * the data arrives. Give it the real element's height and width classes.
 */
export function Skeleton({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="skeleton"
      aria-hidden
      className={cn(
        "animate-shimmer rounded-md bg-[linear-gradient(90deg,var(--color-accent)_0%,var(--color-border-strong)_50%,var(--color-accent)_100%)] bg-size-[200%_100%]",
        className,
      )}
      {...props}
    />
  );
}
