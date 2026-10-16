import * as React from "react";

import { CheckCircleIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Checkbox } from "@/components/ui/checkbox";
import { Field } from "@/components/ui/field";
import { Textarea } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

import type { PublishPreview } from "../queries";
import { names } from "../review";

export type Approvals = PublishPreview["approvals"];

/**
 * The publish dialog's approvals (see `useApprovalGate`): who approved the
 * SQL previewed, what is missing, and for an admin, publishing without them.
 */
export function ApprovalGate({
  approvals,
  short,
  overriding,
  onOverridingChange,
  reason,
  onReasonChange,
}: {
  approvals: Approvals;
  short: boolean;
  overriding: boolean;
  onOverridingChange: (on: boolean) => void;
  reason: string;
  onReasonChange: (reason: string) => void;
}) {
  const id = React.useId();
  // The reason is what comes next once the box is ticked.
  const focusReason = React.useCallback((field: HTMLTextAreaElement | null) => field?.focus(), []);
  const { required, approved_by: by } = approvals;
  if (!short) {
    return (
      <p className="flex items-center gap-2 text-sm text-foreground" data-testid="approval-gate">
        <CheckCircleIcon aria-hidden className="size-4 shrink-0 text-success" />
        <span>
          Approved by {names(by)}{" "}
          <span className="text-muted-foreground">
            ({by.length} of {required})
          </span>
        </span>
      </p>
    );
  }
  const left = required - by.length;
  return (
    <div className="grid gap-3" data-testid="approval-gate">
      <Alert
        variant="warning"
        title={`Waiting for ${left} more approval${left === 1 ? "" : "s"} (${by.length} of ${required})`}
      >
        {by.length > 0 ? `Approved by ${names(by)}. ` : ""}Someone who did not edit the SQL approves
        it from the editor.
      </Alert>
      {approvals.can_override && (
        <div className="grid gap-3 rounded-lg border border-border p-3">
          <div className="flex items-start gap-2">
            <Checkbox
              id={id}
              checked={overriding}
              onCheckedChange={(on) => onOverridingChange(on === true)}
              className="mt-0.5"
            />
            <div className="grid gap-0.5">
              <Label htmlFor={id}>Publish without the approvals</Label>
              <p className="text-sm text-muted-foreground">
                As an admin you can. The database’s activity records who did it and why.
              </p>
            </div>
          </div>
          {overriding && (
            <Field label="Reason">
              <Textarea
                ref={focusReason}
                rows={2}
                maxLength={1000}
                value={reason}
                placeholder="Hotfix for the outage, reviewed on a call"
                onChange={(event) => onReasonChange(event.target.value)}
              />
            </Field>
          )}
        </div>
      )}
    </div>
  );
}
