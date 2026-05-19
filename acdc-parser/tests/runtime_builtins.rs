//! Behaviour pins for the G1/G3 lenient-directive patches.
//!
//! G1 — `with_runtime_builtins()` injects Asciidoctor-style attributes
//! (`asciidoctor`, `asciidoctor-version`, `safe-mode-{name}`, `backend`,
//! `doctype`, ...) so that `ifdef::asciidoctor[]` and the safe-mode marker
//! gates behave the way real-world Asciidoctor docs expect. Top-level
//! `parse()` auto-calls this builder, so every consumer gets the trio
//! without opting in.
//!
//! G3 — The injected attributes use `DocumentAttributes::insert_default`,
//! which keeps them out of the `explicit` map and therefore out of
//! serialized ASG output. Asciidoctor authors gate on these attributes;
//! Glyph publishers do NOT want them leaking into front-matter / search
//! indices / published markdown.
//!
//! Regression class these tests pin:
//!   • Future refactor that removes the auto-`with_runtime_builtins()` call
//!     from `parse()` in `acdc-parser/src/lib.rs` — `ifdef::asciidoctor[]`
//!     would silently drop content in real-world docs.
//!   • Future refactor that promotes built-ins from `insert_default` to
//!     `insert` — would leak `asciidoctor-version=…-acdc` into every
//!     published doc's serialized attribute section.

use acdc_parser::{Options, SafeMode, parse};

/// G1 keep: under any safe mode, `asciidoctor` is auto-defined by
/// `with_runtime_builtins()`, so an `ifdef::asciidoctor[]` block keeps
/// its content.
#[test]
fn ifdef_asciidoctor_keeps_content_under_unsafe() {
    let source = "before\n\nifdef::asciidoctor[]\nkept inside ifdef\nendif::[]\n\nafter\n";
    let opts = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let result = parse(source, &opts).expect("parse succeeds");
    let serialized = serde_json::to_string(result.document()).expect("serialize doc");
    assert!(
        serialized.contains("kept inside ifdef"),
        "expected content inside `ifdef::asciidoctor[]` to survive (with_runtime_builtins should auto-define `asciidoctor`); serialized doc = {serialized}"
    );
}

/// G1 drop: under SafeMode::Unsafe the runtime marker is `safe-mode-unsafe`,
/// so `ifdef::safe-mode-secure[]` is gated on an undefined attribute and
/// the conditional block drops.
#[test]
fn ifdef_safe_mode_secure_drops_under_unsafe() {
    let source = "before\n\nifdef::safe-mode-secure[]\nshould drop\nendif::[]\n\nafter\n";
    let opts = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let result = parse(source, &opts).expect("parse succeeds");
    let serialized = serde_json::to_string(result.document()).expect("serialize doc");
    assert!(
        !serialized.contains("should drop"),
        "expected content inside `ifdef::safe-mode-secure[]` to drop under SafeMode::Unsafe; serialized doc = {serialized}"
    );
}

/// G1 keep symmetric: under SafeMode::Unsafe the runtime marker
/// `safe-mode-unsafe` IS defined, so `ifdef::safe-mode-unsafe[]` keeps.
/// Without this symmetric test, a future regression could flip both
/// branches and the drop test alone would still pass.
#[test]
fn ifdef_safe_mode_unsafe_keeps_under_unsafe() {
    let source = "before\n\nifdef::safe-mode-unsafe[]\nshould keep\nendif::[]\n\nafter\n";
    let opts = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let result = parse(source, &opts).expect("parse succeeds");
    let serialized = serde_json::to_string(result.document()).expect("serialize doc");
    assert!(
        serialized.contains("should keep"),
        "expected content inside `ifdef::safe-mode-unsafe[]` to survive under SafeMode::Unsafe; serialized doc = {serialized}"
    );
}

/// G3 ASG cleanliness: built-ins injected via `insert_default` never
/// surface in serialized output. A trivial doc with no `:attr: val`
/// header lines must serialize its `attributes` map as empty `{}`.
#[test]
fn runtime_builtins_stay_out_of_serialized_attributes() {
    let source = "= Doc Title\n\nbody text\n";
    let opts = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let result = parse(source, &opts).expect("parse succeeds");

    // Direct API check: explicit map is empty.
    assert!(
        result.document().attributes.is_empty(),
        "built-ins must not be tracked as explicit attributes; iter = {:?}",
        result.document().attributes.iter().collect::<Vec<_>>()
    );

    // Serialized JSON check: `attributes` field present as empty object,
    // and none of the built-in keys appear (covers any future Serialize
    // bypass that doesn't go through the `explicit` map).
    let serialized = serde_json::to_value(result.document()).expect("serialize doc to JSON value");
    let attrs = serialized
        .get("attributes")
        .expect("document has `attributes` field");
    let obj = attrs
        .as_object()
        .expect("document.attributes serializes as JSON object");
    assert!(
        obj.is_empty(),
        "serialized attributes must be empty, got {obj:?}"
    );

    // Belt-and-braces: scan the full serialized blob for any built-in
    // attribute name. None of these strings should appear in the doc
    // when only built-ins are defined.
    let serialized_str = serialized.to_string();
    for built_in in [
        "asciidoctor-version",
        "safe-mode-name",
        "safe-mode-level",
        "safe-mode-unsafe",
        "backend-html5",
        "basebackend-html",
        "filetype-html",
        "doctype-article",
    ] {
        assert!(
            !serialized_str.contains(built_in),
            "built-in attribute `{built_in}` leaked into serialized doc: {serialized_str}"
        );
    }
}

/// G3 user override DOES surface: explicit `:attr: val` header lines
/// remain in the serialized output. Without this, the previous test
/// could be vacuously satisfied by a Serialize impl that drops the
/// whole `attributes` field (regressing real header attributes).
#[test]
fn user_explicit_attributes_still_serialize() {
    let source = ":custom: hello\n:another: world\n\nbody\n";
    let opts = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let result = parse(source, &opts).expect("parse succeeds");

    let serialized = serde_json::to_value(result.document()).expect("serialize doc to JSON value");
    let obj = serialized
        .get("attributes")
        .and_then(|v| v.as_object())
        .expect("attributes is a JSON object");
    assert!(
        obj.contains_key("custom"),
        "user-set attribute `custom` should serialize, got {obj:?}"
    );
    assert!(
        obj.contains_key("another"),
        "user-set attribute `another` should serialize, got {obj:?}"
    );
}
