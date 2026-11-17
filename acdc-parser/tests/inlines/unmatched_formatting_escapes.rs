//! A backslash before a formatting mark is removed only when the escaped mark
//! opens a span that would otherwise match; a lone `\*` stays as written (the
//! quote substitution patterns carry the backslash, `\\?\*(\S|\S.*?\S)\*(?!\w)`).
//! Every expectation is Asciidoctor 2.0.26's text for the same source.

use acdc_parser::{
    Options,
    ast::{BlockKind, BlockRef, InlineKind},
    parse,
};

type Error = Box<dyn std::error::Error>;

#[rstest::rstest]
#[case("a \\*b", "a \\*b")]
#[case("a \\*b*", "a *b*")]
#[case("tb x:x {xbo\\*", "tb x:x {xbo\\*")]
#[case("ne \\`tab", "ne \\`tab")]
#[case("a \\_b", "a \\_b")]
#[case("a \\_b_ c", "a _b_ c")]
#[case("a \\#b", "a \\#b")]
#[case("a \\#b# c", "a #b# c")]
#[case("x \\`y` z", "x `y` z")]
fn an_escape_is_removed_only_before_a_span(#[case] source: &str, #[case] want: &str) -> Result<(), Error> {
    let parsed = parse(source, &Options::default())?;
    let Some(BlockKind::Paragraph(paragraph)) = parsed.document().blocks().next().map(BlockRef::kind) else {
        return Err("expected a paragraph".into());
    };
    let text: String = paragraph
        .content()
        .inlines()
        .map(|node| match node.kind() {
            InlineKind::Plain(text) => Ok(text.text().to_string()),
            other => Err(format!("unexpected inline {other:?}")),
        })
        .collect::<Result<_, _>>()?;
    assert_eq!(text, want);
    Ok(())
}
