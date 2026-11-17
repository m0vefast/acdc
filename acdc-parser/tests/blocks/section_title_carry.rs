//! A block title written before a section heading belongs to the next content block
//! (asciidoctor parser.rb `next_section`), as asciidoctor 2.0.26 draws each source here.
use acdc_parser::{
    Options,
    ast::{BlockKind, ContentRef, InlineKind, StructuralRef},
    parse,
};

type Error = Box<dyn std::error::Error>;

fn plain_text(content: ContentRef<'_>) -> String {
    content
        .inlines()
        .filter_map(|inline| if let InlineKind::Plain(text) = inline.kind() { Some(text.text()) } else { None })
        .collect()
}

/// Each paragraph's title, in document order.
fn paragraph_titles(source: &str) -> Result<Vec<Option<String>>, Error> {
    let parsed = parse(source, &Options::default())?;
    Ok(StructuralRef::Document(parsed.document())
        .descendants()
        .filter_map(|node| match node {
            StructuralRef::Block(block) => match block.kind() {
                BlockKind::Paragraph(paragraph) => Some(paragraph.title().map(plain_text)),
                _ => None,
            },
            _ => None,
        })
        .collect())
}

#[rstest::rstest]
#[case::past_a_blank_line_and_two_headings(".A title\n\n== Pricing\n\n=== beta\n\npara\n", vec![Some("A title")])]
#[case::with_an_anchor_on_the_section("[#x]\n.T\n\n== S\n\npara\n", vec![Some("T")])]
#[case::a_title_of_its_own_wins(".A\n== S\n\n.B\npara\n", vec![Some("B")])]
#[case::past_an_attribute_entry(".A\n== S\n\n:x: y\n\npara\n", vec![Some("A")])]
#[case::past_an_empty_section(".A\n== S\n\n== T\n\npara\n", vec![Some("A")])]
#[case::only_the_next_block(".A\n== S\n\none\n\ntwo\n", vec![Some("A"), None])]
fn a_title_before_a_heading_goes_to_the_next_content_block(
    #[case] source: &str,
    #[case] expected: Vec<Option<&str>>,
) -> Result<(), Error> {
    let titles = paragraph_titles(source)?;
    assert_eq!(titles, expected.into_iter().map(|t| t.map(str::to_string)).collect::<Vec<_>>());
    Ok(())
}

/// A discrete heading's dot-title is read and dropped: the heading stays discrete and the
/// next block takes nothing (asciidoctor 2.0.26, measured 2026-11-15).
#[rstest::rstest]
#[case::title_then_style(".T\n[discrete]\n=== H\n\nafter\n")]
#[case::title_anchor_style(".T\n[#a3]\n[discrete]\n=== H\n\nafter\n")]
fn a_titled_discrete_heading_stays_discrete(#[case] source: &str) -> Result<(), Error> {
    let parsed = parse(source, &Options::default())?;
    let kinds: Vec<&str> = parsed
        .document()
        .blocks()
        .map(|block| match block.kind() {
            BlockKind::DiscreteHeading(_) => "discrete",
            BlockKind::Section(_) => "section",
            BlockKind::Paragraph(_) => "paragraph",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["discrete", "paragraph"]);
    assert_eq!(paragraph_titles(source)?, vec![None]);
    Ok(())
}
