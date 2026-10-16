import { migrationFileName } from "./migration-file";

describe("a written migration's file name", () => {
  test("is the draft's name, file-name safe", () => {
    expect(migrationFileName("Migration from v3")).toBe("migration_from_v3.sql");
    expect(migrationFileName("Ajout des réductions !")).toBe("ajout_des_reductions.sql");
    expect(migrationFileName("¡¿")).toBe("migration.sql");
  });
});
