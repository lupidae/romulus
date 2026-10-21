import * as React from "react";

import { type PlanLimit, planLimitOf } from "./plan-limit";

// The one upgrade dialog (UpgradeDialogHost, in the root route): any screen
// whose action the plan refused opens it with `showUpgrade(error)`.

let shown: PlanLimit | null = null;
const listeners = new Set<() => void>();

function set(next: PlanLimit | null) {
  shown = next;
  for (const listener of listeners) listener();
}

/**
 * Opens the upgrade dialog when `error` is a 402 `plan_limit`, and says so:
 * `if (showUpgrade(error)) return;` before a call site's other error handling.
 */
export function showUpgrade(error: unknown): boolean {
  const limit = planLimitOf(error);
  if (limit) set(limit);
  return limit !== null;
}

export function closeUpgrade() {
  set(null);
}

/** The refusal the dialog shows now, if any. */
export function useShownUpgrade(): PlanLimit | null {
  return React.useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => shown,
    () => null,
  );
}
