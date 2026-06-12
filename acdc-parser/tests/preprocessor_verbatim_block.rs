//! Preprocessor behavior inside verbatim blocks (`----`/`....`/`++++`).
//!
//! Directives like `include::`, `ifdef::`, `ifeval::`, `endif::` inside a
//! verbatim block must NOT be processed — they render as literal source per
//! asciidoctor semantics. Pre-fix, undefined `ifdef::var[]` silently dropped
//! lines from the preprocessor output, shifting downstream source-line
//! numbers and breaking cursor placement in everything after the block.
//!
//! Fix landed in `acdc-parser/src/preprocessor/mod.rs`.

use acdc_parser::{Options, parse};

fn parse_text(input: &str) -> String {
    let opts = Options::default();
    let r = parse(input, &opts).expect("parse");
    serde_json::to_string(r.document()).unwrap()
}

#[test]
fn ifdef_undefined_var_inside_listing_block_preserves_lines() {
    // `var` is NOT defined. Without the fix, `ifdef::var[]` … `endif::[]`
    // would evaluate false and DROP the body lines silently. With the fix,
    // the listing block content is preserved verbatim.
    let input = "\
[source,asciidoc]
----
ifdef::var[]
content that must survive
endif::[]
----
";
    let json = parse_text(input);
    assert!(
        json.contains("ifdef::var[]"),
        "ifdef directive line dropped from listing block; {json}"
    );
    assert!(
        json.contains("content that must survive"),
        "listing block body dropped when ifdef undefined; {json}"
    );
    assert!(
        json.contains("endif::[]"),
        "endif directive line dropped from listing block; {json}"
    );
}

#[test]
fn ifdef_defined_var_inside_listing_block_still_literal() {
    // Even when `var` IS defined, the directive must NOT trigger inside the
    // listing block — asciidoctor treats the body verbatim regardless.
    let input = "\
:var: true

[source,asciidoc]
----
ifdef::var[]
literal body line
endif::[]
----
";
    let json = parse_text(input);
    assert!(
        json.contains("ifdef::var[]") && json.contains("endif::[]"),
        "verbatim block contents lost their directive lines; {json}"
    );
    assert!(
        json.contains("literal body line"),
        "listing block body content missing; {json}"
    );
}

#[test]
fn ifdef_outside_verbatim_block_still_processed() {
    // Sanity: directive processing still works for ifdef OUTSIDE verbatim
    // blocks. With `var` undefined, the gated paragraph should be dropped.
    let input = "\
ifdef::undefined_var[]
should be dropped
endif::[]

surviving paragraph
";
    let json = parse_text(input);
    assert!(
        !json.contains("should be dropped"),
        "ifdef-gated paragraph regressed (must drop when var undefined); {json}"
    );
    assert!(
        json.contains("surviving paragraph"),
        "non-gated paragraph missing; {json}"
    );
}

#[test]
fn escape_unwrap_in_verbatim_block() {
    // `\ifdef::var[]` inside a listing block — backslash is stripped for
    // display but the directive is NOT processed.
    let input = "\
[source,asciidoc]
----
\\ifdef::var[]
body
\\endif::[]
----
";
    let json = parse_text(input);
    // The `\` should be unwrapped; body and directive lines all literal.
    assert!(
        json.contains("ifdef::var[]"),
        "escape-unwrap failed for \\ifdef; {json}"
    );
    assert!(
        json.contains("endif::[]"),
        "escape-unwrap failed for \\endif; {json}"
    );
    assert!(
        json.contains("body"),
        "verbatim body missing; {json}"
    );
}

#[test]
fn directive_in_passthrough_block_also_preserved() {
    // `++++` passthrough block also verbatim — directive must NOT process.
    let input = "\
++++
ifdef::var[]
pass content
endif::[]
++++
";
    let json = parse_text(input);
    assert!(
        json.contains("ifdef::var[]") && json.contains("pass content"),
        "passthrough block lost directive line or body; {json}"
    );
}
