import { Slot } from "radix-ui";
import * as React from "react";

import { cn } from "@/lib/utils";

import { Tooltip } from "./tooltip";

const RailContext = React.createContext(false);

/**
 * Primary navigation. Compose: <SidebarNav> › <SidebarSection> ›
 * <SidebarItem asChild icon={…} label="…"><Link …/></SidebarItem>.
 * TanStack Router's <Link> marks the current page with data-status="active"
 * and aria-current="page"; the item styles read that, so nothing else tracks
 * which item is active.
 *
 * Inside a <SidebarRail> it draws as the collapsed sidebar: icons only, each
 * named by a tooltip. The app shell renders the same nav in both forms, so
 * the routes that build it never know which one shows.
 */
export function SidebarNav({ className, ...props }: React.ComponentProps<"nav">) {
  const rail = useSidebarRail();
  return (
    <nav
      data-slot="sidebar"
      data-rail={rail ? "" : undefined}
      className={cn("flex flex-col text-sidebar-foreground", rail ? "gap-2" : "gap-4", className)}
      {...props}
    />
  );
}

/** Draws the navigation inside as the collapsed rail (or not, `rail={false}`). */
export function SidebarRail({
  rail = true,
  children,
}: {
  rail?: boolean;
  children: React.ReactNode;
}) {
  return <RailContext.Provider value={rail}>{children}</RailContext.Provider>;
}

/** Whether the nav around is the collapsed rail: for parts that change with it. */
export function useSidebarRail() {
  return React.useContext(RailContext);
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
  const rail = useSidebarRail();
  return (
    <div className={cn("grid gap-0.5", className)}>
      {label &&
        (rail ? (
          // The rail has no room for a heading: a short rule marks the section.
          <span aria-hidden className="mx-auto mb-1.5 h-px w-4 bg-border-strong" />
        ) : (
          <p className="truncate px-2 pb-1 text-xs font-medium whitespace-nowrap text-faint-foreground">
            {label}
          </p>
        ))}
      <ul aria-label={label} className="grid gap-px">
        {children}
      </ul>
    </div>
  );
}

interface SidebarItemProps extends Omit<React.ComponentProps<"a">, "children"> {
  asChild?: boolean;
  icon: React.ReactNode;
  /** What the item says when the sidebar is open. */
  label: string;
  /**
   * Its name on the rail, where the section's heading is gone (the
   * organization's "General" settings say "Settings"). Defaults to `label`.
   */
  railLabel?: string;
  /** With `asChild`: the link, without content (the item draws icon and label in it). */
  children?: React.ReactElement;
}

export function SidebarItem({
  asChild = false,
  icon,
  label,
  railLabel,
  className,
  children,
  ...props
}: SidebarItemProps) {
  const rail = useSidebarRail();
  const name = rail ? (railLabel ?? label) : label;
  const Comp = asChild ? Slot.Root : "a";
  const item = (
    <Comp
      data-slot="sidebar-item"
      className={cn(
        "flex h-8 items-center gap-2.5 rounded-md text-sm font-medium whitespace-nowrap outline-none pointer-coarse:h-11",
        rail ? "mx-auto w-8 justify-center pointer-coarse:w-11" : "px-2",
        "transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
        "focus-visible:ring-2 focus-visible:ring-ring",
        "data-[status=active]:bg-sidebar-accent data-[status=active]:text-sidebar-accent-foreground",
        "[&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:text-muted-foreground data-[status=active]:[&_svg]:text-foreground",
        className,
      )}
      {...props}
    >
      {icon}
      <Slot.Slottable>{children}</Slot.Slottable>
      <span className={rail ? "sr-only" : "min-w-0 truncate"}>{name}</span>
    </Comp>
  );
  return (
    <li>
      {rail ? (
        <Tooltip content={name} side="right">
          {item}
        </Tooltip>
      ) : (
        item
      )}
    </li>
  );
}
