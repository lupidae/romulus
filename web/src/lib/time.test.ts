import { formatDate, fromNow, timeAgo } from "./time";

const now = new Date("2026-10-15T12:00:00Z");

describe("time", () => {
  it("says how far a moment is", () => {
    expect(fromNow("2026-10-22T12:00:00Z", now)).toBe("in 7 days");
    expect(fromNow("2026-10-16T12:00:00Z", now)).toBe("tomorrow");
    expect(fromNow("2026-10-15T15:00:00Z", now)).toBe("in 3 hours");
    expect(fromNow("2026-10-13T12:00:00Z", now)).toBe("2 days ago");
  });

  it("says just now rather than this minute", () => {
    expect(timeAgo("2026-10-15T11:59:30Z", now)).toBe("just now");
    expect(timeAgo("2026-10-15T11:55:00Z", now)).toBe("5 minutes ago");
    expect(timeAgo("2026-10-14T12:00:00Z", now)).toBe("yesterday");
  });

  it("adds the year only when it is not this one", () => {
    expect(formatDate("2026-03-02T12:00:00Z", now)).not.toMatch(/2026/);
    expect(formatDate("2025-03-02T12:00:00Z", now)).toMatch(/2025/);
  });

  it("speaks English whatever the browser's locale", () => {
    expect(formatDate("2025-03-02T12:00:00Z", now)).toBe("Mar 2, 2025");
    expect(timeAgo("2026-10-12T12:00:00Z", now)).toBe("3 days ago");
  });
});
