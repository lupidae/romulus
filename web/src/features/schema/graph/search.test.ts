import { mediumFixture } from "../fixtures/synthetic";
import { buildGraph } from "./build";
import { nearestInDirection, nearestTo } from "./navigation";
import { searchTables } from "./search";

const { tables } = buildGraph({ schema: mediumFixture().schema });
const names = (query: string, limit?: number) =>
  searchTables(tables, query, limit).map((h) =>
    h.column ? `${h.table.name}.${h.column}` : h.table.name,
  );

describe("searchTables", () => {
  test("ranks an exact name, then prefixes (shortest first), then word starts", () => {
    expect(names("orders").slice(0, 1)).toEqual(["orders"]);
    expect(names("ord").slice(0, 2)).toEqual(["orders", "order_items"]);
    expect(names("items")[0]).toBe("cart_items");
  });

  test("finds names from their letters in order", () => {
    expect(names("prdvar")).toContain("product_variants");
  });

  test("lists matching columns after the tables, as table.column", () => {
    const hits = names("email");
    expect(hits).toContain("users.email");
    expect(hits.indexOf("users.email")).toBeGreaterThan(-1);
    expect(names("users.ema")).toEqual(["users.email"]);
  });

  test("an empty query lists tables, and nothing matches nonsense", () => {
    expect(names("", 5)).toHaveLength(5);
    expect(names("zzzz")).toEqual([]);
  });

  test("marks the matched part for highlighting", () => {
    const hit = searchTables(tables, "voice")[0]!;
    expect(hit.table.name).toBe("invoices");
    expect(hit.table.name.slice(...hit.match!)).toBe("voice");
  });
});

describe("arrow-key navigation", () => {
  const boxes = [
    { id: "center", x: 400, y: 400, width: 200, height: 100 },
    { id: "right", x: 700, y: 410, width: 200, height: 100 },
    { id: "right-far-off-axis", x: 650, y: 900, width: 200, height: 100 },
    { id: "left", x: 100, y: 380, width: 200, height: 100 },
    { id: "up", x: 420, y: 100, width: 200, height: 100 },
  ];

  test("moves to the nearest table in line with the arrow", () => {
    const from = boxes[0]!;
    expect(nearestInDirection(from, boxes, "right")?.id).toBe("right");
    expect(nearestInDirection(from, boxes, "left")?.id).toBe("left");
    expect(nearestInDirection(from, boxes, "up")?.id).toBe("up");
    expect(nearestInDirection(from, boxes, "down")?.id).toBe("right-far-off-axis");
  });

  test("stays put at the edge", () => {
    expect(nearestInDirection(boxes[1]!, boxes, "right")).toBeUndefined();
  });

  test("starts from the table nearest a point", () => {
    expect(nearestTo({ x: 160, y: 400 }, boxes)?.id).toBe("left");
  });
});
