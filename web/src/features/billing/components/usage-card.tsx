import type * as React from "react";

import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { Progress } from "@/components/ui/progress";
import { intlLocale, useLocale } from "@/features/legal/locale";
import { cn } from "@/lib/utils";

import { type BillingCopy, billingCopy } from "../copy";
import {
  duration,
  formatLongDate,
  historyLabel,
  meterPercent,
  meterState,
  type MeterState,
  planNames,
} from "../format";
import type { Meter, OrgBilling } from "../queries";

type UsageWords = BillingCopy["usage"];

function stateWord(words: UsageWords, state: MeterState): string | null {
  return state === "ok" ? null : words[state];
}

/** Usage against every cap of the plan in force, the plan's one line each. */
export function UsageCard({ billing }: { billing: OrgBilling }) {
  const locale = useLocale();
  const t = billingCopy[locale].usage;
  const number = (n: number) => n.toLocaleString(intlLocale(locale));
  const { usage } = billing;
  const hours = usage.playground_hours;
  return (
    <Card aria-labelledby="usage-title" lang={locale}>
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle id="usage-title">{t.title}</CardTitle>
          <p className="text-sm text-muted-foreground">{t.against(planNames[billing.plan])}</p>
        </div>
      </CardHeader>
      <ul className="divide-y divide-border">
        <MeterRow words={t} label={t.members} meter={usage.members} format={number} />
        <MeterRow words={t} label={t.databases} meter={usage.databases} format={number} />
        <MeterRow
          words={t}
          label={t.versions}
          meter={usage.versions}
          format={number}
          note={
            usage.versions.database && usage.versions.used > 0
              ? t.mostIn(usage.versions.database)
              : undefined
          }
        />
        <MeterRow
          words={t}
          label={t.schema}
          meter={usage.schema_size}
          format={number}
          note={
            usage.schema_size.database && usage.schema_size.used > 0
              ? t.largest(usage.schema_size.database)
              : undefined
          }
        />
        <Row
          words={t}
          label={t.history}
          value={historyLabel(usage.history_days, locale)}
          note={usage.hidden_versions > 0 ? t.hidden(usage.hidden_versions) : undefined}
        />
        <MeterRow
          words={t}
          label={t.hours}
          meter={{ used: hours.used_seconds, limit: hours.limit_seconds }}
          format={duration}
          metered
          note={
            hours.exhausted
              ? t.hoursWait(formatLongDate(hours.resets_on, locale))
              : t.hoursReset(formatLongDate(hours.resets_on, locale))
          }
        />
        <Row words={t} label={t.playground} value={t.size(usage.playground_size)} />
        <Row
          words={t}
          label={t.approvals}
          value={
            usage.approvals.limit === 0
              ? t.approvalsNone
              : t.approvalsValue(
                  usage.approvals.used,
                  usage.approvals.limit ?? 0,
                  usage.approvals_override,
                )
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
  words,
  label,
  value,
  note,
  children,
  state = "ok",
  metered = false,
}: {
  words: UsageWords;
  label: string;
  value: string;
  note?: string;
  children?: React.ReactNode;
  state?: MeterState;
  metered?: boolean;
}) {
  const word = stateWord(words, state);
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
  words,
  label,
  meter,
  note,
  format,
  metered = false,
}: {
  words: UsageWords;
  label: string;
  meter: Pick<Meter, "used" | "limit">;
  note?: string;
  format: (n: number) => string;
  /** Time that runs out (playground hours), not a count. */
  metered?: boolean;
}) {
  const state = meterState(meter.used, meter.limit);
  // "1 of 3", "4 (no limit)".
  const value =
    meter.limit == null
      ? words.unlimited(format(meter.used))
      : words.of(format(meter.used), format(meter.limit));
  const word = stateWord(words, state);
  return (
    <Row words={words} label={label} value={value} note={note} state={state} metered={metered}>
      {meter.limit != null && (
        <Progress
          aria-label={`${label}: ${value}${word ? `, ${word}` : ""}`}
          value={meterPercent(meter.used, meter.limit)}
          tone={toneOf(state, metered)}
        />
      )}
    </Row>
  );
}
