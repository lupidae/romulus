import { Tooltip as TooltipPrimitive } from "radix-ui";
import type * as React from "react";

import { cn } from "@/lib/utils";

import { KbdGroup } from "./kbd";

/** Mounted once in the root route; tooltips share one delay so moving between them is instant. */
export function TooltipProvider(props: React.ComponentProps<typeof TooltipPrimitive.Provider>) {
  return <TooltipPrimitive.Provider delayDuration={400} skipDelayDuration={300} {...props} />;
}

interface TooltipProps {
  /** Short text: a name or a hint, never the only place important information lives. */
  content: React.ReactNode;
  /** Keyboard shortcut for the action, e.g. [modKey, "K"]. */
  shortcut?: string[];
  side?: React.ComponentProps<typeof TooltipPrimitive.Content>["side"];
  /** One focusable element (a Button). Its accessible name must not depend on the tooltip. */
  children: React.ReactElement;
}

export function Tooltip({ content, shortcut, side = "bottom", children }: TooltipProps) {
  return (
    <TooltipPrimitive.Root>
      <TooltipPrimitive.Trigger asChild>{children}</TooltipPrimitive.Trigger>
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Content
          side={side}
          sideOffset={6}
          className={cn(
            "z-(--z-tooltip) flex items-center gap-2 rounded-md bg-foreground px-2 py-1 text-xs font-medium text-background shadow-popover",
            "origin-(--radix-tooltip-content-transform-origin) data-[state=delayed-open]:animate-in data-[state=delayed-open]:fade-in-0 data-[state=delayed-open]:zoom-in-[0.97]",
            "data-[state=closed]:animate-out data-[state=closed]:fade-out-0",
          )}
        >
          {content}
          {shortcut && (
            <KbdGroup
              keys={shortcut}
              className="[&_kbd]:border-background/20 [&_kbd]:bg-background/10 [&_kbd]:text-background [&_kbd]:shadow-none"
            />
          )}
        </TooltipPrimitive.Content>
      </TooltipPrimitive.Portal>
    </TooltipPrimitive.Root>
  );
}
