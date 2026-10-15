// Arrow-key moves between tables: the nearest table in that direction,
// favouring ones in line with the current table over ones far to the side.

export interface Box {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

export type Direction = "left" | "right" | "up" | "down";

const center = (b: Box) => ({ x: b.x + b.width / 2, y: b.y + b.height / 2 });

export function nearestInDirection(
  from: Box,
  boxes: readonly Box[],
  direction: Direction,
): Box | undefined {
  const c = center(from);
  // First within a cone around the arrow (at most twice as far off-axis as
  // along it), then anywhere on that side: "down" never means "mostly right".
  for (const cone of [2, Infinity]) {
    let best: Box | undefined;
    let bestScore = Infinity;
    for (const b of boxes) {
      if (b.id === from.id) continue;
      const p = center(b);
      const dx = p.x - c.x;
      const dy = p.y - c.y;
      const along =
        direction === "left" ? -dx : direction === "right" ? dx : direction === "up" ? -dy : dy;
      const across = direction === "left" || direction === "right" ? Math.abs(dy) : Math.abs(dx);
      if (along <= 1 || across > along * cone) continue;
      // Off-axis distance costs double.
      const score = along + across * 2;
      if (score < bestScore || (score === bestScore && best && b.id < best.id)) {
        best = b;
        bestScore = score;
      }
    }
    if (best) return best;
  }
  return undefined;
}

/** The box whose center is closest to a point: where arrow keys start from. */
export function nearestTo(point: { x: number; y: number }, boxes: readonly Box[]): Box | undefined {
  let best: Box | undefined;
  let bestDistance = Infinity;
  for (const b of boxes) {
    const p = center(b);
    const d = Math.hypot(p.x - point.x, p.y - point.y);
    if (d < bestDistance) {
      best = b;
      bestDistance = d;
    }
  }
  return best;
}
