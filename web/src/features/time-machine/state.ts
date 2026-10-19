// What the time machine shows, kept over a history that changes under it.

import type { Mode } from "./frame";
import type { History } from "./history";
import type { Range } from "./scale";

/** What the view shows: the range, the cursor, the mode, the filters, the selection. */
export interface TimeMachineState {
  range: Range;
  at: number;
  mode: Mode;
  changedOnly: boolean;
  /** A group's id: only its tables stand out. */
  group: string | null;
  query: string;
  /** The selected table's id. */
  table: string | null;
}

/** `state` over `before`'s versions, over `after`'s. */
export function carryOver(
  state: TimeMachineState,
  before: History,
  after: History,
): TimeMachineState {
  const last = after.versions.length - 1;
  const lastBefore = before.versions.length - 1;
  const at = (i: number, fallback: number) => {
    if (i === lastBefore) return last;
    const number = before.versions[i]?.number;
    // A version gone: the nearest one before it that is still there.
    const exact = after.versions.findIndex((v) => v.number === number);
    if (exact >= 0) return exact;
    const below = after.versions.findLastIndex((v) => v.number < (number ?? 0));
    return below >= 0 ? below : fallback;
  };
  let from = at(state.range.from, 0);
  let to = at(state.range.to, last);
  if (to <= from) [from, to] = [0, last];
  const cursor = Math.min(to, Math.max(from, at(state.at, to)));
  return {
    ...state,
    range: { from, to },
    at: cursor,
    table: state.table && after.tables.some((t) => t.id === state.table) ? state.table : null,
  };
}
