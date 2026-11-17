//! A delimiter line ends a list item unless a list continuation attaches it
//! (`Parser.read_lines_for_list_item`: "a delimited block immediately breaks
//! the list unless preceded by a list continuation"). Every expectation is
//! Asciidoctor 2.0.26's output for the same source.

use acdc_parser::{
    CompatibilityMode, Options,
    ast::{BlockKind, BlockRef, InlineKind},
    parse,
};

type Error = Box<dyn std::error::Error>;

fn top_level(source: &str, mode: CompatibilityMode) -> Result<(Vec<&'static str>, String), Error> {
    let parsed = parse(source, &Options::builder().with_compatibility_mode(mode).build()?)?;
    let mut kinds = Vec::new();
    let mut principal = String::new();
    for block in parsed.document().blocks() {
        kinds.push(match block.kind() {
            BlockKind::UnorderedList(list) => {
                if let Some(item) = list.items().next() {
                    principal = item
                        .principal()
                        .inlines()
                        .filter_map(|node| match node.kind() {
                            InlineKind::Plain(text) => Some(text.text().to_string()),
                            _ => None,
                        })
                        .collect();
                }
                "list"
            }
            BlockKind::Paragraph(_) => "paragraph",
            BlockKind::Comment(_) => "comment",
            _ => "delimited",
        });
    }
    Ok((kinds, principal))
}

#[rstest::rstest]
#[case("====\nx\n====")]
#[case("=====\nx\n=====")]
#[case("----\nx\n----")]
#[case("....\nx\n....")]
#[case("****\nx\n****")]
#[case("____\nx\n____")]
#[case("++++\nx\n++++")]
#[case("--\nx\n--")]
#[case("|===\n|x\n|===")]
#[case("```\nx\n```")]
fn a_delimiter_line_ends_the_item(
    #[case] block: &str,
    #[values(CompatibilityMode::Asciidoctor, CompatibilityMode::Draft)] mode: CompatibilityMode,
) -> Result<(), Error> {
    let (kinds, principal) = top_level(&format!("* alpha\n{block}\n"), mode)?;
    assert_eq!(kinds, ["list", "delimited"]);
    assert_eq!(principal, "alpha");
    Ok(())
}

#[test]
fn a_comment_delimiter_ends_the_item() -> Result<(), Error> {
    let (kinds, principal) = top_level("* alpha\n////\nx\n////\n", CompatibilityMode::Asciidoctor)?;
    assert_eq!(kinds.first(), Some(&"list"));
    assert_eq!(principal, "alpha");
    Ok(())
}

#[test]
fn a_delimiter_closing_an_enclosing_block_ends_the_item() -> Result<(), Error> {
    let parsed = parse("====\n* *alpha*\n====\n\nafter\n", &Options::default())?;
    let Some(BlockKind::Example(_)) = parsed.document().blocks().next().map(BlockRef::kind) else {
        return Err("expected the example block first".into());
    };
    assert_eq!(parsed.document().blocks().count(), 2);
    Ok(())
}

#[test]
fn a_continuation_still_attaches_the_block() -> Result<(), Error> {
    let parsed = parse("* alpha\n+\n====\nx\n====\n", &Options::default())?;
    let Some(BlockKind::UnorderedList(list)) = parsed.document().blocks().next().map(BlockRef::kind) else {
        return Err("expected a list".into());
    };
    assert_eq!(parsed.document().blocks().count(), 1);
    assert_eq!(list.items().next().ok_or("item")?.blocks().count(), 1);
    Ok(())
}

/// An enclosing block's blank last line is not part of the item's text.
#[rstest::rstest]
#[case("****\n* alpha\n\n****\n")]
#[case("****\n. alpha\n\n****\n")]
#[case("====\n* *alpha* t\n\n====\n")]
fn the_blank_end_of_an_enclosing_block_stays_out_of_the_item(#[case] source: &str) -> Result<(), Error> {
    let parsed = parse(source, &Options::default())?;
    let list = match parsed.document().blocks().next().map(BlockRef::kind) {
        Some(BlockKind::Sidebar(block)) => block.blocks().next(),
        Some(BlockKind::Example(block)) => block.blocks().next(),
        _ => None,
    }
    .ok_or("list")?;
    let item = match list.kind() {
        BlockKind::UnorderedList(list) => list.items().next(),
        BlockKind::OrderedList(list) => list.items().next(),
        _ => None,
    }
    .ok_or("item")?;
    let text: String = item
        .principal()
        .inlines()
        .filter_map(|node| match node.kind() {
            InlineKind::Plain(text) => Some(text.text().to_string()),
            _ => None,
        })
        .collect();
    assert!(!text.ends_with('\n'), "{text:?}");
    Ok(())
}
