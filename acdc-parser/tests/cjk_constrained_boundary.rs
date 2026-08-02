//! Constrained-formatting (monospace/bold/italic/mark) boundary recognition
//! for CJK + general Unicode punctuation.
//!
//! Before the fix, AsciiDoc constrained inline formatting required ASCII-only
//! boundary chars (space, `,`, `;`, `.`, `(`, `)`, etc.) on each side. Any
//! multi-byte UTF-8 char (e.g. `）` = `0xEF 0xBC 0x89`, `，` = `0xEF 0xBC 0x8C`)
//! failed the byte-based `match_constrained_boundary` check — the first byte
//! `0xEF` isn't in the ASCII set — so closing delimiters before CJK punct
//! silently became plain text. Result: `\`code\`，rest` rendered as a literal
//! backtick eating everything until the next `\``.
//!
//! The fix replaces the PEG ASCII char-class with a `constrained_boundary_char`
//! rule covering the ASCII set + General Punctuation (U+2000-206F) + CJK
//! Symbols/Punctuation (U+3000-303F) + Halfwidth/Fullwidth Forms punctuation
//! ranges (gaps preserved to exclude fullwidth letters U+FF21-FF3A / U+FF41-FF5A).
//! These tests pin the fix so future grammar refactors don't regress.

use acdc_parser::{Options, SafeMode, parse_inline};

fn unsafe_opts() -> Options<'static> {
    Options::builder().with_safe_mode(SafeMode::Unsafe).build()
}

fn parse_json(input: &str) -> String {
    let options = unsafe_opts();
    let r = parse_inline(input, &options).expect("parse_inline");
    serde_json::to_string(r.inlines()).unwrap()
}

// --- Monospace ----------------------------------------------------------

#[test]
fn monospace_closes_before_cjk_close_paren() {
    let json = parse_json("LSP `$0`)。替换为 `$0`");
    assert!(
        json.contains(r##""variant":"code""##),
        "no monospace node for `$0`)；JSON: {json}"
    );
    assert!(
        json.contains(r##""value":"$0""##),
        "monospace content lost; {json}"
    );
}

#[test]
fn monospace_closes_before_cjk_comma() {
    let json = parse_json("vim-snippets `bold text`，详见");
    assert!(
        json.contains(r##""variant":"code""##),
        "no monospace node; {json}"
    );
    assert!(
        json.contains(r##""value":"bold text""##),
        "monospace content lost; {json}"
    );
}

#[test]
fn monospace_closes_before_cjk_semicolon() {
    let json = parse_json("alias `+section 5+`；leveloffset 偏移");
    assert!(
        json.contains(r##""variant":"code""##),
        "no monospace node; {json}"
    );
}

#[test]
fn monospace_opens_after_cjk_open_paren() {
    let json = parse_json("（`code`）");
    assert!(
        json.contains(r##""variant":"code""##),
        "no monospace node when opened after CJK '（'; {json}"
    );
    assert!(
        json.contains(r##""value":"code""##),
        "monospace content lost; {json}"
    );
}

#[test]
fn monospace_opens_after_cjk_quote() {
    let json = parse_json("\u{201C}`code`\u{201D}"); // curly double quotes
    assert!(
        json.contains(r##""variant":"code""##),
        "no monospace node between curly quotes; {json}"
    );
}

// --- Bold ---------------------------------------------------------------

#[test]
fn bold_closes_before_cjk_punct() {
    let json = parse_json("*粗体*，结尾");
    assert!(
        json.contains(r##""variant":"strong""##),
        "no bold node; {json}"
    );
}

// --- Italic -------------------------------------------------------------

#[test]
fn italic_closes_before_cjk_punct() {
    let json = parse_json("_斜体_。结尾");
    assert!(
        json.contains(r##""variant":"emphasis""##),
        "no italic node; {json}"
    );
}

// --- Mark (highlight) ---------------------------------------------------

#[test]
fn mark_closes_before_cjk_punct() {
    let json = parse_json("#高亮#；end");
    assert!(
        json.contains(r##""variant":"mark""##),
        "no mark node; {json}"
    );
}

// --- Negative: word char after delimiter still rejects -----------------

#[test]
fn monospace_does_not_close_before_cjk_letter() {
    // CJK ideographs ARE word chars (Letter category) — closing `\`` immediately
    // followed by 中 should NOT form a constrained monospace span, otherwise
    // we'd over-recognize `\`code\`中文` as monospace + "中文".
    let json = parse_json("X`code`中文");
    assert!(
        !json.contains(r##""variant":"code""##),
        "monospace incorrectly accepted before CJK letter; {json}"
    );
}

#[test]
fn monospace_does_not_close_before_fullwidth_letter() {
    // Fullwidth A (U+FF21) is a Letter — must stay as a word char.
    let json = parse_json("X`code`\u{FF21}rest");
    assert!(
        !json.contains(r##""variant":"code""##),
        "monospace incorrectly accepted before fullwidth letter; {json}"
    );
}

// --- ASCII baseline (regression guard) ---------------------------------

#[test]
fn ascii_boundary_still_works() {
    let json = parse_json("see `code`, then");
    assert!(
        json.contains(r##""variant":"code""##),
        "ASCII comma boundary regressed; {json}"
    );
}

#[test]
fn apostrophe_is_NOT_a_constrained_boundary() {
    // Apostrophe `'` (U+0027) is DELIBERATELY NOT a constrained boundary
    // char — `` `'X' `` is the AsciiDoc smart-quote opening marker, not a
    // monospace span. Including apostrophe in the boundary class would
    // mis-parse curly-quote constructs as monospace, breaking
    // `fixtures/tests/curved_quotes.adoc`.
    //
    // The pre-refactor byte-fn `match_constrained_boundary` did list
    // apostrophe, but the pre-refactor PEG char-class did NOT — the PEG
    // drives matching, so the byte-fn entry was dead. This test pins the
    // correct (current) behavior.
    let json = parse_json("`'00s data");
    assert!(
        !json.contains(r##""variant":"code""##),
        "apostrophe must NOT trigger monospace opening boundary; {json}"
    );
}

// --- Trailing-whitespace rejection (asciidoctor parity) ---------------
//
// Asciidoctor refuses to recognize constrained monospace / passthrough when
// the content's LAST char is whitespace — `` `bold ` `` renders as literal
// backticks, NOT a `<code>` node. Mirror checks added in
// `inlines.rs:constrained_monospace` and
// `inline_preprocessor.rs:constrained_passthrough`.

#[test]
fn monospace_rejects_trailing_space_in_content() {
    // `` `code ` `` has a trailing space before the closing backtick.
    // Per asciidoctor spec this is NOT a monospace span — source must
    // round-trip as literal text.
    let json = parse_json("X `code ` Y");
    assert!(
        !json.contains(r##""variant":"code""##),
        "monospace incorrectly accepted with trailing space in content; {json}"
    );
}

#[test]
fn monospace_rejects_trailing_tab_in_content() {
    let json = parse_json("X `code\t` Y");
    assert!(
        !json.contains(r##""variant":"code""##),
        "monospace incorrectly accepted with trailing tab in content; {json}"
    );
}

#[test]
fn monospace_accepts_trailing_non_whitespace() {
    // Sanity: trailing punctuation is fine.
    let json = parse_json("X `code.` Y");
    assert!(
        json.contains(r##""variant":"code""##),
        "monospace with trailing `.` should still be recognized; {json}"
    );
}

#[test]
fn passthrough_rejects_trailing_space_in_content() {
    // `+text +` (constrained passthrough with trailing space). Asciidoctor
    // renders literal `+text +`. Pre-fix acdc stripped the `+` markers and
    // rendered bare `text `.
    let json = parse_json("X +text + Y");
    // No raw passthrough node should be emitted — must remain literal text.
    assert!(
        !json.contains(r##""name":"text","type":"string","value":"text""##)
            || json.contains(r##""value":"X +text + Y""##),
        "constrained passthrough incorrectly accepted with trailing space; {json}"
    );
}

#[test]
fn passthrough_rejects_trailing_newline_in_content() {
    // Multi-line constrained passthrough with trailing newline before `+`.
    let json = parse_json("X +text\n+ Y");
    // Source must survive intact (no `RawText` extracted from it).
    assert!(
        !json.contains(r##""variant":"raw""##),
        "constrained passthrough incorrectly accepted with trailing newline; {json}"
    );
}

// --- Inline anchor INSIDE constrained monospace ----------------------
//
// This block previously asserted the opposite — that `` `[[id]]` `` stays
// literal — reasoning from the draft AsciiDoc spec, whose substitution group
// for constrained monospace is `[specialchars, callouts]` and so excludes
// `macros`. Asciidoctor does not implement it that way: its output for
// `` `[[id]]` `` is `<code><a id="id"></a></code>`, which is what the
// `section_anchors` L1 fixture in acdc-converters-html pins.
//
// An anchor is a link target, i.e. structure rather than presentation, so this
// is L2a — the tier where Glyph may not drift from asciidoctor — and the rule
// that L1/L2a oracles must be EXTERNAL means the fixture wins over a
// self-authored assertion. The test is kept, inverted, so the behaviour stays
// covered in both directions instead of simply being deleted.

#[test]
fn anchor_is_parsed_inside_constrained_monospace() {
    let json = parse_json("X `[[anchor-id]]` Y");
    // Asciidoctor parity: the anchor IS emitted — JSON shape is
    // {"name":"anchor","type":"inline",...}.
    assert!(
        json.contains(r##""name":"anchor""##),
        "anchor missing inside constrained monospace; {json}"
    );
    // ...and the monospace span still wraps it.
    assert!(
        json.contains(r##""variant":"code""##),
        "monospace span missing; {json}"
    );
}

#[test]
fn anchor_still_works_outside_monospace() {
    // Sanity: bare `[[id]]` outside monospace still produces an anchor.
    let json = parse_json("X [[my-id]] Y");
    assert!(
        json.contains(r##""name":"anchor""##),
        "anchor regressed in plain context; {json}"
    );
}
