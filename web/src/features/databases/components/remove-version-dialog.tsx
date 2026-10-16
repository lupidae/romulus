import { useQueryClient } from "@tanstack/react-query";
import * as React from "react";

import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field } from "@/components/ui/field";
import { Textarea } from "@/components/ui/input";
import { toast } from "@/components/ui/toaster";
import { modKey } from "@/lib/platform";

import { restoreVersion, useRemoveVersion } from "../mutations";
import { type Database, neighbours } from "../queries";

interface RemoveVersionDialogProps {
  org: string;
  database: Database;
  number: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** It is removed: the page showing it should move on. */
  onRemoved: () => void;
}

/**
 * "Remove v3?": what removing hides and for how long it can come back, and
 * the reason the history keeps. Admins only (the API refuses others).
 */
export function RemoveVersionDialog(props: RemoveVersionDialogProps) {
  return (
    <Dialog open={props.open} onOpenChange={props.onOpenChange}>
      <DialogContent className="max-w-lg">{props.open && <RemoveForm {...props} />}</DialogContent>
    </Dialog>
  );
}

function RemoveForm({ org, database, number, onOpenChange, onRemoved }: RemoveVersionDialogProps) {
  const remove = useRemoveVersion(org, database.slug, database.id);
  const queryClient = useQueryClient();
  const [reason, setReason] = React.useState("");
  const [error, setError] = React.useState<string>();
  const latest = database.latest?.number === number;
  const previous = neighbours(database, number).older;
  const after = !latest
    ? ""
    : previous
      ? `, and v${previous} becomes the latest`
      : ", and the database has no version left";

  const submit = () => {
    if (remove.isPending) return;
    if (!reason.trim()) {
      setError("Say why: the database's activity keeps it, and whoever restores it reads it.");
      return;
    }
    remove.mutate(
      { number, reason: reason.trim() },
      {
        onSuccess: () => {
          onOpenChange(false);
          onRemoved();
          toast.success(`v${number} removed`, {
            description: "Restorable for 30 days from the database's settings.",
            duration: 10_000,
            action: {
              label: "Undo",
              onClick: () =>
                void restoreVersion(queryClient, org, database.slug, number).then(
                  () => toast.success(`v${number} restored`),
                  (failure: unknown) =>
                    toast.error(`Could not restore v${number}`, {
                      description:
                        failure instanceof ApiError ? failure.message : "Check your connection.",
                    }),
                ),
            },
          });
        },
        onError: (failure) => {
          if (failure instanceof ApiError && failure.code === "validation") {
            setError(failure.fields.reason ?? failure.message);
          } else {
            setError(failure instanceof ApiError ? failure.message : "Check your connection.");
          }
        },
      },
    );
  };

  return (
    <form
      noValidate
      className="grid gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <DialogHeader>
        <DialogTitle>Remove v{number}?</DialogTitle>
        <DialogDescription>
          v{number} is hidden from the history, diffs and imports{after}. Drafts on it will need
          “Update draft”. Restorable for 30 days, then deleted.
        </DialogDescription>
      </DialogHeader>
      <Field label="Reason" error={error}>
        <Textarea
          rows={3}
          maxLength={1000}
          value={reason}
          placeholder="Pushed from the staging database by mistake"
          onChange={(event) => {
            setReason(event.target.value);
            setError(undefined);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) submit();
          }}
        />
      </Field>
      <DialogFooter>
        <Button onClick={() => onOpenChange(false)}>Cancel</Button>
        <Button
          type="submit"
          variant="destructive"
          loading={remove.isPending}
          aria-keyshortcuts="Meta+Enter Control+Enter"
          title={`${modKey} Enter`}
        >
          Remove v{number}
        </Button>
      </DialogFooter>
    </form>
  );
}
