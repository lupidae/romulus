import { Dialog as DialogPrimitive } from "radix-ui";
import type * as React from "react";

import { XmarkIcon } from "@/components/icons";
import { cn } from "@/lib/utils";

import { Button } from "./button";
import { usePortalContainer } from "./portal";

export const Dialog = DialogPrimitive.Root;
export const DialogTrigger = DialogPrimitive.Trigger;
export const DialogClose = DialogPrimitive.Close;

export function DialogOverlay({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Overlay>) {
  return (
    <DialogPrimitive.Overlay
      data-slot="dialog-overlay"
      className={cn(
        "fixed inset-0 z-(--z-overlay) bg-overlay",
        "data-[state=open]:animate-in data-[state=open]:fade-in-0",
        "data-[state=closed]:animate-out data-[state=closed]:fade-out-0",
        className,
      )}
      {...props}
    />
  );
}

/**
 * A modal. Always give it a DialogTitle (screen readers announce it) and keep
 * one primary action in the footer. Esc and the overlay close it; focus
 * returns to the trigger.
 *
 * Describe it with a DialogDescription, which becomes its accessible
 * description. Without one (the title says it all) the dialog references no
 * description at all; Radix only links one that is rendered.
 *
 * `fit`: a form that can grow (a notice, errors, a consumer's boxes) scrolls
 * inside once taller than the screen, so its footer stays reachable.
 */
export function DialogContent({
  className,
  children,
  hideClose = false,
  fit = false,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Content> & {
  hideClose?: boolean;
  fit?: boolean;
}) {
  return (
    <DialogPrimitive.Portal container={usePortalContainer()}>
      <DialogOverlay />
      <DialogPrimitive.Content
        data-slot="dialog-content"
        className={cn(
          "fixed top-[min(20vh,10rem)] left-1/2 z-(--z-overlay) grid w-[calc(100%-2rem)] max-w-md -translate-x-1/2 gap-4",
          "rounded-xl border border-border bg-popover p-5 text-popover-foreground shadow-dialog outline-none",
          "duration-(--duration-base) data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:slide-in-from-top-2 data-[state=open]:zoom-in-[0.97]",
          "data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-[0.97]",
          fit && "max-h-[calc(100dvh-min(20vh,10rem)-1rem)] overflow-y-auto",
          className,
        )}
        {...props}
      >
        {children}
        {!hideClose && (
          <DialogPrimitive.Close asChild>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Close"
              className="absolute top-3 right-3"
            >
              <XmarkIcon />
            </Button>
          </DialogPrimitive.Close>
        )}
      </DialogPrimitive.Content>
    </DialogPrimitive.Portal>
  );
}

export function DialogHeader({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("grid gap-1 pr-8", className)} {...props} />;
}

export function DialogFooter({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      className={cn("flex flex-col-reverse gap-2 pt-1 sm:flex-row sm:justify-end", className)}
      {...props}
    />
  );
}

export function DialogTitle({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Title>) {
  return (
    <DialogPrimitive.Title
      className={cn("text-lg font-semibold text-foreground", className)}
      {...props}
    />
  );
}

export function DialogDescription({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Description>) {
  return (
    <DialogPrimitive.Description
      className={cn("text-base text-muted-foreground", className)}
      {...props}
    />
  );
}
