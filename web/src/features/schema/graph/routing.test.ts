import { type Point, type Rect, route, roundedPath, simplify, STUB } from "./routing";

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
