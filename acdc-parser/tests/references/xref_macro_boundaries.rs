//! Where an `xref:` macro target ends, against asciidoctor 2.0.26.
//!
//! asciidoctor reads the target (`InlineXrefMacroRx`) across spaces, line breaks and
//! `footnote:` up to the first `[` still there once the constructs it substitutes
//! earlier have taken their own brackets: passthroughs, quoted text with an attribute
//! list, inline images and icons, URLs, `link:`, `mailto:` and inline anchors. Every
//! expectation below is the refid and text of each xref asciidoctor made for the
//! paragraph, recorded by an inline-anchor converter.
//!
//! Not pinned: a target asciidoctor runs across substituted HTML up to a later `[`
//! (`xref:a `[b]` c`, `xref:a https://x.org[g] then xref:b[c]`) — its refid is that
//! HTML — and `anchor:id[]`, which this parser does not read.

use acdc_parser::{
    Options,
    ast::{BlockKind, ContentRef, InlineKind, StructuralRef},
    parse,
};

fn text_of(content: ContentRef<'_>) -> String {
    content
        .inlines()
        .filter_map(|inline| match inline.kind() {
            InlineKind::Plain(plain) => Some(plain.text().to_string()),
            _ => None,
        })
        .collect()
}

fn collect(content: ContentRef<'_>, out: &mut Vec<(String, String)>) {
    for inline in content.inlines() {
        match inline.kind() {
            InlineKind::CrossReference(xref) => out.push((xref.written_target().to_string(), text_of(xref.text()))),
            InlineKind::Bold(span) => collect(span.content(), out),
            InlineKind::Italic(span) => collect(span.content(), out),
            InlineKind::Monospace(span) => collect(span.content(), out),
            InlineKind::Highlight(span) => collect(span.content(), out),
            _ => {}
        }
    }
}

/// The `(target, text)` of every cross reference in the one paragraph `source` is.
fn xrefs(source: &str) -> Vec<(String, String)> {
    let parsed = parse(source, &Options::default()).expect("parses");
    let mut out = Vec::new();
    for node in parsed.document().descendants() {
        if let StructuralRef::Block(block) = node
            && let BlockKind::Paragraph(paragraph) = block.kind()
        {
            collect(paragraph.content(), &mut out);
        }
    }
    out
}

fn pair(target: &str, text: &str) -> (String, String) {
    (target.to_string(), text.to_string())
}

#[test]
fn an_xref_target_never_runs_across_a_construct_asciidoctor_substitutes_first() {
    let none: Vec<(String, String)> = Vec::new();
    assert_eq!(xrefs("See xref:install and read https://example.org[the guide]."), none);
    assert_eq!(xrefs("See xref:install and [[here]]there and [.big]#role# text."), none);
    assert_eq!(xrefs("See xref:install.adoc#setup\nfor details, and image:diagram.png[Diagram]."), none);
    assert_eq!(xrefs("See xref:install and mailto:a@b.co[mail] here."), none);
    assert_eq!(xrefs("See xref:install and link:a.pdf[A] here."), none);
    assert_eq!(xrefs("See xref:install and icon:heart[] here."), none);
    assert_eq!(xrefs("See xref:install and +[pass]+ here."), none);
    assert_eq!(xrefs("See xref:install and [#id]#marked# here."), none);
}

#[test]
fn an_xref_target_runs_to_the_first_bracket_asciidoctor_leaves() {
    assert_eq!(xrefs("See xref:install[Install] now."), vec![pair("install", "Install")]);
    assert_eq!(xrefs("See xref:my doc.adoc#x[Doc]."), vec![pair("my doc.adoc#x", "Doc")]);
    assert_eq!(xrefs("See xref:install and then [the section]."), vec![pair("install and then ", "the section")]);
    assert_eq!(xrefs("See xref:a[x] and xref:b[y]."), vec![pair("a", "x"), pair("b", "y")]);
    assert_eq!(xrefs("See xref:install\n[the section] here."), vec![pair("install\n", "the section")]);
    assert_eq!(xrefs("See xref:install and footnote:[A note.] here."), vec![pair("install and footnote:", "A note.")]);
    assert_eq!(xrefs("See xref:#frag[Here]."), vec![pair("#frag", "Here")]);
}
