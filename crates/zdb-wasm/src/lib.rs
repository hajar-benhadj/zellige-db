//! ZelligeDB compiled to WebAssembly: the browser playground backend.
//!
//! The same engine — pager, B+Tree, WAL, SQL — runs over plain memory via
//! the `PageFile` abstraction, so a web page can create tables, insert
//! rows, and run transactions without any server. The bindings only exist
//! for the wasm32 target; host builds compile this crate as an empty lib
//! so `cargo test --workspace` stays green everywhere.

#[cfg(target_arch = "wasm32")]
pub mod bindings {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    pub struct ZelligeDb {
        engine: zdb_sql::SqlEngine,
    }

    #[wasm_bindgen]
    impl ZelligeDb {
        #[wasm_bindgen(constructor)]
        pub fn new() -> Result<ZelligeDb, JsValue> {
            let engine = zdb_sql::SqlEngine::create_memory()
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            Ok(ZelligeDb { engine })
        }

        /// Execute a (possibly multi-statement) SQL batch. Returns the
        /// ASCII result tables — the same rendering the REPL prints.
        /// Statement errors are reported inline, psql-style.
        pub fn execute(&mut self, sql: &str) -> Result<String, JsValue> {
            let mut rendered = String::new();
            let result = self
                .engine
                .execute_batch(sql, |output| {
                    rendered.push_str(&zdb_sql::output::render(&output));
                })
                .map_err(|e| JsValue::from_str(&e.to_string()));
            match result {
                Ok(()) => Ok(rendered),
                Err(e) => {
                    rendered.push_str(&format!("ERROR: {}\n", e.as_string().unwrap_or_default()));
                    Ok(rendered)
                }
            }
        }
    }

    #[wasm_bindgen]
    pub fn version() -> String {
        format!("ZelligeDB v{} (wasm)", env!("CARGO_PKG_VERSION"))
    }
}
