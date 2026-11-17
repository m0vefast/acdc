//! asciidoctor converts a heading's title while parsing, to generate its ID, so a
//! reference in it reads its target's text only when that target is already registered:
//! one to the heading itself or to a later target draws `[id]` (asciidoctor 2.0.26,
//! measured 2026-11-15). Other references resolve against the whole document.

use acdc_parser::{
    Options,
    ast::{ContentFieldRef, InlineKind},
    parse,
};

type Error = Box<dyn std::error::Error>;

/// Each automatic reference's target and whether it precedes its target.
fn references(source: &str) -> Result<Vec<(String, bool)>, Error> {
    let parsed = parse(source, &Options::default())?;
    let mut found = Vec::new();
    for content in parsed.document().content_occurrences().map(ContentFieldRef::content) {
        for inline in content.inlines() {
            if let InlineKind::CrossReference(xref) = inline.kind()
                && xref.text().is_empty()
            {
                found.push((xref.target().to_string(), xref.precedes_target()));
            }
        }
    }
    Ok(found)
}

#[rstest::rstest]
#[case::its_own_heading("[[r]]\n== See <<r>> again\n", vec![("r", true)])]
#[case::a_later_section("[[a]]\n== A see <<b>>\n\n[[b]]\n== Bee\n", vec![("b", true)])]
#[case::an_earlier_block("[[t]]\n.Target\n====\nx\n====\n\n== Ref <<t>>\n", vec![("t", false)])]
#[case::a_discrete_heading_to_a_later_one("[discrete]\n== See <<b>>\n\n[[b]]\n== Bee\n", vec![("b", true)])]
#[case::a_paragraph_to_a_later_section("Go <<b>>.\n\n[[b]]\n== Bee\n", vec![("b", false)])]
fn a_heading_reference_precedes_an_unregistered_target(
    #[case] source: &str,
    #[case] expected: Vec<(&str, bool)>,
) -> Result<(), Error> {
    let expected: Vec<(String, bool)> = expected.into_iter().map(|(t, p)| (t.to_string(), p)).collect();
    assert_eq!(references(source)?, expected);
    Ok(())
}
