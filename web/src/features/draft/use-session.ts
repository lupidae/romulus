import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import type * as Y from "yjs";

import { DraftSession, type DraftSessionOptions, type Peer, type SessionState } from "./session";

/**
 * The live session of a draft, created in an effect (StrictMode mounts twice;
 * a provider made during render would leak a socket) and destroyed with it.
 * Null until then.
 */
export function useDraftSession(
  org: string,
  draftId: string,
  options?: DraftSessionOptions,
): DraftSession | null {
  const [session, setSession] = useState<DraftSession | null>(null);
  const onConnectionFailed = options?.onConnectionFailed;
  useEffect(() => {
    const created = new DraftSession(org, draftId, {
      onConnectionFailed: () => onConnectionFailed?.(),
    });
    // The session owns a socket: it can only be made here, then handed to render.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setSession(created);
    return () => {
      created.destroy();
      setSession(null);
    };
    // The callback is read when called; a new one must not reconnect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [org, draftId]);
  return session;
}

const noSubscription = () => () => {};

/** A slice of the session's state; re-renders when it changes (compare primitives or stable objects). */
export function useSessionState<T>(
  session: DraftSession | null,
  select: (state: SessionState) => T,
  fallback: T,
): T {
  return useSyncExternalStore(
    session?.subscribe ?? noSubscription,
    () => (session ? select(session.getState()) : fallback),
    () => fallback,
  );
}

interface Snapshot<T> {
  subscribe: (listener: () => void) => () => void;
  get: () => T;
}

/**
 * A Yjs type read as a value: read again only after it changed, and
 * listeners told once a frame at most (a burst of remote edits is one render).
 */
interface Observable {
  observe(f: () => void): void;
  unobserve(f: () => void): void;
}

function snapshotOf<T, Type extends Observable>(type: Type, read: (type: Type) => T): Snapshot<T> {
  let value = read(type);
  let dirty = false;
  return {
    subscribe(listener) {
      let frame = 0;
      const changed = () => {
        dirty = true;
        cancelAnimationFrame(frame);
        frame = requestAnimationFrame(listener);
      };
      type.observe(changed);
      return () => {
        cancelAnimationFrame(frame);
        type.unobserve(changed);
      };
    },
    get() {
      if (dirty) {
        value = read(type);
        dirty = false;
      }
      return value;
    },
  };
}

const readMap = <T>(map: Y.Map<T>) => map.toJSON() as Record<string, T>;
const readText = (text: Y.Text) => text.toJSON();
const emptyRecord = {};

/** A Y.Map as a plain object, a new one per change. */
export function useYMap<T>(map: Y.Map<T> | undefined): Record<string, T> {
  const snapshot = useMemo(() => (map ? snapshotOf(map, readMap<T>) : null), [map]);
  return useSyncExternalStore(
    snapshot?.subscribe ?? noSubscription,
    () => snapshot?.get() ?? (emptyRecord as Record<string, T>),
    () => emptyRecord as Record<string, T>,
  );
}

/** A Y.Text's string. */
export function useYText(text: Y.Text | undefined): string {
  const snapshot = useMemo(() => (text ? snapshotOf(text, readText) : null), [text]);
  return useSyncExternalStore(
    snapshot?.subscribe ?? noSubscription,
    () => snapshot?.get() ?? "",
    () => "",
  );
}

/** Everyone else in the draft, as awareness says (once a frame at most). */
export function usePeers(session: DraftSession | null): Peer[] {
  const snapshot = useMemo<Snapshot<Peer[]> | null>(() => {
    if (!session) return null;
    let value = session.peers();
    let dirty = false;
    return {
      subscribe(listener) {
        let frame = 0;
        const changed = (change: { added: number[]; updated: number[]; removed: number[] }) => {
          // This tab's own pointer (sent while it moves) is no peer's: no render.
          const me = session.doc.clientID;
          const ids = [...change.added, ...change.updated, ...change.removed];
          if (ids.every((id) => id === me)) return;
          dirty = true;
          cancelAnimationFrame(frame);
          frame = requestAnimationFrame(listener);
        };
        session.awareness.on("change", changed);
        return () => {
          cancelAnimationFrame(frame);
          session.awareness.off("change", changed);
        };
      },
      get() {
        if (dirty) {
          value = session.peers();
          dirty = false;
        }
        return value;
      },
    };
  }, [session]);
  return useSyncExternalStore(
    snapshot?.subscribe ?? noSubscription,
    () => snapshot?.get() ?? noPeers,
    () => noPeers,
  );
}

const noPeers: Peer[] = [];

/** Whether there is something to undo and to redo. */
export function useUndoState(session: DraftSession | null): { canUndo: boolean; canRedo: boolean } {
  const subscribe = useMemo(
    () =>
      session
        ? (listener: () => void) => {
            const undo = session.undo;
            undo.on("stack-item-added", listener);
            undo.on("stack-item-popped", listener);
            undo.on("stack-cleared", listener);
            return () => {
              undo.off("stack-item-added", listener);
              undo.off("stack-item-popped", listener);
              undo.off("stack-cleared", listener);
            };
          }
        : noSubscription,
    [session],
  );
  const canUndo = useSyncExternalStore(
    subscribe,
    () => (session?.undo.undoStack.length ?? 0) > 0,
    () => false,
  );
  const canRedo = useSyncExternalStore(
    subscribe,
    () => (session?.undo.redoStack.length ?? 0) > 0,
    () => false,
  );
  return { canUndo, canRedo };
}
