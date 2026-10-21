import type * as React from "react";

import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { Progress } from "@/components/ui/progress";
import { formatDate } from "@/lib/time";
import { cn } from "@/lib/utils";

import {
  count,
  duration,
  historyLabel,
  meterPercent,
  meterState,
  type MeterState,
  planNames,
  sizeNames,
} from "../format";
import type { Meter, OrgBilling } from "../queries";

const stateWords: Record<MeterState, string | null> = {
  ok: null,
  near: "Almost at the limit",
  full: "At the limit",
  over: "Over the limit: creating more waits",
};

/** A meter's value: "1 of 3", "4 (no limit)". */
function ofLimit(
  used: number,
  limit: number | null | undefined,
  format: (n: number) => string,
): string {
  return limit == null ? `${format(used)} (no limit)` : `${format(used)} of ${format(limit)}`;
}

/** Usage against every cap of the plan in force, the plan's one line each. */
export function UsageCard({ billing }: { billing: OrgBilling }) {
  const { usage } = billing;
  const hours = usage.playground_hours;
  return (
    <Card aria-labelledby="usage-title">
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle id="usage-title">Usage</CardTitle>
          <p className="text-sm text-muted-foreground">
            Against the {planNames[billing.plan]} plan. Reaching a limit stops creating more;
            nothing is deleted.
          </p>
        </div>
      </CardHeader>
      <ul className="divide-y divide-border">
        <MeterRow label="Members and invitations" meter={usage.members} />
        <MeterRow label="Databases" meter={usage.databases} />
        <MeterRow
          label="Versions per database"
          meter={usage.versions}
          note={
            usage.versions.database && usage.versions.used > 0
              ? `Most in ${usage.versions.database}`
              : undefined
          }
        />
        <MeterRow
          label="Tables and views per schema"
          meter={usage.schema_size}
          note={
            usage.schema_size.database && usage.schema_size.used > 0
              ? `Largest: ${usage.schema_size.database}`
              : undefined
          }
        />
        <Row
          label="History shown"
          value={historyLabel(usage.history_days)}
          note={
            usage.hidden_versions > 0
              ? `${count(usage.hidden_versions, "older version is", "older versions are")} kept, hidden by the plan`
              : undefined
          }
        />
        <MeterRow
          label="Playground hours this month"
          meter={{ used: hours.used_seconds, limit: hours.limit_seconds }}
          format={duration}
          metered
          note={
            hours.exhausted
              ? `Playgrounds and tests wait until ${formatDate(hours.resets_on)}`
              : `Counts again from ${formatDate(hours.resets_on)}`
          }
        />
        <Row label="Largest playground" value={`${sizeNames[usage.playground_size]} rows`} />
        <Row
          label="Required approvals"
          value={
            usage.approvals.limit === 0
              ? "Not in this plan"
              : `${usage.approvals.used} required, up to ${usage.approvals.limit}${
                  usage.approvals_override ? ", admin override" : ""
                }`
          }
        />
      </ul>
    </Card>
  );
}

/** How a state reads: a count at its limit is calm (the next one asks for a
 * plan); time running out is a warning; past a limit (after a downgrade) is
 * the one red. */
function toneOf(state: MeterState, metered: boolean): "default" | "warning" | "destructive" {
  if (state === "over") return "destructive";
  return metered && state !== "ok" ? "warning" : "default";
}

function Row({
  label,
  value,
  note,
  children,
  state = "ok",
  metered = false,
}: {
  label: string;
  value: string;
  note?: string;
  children?: React.ReactNode;
  state?: MeterState;
  metered?: boolean;
}) {
  const word = stateWords[state];
  const tone = toneOf(state, metered);
  return (
    <li className="grid gap-1.5 px-4 py-3">
      <div className="flex items-baseline justify-between gap-4">
        <span className="text-sm font-medium text-foreground">{label}</span>
        <span className="shrink-0 text-sm text-foreground tabular-nums">{value}</span>
      </div>
      {children}
      {(note || word) && (
        <div className="flex items-baseline justify-between gap-4 text-sm text-muted-foreground">
          <span className="min-w-0 truncate">{note}</span>
          {word && (
            <span
              className={cn(
                "shrink-0 font-medium",
                tone === "destructive" && "text-destructive",
                tone === "warning" && "text-warning",
                tone === "default" && "text-foreground",
              )}
            >
              {word}
            </span>
          )}
        </div>
      )}
    </li>
  );
}

function MeterRow({
  label,
  meter,
  note,
  format = (n: number) => n.toLocaleString("en-US"),
  metered = false,
}: {
  label: string;
  meter: Pick<Meter, "used" | "limit">;
  note?: string;
  format?: (n: number) => string;
  /** Time that runs out (playground hours), not a count. */
  metered?: boolean;
}) {
  const state = meterState(meter.used, meter.limit);
  const value = ofLimit(meter.used, meter.limit, format);
  return (
    <Row label={label} value={value} note={note} state={state} metered={metered}>
      {meter.limit != null && (
        <Progress
          aria-label={`${label}: ${value}${stateWords[state] ? `, ${stateWords[state]}` : ""}`}
          value={meterPercent(meter.used, meter.limit)}
          tone={toneOf(state, metered)}
        />
      )}
    </Row>
  );
}
