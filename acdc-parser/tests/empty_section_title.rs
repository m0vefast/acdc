//! A section marker is a heading only with a title after it, as asciidoctor's
//! `SectionTitleRx` reads it: `== ` or `==` alone is paragraph text, never a parse error
//! (an editor that deletes a heading's words leaves exactly that line).

use acdc_parser::{Block, InlineNode, Options, SafeMode};

fn shape(doc: &str) -> Result<Vec<String>, String> {
    let options = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let parsed = acdc_parser::parse(doc, &options).map_err(|e| e.to_string())?;
    let text = |inlines: &[InlineNode]| {
        inlines
            .iter()
            .map(|i| match i {
                InlineNode::PlainText(t) => t.content.to_string(),
                other => format!("{other:?}"),
            })
            .collect::<String>()
    };
    Ok(parsed
        .document()
        .blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => format!("p {}", text(&p.content)),
            Block::Section(s) => format!("section {}", text(&s.title)),
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
    let options = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
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
        let parsed = match acdc_parser::parse(doc, &options) {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{doc:?}: {e}"));
                continue;
            }
        };
        let got_title = parsed
            .document()
            .header
            .as_ref()
            .filter(|h| !h.title.is_empty())
            .map(|h| {
                let text: String = h
                    .title
                    .iter()
                    .map(|i| match i {
                        InlineNode::PlainText(t) => t.content.to_string(),
                        other => format!("{other:?}"),
                    })
                    .collect();
                let at = match h.title.first() {
                    Some(InlineNode::PlainText(t)) => t.location.absolute_start,
                    _ => usize::MAX,
                };
                (text, at)
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
