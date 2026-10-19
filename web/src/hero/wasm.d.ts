// The two wasm modules the hero loads. `romulus-wasm` is crates/romulus-wasm
// built by wasm-bindgen (site/tools/hero.mjs puts it where vite.hero.config.ts
// resolves the name); `libpg-query`'s emscripten glue ships without types.

declare module "romulus-wasm" {
  export default function init(options?: { module_or_path: string | URL }): Promise<unknown>;
  export function compile(sql: string, tree: string): string;
  export function syntaxError(sql: string, message: string, cursor: number): string;
  export function diff(before: string, after: string): string;
  export function migration(before: string, after: string, request: string): string;
  export function texts(model: string): string;
  /** A fresh instance, after a trap (wasm-bindgen's --experimental-reset-state-function). */
  export function __wbg_reset_state(): void;
}

declare module "libpg-query/wasm/libpg-query.js" {
  export interface PgQueryModule {
    _malloc(size: number): number;
    _free(pointer: number): void;
    _wasm_parse_query_raw(query: number): number;
    _wasm_free_parse_result(result: number): void;
    lengthBytesUTF8(text: string): number;
    stringToUTF8(text: string, pointer: number, size: number): void;
    UTF8ToString(pointer: number): string;
    getValue(pointer: number, type: "i32"): number;
  }
  const load: (options?: { locateFile?: (path: string) => string }) => Promise<PgQueryModule>;
  export default load;
}
