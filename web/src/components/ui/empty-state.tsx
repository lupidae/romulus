import type * as React from "react";

import { cn } from "@/lib/utils";

interface EmptyStateProps {
  /** One icon from @/components/icons. */
  icon?: React.ReactNode;
  /** What is missing, in a few words: “No databases yet”. */
  title: string;
  /** Why it matters or what happens next, one short line. */
  description?: React.ReactNode;
  /** The action that fills it, usually one primary Button. */
  action?: React.ReactNode;
  className?: string;
}

/** A place with nothing in it yet: says what belongs here and how to add it. */
export function EmptyState({ icon, title, description, action, className }: EmptyStateProps) {
  return (
    <div
      data-slot="empty-state"
      className={cn(
        "flex flex-col items-center justify-center gap-4 px-6 py-14 text-center",
        className,
      )}
    >
      {icon && (
        <div className="flex size-10 items-center justify-center rounded-lg border border-border bg-card text-muted-foreground shadow-xs [&_svg]:size-5">
          {icon}
        </div>
      )}
      <div className="grid max-w-sm gap-1">
        <h2 className="text-lg font-semibold text-foreground">{title}</h2>
        {description && <p className="text-base text-muted-foreground">{description}</p>}
      </div>
      {action && <div className="flex flex-wrap justify-center gap-2">{action}</div>}
    </div>
  );
}
