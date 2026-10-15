import { requestDetail } from "./detail-scheduler";

describe("requestDetail", () => {
  test("grants a few mounts per frame, in order, and honours cancellation", () => {
    const frames: FrameRequestCallback[] = [];
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb) => frames.push(cb));
    const mounted: number[] = [];
    const cancels = Array.from({ length: 10 }, (_, i) => requestDetail(() => mounted.push(i)));
    cancels[7]!();
    frames.shift()!(0);
    expect(mounted).toEqual([0, 1, 2, 3, 4, 5]);
    frames.shift()!(16);
    expect(mounted).toEqual([0, 1, 2, 3, 4, 5, 6, 8, 9]);
    expect(frames).toHaveLength(0);
    vi.restoreAllMocks();
  });
});
