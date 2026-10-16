import { Progress as ProgressPrimitive } from "radix-ui";
import type * as React from "react";

import { cn } from "@/lib/utils";

/**
 * How far a long task has come: a bar filling from the left. Give it an
 * `aria-label` (or `aria-labelledby`) naming the task; `value` of `max`
 * (100 by default). Without a value it is indeterminate.
 */
export function Progress({
  className,
  value,
  max = 100,
  ...props
}: React.ComponentProps<typeof ProgressPrimitive.Root>) {
  const share = value == null ? null : Math.min(Math.max(value / max, 0), 1);
  return (
    <ProgressPrimitive.Root
      data-slot="progress"
      value={value}
      max={max}
      className={cn("relative h-1.5 w-full overflow-hidden rounded-full bg-muted", className)}
      {...props}
    >
      <ProgressPrimitive.Indicator
        className={cn(
          "h-full rounded-full bg-primary transition-[width] duration-300 ease-out motion-reduce:transition-none",
          share === null && "w-1/3 animate-pulse",
        )}
        style={share === null ? undefined : { width: `${share * 100}%` }}
      />
    </ProgressPrimitive.Root>
  );
}
