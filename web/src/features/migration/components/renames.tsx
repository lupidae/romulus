import * as React from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { useMediaQuery } from "@/features/schema/viewer/use-preference";
import { cn } from "@/lib/utils";

import type { RenameCandidate, RenameDecisions } from "../queries";

/** Rows shown before "Show N more". */
const FIRST_ROWS = 4;

type Rename = RenameCandidate["rename"];

/** `users.email`, `shop.users`: what a rename belongs to, schema shown when not public. */
function where(rename: Rename): string {
  const schema = rename.schema === "public" ? "" : `${rename.schema}.`;
  return `${schema}${rename.parent ?? ""}`;
}

/** What is renamed, as a short tag: `Column of users`. */
function renameTag(rename: Rename): string {
  const of = where(rename);
  switch (rename.kind) {
    case "column":
      return `Column of ${of}`;
    case "constraint":
      return `Constraint on ${of}`;
    case "index":
      return `Index on ${of}`;
    case "policy":
      return `Policy on ${of}`;
    case "sequence":
      return `Sequence of ${of}`;
    case "enum_value":
      return `Value of ${of}`;
    case "attribute":
      return `Attribute of ${of}`;
    case "table":
      return "Table";
    case "partition":
      return "Partition";
    case "view":
      return "View";
    case "materialized_view":
      return "Materialized view";
    case "enum":
      return "Enum";
    case "domain":
      return "Domain";
    case "composite":
      return "Type";
  }
}

/** A move to another schema (`ALTER … SET SCHEMA`), renamed or not. */
function movedTo(rename: Rename): string | null {
  const to = rename.to_schema;
  return to && to !== rename.schema ? to : null;
}

/** What happens without the rename, in a sentence. */
function withoutIt(rename: Rename): string {
  const { from, to } = rename;
  const moved = movedTo(rename);
  if (moved) {
    const what =
      rename.kind === "table" || rename.kind === "partition"
        ? "losing its rows"
        : rename.kind === "view" || rename.kind === "materialized_view"
          ? "and what reads it rebuilt"
          : "and its columns converted, which can fail";
    return `${rename.schema}.${from} is dropped and ${moved}.${to} created, ${what}.`;
  }
  switch (rename.kind) {
    case "column":
      return `${from} is dropped and ${to} added, losing the column's data.`;
    case "table":
    case "partition":
      return `${from} is dropped and ${to} created, losing its rows.`;
    case "enum_value":
      return `The type is created again and its columns converted: rows holding '${from}' fail.`;
    case "enum":
    case "domain":
    case "composite":
      return `${to} is created anew and its columns converted to it, which can fail.`;
    case "view":
    case "materialized_view":
      return `${from} is dropped and ${to} created again.`;
    default:
      return `${from} is dropped and ${to} created again.`;
  }
}

/** A name as it reads in SQL: labels quoted. */
function quoted(rename: Rename, name: string): string {
  return rename.kind === "enum_value" ? `'${name}'` : name;
}

/** Turned off, it drops data (rather than failing, or rebuilding a view). */
function dropsData(rename: Rename): boolean {
  return rename.kind === "column" || rename.kind === "table" || rename.kind === "partition";
}

/** What romulus does with no decision: on when confident, off otherwise. */
function byDefault(c: RenameCandidate): boolean {
  return c.confidence === "certain" || (c.confidence === "high" && c.rivals.length === 0);
}

/**
 * Where a row goes: off and dropping data first (whoever switched it off),
 * then unsure, then confident, then known.
 */
function rank(c: RenameCandidate, on: boolean): number {
  if (c.confidence === "certain") return 3;
  if (!on && dropsData(c.rename)) return 0;
  return byDefault(c) ? 2 : 1;
}

/** The rows' order as the list appeared, and how many lead it (off or unsure). */
interface Order {
  sig: string;
  keys: readonly string[];
  lead: number;
}

export interface RenamesProps {
  candidates: readonly RenameCandidate[];
  /** What is on screen: the stored decisions with this person's changes. */
  decisions: RenameDecisions;
  /**
   * `candidates` are of `decisions` (the migration for them arrived): each
   * row shows what ran. Until then, what was switched.
   */
  fresh?: boolean;
  /**
   * Turns renames on or off (a rival's partners off with it), with what
   * romulus would do for each of them by itself (`defaults`: where a
   * decision equal to it can go); absent, read-only.
   */
  onChange?: (patch: RenameDecisions, defaults: RenameDecisions) => void;
  /** Why the switches are disabled, shown under the list. */
  readOnly?: string;
  /** What makes a known rename known here: the draft's ALTER … RENAME, a publish. */
  known?: string;
}

/**
 * The renames a migration considered: "Renamed email → email_address?" with
 * a switch for each detected one (on when romulus is confident, off when
 * unsure), the known ones stated, and what comes along with each (a key
 * named after a renamed column). The migration regenerates as soon as one
 * is switched.
 */
export function Renames({
  candidates,
  decisions,
  fresh = false,
  onChange,
  readOnly,
  known = "Known renames ran as ALTER … RENAME, or were chosen when a version was published.",
}: RenamesProps) {
  const uid = React.useId();
  const narrow = useMediaQuery("(max-width: 40rem)");
  const [expanded, setExpanded] = React.useState(false);

  // Once the migration for what is on screen arrived, what ran (two rivals
  // switched on at once in two tabs: one ran); until then, what was switched.
  const on = (c: RenameCandidate) =>
    c.confidence === "certain" || (fresh ? c.applied : (decisions[c.key] ?? c.applied));
  // Ordered as the list appeared, then kept: a row never moves when it is
  // switched (a new list of renames is ordered again).
  const sig = candidates.map((c) => c.key).join("\n");
  const [order, setOrder] = React.useState<Order | null>(null);
  let current = order;
  if (current?.sig !== sig) {
    const ranked = candidates
      .filter((c) => !c.follows)
      .map((c) => ({ key: c.key, rank: rank(c, on(c)) }))
      .sort((a, b) => a.rank - b.rank);
    current = {
      sig,
      keys: ranked.map((r) => r.key),
      lead: ranked.filter((r) => r.rank <= 1).length,
    };
    setOrder(current);
  }
  const position = new Map(current.keys.map((key, i) => [key, i]));
  const rows = candidates
    .filter((c) => !c.follows)
    .sort((a, b) => (position.get(a.key) ?? 0) - (position.get(b.key) ?? 0));
  const followers = (key: string) => candidates.filter((c) => c.follows === key);
  const switchable = rows.filter((c) => c.confidence !== "certain");
  // A phone shows the rows that lead (off, unsure), a wide screen a few;
  // either way a row that is off and drops data is never behind "Show".
  const base = narrow ? current.lead : Math.min(FIRST_ROWS, rows.length);
  const visible = expanded
    ? rows
    : rows.filter(
        (c, i) => i < base || (c.confidence !== "certain" && !on(c) && dropsData(c.rename)),
      );
  const more = rows.length - visible.length;
  if (rows.length === 0) return null;

  const toggle = (c: RenameCandidate, next: boolean) => {
    const patch: RenameDecisions = { [c.key]: next };
    const defaults: RenameDecisions = { [c.key]: byDefault(c) };
    // Rivals are one choice: turning one on turns the others off.
    if (next) {
      for (const rival of c.rivals) {
        patch[rival] = false;
        const partner = candidates.find((x) => x.key === rival);
        defaults[rival] = partner ? byDefault(partner) : false;
      }
    }
    onChange?.(patch, defaults);
  };

  const losing = switchable.filter((c) => !on(c) && dropsData(c.rename)).length;
  const summary =
    switchable.length === 0
      ? `${rows.length} known`
      : `${switchable.filter(on).length} of ${switchable.length} on`;

  return (
    <section
      aria-labelledby={`${uid}-title`}
      className="shrink-0 border-b border-border px-3 py-2.5"
    >
      <div className="flex items-baseline justify-between gap-3">
        <h2 id={`${uid}-title`} className="text-sm font-medium">
          Renames
        </h2>
        <span className="text-sm text-muted-foreground tabular-nums">
          {summary}
          {losing > 0 && (
            <span className="text-destructive">
              , {losing} {losing === 1 ? "drops" : "drop"} data
            </span>
          )}
        </span>
      </div>
      {visible.length > 0 && (
        <ul className="mt-1 grid divide-y divide-border">
          {visible.map((c) => (
            <Row
              key={c.key}
              // Not the key: names may hold spaces, which ids may not.
              id={`${uid}-${position.get(c.key) ?? 0}`}
              candidate={c}
              on={on(c)}
              along={followers(c.key)}
              rivals={c.rivals.length}
              disabled={!onChange || readOnly !== undefined}
              onToggle={(next) => toggle(c, next)}
            />
          ))}
        </ul>
      )}
      {visible.some((c) => c.confidence === "certain") && (
        <p className="mt-1.5 text-xs text-muted-foreground">{known}</p>
      )}
      {readOnly && visible.length > 0 && switchable.length > 0 && (
        <p className="mt-1.5 text-xs text-muted-foreground">{readOnly}</p>
      )}
      {more > 0 ? (
        <Button size="sm" variant="link" className="mt-1" onClick={() => setExpanded(true)}>
          {visible.length === 0 ? `Show ${rows.length === 1 ? "it" : "them"}` : `Show ${more} more`}
        </Button>
      ) : (
        expanded &&
        rows.length > base && (
          <Button size="sm" variant="link" className="mt-1" onClick={() => setExpanded(false)}>
            Show fewer
          </Button>
        )
      )}
    </section>
  );
}

function Row({
  id,
  candidate,
  on,
  along,
  rivals,
  disabled,
  onToggle,
}: {
  id: string;
  candidate: RenameCandidate;
  on: boolean;
  along: RenameCandidate[];
  rivals: number;
  disabled: boolean;
  onToggle: (on: boolean) => void;
}) {
  const r = candidate.rename;
  const known = candidate.confidence === "certain";
  const moved = movedTo(r);
  const code = (text: string) => <code className="font-mono text-[0.92em]">{text}</code>;
  const name = moved ? (
    <>
      Moved {code(`${r.schema}.${r.from}`)}{" "}
      {r.from === r.to ? (
        <>to {code(moved)}</>
      ) : (
        <>
          <span aria-hidden>→</span>
          <span className="sr-only">to</span> {code(`${moved}.${r.to}`)}
        </>
      )}
      {known ? "" : "?"}
    </>
  ) : (
    <>
      Renamed {code(quoted(r, r.from))} <span aria-hidden>→</span>
      <span className="sr-only">to</span> {code(quoted(r, r.to))}
      {known ? "" : "?"}
    </>
  );
  return (
    <li className="flex items-start gap-3 py-2">
      <div className="grid min-w-0 flex-1 gap-0.5">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5">
          <span className="text-xs text-muted-foreground">{renameTag(r)}</span>
          {known && <Badge variant="outline">Known</Badge>}
          {!known && rivals > 0 && <Badge variant="outline">Pick one</Badge>}
          {!known && rivals === 0 && candidate.confidence === "medium" && (
            <Badge variant="outline">Unsure</Badge>
          )}
        </div>
        {known ? (
          <p className="text-sm break-words">{name}</p>
        ) : (
          <Label htmlFor={id} className="cursor-pointer text-sm font-normal break-words">
            {name}
          </Label>
        )}
        {!known && (
          <p id={`${id}-why`} className="text-xs text-muted-foreground">
            {candidate.reason}
            {rivals > 0 && ` Turning it on turns the ${rivals === 1 ? "other one" : "others"} off.`}
          </p>
        )}
        {along.length > 0 && (
          <p className={cn("text-xs text-muted-foreground", !on && "line-through opacity-70")}>
            Along with it:{" "}
            {along.map((x, i) => (
              <React.Fragment key={x.key}>
                {i > 0 && ", "}
                <code className="font-mono">{x.rename.from}</code> →{" "}
                <code className="font-mono">{x.rename.to}</code>
              </React.Fragment>
            ))}
          </p>
        )}
        {!on && (
          <p id={`${id}-off`} className="text-xs text-destructive">
            <span className="font-medium">Off:</span> {withoutIt(r)}
          </p>
        )}
      </div>
      {!known && (
        <Switch
          id={id}
          checked={on}
          disabled={disabled}
          onCheckedChange={onToggle}
          aria-describedby={on ? `${id}-why` : `${id}-why ${id}-off`}
          className="mt-0.5"
        />
      )}
    </li>
  );
}
