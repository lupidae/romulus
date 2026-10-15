import { Link, useRouterState } from "@tanstack/react-router";
import * as React from "react";

import { LogoMark } from "@/components/brand/logo";
import { MenuIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetTitle, SheetTrigger } from "@/components/ui/sheet";

import { OrgSwitcher } from "./org-switcher";
import type { ShellOrg, ShellUser } from "./types";
import { UserMenu } from "./user-menu";

interface AppShellProps {
  user: ShellUser;
  org: ShellOrg;
  orgs: ShellOrg[];
  /** The left navigation: a <SidebarNav> with typed <Link>s, supplied by the route. */
  nav: React.ReactNode;
  /** Context after the org in the top bar, e.g. the current database. */
  breadcrumb?: React.ReactNode;
  onSwitchOrg: (org: ShellOrg) => void;
  onCreateOrg: () => void;
  onSignOut: () => void;
  children: React.ReactNode;
}

/**
 * The signed-in frame: a top bar (org switcher, user menu), the left
 * navigation, and the page in a panel. Below `md` the navigation moves into
 * a drawer opened from the top bar. Pages render inside the panel and scroll
 * there, so the bars never move.
 */
export function AppShell({
  user,
  org,
  orgs,
  nav,
  breadcrumb,
  onSwitchOrg,
  onCreateOrg,
  onSignOut,
  children,
}: AppShellProps) {
  const [drawerOpen, setDrawerOpen] = React.useState(false);
  const href = useRouterState({ select: (s) => s.location.href });

  // A navigation from inside the drawer closes it (adjusting state during
  // render, not in an effect, so the drawer never paints open on the new page).
  const [drawerHref, setDrawerHref] = React.useState(href);
  if (drawerHref !== href) {
    setDrawerHref(href);
    setDrawerOpen(false);
  }

  return (
    <div className="flex h-dvh flex-col bg-background">
      <a
        href="#main"
        className="sr-only z-(--z-popover) rounded-md bg-card px-3 py-2 text-sm font-medium shadow-popover focus:not-sr-only focus:fixed focus:top-2 focus:left-2"
      >
        Skip to content
      </a>

      <header className="flex h-12 shrink-0 items-center justify-between gap-4 px-2 md:px-3">
        <div className="flex min-w-0 items-center gap-1.5">
          <Sheet open={drawerOpen} onOpenChange={setDrawerOpen}>
            <SheetTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                aria-label="Open navigation"
                className="md:hidden"
              >
                <MenuIcon />
              </Button>
            </SheetTrigger>
            <SheetContent>
              <SheetTitle className="sr-only">Navigation</SheetTitle>
              <div className="flex h-12 items-center gap-2 px-4">
                <LogoMark />
                <span className="truncate text-sm font-semibold text-foreground">{org.name}</span>
              </div>
              <div className="flex-1 overflow-y-auto px-2 py-2">{nav}</div>
            </SheetContent>
          </Sheet>

          <Link
            to="/"
            aria-label="romulus home"
            className="hidden size-8 items-center justify-center rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring md:flex"
          >
            <LogoMark />
          </Link>
          <Slash />
          <OrgSwitcher current={org} orgs={orgs} onSelect={onSwitchOrg} onCreate={onCreateOrg} />
          {breadcrumb && (
            <>
              <Slash />
              <div className="hidden min-w-0 items-center gap-1.5 md:flex">{breadcrumb}</div>
            </>
          )}
        </div>

        <div className="flex shrink-0 items-center gap-1.5">
          <UserMenu user={user} onSignOut={onSignOut} />
        </div>
      </header>

      <div className="flex min-h-0 flex-1">
        <aside
          aria-label="Primary"
          className="hidden w-56 shrink-0 overflow-y-auto px-2 py-2 md:block"
        >
          {nav}
        </aside>
        <main
          id="main"
          tabIndex={-1}
          className="min-w-0 flex-1 overflow-y-auto border-t border-border bg-card outline-none md:mr-2 md:mb-2 md:rounded-xl md:border md:shadow-xs"
        >
          {children}
        </main>
      </div>
    </div>
  );
}

/** The separator between levels of the top bar (logo / org / database). */
function Slash() {
  return <span aria-hidden className="hidden h-5 w-px rotate-12 bg-border-strong md:block" />;
}

/**
 * Padding and measure for a page inside the shell. Every page uses it, so all
 * pages share one left edge; a page never centers itself or adds its own
 * outer padding.
 */
export function PageBody({ children }: { children: React.ReactNode }) {
  return <div className="grid w-full max-w-5xl gap-6 px-4 py-6 md:px-8 md:py-8">{children}</div>;
}
