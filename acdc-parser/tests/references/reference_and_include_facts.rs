//! Facts a consumer used to re-derive from the text: what a cross reference names and
//! which include directives the document holds.
//!
//! Every rule here is asciidoctor 2.0.26's. A consumer that rewrites a reference or an
//! include must not guess any of it: a guessed rule is a second implementation of the
//! parser, and the two drift.

use acdc_parser::{Options, XrefTarget, ast::InlineKind, parse, parse_inline, xref_target_kind};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// `sub_inline_xrefs`: a shorthand without `#` is an id; with `#` it names a document,
/// dropping any AsciiDoc extension. The macro names a document for `.adoc`, or for no
/// extension with a `#`; another extension is a file; no extension and no `#` is an id.
#[test]
fn a_cross_reference_says_what_it_names() -> TestResult {
    use XrefTarget::{Document, File, Id};
    for (input, expected) in [
        ("<<install>>", Id { id: Some("install") }),
        ("<<guide.adoc>>", Id { id: Some("guide.adoc") }),
        ("<<#install>>", Id { id: Some("install") }),
        ("<<guide.adoc#install>>", Document { page: "guide.adoc", name: "guide", fragment: Some("install") }),
        ("<<guide.asciidoc#install>>", Document { page: "guide.asciidoc", name: "guide", fragment: Some("install") }),
        ("<<guide#install>>", Document { page: "guide", name: "guide", fragment: Some("install") }),
        ("<<../parts/chap1#c1,Chapter>>", Document { page: "../parts/chap1", name: "../parts/chap1", fragment: Some("c1") }),
        ("xref:guide.adoc[]", Document { page: "guide.adoc", name: "guide", fragment: None }),
        ("xref:guide.adoc#install[]", Document { page: "guide.adoc", name: "guide", fragment: Some("install") }),
        ("xref:guide#install[]", Document { page: "guide", name: "guide", fragment: Some("install") }),
        ("xref:guide.asciidoc#install[]", File { page: "guide.asciidoc", fragment: Some("install") }),
        ("xref:report.pdf[]", File { page: "report.pdf", fragment: None }),
        ("xref:install[]", Id { id: Some("install") }),
        ("xref:#install[Here]", Id { id: Some("install") }),
    ] {
        let parsed = parse_inline(input, &Options::default())?;
        let kinds: Vec<XrefTarget<'_>> = parsed
            .content()
            .inlines()
            .filter_map(|inline| match inline.kind() {
                InlineKind::CrossReference(xref) => Some(xref_target_kind(xref.written_target(), xref.form())),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, [expected], "{input}");
    }
    Ok(())
}

/// Every include directive the preprocessor acts on in the parsed input: its line, the
/// target as written, and the target after attribute substitution. An escaped directive
/// and one inside a false conditional are not acted on, so they are not reported.
#[test]
fn the_include_directives_a_document_holds() -> TestResult {
    let input = "= Book\n:parts: chapters\n\ninclude::./intro.adoc[]\n\ninclude::{parts}/one.adoc[leveloffset=+1]  \n\ninclude::../../shared/x.adoc[]\n\n\\include::escaped.adoc[]\n\nifdef::never-set[]\ninclude::skipped.adoc[]\nendif::[]\n";
    let parsed = parse(input, &Options::default())?;
    let got: Vec<(usize, usize, &str, &str, &str)> = parsed
        .include_directives()
        .iter()
        .map(|d| (d.line, d.target_column, d.target.as_str(), d.resolved_target.as_str(), d.path.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (4, 9, "./intro.adoc", "./intro.adoc", "intro.adoc"),
            (6, 9, "{parts}/one.adoc", "chapters/one.adoc", "chapters/one.adoc"),
            (8, 9, "../../shared/x.adoc", "../../shared/x.adoc", "../../shared/x.adoc"),
        ]
    );
    Ok(())
}

/// Line numbers are the input's own, whatever the line endings.
#[test]
fn include_directive_lines_survive_crlf() -> TestResult {
    let input = "= Book\r\n\r\ninclude::a.adoc[]\r\n";
    let parsed = parse(input, &Options::default())?;
    let lines: Vec<usize> = parsed.include_directives().iter().map(|d| d.line).collect();
    assert_eq!(lines, [3]);
    Ok(())
}
