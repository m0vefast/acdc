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

use std::borrow::Cow;
use std::path::Path;

use acdc_parser::{
    DynFileResolver, FileResolver, FileResolverError, Options, SafeMode,
};
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// Wraps a JS callback `(path: string) => string | null` so acdc's include
/// preprocessor can read files from a JS-managed source (e.g. Glyph's vault
/// cache). The callback runs SYNCHRONOUSLY — embedders must pre-populate
/// their cache before calling `parse_block_with_resolver`.
///
/// The callback receives a path string derived from the include directive's
/// target after resolution against the `virtual_current_file` parent and
/// lexical `..`/`.` normalization. Returning a String yields the file
/// content (UTF-8); returning `null`/`undefined` signals not-found.
///
/// # Encoding contract
/// The callback returns a JS String — already decoded to UTF-16 by the JS
/// engine — which `JsFileResolver::read` then re-encodes as UTF-8 bytes
/// (`s.into_bytes()`) for acdc's preprocessor. Resolver implementers MUST:
/// - Strip any leading U+FEFF (BOM) before returning, OR
/// - Return content that has no BOM in the first place (the common case;
///   `TextDecoder('utf-8')` strips it by default).
///
/// If the caller passes a U+FEFF-prefixed string, acdc's downstream BOM
/// scan (operating on UTF-8 bytes `EF BB BF`) WILL match and double-strip
/// — usually harmless but worth knowing. `[encoding=…]` on the include
/// directive is honored against the bytes you return; do not pre-decode
/// if you want that path active.
///
/// # Re-entrancy
/// The JS callback MUST NOT re-enter any wasm export. The preprocessor
/// holds `Rc<RefCell<Vec<Warning>>>` open across the callback; a nested
/// parse call panics with `BorrowMutError`, poisoning the wasm instance.
///
/// # Thread safety
/// `js_sys::Function` derives `Send + Sync` automatically via wasm-bindgen's
/// `unsafe impl Send + Sync for JsValue` (gated `#[cfg(not(target_feature =
/// "atomics"))]` — see wasm-bindgen 0.2 src lib.rs:173-176). So `JsFileResolver`
/// also auto-derives Send+Sync on the default `wasm32-unknown-unknown` target,
/// no `unsafe impl` here. A future threaded-wasm build (`+atomics`, web workers,
/// or `wasm32-wasi-threads`) would lose wasm-bindgen's blanket impl and force
/// the resolver `!Send + !Sync` — caller would need a thread-confinement
/// strategy (e.g. one parser instance per worker), not an `unsafe impl` lie.
struct JsFileResolver {
    callback: js_sys::Function,
}

/// Format a `JsValue` thrown from a JS resolver into a useful message.
///
/// Probe order (most-specific to most-generic):
/// 1. `js_sys::Error::message()` — `throw new Error("…")` (common case)
/// 2. `JsValue::as_string()` — `throw "string"`
/// 3. `Reflect::get(value, "message")` — `throw { message: "…" }` plain object
/// 4. `format!("{value:?}")` — anything else (`throw 42`, `throw {code: 500}` etc.)
fn js_err_message(value: &JsValue) -> String {
    if let Some(err) = value.dyn_ref::<js_sys::Error>() {
        return err.message().into();
    }
    if let Some(s) = value.as_string() {
        return s;
    }
    if let Ok(msg) = js_sys::Reflect::get(value, &JsValue::from_str("message"))
        && let Some(s) = msg.as_string()
    {
        return s;
    }
    format!("{value:?}")
}

impl FileResolver for JsFileResolver {
    fn read(&self, path: &Path) -> Result<Cow<'_, [u8]>, FileResolverError> {
        let path_str = path.to_string_lossy().to_string();
        let arg = JsValue::from_str(&path_str);
        let result = self.callback.call1(&JsValue::NULL, &arg).map_err(|e| {
            FileResolverError::io(
                path,
                std::io::Error::other(format!("js resolver threw: {}", js_err_message(&e))),
            )
        })?;
        if result.is_null() || result.is_undefined() {
            return Err(FileResolverError::not_found(path));
        }
        let s = result.as_string().ok_or_else(|| {
            FileResolverError::io(
                path,
                std::io::Error::other("js resolver returned non-string value"),
            )
        })?;
        Ok(Cow::Owned(s.into_bytes()))
    }
}

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

/// Parse a full AsciiDoc document with `include::` directives resolved via
/// a JS callback.
///
/// Same envelope as `parse_block`, but additionally:
/// - `virtual_current_file`: a vault-relative path (e.g. `"docs/api.adoc"`)
///   used to anchor relative include targets. Doesn't need to exist on disk.
/// - `js_resolver`: a JS function `(path: string) => string | null` returning
///   the file content (UTF-8 text) for the given path, or `null` to signal
///   not-found. Called SYNCHRONOUSLY for each `include::` target. Embedders
///   should pre-populate their cache before this call.
///
/// All of acdc's Asciidoctor-spec include semantics work end-to-end:
/// nested includes, `[lines=X..Y]`, `[tag=foo]`, `[leveloffset=+N]`,
/// `[indent=N]`, `[encoding=…]`, `[opts=optional]`, tag wildcards, etc.
#[wasm_bindgen]
pub fn parse_block_with_resolver(
    source: &str,
    safe_mode: Option<String>,
    virtual_current_file: String,
    js_resolver: js_sys::Function,
) -> Result<JsValue, JsValue> {
    ensure_panic_hook();
    let mode = safe_mode
        .as_deref()
        .and_then(|s| s.parse::<SafeMode>().ok())
        .unwrap_or(SafeMode::Unsafe);
    let resolver = DynFileResolver::new(JsFileResolver { callback: js_resolver });
    let opts = Options::builder()
        .with_safe_mode(mode)
        .with_file_resolver(resolver)
        .with_virtual_current_file(virtual_current_file)
        .build();
    match acdc_parser::parse(source, &opts) {
        Ok(result) => {
            let doc = result.document();
            let warnings: Vec<WarningJson> =
                result.warnings().iter().map(warning_to_json).collect();
            // Per-include expansion metadata — embedders (e.g. Glyph) use
            // this to translate post-expansion block positions back to the
            // original root-source line number, so editor cursor / scroll
            // sync stays aligned with the user's buffer. acdc handles the
            // attribute filtering (lines=, tag=, tags=, leveloffset=, …) so
            // consumers don't replicate that logic.
            let include_expansions: Vec<serde_json::Value> = result
                .include_expansions()
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "sourceLine": e.source_line,
                        "expandedLines": e.expanded_lines,
                    })
                })
                .collect();
            let json = serde_json::to_value(doc)
                .map_err(|e| JsValue::from_str(&format!("serialize document: {e}")))?;
            let envelope = serde_json::json!({
                "ok": true,
                "value": json,
                "warnings": warnings,
                "includeExpansions": include_expansions,
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
