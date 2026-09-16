//! Facts a consumer used to re-derive from the text: what a cross reference names,
//! where its target is written, and which include directives the document holds.
//!
//! Every rule here is asciidoctor 2.0.26's. A consumer that rewrites a reference or an
//! include must not guess any of it: a guessed rule is a second implementation of the
//! parser, and the two drift.

use acdc_parser::{Block, InlineMacro, InlineNode, LineIndex, Options, XrefTarget, parse, parse_inline};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The inline nodes of every top-level paragraph, in order.
fn paragraph_inlines<'a>(doc: &'a acdc_parser::Document<'a>) -> impl Iterator<Item = &'a InlineNode<'a>> {
    doc.blocks.iter().flat_map(|block| match block {
        Block::Paragraph(p) => p.content.iter(),
        _ => [].iter(),
    })
}

fn only_xref<'a>(input: &str, inlines: &'a [InlineNode<'a>]) -> Result<&'a acdc_parser::CrossReference<'a>, String> {
    match inlines {
        [InlineNode::Macro(InlineMacro::CrossReference(xref))] => Ok(xref),
        other => Err(format!("{input}: expected one cross reference, got {other:?}")),
    }
}

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
        let xref = only_xref(input, parsed.inlines())?;
        assert_eq!(xref.target_kind(), expected, "{input}");
    }
    Ok(())
}

/// The input as saved: clean, with trailing whitespace above, and with CRLF line ends —
/// the last two differ from the preprocessed text acdc's byte offsets count.
fn as_saved(input: &str) -> [String; 3] {
    [input.to_string(), input.replacen("= Doc\n", "= Doc \t\n", 1), input.replace('\n', "\r\n")]
}

/// The target's own location, in the input's bytes — so a rewrite replaces exactly the
/// target and nothing around it, whatever the form, text, preceding characters or line
/// ends.
#[test]
fn a_cross_reference_locates_its_target() -> TestResult {
    for input in as_saved("= Doc\n\nSé <<guide.adoc#install,Install>> and xref:parts/one.adoc#x[One] ok.\n") {
        let parsed = parse(&input, &Options::default())?;
        let lines = LineIndex::new(&input);
        let targets: Vec<Option<&str>> = paragraph_inlines(parsed.document())
            .filter_map(|node| match node {
                InlineNode::Macro(InlineMacro::CrossReference(xref)) => {
                    Some(lines.range(&xref.target_location).map(|span| &input[span]))
                }
                _ => None,
            })
            .collect();
        assert_eq!(targets, [Some("guide.adoc#install"), Some("parts/one.adoc#x")], "{input:?}");
    }
    Ok(())
}

/// A `link:` macro locates its target in the input, and refuses when the written bytes
/// are not the parsed target — an attribute reference substituted into it.
#[test]
fn a_link_macro_locates_its_target() -> TestResult {
    for input in as_saved("= Doc\n:dir: files\n\nSé link:files/report.pdf[the report] and link:{dir}/other.pdf[x].\n") {
        let parsed = parse(&input, &Options::default())?;
        let lines = LineIndex::new(&input);
        let targets: Vec<Option<&str>> = paragraph_inlines(parsed.document())
            .filter_map(|node| match node {
                InlineNode::Macro(InlineMacro::Link(link)) => Some(link.target_span(&lines).map(|span| &input[span])),
                _ => None,
            })
            .collect();
        assert_eq!(targets, [Some("files/report.pdf"), None], "{input:?}");
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
