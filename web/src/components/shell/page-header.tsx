import type * as React from "react";

import { cn } from "@/lib/utils";

interface PageHeaderProps {
  title: React.ReactNode;
  /** One supporting line, about 15 words at most. */
  description?: React.ReactNode;
  /** The page's primary action (one primary Button) and at most a few secondary ones. */
  actions?: React.ReactNode;
  className?: string;
}

/** The top of every page in the shell: one h1 at app scale, its line, its action. */
export function PageHeader({ title, description, actions, className }: PageHeaderProps) {
  return (
    <header
      className={cn(
        "flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between sm:gap-6",
        className,
      )}
    >
      <div className="grid min-w-0 gap-1">
        <h1 className="text-xl font-semibold text-foreground">{title}</h1>
        {description && <p className="text-base text-muted-foreground">{description}</p>}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </header>
  );
}
