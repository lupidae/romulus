// One open draft in this tab: its Y.Doc, the websocket to the collab server
// (y-websocket), what the server says about it (its events: tag 100), and
// this client's undo history. Plain TypeScript; React reads it through
// `use-session.ts`.
//
// The doc (docs/MVP.md "The core design", "Review round 1" item 2):
//   sql:       Y.Text                    the schema SQL: the source in schema mode
//   migration: Y.Text                    a migration from the base version: the
//                                        source in migration mode
//   layout:    Y.Map<qualified name, {x, y}>
//   groups:    Y.Map<group id, {name, color, members}>
//   threads:   Y.Map<thread id, {anchor, by, at, resolved}>  comment threads
//   comments:  Y.Map<comment id, {thread, author, body…}>    and their comments
//              (features/comments/model.ts; the server checks and stamps them,
//              reviewers write them too)
//   meta:      Y.Map: `rewrite` = {count, reason}, bumped by the server in the
//              same transaction as each rewrite of the SQL (Tidy, Update draft);
//              `mode` = "schema" | "migration" (absent: schema), written by
//              the server only, with the texts it switches
// One text is the source at a time: in migration mode the schema (its SQL
// sent with each compile) and the graph are what the migration makes of
// the base version, and graph edits append to the migration.
// Awareness carries this client's `cursor` (y-codemirror), `pointer` (graph
// coordinates), `selection` (a table) and `typing` (the comment thread being
// written in, `new` for one being started); the server stamps `user`.

import * as decoding from "lib0/decoding";
import * as encoding from "lib0/encoding";
import { Awareness } from "y-protocols/awareness";
import { WebsocketProvider } from "y-websocket";
import * as Y from "yjs";

import {
  type Anchor,
  type DocComment,
  type DocThread,
  renameColumn,
  renameTable,
} from "@/features/comments/model";
import { appendTo } from "@/features/schema/edits/append";
import { applyEdit, layoutChange, type LayoutChange } from "@/features/schema/edits/apply";
import { type GraphEdit, sqlFor } from "@/features/schema/edits/edits";
import { type GroupsChange, nextGroupName, regroup } from "@/features/schema/graph/groups";
import { type Group, type GroupColor, type Layout, type Position } from "@/features/schema/layout";
import { parseSchema, type Schema } from "@/features/schema/model";

import type { Role } from "@/features/orgs/queries";

import { type EditEntry, KEPT_EDITS, touchedEntities } from "./graph-edits";
import type { CompileState, Diagnostic, DraftMode, LiveStatus, ServerEvent } from "./queries";
import type { Review } from "./review";

/** The server's own messages on the socket (crates/romulus-api/src/collab/protocol.rs). */
export const MESSAGE_EVENT = 100;
/** Comment operations, from this client: the server writes what they mean. */
export const MESSAGE_COMMENT = 101;

/** Final close codes (collab::room::close): the session ended… */
export const CLOSE_SESSION = 4401;
/** …the draft is gone… */
export const CLOSE_DELETED = 4404;
/** …an update made it larger than a draft may be (it was not applied)… */
export const CLOSE_TOO_LARGE = 4413;
/** …the person has it open in too many tabs already. */
export const CLOSE_TOO_MANY = 4429;
/** …this tab's copy no longer matches the server's: reload. */
export const CLOSE_OUT_OF_STEP = 4409;
/**
 * …this tab wrote with an id another tab announced: reconnect and sync
 * again (y-websocket does), a few times at most (`syncStuck`).
 */
export const CLOSE_RESYNC = 4003;
/** Refusals to sync (4003) within a minute before this tab stops trying. */
const MAX_RESYNCS = 3;

/**
 * Transaction origins. Graph edits made here are `LOCAL`: the undo manager
 * tracks them (with y-codemirror's own origin for typing), so one ⌘Z undoes
 * a move, a group change, a schema edit (its SQL and its layout move
 * together) or typing, and never someone else's work.
 * Positions the graph picks for tables that had none are `PLACED`: not an
 * action of anyone's, never undone.
 */
export const LOCAL = { origin: "local" } as const;
export const PLACED = { origin: "placed" } as const;
/**
 * Comments written here without a server (a local draft): not undone by ⌘Z.
 * With a server, comments from the panel or the graph are operations the
 * server writes (`sendComment`); a `---` line turned into a comment is
 * `LOCAL`: one step with the text it removes.
 */
export const COMMENT = { origin: "comment" } as const;

/** An operation on the discussion (collab::comments::Op). */
export type CommentOp =
  | {
      type: "start";
      thread: string;
      comment: string;
      anchor: Anchor;
      body: string;
      mentions: string[];
    }
  | { type: "reply"; thread: string; comment: string; body: string; mentions: string[] }
  | { type: "edit"; comment: string; body: string; mentions: string[] }
  | { type: "delete"; comment: string }
  | { type: "delete_thread"; thread: string }
  | { type: "resolve"; thread: string; resolved: boolean };

/** The server refused a comment operation, or never answered. */
export class CommentRefused extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = "CommentRefused";
    this.code = code;
  }
}

/** How long a comment operation waits for the server's answer. */
const COMMENT_TIMEOUT = 10_000;

/** Who writes comments here (the server stamps it anyway). */
export interface Me {
  id: string;
  name: string;
}

/** `stopped`: no longer reconnecting (`syncStuck`) until asked to. */
export type Connection = "connecting" | "connected" | "reconnecting" | "offline" | "stopped";

export interface CompileView {
  /** `waiting`: no result yet. */
  state: CompileState | "waiting";
  /** The text changed since the result below: a newer compile is coming. */
  stale: boolean;
  /** Of the last result whose text this client had: their spans fit it. */
  diagnostics: Diagnostic[];
  /** The last model that compiled: what the graph shows. */
  model: Schema | null;
  /** SHA-256 of this client's source when the last result is `ok` and about it. */
  sha256: string | null;
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
  /** Which text is the source (the doc's `meta.mode`). */
  mode: DraftMode;
  /** Migration mode: the schema the migration makes, as SQL (of the last model that compiled). */
  schemaSql: string | null;
  /** The last mode switch while this tab was open (someone else's or this tab's). */
  modeChanged: { mode: DraftMode; by: string } | null;
  /** Someone moved the database to another Postgres major: the draft compiles there now. */
  majorChanged: { major: number; by: string } | null;
  /** This tab's edit reached the text that is not the source (before it saw a switch) and was undone. */
  staleEdit: { mode: DraftMode; at: number } | null;
  compile: CompileView;
  /** The database got a new version meanwhile: its number. */
  versionCreated: number | null;
  /** An admin removed or restored a version meanwhile (`seq` tells repeats apart). */
  versionsChanged: {
    kind: "removed" | "restored";
    number: number;
    by: string;
    seq: number;
  } | null;
  /** Approvals and the review request, as the server counts them (null until it says). */
  review: Review | null;
  /** The server closed the socket for good. */
  deleted: boolean;
  /** The session ended (signed out elsewhere, expired): sign in again. */
  signedOut: boolean;
  /** The last update would have made the draft too large: it was refused. */
  tooLarge: boolean;
  /** Refused: the draft is open in too many of this person's tabs. */
  tooManyTabs: boolean;
  /** Let go: this tab's copy no longer matches the server's (reload). */
  outOfStep: boolean;
  /** Stopped reconnecting: the server kept asking it to sync again (4003). */
  syncStuck: boolean;
  /** The server could not store some edits. */
  saveFailed: boolean;
  /** The last "Update draft" while this tab was open. */
  updated: { version: number; by: string; conflicts: number } | null;
  /** Edits made here while disconnected, not on the server yet. */
  unsynced: boolean;
  /** Schema edits made from the graph in this tab, oldest first (see graph-edits.ts). */
  edits: readonly EditEntry[];
  /**
   * The server rewrote the SQL (Tidy, Update draft) and this tab's undo
   * history went with it; `mine` when this tab asked for the rewrite.
   */
  historyCleared: { reason: string; count: number; mine: boolean } | null;
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
    case "version_removed":
    case "version_restored":
      return {
        versionsChanged: {
          kind: event.type === "version_removed" ? "removed" : "restored",
          number: event.number,
          by: event.by,
          seq: (state.versionsChanged?.seq ?? 0) + 1,
        },
      };
    case "review": {
      const { type: _type, ...review } = event;
      return {
        review: {
          ...review,
          requested: review.requested ?? null,
          editors_unknown: review.editors_unknown ?? false,
        },
      };
    }
    case "save_failed":
      return { saveFailed: true };
    case "updated":
      return {
        updated: { version: event.version, by: event.by, conflicts: event.conflicts },
      };
    case "mode":
      return { modeChanged: { mode: event.mode, by: event.by } };
    case "major":
      return { majorChanged: { major: event.pg_major, by: event.by } };
    case "stale_edit":
      return { staleEdit: { mode: event.mode, at: Date.now() } };
    case "comment_result":
      // The session answers its own operations (`sendComment`).
      return {};
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
  /** The comment thread they are writing in (`new`: starting one). */
  typing: string | null;
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
  /** No server: a draft held in this tab only (dev screens, tests). */
  local?: LocalDraft;
}

/** A draft without a server: its content, and what stands in for the compile. */
export interface LocalDraft {
  sql: string;
  /** Migration mode: the migration, then the source (`compile` gets it). */
  migration?: string;
  layout?: Layout;
  groups?: Record<string, Group>;
  /** Comment threads and comments, by id, to start with. */
  threads?: Record<string, DocThread>;
  comments?: Record<string, DocComment>;
  role: Role;
  /** Compiles the text, as the server would, when typing pauses. */
  compile: (sql: string) => { model: unknown; diagnostics: Diagnostic[] };
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
  readonly migration: Y.Text;
  readonly layout: Y.Map<Position>;
  readonly groups: Y.Map<Group>;
  /** Comment threads and their comments (features/comments/model.ts). */
  readonly threads: Y.Map<unknown>;
  readonly comments: Y.Map<unknown>;
  /** Notes from the server about the doc (see `onMeta`). */
  readonly meta: Y.Map<unknown>;
  /** Null for a local draft. */
  readonly provider: WebsocketProvider | null;
  readonly awareness: Awareness;
  /** One per client, over the texts, the layout, the groups and comment anchors; local origins only. */
  readonly undo: Y.UndoManager;

  private state: SessionState = {
    connection: "connecting",
    synced: false,
    role: null,
    status: "open",
    statusBy: null,
    publishedVersion: null,
    name: null,
    mode: "schema",
    schemaSql: null,
    modeChanged: null,
    majorChanged: null,
    staleEdit: null,
    compile: { state: "waiting", stale: true, diagnostics: [], model: null, sha256: null },
    versionCreated: null,
    versionsChanged: null,
    review: null,
    deleted: false,
    signedOut: false,
    tooLarge: false,
    tooManyTabs: false,
    outOfStep: false,
    syncStuck: false,
    saveFailed: false,
    updated: null,
    unsynced: false,
    edits: [],
    historyCleared: null,
  };
  /** Each schema edit's entry in the undo history. */
  private undoItems = new Map<number, unknown>();
  /** No server: comments are written here. */
  private readonly isLocal: boolean;
  /** Comment operations waiting for the server's answer, by id. */
  private answers = new Map<
    string,
    { resolve: () => void; reject: (error: Error) => void; timer: number }
  >();
  /** This tab asked for a rewrite (Tidy) that has not arrived yet. */
  private rewriteExpected = false;
  private lastEdit = 0;
  private listeners = new Set<() => void>();
  private destroyed = false;
  private opened = false;
  /** When the server last asked this tab to sync again (4003). */
  private resyncs: number[] = [];
  /** Compile events are read in order: a slow hash never lets an older one win. */
  private compiles = 0;
  private lastCompile: Extract<ServerEvent, { type: "compile" }> | null = null;
  private recheck = 0;

  constructor(org: string, draftId: string, options: DraftSessionOptions = {}) {
    this.sql = this.doc.getText("sql");
    this.migration = this.doc.getText("migration");
    this.layout = this.doc.getMap("layout");
    this.groups = this.doc.getMap("groups");
    this.threads = this.doc.getMap("threads");
    this.comments = this.doc.getMap("comments");
    this.meta = this.doc.getMap("meta");
    this.meta.observe(this.onMeta);
    this.undo = new Y.UndoManager(
      [this.sql, this.migration, this.layout, this.groups, this.threads, this.comments],
      { trackedOrigins: new Set([LOCAL]) },
    );

    this.isLocal = options.local !== undefined;
    if (options.local) {
      this.provider = null;
      this.awareness = new Awareness(this.doc);
      this.startLocal(options.local);
      this.sql.observe(this.onText);
      this.migration.observe(this.onText);
      return;
    }
    const origin = options.origin ?? window.location.origin.replace(/^http/, "ws");
    const provider = new WebsocketProvider(
      `${origin}/api/orgs/${encodeURIComponent(org)}/drafts`,
      `${encodeURIComponent(draftId)}/ws`,
      this.doc,
      // No cross-tab channel: every edit goes through the server, which
      // decides what a reviewer's tab may change.
      { disableBc: true, maxBackoffTime: 5000 },
    );
    this.provider = provider;
    this.awareness = provider.awareness;
    // Receivers ignore a first state at clock 0 from a client they do not
    // know (y-protocols): set one field now, so this one starts at 1 and
    // everyone sees who joined before they move.
    this.awareness.setLocalStateField("selection", null);

    provider.messageHandlers[MESSAGE_EVENT] = (_encoder, decoder) => {
      const body = decoding.readVarUint8Array(decoder);
      try {
        this.onEvent(JSON.parse(new TextDecoder().decode(body)) as ServerEvent);
      } catch (error) {
        console.error("draft: unreadable server event", error);
      }
    };
    provider.on("status", ({ status }) => {
      if (status === "connected") this.opened = true;
      this.set({ connection: this.connection(status) });
    });
    provider.on("sync", (synced) => {
      // Our missing edits went with the answer to the server's step 1, before this.
      if (synced) {
        this.set({ synced: true, unsynced: false });
        void this.checkCompile();
      }
    });
    this.doc.on("update", (_update: Uint8Array, origin: unknown) => {
      if (origin !== provider && !provider.wsconnected && !this.state.unsynced) {
        this.set({ unsynced: true });
      }
    });
    provider.on("connection-close", (event) => {
      if (!this.opened && event) options.onConnectionFailed?.();
      this.opened = false;
      if (event?.code === CLOSE_RESYNC) this.onResync();
      // The server may have written them or not: the doc tells once back.
      this.refuseWaiting(
        new CommentRefused(
          "disconnected",
          "The connection dropped before the server answered. If it is not there once reconnected, send it again.",
        ),
      );
    });
    provider.on("closed", ({ code }) => {
      if (code === CLOSE_DELETED) this.set({ deleted: true, status: "deleted" });
      else if (code === CLOSE_SESSION) this.set({ signedOut: true });
      else if (code === CLOSE_TOO_LARGE) this.set({ tooLarge: true });
      else if (code === CLOSE_TOO_MANY) this.set({ tooManyTabs: true });
      else if (code === CLOSE_OUT_OF_STEP) this.set({ outOfStep: true });
    });
    window.addEventListener("online", this.onNetwork);
    window.addEventListener("offline", this.onNetwork);

    this.sql.observe(this.onText);
    this.migration.observe(this.onText);
  }

  /** The text that is the source now: the SQL, or the migration. */
  source(): Y.Text {
    return this.state.mode === "migration" ? this.migration : this.sql;
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
    if (this.state.syncStuck) return "stopped";
    return this.state.synced ? "reconnecting" : "connecting";
  }

  private onNetwork = () => {
    const provider = this.provider;
    if (!provider) return;
    this.set({
      connection: this.connection(provider.wsconnected ? "connected" : "disconnected"),
    });
    // Back online: try now rather than at the end of the backoff.
    if (navigator.onLine && !provider.wsconnected && provider.shouldConnect) {
      provider.disconnect();
      provider.connect();
    }
  };

  /** A local draft: seeded, "connected", compiled by `local.compile` when typing pauses. */
  private startLocal(local: LocalDraft) {
    this.doc.transact(() => {
      this.sql.insert(0, local.sql);
      if (local.migration !== undefined) {
        this.migration.insert(0, local.migration);
        this.meta.set("mode", "migration");
      }
      for (const [key, p] of Object.entries(local.layout ?? {})) this.layout.set(key, p);
      for (const [id, g] of Object.entries(local.groups ?? {})) this.groups.set(id, g);
      for (const [id, t] of Object.entries(local.threads ?? {})) this.threads.set(id, t);
      for (const [id, c] of Object.entries(local.comments ?? {})) this.comments.set(id, c);
    });
    this.set({ connection: "connected", synced: true });
    this.onEvent({
      type: "welcome",
      role: local.role,
      read_only: local.role === "reviewer",
      status: "open",
    });
    let timer = 0;
    const compile = async () => {
      const source = this.source().toJSON();
      const { model, diagnostics } = local.compile(source);
      const event: ServerEvent = {
        type: "compile",
        state: model ? "ok" : "failed",
        sha256: await sha256(source),
        diagnostics,
        model: model as Record<string, never> | null,
      };
      if (!this.destroyed) this.onEvent(event);
    };
    const schedule = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => void compile(), 300);
    };
    this.sql.observe(schedule);
    this.migration.observe(schedule);
    void compile();
  }

  // ---- Server events -----------------------------------------------------------

  private onEvent(event: ServerEvent) {
    if (event.type === "compile") {
      this.onCompile(event);
      return;
    }
    if (event.type === "comment_result") {
      const waiting = this.answers.get(event.id);
      if (!waiting) return;
      this.answers.delete(event.id);
      window.clearTimeout(waiting.timer);
      if (event.ok) waiting.resolve();
      else
        waiting.reject(
          new CommentRefused(event.code ?? "refused", event.message ?? "The server refused it."),
        );
      return;
    }
    this.set(eventPatch(this.state, event));
    if (event.type === "status" && (event.status === "published" || event.status === "deleted")) {
      // Nothing will change it any more: stop reconnecting.
      this.provider?.disconnect();
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
    this.set({
      compile: { ...this.state.compile, state: event.state, model },
      ...(event.schema_sql != null && this.state.mode === "migration"
        ? { schemaSql: event.schema_sql }
        : {}),
    });
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
    const source = this.source();
    const text = source.toJSON();
    const matches = !!event.sha256 && event.sha256 === (await sha256(text));
    if (this.destroyed || seq !== this.compiles || this.lastCompile !== event) return;
    const current = matches && this.source() === source && source.toJSON() === text;
    this.set({
      compile: {
        ...this.state.compile,
        state: event.state,
        // A result for another text than this client's: one for this text follows.
        stale: event.state === "pending" || (event.state !== "unavailable" && !current),
        diagnostics: current ? event.diagnostics : this.state.compile.diagnostics,
        sha256: current && event.state === "ok" ? event.sha256! : null,
      },
    });
  }

  private onText = (event: Y.YTextEvent) => {
    // The other text: not the source, nothing to compile.
    if (event.target !== this.source()) return;
    if (!this.state.compile.stale && this.state.compile.state !== "unavailable") {
      this.set({ compile: { ...this.state.compile, stale: true, sha256: null } });
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
        typing: typeof state.typing === "string" ? state.typing : null,
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

  /** The thread this person is writing in (`new`: starting one), or null. */
  setTyping(thread: string | null) {
    if ((this.awareness.getLocalState()?.typing ?? null) === thread) return;
    this.awareness.setLocalStateField("typing", thread);
  }

  // ---- Comments ----------------------------------------------------------------------

  /**
   * Sends a comment operation; resolves once the server wrote it (its update
   * comes first), rejects with a `CommentRefused` saying why not. Without a
   * server (a local draft), writes it here.
   */
  private sendComment(op: CommentOp, me: Me): Promise<void> {
    if (this.isLocal) {
      this.doc.transact(() => this.writeLocally(op, me), COMMENT);
      return Promise.resolve();
    }
    const provider = this.provider;
    const ws = provider?.ws;
    if (!provider?.wsconnected || !ws || ws.readyState !== ws.OPEN) {
      return Promise.reject(
        new CommentRefused("offline", "Not connected. Send it again once the draft reconnects."),
      );
    }
    const id = crypto.randomUUID();
    const encoder = encoding.createEncoder();
    encoding.writeVarUint(encoder, MESSAGE_COMMENT);
    encoding.writeVarUint8Array(encoder, new TextEncoder().encode(JSON.stringify({ id, op })));
    return new Promise<void>((resolve, reject) => {
      const timer = window.setTimeout(() => {
        this.answers.delete(id);
        reject(
          new CommentRefused(
            "timeout",
            "The server did not answer. Check your connection, then send it again.",
          ),
        );
      }, COMMENT_TIMEOUT);
      this.answers.set(id, { resolve, reject, timer });
      ws.send(encoding.toUint8Array(encoder));
    });
  }

  private refuseWaiting(error: CommentRefused) {
    for (const { reject, timer } of this.answers.values()) {
      window.clearTimeout(timer);
      reject(error);
    }
    this.answers.clear();
  }

  /** A local draft: what the server would write for `op` (no checks to make). */
  private writeLocally(op: CommentOp, me: Me) {
    const at = new Date().toISOString();
    const comment = (thread: string, body: string, mentions: string[]): DocComment => ({
      thread,
      author: { id: me.id, name: me.name },
      body,
      mentions,
      at,
      edited: null,
    });
    switch (op.type) {
      case "start":
        this.threads.set(op.thread, { anchor: op.anchor, by: me.id, at, resolved: null });
        this.comments.set(op.comment, comment(op.thread, op.body, op.mentions));
        break;
      case "reply":
        this.comments.set(op.comment, comment(op.thread, op.body, op.mentions));
        break;
      case "edit": {
        const old = this.comments.get(op.comment) as DocComment | undefined;
        if (old)
          this.comments.set(op.comment, {
            ...old,
            body: op.body,
            mentions: op.mentions,
            edited: at,
          });
        break;
      }
      case "delete": {
        const old = this.comments.get(op.comment) as DocComment | undefined;
        if (!old) break;
        this.comments.delete(op.comment);
        const left = [...this.comments.values()].some(
          (c) => (c as DocComment).thread === old.thread,
        );
        if (!left) this.threads.delete(old.thread);
        break;
      }
      case "delete_thread": {
        for (const [id, c] of [...this.comments.entries()]) {
          if ((c as DocComment).thread === op.thread) this.comments.delete(id);
        }
        this.threads.delete(op.thread);
        break;
      }
      case "resolve": {
        const old = this.threads.get(op.thread) as DocThread | undefined;
        if (old)
          this.threads.set(op.thread, {
            ...old,
            resolved: op.resolved ? { by: me.id, name: me.name, at } : null,
          });
        break;
      }
    }
  }

  /** A new thread with its first comment; its id, once the server wrote it. */
  async startThread(anchor: Anchor, body: string, mentions: string[], me: Me): Promise<string> {
    const thread = crypto.randomUUID();
    const comment = crypto.randomUUID();
    await this.sendComment({ type: "start", thread, comment, anchor, body, mentions }, me);
    return thread;
  }

  reply(thread: string, body: string, mentions: string[], me: Me): Promise<void> {
    const comment = crypto.randomUUID();
    return this.sendComment({ type: "reply", thread, comment, body, mentions }, me);
  }

  /** One's own comment, rewritten. */
  editComment(comment: string, body: string, mentions: string[], me: Me): Promise<void> {
    return this.sendComment({ type: "edit", comment, body, mentions }, me);
  }

  /**
   * One's own comment (anyone's, for developers and admins); the thread
   * goes with its last one (never others' replies).
   */
  deleteComment(comment: string, me: Me): Promise<void> {
    return this.sendComment({ type: "delete", comment }, me);
  }

  /** A thread and every comment in it (others' too, for developers and admins). */
  deleteThread(thread: string, me: Me): Promise<void> {
    return this.sendComment({ type: "delete_thread", thread }, me);
  }

  setResolved(thread: string, resolved: boolean, me: Me): Promise<void> {
    return this.sendComment({ type: "resolve", thread, resolved }, me);
  }

  /** A thread and its first comment written in this tab's own transaction (`---` lines). */
  private writeThread(id: string, anchor: Anchor, body: string, me: Me) {
    const at = new Date().toISOString();
    const thread: DocThread = { anchor, by: me.id, at, resolved: null };
    this.threads.set(id, thread);
    const comment: DocComment = {
      thread: id,
      author: { id: me.id, name: me.name },
      body,
      mentions: [],
      at,
      edited: null,
    };
    this.comments.set(crypto.randomUUID(), comment);
  }

  /**
   * A `---` line typed in the source becomes a comment pinned to `anchor`
   * (the statement below it): the line leaves the text in the same
   * transaction, one undo step, so the SQL never holds it. `from`/`to`: the
   * line, its line break included.
   */
  commentFromLine(range: { from: number; to: number }, body: string, anchor: Anchor, me: Me) {
    const text = this.source();
    const id = crypto.randomUUID();
    this.undo.stopCapturing();
    this.doc.transact(() => {
      text.delete(range.from, range.to - range.from);
      this.writeThread(id, anchor, body, me);
    }, LOCAL);
    this.undo.stopCapturing();
    return id;
  }

  /**
   * Text pasted with `---` lines in it: the text without them replaces
   * `range`, and each becomes a thread on the statement below it, in one
   * transaction (one undo step).
   */
  pasteWithComments(
    range: { from: number; to: number },
    insert: string,
    comments: { body: string; anchor: Anchor }[],
    me: Me,
  ) {
    const text = this.source();
    this.undo.stopCapturing();
    this.doc.transact(() => {
      if (range.to > range.from) text.delete(range.from, range.to - range.from);
      if (insert) text.insert(range.from, insert);
      for (const { body, anchor } of comments)
        this.writeThread(crypto.randomUUID(), anchor, body, me);
    }, LOCAL);
    this.undo.stopCapturing();
  }

  /** Threads pinned to a renamed table or column follow it (in the edit's transaction). */
  private moveAnchors(rename: (anchor: Anchor) => Anchor) {
    for (const [id, value] of this.threads.entries()) {
      const thread = value as DocThread;
      if (!thread?.anchor) continue;
      const anchor = rename(thread.anchor);
      if (anchor !== thread.anchor) this.threads.set(id, { ...thread, anchor });
    }
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

  /**
   * One step of someone's, alone in the undo history: never merged with
   * what came just before or after (a move, a rename typed a moment ago).
   */
  private step(write: () => void) {
    this.undo.stopCapturing();
    this.doc.transact(write, LOCAL);
    this.undo.stopCapturing();
  }

  private writeGroups(change: GroupsChange) {
    for (const [id, g] of Object.entries(change.set)) this.groups.set(id, g);
    for (const id of change.deleted) this.groups.delete(id);
  }

  /**
   * A new group of these tables, named and colored by default; its id. A
   * table belongs to one group: it leaves the others (a group left empty
   * goes).
   */
  createGroup(members: string[]): string {
    const groups = Object.fromEntries(this.groups.entries());
    const used = new Set(Object.values(groups).map((g) => g.color));
    const color =
      GROUP_ORDER.find((c) => !used.has(c)) ?? GROUP_ORDER[this.groups.size % GROUP_ORDER.length]!;
    const id = crypto.randomUUID().slice(0, 8);
    const group: Group = { name: nextGroupName(groups), color, members: [] };
    this.step(() => {
      this.writeGroups(regroup({ ...groups, [id]: group }, members, id));
    });
    return id;
  }

  updateGroup(id: string, patch: Partial<Pick<Group, "name" | "color">>) {
    const group = this.groups.get(id);
    if (!group) return;
    this.step(() => this.groups.set(id, { ...group, ...patch }));
  }

  /** The group goes; its tables stay where they are. */
  deleteGroup(id: string) {
    if (!this.groups.has(id)) return;
    this.step(() => this.groups.delete(id));
  }

  /** Tables into a group (null: out of theirs), from a menu. */
  assignGroup(tables: string[], group: string | null) {
    const change = regroup(Object.fromEntries(this.groups.entries()), tables, group);
    if (Object.keys(change.set).length === 0 && change.deleted.length === 0) return;
    this.step(() => this.writeGroups(change));
  }

  /**
   * Tables moved by hand (a table dragged, a group dragged with all its
   * tables) and, when the drop decided it, the group they belong to now:
   * one transaction, one undo step, seen by everyone at once.
   */
  placeTables(positions: Layout, membership?: { tables: string[]; group: string | null }) {
    const change = membership
      ? regroup(Object.fromEntries(this.groups.entries()), membership.tables, membership.group)
      : null;
    this.step(() => {
      for (const [key, p] of Object.entries(positions))
        this.layout.set(key, { x: Math.round(p.x), y: Math.round(p.y) });
      if (change) this.writeGroups(change);
    });
  }

  /**
   * This tab is asking the server to rewrite the SQL (Tidy): the next rewrite
   * is its own. `false` when the request failed or changed nothing.
   */
  expectRewrite(expected = true) {
    this.rewriteExpected = expected;
  }

  /**
   * The server rewrote the SQL. The rewrite reuses characters this tab
   * typed or a graph edit appended, in other places: undoing anything from
   * before it would delete them wherever they are now (and move layout
   * entries back under names the text no longer has). So the history goes,
   * for everyone, with the edits judged on it (their statements are folded
   * into the new text). Seen in the same transaction as the rewrite, also by
   * a tab that syncs it later.
   */
  private onMeta = (event: Y.YMapEvent<unknown>) => {
    if (event.keysChanged.has("mode")) this.onMode();
    if (event.transaction.local || !event.keysChanged.has("rewrite")) return;
    const note = this.meta.get("rewrite") as { count?: unknown; reason?: unknown } | undefined;
    const had = this.undo.undoStack.length + this.undo.redoStack.length > 0;
    const mine = this.rewriteExpected;
    this.rewriteExpected = false;
    this.undo.clear();
    this.undoItems.clear();
    this.set({
      edits: [],
      historyCleared: had
        ? {
            reason: typeof note?.reason === "string" ? note.reason : "rewrite",
            count: typeof note?.count === "number" ? note.count : 0,
            mine,
          }
        : this.state.historyCleared,
    });
  };

  /**
   * The doc's mode changed (a switch, or the doc arriving): the other text
   * is the source now. The undo history goes, silently (the switch is told
   * apart): undoing typing in a text that is no longer the source would do
   * nothing anyone sees, and a graph edit's statements were in the old one.
   */
  private onMode() {
    const mode: DraftMode = this.meta.get("mode") === "migration" ? "migration" : "schema";
    if (mode === this.state.mode) return;
    this.undo.clear();
    this.undoItems.clear();
    this.set({
      mode,
      edits: [],
      schemaSql: mode === "migration" ? this.state.schemaSql : null,
      compile: { ...this.state.compile, stale: true, sha256: null, diagnostics: [] },
    });
    void this.checkCompile();
  }

  /**
   * A schema edit from the graph (docs/MVP.md "Schema edits from the graph
   * append SQL"): its statements appended under the marker, and the layout
   * change that goes with it (a renamed table's entries move, a new table is
   * placed), in one transaction that one undo takes back. `model` is what
   * the graph showed: the edit's effect is measured against it.
   */
  editSchema(edit: GraphEdit, model: Schema, position?: Position): EditEntry {
    const statements = sqlFor(edit);
    // In migration mode the migration takes the statements: they are a migration's.
    const text = this.source();
    const append = appendTo(text.toJSON(), statements);
    const change = layoutChange(
      edit,
      position && { x: Math.round(position.x), y: Math.round(position.y) },
    );
    // Its own step in the history, whatever came just before or after.
    this.undo.stopCapturing();
    this.doc.transact(() => {
      text.insert(append.at, append.insert);
      this.moveLayout(change);
      if (edit.kind === "renameColumn") {
        const table = `${edit.table.schema}.${edit.table.name}`;
        this.moveAnchors((a) => renameColumn(a, table, edit.column, edit.to));
      }
    }, LOCAL);
    this.undo.stopCapturing();
    const entry: EditEntry = {
      id: ++this.lastEdit,
      edit,
      sql: statements.join("\n"),
      touched: touchedEntities(model, applyEdit(model, edit)),
    };
    this.undoItems.set(entry.id, this.undo.undoStack.at(-1));
    const edits = [...this.state.edits, entry];
    for (const gone of edits.splice(0, Math.max(0, edits.length - KEPT_EDITS)))
      this.undoItems.delete(gone.id);
    this.set({ edits });
    return entry;
  }

  /**
   * Takes back one schema edit (one that failed): an undo when it is the
   * last thing done here; otherwise its statements leave the text (a
   * renamed table moving back), as a step of its own.
   */
  undoSchemaEdit(id: number) {
    const entry = this.state.edits.find((e) => e.id === id);
    if (!entry) return;
    const item = this.undoItems.get(id);
    if (item !== undefined && this.undo.undoStack.at(-1) === item) {
      this.undo.undo();
      return;
    }
    const source = this.source();
    const text = source.toJSON();
    const at = text.lastIndexOf(entry.sql);
    if (at < 0) return;
    const end = at + entry.sql.length + (text[at + entry.sql.length] === "\n" ? 1 : 0);
    const change = layoutChange(entry.edit);
    this.undo.stopCapturing();
    this.doc.transact(() => {
      source.delete(at, end - at);
      if (change.rename) this.moveLayout({ rename: [change.rename[1], change.rename[0]] });
      const edit = entry.edit;
      if (edit.kind === "renameColumn") {
        const table = `${edit.table.schema}.${edit.table.name}`;
        this.moveAnchors((a) => renameColumn(a, table, edit.to, edit.column));
      }
    }, LOCAL);
    this.undo.stopCapturing();
  }

  private moveLayout(change: LayoutChange) {
    if (change.rename) {
      const [from, to] = change.rename;
      const position = this.layout.get(from);
      if (position) {
        this.layout.set(to, position);
        this.layout.delete(from);
      }
      for (const [id, group] of this.groups.entries()) {
        if (!group.members.includes(from)) continue;
        const members = [...new Set(group.members.map((m) => (m === from ? to : m)))];
        this.groups.set(id, { ...group, members });
      }
      this.moveAnchors((a) => renameTable(a, from, to));
    }
    if (change.place) {
      const { key, x, y } = change.place;
      this.layout.set(key, { x, y });
    }
  }

  /**
   * The server asked this tab to sync again (another tab announced the id
   * it writes with): y-websocket reconnects; past a few times in a minute,
   * it stops and says so (`syncStuck`), never looping unseen.
   */
  private onResync() {
    const now = Date.now();
    this.resyncs = [...this.resyncs.filter((at) => now - at < 60_000), now];
    if (this.resyncs.length < MAX_RESYNCS) return;
    // After y-websocket has finished closing (it schedules the reconnect).
    queueMicrotask(() => this.provider?.disconnect());
    this.set({ syncStuck: true, connection: "stopped" });
  }

  /** Tries again after `syncStuck`. */
  retrySync() {
    this.resyncs = [];
    this.set({ syncStuck: false });
    this.provider?.connect();
  }

  destroy() {
    this.destroyed = true;
    this.refuseWaiting(new CommentRefused("closed", "The draft was closed."));
    window.clearTimeout(this.recheck);
    window.removeEventListener("online", this.onNetwork);
    window.removeEventListener("offline", this.onNetwork);
    this.sql.unobserve(this.onText);
    this.migration.unobserve(this.onText);
    this.meta.unobserve(this.onMeta);
    this.listeners.clear();
    this.undo.destroy();
    this.provider?.destroy();
    if (!this.provider) this.awareness.destroy();
    this.doc.destroy();
  }
}
