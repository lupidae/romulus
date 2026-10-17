import { compiledOn, majorDescription, pushedFromAnotherMajor } from "./postgres";
import type { Database, VersionSummary } from "./queries";

const postgres = (major: number, database_major: number, other_major = false) => ({
  server_version: `${major}.4 (Debian ${major}.4-1)`,
  major,
  database_major,
  other_major,
});

const database = (pg_major: Database["pg_major"], latest: Database["latest"]): Database => ({
  id: "d",
  slug: "shop",
  name: "Shop",
  description: "",
  pg_major,
  created_at: "2026-10-15T10:00:00Z",
  updated_at: "2026-10-15T10:00:00Z",
  latest,
  versions: 1,
  next_number: 2,
  missing: [],
  last_activity_at: "2026-10-15T10:00:00Z",
});

const pushed = (summary: Partial<VersionSummary>): VersionSummary => ({
  id: "v",
  number: 4,
  origin: "push",
  source_kind: "remus_json",
  message: null,
  created_at: "2026-10-15T10:00:00Z",
  author: null,
  written_migration: false,
  postgres: postgres(17, 17),
  ...summary,
});

describe("majors", () => {
  it("says what this server compiles, and what to know about a major", () => {
    expect(majorDescription(17)).toBe("Default");
    expect(
      majorDescription(17, {
        major: 17,
        default: true,
        status: "ok",
        server_version: "17.11 (Debian)",
      }),
    ).toBe("Default · 17.11");
    expect(majorDescription(18)).toBe("syntax new in 18 is not read yet");
    expect(majorDescription(15, { major: 15, default: false, status: "off" })).toBe(
      "Not available on this server",
    );
    expect(majorDescription(16, { major: 16, default: false, status: "unavailable" })).toBe(
      "Its compiler is down right now",
    );
  });

  it("names where a version was compiled, or pushed from", () => {
    expect(compiledOn(postgres(17, 17), false)).toBe("Compiled on Postgres 17.4");
    expect(compiledOn(postgres(16, 17), true)).toBe("Pushed from Postgres 16.4");
  });

  it("offers to follow the server only while the latest push came from another major", () => {
    const latest = pushed({ postgres: postgres(16, 17, true) });
    expect(pushedFromAnotherMajor(database(17, latest))).toMatchObject({ number: 4, major: 16 });
    // Already moved, an older push, or a major romulus does not compile on.
    expect(pushedFromAnotherMajor(database(16, latest))).toBeNull();
    expect(pushedFromAnotherMajor(database(17, pushed({})))).toBeNull();
    expect(
      pushedFromAnotherMajor(database(17, pushed({ postgres: postgres(14, 17, true) }))),
    ).toBeNull();
    expect(pushedFromAnotherMajor(database(17, null))).toBeNull();
  });
});
