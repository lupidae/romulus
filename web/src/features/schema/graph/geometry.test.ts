import { freeSpot, nextLod, OVERVIEW_ZOOM } from "./geometry";

describe("level of detail", () => {
  test("switches with hysteresis, not back and forth around one line", () => {
    expect(nextLod("overview", 0.43)).toBe("overview");
    expect(nextLod("overview", OVERVIEW_ZOOM.leave + 0.01)).toBe("detail");
    expect(nextLod("detail", 0.43)).toBe("detail");
    expect(nextLod("detail", OVERVIEW_ZOOM.enter - 0.01)).toBe("overview");
  });
});

describe("freeSpot", () => {
  const size = { width: 220, height: 80 };
  test("keeps a free place", () => {
    expect(freeSpot({ x: 0, y: 0 }, size, [{ x: 500, y: 500, width: 200, height: 200 }])).toEqual({
      x: 0,
      y: 0,
    });
  });
  test("moves off a table to the nearest free place", () => {
    const taken = [{ x: 0, y: 0, width: 240, height: 300 }];
    const spot = freeSpot({ x: 10, y: 10 }, size, taken);
    const overlaps =
      spot.x < 240 + 24 && spot.x + 220 + 24 > 0 && spot.y < 300 + 24 && spot.y + 80 + 24 > 0;
    expect(overlaps).toBe(false);
  });
});
