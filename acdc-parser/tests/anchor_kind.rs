//! Bibliography anchor `[[[id]]]` must serialize with `kind: "bibliography"`
//! in the ASG output so downstream renderers (Glyph etc.) can switch to the
//! visible `[id]` label rendering. Regular inline anchors `[[id]]` retain
//! the default kind and must NOT emit the `kind` field (to keep existing
//! fixtures byte-equal).

use acdc_parser::{Options, SafeMode, parse_inline};

fn build_options() -> Options<'static> {
    Options::builder().with_safe_mode(SafeMode::Unsafe).build()
}

#[test]
fn bibliography_anchor_serializes_with_kind_field() {
    let options = build_options();
    let result = parse_inline("[[[ref-1]]]", &options).expect("parse");
    let json = serde_json::to_string(result.inlines()).expect("serialize");
    // Must contain the kind discriminator so downstream can render
    // `<a id>` + visible `[id]` label.
    assert!(
        json.contains(r#""kind":"bibliography""#),
        "biblio anchor missing kind field; JSON was:\n{json}"
    );
    assert!(json.contains(r#""id":"ref-1""#), "biblio anchor missing id");
}

#[test]
fn bibliography_anchor_with_reftext_includes_xreflabel() {
    let options = build_options();
    let result = parse_inline("[[[ref-2,Smith2024]]]", &options).expect("parse");
    let json = serde_json::to_string(result.inlines()).expect("serialize");
    assert!(
        json.contains(r#""kind":"bibliography""#),
        "biblio anchor with reftext missing kind field; JSON was:\n{json}"
    );
    assert!(
        json.contains(r#""xreflabel":"Smith2024""#),
        "biblio anchor missing xreflabel; JSON was:\n{json}"
    );
}

#[test]
fn regular_inline_anchor_omits_kind_field() {
    let options = build_options();
    let result = parse_inline("[[my-section]]", &options).expect("parse");
    let json = serde_json::to_string(result.inlines()).expect("serialize");
    // Default (Inline) kind must be skipped to keep existing ASG fixtures
    // byte-equal — `skip_serializing_if = AnchorKind::is_inline` on the derive
    // AND the manual `is_inline()` check in the InlineNode serializer.
    assert!(
        !json.contains(r#""kind""#),
        "regular anchor leaked kind field; JSON was:\n{json}"
    );
    assert!(json.contains(r#""id":"my-section""#), "anchor missing id");
}

/// Regression: `BlockMetadata` carries its own `Anchor` list (e.g. `[[id]]`
/// on a section header). The derive-based `Serialize` on `Anchor` skips the
/// `kind` field when default (Inline). If a future derive refactor or a new
/// `Anchor` constructor forgets to set `kind: Inline`, the field would leak
/// into every existing fixture and silently break byte-equality. Pin it here.
#[test]
fn section_anchor_in_block_metadata_omits_kind_field() {
    use acdc_parser::parse;
    let options = build_options();
    let result = parse("[[my-section]]\n== Title\n", &options).expect("parse");
    let json = serde_json::to_string(result.document()).expect("serialize");
    assert!(
        !json.contains(r#""kind""#),
        "section anchor leaked kind field; JSON was:\n{json}"
    );
    // Sanity: the anchor was actually parsed and attached to the section.
    assert!(
        json.contains(r#""id":"my-section""#),
        "section anchor missing id; JSON was:\n{json}"
    );
}
