import { AlertDialog as AlertDialogPrimitive } from "radix-ui";
import * as React from "react";

import { cn } from "@/lib/utils";

import { Button } from "./button";
import { usePortalContainer } from "./portal";

interface ConfirmDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** A question naming the thing: “Remove Bob Durand from Acme?” */
  title: React.ReactNode;
  /** What happens, and whether it can be undone. */
  description: React.ReactNode;
  /** The verb, never “OK” or “Yes”: “Remove member”. */
  confirmLabel: string;
  cancelLabel?: string;
  /** Red confirm button, for actions that lose access or data. */
  destructive?: boolean;
  /**
   * Runs on confirm. The button shows a spinner while it runs; the dialog
   * closes when it resolves and stays open (for another try) when it throws.
   */
  onConfirm: () => unknown;
}

/**
 * Asks before an action that is hard to undo. An alert dialog: focus starts
 * on Cancel, so Enter never confirms by accident; Esc cancels.
 */
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  cancelLabel = "Cancel",
  destructive = false,
  onConfirm,
}: ConfirmDialogProps) {
  const [busy, setBusy] = React.useState(false);

  const confirm = async () => {
    setBusy(true);
    try {
      await onConfirm();
      onOpenChange(false);
    } catch {
      // The caller reported the failure; keep the dialog for another try.
    } finally {
      setBusy(false);
    }
  };

  return (
    <AlertDialogPrimitive.Root open={open} onOpenChange={(next) => !busy && onOpenChange(next)}>
      <AlertDialogPrimitive.Portal container={usePortalContainer()}>
        <AlertDialogPrimitive.Overlay
          className={cn(
            "fixed inset-0 z-(--z-overlay) bg-overlay",
            "data-[state=open]:animate-in data-[state=open]:fade-in-0",
            "data-[state=closed]:animate-out data-[state=closed]:fade-out-0",
          )}
        />
        <AlertDialogPrimitive.Content
          data-slot="confirm-dialog"
          className={cn(
            "fixed top-[min(20vh,10rem)] left-1/2 z-(--z-overlay) grid w-[calc(100%-2rem)] max-w-md -translate-x-1/2 gap-4",
            "rounded-xl border border-border bg-popover p-5 text-popover-foreground shadow-dialog outline-none",
            "duration-(--duration-base) data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-[0.97]",
            "data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-[0.97]",
          )}
        >
          <div className="grid gap-1">
            <AlertDialogPrimitive.Title className="text-lg font-semibold text-foreground">
              {title}
            </AlertDialogPrimitive.Title>
            <AlertDialogPrimitive.Description className="text-base text-muted-foreground">
              {description}
            </AlertDialogPrimitive.Description>
          </div>
          <div className="flex flex-col-reverse gap-2 pt-1 sm:flex-row sm:justify-end">
            <AlertDialogPrimitive.Cancel asChild>
              <Button disabled={busy}>{cancelLabel}</Button>
            </AlertDialogPrimitive.Cancel>
            <Button
              variant={destructive ? "destructive" : "primary"}
              loading={busy}
              onClick={() => void confirm()}
            >
              {confirmLabel}
            </Button>
          </div>
        </AlertDialogPrimitive.Content>
      </AlertDialogPrimitive.Portal>
    </AlertDialogPrimitive.Root>
  );
}
