import { formatDate, fromNow } from "./time";

const now = new Date("2026-10-15T12:00:00Z");

describe("time", () => {
  it("says how far a moment is", () => {
    expect(fromNow("2026-10-22T12:00:00Z", now)).toBe("in 7 days");
    expect(fromNow("2026-10-16T12:00:00Z", now)).toBe("tomorrow");
    expect(fromNow("2026-10-15T15:00:00Z", now)).toBe("in 3 hours");
    expect(fromNow("2026-10-13T12:00:00Z", now)).toBe("2 days ago");
  });

  it("adds the year only when it is not this one", () => {
    expect(formatDate("2026-03-02T12:00:00Z", now)).not.toMatch(/2026/);
    expect(formatDate("2025-03-02T12:00:00Z", now)).toMatch(/2025/);
  });
});
