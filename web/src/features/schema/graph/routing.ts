// Orthogonal routes for relationship edges that go around tables, not behind
// them. An edge leaves its column's row sideways and enters the other's the
// same way; between the two it takes the cheapest of a few shapes:
//
//   3 segments  ──┐          5 segments  ──┐
//                 └──                      └────┐   (through a free corridor)
//                                               └──
//
// Cost is crossings first, then length and bends, so the router prefers a
// detour to a line drawn through a table. Pure: the graph recomputes routes
// when positions change, and tests check them without a DOM.

export interface Point {
  x: number;
  y: number;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type Side = "left" | "right";

export interface RouteRequest {
  id: string;
  source: Rect;
  target: Rect;
  /** Absolute y of the anchors (the column rows). */
  sourceY: number;
  targetY: number;
}

export interface Route {
  points: Point[];
  sourceSide: Side;
  targetSide: Side;
}

/** Straight run out of a node before any bend: room for the crow's foot. */
export const STUB = 26;
/** Clearance kept between a route and the tables it passes. */
const CLEARANCE = 14;
const BEND = 24;
const CROSSING = 100_000;
/** Obstacles further than this from an edge's box are not considered. */
export const REACH = 320;

const right = (r: Rect) => r.x + r.width;
const bottom = (r: Rect) => r.y + r.height;

/** Strict interior hit of an axis-aligned segment and a rect grown by `pad`. */
function hits(a: Point, b: Point, r: Rect, pad: number): boolean {
  const x0 = r.x - pad;
  const x1 = right(r) + pad;
  const y0 = r.y - pad;
  const y1 = bottom(r) + pad;
  if (a.y === b.y) {
    const lo = Math.min(a.x, b.x);
    const hi = Math.max(a.x, b.x);
    return a.y > y0 && a.y < y1 && hi > x0 && lo < x1;
  }
  const lo = Math.min(a.y, b.y);
  const hi = Math.max(a.y, b.y);
  return a.x > x0 && a.x < x1 && hi > y0 && lo < y1;
}

/** `own` are the edge's two tables: leaving or touching them is fine, crossing them is not. */
function cost(points: Point[], obstacles: Rect[], own: Rect[]): number {
  let total = 0;
  for (let i = 1; i < points.length; i++) {
    const a = points[i - 1]!;
    const b = points[i]!;
    total += Math.abs(a.x - b.x) + Math.abs(a.y - b.y);
    for (const o of obstacles) if (hits(a, b, o, CLEARANCE / 2)) total += CROSSING;
    for (const o of own) if (hits(a, b, o, -1)) total += CROSSING;
  }
  return total + (points.length - 2) * BEND;
}

/** Drops zero-length segments and merges collinear runs. */
export function simplify(points: Point[]): Point[] {
  const out: Point[] = [];
  for (const p of points) {
    const last = out[out.length - 1];
    if (last && last.x === p.x && last.y === p.y) continue;
    const prev = out[out.length - 2];
    if (
      prev &&
      last &&
      ((prev.x === last.x && last.x === p.x) || (prev.y === last.y && last.y === p.y))
    ) {
      out[out.length - 1] = p;
      continue;
    }
    out.push(p);
  }
  return out;
}

function sides(source: Rect, target: Rect, selfLoop: boolean): [Side, Side] {
  if (!selfLoop && right(source) + STUB * 2 <= target.x) return ["right", "left"];
  if (!selfLoop && right(target) + STUB * 2 <= source.x) return ["left", "right"];
  // Overlapping horizontally (stacked, or the same table): both on the side
  // nearer the other's center, the right one for a self reference.
  const leftward = !selfLoop && source.x + source.width / 2 > target.x + target.width / 2;
  return leftward ? ["left", "left"] : ["right", "right"];
}

export function route(request: RouteRequest, all: readonly Rect[]): Route {
  const { source, target, sourceY, targetY } = request;
  const selfLoop = source === target;
  const [sourceSide, targetSide] = sides(source, target, selfLoop);
  const S: Point = { x: sourceSide === "right" ? right(source) : source.x, y: sourceY };
  const T: Point = { x: targetSide === "right" ? right(target) : target.x, y: targetY };
  const ds = sourceSide === "right" ? 1 : -1;
  const dt = targetSide === "right" ? 1 : -1;
  const S1 = { x: S.x + ds * STUB, y: S.y };
  const T1 = { x: T.x + dt * STUB, y: T.y };

  const box = {
    x0: Math.min(S.x, T.x) - REACH,
    x1: Math.max(S.x, T.x) + REACH,
    y0: Math.min(S.y, T.y) - REACH,
    y1: Math.max(S.y, T.y) + REACH,
  };
  const own = selfLoop ? [source] : [source, target];
  const obstacles = all.filter(
    (r) =>
      r !== source &&
      r !== target &&
      right(r) > box.x0 &&
      r.x < box.x1 &&
      bottom(r) > box.y0 &&
      r.y < box.y1,
  );

  // Channels: x positions just outside the obstacles' sides.
  const xs: number[] = [];
  for (const o of [...obstacles, ...own]) xs.push(o.x - CLEARANCE, right(o) + CLEARANCE);

  let best: Point[] = [];
  let bestCost = Infinity;
  const consider = (points: Point[]) => {
    const c = cost(points, obstacles, own);
    if (c < bestCost) {
      best = points;
      bestCost = c;
    }
  };

  // 3 segments, through one vertical channel.
  const channels = new Set<number>();
  if (sourceSide !== targetSide) {
    const lo = Math.min(S1.x, T1.x);
    const hi = Math.max(S1.x, T1.x);
    channels.add(Math.round((S1.x + T1.x) / 2));
    channels.add(S1.x);
    channels.add(T1.x);
    for (const x of xs) if (x > lo && x < hi) channels.add(x);
  } else {
    const edge = ds > 0 ? Math.max(S1.x, T1.x) : Math.min(S1.x, T1.x);
    channels.add(edge);
    for (const x of xs) if (ds > 0 ? x > edge : x < edge) channels.add(x);
  }
  for (const x of channels) consider([S, { x, y: S.y }, { x, y: T.y }, T]);

  // 5 segments, through a horizontal corridor, when every channel crosses something.
  if (bestCost >= CROSSING) {
    const outward = (from: number, d: number) =>
      [from, ...xs.filter((x) => (d > 0 ? x > from : x < from))]
        .sort((a, b) => Math.abs(a - from) - Math.abs(b - from))
        .slice(0, 3);
    const x1s = outward(S1.x, ds);
    const x2s = outward(T1.x, dt);
    const mid = (S.y + T.y) / 2;
    const ys = [S.y, T.y];
    for (const o of [...obstacles, ...own]) ys.push(o.y - CLEARANCE, bottom(o) + CLEARANCE);
    const corridors = ys.sort((a, b) => Math.abs(a - mid) - Math.abs(b - mid)).slice(0, 16);
    for (const x1 of x1s) {
      for (const x2 of x2s) {
        for (const y of corridors) {
          consider([S, { x: x1, y: S.y }, { x: x1, y }, { x: x2, y }, { x: x2, y: T.y }, T]);
        }
      }
    }
  }
  return { points: simplify(best), sourceSide, targetSide };
}

/** An SVG path through `points` with rounded corners. */
export function roundedPath(points: Point[], radius = 10): string {
  if (points.length < 2) return "";
  let d = `M ${points[0]!.x} ${points[0]!.y}`;
  for (let i = 1; i < points.length - 1; i++) {
    const prev = points[i - 1]!;
    const p = points[i]!;
    const next = points[i + 1]!;
    const r = Math.min(
      radius,
      Math.hypot(p.x - prev.x, p.y - prev.y) / 2,
      Math.hypot(next.x - p.x, next.y - p.y) / 2,
    );
    const inX = p.x - Math.sign(p.x - prev.x) * r;
    const inY = p.y - Math.sign(p.y - prev.y) * r;
    const outX = p.x + Math.sign(next.x - p.x) * r;
    const outY = p.y + Math.sign(next.y - p.y) * r;
    d += ` L ${inX} ${inY} Q ${p.x} ${p.y} ${outX} ${outY}`;
  }
  const last = points[points.length - 1]!;
  return `${d} L ${last.x} ${last.y}`;
}

const overlaps = (a: Rect, b: Rect) =>
  a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;

/**
 * The edges whose routes can differ once tables moved from `before` to
 * `after`: those of a moved table, and those with a moved table (where it
 * was or where it is) within reach of their two ends, as `route` only looks
 * that far. Every other route is the same; null when nothing moved.
 */
export function affectedEdges<E extends { source: string; target: string }>(
  edges: readonly E[],
  before: ReadonlyMap<string, Rect>,
  after: ReadonlyMap<string, Rect>,
): E[] | null {
  const moved = new Set<string>();
  const places: Rect[] = [];
  const same = (a: Rect, b: Rect) =>
    a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height;
  for (const [key, r] of after) {
    const old = before.get(key);
    if (old && same(old, r)) continue;
    moved.add(key);
    places.push(r);
    if (old) places.push(old);
  }
  for (const [key, old] of before)
    if (!after.has(key)) {
      moved.add(key);
      places.push(old);
    }
  if (moved.size === 0) return null;
  return edges.filter((e) => {
    if (moved.has(e.source) || moved.has(e.target)) return true;
    const a = after.get(e.source);
    const b = after.get(e.target);
    if (!a || !b) return false;
    const x = Math.min(a.x, b.x) - REACH;
    const y = Math.min(a.y, b.y) - REACH;
    const reach = {
      x,
      y,
      width: Math.max(right(a), right(b)) + REACH - x,
      height: Math.max(bottom(a), bottom(b)) + REACH - y,
    };
    return places.some((p) => overlaps(p, reach));
  });
}
