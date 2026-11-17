//! Parse WASM bindings for `acdc`, designed for the Glyph editor.
//!
//! Parse entry points (return serde-serialized AST graphs):
//! - `parse_block(source)` — full document parse, returns the `Document` ASG
//! - `parse_inline(source)` — inline-only parse, returns `InlineNode[]`
//! - `parse_block_with_resolver(source, …, resolver)` — full parse with
//!   include directive resolution via a JS callback
//!
//! `render_html(source)` renders embedded HTML; Glyph uses it only to compare
//! acdc's output with asciidoctor's.
//!
//! Every node carries `location: { line, col }` ranges, inline elements
//! included. `Location::absolute_start/end` are UTF-8 byte indices into the
//! source with an inclusive end: the end is the first byte of the last
//! codepoint, so JS consumers translate bytes to UTF-16 code units before
//! slicing (`byteEndInclusiveToCharExclusiveEnd` in `web/src/renderer/asciidoc.ts`).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use acdc_converters_core::Converter as _;
use acdc_converters_core::xref::{XrefDisplay, XrefGuard, resolve_reference, with_caption_prefix};
use acdc_parser::ast::{
    BlockKind, CaptionRef, ContentFieldRef, ContentRef, DocumentRef, InlineKind, LocationRef, ReferencesRef, StructuralRef,
};
use acdc_parser::{
    IncludeLoader, IncludeSource, IncludeSourceError, IncludeSourceErrorKind,
    IncludeSourceProvider, IncludeSourceTarget, NORMAL, Options, OptionsBuilder, SafeMode,
    Substitution, SubstitutionSpec, VERBATIM, XrefForm,
};
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// Where a node stands: start and end line and column, and the include chain.
type SpanKey = (u32, u32, u32, u32, Vec<String>);

fn span_key(location: LocationRef<'_>) -> SpanKey {
    let (start, end) = (location.start(), location.end());
    let chain = start
        .include_chain()
        .map(|chain| chain.map(str::to_string).collect())
        .unwrap_or_default();
    (start.line(), start.column(), end.line(), end.column(), chain)
}

/// The same key read from a serialized `location` (`[{line, col, file?}, {line, col}]`).
fn json_span_key(location: Option<&serde_json::Value>) -> Option<SpanKey> {
    let bounds = location?.as_array()?;
    let (start, end) = (bounds.first()?, bounds.get(1)?);
    let number = |b: &serde_json::Value, k: &str| b.get(k)?.as_u64().and_then(|n| u32::try_from(n).ok());
    let chain = start
        .get("file")
        .and_then(serde_json::Value::as_array)
        .map(|files| files.iter().filter_map(|f| f.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Some((number(start, "line")?, number(start, "col")?, number(end, "line")?, number(end, "col")?, chain))
}

/// A location written as the serialized tree writes one (`[{line, col, file?}, {line, col, file?}]`).
fn json_location(location: LocationRef<'_>) -> serde_json::Value {
    let position = |p: acdc_parser::ast::PositionRef<'_>| {
        let mut map = serde_json::Map::new();
        map.insert("line".to_string(), p.line().into());
        map.insert("col".to_string(), p.column().into());
        if let Some(chain) = p.include_chain() {
            let files: Vec<serde_json::Value> = chain.map(Into::into).collect();
            if !files.is_empty() {
                map.insert("file".to_string(), files.into());
            }
        }
        serde_json::Value::Object(map)
    };
    serde_json::Value::Array(vec![position(location.start()), position(location.end())])
}

/// Source provenance the parser keeps and its serialized tree leaves out, by
/// where each node stands: how a cross reference was written, and where a
/// formatted span's attribute list (`[.role]`) is written.
#[derive(Default)]
struct Provenance {
    xrefs: HashMap<SpanKey, (XrefForm, String, serde_json::Value)>,
    link_targets: HashMap<SpanKey, serde_json::Value>,
    /// Links whose automatic text omits the scheme: `hide-uri-scheme` was set where they stand.
    hidden_schemes: HashSet<SpanKey>,
    /// What a cross reference with no text of its own draws, as acdc resolves it.
    auto_texts: HashMap<SpanKey, serde_json::Value>,
    attribute_lists: HashMap<SpanKey, serde_json::Value>,
}

fn text_json(value: &str) -> serde_json::Value {
    serde_json::json!({ "name": "text", "type": "string", "value": value })
}

fn content_json(content: ContentRef<'_>) -> Vec<serde_json::Value> {
    let mut items = match serde_json::to_value(content) {
        Ok(serde_json::Value::Array(items)) => items,
        Ok(item) => vec![item],
        Err(_) => Vec::new(),
    };
    items.iter_mut().for_each(mark_nested_references);
    items
}

/// A reference inside another reference's text reads as `[id]` (`XrefDisplay::Nested`).
fn mark_nested_references(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(items) => items.iter_mut().for_each(mark_nested_references),
        serde_json::Value::Object(map) => {
            let automatic = map.get("name").and_then(serde_json::Value::as_str) == Some("xref")
                && map.get("inlines").and_then(serde_json::Value::as_array).is_none_or(Vec::is_empty);
            if automatic && let Some(target) = map.get("target").and_then(serde_json::Value::as_str) {
                let text = serde_json::Value::Array(vec![text_json(&format!("[{target}]"))]);
                map.insert("autoText".to_string(), text);
            }
            map.values_mut().for_each(mark_nested_references);
        }
        _ => {}
    }
}

/// An automatic reference's text as inline nodes, composed as acdc's html converter draws
/// it (`render_xref_display`). None for a reference to another document.
fn auto_text_json(display: XrefDisplay<'_, ContentRef<'_>>) -> Option<serde_json::Value> {
    let nodes = match display {
        XrefDisplay::Title(content, _) | XrefDisplay::Label(content, _) => content_json(content),
        XrefDisplay::ShortCaption(prefix) => vec![text_json(&prefix)],
        XrefDisplay::FullCaption(prefix, content, _) => {
            [vec![text_json(&format!("{prefix}, \u{201c}"))], content_json(content), vec![text_json("\u{201d}")]].concat()
        }
        XrefDisplay::ShortCustomCaption(prefix, _) => with_caption_prefix(prefix, content_json),
        XrefDisplay::FullCustomCaption(prefix, content, _) => [
            with_caption_prefix(prefix, content_json),
            vec![text_json(", \u{201c}")],
            content_json(content),
            vec![text_json("\u{201d}")],
        ]
        .concat(),
        XrefDisplay::Emphasized(prefix, content, _) => {
            let emphasis = serde_json::json!({
                "name": "span", "type": "inline", "variant": "emphasis", "form": "constrained",
                "inlines": content_json(content),
            });
            prefix.map(|p| text_json(&format!("{p}, "))).into_iter().chain(std::iter::once(emphasis)).collect()
        }
        XrefDisplay::Fallback(text) | XrefDisplay::Unresolved(text) | XrefDisplay::Nested(text) => vec![text_json(&text)],
        XrefDisplay::External(_) => return None,
    };
    Some(serde_json::Value::Array(nodes))
}

fn provenance<'d>(fields: impl Iterator<Item = ContentRef<'d>>, references: Option<ReferencesRef<'d>>) -> Provenance {
    let mut found = Provenance::default();
    let guard = XrefGuard::default();
    for content in fields {
        for inline in content.inlines() {
            let attribute_list = match inline.kind() {
                InlineKind::CrossReference(xref) => {
                    found.xrefs.insert(
                        span_key(inline.location()),
                        (xref.form(), xref.written_target().to_string(), json_location(xref.target_location())),
                    );
                    if let Some(references) = references
                        && xref.text().is_empty()
                        && let Some(text) = auto_text_json(resolve_reference(references.get(xref.target()), xref, &guard))
                    {
                        found.auto_texts.insert(span_key(inline.location()), text);
                    }
                    None
                }
                InlineKind::Link(link) => {
                    found.link_targets.insert(span_key(inline.location()), json_location(link.target_location()));
                    if link.hides_uri_scheme() {
                        found.hidden_schemes.insert(span_key(inline.location()));
                    }
                    None
                }
                InlineKind::UrlMacro(link) => {
                    found.link_targets.insert(span_key(inline.location()), json_location(link.target_location()));
                    if link.hides_uri_scheme() {
                        found.hidden_schemes.insert(span_key(inline.location()));
                    }
                    None
                }
                InlineKind::Autolink(link) => {
                    if link.hides_uri_scheme() {
                        found.hidden_schemes.insert(span_key(inline.location()));
                    }
                    None
                }
                InlineKind::Mailto(link) => {
                    found.link_targets.insert(span_key(inline.location()), json_location(link.target_location()));
                    None
                }
                InlineKind::Bold(span) => span.attribute_location(),
                InlineKind::Italic(span) => span.attribute_location(),
                InlineKind::Monospace(span) => span.attribute_location(),
                InlineKind::Highlight(span) => span.attribute_location(),
                InlineKind::Subscript(span) => span.attribute_location(),
                InlineKind::Superscript(span) => span.attribute_location(),
                InlineKind::DoubleQuotedSpan(span) => span.attribute_location(),
                InlineKind::SingleQuotedSpan(span) => span.attribute_location(),
                _ => None,
            };
            if let Some(location) = attribute_list {
                found.attribute_lists.insert(span_key(inline.location()), json_location(location));
            }
        }
    }
    found
}

/// Each titled block's caption prefix as acdc resolves it (asciidoctor `assign_caption`:
/// the block's `caption`, the document's, or `<label> <N>. ` from `<context>-caption`,
/// an unset attribute included). An authored caption with inline content is already in
/// the tree as `metadata.caption`.
fn caption_prefixes(document: DocumentRef<'_>) -> HashMap<SpanKey, String> {
    let attributes = document.attributes();
    StructuralRef::Document(document)
        .descendants()
        .filter_map(|node| match node {
            StructuralRef::Block(block) => Some(block),
            _ => None,
        })
        .filter_map(|block| {
            let caption = CaptionRef::resolve(block.attributes()?, attributes, acdc_parser::CaptionKind::for_block(block))?;
            let prefix = match caption {
                CaptionRef::Numbered { label, number: Some(number), .. } => format!("{label} {number}. "),
                CaptionRef::Custom(prefix) => prefix.to_string(),
                _ => return None,
            };
            Some((span_key(block.location()), prefix))
        })
        .collect()
}

/// The number acdc assigned each numbered section (no signifier, no punctuation; none past
/// `sectnumlevels`), by where the section stands.
fn section_numbers(document: DocumentRef<'_>) -> HashMap<SpanKey, String> {
    StructuralRef::Document(document)
        .descendants()
        .filter_map(|node| match node {
            StructuralRef::Block(block) => match block.kind() {
                BlockKind::Section(section) => Some((span_key(block.location()), section.number()?.to_string())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Writes each header unset (`:name!:`) into the document's attributes as `false`, as a body
/// entry carries it: an unset attribute is not one never written (`:appendix-caption!:`
/// drops the caption asciidoctor otherwise defaults).
fn annotate_unset_attributes(value: &mut serde_json::Value, document: DocumentRef<'_>) {
    let unset: Vec<&str> = document
        .attributes()
        .assignments()
        .filter(|(_, assignment)| matches!(assignment, acdc_parser::DocumentAttributeAssignment::Unset))
        .map(|(name, _)| name)
        .collect();
    if unset.is_empty() {
        return;
    }
    let Some(map) = value.as_object_mut() else { return };
    let attributes = map.entry("attributes").or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(attributes) = attributes.as_object_mut() {
        for name in unset {
            attributes.insert(name.to_string(), serde_json::Value::Bool(false));
        }
    }
}

/// Writes each block's resolved values (`field` → value by where the block stands).
fn annotate_blocks(value: &mut serde_json::Value, fields: &[(&str, &HashMap<SpanKey, String>)]) {
    match value {
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| annotate_blocks(v, fields)),
        serde_json::Value::Object(map) => {
            if map.get("type").and_then(serde_json::Value::as_str) == Some("block")
                && let Some(key) = json_span_key(map.get("location"))
            {
                for (field, values) in fields {
                    if let Some(found) = values.get(&key) {
                        map.insert((*field).to_string(), serde_json::Value::String(found.clone()));
                    }
                }
            }
            map.values_mut().for_each(|v| annotate_blocks(v, fields));
        }
        _ => {}
    }
}

/// Writes the provenance into a serialized tree:
/// - every cross reference gets `form`, `targetLocation` and `targetKind` (what its written target names,
///   read with acdc's own rule `xref_target_kind`), so the renderer does not re-derive
///   asciidoctor's rule in TypeScript;
/// - every formatted span with an attribute list gets `attributeLocation`;
/// - every link, URL and mailto macro gets `targetLocation`.
fn annotate_provenance(value: &mut serde_json::Value, provenance: &Provenance) {
    match value {
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| annotate_provenance(v, provenance)),
        serde_json::Value::Object(map) => {
            if let Some(key) = json_span_key(map.get("location")) {
                let xref = (map.get("name").and_then(serde_json::Value::as_str) == Some("xref"))
                    .then(|| provenance.xrefs.get(&key))
                    .flatten();
                if let Some((form, target, target_location)) = xref {
                    if let Some(text) = provenance.auto_texts.get(&key) {
                        map.insert("autoText".to_string(), text.clone());
                    }
                    map.insert("form".to_string(), serde_json::to_value(form).unwrap_or_default());
                    map.insert("targetLocation".to_string(), target_location.clone());
                    if let Ok(kind) = serde_json::to_value(acdc_parser::xref_target_kind(target, *form)) {
                        map.insert("targetKind".to_string(), kind);
                    }
                }
                if let Some(location) = provenance.attribute_lists.get(&key) {
                    map.insert("attributeLocation".to_string(), location.clone());
                }
                if map.get("name").and_then(serde_json::Value::as_str) == Some("ref")
                    && let Some(location) = provenance.link_targets.get(&key)
                {
                    map.insert("targetLocation".to_string(), location.clone());
                }
                if provenance.hidden_schemes.contains(&key) {
                    map.insert("hideUriScheme".to_string(), serde_json::Value::Bool(true));
                }
            }
            // An automatic text is a copy of its target's title: it holds the title's own
            // nodes, locations and all, so annotating it again would expand a title that
            // references itself without end.
            map.iter_mut()
                .filter(|(field, _)| field.as_str() != "autoText")
                .for_each(|(_, v)| annotate_provenance(v, provenance));
        }
        _ => {}
    }
}

/// Wraps a JS callback `(path: string) => string | null` so acdc's include
/// preprocessor reads files from a JS-managed source (e.g. Glyph's vault
/// cache). The callback runs SYNCHRONOUSLY — embedders pre-populate their
/// cache before calling `parse_block_with_resolver`.
///
/// The callback receives the include target resolved by the parser against the
/// including file. Returning a String yields the file content; returning
/// `null`/`undefined` signals not-found. The returned string is re-encoded as
/// UTF-8, so a leading U+FEFF should be stripped by the resolver.
///
/// The JS callback MUST NOT re-enter any wasm export.
struct JsIncludeProvider {
    callback: js_sys::Function,
}

/// Format a `JsValue` thrown from a JS resolver into a useful message.
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

impl IncludeSourceProvider for JsIncludeProvider {
    fn open(&self, target: &IncludeSourceTarget) -> Result<IncludeSource, IncludeSourceError> {
        let path: &Path = match target {
            IncludeSourceTarget::File(path) => path,
            IncludeSourceTarget::Uri(uri) => {
                return Err(IncludeSourceError::new(
                    IncludeSourceErrorKind::Unsupported,
                    format!("uri includes are not resolved: {uri}"),
                ));
            }
            _ => {
                return Err(IncludeSourceError::new(
                    IncludeSourceErrorKind::Unsupported,
                    "unsupported include target",
                ));
            }
        };
        let arg = JsValue::from_str(&path.to_string_lossy());
        let result = self.callback.call1(&JsValue::NULL, &arg).map_err(|e| {
            IncludeSourceError::new(
                IncludeSourceErrorKind::Unavailable,
                format!("js resolver threw: {}", js_err_message(&e)),
            )
        })?;
        if result.is_null() || result.is_undefined() {
            return Err(IncludeSourceError::new(
                IncludeSourceErrorKind::NotFound,
                format!("not found: {}", path.display()),
            ));
        }
        let s = result.as_string().ok_or_else(|| {
            IncludeSourceError::new(
                IncludeSourceErrorKind::Unavailable,
                "js resolver returned non-string value",
            )
        })?;
        Ok(IncludeSource::from_string(s))
    }
}

/// JS-friendly serializer: maps as plain objects, BigInt off, JSON-compatible.
fn js_serializer() -> serde_wasm_bindgen::Serializer {
    serde_wasm_bindgen::Serializer::new()
        .serialize_maps_as_objects(true)
        .serialize_large_number_types_as_bigints(false)
}

/// Install a panic hook that surfaces Rust panics in the browser console.
fn ensure_panic_hook() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        console_error_panic_hook::set_once();
    });
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
        let f = loc.file.as_ref().map(|p| p.to_string_lossy().to_string());
        (
            Some(loc.location.start.line as usize),
            Some(loc.location.start.column as usize),
            f,
        )
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

fn parse_safe_mode(safe_mode: Option<&str>) -> SafeMode {
    safe_mode
        .and_then(|s| s.parse::<SafeMode>().ok())
        .unwrap_or(SafeMode::Unsafe)
}

fn options_builder(safe_mode: Option<&str>) -> OptionsBuilder<'static> {
    Options::builder().with_safe_mode(parse_safe_mode(safe_mode))
}

fn to_js(value: &impl Serialize) -> Result<JsValue, JsValue> {
    value
        .serialize(&js_serializer())
        .map_err(|e| JsValue::from_str(&format!("to_value: {e}")))
}

fn err_envelope(error: impl std::fmt::Display) -> Result<JsValue, JsValue> {
    to_js(&serde_json::json!({ "ok": false, "error": error.to_string() }))
}

fn document_envelope(
    source: &str,
    file: Option<&str>,
    options: Result<Options<'static>, acdc_parser::Error>,
) -> Result<JsValue, JsValue> {
    let options = match options {
        Ok(options) => options,
        Err(e) => return err_envelope(e),
    };
    let parsed = match file {
        Some(file) => acdc_parser::parse_file_text(source, file, &options),
        None => acdc_parser::parse(source, &options),
    };
    match parsed {
        Ok(result) => {
            let warnings: Vec<WarningJson> =
                result.warnings().iter().map(warning_to_json).collect();
            let mut json = serde_json::to_value(result.document())
                .map_err(|e| JsValue::from_str(&format!("serialize document: {e}")))?;
            let provenance = provenance(
                result.document().content_occurrences().map(ContentFieldRef::content),
                Some(result.document().references()),
            );
            annotate_provenance(&mut json, &provenance);
            annotate_blocks(
                &mut json,
                &[("captionPrefix", &caption_prefixes(result.document())), ("sectionNumber", &section_numbers(result.document()))],
            );
            annotate_unset_attributes(&mut json, result.document());
            to_js(&serde_json::json!({ "ok": true, "value": json, "warnings": warnings }))
        }
        Err(e) => err_envelope(e),
    }
}

/// Parse a full AsciiDoc document.
///
/// Returns `{ ok: true, value: <Document ASG>, warnings: [...] }` on success,
/// `{ ok: false, error }` on failure. `safe_mode` accepts `"unsafe"` (default),
/// `"safe"`, `"server"`, `"secure"`.
#[wasm_bindgen]
pub fn parse_block(source: &str, safe_mode: Option<String>) -> Result<JsValue, JsValue> {
    ensure_panic_hook();
    document_envelope(source, None, options_builder(safe_mode.as_deref()).build())
}

/// Parse a fragment of inline AsciiDoc and return the inline nodes, in the
/// same envelope as `parse_block`.
#[wasm_bindgen]
pub fn parse_inline(source: &str, safe_mode: Option<String>) -> Result<JsValue, JsValue> {
    ensure_panic_hook();
    let options = match options_builder(safe_mode.as_deref()).build() {
        Ok(options) => options,
        Err(e) => return err_envelope(e),
    };
    match acdc_parser::parse_inline(source, &options) {
        Ok(result) => {
            let warnings: Vec<WarningJson> =
                result.warnings().iter().map(warning_to_json).collect();
            let content = result.content();
            let mut json = serde_json::to_value(content)
                .map_err(|e| JsValue::from_str(&format!("serialize inlines: {e}")))?;
            let nested = content
                .inlines()
                .flat_map(|inline| inline.content_occurrences().map(ContentFieldRef::content));
            let provenance = provenance(std::iter::once(content).chain(nested), None);
            annotate_provenance(&mut json, &provenance);
            to_js(&serde_json::json!({ "ok": true, "value": json, "warnings": warnings }))
        }
        Err(e) => err_envelope(e),
    }
}

/// Parse a full AsciiDoc document with `include::` directives resolved via
/// a JS callback.
///
/// - `virtual_current_file`: the document's path, whose parent anchors relative
///   include targets and is its `docdir`.
/// - `js_resolver`: `(path: string) => string | null`, called synchronously
///   for each include target.
#[wasm_bindgen]
pub fn parse_block_with_resolver(
    source: &str,
    safe_mode: Option<String>,
    virtual_current_file: String,
    js_resolver: js_sys::Function,
) -> Result<JsValue, JsValue> {
    ensure_panic_hook();
    // The page is the file the vault holds (asciidoctor `load_file`): it sets `docfile`,
    // `docdir`, `docname`, anchors relative includes and names itself in an unresolved
    // include's line.
    let options = options_builder(safe_mode.as_deref())
        .with_include_loader(IncludeLoader::custom(JsIncludeProvider {
            callback: js_resolver,
        }))
        .build();
    document_envelope(source, Some(&virtual_current_file), options)
}

/// Parse a full AsciiDoc document and render it to HTML: embedded unless `embedded` is
/// `false`, which renders the standalone page with its header.
///
/// Returns `{ ok: true, html, warnings }` or `{ ok: false, error }`.
#[wasm_bindgen]
pub fn render_html(
    source: &str,
    safe_mode: Option<String>,
    embedded: Option<bool>,
) -> Result<JsValue, JsValue> {
    ensure_panic_hook();
    let embedded = embedded.unwrap_or(true);
    let converter = match acdc_converters_html::Converter::new(
        acdc_converters_core::Options::builder().embedded(embedded).build(),
        options_builder(safe_mode.as_deref()),
    ) {
        Ok(converter) => converter,
        Err(e) => return err_envelope(e),
    };
    let result = match acdc_parser::parse(source, converter.parser_options()) {
        Ok(result) => result,
        Err(e) => return err_envelope(e),
    };
    let warnings: Vec<WarningJson> = result.warnings().iter().map(warning_to_json).collect();
    let render_opts = acdc_converters_html::RenderOptions {
        embedded,
        ..acdc_converters_html::RenderOptions::default()
    };
    let html = converter
        .convert_to_string(result.document(), &render_opts)
        .map_err(|e| JsValue::from_str(&format!("render: {e}")))?;
    to_js(&serde_json::json!({ "ok": true, "html": html, "warnings": warnings }))
}

/// A block's authored `subs` entries (as `metadata.substitutions` carries them)
/// resolved against `baseline` — `normal`, `verbatim` or `none` — the way the parser
/// resolves them: the substitutions to apply, in order.
/// An inter-document cross-reference's destination and fallback text under the document's
/// file controls (`relfilesuffix`, `outfilesuffix`, `relfileprefix`; `None` = unset), as
/// acdc's HTML converter draws it. `None` when `target` does not name another document.
#[wasm_bindgen]
pub fn interdocument_link(
    target: &str,
    relfilesuffix: Option<String>,
    outfilesuffix: Option<String>,
    relfileprefix: Option<String>,
) -> Option<Vec<String>> {
    acdc_converters_core::xref::interdocument_link(
        target,
        relfilesuffix.as_deref(),
        outfilesuffix.as_deref(),
        relfileprefix.as_deref(),
    )
    .map(|(destination, text)| vec![destination, text])
}

/// A `mailto:` link's destination from its written target and positional subject and body,
/// as acdc's converters build it.
#[wasm_bindgen]
pub fn mailto_href(target: &str, subject: Option<String>, body: Option<String>) -> String {
    acdc_converters_core::link::mailto_href(target, subject.as_deref(), body.as_deref())
}

#[wasm_bindgen]
pub fn resolve_substitutions(entries: Vec<String>, baseline: &str) -> Result<Vec<String>, JsError> {
    let defaults: &[Substitution] = match baseline {
        "normal" => NORMAL,
        "verbatim" => VERBATIM,
        "none" => &[],
        other => return Err(JsError::new(&format!("unknown substitution baseline `{other}`"))),
    };
    Ok(SubstitutionSpec::Source(entries)
        .resolve(defaults)
        .iter()
        .map(ToString::to_string)
        .collect())
}

/// Return the underlying `acdc-parser` version.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// One `acdc-lint` diagnostic, flattened for the JS envelope. `lint` is the
/// stable rule id (kebab-case, e.g. `one-sentence-per-line`).
#[derive(serde::Serialize)]
struct LintJson {
    lint: String,
    level: String,
    message: String,
    help: Option<String>,
    line: Option<usize>,
    column: Option<usize>,
}

fn lint_diag_to_json(d: &acdc_lint::LintDiagnostic) -> LintJson {
    let (line, column) = d.location().map_or((None, None), |loc| {
        (
            Some(loc.location.start.line as usize),
            Some(loc.location.start.column as usize),
        )
    });
    LintJson {
        lint: d.lint().name().to_string(),
        level: d.level().to_string(),
        message: d.message().to_string(),
        help: d.help().map(str::to_string),
        line,
        column,
    }
}

/// Lint an AsciiDoc document with `acdc-lint`'s full default rule set.
///
/// Returns `{ ok: true, diagnostics: [{ lint, level, message, help, line,
/// column }, …] }` or `{ ok: false, error }`.
#[wasm_bindgen]
pub fn lint_block(source: &str) -> Result<JsValue, JsValue> {
    ensure_panic_hook();
    let options = acdc_lint::LintOptions::new(Vec::new());
    match acdc_lint::Lintable::lint(source, &options) {
        Ok(report) => {
            let diagnostics: Vec<LintJson> =
                report.diagnostics().iter().map(lint_diag_to_json).collect();
            to_js(&serde_json::json!({ "ok": true, "diagnostics": diagnostics }))
        }
        Err(e) => err_envelope(e),
    }
}
