import {
  affectedEdges,
  type Point,
  type Rect,
  route,
  roundedPath,
  simplify,
  STUB,
} from "./routing";

const box = (x: number, y: number, width = 200, height = 120): Rect => ({ x, y, width, height });

function crosses(points: Point[], r: Rect): boolean {
  for (let i = 1; i < points.length; i++) {
    const a = points[i - 1]!;
    const b = points[i]!;
    if (a.y === b.y) {
      const [lo, hi] = [Math.min(a.x, b.x), Math.max(a.x, b.x)];
      if (a.y > r.y && a.y < r.y + r.height && hi > r.x && lo < r.x + r.width) return true;
    } else {
      const [lo, hi] = [Math.min(a.y, b.y), Math.max(a.y, b.y)];
      if (a.x > r.x && a.x < r.x + r.width && hi > r.y && lo < r.y + r.height) return true;
    }
  }
  return false;
}

describe("route", () => {
  test("leaves and enters sideways, facing each other", () => {
    const source = box(500, 0);
    const target = box(0, 40);
    const r = route({ id: "e", source, target, sourceY: 60, targetY: 100 }, [source, target]);
    expect(r.sourceSide).toBe("left");
    expect(r.targetSide).toBe("right");
    expect(r.points[0]).toEqual({ x: 500, y: 60 });
    expect(r.points.at(-1)).toEqual({ x: 200, y: 100 });
    // Orthogonal: every segment horizontal or vertical.
    for (let i = 1; i < r.points.length; i++) {
      const [a, b] = [r.points[i - 1]!, r.points[i]!];
      expect(a.x === b.x || a.y === b.y).toBe(true);
    }
  });

  test("goes around a table in the way rather than behind it", () => {
    const source = box(800, 0);
    const target = box(0, 300);
    const wall = box(380, 0, 200, 600);
    const r = route({ id: "e", source, target, sourceY: 60, targetY: 360 }, [source, target, wall]);
    expect(crosses(r.points, wall)).toBe(false);
  });

  test("routes stacked tables and self references out on one side", () => {
    const a = box(0, 0);
    const b = box(20, 300);
    const stacked = route({ id: "e", source: b, target: a, sourceY: 340, targetY: 40 }, [a, b]);
    expect(stacked.sourceSide).toBe(stacked.targetSide);

    const self = route({ id: "s", source: a, target: a, sourceY: 90, targetY: 40 }, [a]);
    expect(self.sourceSide).toBe("right");
    expect(self.targetSide).toBe("right");
    expect(Math.max(...self.points.map((p) => p.x))).toBeGreaterThanOrEqual(200 + STUB);
  });
});

describe("simplify", () => {
  test("drops repeated points and merges straight runs", () => {
    expect(
      simplify([
        { x: 0, y: 0 },
        { x: 0, y: 0 },
        { x: 10, y: 0 },
        { x: 20, y: 0 },
        { x: 20, y: 30 },
      ]),
    ).toEqual([
      { x: 0, y: 0 },
      { x: 20, y: 0 },
      { x: 20, y: 30 },
    ]);
  });
});

describe("roundedPath", () => {
  test("rounds each corner with a quadratic curve", () => {
    const d = roundedPath([
      { x: 0, y: 0 },
      { x: 100, y: 0 },
      { x: 100, y: 100 },
    ]);
    expect(d).toBe("M 0 0 L 90 0 Q 100 0 100 10 L 100 100");
  });

  test("never rounds past half a short segment", () => {
    const d = roundedPath([
      { x: 0, y: 0 },
      { x: 8, y: 0 },
      { x: 8, y: 100 },
    ]);
    expect(d).toBe("M 0 0 L 4 0 Q 8 0 8 4 L 8 100");
  });
});

describe("routes after a move", () => {
  /** A seeded grid of tables, edges between random pairs. */
  function scene(seed: number) {
    let state = seed;
    const rand = () => (state = (state * 1103515245 + 12345) % 2 ** 31) / 2 ** 31;
    const rects = new Map<string, Rect>();
    for (let i = 0; i < 60; i++)
      rects.set(`t${i}`, box((i % 10) * 320 + rand() * 40, Math.floor(i / 10) * 260 + rand() * 40));
    const edges = Array.from({ length: 90 }, (_, i) => ({
      id: `e${i}`,
      source: `t${Math.floor(rand() * 60)}`,
      target: `t${Math.floor(rand() * 60)}`,
    }));
    return { rects, edges, rand };
  }
  const routeOf = (e: { id: string; source: string; target: string }, rects: Map<string, Rect>) =>
    route(
      {
        id: e.id,
        source: rects.get(e.source)!,
        target: rects.get(e.target)!,
        sourceY: rects.get(e.source)!.y + 20,
        targetY: rects.get(e.target)!.y + 20,
      },
      [...rects.values()],
    );

  test("routing only the edges a move can change gives the same routes as routing all", () => {
    for (const seed of [1, 7, 42]) {
      const { rects, edges, rand } = scene(seed);
      const before = new Map(edges.map((e) => [e.id, routeOf(e, rects)]));
      // A group of tables moved together, and one moved on its own.
      const after = new Map(rects);
      for (const key of ["t11", "t12", "t21", "t22"]) {
        const r = rects.get(key)!;
        after.set(key, { ...r, x: r.x + 140, y: r.y - 90 });
      }
      const lone = rects.get("t47")!;
      after.set("t47", { ...lone, x: lone.x - 500 * rand(), y: lone.y + 300 });

      const affected = affectedEdges(edges, rects, after)!;
      expect(affected.length).toBeLessThan(edges.length);
      const incremental = new Map(before);
      for (const e of affected) incremental.set(e.id, routeOf(e, after));
      for (const e of edges) expect(incremental.get(e.id)).toEqual(routeOf(e, after));
    }
  });

  test("is nothing when nothing moved", () => {
    const { rects, edges } = scene(3);
    expect(affectedEdges(edges, rects, new Map(rects))).toBeNull();
  });
});
