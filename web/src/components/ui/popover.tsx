import { Popover as PopoverPrimitive } from "radix-ui";
import type * as React from "react";

import { cn } from "@/lib/utils";

/**
 * Non-modal floating content opened by a button: a legend, a small form, a
 * detail. Esc and an outside click close it; focus moves in and returns to the
 * trigger. For a list of actions use Menu; for a name or a hint, Tooltip.
 */
export const Popover = PopoverPrimitive.Root;
export const PopoverTrigger = PopoverPrimitive.Trigger;
export const PopoverAnchor = PopoverPrimitive.Anchor;
export const PopoverClose = PopoverPrimitive.Close;

export function PopoverContent({
  className,
  align = "start",
  sideOffset = 6,
  ...props
}: React.ComponentProps<typeof PopoverPrimitive.Content>) {
  return (
    <PopoverPrimitive.Portal>
      <PopoverPrimitive.Content
        data-slot="popover-content"
        align={align}
        sideOffset={sideOffset}
        className={cn(
          "z-(--z-popover) w-72 rounded-lg border border-border bg-popover p-3 text-popover-foreground shadow-popover outline-none",
          "origin-(--radix-popover-content-transform-origin) data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-[0.97]",
          "data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-[0.97]",
          className,
        )}
        {...props}
      />
    </PopoverPrimitive.Portal>
  );
}
