import { Slot } from "radix-ui";
import type * as React from "react";

import { cn } from "@/lib/utils";

/**
 * Tabs that are pages: each one is a link with its own URL (`…/history`,
 * `…/settings`), the current one marked `aria-current="page"`. For views of
 * one thing that share a URL, use Tabs instead.
 */
export function TabNav({ className, ...props }: React.ComponentProps<"nav">) {
  return (
    <nav
      data-slot="tab-nav"
      className={cn(
        "-mb-px flex [scrollbar-width:none] items-center gap-4 overflow-x-auto",
        className,
      )}
      {...props}
    />
  );
}

/**
 * One tab: pass a router `<Link>` as the child (`asChild`); the link marks
 * itself current (TanStack Router sets `aria-current` and `data-status`).
 */
export function TabNavItem({
  className,
  asChild = false,
  ...props
}: React.ComponentProps<"a"> & { asChild?: boolean }) {
  const Comp = asChild ? Slot.Root : "a";
  return (
    <Comp
      data-slot="tab-nav-item"
      className={cn(
        "relative inline-flex h-10 shrink-0 items-center gap-1.5 text-sm font-medium whitespace-nowrap text-muted-foreground",
        "rounded-xs transition-colors outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
        "after:absolute after:inset-x-0 after:bottom-0 after:h-0.5 after:rounded-full after:bg-transparent",
        "aria-[current=page]:text-foreground aria-[current=page]:after:bg-primary",
        "[&_svg]:size-3.5",
        className,
      )}
      {...props}
    />
  );
}
