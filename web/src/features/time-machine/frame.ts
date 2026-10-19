// One frame of the time machine: the graph of a range of versions at a
// cursor. Superposed, every object the range ever had is drawn, whatever the
// cursor: what is there at the cursor in full, the rest faded back (gone by
// then: struck through and dashed; not there yet: plain). At the cursor,
// exactly the schema of that version. Either way each object is colored by
// when it appeared in the range (the age scale), marked when it changed, and
// faded back when a filter leaves it out.
//
// `Frames` keeps what it built: a table whose picture did not change from
// one frame to the next is handed again as the same `TableModel` (and its
// edges as the same `EdgeModel`s), so the graph redraws and reroutes only
// what changed. Objects coming into view animate in; objects leaving stay
// one more frame, animating out, until `settle`.

import {
  buildGraph,
  type ColumnModel,
  type EdgeModel,
  type GraphModel,
  type TableModel,
  type TimeMark,
} from "@/features/schema/graph/build";
import type {
  Column,
  CompositeType,
  Constraint,
  DomainType,
  Entity,
  EnumType,
  Index,
  Partition,
  Policy,
  RelationRef,
  Schema,
} from "@/features/schema/model";

import { historyLines } from "./describe";
import {
  bornWithin,
  changesWithin,
  firstWithin,
  goneWithin,
  type History,
  lastWithin,
  type PartTrack,
  runOf,
  type State,
  stateAt,
  type TableHead,
  type TableTrack,
} from "./history";
import { eraOf, type Range, type Scale, scaleOf } from "./scale";

export type Mode = "superposed" | "cursor";

export interface Filters {
  /** Only what appeared, went or changed in the range stands out. */
  changedOnly?: boolean;
  /** Only these tables (ids) stand out: a group's members. */
  tables?: ReadonlySet<string> | null;
  /** Only tables whose name, or a column's, holds this stand out. */
  query?: string;
}

export interface FrameRequest {
  range: Range;
  /** The cursor, a version index within the range. */
  at: number;
  mode: Mode;
  filters?: Filters;
}

export interface Counts {
  added: number;
  removed: number;
  changed: number;
}

export interface Frame {
  request: FrameRequest;
  scale: Scale;
  /** The model drawn: tables under their ids, parts under theirs. */
  schema: Schema;
  graph: GraphModel;
  /** Objects drawn leaving: `settle` drops them. */
  leaving: number;
  /** Objects drawn coming in: `settle` stills them. */
  entering: number;
  /** What the range changed, from its first version to its last (whatever the cursor and mode). */
  counts: { tables: Counts; columns: Counts };
  /** Superposed: tables and columns drawn faded, gone by the cursor or not there yet. */
  away: { gone: number; later: number };
}

/**
 * Where an object stands at the cursor: there, gone by then, or not there
 * yet (later in the range). Only superposed frames draw the last two.
 */
type Presence = "here" | "gone" | "later";

/** A part of a table as a frame draws it. */
interface PartPick<V> {
  id: string;
  /** Index of its track in the table's list. */
  track: number;
  /** Index of the state drawn in its track. */
  index: number;
  state: State<V>;
  presence: Presence;
  mark: TimeMark;
}

/** A table as a frame draws it. */
interface Picture {
  track: TableTrack;
  head: State<TableHead>;
  headIndex: number;
  presence: Presence;
  mark: TimeMark;
  columns: PartPick<Column>[];
  constraints: PartPick<Constraint>[];
  indexes: PartPick<Index>[];
  policies: PartPick<Policy>[];
  sig: string;
}

interface Built {
  sig: string;
  context: string;
  entity: Entity;
  table?: TableModel;
}

const NO_FILTERS: Filters = {};
const EMPTY: Counts = { added: 0, removed: 0, changed: 0 };

/**
 * Which state a frame draws: the one at the cursor; superposed, otherwise
 * the last one before it in the range (gone by then), else the first one
 * after it (not there yet).
 */
function pick<V>(
  states: readonly State<V>[],
  request: FrameRequest,
): { state: State<V>; index: number; presence: Presence } | undefined {
  const chosen = (state: State<V> | undefined, presence: Presence) =>
    state ? { state, index: states.indexOf(state), presence } : undefined;
  const now = stateAt(states, request.at);
  if (now) return chosen(now, "here");
  if (request.mode === "cursor") return undefined;
  return (
    chosen(lastWithin(states, request.range.from, request.at), "gone") ??
    chosen(firstWithin(states, request.at + 1, request.range.to), "later")
  );
}

/** The part of a mark that the graph draws, as a string: what a picture's signature holds. */
function markSig(m: TimeMark): string {
  return `${m.era ?? ""}${m.ghost ? "g" : ""}${m.later ? "l" : ""}${m.changed ? "c" : ""}${
    m.muted ? "m" : ""
  }${m.motion ?? ""}:${m.label ?? ""}:${m.added ?? ""}:${m.removed ?? ""}`;
}

const lower = (s: string) => s.toLocaleLowerCase();

/** The name a table has in its id (`#2` kept): what the frame's model calls it. */
export function idName(track: Pick<TableTrack, "id" | "schema">): string {
  return track.id.slice(track.schema.length + 1);
}

/**
 * Builds the frames of one history. Keep one per history: it remembers the
 * last frame, to hand unchanged tables again and to animate what comes and
 * goes.
 */
export class Frames {
  private readonly history: History;
  private built = new Map<string, Built>();
  private edges = new Map<string, EdgeModel>();
  private lines = new Map<string, readonly string[]>();
  private previous = new Map<string, Picture>();
  private last: Frame | null = null;

  constructor(history: History) {
    this.history = history;
  }

  /** The frame for `request`, animating from the last one. */
  frame(request: FrameRequest): Frame {
    return this.make(request, true);
  }

  /** The last frame again, with what was leaving gone and what was coming in still. */
  settle(): Frame | null {
    return this.last ? this.make(this.last.request, false) : null;
  }

  private historyOf(key: string, states: readonly State<unknown>[]) {
    let lines = this.lines.get(key);
    if (!lines) {
      lines = historyLines(states, this.history);
      this.lines.set(key, lines);
    }
    return lines;
  }

  private make(request: FrameRequest, animate: boolean): Frame {
    const { history } = this;
    const { from, to } = request.range;
    const at = request.at;
    // Superposed, the marks speak of the whole range; at the cursor, of the
    // range up to it (the version drawn has nothing of later).
    const superposed = request.mode === "superposed";
    const end = superposed ? to : at;
    const filters = request.filters ?? NO_FILTERS;
    const query = filters.query?.trim() ? lower(filters.query.trim()) : null;
    const scale = scaleOf(request.range);
    const versions = history.versions;
    const animating = animate && this.last !== null;
    const number = (i: number | null | undefined) =>
      i === null || i === undefined ? undefined : versions[i]?.number;

    // When it appeared and when it goes, as the run of states drawn says:
    // `+v4` the run's start within the range (or, gone by the cursor, its
    // return after it: `−v5 +v8`), `−v7` its end (superposed only: at the
    // cursor, nothing of later is drawn).
    const markOf = (
      states: readonly State<unknown>[],
      chosen: { index: number; presence: Presence },
      key: string,
    ): TimeMark => {
      const { presence } = chosen;
      const run = runOf(states, chosen.index);
      let appeared = run.first.from > from ? run.first.from : undefined;
      const went =
        superposed && run.last.to !== null && run.last.to <= to ? run.last.to : undefined;
      if (presence === "gone") {
        const back = states.find((s) => s.from > at && s.from <= to);
        if (back) appeared = back.from;
      }
      const mark: TimeMark = { history: this.historyOf(key, states) };
      const era = eraOf(scale, appeared);
      if (era !== undefined) mark.era = era;
      if (appeared !== undefined) mark.added = number(appeared);
      if (went !== undefined) mark.removed = number(went);
      if (presence === "gone") mark.ghost = true;
      if (presence === "later") mark.later = true;
      if (changesWithin(states, from, end).length > 0) mark.changed = true;
      return mark;
    };
    const eventful = (states: readonly State<unknown>[]) =>
      bornWithin(states, from, end) !== undefined ||
      goneWithin(states, from, end) !== undefined ||
      changesWithin(states, from, end).length > 0;

    // What the range changed, from its first version to its last.
    const counts = { tables: { ...EMPTY }, columns: { ...EMPTY } };
    const tally = (into: Counts, states: readonly State<unknown>[]) => {
      const first = stateAt(states, from) !== undefined;
      const last = stateAt(states, to) !== undefined;
      if (!first && last) into.added++;
      else if (first && !last) into.removed++;
      else if (first && last && changesWithin(states, from, to).length > 0) into.changed++;
      // Neither at the start nor at the end: came and went within the range.
    };

    // ---- Pictures: what each table draws -----------------------------------
    const pictures = new Map<string, Picture>();
    for (const track of history.tables) {
      const head = pick(track.states, request);
      if (!head) continue;
      const parts = <V>(list: PartTrack<V>[], kind: string): PartPick<V>[] => {
        const out: PartPick<V>[] = [];
        list.forEach((p, i) => {
          const chosen = pick(p.states, request);
          if (!chosen) return;
          out.push({
            id: p.id,
            track: i,
            index: chosen.index,
            state: chosen.state,
            presence: chosen.presence,
            mark: markOf(p.states, chosen, `${track.id}\u0000${kind}\u0000${p.id}`),
          });
        });
        return out;
      };
      const columns = orderColumns(track, head.state, parts(track.columns, "c"));
      const picture: Picture = {
        track,
        head: head.state,
        headIndex: head.index,
        presence: head.presence,
        mark: markOf(track.states, head, track.id),
        columns,
        constraints: parts(track.constraints, "k"),
        indexes: parts(track.indexes, "i"),
        policies: parts(track.policies, "p"),
        sig: "",
      };
      const frameName = picture.head.value.name;
      if (frameName !== idName(track)) picture.mark.label = frameName;
      for (const c of columns) if (c.state.value.name !== c.id) c.mark.label = c.state.value.name;

      // Filters fade back what they leave out.
      const changedHere =
        eventful(track.states) ||
        [...track.columns, ...track.constraints, ...track.indexes, ...track.policies].some((p) =>
          eventful(p.states),
        );
      const muted =
        (filters.changedOnly && !changedHere) ||
        (filters.tables && !filters.tables.has(track.id)) ||
        (query !== null &&
          !lower(track.id).includes(query) &&
          !lower(frameName).includes(query) &&
          !columns.some((c) => lower(c.state.value.name).includes(query)));
      if (muted) picture.mark.muted = true;
      if (
        filters.changedOnly &&
        !muted &&
        picture.mark.era === undefined &&
        head.presence !== "gone"
      ) {
        for (const c of columns) {
          const states = track.columns[c.track]!.states;
          if (!eventful(states)) c.mark.muted = true;
        }
      }
      pictures.set(track.id, picture);
    }
    for (const track of history.tables) {
      tally(counts.tables, track.states);
      if (stateAt(track.states, from) && stateAt(track.states, to))
        for (const c of track.columns) tally(counts.columns, c.states);
    }

    // ---- Motion: what comes into view, and what leaves it --------------------
    let entering = 0;
    let leaving = 0;
    if (animating) {
      for (const [id, picture] of pictures) {
        const before = this.previous.get(id);
        if (!before) {
          picture.mark.motion = "enter";
          entering++;
          continue;
        }
        const was = new Set(before.columns.map((c) => c.id));
        for (const c of picture.columns)
          if (!was.has(c.id)) {
            c.mark.motion = "enter";
            entering++;
          }
        const now = new Set(picture.columns.map((c) => c.id));
        before.columns.forEach((c, i) => {
          if (now.has(c.id) || c.mark.motion === "leave") return;
          // Where it was: after the column it followed then.
          const after = before.columns[i - 1]?.id;
          const at = after ? picture.columns.findIndex((x) => x.id === after) + 1 : 0;
          picture.columns.splice(at, 0, { ...c, mark: { ...c.mark, motion: "leave" } });
          leaving++;
        });
      }
      for (const [id, before] of this.previous) {
        if (pictures.has(id) || before.mark.motion === "leave") continue;
        pictures.set(id, {
          ...before,
          mark: { ...before.mark, motion: "leave" },
          columns: before.columns.filter((c) => c.mark.motion !== "leave"),
        });
        leaving++;
      }
    }

    // ---- The model drawn --------------------------------------------------------
    // Tables and columns go by their ids; references by the names of the
    // moment are pointed at them (renames).
    const tableIds = new Map<string, RelationRef>();
    const columnIds = new Map<string, Map<string, string>>();
    let renames = "";
    for (const p of pictures.values()) {
      const id = { schema: p.track.schema, name: idName(p.track) };
      tableIds.set(`${p.head.value.schema}.${p.head.value.name}`, id);
      const columns = new Map(p.columns.map((c) => [c.state.value.name, c.id]));
      columnIds.set(p.track.id, columns);
      if (p.head.value.name !== id.name || p.head.value.schema !== id.schema)
        renames += `${p.track.id}=${p.head.value.schema}.${p.head.value.name};`;
      for (const c of p.columns) if (c.state.value.name !== c.id) renames += `${c.id}~`;
    }

    const types = this.types(request);
    const partitions: Partition[] = [];
    let partitionSig = "";
    for (const p of history.partitions) {
      const chosen = pick(p.states, request);
      if (!chosen) continue;
      const parent = tableIds.get(
        `${chosen.state.value.parent_schema}.${chosen.state.value.parent}`,
      );
      partitions.push(
        parent
          ? { ...chosen.state.value, parent_schema: parent.schema, parent: parent.name }
          : chosen.state.value,
      );
      partitionSig += `${p.schema}.${p.name}:${chosen.index};`;
    }
    const schemas = new Set([...pictures.values()].map((p) => p.track.schema));
    const context = `${types.sig}|${partitionSig}|${renames}|${schemas.size > 1}`;

    const entities: Entity[] = [];
    const sigs = new Map<string, string>();
    for (const p of pictures.values()) {
      const sig = signature(p);
      sigs.set(p.track.id, sig);
      const kept = this.built.get(p.track.id);
      if (kept && kept.sig === sig && kept.context === context) {
        entities.push(kept.entity);
        continue;
      }
      const entity = entityOf(p, tableIds, columnIds);
      this.built.set(p.track.id, { sig, context, entity });
      entities.push(entity);
    }

    const latest = versions[versions.length - 1];
    const schema: Schema = {
      remus_version: "",
      database: "",
      server_version: latest?.postgres.server_version ?? "",
      entities,
      enums: types.enums,
      domains: types.domains,
      composites: types.composites,
      partitions,
    };

    // ---- The graph, handing unchanged tables and edges again -----------------
    const fresh = buildGraph({ schema });
    const reused = new Set<string>();
    const tables = fresh.tables.map((t) => {
      const p = pictures.get(trackIdOf(t));
      if (!p) return t;
      const kept = this.built.get(p.track.id);
      if (kept?.table && kept.entity === t.entity) {
        reused.add(t.key);
        return kept.table;
      }
      t.time = p.mark;
      const marks = new Map(p.columns.map((c) => [c.id, c.mark]));
      t.columns = t.columns.map((c: ColumnModel) => {
        const mark = marks.get(c.name);
        return mark ? { ...c, time: mark } : c;
      });
      if (kept) kept.table = t;
      return t;
    });
    const byKey = new Map(tables.map((t) => [t.key, t]));
    const keptEdges = this.edges;
    this.edges = new Map();
    const edges = fresh.edges.map((e) => {
      const kept = keptEdges.get(e.id);
      if (
        kept &&
        reused.has(e.source) &&
        reused.has(e.target) &&
        kept.target === e.target &&
        kept.sourceColumn === e.sourceColumn &&
        kept.targetColumn === e.targetColumn
      ) {
        this.edges.set(e.id, kept);
        return kept;
      }
      const source = byKey.get(e.source);
      const p = source ? pictures.get(trackIdOf(source)) : undefined;
      if (p) {
        const target = byKey.get(e.target);
        const own = e.kind === "fk" ? p.constraints.find((c) => c.id === e.name)?.mark : undefined;
        const time: TimeMark = {
          ...(own ?? {
            era: p.mark.era,
            ghost: p.mark.ghost,
            later: p.mark.later,
            added: p.mark.added,
            removed: p.mark.removed,
          }),
        };
        // An edge is there at the cursor only when both its ends are.
        if (p.mark.ghost || target?.time?.ghost) {
          time.ghost = true;
          delete time.later;
        } else if (p.mark.later || target?.time?.later) time.later = true;
        if (p.mark.muted || target?.time?.muted) time.muted = true;
        if (p.mark.motion === "leave" || target?.time?.motion === "leave") time.motion = "leave";
        else if (p.mark.motion === "enter" || target?.time?.motion === "enter")
          time.motion = "enter";
        delete time.history;
        e.time = time;
      }
      this.edges.set(e.id, e);
      return e;
    });

    // Types are drawn in the hover cards of the columns using them.
    for (const t of history.types) {
      const drawn = fresh.types.get(`${t.schema}.${t.name}`);
      const chosen = drawn?.kind === t.kind ? pick(t.states, request) : undefined;
      if (drawn && chosen)
        drawn.time = markOf(t.states, chosen, `type\u0000${t.kind}:${t.schema}.${t.name}`);
    }

    const graph: GraphModel = {
      ...fresh,
      tables,
      byKey,
      edges,
      time: { eras: scale.buckets.length },
    };
    this.previous = new Map([...pictures].filter(([, p]) => p.mark.motion !== "leave"));
    // What leaves is not there any more for the next frame; what comes in is.
    for (const p of this.previous.values())
      p.columns = p.columns.filter((c) => c.mark.motion !== "leave");
    const away = { gone: 0, later: 0 };
    for (const p of pictures.values()) {
      for (const x of [p, ...p.columns]) {
        if (x.mark.motion === "leave") continue;
        if (x.presence === "gone") away.gone++;
        else if (x.presence === "later") away.later++;
      }
    }
    const frame: Frame = {
      request,
      scale,
      schema,
      graph,
      leaving,
      entering,
      counts,
      away,
    };
    this.last = frame;
    return frame;
  }

  private types(request: FrameRequest) {
    const enums: EnumType[] = [];
    const domains: DomainType[] = [];
    const composites: CompositeType[] = [];
    let sig = "";
    this.history.types.forEach((t, i) => {
      const chosen = pick(t.states, request);
      if (!chosen) return;
      sig += `${i}:${chosen.index};`;
      const value = chosen.state.value;
      if (t.kind === "enum") enums.push(value);
      else if (t.kind === "domain") domains.push(value as DomainType);
      else composites.push(value);
    });
    return { enums, domains, composites, sig };
  }
}

/** The track a table model was built from (its key is the track's id). */
function trackIdOf(t: TableModel): string {
  return t.key;
}

/** What a picture draws, as a string: equal strings draw alike. */
function signature(p: Picture): string {
  const parts = (list: PartPick<unknown>[]) =>
    list.map((x) => `${x.track}.${x.index}${x.presence[0]}${markSig(x.mark)}`).join(",");
  return [
    p.headIndex,
    p.presence,
    markSig(p.mark),
    parts(p.columns),
    parts(p.constraints),
    parts(p.indexes),
    parts(p.policies),
  ].join("|");
}

/**
 * The columns a table draws, in table order: those there at the cursor as
 * the head drawn orders them, then each of the others after the column it
 * followed when it was there (its last version for one gone by then, its
 * first for one not there yet).
 */
function orderColumns(
  track: TableTrack,
  head: State<TableHead>,
  picks: PartPick<Column>[],
): PartPick<Column>[] {
  const order = head.value.column_order ?? [];
  const byName = new Map(
    picks.filter((p) => p.presence === "here").map((p) => [p.state.value.name, p]),
  );
  const out: PartPick<Column>[] = [];
  const placed = new Set<string>();
  for (const name of order) {
    const p = byName.get(name);
    if (p && !placed.has(p.id)) {
      out.push(p);
      placed.add(p.id);
    }
  }
  // Columns there that the head does not order (a head older than them): last.
  for (const p of picks) {
    if (p.presence !== "here" || placed.has(p.id)) continue;
    out.push(p);
    placed.add(p.id);
  }
  for (const p of picks) {
    if (placed.has(p.id)) continue;
    // The head of when it was there says what it followed.
    const when = p.presence === "later" || p.state.to === null ? p.state.from : p.state.to - 1;
    const then = stateAt(track.states, when)?.value.column_order ?? [];
    const i = then.indexOf(p.state.value.name);
    let at = i < 0 ? out.length : 0;
    for (let j = i - 1; j >= 0; j--) {
      const k = out.findIndex((x) => x.state.value.name === then[j]);
      if (k >= 0) {
        at = k + 1;
        break;
      }
    }
    out.splice(at, 0, p);
    placed.add(p.id);
  }
  return out;
}

/** The entity a picture draws, every name its id, references pointed at ids. */
function entityOf(
  p: Picture,
  tableIds: Map<string, RelationRef>,
  columnIds: Map<string, Map<string, string>>,
): Entity {
  const { column_order: _order, ...head } = p.head.value;
  const own = columnIds.get(p.track.id);
  const column = (columns: Map<string, string> | undefined, name: string) =>
    columns?.get(name) ?? name;
  const constraint = (c: Constraint): Constraint => {
    const out: Constraint = { ...c, columns: c.columns?.map((n) => column(own, n)) };
    if (c.ref_schema && c.ref_table) {
      const target = tableIds.get(`${c.ref_schema}.${c.ref_table}`);
      if (target) {
        out.ref_schema = target.schema;
        out.ref_table = target.name;
        const targetTrack = `${target.schema}.${target.name}`;
        out.ref_columns = c.ref_columns?.map((n) => column(columnIds.get(targetTrack), n));
      }
    }
    return out;
  };
  return {
    ...head,
    schema: p.track.schema,
    name: idName(p.track),
    depends_on: head.depends_on?.map((r) => tableIds.get(`${r.schema}.${r.name}`) ?? r),
    columns: p.columns.map((c) => ({ ...c.state.value, name: c.id })),
    constraints: p.constraints.map((c) => ({ ...constraint(c.state.value), name: c.id })),
    indexes: p.indexes.map((i) => ({ ...i.state.value, name: i.id })),
    policies: p.policies.map((x) => ({ ...x.state.value, name: x.id })),
  };
}
