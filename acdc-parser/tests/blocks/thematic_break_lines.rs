//! A thematic break is a whole line (`ExtLayoutBreakRx`,
//! `MarkdownThematicBreakRx`). Expected kinds were produced by Asciidoctor 2.0.26.

use acdc_parser::{Options, ast::BlockKind, parse};

type Error = Box<dyn std::error::Error>;

#[rstest::rstest]
#[case("'''\n", true)]
#[case("''''\n", true)]
#[case("'''  \n", true)]
#[case("---\n", true)]
#[case("- - -\n", true)]
#[case("***\n", true)]
#[case("* * *  \n", true)]
#[case("___\n", true)]
#[case("_ _ _\n", true)]
#[case("_  _  _\n", true)]
#[case("  ___\n", true)]
#[case("*** a\n", false)]
#[case("- -  -\n", false)]
#[case("'''x\n", false)]
#[case("****\n", false)]
fn a_thematic_break_is_the_whole_line(#[case] source: &str, #[case] is_break: bool) -> Result<(), Error> {
    let parsed = parse(source, &Options::default())?;
    let first = parsed.document().blocks().next().ok_or("a block")?;
    assert_eq!(matches!(first.kind(), BlockKind::ThematicBreak(_)), is_break, "{source:?}");
    Ok(())
}
