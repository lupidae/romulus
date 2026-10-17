import { Panel, useReactFlow, useStore, useStoreApi, ViewportPortal } from "@xyflow/react";
import * as React from "react";

import { ChatPlusInIcon } from "@/components/icons";
import { Avatar } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import { Tooltip } from "@/components/ui/tooltip";
import type { TableModel } from "@/features/schema/graph/build";
import { columnAnchorY } from "@/features/schema/graph/geometry";
import { useGraphUi } from "@/features/schema/graph/graph-ui";
import { cn } from "@/lib/utils";

import { type Anchor, anchorLabel, type ThreadView } from "../model";

/** Rendered inside a schema graph (its `children`): pins on what threads are pinned to. */
interface CommentPinsProps {
  /** The threads to pin (open ones whose anchor is there). */
  threads: readonly ThreadView[];
  unread: (thread: ThreadView) => boolean;
  /** The thread open in the panel. */
  active: string | null;
  /** A new thread being written there: a pin of its own. */
  composing: Anchor | null;
  onOpen: (thread: string) => void;
  /** Comment mode: clicks pick what to comment on. */
  commentMode: boolean;
  onPick?: (anchor: Anchor) => void;
  onExitCommentMode?: () => void;
}

interface Place {
  key: string;
  x: number;
  y: number;
  /** The bubble's bottom-left on the point (a corner, the canvas), or its left middle (a row). */
  align: "bottom" | "middle";
}

interface Pin extends Place {
  threads: ThreadView[];
  label: string;
}

const ALIGN = {
  bottom: "origin-bottom-left -translate-y-full",
  middle: "origin-left -translate-y-1/2",
};

/**
 * Figma-like pins: a bubble with the author's avatar at a table's corner,
 * beside a column's row (zoomed in; on its table's pin zoomed out), or at a
 * point of the canvas. In comment mode a click on a column, a table or the
 * canvas picks it for a new thread; Enter picks the selected table.
 */
export function CommentPins({
  threads,
  unread,
  active,
  composing,
  onOpen,
  commentMode,
  onPick,
  onExitCommentMode,
}: CommentPinsProps) {
  // The lookup is mutated in place: the node list's identity says when it changed.
  const nodeList = useStore((s) => s.nodes);
  const nodes = useStore((s) => s.nodeLookup);
  const lod = useGraphUi((s) => s.lod);
  const nodeState = useGraphUi((s) => s.nodeState);

  const placeOf = React.useCallback(
    (anchor: Anchor): Place | null => {
      if (anchor.kind === "point")
        return { key: `point:${anchor.x},${anchor.y}`, x: anchor.x, y: anchor.y, align: "bottom" };
      if (anchor.kind === "statement") return null;
      const node = nodes.get(anchor.table);
      if (!node || node.type !== "table") return null;
      const { x, y } = node.internals.positionAbsolute;
      const width = node.measured.width ?? node.width ?? 0;
      if (anchor.kind === "column" && lod === "detail") {
        const table = (node.data as { table: TableModel }).table;
        if (table.columns.some((c) => c.name === anchor.column)) {
          const rowY = columnAnchorY(table, anchor.column, nodeState[anchor.table]);
          return {
            key: `column:${anchor.table}\u0000${anchor.column}`,
            x: x + width + 6,
            y: y + rowY,
            align: "middle",
          };
        }
      }
      return { key: `table:${anchor.table}`, x: x + width - 10, y: y + 8, align: "bottom" };
    },
    // `nodeList` stands for the lookup's content.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [nodes, nodeList, lod, nodeState],
  );

  const pins = React.useMemo(() => {
    const out = new Map<string, Pin>();
    for (const thread of threads) {
      const place = placeOf(thread.anchor);
      if (!place) continue;
      const pin = out.get(place.key) ?? {
        ...place,
        threads: [],
        label: anchorLabel(thread.anchor),
      };
      pin.threads.push(thread);
      out.set(place.key, pin);
    }
    return [...out.values()];
  }, [threads, placeOf]);
  const draft = composing ? placeOf(composing) : null;

  return (
    <>
      <CommentMode on={commentMode} onPick={onPick} onExit={onExitCommentMode} />
      <ViewportPortal>
        {pins.map((pin) => (
          <PinBubble
            key={pin.key}
            pin={pin}
            unread={pin.threads.some(unread)}
            active={pin.threads.some((t) => t.id === active)}
            onOpen={() => {
              const next = pin.threads.find(unread) ?? pin.threads.at(-1)!;
              onOpen(next.id);
            }}
          />
        ))}
        {draft && (
          <div
            aria-hidden
            className="pointer-events-none absolute top-0 left-0 z-40"
            style={{ transform: `translate(${draft.x}px, ${draft.y}px)` }}
          >
            <div className={cn(ALIGN[draft.align], "[scale:calc(1/var(--graph-zoom,1))]")}>
              <span className="flex size-7 items-center justify-center rounded-full rounded-bl-sm border-2 border-primary bg-card text-primary shadow-popover">
                <ChatPlusInIcon className="size-3.5" />
              </span>
            </div>
          </div>
        )}
      </ViewportPortal>
    </>
  );
}

function PinBubble({
  pin,
  unread,
  active,
  onOpen,
}: {
  pin: Pin;
  unread: boolean;
  active: boolean;
  onOpen: () => void;
}) {
  const latest = pin.threads.at(-1)!;
  const author = latest.comments[0]!;
  const count = pin.threads.length;
  const label = `${count === 1 ? "Comment" : `${count} threads`} on ${pin.label}${unread ? ", unread" : ""}: open`;
  return (
    <div
      data-comment-pin=""
      className="absolute top-0 left-0 z-40"
      style={{ transform: `translate(${pin.x}px, ${pin.y}px)` }}
    >
      {/* On the anchor, the same size at any zoom. */}
      <div className={cn(ALIGN[pin.align], "[scale:calc(1/var(--graph-zoom,1))]")}>
        <Tooltip
          content={`${author.authorName}: ${author.body.slice(0, 80)}${author.body.length > 80 ? "…" : ""}`}
        >
          <Button
            variant="ghost"
            aria-label={label}
            onClick={(event) => {
              event.stopPropagation();
              onOpen();
            }}
            onPointerDown={(event) => event.stopPropagation()}
            className={cn(
              "nopan nodrag pointer-events-auto size-7 rounded-full rounded-bl-sm border bg-card p-0.5 shadow-popover hover:bg-card sm:size-7",
              "transition-transform duration-(--duration-fast) hover:scale-110 focus-visible:ring-offset-0 motion-reduce:transition-none",
              unread ? "border-primary ring-2 ring-primary/35" : "border-border-strong",
              active && "scale-110 border-primary ring-3 ring-primary/30",
            )}
          >
            <Avatar name={author.authorName} id={author.authorId ?? author.authorName} size="md" />
            {count > 1 && (
              <span className="absolute -top-1.5 -right-1.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-primary px-1 text-[0.625rem] font-semibold text-primary-foreground tabular-nums">
                {count}
              </span>
            )}
          </Button>
        </Tooltip>
      </div>
    </div>
  );
}

/** Comment mode: the cursor, a hint, and clicks (and Enter) that pick what to comment on. */
function CommentMode({
  on,
  onPick,
  onExit,
}: {
  on: boolean;
  onPick?: (anchor: Anchor) => void;
  onExit?: () => void;
}) {
  const store = useStoreApi();
  const flow = useReactFlow();
  const selected = useGraphUi((s) => s.selected);
  const latest = React.useRef({ onPick, onExit, selected });
  React.useEffect(() => {
    latest.current = { onPick, onExit, selected };
  });

  React.useEffect(() => {
    const root = store.getState().domNode;
    if (!on || !root) return;
    root.dataset.commenting = "";
    let down: { x: number; y: number } | null = null;
    const onDown = (event: PointerEvent) => {
      down = { x: event.clientX, y: event.clientY };
    };
    const onClick = (event: MouseEvent) => {
      const target = event.target as Element;
      // A drag (panning) is not a pick; neither are the toolbars and pins.
      if (down && Math.hypot(event.clientX - down.x, event.clientY - down.y) > 4) return;
      if (target.closest(".react-flow__panel, [data-comment-pin], button, input, textarea, a"))
        return;
      const node = target.closest<HTMLElement>(".react-flow__node[data-id]");
      const id = node?.dataset.id;
      let anchor: Anchor | null = null;
      if (node && id && node.classList.contains("react-flow__node-table")) {
        const column = target.closest<HTMLElement>("[data-column]")?.dataset.column;
        anchor = column ? { kind: "column", table: id, column } : { kind: "table", table: id };
      } else if (target.closest(".react-flow__pane, .react-flow__node")) {
        const at = flow.screenToFlowPosition({ x: event.clientX, y: event.clientY });
        anchor = { kind: "point", x: Math.round(at.x), y: Math.round(at.y) };
      }
      if (!anchor) return;
      event.preventDefault();
      event.stopPropagation();
      latest.current.onPick?.(anchor);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      const typing =
        event.target instanceof HTMLElement &&
        event.target.closest(
          "input, textarea, [contenteditable=true], .cm-editor, [role=dialog], [role=menu]",
        );
      if (typing) return;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        latest.current.onExit?.();
      } else if (
        event.key === "Enter" &&
        root.contains(event.target as Node) &&
        latest.current.selected
      ) {
        event.preventDefault();
        event.stopPropagation();
        latest.current.onPick?.({ kind: "table", table: latest.current.selected });
      }
    };
    root.addEventListener("pointerdown", onDown, true);
    root.addEventListener("click", onClick, true);
    window.addEventListener("keydown", onKey, true);
    return () => {
      delete root.dataset.commenting;
      root.removeEventListener("pointerdown", onDown, true);
      root.removeEventListener("click", onClick, true);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [on, store, flow]);

  if (!on) return null;
  return (
    <Panel position="bottom-center" className="!mb-4">
      <div
        role="status"
        className="flex items-center gap-2 rounded-lg border border-border bg-popover px-3 py-1.5 text-sm text-popover-foreground shadow-popover"
      >
        <ChatPlusInIcon aria-hidden className="size-4 text-primary" />
        <span className="whitespace-nowrap">
          Click what to comment on
          <span className="max-lg:hidden">, or press Enter for the selected table</span>
        </span>
        <Kbd>Esc</Kbd>
        <Button variant="ghost" size="sm" onClick={onExit} className="-mr-1.5">
          Stop commenting
        </Button>
      </div>
    </Panel>
  );
}

/** The graph toolbar's comment-mode toggle (C). */
export function CommentModeButton({
  on,
  onChange,
}: {
  on: boolean;
  onChange: (on: boolean) => void;
}) {
  return (
    <Tooltip content={on ? "Stop commenting" : "Comment"} shortcut={["C"]}>
      <Button
        variant="ghost"
        size="icon-sm"
        aria-label="Comment mode"
        aria-pressed={on}
        className={cn(on && "bg-primary/10 text-primary hover:bg-primary/15 hover:text-primary")}
        onClick={() => onChange(!on)}
      >
        <ChatPlusInIcon />
      </Button>
    </Tooltip>
  );
}
