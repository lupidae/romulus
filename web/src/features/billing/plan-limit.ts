import { ApiError } from "@/api/client";

import { count, historyLabel, planNames, sizeNames } from "./format";
import type { Cap, Plan, PlanLimitDetails } from "./queries";

/** A refusal by the organization's plan (402 `plan_limit`): what the API said, and why. */
export interface PlanLimit extends PlanLimitDetails {
  /** The API's sentence: what was refused, the cap, the plan that lifts it. */
  message: string;
}

const CAPS: readonly Cap[] = [
  "members",
  "databases",
  "versions",
  "schema_size",
  "history",
  "playground_size",
  "playground_hours",
  "approvals",
  "approvals_override",
];
const PLANS: readonly Plan[] = ["free", "starter", "team", "business"];

/** The plan limit an error is, or `null` for any other error. */
export function planLimitOf(error: unknown): PlanLimit | null {
  if (!(error instanceof ApiError) || error.code !== "plan_limit") return null;
  const details = (error.details ?? {}) as Partial<Record<keyof PlanLimitDetails, unknown>>;
  const cap = CAPS.find((c) => c === details.cap);
  const plan = PLANS.find((p) => p === details.plan);
  if (!cap || !plan) return null;
  const number = (value: unknown) => (typeof value === "number" ? value : null);
  return {
    cap,
    plan,
    limit: number(details.limit),
    usage: number(details.usage),
    upgrade_to: PLANS.find((p) => p === details.upgrade_to) ?? null,
    message: error.message,
  };
}

export const isPlanLimit = (error: unknown): boolean => planLimitOf(error) !== null;

/** What was refused, as a title: "Database limit reached". */
export const capTitles: Record<Cap, string> = {
  members: "Member limit reached",
  databases: "Database limit reached",
  versions: "Version limit reached",
  schema_size: "Schema too large for this plan",
  history: "Older than this plan's history",
  playground_size: "Playground size not in this plan",
  playground_hours: "Playground hours used for this month",
  approvals: "More approvals than this plan allows",
  approvals_override: "Override not in this plan",
};

/** The cap in a few words: "Free keeps 1 database". */
export function capSummary(limit: PlanLimit): string {
  const plan = planNames[limit.plan];
  const n = limit.limit ?? 0;
  switch (limit.cap) {
    case "members":
      return `${plan} has ${count(n, "member", "members")}`;
    case "databases":
      return `${plan} keeps ${count(n, "database", "databases")}`;
    case "versions":
      return `${plan} keeps ${count(n, "version", "versions")} per database`;
    case "schema_size":
      return `${plan} keeps ${n.toLocaleString("en-US")} tables and views per schema`;
    case "history":
      return `${plan} shows ${historyLabel(n)} of history`;
    case "playground_size":
      return `${plan} makes ${sizeNames.small.toLowerCase()} playgrounds`;
    case "playground_hours":
      return `${plan} has ${count(Math.round(n / 3600), "playground hour", "playground hours")} a month`;
    case "approvals":
      return n === 0
        ? `${plan} has no required approvals`
        : `${plan} allows ${count(n, "required approval", "required approvals")}`;
    case "approvals_override":
      return `${plan} publishes with approvals only`;
  }
}

/** The way out, in a line: "Starter lifts it." */
export function liftLine(limit: PlanLimit): string {
  return limit.upgrade_to
    ? `${planNames[limit.upgrade_to]} lifts it.`
    : "No plan lifts it: contact us for more.";
}
