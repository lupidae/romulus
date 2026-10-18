// How playground results read: durations, lock modes, a run's verdict.

import type { MigrationReport } from "./queries";

/** `AccessExclusiveLock` → `ACCESS EXCLUSIVE`, as the documentation names locks. */
export function lockName(mode: string): string {
  return mode
    .replace(/Lock$/, "")
    .replace(/([a-z])([A-Z])/g, "$1 $2")
    .toUpperCase();
}

/** Short for a duration in ms: "4 ms", "1.2 s", "2 min 5 s". */
export function duration(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)} s`;
  const minutes = Math.floor(ms / 60_000);
  return `${minutes} min ${Math.round((ms % 60_000) / 1000)} s`;
}

/** What a run did, in one sentence. */
export function verdict(report: MigrationReport): string {
  const total = report.statements.length;
  if (report.passed)
    return `Passed: ${total} statement${total === 1 ? "" : "s"} in ${duration(report.duration_ms)}`;
  return `Failed at statement ${(report.failed_at ?? 0) + 1} of ${total}`;
}

/** How a statement's hazard reads, and its badge. */
export const HAZARD: Record<
  string,
  { label: string; variant: "destructive" | "warning" | "info" }
> = {
  deletes_data: { label: "Deletes data", variant: "destructive" },
  can_fail: { label: "Can fail", variant: "warning" },
  widens_access: { label: "Widens access", variant: "info" },
};
