//! The functions the hero calls from JavaScript: JSON strings in, JSON
//! strings out (`{"error": "…"}` when an argument does not read).

use remus_core::Schema;
use serde::Serialize;
use wasm_bindgen::prelude::wasm_bindgen;

use crate::MigrationRequest;

/// A panic aborts the instance (the hero makes a new one): its message goes
/// to the console first, so a report can say where. Set by every entry, as
/// an instance made afresh has none.
fn hook() {
    #[cfg(target_arch = "wasm32")]
    {
        static HOOK: std::sync::Once = std::sync::Once::new();
        HOOK.call_once(|| {
            std::panic::set_hook(Box::new(|info| {
                console_error(&format!("romulus-wasm {info}"));
            }));
        });
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(message: &str);
}

fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|e| error(&e.to_string()))
}

fn error(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

fn model(json: &str) -> Result<Schema, String> {
    Schema::from_json(json).map_err(|e| format!("not a remus model: {e}"))
}

/// `Compiled` of `sql`, from `libpg_query`'s JSON parse tree.
#[wasm_bindgen(js_name = compile)]
pub fn js_compile(sql: &str, tree: &str) -> String {
    hook();
    json(&crate::compile(sql, tree))
}

/// `Compiled` of a text `libpg_query` refused: its message, its `cursorpos`.
#[wasm_bindgen(js_name = syntaxError)]
pub fn js_syntax_error(sql: &str, message: &str, cursor: u32) -> String {
    hook();
    json(&crate::syntax_error(sql, message, cursor))
}

/// `{diff, sql: {before, after}}` of two models.
#[wasm_bindgen(js_name = diff)]
pub fn js_diff(before: &str, after: &str) -> String {
    hook();
    match (model(before), model(after)) {
        (Ok(before), Ok(after)) => json(&crate::diff(&before, &after)),
        (Err(e), _) | (_, Err(e)) => error(&e),
    }
}

/// The API's `SchemaMigration` from one model to the other.
#[wasm_bindgen(js_name = migration)]
pub fn js_migration(before: &str, after: &str, request: &str) -> String {
    hook();
    let request: MigrationRequest = match serde_json::from_str(request) {
        Ok(r) => r,
        Err(e) => return error(&format!("not a migration request: {e}")),
    };
    match (model(before), model(after)) {
        (Ok(before), Ok(after)) => json(&crate::migration(&before, &after, &request)),
        (Err(e), _) | (_, Err(e)) => error(&e),
    }
}

/// `{json, dbml, mermaid}` of a model.
#[wasm_bindgen(js_name = texts)]
pub fn js_texts(model_json: &str) -> String {
    hook();
    match model(model_json) {
        Ok(schema) => json(&crate::texts(&schema)),
        Err(e) => error(&e),
    }
}
