import { Slot } from "radix-ui";
import type * as React from "react";

import { cn } from "@/lib/utils";

/**
 * Primary navigation. Compose: <SidebarNav> › <SidebarSection> › <SidebarItem asChild><Link …/></SidebarItem>.
 * TanStack Router's <Link> marks the current page with data-status="active"
 * and aria-current="page"; the item styles read that, so nothing else tracks
 * which item is active.
 */
export function SidebarNav({ className, ...props }: React.ComponentProps<"nav">) {
  return (
    <nav
      data-slot="sidebar"
      className={cn("flex flex-col gap-4 text-sidebar-foreground", className)}
      {...props}
    />
  );
}

export function SidebarSection({
  label,
  className,
  children,
}: {
  label?: string;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <div className={cn("grid gap-0.5", className)}>
      {label && <p className="px-2 pb-1 text-xs font-medium text-faint-foreground">{label}</p>}
      <ul className="grid gap-px">{children}</ul>
    </div>
  );
}

export function SidebarItem({
  asChild = false,
  className,
  ...props
}: React.ComponentProps<"a"> & { asChild?: boolean }) {
  const Comp = asChild ? Slot.Root : "a";
  return (
    <li>
      <Comp
        data-slot="sidebar-item"
        className={cn(
          "flex h-8 items-center gap-2.5 rounded-md px-2 text-sm font-medium outline-none",
          "transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
          "focus-visible:ring-2 focus-visible:ring-ring",
          "data-[status=active]:bg-sidebar-accent data-[status=active]:text-sidebar-accent-foreground",
          "[&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:text-muted-foreground data-[status=active]:[&_svg]:text-foreground",
          className,
        )}
        {...props}
      />
    </li>
  );
}
