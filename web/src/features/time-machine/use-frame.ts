import * as React from "react";

import { type Frame, type FrameRequest, Frames } from "./frame";
import type { History } from "./history";

/** How long what comes and goes animates before the frame settles. */
const SETTLE_MS = 320;

/**
 * The frame of `request`, built from the last one (unchanged tables handed
 * again, what comes and goes animated), then settled once the animations
 * are over.
 */
export function useFrame(history: History, request: FrameRequest): Frame {
  const frames = React.useMemo(() => new Frames(history), [history]);
  const { range, at, mode, filters } = request;
  const group = filters?.tables;
  const query = filters?.query ?? "";
  const changedOnly = filters?.changedOnly === true;
  // The request by value: a new object with the same fields is the same frame.
  const frame = React.useMemo(
    () =>
      frames.frame({
        range: { from: range.from, to: range.to },
        at,
        mode,
        filters: { changedOnly, tables: group, query },
      }),
    [frames, range.from, range.to, at, mode, changedOnly, group, query],
  );
  const [settled, setSettled] = React.useState<{ of: Frame; frame: Frame } | null>(null);
  React.useEffect(() => {
    if (frame.leaving === 0 && frame.entering === 0) return;
    const timer = window.setTimeout(() => {
      const next = frames.settle();
      if (next) setSettled({ of: frame, frame: next });
    }, SETTLE_MS);
    return () => window.clearTimeout(timer);
  }, [frame, frames]);
  return settled?.of === frame ? settled.frame : frame;
}

/**
 * Plays the cursor from where it is to the end of the range, a version per
 * tick: quicker for long ranges, never so quick a frame cannot be read.
 */
export function usePlayback({
  at,
  from,
  to,
  playing,
  onStep,
  onEnd,
}: {
  at: number;
  from: number;
  to: number;
  playing: boolean;
  onStep: (at: number) => void;
  onEnd: () => void;
}) {
  const steps = Math.max(1, to - from);
  const interval = Math.round(Math.min(900, Math.max(260, 9000 / steps)));
  const latest = React.useRef({ at, to, onStep, onEnd });
  React.useEffect(() => {
    latest.current = { at, to, onStep, onEnd };
  });
  React.useEffect(() => {
    if (!playing) return;
    const timer = window.setInterval(() => {
      const { at, to, onStep, onEnd } = latest.current;
      if (at >= to) onEnd();
      else onStep(at + 1);
    }, interval);
    return () => window.clearInterval(timer);
  }, [playing, interval]);
}
