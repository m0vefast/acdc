//! A section marker is a heading only with a title after it, as asciidoctor's
//! `SectionTitleRx` reads it: `== ` or `==` alone is paragraph text, never a parse error
//! (an editor that deletes a heading's words leaves exactly that line).

use acdc_parser::{
    Options, SafeMode,
    ast::{BlockKind, ContentRef, InlineKind},
};

fn options() -> Options<'static> {
    Options::builder().with_safe_mode(SafeMode::Unsafe).build().expect("options")
}

fn text(content: Option<ContentRef<'_>>) -> String {
    content
        .into_iter()
        .flat_map(ContentRef::inlines)
        .map(|inline| match inline.kind() {
            InlineKind::Plain(plain) => plain.text().to_string(),
            other => format!("{other:?}"),
        })
        .collect()
}

fn shape(doc: &str) -> Result<Vec<String>, String> {
    let parsed = acdc_parser::parse(doc, &options()).map_err(|e| e.to_string())?;
    Ok(parsed
        .document()
        .blocks()
        .map(|block| match block.kind() {
            BlockKind::Paragraph(paragraph) => format!("p {}", text(Some(paragraph.content()))),
            BlockKind::Section(_) => format!("section {}", text(block.title())),
            other => format!("{other:?}").chars().take(30).collect(),
        })
        .collect())
}

#[test]
fn a_section_marker_without_a_title_is_paragraph_text() {
    #[rustfmt::skip]
    let rows: &[(&str, &[&str])] = &[
        ("== \n\nbeta\n", &["p ==", "p beta"]),
        ("== ", &["p =="]),
        ("para\n\n== \n", &["p para", "p =="]),
        ("==\n", &["p =="]),
        ("=== \n", &["p ==="]),
        ("==  x\n", &["section x"]),
        ("== x\n", &["section x"]),
    ];
    let mut failures = Vec::new();
    for (doc, want) in rows {
        let want: Vec<String> = want.iter().map(|s| (*s).to_string()).collect();
        match shape(doc) {
            Ok(got) if got == want => {}
            got => failures.push(format!("{doc:?}\n  got  {got:?}\n  want {want:?}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The document title's marker (`=`, `#`) reads the same way: with no title after it the
/// line is paragraph text; spaces before the title are not part of it.
#[test]
fn a_document_title_marker_reads_as_a_section_marker() {
    #[rustfmt::skip]
    let rows: &[(&str, Option<(&str, usize)>, &[&str])] = &[
        ("= \n\nbody\n", None, &["p =", "p body"]),
        ("=\n\nbody\n", None, &["p =", "p body"]),
        ("# \n\nbody\n", None, &["p #", "p body"]),
        ("=  Title\n\nbody\n", Some(("Title", 3)), &["p body"]),
        ("= Title\n\nbody\n", Some(("Title", 2)), &["p body"]),
    ];
    let mut failures = Vec::new();
    for (doc, title, blocks) in rows {
        let parsed = match acdc_parser::parse(doc, &options()) {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{doc:?}: {e}"));
                continue;
            }
        };
        let got_title = parsed.document().title().filter(|title| !title.is_empty()).map(|title| {
            let at = title.inlines().next().map_or(usize::MAX, |inline| inline.location().absolute_start());
            (text(Some(title)), at)
        });
        let want_title = title.map(|(t, at)| (t.to_string(), at));
        let want_blocks: Vec<String> = blocks.iter().map(|s| (*s).to_string()).collect();
        let got_blocks = shape(doc);
        if got_title != want_title || got_blocks.as_ref() != Ok(&want_blocks) {
            failures.push(format!("{doc:?}\n  got  {got_title:?} {got_blocks:?}\n  want {want_title:?} {want_blocks:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
