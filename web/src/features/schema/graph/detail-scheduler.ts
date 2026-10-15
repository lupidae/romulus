// Mounting a node's rows is the expensive part of zooming in: twenty tables
// coming into readable range at once would commit hundreds of rows in one
// frame. Requests are granted a few per animation frame instead, so each
// frame's commit stays small and the zoom keeps moving.

const PER_FRAME = 6;
const queue = new Set<() => void>();
let scheduled = false;

function flush() {
  scheduled = false;
  let granted = 0;
  for (const run of queue) {
    if (granted++ === PER_FRAME) break;
    queue.delete(run);
    run();
  }
  if (queue.size) schedule();
}

function schedule() {
  if (scheduled) return;
  scheduled = true;
  requestAnimationFrame(flush);
}

/** Runs `mount` in an upcoming frame; the returned function cancels it. */
export function requestDetail(mount: () => void): () => void {
  queue.add(mount);
  schedule();
  return () => queue.delete(mount);
}
