//! A bibliography anchor `[[[id]]]` serializes with `"bibliography":true` so
//! downstream renderers (Glyph's `asg-to-mdast`) draw the visible `[label]`.
//! A regular anchor `[[id]]` never carries the flag.

use acdc_parser::{Options, SafeMode, parse};

fn document_json(source: &str) -> String {
    let options = Options::builder().with_safe_mode(SafeMode::Unsafe).build().expect("options");
    let result = parse(source, &options).expect("parse");
    serde_json::to_string(&result.document()).expect("serialize")
}

#[test]
fn bibliography_anchor_serializes_with_the_bibliography_flag() {
    let json = document_json("[bibliography]\n* [[[ref-1]]] A book.\n");
    assert!(json.contains(r#""bibliography":true"#), "missing flag; JSON was:\n{json}");
    assert!(json.contains(r#""id":"ref-1""#), "missing id; JSON was:\n{json}");
}

#[test]
fn bibliography_anchor_with_reftext_carries_its_label() {
    let json = document_json("[bibliography]\n* [[[ref-2,Smith2024]]] A book.\n");
    assert!(json.contains(r#""bibliography":true"#), "missing flag; JSON was:\n{json}");
    assert!(json.contains(r#""bibliography_label""#), "missing label; JSON was:\n{json}");
    assert!(json.contains("Smith2024"), "missing label text; JSON was:\n{json}");
}

#[test]
fn regular_inline_anchor_omits_the_bibliography_flag() {
    let json = document_json("Text [[my-anchor]] here.\n");
    assert!(!json.contains(r#""bibliography""#), "regular anchor leaked the flag; JSON was:\n{json}");
    assert!(json.contains(r#""id":"my-anchor""#), "missing id; JSON was:\n{json}");
}

#[test]
fn section_anchor_in_block_metadata_omits_the_bibliography_flag() {
    let json = document_json("[[my-section]]\n== Title\n");
    assert!(!json.contains(r#""bibliography""#), "section anchor leaked the flag; JSON was:\n{json}");
    assert!(json.contains(r#""id":"my-section""#), "missing id; JSON was:\n{json}");
}
