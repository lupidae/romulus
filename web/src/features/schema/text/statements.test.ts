import showcaseSql from "../fixtures/showcase.sql?raw";
import showcase from "../fixtures/showcase.json";
import { parseSchema, qualifiedName } from "../model";
import { findStatements, statementAt } from "./statements";

const at = (sql: string, s: { from: number; to: number }) => sql.slice(s.from, s.to);

describe("findStatements", () => {
  test("splits on top-level semicolons, ranges excluding comments before", () => {
    const sql = "-- users\nCREATE TABLE users (id int);\n\n/* two */ CREATE TABLE b.t (x int);";
    const [first, second] = findStatements(sql);
    expect(at(sql, first!)).toBe("CREATE TABLE users (id int);");
    expect(first).toMatchObject({ kind: "create", target: { schema: "public", name: "users" } });
    expect(at(sql, second!)).toBe("CREATE TABLE b.t (x int);");
    expect(second!.target).toEqual({ schema: "b", name: "t" });
  });

  test("does not split inside strings, quoted names, dollar quotes or comments", () => {
    const sql = [
      "COMMENT ON TABLE t IS 'a; b';",
      'CREATE TABLE "we;ird" (x int);',
      "COMMENT ON COLUMN t.x IS $$c; d$$;",
      "CREATE TABLE s.u (x text DEFAULT E'it\\'s; ok' /* ; */); -- ; trailing",
    ].join("\n");
    const statements = findStatements(sql);
    expect(statements).toHaveLength(4);
    expect(statements[1]!.target).toEqual({ schema: "public", name: "we;ird" });
    expect(statements[3]!.target).toEqual({ schema: "s", name: "u" });
  });

  test("folds unquoted names to lower case and keeps quoted ones", () => {
    const [unquoted, quoted] = findStatements(
      'CREATE TABLE Shop.Orders (x int); CREATE TABLE "Shop"."Or""ders" (x int);',
    );
    expect(unquoted!.target).toEqual({ schema: "shop", name: "orders" });
    expect(quoted!.target).toEqual({ schema: "Shop", name: 'Or"ders' });
  });

  test("reads which relation each kind of statement is about", () => {
    const sql = [
      "CREATE UNLOGGED TABLE IF NOT EXISTS a.t1 (x int);",
      "CREATE OR REPLACE VIEW a.v AS SELECT 1;",
      "CREATE MATERIALIZED VIEW a.m AS SELECT 1;",
      "CREATE TABLE a.p1 PARTITION OF a.t1 FOR VALUES IN (1);",
      "ALTER TABLE ONLY a.t1 ADD CONSTRAINT fk FOREIGN KEY (x) REFERENCES a.t2(id);",
      "CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS i ON ONLY a.t1 USING btree (x);",
      "CREATE INDEX ON a.t2 (y);",
      "CREATE POLICY p ON a.t1 USING (true);",
      "COMMENT ON COLUMN a.t1.x IS 'x';",
      "COMMENT ON CONSTRAINT fk ON a.t1 IS 'fk';",
      "COMMENT ON MATERIALIZED VIEW a.m IS 'm';",
      "CREATE TYPE a.mood AS ENUM ('ok');",
      "CREATE EXTENSION pgcrypto;",
    ].join("\n");
    const summary = findStatements(sql).map(
      (s) => `${s.kind} ${s.target ? qualifiedName(s.target) : "-"}`,
    );
    expect(summary).toEqual([
      "create a.t1",
      "create a.v",
      "create a.m",
      "create a.p1",
      "alter a.t1",
      "index a.t1",
      "index a.t2",
      "policy a.t1",
      "comment a.t1",
      "comment a.t1",
      "comment a.m",
      "type a.mood",
      "other -",
    ]);
    expect(findStatements(sql)[3]!.partitionOf).toEqual({ schema: "a", name: "t1" });
  });

  test("finds the CREATE of every entity in remus-sql's output", () => {
    const statements = findStatements(showcaseSql);
    const created = new Set(
      statements
        .filter((s) => s.kind === "create" && !s.partitionOf)
        .map((s) => qualifiedName(s.target!)),
    );
    for (const e of parseSchema(showcase).entities ?? [])
      expect(created).toContain(qualifiedName(e));
  });

  test("statementAt finds the statement holding an offset", () => {
    const sql = "CREATE TABLE a (x int);\n\nCREATE TABLE b (y int);";
    const statements = findStatements(sql);
    expect(statementAt(statements, sql.indexOf("x int"))?.target?.name).toBe("a");
    expect(statementAt(statements, sql.indexOf("y int"))?.target?.name).toBe("b");
    expect(statementAt(statements, sql.indexOf("\n\n") + 1)).toBeUndefined();
  });
});
