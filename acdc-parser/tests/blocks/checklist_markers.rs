//! Asciidoctor marks an unordered item as a checklist item only when its text
//! starts with `[ ] `, `[x] ` or `[*] ` (`Parser.parse_list_item`); `[X]` stays
//! text. Draft keeps its own, wider set.

use acdc_parser::{
    CompatibilityMode, ListItemCheckedStatus, Options,
    ast::{BlockKind, BlockRef, InlineKind},
    parse,
};

type Error = Box<dyn std::error::Error>;

fn first_item(source: &str, mode: CompatibilityMode) -> Result<(Option<ListItemCheckedStatus>, String), Error> {
    let parsed = parse(source, &Options::builder().with_compatibility_mode(mode).build()?)?;
    let Some(BlockKind::UnorderedList(list)) = parsed.document().blocks().next().map(BlockRef::kind) else {
        return Err("expected an unordered list".into());
    };
    let item = list.items().next().ok_or("item")?;
    let text = item
        .principal()
        .inlines()
        .filter_map(|node| match node.kind() {
            InlineKind::Plain(text) => Some(text.text().to_string()),
            _ => None,
        })
        .collect();
    Ok((item.checked(), text))
}

#[rstest::rstest]
#[case("* [ ] Open\n", Some(ListItemCheckedStatus::Unchecked), "Open")]
#[case("* [x] Done\n", Some(ListItemCheckedStatus::Checked), "Done")]
#[case("* [*] Done\n", Some(ListItemCheckedStatus::Checked), "Done")]
#[case("* [X] Done\n", None, "[X] Done")]
fn asciidoctor_checkbox_markers(
    #[case] source: &str,
    #[case] checked: Option<ListItemCheckedStatus>,
    #[case] text: &str,
) -> Result<(), Error> {
    assert_eq!(first_item(source, CompatibilityMode::Asciidoctor)?, (checked, text.to_string()));
    Ok(())
}

#[test]
fn draft_keeps_the_uppercase_marker() -> Result<(), Error> {
    assert_eq!(
        first_item("* [X] Done\n", CompatibilityMode::Draft)?,
        (Some(ListItemCheckedStatus::Checked), "Done".to_string())
    );
    Ok(())
}
