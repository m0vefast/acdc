//! CJK cross reference targets and the `xref:#X[Y]` short form.
//!
//! Gaps found during Glyph integration: `<<id>>` with a CJK id, a CJK `#fragment`
//! after a file path, and `xref:#X[Y]` with no file path.

use acdc_parser::{
    Options, SafeMode,
    ast::{BlockKind, InlineKind, StructuralRef},
    parse, parse_inline,
};

fn unsafe_opts() -> Options<'static> {
    Options::builder().with_safe_mode(SafeMode::Unsafe).build().expect("options")
}

/// The written target of every cross reference in `input`.
fn written_targets(input: &str) -> Vec<String> {
    let parsed = parse_inline(input, &unsafe_opts()).expect("parse");
    parsed
        .content()
        .inlines()
        .filter_map(|inline| match inline.kind() {
            InlineKind::CrossReference(xref) => Some(xref.written_target().to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn cjk_shorthand_parses_as_xref() {
    assert_eq!(written_targets("见 <<结论>>"), ["结论"]);
}

#[test]
fn xref_macro_hash_only_form() {
    assert_eq!(written_targets("见 xref:#concl[末章]"), ["#concl"]);
}

#[test]
fn xref_macro_cjk_fragment() {
    assert_eq!(written_targets("见 xref:#结论[末章]"), ["#结论"]);
}

#[test]
fn xref_macro_cjk_filepath_fragment() {
    assert_eq!(written_targets("见 xref:other.adoc#章节[末章]"), ["other.adoc#章节"]);
}

#[test]
fn full_doc_cjk_section_and_xref() {
    let parsed = parse("[#结论]\n== 结论\n\n见 <<结论>>。\n", &unsafe_opts()).expect("parse");
    let document = parsed.document();
    let mut section_ids = Vec::new();
    let mut targets = Vec::new();
    for node in document.descendants() {
        let StructuralRef::Block(block) = node else { continue };
        match block.kind() {
            BlockKind::Section(section) => section_ids.push(section.resolve_id().into_owned()),
            BlockKind::Paragraph(paragraph) => targets.extend(paragraph.content().inlines().filter_map(|inline| {
                match inline.kind() {
                    InlineKind::CrossReference(xref) => Some((xref.target().to_string(), xref.target_is_local())),
                    _ => None,
                }
            })),
            _ => {}
        }
    }
    assert_eq!(section_ids, ["结论"]);
    assert_eq!(targets, [("结论".to_string(), true)]);
    assert!(document.references().get("结论").is_some(), "the CJK id resolves");
}

#[test]
fn ascii_xref_shorthand_still_works() {
    assert_eq!(written_targets("see <<my-id>>"), ["my-id"]);
}
