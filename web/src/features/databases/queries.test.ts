import { pushCommand } from "./push-command";
import { authorName, type Database, diagnosticsOf, neighbours, wasRemoved } from "./queries";

/** v1…v6 given, v3 and v4 removed (or purged), v6 removed: v5 is the latest. */
const gaps = {
  latest: { number: 5 } as Database["latest"],
  missing: [3, 4],
  next_number: 7,
};

describe("databases", () => {
  it("steps over removed versions", () => {
    expect(neighbours(gaps, 5)).toEqual({ older: 2, newer: undefined });
    expect(neighbours(gaps, 2)).toEqual({ older: 1, newer: 5 });
    expect(neighbours(gaps, 1)).toEqual({ older: undefined, newer: 2 });
  });

  it("knows a number given to a version removed since", () => {
    expect([1, 3, 4, 5, 6, 7].map((n) => wasRemoved(gaps, n))).toEqual([
      false,
      true,
      true,
      false,
      true,
      false,
    ]);
  });

  it("reads the diagnostics of a compile_failed", () => {
    const diagnostic = { severity: "error", code: "sql", message: "nope" };
    expect(diagnosticsOf({ diagnostics: [diagnostic] })).toEqual([diagnostic]);
    expect(diagnosticsOf({ sql: "Paste the SQL" })).toEqual([]);
    expect(diagnosticsOf(undefined)).toEqual([]);
  });

  it("names a deleted author", () => {
    expect(authorName(null)).toBe("Deleted account");
    expect(authorName({ kind: "user", id: "u", name: "Alice" })).toBe("Alice");
  });

  it("pushes with the real database id and the page's origin", () => {
    const command = pushCommand("0193-db", "https://romulus.example");
    expect(command).toContain('Authorization: Bearer $ROMULUS_TOKEN"');
    expect(command).toContain("https://romulus.example/api/databases/0193-db/versions");
    expect(command.startsWith("remus -f json | curl")).toBe(true);
  });
});
