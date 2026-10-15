import { useReactFlow, useStore, useStoreApi, ViewportPortal } from "@xyflow/react";
import { useEffect } from "react";

import { cn } from "@/lib/utils";

import type { DraftSession, Peer } from "../session";

// Static class names: Tailwind only generates classes it can see.
const FILL = [
  "fill-presence-1",
  "fill-presence-2",
  "fill-presence-3",
  "fill-presence-4",
  "fill-presence-5",
  "fill-presence-6",
  "fill-presence-7",
  "fill-presence-8",
];
const BG = [
  "bg-presence-1",
  "bg-presence-2",
  "bg-presence-3",
  "bg-presence-4",
  "bg-presence-5",
  "bg-presence-6",
  "bg-presence-7",
  "bg-presence-8",
];
const BORDER = [
  "border-presence-1",
  "border-presence-2",
  "border-presence-3",
  "border-presence-4",
  "border-presence-5",
  "border-presence-6",
  "border-presence-7",
  "border-presence-8",
];

const tone = (list: string[], presence: number) => list[(presence - 1 + 8) % 8]!;

/** Everyone's place in the graph: their pointer, and the table they selected. */
export function LiveGraph({ session, peers }: { session: DraftSession; peers: Peer[] }) {
  return (
    <>
      <PointerReporter session={session} />
      <ViewportPortal>
        <RemoteSelections peers={peers} />
        {peers.map((peer) =>
          peer.pointer ? <RemotePointer key={peer.clientId} peer={peer} /> : null,
        )}
      </ViewportPortal>
    </>
  );
}

/** Shares this pointer's place in graph coordinates, a few times a second at most. */
function PointerReporter({ session }: { session: DraftSession }) {
  const store = useStoreApi();
  const flow = useReactFlow();
  useEffect(() => {
    const pane = store.getState().domNode;
    if (!pane) return;
    let timer = 0;
    let last = 0;
    let at: { x: number; y: number } | null = null;
    const send = () => {
      timer = 0;
      last = performance.now();
      session.setPointer(at);
    };
    const onMove = (event: PointerEvent) => {
      if (event.pointerType === "touch") return;
      at = flow.screenToFlowPosition({ x: event.clientX, y: event.clientY });
      if (!timer) timer = window.setTimeout(send, Math.max(0, 50 - (performance.now() - last)));
    };
    const onLeave = () => {
      window.clearTimeout(timer);
      timer = 0;
      session.setPointer(null);
    };
    pane.addEventListener("pointermove", onMove);
    pane.addEventListener("pointerleave", onLeave);
    return () => {
      pane.removeEventListener("pointermove", onMove);
      pane.removeEventListener("pointerleave", onLeave);
      onLeave();
    };
  }, [store, flow, session]);
  return null;
}

/**
 * A collaborator's pointer: an arrow and their name, the same size at any
 * zoom (`--graph-zoom` is the graph's), gliding between updates.
 */
function RemotePointer({ peer }: { peer: Peer }) {
  const { x, y } = peer.pointer!;
  return (
    <div
      data-testid="remote-pointer"
      className="pointer-events-none absolute top-0 left-0 z-50 transition-transform duration-100 ease-linear motion-reduce:transition-none"
      style={{ transform: `translate(${x}px, ${y}px)` }}
    >
      <div className="origin-top-left [transform:scale(calc(1/var(--graph-zoom,1)))]">
        <svg
          aria-hidden
          viewBox="0 0 16 16"
          className={cn("size-4 drop-shadow-sm", tone(FILL, peer.user.presence))}
        >
          <path
            d="M1.5 1.2 14 6.4 8.3 8.2 6.4 14.1z"
            className="stroke-presence-foreground"
            strokeWidth="1.2"
            strokeLinejoin="round"
          />
        </svg>
        <span
          className={cn(
            "shadow-sm absolute top-3.5 left-3 rounded-md px-1.5 py-0.5 text-xs font-medium whitespace-nowrap text-presence-foreground",
            tone(BG, peer.user.presence),
          )}
        >
          {peer.user.name}
        </span>
      </div>
    </div>
  );
}

/** The tables collaborators selected, outlined in their color with their name. */
function RemoteSelections({ peers }: { peers: Peer[] }) {
  const nodes = useStore((s) => s.nodeLookup);
  const seen = new Map<string, number>();
  return peers.map((peer) => {
    if (!peer.selection) return null;
    const node = nodes.get(peer.selection);
    if (!node) return null;
    // Several people on one table: rings nest outwards.
    const depth = seen.get(peer.selection) ?? 0;
    seen.set(peer.selection, depth + 1);
    const pad = 4 + depth * 4;
    const { x, y } = node.internals.positionAbsolute;
    const width = (node.measured.width ?? node.width ?? 0) + pad * 2;
    const height = (node.measured.height ?? node.height ?? 0) + pad * 2;
    return (
      <div
        key={peer.clientId}
        data-testid="remote-selection"
        className={cn(
          "pointer-events-none absolute top-0 left-0 z-40 rounded-xl border-2",
          tone(BORDER, peer.user.presence),
        )}
        style={{ transform: `translate(${x - pad}px, ${y - pad}px)`, width, height }}
      >
        <span
          className={cn(
            "absolute bottom-full left-2 origin-bottom-left [transform:scale(calc(1/var(--graph-zoom,1)))] rounded-t-md px-1.5 py-0.5 text-xs font-medium whitespace-nowrap text-presence-foreground",
            tone(BG, peer.user.presence),
          )}
        >
          {peer.user.name}
        </span>
      </div>
    );
  });
}
