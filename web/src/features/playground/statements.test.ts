import { describe, expect, it } from "vitest";

import { statementAt, statements } from "./statements";

describe("statements", () => {
  it("splits on semicolons outside quotes, dollar quotes and comments", () => {
    const text = `SELECT 'a;b' AS x; -- c;d
SELECT $$ e;f $$; /* g; */ SELECT "h;i" FROM t`;
    expect(statements(text).map((p) => p.text)).toEqual([
      "SELECT 'a;b' AS x",
      "-- c;d\nSELECT $$ e;f $$",
      '/* g; */ SELECT "h;i" FROM t',
    ]);
  });

  it("drops what holds only comments", () => {
    expect(statements("-- nothing;\n  ;  ")).toEqual([]);
  });

  it("keeps the offsets of each statement", () => {
    const text = "SELECT 1;\n\nSELECT 2;";
    const second = statements(text)[1];
    expect(second && text.slice(second.from, second.to)).toBe("SELECT 2");
  });
});

describe("statementAt", () => {
  const text = "SELECT 1;\nSELECT 2;\n";

  it("finds the statement under the cursor", () => {
    expect(statementAt(text, 3)?.text).toBe("SELECT 1");
    expect(statementAt(text, 14)?.text).toBe("SELECT 2");
  });

  it("takes the statement just before a cursor after it", () => {
    expect(statementAt(text, text.length)?.text).toBe("SELECT 2");
  });

  it("finds nothing in an empty text", () => {
    expect(statementAt("  ", 0)).toBeNull();
  });
});
