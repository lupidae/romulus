import * as React from "react";

import { TableIcon, XmarkIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { formatDate, formatDateTime } from "@/lib/time";
import { cn } from "@/lib/utils";

import { EVENT_VERB, eventsOf, type HistoryEvent } from "../describe";
import type { History, PartTrack, TableTrack } from "../history";
import { ERA_BG, eraOf, type Range, type Scale } from "../scale";

/**
 * Links a version of the history to what it changed (its diff with the
 * version before it); `null` when there is nothing to link (a removed one).
 */
export type VersionLink = React.ComponentType<{
  at: number;
  className?: string;
  children: React.ReactNode;
  label: string;
}>;

interface PartHistory {
  kind: string;
  id: string;
  /** Its name in its last state, when not its id. */
  events: HistoryEvent[];
}

/**
 * Parts with a history of their own: what happened to them apart from
 * coming and going with their table.
 */
function partsOf(track: TableTrack): { title: string; parts: PartHistory[] }[] {
  const table = eventsOf(track.states);
  const withTable = new Set(
    table
      .filter((e) => e.kind !== "changed" && e.kind !== "renamed")
      .map((e) => `${e.at}:${e.kind}`),
  );
  const own = (kind: string, list: PartTrack<unknown>[]) =>
    list
      .map((p) => ({
        kind,
        id: p.id,
        events: eventsOf(p.states).filter(
          (e) => !withTable.has(`${e.at}:${e.kind === "restored" ? "restored" : e.kind}`),
        ),
      }))
      .filter((p) => p.events.length > 0);
  return [
    { title: "Columns", parts: own("column", track.columns) },
    {
      title: "Keys, checks and indexes",
      parts: [...own("constraint", track.constraints), ...own("index", track.indexes)],
    },
    { title: "Policies", parts: own("policy", track.policies) },
  ].filter((s) => s.parts.length > 0);
}

/**
 * The history of the selected table and of its parts: every version that
 * added, changed, renamed or removed something of it, each linking to that
 * version's diff. Events inside the range are in full ink, tinted by the age
 * scale; the others step back.
 */
export function HistoryPanel({
  history,
  track,
  range,
  scale,
  VersionLink,
  onClose,
  className,
}: {
  history: History;
  track: TableTrack;
  range: Range;
  scale: Scale;
  VersionLink: VersionLink;
  onClose: () => void;
  className?: string;
}) {
  const events = React.useMemo(() => eventsOf(track.states), [track]);
  const sections = React.useMemo(() => partsOf(track), [track]);
  const first = events[0];
  const gone = events[events.length - 1]?.kind === "removed";
  const titleId = React.useId();

  return (
    <section aria-labelledby={titleId} className={cn("flex min-h-0 flex-col", className)}>
      <header className="flex shrink-0 items-start gap-2 border-b border-border px-4 py-3">
        <TableIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <div className="grid min-w-0 flex-1 gap-0.5">
          <h2 id={titleId} className="truncate font-mono text-sm font-semibold text-foreground">
            {track.id}
          </h2>
          <p className="text-xs text-muted-foreground">
            {first
              ? `Added in v${history.versions[first.at]?.number}${
                  gone
                    ? `, removed in v${history.versions[events[events.length - 1]!.at]?.number}`
                    : ""
                }`
              : ""}
          </p>
        </div>
        <Button variant="ghost" size="icon-sm" aria-label="Close the history" onClick={onClose}>
          <XmarkIcon />
        </Button>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
        <h3 className="mb-1.5 text-xs font-medium text-muted-foreground">Table</h3>
        <EventList
          events={events}
          history={history}
          range={range}
          scale={scale}
          VersionLink={VersionLink}
        />
        {sections.map((section) => (
          <div key={section.title} className="mt-4">
            <h3 className="mb-1.5 text-xs font-medium text-muted-foreground">{section.title}</h3>
            <ul className="grid gap-2.5">
              {section.parts.map((p) => (
                <li key={`${p.kind}:${p.id}`} className="grid gap-1">
                  <span className="truncate font-mono text-xs font-semibold text-foreground">
                    {p.id}
                    {p.kind !== "column" && (
                      <span className="ml-1.5 font-sans font-normal text-faint-foreground">
                        {p.kind}
                      </span>
                    )}
                  </span>
                  <EventList
                    events={p.events}
                    history={history}
                    range={range}
                    scale={scale}
                    VersionLink={VersionLink}
                  />
                </li>
              ))}
            </ul>
          </div>
        ))}
        {sections.length === 0 && (
          <p className="mt-4 text-xs text-muted-foreground">
            Its columns, keys and indexes never changed on their own.
          </p>
        )}
      </div>
    </section>
  );
}

function EventList({
  events,
  history,
  range,
  scale,
  VersionLink,
}: {
  events: HistoryEvent[];
  history: History;
  range: Range;
  scale: Scale;
  VersionLink: VersionLink;
}) {
  return (
    <ol className="grid gap-1">
      {events.map((e, i) => {
        const v = history.versions[e.at];
        if (!v) return null;
        const inRange = e.at >= range.from && e.at <= range.to;
        const slot = eraOf(scale, e.at);
        const what = e.details.length > 0 ? e.details.join("; ") : EVENT_VERB[e.kind];
        return (
          <li key={i} className={cn("flex items-start gap-2", !inRange && "opacity-60")}>
            <VersionLink
              at={e.at}
              label={`v${v.number}: ${what}. See what v${v.number} changed`}
              className="flex h-5 shrink-0 items-center gap-1 rounded-sm px-1 font-mono text-xs font-medium text-foreground tabular ring-1 ring-border-strong ring-inset"
            >
              <span
                aria-hidden
                className={cn(
                  "size-1.5 rounded-full",
                  slot !== undefined
                    ? ERA_BG[slot]
                    : inRange
                      ? "bg-muted-foreground"
                      : "bg-border-strong",
                )}
              />
              v{v.number}
            </VersionLink>
            <span className="min-w-0 text-xs leading-5">
              <span
                className={cn(
                  "text-foreground",
                  e.kind === "removed" && "text-muted-foreground line-through",
                )}
              >
                {what}
              </span>
              <span className="text-muted-foreground">
                {" "}
                · {v.author?.name ?? "deleted account"},{" "}
                <time dateTime={v.created_at} title={formatDateTime(v.created_at)}>
                  {formatDate(v.created_at)}
                </time>
              </span>
            </span>
          </li>
        );
      })}
    </ol>
  );
}
