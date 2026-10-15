import { describe, expect, it } from "vitest";

import { eventPatch, mayEdit, type SessionState } from "./session";

const initial: SessionState = {
  connection: "connected",
  synced: true,
  role: null,
  status: "open",
  statusBy: null,
  publishedVersion: null,
  name: null,
  compile: { state: "ok", stale: false, diagnostics: [], model: null },
  versionCreated: null,
  deleted: false,
  signedOut: false,
  tooLarge: false,
  saveFailed: false,
  unsynced: false,
};

const apply = (state: SessionState, ...events: Parameters<typeof eventPatch>[1][]) =>
  events.reduce((s, e) => ({ ...s, ...eventPatch(s, e) }), state);

describe("a draft session's state", () => {
  it("lets someone who joined during a publish edit once it failed", () => {
    const joined = apply(initial, {
      type: "welcome",
      role: "developer",
      read_only: true,
      status: "publishing",
    });
    expect(mayEdit(joined)).toBe(false);
    const reopened = apply(joined, { type: "status", status: "open", by: null, version: null });
    expect(mayEdit(reopened)).toBe(true);
  });

  it("never lets a reviewer edit, open or not", () => {
    const reviewer = apply(initial, {
      type: "welcome",
      role: "reviewer",
      read_only: true,
      status: "open",
    });
    expect(mayEdit(reviewer)).toBe(false);
  });

  it("closes for good when published, and keeps who did it", () => {
    const published = apply(
      initial,
      { type: "welcome", role: "admin", read_only: false, status: "open" },
      { type: "status", status: "published", by: "Alice", version: 4 },
    );
    expect(mayEdit(published)).toBe(false);
    expect(published).toMatchObject({ statusBy: "Alice", publishedVersion: 4 });
  });

  it("says when the server could not save", () => {
    expect(apply(initial, { type: "save_failed" }).saveFailed).toBe(true);
  });
});
