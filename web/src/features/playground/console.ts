// A playground's console session: one websocket, one Postgres session on the
// playground host (read-only for everyone in phase 1). The API passes the
// host's results through (`ConsoleEvent`) and adds what everyone in the
// playground hears (`ConsoleNotice`: the shared history, who is here,
// resets and migration runs). This store folds both into one state that
// React reads with useSyncExternalStore (`use-console.ts`).

import type { components } from "@/api/schema.gen";

export type ConsoleEvent = components["schemas"]["ConsoleEvent"];
export type ConsoleNotice = components["schemas"]["ConsoleNotice"];
export type ConsoleRequest = components["schemas"]["ConsoleRequest"];
export type ConsoleColumn = components["schemas"]["ConsoleColumn"];
export type SqlError = components["schemas"]["PlaygroundSqlError"];
export type TextSpan = components["schemas"]["PlaygroundTextSpan"];
export type Txn = components["schemas"]["ConsoleTxn"];
export type RunStatus = components["schemas"]["ConsoleRunStatus"];
export type PlaygroundRun = components["schemas"]["PlaygroundRun"];
export type Person = components["schemas"]["Person"];
export type Announcement = components["schemas"]["Announcement"];

/** One statement's result. */
export interface StatementResult {
  index: number;
  span: TextSpan;
  columns: ConsoleColumn[];
  rows: (string | null)[][];
  /** Postgres's tag (`SELECT 212`), once done. */
  tag: string | null;
  rowCount: number | null;
  /** Only the first 1 000 rows were kept. */
  truncated: boolean;
  /** Some values were longer than 64 KiB and are cut. */
  cellsCut: boolean;
  ms: number | null;
  plan: string | null;
  notices: { severity: string; message: string }[];
  error: SqlError | null;
}

export interface Run {
  id: string;
  /** The text it ran (spans are in it). */
  sql: string;
  /** Where that text starts in the editor (a selection, a statement). */
  offset: number;
  explain: boolean;
  startedAt: number;
  /** Waiting for a free slot on the host. */
  queued: boolean;
  statements: StatementResult[];
  /** Once finished. */
  status: RunStatus | null;
  ms: number | null;
  /** The run's own error (refused before any statement ran). */
  error: string | null;
}

export type Connection =
  /** Opening the socket. */
  | "connecting"
  /** The playground's database is starting (it stops when unused). */
  | "starting"
  | "ready"
  /** Someone resets or migrates it: `pausedBecause`. It comes back by itself. */
  | "paused"
  /** Its database was stopped (unused, or room for another): `pausedBecause`. A run starts it. */
  | "stopped"
  /** It could not start: `unavailable` says why. Try again (or a run) retries. */
  | "unavailable"
  /** Lost: retrying. */
  | "reconnecting"
  /** Over for good: `closed` says why. */
  | "closed";

/** Why the playground could not start (`unavailable`). */
export interface Unavailable {
  /** `host_full`, `org_full`, `not_starting`, `unreachable`. */
  code: string;
  reason: string;
}

/** Whether a run can be sent now: the host starts a stopped playground for it. */
export function canRun(connection: Connection): boolean {
  return (
    connection === "ready" ||
    connection === "stopped" ||
    connection === "unavailable" ||
    connection === "paused"
  );
}

export interface Notice {
  id: number;
  event: Announcement;
  by: Person;
  at: number;
}

export interface ConsoleState {
  connection: Connection;
  /** Why it is paused or stopped. */
  pausedBecause: string | null;
  /** Why it could not start. */
  unavailable: Unavailable | null;
  /** Why it closed for good. */
  closed: "deleted" | "membership" | "signed_out" | "too_many" | "unavailable" | null;
  txn: Txn;
  /** Newest last. */
  runs: Run[];
  /** The run in progress. */
  running: string | null;
  present: Person[];
  /** History entries recorded while this page is open, newest first. */
  history: PlaygroundRun[];
  /** The last announcement (reset, migration run…). */
  notice: Notice | null;
}

const INITIAL: ConsoleState = {
  connection: "connecting",
  pausedBecause: null,
  unavailable: null,
  closed: null,
  txn: "idle",
  runs: [],
  running: null,
  present: [],
  history: [],
  notice: null,
};

/** Runs kept in memory (each holds at most 1 000 rows per statement). */
const KEEP_RUNS = 20;

const emptyStatement = (index: number, span: TextSpan): StatementResult => ({
  index,
  span,
  columns: [],
  rows: [],
  tag: null,
  rowCount: null,
  truncated: false,
  cellsCut: false,
  ms: null,
  plan: null,
  notices: [],
  error: null,
});

/** Applies one message from the socket to the state: pure, for tests. */
export function reduce(state: ConsoleState, message: ConsoleEvent | ConsoleNotice): ConsoleState {
  const withRun = (id: string, update: (run: Run) => Run): ConsoleState => ({
    ...state,
    runs: state.runs.map((run) => (run.id === id ? update(run) : run)),
  });
  const withStatement = (
    id: string,
    index: number,
    update: (statement: StatementResult) => StatementResult,
  ) =>
    withRun(id, (run) => ({
      ...run,
      statements: run.statements.map((s) => (s.index === index ? update(s) : s)),
    }));

  switch (message.type) {
    case "starting":
      return { ...state, connection: "starting", unavailable: null };
    case "ready":
      return {
        ...state,
        connection: "ready",
        pausedBecause: null,
        unavailable: null,
        closed: null,
        txn: message.txn,
      };
    case "paused":
      return {
        ...state,
        connection: "paused",
        pausedBecause: message.reason,
        txn: "idle",
      };
    case "stopped":
      return {
        ...state,
        connection: "stopped",
        pausedBecause: message.reason,
        txn: "idle",
      };
    case "deleting":
      // The socket closes (4404) once it is done; a failed deletion resumes.
      return { ...state, connection: "closed", closed: "deleted", txn: "idle" };
    case "restarted":
      // Its pause and resume reach everyone else: nothing to show here.
      return state;
    case "unavailable":
      return {
        ...state,
        connection: "unavailable",
        unavailable: { code: message.code, reason: message.reason },
        txn: "idle",
      };
    case "queued":
      return withRun(message.id, (run) => ({ ...run, queued: true }));
    case "statement":
      return withRun(message.id, (run) => ({
        ...run,
        queued: false,
        statements: [...run.statements, emptyStatement(message.index, message.span)],
      }));
    case "columns":
      return withStatement(message.id, message.index, (s) => ({ ...s, columns: message.columns }));
    case "rows":
      return withStatement(message.id, message.index, (s) => ({
        ...s,
        rows: s.rows.concat(message.rows),
      }));
    case "plan":
      return withStatement(message.id, message.index, (s) => ({ ...s, plan: message.text }));
    case "notice":
      return withStatement(message.id, message.index, (s) => ({
        ...s,
        notices: [...s.notices, { severity: message.severity, message: message.message }],
      }));
    case "done":
      return withStatement(message.id, message.index, (s) => ({
        ...s,
        tag: message.tag,
        rowCount: message.rows,
        truncated: message.truncated,
        cellsCut: message.cells_cut ?? false,
        ms: message.ms,
      }));
    case "error": {
      const known = state.runs
        .find((run) => run.id === message.id)
        ?.statements.some((s) => s.index === message.index);
      if (!known) {
        // A syntax error: no statement started.
        return withRun(message.id, (run) => ({
          ...run,
          statements: [
            ...run.statements,
            {
              ...emptyStatement(
                message.index,
                message.error.span ?? { start: 0, end: 0, utf16_start: 0, utf16_end: 0 },
              ),
              error: message.error,
            },
          ],
        }));
      }
      return withStatement(message.id, message.index, (s) => ({ ...s, error: message.error }));
    }
    case "finished": {
      const next = withRun(message.id, (run) => ({
        ...run,
        queued: false,
        status: message.status,
        ms: message.ms,
        error: run.statements.length === 0 ? (message.error ?? null) : null,
      }));
      return { ...next, running: state.running === message.id ? null : state.running };
    }
    case "txn":
      return { ...state, txn: message.txn };
    case "history":
      return {
        ...state,
        history: [message.run, ...state.history.filter((r) => r.id !== message.run.id)].slice(
          0,
          100,
        ),
      };
    case "presence":
      return { ...state, present: message.people };
    case "announcement":
      return {
        ...state,
        notice: {
          id: (state.notice?.id ?? 0) + 1,
          event: message.event,
          by: message.by,
          at: Date.now(),
        },
      };
  }
}

/** Starts a run in the state (before the socket hears of it). */
export function begin(
  state: ConsoleState,
  id: string,
  sql: string,
  explain: boolean,
  offset = 0,
): ConsoleState {
  const run: Run = {
    id,
    sql,
    offset,
    explain,
    startedAt: Date.now(),
    queued: false,
    statements: [],
    status: null,
    ms: null,
    error: null,
  };
  return { ...state, runs: [...state.runs, run].slice(-KEEP_RUNS), running: id };
}

/** Close codes of the API's console socket (playgrounds::console). */
const FINAL: Record<number, ConsoleState["closed"]> = {
  4001: "membership",
  4401: "signed_out",
  4404: "deleted",
  4429: "too_many",
};

type Listener = () => void;

/** The console's socket and state. Create in an effect, `close()` in its cleanup. */
export class ConsoleSession {
  private socket: WebSocket | null = null;
  private state: ConsoleState = INITIAL;
  private listeners = new Set<Listener>();
  private retries = 0;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private closedByUs = false;
  private nextRun = 1;

  private readonly url: string;

  constructor(url: string) {
    this.url = url;
    this.open();
  }

  getState = (): ConsoleState => this.state;

  subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  private set(state: ConsoleState) {
    this.state = state;
    for (const listener of this.listeners) listener();
  }

  private open() {
    const socket = new WebSocket(this.url);
    this.socket = socket;
    socket.onopen = () => {
      this.retries = 0;
    };
    socket.onmessage = (event: MessageEvent<string>) => {
      try {
        this.set(reduce(this.state, JSON.parse(event.data) as ConsoleEvent | ConsoleNotice));
      } catch {
        // Not ours: ignored.
      }
    };
    socket.onclose = (event) => {
      if (this.socket !== socket || this.closedByUs) return;
      this.socket = null;
      // A run in progress is lost with its session.
      const runs = this.state.runs.map((run) =>
        run.status === null
          ? { ...run, status: "cancelled" as const, error: "The connection was lost." }
          : run,
      );
      const final = FINAL[event.code];
      if (final) {
        this.set({ ...this.state, runs, running: null, connection: "closed", closed: final });
        return;
      }
      // 1011: the host is unavailable; anything else: retry, backing off.
      this.retries += 1;
      if (this.retries > 6) {
        this.set({
          ...this.state,
          runs,
          running: null,
          connection: "closed",
          closed: "unavailable",
        });
        return;
      }
      this.set({ ...this.state, runs, running: null, connection: "reconnecting", txn: "idle" });
      const delay = Math.min(1000 * 2 ** (this.retries - 1), 15_000);
      this.retryTimer = setTimeout(() => this.open(), delay);
    };
  }

  private send(request: ConsoleRequest): boolean {
    if (this.socket?.readyState !== WebSocket.OPEN) return false;
    this.socket.send(JSON.stringify(request));
    return true;
  }

  /** Runs `sql` (every statement, stopping at the first error), or explains it. A
   *  stopped playground starts for it. */
  run(sql: string, explain = false, offset = 0): string | null {
    if (this.state.running || !canRun(this.state.connection)) return null;
    const id = `r${this.nextRun++}`;
    if (!this.send({ type: "run", id, sql, explain })) return null;
    this.set(begin(this.state, id, sql, explain, offset));
    return id;
  }

  /** Stops the run in progress. */
  cancel() {
    if (this.state.running) this.send({ type: "cancel", id: this.state.running });
  }

  /** Starts a stopped playground, or tries again after it could not start. */
  start() {
    const { connection } = this.state;
    if (connection !== "stopped" && connection !== "unavailable") return;
    if (this.send({ type: "open" })) this.set({ ...this.state, connection: "starting" });
  }

  /** Retries after the connection closed for good (the host was unavailable). */
  retry() {
    if (this.socket || this.state.closed !== "unavailable") return;
    this.retries = 0;
    this.set({ ...this.state, connection: "connecting", closed: null });
    this.open();
  }

  close() {
    this.closedByUs = true;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.socket?.close(1000);
    this.socket = null;
    this.listeners.clear();
  }
}

/** The console's websocket URL for a playground. */
export function consoleUrl(org: string, db: string, id: string): string {
  const { protocol, host } = window.location;
  const scheme = protocol === "https:" ? "wss:" : "ws:";
  const path = [org, db, id].map(encodeURIComponent);
  return `${scheme}//${host}/api/orgs/${path[0]}/databases/${path[1]}/playgrounds/${path[2]}/console`;
}
