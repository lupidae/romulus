// One open draft in this tab: its Y.Doc, the websocket to the collab server
// (y-websocket), what the server says about it (its events: tag 100), and
// this client's undo history. Plain TypeScript; React reads it through
// `use-session.ts`.
//
// The doc (docs/MVP.md "The core design"):
//   sql:    Y.Text                       canonical DDL
//   layout: Y.Map<qualified name, {x, y}>
//   groups: Y.Map<group id, {name, color, members}>
// Awareness carries this client's `cursor` (y-codemirror), `pointer` (graph
// coordinates) and `selection` (a table); the server stamps `user`.

import * as decoding from "lib0/decoding";
import type { Awareness } from "y-protocols/awareness";
import { WebsocketProvider } from "y-websocket";
import * as Y from "yjs";

import { type Group, type GroupColor, type Layout, type Position } from "@/features/schema/layout";
import { parseSchema, type Schema } from "@/features/schema/model";

import type { Role } from "@/features/orgs/queries";

import type { CompileState, Diagnostic, LiveStatus, ServerEvent } from "./queries";

/** The server's own messages on the socket (crates/romulus-api/src/collab/protocol.rs). */
export const MESSAGE_EVENT = 100;

/** Final close codes (collab::room::close): the session ended… */
export const CLOSE_SESSION = 4401;
/** …the draft is gone… */
export const CLOSE_DELETED = 4404;
/** …an update made it larger than a draft may be (it was not applied). */
export const CLOSE_TOO_LARGE = 4413;

/**
 * Transaction origins. Graph edits made here are `LOCAL`: the undo manager
 * tracks them (with y-codemirror's own origin for typing), so one ⌘Z undoes
 * a move, a group change or typing, and never someone else's work.
 * Positions the graph picks for tables that had none are `PLACED`: not an
 * action of anyone's, never undone.
 */
export const LOCAL = { origin: "local" } as const;
export const PLACED = { origin: "placed" } as const;

export type Connection = "connecting" | "connected" | "reconnecting" | "offline";

export interface CompileView {
  /** `waiting`: no result yet. */
  state: CompileState | "waiting";
  /** The text changed since the result below: a newer compile is coming. */
  stale: boolean;
  /** Of the last result whose text this client had: their spans fit it. */
  diagnostics: Diagnostic[];
  /** The last model that compiled: what the graph shows. */
  model: Schema | null;
}

export interface SessionState {
  connection: Connection;
  /** The doc arrived (once: later reconnects keep showing what is here). */
  synced: boolean;
  /** This connection's role, as the server sees it (null until it says). */
  role: Role | null;
  status: LiveStatus;
  /** Who published, is publishing, or deleted it. */
  statusBy: string | null;
  publishedVersion: number | null;
  /** A rename made while the draft is open. */
  name: string | null;
  compile: CompileView;
  /** The database got a new version meanwhile: its number. */
  versionCreated: number | null;
  /** The server closed the socket for good. */
  deleted: boolean;
  /** The session ended (signed out elsewhere, expired): sign in again. */
  signedOut: boolean;
  /** The last update would have made the draft too large: it was refused. */
  tooLarge: boolean;
  /** The server could not store some edits. */
  saveFailed: boolean;
  /** Edits made here while disconnected, not on the server yet. */
  unsynced: boolean;
}

/**
 * Whether this person may edit the draft now: their role as the server
 * sees it, and the draft's live status (not a snapshot taken on joining:
 * someone who joined while it was being published edits again once that
 * failed).
 */
export function mayEdit(state: Pick<SessionState, "role" | "status">): boolean {
  return state.role !== null && state.role !== "reviewer" && state.status === "open";
}

/** What a server event (other than a compile) changes in the state. */
export function eventPatch(
  state: SessionState,
  event: Exclude<ServerEvent, { type: "compile" }>,
): Partial<SessionState> {
  switch (event.type) {
    case "welcome":
      return { role: event.role, status: event.status };
    case "status":
      return {
        status: event.status,
        statusBy: event.by ?? null,
        publishedVersion: event.version ?? state.publishedVersion,
        deleted: event.status === "deleted",
      };
    case "renamed":
      return { name: event.name };
    case "version_created":
      return { versionCreated: event.number };
    case "save_failed":
      return { saveFailed: true };
  }
}

/** A collaborator as the server stamped them (collab::awareness::Peer::stamp). */
export interface PeerUser {
  id: string;
  name: string;
  role: string;
  presence: number;
}

export interface Peer {
  clientId: number;
  user: PeerUser;
  /** In graph coordinates; null off the graph. */
  pointer: Position | null;
  /** The table selected in their graph. */
  selection: string | null;
}

async function sha256(text: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function isPeerUser(value: unknown): value is PeerUser {
  const v = value as Partial<PeerUser> | null;
  return (
    typeof v === "object" &&
    v !== null &&
    typeof v.id === "string" &&
    typeof v.name === "string" &&
    typeof v.presence === "number"
  );
}

function isPosition(value: unknown): value is Position {
  const v = value as Partial<Position> | null;
  return (
    typeof v === "object" &&
    v !== null &&
    Number.isFinite(v.x) &&
    Number.isFinite(v.y) &&
    Math.abs(v.x!) < 1e7 &&
    Math.abs(v.y!) < 1e7
  );
}

export interface DraftSessionOptions {
  /** `ws://host` or `wss://host`; default: this page's origin. */
  origin?: string;
  /** A failed connection attempt (the upgrade refused, the network down). */
  onConnectionFailed?: () => void;
}

let sessions = 0;

/** New groups take the calm colors first (red reads as an error, stone as none). */
const GROUP_ORDER: GroupColor[] = [
  "blue",
  "teal",
  "violet",
  "green",
  "amber",
  "pink",
  "red",
  "stone",
];

export class DraftSession {
  /** Tells sessions apart (a remount makes a new one): React keys. */
  readonly key = ++sessions;
  readonly doc = new Y.Doc();
  readonly sql: Y.Text;
  readonly layout: Y.Map<Position>;
  readonly groups: Y.Map<Group>;
  readonly provider: WebsocketProvider;
  readonly awareness: Awareness;
  /** One per client, over the text, the layout and the groups; local origins only. */
  readonly undo: Y.UndoManager;

  private state: SessionState = {
    connection: "connecting",
    synced: false,
    role: null,
    status: "open",
    statusBy: null,
    publishedVersion: null,
    name: null,
    compile: { state: "waiting", stale: true, diagnostics: [], model: null },
    versionCreated: null,
    deleted: false,
    signedOut: false,
    tooLarge: false,
    saveFailed: false,
    unsynced: false,
  };
  private listeners = new Set<() => void>();
  private destroyed = false;
  private opened = false;
  /** Compile events are read in order: a slow hash never lets an older one win. */
  private compiles = 0;
  private lastCompile: Extract<ServerEvent, { type: "compile" }> | null = null;
  private recheck = 0;

  constructor(org: string, draftId: string, options: DraftSessionOptions = {}) {
    this.sql = this.doc.getText("sql");
    this.layout = this.doc.getMap("layout");
    this.groups = this.doc.getMap("groups");
    this.undo = new Y.UndoManager([this.sql, this.layout, this.groups], {
      trackedOrigins: new Set([LOCAL]),
    });

    const origin = options.origin ?? window.location.origin.replace(/^http/, "ws");
    this.provider = new WebsocketProvider(
      `${origin}/api/orgs/${encodeURIComponent(org)}/drafts`,
      `${encodeURIComponent(draftId)}/ws`,
      this.doc,
      // No cross-tab channel: every edit goes through the server, which
      // decides what a reviewer's tab may change.
      { disableBc: true, maxBackoffTime: 5000 },
    );
    this.awareness = this.provider.awareness;
    // Receivers ignore a first state at clock 0 from a client they do not
    // know (y-protocols): set one field now, so this one starts at 1 and
    // everyone sees who joined before they move.
    this.awareness.setLocalStateField("selection", null);

    this.provider.messageHandlers[MESSAGE_EVENT] = (_encoder, decoder) => {
      const body = decoding.readVarUint8Array(decoder);
      try {
        this.onEvent(JSON.parse(new TextDecoder().decode(body)) as ServerEvent);
      } catch (error) {
        console.error("draft: unreadable server event", error);
      }
    };
    this.provider.on("status", ({ status }) => {
      if (status === "connected") this.opened = true;
      this.set({ connection: this.connection(status) });
    });
    this.provider.on("sync", (synced) => {
      // Our missing edits went with the answer to the server's step 1, before this.
      if (synced) {
        this.set({ synced: true, unsynced: false });
        void this.checkCompile();
      }
    });
    this.doc.on("update", (_update: Uint8Array, origin: unknown) => {
      if (origin !== this.provider && !this.provider.wsconnected && !this.state.unsynced) {
        this.set({ unsynced: true });
      }
    });
    this.provider.on("connection-close", (event) => {
      if (!this.opened && event) options.onConnectionFailed?.();
      this.opened = false;
    });
    this.provider.on("closed", ({ code }) => {
      if (code === CLOSE_DELETED) this.set({ deleted: true, status: "deleted" });
      else if (code === CLOSE_SESSION) this.set({ signedOut: true });
      else if (code === CLOSE_TOO_LARGE) this.set({ tooLarge: true });
    });
    window.addEventListener("online", this.onNetwork);
    window.addEventListener("offline", this.onNetwork);

    this.sql.observe(this.onText);
  }

  // ---- State -------------------------------------------------------------------

  getState = () => this.state;

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  private set(patch: Partial<SessionState>) {
    this.state = { ...this.state, ...patch };
    this.listeners.forEach((l) => l());
  }

  private connection(status: "connected" | "disconnected" | "connecting"): Connection {
    if (typeof navigator !== "undefined" && !navigator.onLine) return "offline";
    if (status === "connected") return "connected";
    return this.state.synced ? "reconnecting" : "connecting";
  }

  private onNetwork = () => {
    this.set({
      connection: this.connection(this.provider.wsconnected ? "connected" : "disconnected"),
    });
    // Back online: try now rather than at the end of the backoff.
    if (navigator.onLine && !this.provider.wsconnected && this.provider.shouldConnect) {
      this.provider.disconnect();
      this.provider.connect();
    }
  };

  // ---- Server events -----------------------------------------------------------

  private onEvent(event: ServerEvent) {
    if (event.type === "compile") {
      this.onCompile(event);
      return;
    }
    this.set(eventPatch(this.state, event));
    if (event.type === "status" && (event.status === "published" || event.status === "deleted")) {
      // Nothing will change it any more: stop reconnecting.
      this.provider.disconnect();
    }
  }

  private onCompile(event: Extract<ServerEvent, { type: "compile" }>) {
    let model = this.state.compile.model;
    if (event.model) {
      try {
        model = parseSchema(event.model);
      } catch (error) {
        console.error("draft: the compiled model does not read", error);
      }
    }
    this.lastCompile = event;
    this.set({ compile: { ...this.state.compile, state: event.state, model } });
    void this.checkCompile();
  }

  /**
   * Whether the last result is about this client's text: then its
   * diagnostics fit it and nothing is compiling. Checked on each result,
   * once the doc arrived (a result can come first), and when typing pauses
   * (an undo can bring the compiled text back).
   */
  private async checkCompile() {
    const event = this.lastCompile;
    if (!event) return;
    const seq = ++this.compiles;
    const text = this.sql.toJSON();
    const matches = !!event.sha256 && event.sha256 === (await sha256(text));
    if (this.destroyed || seq !== this.compiles || this.lastCompile !== event) return;
    const current = matches && this.sql.toJSON() === text;
    this.set({
      compile: {
        ...this.state.compile,
        state: event.state,
        // A result for another text than this client's: one for this text follows.
        stale: event.state === "pending" || (event.state !== "unavailable" && !current),
        diagnostics: current ? event.diagnostics : this.state.compile.diagnostics,
      },
    });
  }

  private onText = () => {
    if (!this.state.compile.stale && this.state.compile.state !== "unavailable") {
      this.set({ compile: { ...this.state.compile, stale: true } });
    }
    window.clearTimeout(this.recheck);
    this.recheck = window.setTimeout(() => void this.checkCompile(), 600);
  };

  // ---- Awareness -----------------------------------------------------------------

  /** Everyone else here, one entry per tab. */
  peers(): Peer[] {
    const out: Peer[] = [];
    this.awareness.getStates().forEach((state, clientId) => {
      if (clientId === this.doc.clientID || !isPeerUser(state.user)) return;
      out.push({
        clientId,
        user: state.user,
        pointer: isPosition(state.pointer) ? state.pointer : null,
        selection: typeof state.selection === "string" ? state.selection : null,
      });
    });
    return out;
  }

  setPointer(pointer: Position | null) {
    const current = this.awareness.getLocalState()?.pointer as Position | null | undefined;
    if (pointer === null && (current === null || current === undefined)) return;
    this.awareness.setLocalStateField(
      "pointer",
      pointer && { x: Math.round(pointer.x), y: Math.round(pointer.y) },
    );
  }

  setSelection(key: string | null) {
    if ((this.awareness.getLocalState()?.selection ?? null) === key) return;
    this.awareness.setLocalStateField("selection", key);
  }

  // ---- Edits -----------------------------------------------------------------------

  /**
   * Positions from the graph: tables moved by hand (undoable) and tables it
   * placed because they had none (not undoable, nobody's action).
   */
  setPositions(positions: Layout) {
    const moved: Layout = {};
    const placed: Layout = {};
    for (const [key, p] of Object.entries(positions)) {
      (this.layout.has(key) ? moved : placed)[key] = { x: Math.round(p.x), y: Math.round(p.y) };
    }
    const write = (entries: Layout, origin: object) => {
      if (Object.keys(entries).length === 0) return;
      this.doc.transact(() => {
        for (const [key, p] of Object.entries(entries)) this.layout.set(key, p);
      }, origin);
    };
    write(placed, PLACED);
    write(moved, LOCAL);
  }

  /** A new group of these tables, named and colored by default; its id. */
  createGroup(members: string[]): string {
    const used = new Set([...this.groups.values()].map((g) => g.color));
    const color =
      GROUP_ORDER.find((c) => !used.has(c)) ?? GROUP_ORDER[this.groups.size % GROUP_ORDER.length]!;
    const id = crypto.randomUUID().slice(0, 8);
    const name = `Group ${this.groups.size + 1}`;
    // A table belongs to one group: it leaves the others.
    this.doc.transact(() => {
      for (const [gid, g] of this.groups.entries()) {
        const kept = g.members.filter((m) => !members.includes(m));
        if (kept.length !== g.members.length) this.groups.set(gid, { ...g, members: kept });
      }
      this.groups.set(id, { name, color, members: [...new Set(members)] });
    }, LOCAL);
    return id;
  }

  updateGroup(id: string, patch: Partial<Group>) {
    const group = this.groups.get(id);
    if (!group) return;
    this.doc.transact(() => this.groups.set(id, { ...group, ...patch }), LOCAL);
  }

  deleteGroup(id: string) {
    this.doc.transact(() => this.groups.delete(id), LOCAL);
  }

  destroy() {
    this.destroyed = true;
    window.clearTimeout(this.recheck);
    window.removeEventListener("online", this.onNetwork);
    window.removeEventListener("offline", this.onNetwork);
    this.sql.unobserve(this.onText);
    this.listeners.clear();
    this.undo.destroy();
    this.provider.destroy();
    this.doc.destroy();
  }
}
