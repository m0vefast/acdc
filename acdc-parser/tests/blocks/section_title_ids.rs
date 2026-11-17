//! A generated section ID is made from the text asciidoctor's converted title leaves
//! (section.rb `generate_id`): a formula leaves its own text, as asciidoctor 2.0.26 draws
//! each title here (measured 2026-11-15).

use acdc_parser::{
    Options,
    ast::{BlockKind, StructuralRef},
    parse,
};

type Error = Box<dyn std::error::Error>;

fn heading_ids(source: &str) -> Result<Vec<String>, Error> {
    let parsed = parse(source, &Options::default())?;
    Ok(StructuralRef::Document(parsed.document())
        .descendants()
        .filter_map(|node| match node {
            StructuralRef::Block(block) => match block.kind() {
                BlockKind::Section(s) => s.id().map(|id| id.as_str().to_string()),
                BlockKind::DiscreteHeading(h) => h.id().map(|id| id.as_str().to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect())
}

#[rstest::rstest]
#[case::latexmath("== latexmath:[\\alpha] x\n", "_alpha_x")]
#[case::stem("== stem:[x^2] end\n", "_x2_end")]
#[case::a_formula_with_a_less_than("== a stem:[a<b] c\n", "_a_ab_c")]
#[case::asciimath_with_punctuation("== asciimath:[a-b.c] d\n", "_a_b_c_d")]
#[case::formulas_a_link_and_a_footnote(
    "[discrete]\n=== latexmath:[\\alpha] https://a.com[alpha] footnote:[Roadmap] stem:[x^2]\n",
    "_alpha_alpha_1_x2"
)]
fn a_formula_in_a_title_leaves_its_text_in_the_id(#[case] source: &str, #[case] expected: &str) -> Result<(), Error> {
    assert_eq!(heading_ids(source)?, vec![expected.to_string()]);
    Ok(())
}
