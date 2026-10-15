import {
  MiniMap,
  type MiniMapProps,
  type Node,
  type ReactFlowState,
  useStore,
} from "@xyflow/react";

/** Whether every node is inside the pane: then there is nowhere to navigate to. */
function everythingInView(s: ReactFlowState): boolean {
  const {
    width,
    height,
    transform: [x, y, zoom],
    nodeLookup,
  } = s;
  if (!width || !height || nodeLookup.size === 0) return true;
  let [left, top, right, bottom] = [Infinity, Infinity, -Infinity, -Infinity];
  for (const node of nodeLookup.values()) {
    if (node.hidden) continue;
    const at = node.internals.positionAbsolute;
    left = Math.min(left, at.x);
    top = Math.min(top, at.y);
    right = Math.max(right, at.x + (node.measured.width ?? node.width ?? 0));
    bottom = Math.max(bottom, at.y + (node.measured.height ?? node.height ?? 0));
  }
  const slack = 1;
  return (
    left * zoom + x >= -slack &&
    top * zoom + y >= -slack &&
    right * zoom + x <= width + slack &&
    bottom * zoom + y <= height + slack
  );
}

/**
 * The minimap, shown only when part of the graph is out of view: at fit to
 * screen (and for any schema that fits) it is hidden, so it never covers a
 * table there and leaves the graph its whole pane.
 */
export function NavigatorMap<N extends Node = Node>(props: MiniMapProps<N>) {
  const needed = !useStore(everythingInView);
  return needed ? <MiniMap<N> {...props} /> : null;
}
