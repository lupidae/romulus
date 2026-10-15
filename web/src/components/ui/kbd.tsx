import type * as React from "react";

import { cn } from "@/lib/utils";

/** One key of a keyboard shortcut. Use `modKey` from @/lib/platform for ⌘/Ctrl. */
export function Kbd({ className, ...props }: React.ComponentProps<"kbd">) {
  return (
    <kbd
      data-slot="kbd"
      className={cn(
        "inline-flex h-5 min-w-5 items-center justify-center rounded-xs border border-border bg-muted px-1",
        "font-sans text-xs font-medium text-muted-foreground shadow-[inset_0_-0.0625rem_0_var(--color-border)]",
        className,
      )}
      {...props}
    />
  );
}

/** A shortcut such as ⌘ K: keys sit side by side, read as one combination. */
export function KbdGroup({ keys, className }: { keys: string[]; className?: string }) {
  return (
    <span className={cn("inline-flex items-center gap-0.5", className)}>
      {keys.map((key) => (
        <Kbd key={key}>{key}</Kbd>
      ))}
    </span>
  );
}
