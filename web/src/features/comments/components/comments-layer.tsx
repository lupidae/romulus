import * as React from "react";

import { ChatLinesIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { Tooltip } from "@/components/ui/tooltip";
import { useMediaQuery } from "@/features/schema/viewer/use-preference";
import { cn } from "@/lib/utils";

import type { CommentsUi } from "../use-comments-ui";

/** The bar's button for the comments panel: how many threads are open, and unread. */
export function CommentsToggle({
  ui,
  open,
  unread,
  label = "Comments",
}: {
  ui: CommentsUi;
  /** Open threads. */
  open: number;
  unread: number;
  label?: string;
}) {
  const name =
    unread > 0 ? `${label}: ${unread} unread` : open > 0 ? `${label}: ${open} open` : label;
  return (
    <Tooltip content={ui.open ? `Hide ${label.toLowerCase()}` : `Show ${label.toLowerCase()}`}>
      <Button
        variant="ghost"
        size="sm"
        aria-label={name}
        aria-pressed={ui.open}
        onClick={() => (ui.open ? ui.close() : ui.setOpen(true))}
        className={cn("relative gap-1.5 px-2", ui.open && "bg-accent text-accent-foreground")}
      >
        <ChatLinesIcon />
        <span aria-hidden className="max-md:hidden">
          {label}
        </span>
        {open > 0 && (
          <span
            aria-hidden
            className={cn(
              "min-w-4 rounded-full px-1 text-xs leading-4 font-semibold tabular-nums",
              unread > 0 ? "bg-primary text-primary-foreground" : "bg-muted text-muted-foreground",
            )}
          >
            {unread > 0 ? unread : open}
          </span>
        )}
      </Button>
    </Tooltip>
  );
}

/**
 * Where the panel goes: beside the viewer on wide screens, over it from the
 * right on a phone.
 */
export function CommentsAside({
  ui,
  title,
  children,
}: {
  ui: CommentsUi;
  title: string;
  children: React.ReactNode;
}) {
  const wide = useMediaQuery("(min-width: 48rem)");
  if (wide) {
    return ui.open ? (
      <aside
        aria-label={title}
        className="flex w-80 shrink-0 flex-col border-l border-border xl:w-88"
      >
        {children}
      </aside>
    ) : null;
  }
  return (
    <Sheet open={ui.open} onOpenChange={(open) => !open && ui.close()}>
      <SheetContent
        side="right"
        className="w-full max-w-[24rem] bg-card p-0"
        // The panel, not its first button (whose tooltip would open): Tab goes on from there.
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          (event.currentTarget as HTMLElement | null)?.focus();
        }}
      >
        <SheetTitle className="sr-only">{title}</SheetTitle>
        {children}
      </SheetContent>
    </Sheet>
  );
}
