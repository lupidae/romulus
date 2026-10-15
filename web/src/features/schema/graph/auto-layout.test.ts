import ELK from "elkjs/lib/elk.bundled.js";

import blog from "../fixtures/blog.json";
import { largeFixture, mediumFixture } from "../fixtures/synthetic";
import type { Layout } from "../layout";
import { parseSchema, type Schema } from "../model";
import {
  clusters,
  type LayoutEdge,
  type LayoutNode,
  type LayoutRequest,
  placeMissing,
} from "./auto-layout";
import { buildGraph } from "./build";
import { tableSize } from "./geometry";

// elkjs in-process (the app runs the same algorithm in a web worker).
const elk = new ELK();

function request(
  schema: Schema,
  positions: Layout = {},
  groups?: Record<string, string[]>,
): LayoutRequest {
  const graph = buildGraph({ schema });
  return {
    nodes: graph.tables.map((t) => ({
      id: t.key,
      ...tableSize(t, graph.multiSchema),
      partition: graph.multiSchema ? t.schema : undefined,
    })),
    edges: graph.edges.map((e) => ({ source: e.source, target: e.target })),
    positions,
    groups,
  };
}

function overlapping(nodes: LayoutNode[], layout: Layout): string[] {
  const out: string[] = [];
  nodes.forEach((a, i) => {
    const p = layout[a.id]!;
    for (const b of nodes.slice(i + 1)) {
      const q = layout[b.id]!;
      if (
        p.x < q.x + b.width &&
        q.x < p.x + a.width &&
        p.y < q.y + b.height &&
        q.y < p.y + a.height
      ) {
        out.push(`${a.id} / ${b.id}`);
      }
    }
  });
  return out;
}

const center = (n: LayoutNode, l: Layout) => ({
  x: l[n.id]!.x + n.width / 2,
  y: l[n.id]!.y + n.height / 2,
});

describe("placeMissing", () => {
  test("lays out every table the first time, without overlaps", async () => {
    const req = request(parseSchema(blog));
    const layout = await placeMissing(req, elk);
    expect(Object.keys(layout).sort()).toEqual(req.nodes.map((n) => n.id).sort());
    expect(overlapping(req.nodes, layout)).toEqual([]);
  });

  test("is deterministic", async () => {
    const req = request(mediumFixture().schema);
    expect(await placeMissing(req, elk)).toEqual(
      await placeMissing(request(mediumFixture().schema), elk),
    );
  });

  test("puts referenced tables left of the tables referencing them", async () => {
    const req = request(parseSchema(blog));
    const layout = await placeMissing(req, elk);
    expect(layout["public.posts"]!.x).toBeLessThan(layout["public.comments"]!.x);
  });

  test("keeps 300 tables in a screen-shaped area", async () => {
    const req = { ...request(largeFixture()), aspectRatio: 1.6 };
    const layout = await placeMissing(req, elk);
    let width = 0;
    let height = 0;
    for (const n of req.nodes) {
      width = Math.max(width, layout[n.id]!.x + n.width);
      height = Math.max(height, layout[n.id]!.y + n.height);
    }
    expect(overlapping(req.nodes, layout)).toEqual([]);
    // Screen-shaped, never a strip: six schema blocks pack about square.
    expect(width / height).toBeGreaterThan(0.7);
    expect(width / height).toBeLessThan(3);
  });

  test("leaves room for each schema's area, so areas never overlap", async () => {
    const req = request(largeFixture(120));
    const layout = await placeMissing(req, elk);
    const size = new Map(req.nodes.map((n) => [n.id, n]));
    const pad = { top: 60, side: 24, bottom: 24 };
    const areas = [...new Set(req.nodes.map((n) => n.partition))].map((schema) => {
      const ids = req.nodes.filter((n) => n.partition === schema).map((n) => n.id);
      const xs = ids.flatMap((id) => [layout[id]!.x, layout[id]!.x + size.get(id)!.width]);
      const ys = ids.flatMap((id) => [layout[id]!.y, layout[id]!.y + size.get(id)!.height]);
      return {
        x0: Math.min(...xs) - pad.side,
        x1: Math.max(...xs) + pad.side,
        y0: Math.min(...ys) - pad.top,
        y1: Math.max(...ys) + pad.bottom,
      };
    });
    areas.forEach((a, i) =>
      areas.slice(i + 1).forEach((b) => {
        expect(a.x1 <= b.x0 || b.x1 <= a.x0 || a.y1 <= b.y0 || b.y1 <= a.y0).toBe(true);
      }),
    );
  });

  test("keeps each group's tables together", async () => {
    const { schema, groups } = mediumFixture();
    const members = Object.fromEntries(Object.entries(groups).map(([id, g]) => [id, g.members]));
    const req = request(schema, {}, members);
    const layout = await placeMissing(req, elk);
    const size = new Map(req.nodes.map((n) => [n.id, n]));
    const boxes = Object.values(members).map((ids) => {
      const xs = ids.flatMap((id) => [layout[id]!.x, layout[id]!.x + size.get(id)!.width]);
      const ys = ids.flatMap((id) => [layout[id]!.y, layout[id]!.y + size.get(id)!.height]);
      return { x0: Math.min(...xs), x1: Math.max(...xs), y0: Math.min(...ys), y1: Math.max(...ys) };
    });
    boxes.forEach((a, i) =>
      boxes.slice(i + 1).forEach((b) => {
        const apart = a.x1 <= b.x0 || b.x1 <= a.x0 || a.y1 <= b.y0 || b.y1 <= a.y0;
        expect(apart).toBe(true);
      }),
    );
  });

  test("places only the missing tables, next to what they reference", async () => {
    const full = request(parseSchema(blog));
    const first = await placeMissing(full, elk);
    const { "public.comments": _, ...known } = first;
    const placed = await placeMissing({ ...full, positions: known }, elk);
    expect(Object.keys(placed)).toEqual(["public.comments"]);

    const layout = { ...known, ...placed };
    expect(overlapping(full.nodes, layout)).toEqual([]);
    // Nearer to posts (which it references) than the farthest table is.
    const byId = new Map(full.nodes.map((n) => [n.id, n]));
    const c = center(byId.get("public.comments")!, layout);
    const dist = (id: string) => {
      const p = center(byId.get(id)!, layout);
      return Math.hypot(p.x - c.x, p.y - c.y);
    };
    const farthest = Math.max(
      ...full.nodes.filter((n) => n.id !== "public.comments").map((n) => dist(n.id)),
    );
    expect(dist("public.posts")).toBeLessThan(farthest);
  });

  test("returns nothing when every table has a place", async () => {
    const full = request(parseSchema(blog));
    const layout = await placeMissing(full, elk);
    expect(await placeMissing({ ...full, positions: layout }, elk)).toEqual({});
  });

  test("puts a table with no relation under the diagram", async () => {
    const nodes: LayoutNode[] = [
      { id: "a", width: 200, height: 100 },
      { id: "b", width: 200, height: 100 },
      { id: "lonely", width: 200, height: 100 },
    ];
    const edges: LayoutEdge[] = [{ source: "b", target: "a" }];
    const placed = await placeMissing(
      { nodes, edges, positions: { a: { x: 0, y: 0 }, b: { x: 300, y: 0 } } },
      elk,
    );
    expect(placed.lonely!.y).toBeGreaterThan(100);
  });
});

describe("clusters", () => {
  test("splits a big component into pieces no bigger than fourteen", () => {
    const ids = Array.from({ length: 40 }, (_, i) => `t${String(i).padStart(2, "0")}`);
    // A hub referenced by every table, and pairs referencing each other.
    const edges: LayoutEdge[] = ids.slice(1).map((id) => ({ source: id, target: "t00" }));
    for (let i = 1; i < 39; i += 2) edges.push({ source: ids[i + 1]!, target: ids[i]! });
    const parts = clusters(ids, edges);
    expect(parts.flat().sort()).toEqual(ids);
    expect(Math.max(...parts.map((p) => p.length))).toBeLessThanOrEqual(14);
  });

  test("keeps a small connected set whole", () => {
    expect(clusters(["a", "b", "c"], [{ source: "b", target: "a" }])).toEqual([["a", "b"], ["c"]]);
  });
});
