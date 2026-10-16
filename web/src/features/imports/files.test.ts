import { fromInput, sendable, tooMuch } from "./files";

function file(path: string, text = "x") {
  const f = new File([text], path.split("/").pop() ?? path);
  Object.defineProperty(f, "webkitRelativePath", { value: path });
  return f;
}

describe("the files sent with an import", () => {
  it("are SQL, zips and Prisma's lock file, never dependencies", () => {
    expect(sendable("migrations/0001_init.sql")).toBe(true);
    expect(sendable("migrations.zip")).toBe(true);
    expect(sendable("prisma/migrations/migration_lock.toml")).toBe(true);
    expect(sendable("migrations/README.md")).toBe(false);
    expect(sendable("app/node_modules/pkg/schema.sql")).toBe(false);
    expect(sendable(".git/x.sql")).toBe(false);
  });

  it("are named after the folder picked, and counted when left out", () => {
    const picked = fromInput([
      file("migrations/0001_init.sql", "CREATE TABLE a ();"),
      file("migrations/0002_b.sql"),
      file("migrations/notes.md"),
    ]);
    expect(picked.label).toBe("migrations");
    expect(picked.files.map((f) => f.path)).toEqual([
      "migrations/0001_init.sql",
      "migrations/0002_b.sql",
    ]);
    expect(picked.left).toBe(1);
    expect(picked.bytes).toBe(19);
  });

  it("say why they cannot be sent", () => {
    expect(tooMuch(fromInput([file("docs/notes.md")]))).toMatch(/No .sql file in docs/);
    expect(tooMuch(fromInput([file("a.sql")]))).toBeNull();
  });
});
