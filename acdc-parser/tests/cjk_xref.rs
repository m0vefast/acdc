//! CJK xref + `xref:#X[Y]` short-form regression coverage.
//!
//! Three grammar gaps surfaced during Glyph integration:
//!   1. `<<id>>` shorthand only accepted ASCII in target — `<<结论>>` was
//!      silently swallowed into the surrounding text node.
//!   2. `path_fragment()` only accepted ASCII in `#anchor` — CJK ids in
//!      `xref:other.adoc#章节[…]` failed to match.
//!   3. `cross_reference_macro` required a non-empty `source()` for the
//!      target — `xref:#X[Y]` (explicit-hash, no file path) was rejected.
//!
//! These tests pin the fix so future grammar refactors don't regress.

use acdc_parser::{Options, SafeMode, parse, parse_inline};

fn unsafe_opts() -> Options<'static> {
    Options::builder().with_safe_mode(SafeMode::Unsafe).build()
}

#[test]
fn cjk_shorthand_parses_as_xref() {
    let options = unsafe_opts();
    let r = parse_inline("见 <<结论>>", &options).expect("parse");
    let json = serde_json::to_string(r.inlines()).unwrap();
    assert!(
        json.contains(r##""name":"xref""##),
        "shorthand CJK: no xref node; {json}"
    );
    assert!(
        json.contains(r##""target":"结论""##),
        "shorthand CJK: wrong target; {json}"
    );
}

#[test]
fn xref_macro_hash_only_form() {
    let options = unsafe_opts();
    let r = parse_inline("见 xref:#concl[末章]", &options).expect("parse");
    let json = serde_json::to_string(r.inlines()).unwrap();
    assert!(
        json.contains(r##""name":"xref""##),
        "macro #-only: no xref node; {json}"
    );
    // target carries the leading `#` — the TS-side converter strips it.
    assert!(
        json.contains(r##""target":"#concl""##),
        "macro #-only: wrong target; {json}"
    );
}

#[test]
fn xref_macro_cjk_fragment() {
    let options = unsafe_opts();
    let r = parse_inline("见 xref:#结论[末章]", &options).expect("parse");
    let json = serde_json::to_string(r.inlines()).unwrap();
    assert!(
        json.contains(r##""name":"xref""##),
        "macro CJK frag: no xref; {json}"
    );
    assert!(
        json.contains(r##""target":"#结论""##),
        "macro CJK frag: wrong target; {json}"
    );
}

#[test]
fn xref_macro_cjk_filepath_fragment() {
    let options = unsafe_opts();
    let r = parse_inline("见 xref:other.adoc#章节[末章]", &options).expect("parse");
    let json = serde_json::to_string(r.inlines()).unwrap();
    assert!(
        json.contains(r##""name":"xref""##),
        "macro CJK file+frag: no xref; {json}"
    );
    assert!(
        json.contains(r##""target":"other.adoc#章节""##),
        "macro CJK file+frag: wrong target; {json}"
    );
}

#[test]
fn full_doc_cjk_section_and_xref() {
    // End-to-end: a CJK `[#id]` section anchor + a same-page `<<id>>` xref
    // must both round-trip the CJK id intact.
    let src = "[#结论]\n== 结论\n\n见 <<结论>>。\n";
    let r = parse(src, &unsafe_opts()).expect("parse");
    let json = serde_json::to_string(r.document()).unwrap();
    assert!(
        json.contains(r##""target":"结论""##),
        "full doc: xref missing; {json}"
    );
    assert!(
        json.contains(r##""id":"结论""##),
        "full doc: section anchor missing; {json}"
    );
}

#[test]
fn ascii_xref_shorthand_still_works() {
    // Regression — relaxing the char class must NOT break ASCII parsing.
    let options = unsafe_opts();
    let r = parse_inline("see <<my-id>>", &options).expect("parse");
    let json = serde_json::to_string(r.inlines()).unwrap();
    assert!(
        json.contains(r##""target":"my-id""##),
        "ASCII shorthand broken; {json}"
    );
}
