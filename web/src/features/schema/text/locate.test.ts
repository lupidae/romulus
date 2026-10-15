import showcaseDbml from "../fixtures/showcase.dbml?raw";
import showcase from "../fixtures/showcase.json";
import showcaseMermaid from "../fixtures/showcase.mmd?raw";
import { locateInDbml, locateInJson, locateInMermaid, mermaidNode } from "./locate";

const text = (source: string, range: { from: number; to: number } | undefined) =>
  range ? source.slice(range.from, range.to) : undefined;

describe("locating a table in the other renderings", () => {
  const orders = { schema: "shop", name: "orders" };

  test("JSON: the entity's whole object", () => {
    const json = JSON.stringify(showcase, null, 2);
    const found = text(json, locateInJson(json, orders))!;
    expect(found.startsWith("{")).toBe(true);
    expect(found.endsWith("}")).toBe(true);
    expect(JSON.parse(found)).toMatchObject({ schema: "shop", name: "orders", kind: "r" });
  });

  test("DBML: from `Table` to its closing brace", () => {
    const found = text(showcaseDbml, locateInDbml(showcaseDbml, orders))!;
    expect(found.startsWith('Table "shop"."orders" {')).toBe(true);
    expect(found.trimEnd().endsWith("}")).toBe(true);
    expect(found).not.toContain("\nTable ");
  });

  test("Mermaid: the node block, named as remus-mermaid names it", () => {
    expect(mermaidNode(orders, true)).toBe("shop_orders");
    expect(mermaidNode({ schema: "s", name: "9 lives" }, false)).toBe("_9_lives");
    const found = text(showcaseMermaid, locateInMermaid(showcaseMermaid, orders, true))!;
    expect(found.trimStart().startsWith("shop_orders {")).toBe(true);
    expect(found.trimEnd().endsWith("}")).toBe(true);
  });

  test("an unknown table is not found", () => {
    const missing = { schema: "shop", name: "nope" };
    expect(locateInJson(JSON.stringify(showcase), missing)).toBeUndefined();
    expect(locateInDbml(showcaseDbml, missing)).toBeUndefined();
    expect(locateInMermaid(showcaseMermaid, missing, true)).toBeUndefined();
  });
});
