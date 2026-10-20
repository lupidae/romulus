import { describe, expect, it } from "vitest";

import type { Playground } from "@/features/playground/queries";

import { choiceDetail, choiceLabel, rowsChoices } from "./rows";

function playground(over: Partial<Playground>): Playground {
  return {
    id: "p",
    status: "ready",
    draft: null,
    version: 7,
    size: "small",
    profile: "realistic",
    seed: "7f3a91",
    generator: "1",
    tables: [{ schema: "public", name: "orders", rows: 2400 }],
    report: null,
    failure: null,
    progress: null,
    live: null,
    present: [],
    migrated_by: null,
    created_by: { id: "alice", name: "Alice" },
    created_at: "2026-10-20T10:00:00Z",
    ready_at: "2026-10-20T10:01:00Z",
    last_used_at: "2026-10-20T10:01:00Z",
    expires_at: "2026-10-27T10:01:00Z",
    ...over,
  } as Playground;
}

describe("the rows a test runs on", () => {
  it("defaults to a copy of the person's playground at the base, the draft's own first", () => {
    const choices = rowsChoices(
      [
        playground({ id: "bob", created_by: { id: "bob", name: "Bob" } }),
        playground({ id: "mine" }),
        playground({ id: "draft", draft: { id: "d1" } as Playground["draft"] }),
      ],
      7,
      "alice",
      "d1",
    );
    expect(choices.map((c) => (c.kind === "fresh" ? "fresh" : c.playground.id))).toEqual([
      "draft",
      "mine",
      "bob",
      "fresh",
    ]);
    expect(choiceLabel(choices[0]!)).toBe("Your playground");
    expect(choiceLabel(choices[2]!)).toBe("Bob's playground");
    expect(choiceDetail(choices[0]!)).toBe("Small · 2,400 rows · seed 7f3a91");
  });

  it("leaves out playgrounds at another version, migrated or not ready", () => {
    const choices = rowsChoices(
      [
        playground({ id: "older", version: 6 }),
        playground({
          id: "migrated",
          migrated_by: { id: "r" } as Playground["migrated_by"],
        }),
        playground({ id: "building", status: "building" }),
      ],
      7,
      "alice",
    );
    expect(choices).toEqual([{ kind: "fresh" }]);
    expect(rowsChoices([playground({})], null, "alice")).toEqual([{ kind: "fresh" }]);
  });
});
