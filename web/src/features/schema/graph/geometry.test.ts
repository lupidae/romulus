import { nextLod, OVERVIEW_ZOOM } from "./geometry";

describe("level of detail", () => {
  test("switches with hysteresis, not back and forth around one line", () => {
    expect(nextLod("overview", 0.43)).toBe("overview");
    expect(nextLod("overview", OVERVIEW_ZOOM.leave + 0.01)).toBe("detail");
    expect(nextLod("detail", 0.43)).toBe("detail");
    expect(nextLod("detail", OVERVIEW_ZOOM.enter - 0.01)).toBe("overview");
  });
});
