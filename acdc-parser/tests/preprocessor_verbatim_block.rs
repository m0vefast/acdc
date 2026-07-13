//! Preprocessor behavior inside verbatim blocks (`----`/`....`/`++++`).
//!
//! asciidoctor parity (cron #14 decision): the preprocessor runs BEFORE block
//! parsing, so `include::`, `ifdef::`, `ifeval::`, `endif::` directives are
//! processed everywhere — including inside `----` listing / `....` literal /
//! `++++` passthrough blocks. asciidoctor 2.0.26 confirmed: an undefined
//! `ifdef::var[]` inside a listing DROPS its body; a defined one keeps it; the
//! directive lines themselves are consumed. To render a literal directive in a
//! source-code example, escape it (`\ifdef::…`) — asciidoctor strips the
//! backslash and shows the directive text.

use acdc_parser::{Options, parse};

fn parse_text(input: &str) -> String {
    let opts = Options::default();
    let r = parse(input, &opts).expect("parse");
    serde_json::to_string(r.document()).unwrap()
}

#[test]
fn ifdef_undefined_var_inside_listing_block_drops_body() {
    // `var` is NOT defined. asciidoctor processes the directive even inside a
    // `[source,asciidoc]` listing → the gated body is dropped and the
    // `ifdef`/`endif` lines are consumed.
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
        !json.contains("content that must survive"),
        "undefined ifdef body should be dropped (asciidoctor parity); {json}"
    );
    assert!(
        !json.contains("ifdef::var[]") && !json.contains("endif::[]"),
        "directive lines should be consumed, not left literal; {json}"
    );
}

#[test]
fn ifdef_defined_var_inside_listing_block_keeps_body() {
    // `var` IS defined → the directive triggers inside the listing block and
    // the body survives; the directive lines are consumed.
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
        json.contains("literal body line"),
        "defined ifdef body should survive; {json}"
    );
    assert!(
        !json.contains("ifdef::var[]") && !json.contains("endif::[]"),
        "directive lines should be consumed, not left literal; {json}"
    );
}

#[test]
fn ifdef_outside_verbatim_block_still_processed() {
    // Sanity: directive processing works OUTSIDE verbatim blocks too. With
    // `var` undefined, the gated paragraph is dropped.
    let input = "\
ifdef::undefined_var[]
should be dropped
endif::[]

surviving paragraph
";
    let json = parse_text(input);
    assert!(
        !json.contains("should be dropped"),
        "ifdef-gated paragraph must drop when var undefined; {json}"
    );
    assert!(
        json.contains("surviving paragraph"),
        "non-gated paragraph missing; {json}"
    );
}

#[test]
fn escape_unwrap_in_verbatim_block() {
    // `\ifdef::var[]` inside a listing block — asciidoctor strips the backslash
    // and shows the directive text LITERALLY (the directive is not processed).
    let input = "\
[source,asciidoc]
----
\\ifdef::var[]
body
\\endif::[]
----
";
    let json = parse_text(input);
    assert!(
        json.contains("ifdef::var[]"),
        "escape-unwrap failed for \\ifdef (should show literal directive); {json}"
    );
    assert!(
        json.contains("endif::[]"),
        "escape-unwrap failed for \\endif; {json}"
    );
    assert!(json.contains("body"), "verbatim body missing; {json}");
}

#[test]
fn directive_in_passthrough_block_is_processed() {
    // asciidoctor parity: `++++` passthrough blocks are also preprocessed, so
    // an undefined `ifdef::var[]` drops its body there too.
    let input = "\
++++
ifdef::var[]
pass content
endif::[]
++++
";
    let json = parse_text(input);
    assert!(
        !json.contains("pass content"),
        "undefined ifdef body in passthrough should be dropped; {json}"
    );
}
