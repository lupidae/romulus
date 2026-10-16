import * as React from "react";

import { ApprovalGate, type Approvals } from "./components/approval-gate";

/**
 * The publish dialog's approvals: whether the SQL previewed has the
 * approvals the organization requires, and for an admin, publishing
 * without them with a reason (the database's activity keeps it).
 */
export function useApprovalGate(approvals: Approvals | undefined) {
  const [overriding, setOverriding] = React.useState(false);
  const [reason, setReason] = React.useState("");
  const required = approvals?.required ?? 0;
  const count = approvals?.approved_by.length ?? 0;
  const short = required > 0 && count < required;
  const overridden = short && !!approvals?.can_override && overriding && reason.trim() !== "";
  return {
    /** Publishing waits: not enough approvals, and no override with a reason. */
    blocked: short && !overridden,
    /** Sent with the publish when the admin publishes without them. */
    overrideReason: overridden ? reason.trim() : undefined,
    element:
      required === 0 || !approvals ? null : (
        <ApprovalGate
          approvals={approvals}
          short={short}
          overriding={overriding}
          onOverridingChange={setOverriding}
          reason={reason}
          onReasonChange={setReason}
        />
      ),
  };
}
