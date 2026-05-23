//! Parse + render-html WASM bindings for `acdc`, designed for the Glyph editor.
//!
//! Parse entry points (return serde-serialized AST graphs):
//! - `parse_block(source)` — full document parse, returns the `Document` ASG
//! - `parse_inline(source)` — inline-only parse, returns `InlineNode[]`
//! - `parse_block_with_resolver(source, …, resolver)` — full parse with
//!   include directive resolution via a JS callback
//!
//! Render entry points (return HTML strings with optional source-position
//! markup for cursor mapping):
//! - `render_html(source, …, emit_source_positions)` — parse + convert to
//!   embedded HTML in one call. When `emit_source_positions=true`, text
//!   leaves wrap in `<span data-src-start="N" data-src-end="M">…</span>`
//!   and block opening tags carry the same attrs, letting the consumer map
//!   cursor positions between source and rendered DOM without maintaining
//!   a parallel Segment[] structure on the host side.
//! - `render_html_with_resolver(…)` — render variant with include support.
//!
//! All parse entry points produce `location: { line, col }` ranges on every
//! node, including inline elements (bold / italic / monospace / macros /
//! anchors). The render entry points consume `Location::absolute_start/end`
//! (inclusive end) from the same AST and surface them as `data-src-*` attrs.
//!
//! # Wire-format contract: `data-src-*` offsets
//!
//! **`data-src-start` / `data-src-end` are UTF-8 byte indices** into the
//! original source string (acdc's `Location::absolute_start/absolute_end`).
//!
//! **`data-src-start`** is the byte index of the **first byte** of the
//! first codepoint in the span. For ASCII content, also the codepoint index.
//!
//! **`data-src-end`** is the byte index of the **first byte of the LAST
//! codepoint** in the span — NOT the last byte of that codepoint. For ASCII
//! the two coincide (every codepoint is 1 byte). For multi-byte content,
//! the byte range `[start, end]` ends mid-codepoint:
//!
//! | source | bytes | `data-src-end` | last codepoint covers |
//! |---|---|---|---|
//! | `"Hello"`  | `H=0 e=1 l=2 l=3 o=4` | `4` | byte 4 (the `o`) |
//! | `"你好世界"` | `你=0..2 好=3..5 世=6..8 界=9..11` | `9` | bytes 9..11 (the `界`) |
//!
//! Consumers that slice the source MUST therefore convert `data-src-end`
//! by finding the codepoint that starts at that byte and adding its byte
//! length, NOT by `end + 1`. The Glyph TS bridge does this conversion via
//! `byteEndInclusiveToCharExclusiveEnd` in `web/src/renderer/asciidoc.ts`.
//!
//! **UTF-8 vs. UTF-16**: Rust strings are UTF-8 so these are UTF-8 byte
//! indices. JS strings are UTF-16. Consumers slicing in JS MUST translate
//! byte→code-unit offsets first; identity-passing works for ASCII but
//! corrupts any CJK / emoji content.
//!
//! The bundle excludes the editor crate's `web-sys` dependency, so the
//! .wasm payload stays lean.

use std::borrow::Cow;
use std::path::Path;

use acdc_converters_core::Converter;
use acdc_parser::{DynFileResolver, FileResolver, FileResolverError, Options, SafeMode};
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

/// Build the wasm envelope's `includeExpansions` array — the JS-side
/// source-line translator's input. Merges acdc's two preprocessor
/// accounting fields:
///   1. `include_expansions` — one entry per root-level `include::`
///      (`{sourceLine, expandedLines: N}`, where N is the post-filter line
///      count from acdc, including 0 for missing/optional includes).
///   2. `conditional_drops` — one entry per source line consumed by
///      `ifdef`/`ifndef`/`ifeval`/`endif` that produced no output
///      (`{sourceLine, expandedLines: 0}`).
///
/// Entries are sorted by `sourceLine` so the translator's left-to-right
/// fold (delta accumulator) sees the same source order acdc processed.
/// Single uniform field name keeps the JS consumer path identical to the
/// resolver-mode case — no branching on whether a resolver was wired.
fn build_preprocessor_line_map(result: &acdc_parser::ParseResult) -> Vec<serde_json::Value> {
    let mut entries: Vec<(usize, usize)> = result
        .include_expansions()
        .iter()
        .map(|e| (e.source_line, e.expanded_lines))
        .chain(result.conditional_drops().iter().map(|&line| (line, 0)))
        .collect();
    entries.sort_by_key(|&(line, _)| line);
    entries
        .into_iter()
        .map(|(source_line, expanded_lines)| {
            serde_json::json!({
                "sourceLine": source_line,
                "expandedLines": expanded_lines,
            })
        })
        .collect()
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
            let warnings: Vec<WarningJson> =
                result.warnings().iter().map(warning_to_json).collect();
            // No `FileResolver` here, so every `include::` directive is
            // dropped by the preprocessor (resolved as missing file). The
            // directive line itself disappears from the output, shifting
            // every subsequent block up by 1; embedders that map
            // post-expansion positions back to original source still need
            // the metadata so the translator can compensate. Same envelope
            // shape as `parse_block_with_resolver` (with
            // `expanded_lines: 0` for every dropped directive) so the
            // consumer path stays uniform.
            let include_expansions = build_preprocessor_line_map(&result);
            // Serialize via serde_json::Value to a JS-friendly intermediate
            // (avoids serde-wasm-bindgen lifetime issues with bumpalo arenas).
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
            let warnings: Vec<WarningJson> =
                result.warnings().iter().map(warning_to_json).collect();
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
    let resolver = DynFileResolver::new(JsFileResolver {
        callback: js_resolver,
    });
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
            // consumers don't replicate that logic. Merged with conditional
            // (ifdef/ifndef/ifeval/endif) line drops so the translator
            // compensates for EVERY preprocessor-induced shift in one pass.
            let include_expansions = build_preprocessor_line_map(&result);
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

/// Parse a full AsciiDoc document and render it to HTML in one call.
///
/// Returns a JS object:
/// - `{ ok: true, html: "<...>", warnings: [...], includeExpansions: [...] }` on success
/// - `{ ok: false, error: <message> }` on parse or render failure
///
/// `safe_mode` accepts `"unsafe"` (default), `"safe"`, `"server"`, `"secure"`.
///
/// When `emit_source_positions=true`, every text-leaf renderer wraps its
/// output in `<span data-src-start="N" data-src-end="M">…</span>` using the
/// **UTF-8 byte offsets** from `Location::absolute_start` / `absolute_end`.
/// Block opening tags (paragraphs, lists, tables, admonitions, sections,
/// list items, table cells, …) carry the same attrs. The Glyph TS bridge
/// uses these spans to drive cursor mapping between source and rendered DOM
/// without maintaining a parallel Segment[] in JS.
///
/// # `data-src-*` contract (see crate docs for full details)
///
/// - **`data-src-end` is the first byte of the LAST codepoint** in the span
///   (NOT the last byte). For ASCII these coincide; for multi-byte content
///   `[start, end]` ends mid-codepoint. To get an exclusive end, find the
///   codepoint at byte `end` and add its byte length.
/// - **Offsets are UTF-8 byte indices**, not UTF-16 code-unit indices. JS
///   consumers MUST translate before slicing — `String.prototype.slice`
///   takes UTF-16 code units. Untranslated offsets corrupt cursor mapping
///   for any CJK / emoji / non-ASCII content.
///
/// Output is always `embedded` (no `<!DOCTYPE>`, `<html>`, `<head>`, `<body>`
/// wrappers) — the embedder controls the document chrome.
#[wasm_bindgen]
pub fn render_html(
    source: &str,
    safe_mode: Option<String>,
    emit_source_positions: bool,
) -> Result<JsValue, JsValue> {
    ensure_panic_hook();
    let opts = build_options(safe_mode);
    match acdc_parser::parse(source, &opts) {
        Ok(result) => render_envelope(&result, emit_source_positions),
        Err(e) => ParseErr {
            ok: false,
            error: format!("{e}"),
        }
        .serialize(&js_serializer())
        .map_err(|e| JsValue::from_str(&format!("to_value: {e}"))),
    }
}

/// Parse + render variant with `include::` directive resolution via JS callback.
///
/// Same envelope as `render_html`, plus:
/// - `virtual_current_file`: vault-relative path anchoring relative include targets
/// - `js_resolver`: `(path: string) => string | null` returning UTF-8 content
///   or `null` for not-found. Called SYNCHRONOUSLY for each include target.
///   See `parse_block_with_resolver` for the encoding / re-entrancy contract.
#[wasm_bindgen]
pub fn render_html_with_resolver(
    source: &str,
    safe_mode: Option<String>,
    virtual_current_file: String,
    js_resolver: js_sys::Function,
    emit_source_positions: bool,
) -> Result<JsValue, JsValue> {
    ensure_panic_hook();
    let mode = safe_mode
        .as_deref()
        .and_then(|s| s.parse::<SafeMode>().ok())
        .unwrap_or(SafeMode::Unsafe);
    let resolver = DynFileResolver::new(JsFileResolver {
        callback: js_resolver,
    });
    let opts = Options::builder()
        .with_safe_mode(mode)
        .with_file_resolver(resolver)
        .with_virtual_current_file(virtual_current_file)
        .build();
    match acdc_parser::parse(source, &opts) {
        Ok(result) => render_envelope(&result, emit_source_positions),
        Err(e) => ParseErr {
            ok: false,
            error: format!("{e}"),
        }
        .serialize(&js_serializer())
        .map_err(|e| JsValue::from_str(&format!("to_value: {e}"))),
    }
}

/// Shared body for the two render entry points — converts a `ParseResult` to
/// the JS envelope shape `{ ok: true, html, warnings, includeExpansions }`.
fn render_envelope(
    result: &acdc_parser::ParseResult,
    emit_source_positions: bool,
) -> Result<JsValue, JsValue> {
    let doc = result.document();
    let warnings: Vec<WarningJson> = result.warnings().iter().map(warning_to_json).collect();
    let include_expansions = build_preprocessor_line_map(result);

    let processor = acdc_converters_html::Processor::new(
        acdc_converters_core::Options::default(),
        doc.attributes.clone(),
    );
    let render_opts = acdc_converters_html::RenderOptions {
        embedded: true,
        emit_source_positions,
        ..acdc_converters_html::RenderOptions::default()
    };
    let html = processor
        .convert_to_string(doc, &render_opts)
        .map_err(|e| JsValue::from_str(&format!("render: {e}")))?;

    let envelope = serde_json::json!({
        "ok": true,
        "html": html,
        "warnings": warnings,
        "includeExpansions": include_expansions,
    });
    envelope
        .serialize(&js_serializer())
        .map_err(|e| JsValue::from_str(&format!("to_value: {e}")))
}

/// Return the underlying `acdc-parser` version.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::missing_panics_doc
)]
mod tests {
    //! Pin the wasm envelope key names at the Rust layer. The JS-side
    //! `asciidoc.ts` consumer accesses `entry.sourceLine` /
    //! `entry.expandedLines` in a tight loop; a `serde_json::json!` typo
    //! here (e.g. `"source_line"` or `"includeExpansion"` singular) would
    //! surface only as a Glyph cursor-jump bug in production — these
    //! tests catch it at the wasm boundary instead.
    use super::{Options, SafeMode, build_preprocessor_line_map};

    /// `build_preprocessor_line_map` emits camelCase keys for every
    /// entry, including the conditional-drop case (`expandedLines: 0`).
    #[test]
    fn preprocessor_line_map_uses_camelcase_keys() {
        // 3 source lines consumed by ifdef-false / endif → 3 conditional_drops
        // entries, each with `expandedLines: 0`.
        let source = "ifdef::nonexistent_attr[]\nbody\nendif::[]\n";
        let opts = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
        let result = acdc_parser::parse(source, &opts).expect("parse succeeds");
        let entries = build_preprocessor_line_map(&result);
        assert!(
            !entries.is_empty(),
            "expected at least one conditional_drops entry, got none"
        );
        for (i, entry) in entries.iter().enumerate() {
            let obj = entry.as_object().expect("entry is JSON object");
            assert!(
                obj.contains_key("sourceLine"),
                "entry {i}: missing camelCase key `sourceLine`, got keys = {:?}",
                obj.keys().collect::<Vec<_>>()
            );
            assert!(
                obj.contains_key("expandedLines"),
                "entry {i}: missing camelCase key `expandedLines`, got keys = {:?}",
                obj.keys().collect::<Vec<_>>()
            );
            // Defend against accidental snake_case re-introduction
            // (the source-side fields on `IncludeExpansion` are
            // `source_line` / `expanded_lines`; the bridge MUST rename).
            assert!(
                !obj.contains_key("source_line"),
                "entry {i}: snake_case `source_line` leaked through bridge"
            );
            assert!(
                !obj.contains_key("expanded_lines"),
                "entry {i}: snake_case `expanded_lines` leaked through bridge"
            );
        }
    }

    /// Conditional-drop entries carry `expandedLines: 0` per the wire
    /// contract (the JS translator's left-fold relies on this — any
    /// other sentinel would shift the position math by one).
    #[test]
    fn conditional_drop_entries_have_zero_expanded_lines() {
        let source = "ifdef::nonexistent_attr[]\nbody\nendif::[]\n";
        let opts = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
        let result = acdc_parser::parse(source, &opts).expect("parse succeeds");
        let entries = build_preprocessor_line_map(&result);
        // All entries here originate from conditional_drops (no real
        // includes in this source), so each must have expandedLines == 0.
        for entry in &entries {
            let expanded = entry
                .get("expandedLines")
                .and_then(serde_json::Value::as_u64)
                .expect("expandedLines is unsigned int");
            assert_eq!(
                expanded, 0,
                "conditional_drops entry must have expandedLines:0, got {entry:?}"
            );
        }
    }
}
