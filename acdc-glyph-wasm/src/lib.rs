//! Parse-only WASM bindings for `acdc-parser`, designed for the Glyph editor.
//!
//! Exports two parse entry points that mirror the native API but return
//! serde-serialized AST graphs as plain JS objects (via `serde-wasm-bindgen`):
//!
//! - `parse_block(source)` — full document parse, returns the `Document` ASG
//! - `parse_inline(source)` — inline-only parse, returns `InlineNode[]`
//!
//! Both produce `location: { line, col }` ranges on every node, including
//! inline elements (bold / italic / monospace / macros / anchors), so the
//! caller can drive a render-pass Map (block-index → line range, inline
//! offset → char range) without re-deriving structure from the rendered DOM.
//!
//! The bundle deliberately excludes `acdc-converters-*` and the editor
//! crate's `web-sys` dependency, so the .wasm payload covers parsing only.

use acdc_parser::{Options, SafeMode};
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// JS-friendly serializer: maps as plain objects, BigInt off, JSON-compatible.
/// This is what JS consumers expect — `r.ok` rather than `r.get("ok")`.
fn js_serializer() -> serde_wasm_bindgen::Serializer {
    serde_wasm_bindgen::Serializer::new()
        .serialize_maps_as_objects(true)
        .serialize_large_number_types_as_bigints(false)
}

/// Install a panic hook that surfaces Rust panics in the browser console.
/// Idempotent — safe to call from each WASM entry point.
fn ensure_panic_hook() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        console_error_panic_hook::set_once();
    });
}

#[derive(Serialize)]
struct ParseErr {
    ok: bool,
    error: String,
}

#[derive(Serialize)]
struct WarningJson {
    kind: String,
    message: String,
    line: Option<usize>,
    column: Option<usize>,
    file: Option<String>,
}

fn warning_to_json(w: &acdc_parser::Warning) -> WarningJson {
    let (line, column, file) = if let Some(loc) = w.source_location() {
        let (l, c) = match &loc.positioning {
            acdc_parser::Positioning::Position(p) => (Some(p.line), Some(p.column)),
            acdc_parser::Positioning::Location(loc_range) => {
                (Some(loc_range.start.line), Some(loc_range.start.column))
            }
        };
        let f = loc.file.as_ref().map(|p| p.to_string_lossy().to_string());
        (l, c, f)
    } else {
        (None, None, None)
    };
    WarningJson {
        kind: format!("{:?}", w.kind),
        message: w.kind.to_string(),
        line,
        column,
        file,
    }
}

fn build_options(safe_mode: Option<String>) -> Options<'static> {
    let mode = safe_mode
        .as_deref()
        .and_then(|s| s.parse::<SafeMode>().ok())
        .unwrap_or(SafeMode::Unsafe);
    Options::builder().with_safe_mode(mode).build()
}

/// Parse a full AsciiDoc document.
///
/// Returns a JS object:
/// - `{ ok: true, value: <Document ASG>, warnings: [...] }` on success
/// - `{ ok: false, error: <message> }` on parse failure
///
/// `safe_mode` accepts `"unsafe"` (default), `"safe"`, `"server"`, `"secure"`.
/// The runtime built-in attributes (`safe-mode-level`, `asciidoctor-version`,
/// `backend`, `doctype`, etc.) are injected automatically by the parser.
#[wasm_bindgen]
pub fn parse_block(source: &str, safe_mode: Option<String>) -> Result<JsValue, JsValue> {
    ensure_panic_hook();
    let opts = build_options(safe_mode);
    match acdc_parser::parse(source, &opts) {
        Ok(result) => {
            let doc = result.document();
            let warnings: Vec<WarningJson> = result.warnings().iter().map(warning_to_json).collect();
            // Serialize via serde_json::Value to a JS-friendly intermediate
            // (avoids serde-wasm-bindgen lifetime issues with bumpalo arenas).
            let json = serde_json::to_value(doc)
                .map_err(|e| JsValue::from_str(&format!("serialize document: {e}")))?;
            let envelope = serde_json::json!({
                "ok": true,
                "value": json,
                "warnings": warnings,
            });
            envelope
                .serialize(&js_serializer())
                .map_err(|e| JsValue::from_str(&format!("to_value: {e}")))
        }
        Err(e) => {
            let envelope = ParseErr {
                ok: false,
                error: format!("{e}"),
            };
            envelope
                .serialize(&js_serializer())
                .map_err(|e| JsValue::from_str(&format!("to_value: {e}")))
        }
    }
}

/// Parse a fragment of inline AsciiDoc and return the inline nodes.
///
/// Same envelope as `parse_block`. Every returned `InlineNode` variant
/// carries a `location` field with `{ line, col }` ranges — useful when the
/// caller already has block-level structure and wants char-level offsets
/// inside one block's source text.
#[wasm_bindgen]
pub fn parse_inline(source: &str, safe_mode: Option<String>) -> Result<JsValue, JsValue> {
    ensure_panic_hook();
    let opts = build_options(safe_mode);
    match acdc_parser::parse_inline(source, &opts) {
        Ok(result) => {
            let inlines = result.inlines();
            let warnings: Vec<WarningJson> = result.warnings().iter().map(warning_to_json).collect();
            let json = serde_json::to_value(inlines)
                .map_err(|e| JsValue::from_str(&format!("serialize inlines: {e}")))?;
            let envelope = serde_json::json!({
                "ok": true,
                "value": json,
                "warnings": warnings,
            });
            envelope
                .serialize(&js_serializer())
                .map_err(|e| JsValue::from_str(&format!("to_value: {e}")))
        }
        Err(e) => {
            let envelope = ParseErr {
                ok: false,
                error: format!("{e}"),
            };
            envelope
                .serialize(&js_serializer())
                .map_err(|e| JsValue::from_str(&format!("to_value: {e}")))
        }
    }
}

/// Return the underlying `acdc-parser` version.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
