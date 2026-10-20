// romulus without a server: Postgres 17's own parser (libpg_query, built to
// wasm by the `libpg-query` package, the grammar the app's server reads
// with) and crates/romulus-wasm, which builds the remus model from the parse
// tree and runs romulus-diff and romulus-migration. Nothing leaves the page.

import loadPgQuery, { type PgQueryModule } from "libpg-query/wasm/libpg-query.js";
import pgQueryWasm from "libpg-query/wasm/libpg-query.wasm?url";
import initRomulus, * as romulus from "romulus-wasm";
import romulusWasm from "romulus-wasm/romulus_wasm_bg.wasm?url";

import type { Diagnostic } from "@/features/draft/queries";
import type { SchemaDiff } from "@/features/diff/changes";
import type { MigrationChoice, SchemaMigration } from "@/features/migration/queries";
import { parseSchema, type Schema } from "@/features/schema/model";

export interface Compiled {
  model: Schema | null;
  diagnostics: Diagnostic[];
}

export interface Compared {
  diff: SchemaDiff;
  sql: { before: string; after: string };
}

/** What the files are called, as the API names them for two versions. */
export interface MigrationNames {
  from: number | null;
  to: number;
  fromLabel: string;
  toLabel: string;
  database: string;
}

/** A model as remus emits it, for the text pane's other tabs. */
export interface Texts {
  json: string;
  dbml: string;
  mermaid: string;
}

/**
 * Never throws for what the SQL is: a failure of the engine itself (a wasm
 * trap, a parser that aborted) is reported as an error on the text, the
 * module is started afresh, and the next call works. Only when that restart
 * fails does `onFatal` fire: the hero then gives up and the page shows its
 * picture again.
 */
export interface Engine {
  compile(sql: string): Compiled;
  /** Null when the engine failed (the tabs then show the SQL only). */
  texts(model: Schema): Texts | null;
  /** Null when the engine failed. */
  diff(before: Schema, after: Schema): Compared | null;
  /** Throws `EngineError` when the engine failed (the view shows its error). */
  migration(
    before: Schema,
    after: Schema,
    choice: MigrationChoice,
    names: MigrationNames,
  ): SchemaMigration;
  onFatal(listener: (error: unknown) => void): () => void;
}

/** The database the hero's models are of. */
export const DATABASE = "billing";

export class EngineError extends Error {}

function read<T>(json: string): T {
  const value = JSON.parse(json) as T | { error: string };
  if (value && typeof value === "object" && "error" in value && typeof value.error === "string")
    throw new EngineError(value.error);
  return value as T;
}

/** What a compile answers when the engine itself failed on the text. */
const FAILED: Compiled = {
  model: null,
  diagnostics: [
    {
      severity: "error",
      code: "unavailable",
      message: "The browser preview could not check this SQL.",
      hint: "Edit it to check again; the app compiles it in a real Postgres.",
    },
  ],
};

/** The parse tree of `sql` as libpg_query's JSON, or its syntax error. */
function parse(
  pg: PgQueryModule,
  sql: string,
): { tree: string } | { message: string; cursor: number } {
  const size = pg.lengthBytesUTF8(sql) + 1;
  const query = pg._malloc(size);
  let result = 0;
  try {
    pg.stringToUTF8(sql, query, size);
    result = pg._wasm_parse_query_raw(query);
    // struct { char *parse_tree; char *stderr_buffer; PgQueryError *error; }
    const tree = pg.getValue(result, "i32");
    const error = pg.getValue(result + 8, "i32");
    if (error) {
      // struct { char *message; char *funcname; char *filename; int lineno; int cursorpos; … }
      const message = pg.UTF8ToString(pg.getValue(error, "i32"));
      return { message, cursor: pg.getValue(error + 16, "i32") };
    }
    return { tree: pg.UTF8ToString(tree) };
  } finally {
    pg._free(query);
    if (result) pg._wasm_free_parse_result(result);
  }
}

function today() {
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  const date = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
  const time = `${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
  return { date, timestamp: date.replaceAll("-", "") + time };
}

const loadParser = () => loadPgQuery({ locateFile: () => pgQueryWasm });

let loading: Promise<Engine> | null = null;
/** An engine died: the next load starts romulus-wasm afresh. */
let poisoned = false;

/** Both wasm modules, fetched and compiled once (again after a fatal failure). */
export function loadEngine(): Promise<Engine> {
  loading ??= Promise.all([loadParser(), initRomulus({ module_or_path: romulusWasm })]).then(
    ([parser]) => {
      if (poisoned) {
        // init() keeps the instance it made: replace it.
        romulus.__wbg_reset_state();
        poisoned = false;
      }
      return engine(parser);
    },
  );
  // A failed load can be tried again.
  loading.catch(() => {
    loading = null;
  });
  return loading;
}

function engine(parser: PgQueryModule): Engine {
  let pg: PgQueryModule | null = parser;
  const fatal = new Set<(error: unknown) => void>();
  let dead = false;
  const die = (error: unknown) => {
    if (dead) return;
    dead = true;
    poisoned = true;
    loading = null;
    console.error("romulus: the live editor's engine cannot restart", error);
    fatal.forEach((listener) => listener(error));
  };

  /**
   * Runs `work` on romulus-wasm. A trap (a panic: the module aborts) leaves
   * its memory in an unknown state: it is instantiated afresh, and the
   * caller answers with its failure value. `EngineError`s are answers.
   */
  const guarded = <T>(work: () => T, failed: () => T): T => {
    if (dead) return failed();
    try {
      return work();
    } catch (error) {
      if (error instanceof EngineError) throw error;
      console.error("romulus: the live editor's engine failed; starting it again", error);
      try {
        romulus.__wbg_reset_state();
      } catch (reset) {
        die(reset);
      }
      return failed();
    }
  };

  /** The parse tree, or null when the parser itself failed (it then restarts). */
  const parseWith = (sql: string): ReturnType<typeof parse> | null => {
    if (!pg) return null;
    try {
      return parse(pg, sql);
    } catch (error) {
      // An emscripten module that aborted stays aborted: load a new one.
      console.error("romulus: the live editor's parser failed; loading it again", error);
      pg = null;
      loadParser().then((fresh) => {
        pg = fresh;
      }, die);
      return null;
    }
  };

  return {
    compile(sql) {
      const parsed = parseWith(sql);
      if (!parsed) return FAILED;
      return guarded(
        () => {
          const compiled = read<{ model: object | null; diagnostics: Diagnostic[] }>(
            "tree" in parsed
              ? romulus.compile(sql, parsed.tree)
              : romulus.syntaxError(sql, parsed.message, parsed.cursor),
          );
          const model = compiled.model
            ? parseSchema({ ...compiled.model, database: DATABASE })
            : null;
          return { model, diagnostics: compiled.diagnostics };
        },
        () => FAILED,
      );
    },
    texts(model) {
      return guarded(
        () => read<Texts>(romulus.texts(JSON.stringify(model))),
        () => null,
      );
    },
    diff(before, after) {
      return guarded(
        () => read<Compared>(romulus.diff(JSON.stringify(before), JSON.stringify(after))),
        () => null,
      );
    },
    migration(before, after, choice, names) {
      const { date, timestamp } = today();
      return guarded(
        () =>
          read<SchemaMigration>(
            romulus.migration(
              JSON.stringify(before),
              JSON.stringify(after),
              JSON.stringify({
                format: choice.format,
                concurrently: choice.concurrently,
                version: choice.version,
                from: names.from,
                to: names.to,
                from_label: names.fromLabel,
                to_label: names.toLabel,
                database: names.database,
                date,
                timestamp,
                major: 17,
                renames: choice.renames ?? {},
              }),
            ),
          ),
        () => {
          throw new EngineError("The browser preview could not write this migration.");
        },
      );
    },
    onFatal(listener) {
      fatal.add(listener);
      return () => fatal.delete(listener);
    },
  };
}
