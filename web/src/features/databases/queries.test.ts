import { pushCommand } from "./push-command";
import { authorName, diagnosticsOf } from "./queries";

describe("databases", () => {
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
