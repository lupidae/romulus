import type * as React from "react";

import { cn } from "@/lib/utils";

/**
 * Rows of the same kind of thing (members, invitations). Put it inside a
 * Card; hide secondary columns below `sm` with `hidden sm:table-cell` on both
 * the head and the cells, so a phone shows what matters.
 */
export function Table({ className, ...props }: React.ComponentProps<"table">) {
  return (
    <div className="w-full overflow-x-auto">
      <table
        data-slot="table"
        className={cn("w-full border-collapse text-left text-sm", className)}
        {...props}
      />
    </div>
  );
}

export function TableHeader({ className, ...props }: React.ComponentProps<"thead">) {
  return <thead className={cn("border-b border-border", className)} {...props} />;
}

export function TableBody({ className, ...props }: React.ComponentProps<"tbody">) {
  return <tbody className={cn("divide-y divide-border", className)} {...props} />;
}

export function TableRow({ className, ...props }: React.ComponentProps<"tr">) {
  return <tr data-slot="table-row" className={cn("transition-colors", className)} {...props} />;
}

export function TableHead({ className, ...props }: React.ComponentProps<"th">) {
  return (
    <th
      className={cn(
        "h-9 px-4 text-xs font-medium whitespace-nowrap text-muted-foreground",
        className,
      )}
      {...props}
    />
  );
}

export function TableCell({ className, ...props }: React.ComponentProps<"td">) {
  return <td className={cn("h-14 px-4 align-middle", className)} {...props} />;
}
