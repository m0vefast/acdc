//! Description list items hold the lines Asciidoctor's reader gives them
//! (`Parser.parse_description_list`, `read_lines_for_list_item` with
//! `list_type == :dlist`): a term without text takes the next text even past
//! blank lines, block attribute lines stay only before a nested list, and a
//! term without a definition joins the next term. Expected shapes were
//! produced by Asciidoctor 2.0.26.

use acdc_parser::{
    Options,
    ast::{BlockKind, BlockRef, ContentRef, DescriptionListItemRef, ListItemRef},
    parse,
};

type Error = Box<dyn std::error::Error>;

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn source_of(source: &str, start: usize, end: usize) -> &str {
    source.get(start..=end).unwrap_or_default()
}

fn content_text(source: &str, content: ContentRef<'_>) -> String {
    let inlines: Vec<_> = content.inlines().collect();
    match (inlines.first(), inlines.last()) {
        (Some(first), Some(last)) => collapse(source_of(
            source,
            first.location().absolute_start(),
            last.location().absolute_end(),
        )),
        _ => String::new(),
    }
}

fn with_blocks(text: String, blocks: String) -> String {
    match (text.is_empty(), blocks.is_empty()) {
        (_, true) => text,
        (true, false) => blocks,
        (false, false) => format!("{text} {blocks}"),
    }
}

fn list_item_shape(source: &str, item: ListItemRef<'_>) -> String {
    with_blocks(content_text(source, item.principal()), blocks_shape(source, item.blocks()))
}

fn description_item_shape(source: &str, item: DescriptionListItemRef<'_>) -> String {
    let terms = item.terms().map(|term| content_text(source, term.content())).collect::<Vec<_>>().join("|");
    let definition = item.definition().map_or_else(
        || "nil".to_string(),
        |definition| {
            let text = definition.principal().map(|principal| content_text(source, principal)).unwrap_or_default();
            format!("({})", with_blocks(text, blocks_shape(source, definition.blocks())))
        },
    );
    format!("{terms}=>{definition}")
}

fn blocks_shape<'a>(source: &str, blocks: impl Iterator<Item = BlockRef<'a>>) -> String {
    blocks
        .map(|block| match block.kind() {
            BlockKind::DescriptionList(list) => format!(
                "dl[{}]",
                list.items().map(|item| description_item_shape(source, item)).collect::<Vec<_>>().join("; ")
            ),
            BlockKind::UnorderedList(list) => format!(
                "ul[{}]",
                list.items().map(|item| list_item_shape(source, item)).collect::<Vec<_>>().join("; ")
            ),
            BlockKind::OrderedList(list) => format!(
                "ol[{}]",
                list.items().map(|item| list_item_shape(source, item)).collect::<Vec<_>>().join("; ")
            ),
            BlockKind::Paragraph(_) => {
                let location = block.location();
                format!("p({})", collapse(source_of(source, location.absolute_start(), location.absolute_end())))
            }
            BlockKind::Listing(_) => "listing".to_string(),
            _ => "other".to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[rstest::rstest]
#[case("a:: one\nb::\nc:: three\n", "dl[a=>(one); b|c=>(three)]")]
#[case("a::\n\ntext\nb:: two\n", "dl[a=>(text); b=>(two)]")]
#[case("a::\n\n  lit\n  more\nb:: two\n", "dl[a=>(lit more); b=>(two)]")]
#[case("a:: one\nmore\n\nb:: two\n", "dl[a=>(one more); b=>(two)]")]
#[case("a:: one\n\npara\n", "dl[a=>(one)] p(para)")]
#[case("a::\n* x\n* y\nb:: two\n", "dl[a=>(ul[x; y]); b=>(two)]")]
#[case("a:: one\n[square]\n* x\nb:: two\n", "dl[a=>(one ul[x]); b=>(two)]")]
#[case("a:: one\n[source]\n----\nx\n----\n", "dl[a=>(one)] listing")]
#[case("a:: one\n+\n----\nx\n----\nb:: two\n", "dl[a=>(one listing); b=>(two)]")]
#[case("a:: one\nb::: nested\nc::: n2\nd:: two\n", "dl[a=>(one dl[b=>(nested); c=>(n2)]); d=>(two)]")]
#[case("a:: one\n\nb::: nested\n\nc:: two\n", "dl[a=>(one dl[b=>(nested)]); c=>(two)]")]
#[case("a:: one\n+\npara\n\n+\nmore\nb:: two\n", "dl[a=>(one p(para) p(more)); b=>(two)]")]
#[case("a::\nb::\n\nc:: three\n", "dl[a|b|c=>(three)]")]
#[case("a:: one\n  indented\nb:: two\n", "dl[a=>(one indented); b=>(two)]")]
#[case("a::\n  lit\n  more\n", "dl[a=>(lit more)]")]
#[case("a::\n+\n----\nx\n----\n", "dl[a=>(listing)]")]
#[case("a:: one\n\n[square]\n* x\n", "dl[a=>(one ul[x])]")]
#[case("a:: one\n\n.Title\n* x\n", "dl[a=>(one)] ul[x]")]
#[case("a:: one\n\n  b:: two\n", "dl[a=>(one); b=>(two)]")]
#[case("a:: one\n// c\nb:: two\n", "dl[a=>(one); b=>(two)]")]
#[case("a;; one\nb:: two\nc;; three\n", "dl[a=>(one dl[b=>(two)]); c=>(three)]")]
#[case("a:: one\n* x\nb::: y\nc:: two\n", "dl[a=>(one ul[x dl[b=>(y)]]); c=>(two)]")]
#[case("a::\n\nb:: two\n", "dl[a|b=>(two)]")]
#[case("a:: one\n\n\n+\nmore\n", "dl[a=>(one p(more))]")]
fn description_lists_read_as_asciidoctor_reads_them(#[case] source: &str, #[case] expected: &str) -> Result<(), Error> {
    let parsed = parse(source, &Options::default())?;
    assert_eq!(blocks_shape(source, parsed.document().blocks()), expected, "{source:?}");
    Ok(())
}

/// asciidoctor 2.0.26 (measured 2026-11-16): a term's leading anchor without its own label
/// takes the rest of the term (`catalog_inline_anchor $1, ($2 || $'.lstrip)`): `<<first-term>>`
/// draws `First`, `<<second>>` draws `<strong>Bold</strong> term`.
#[test]
fn leading_term_anchor_takes_the_term_as_its_label() -> Result<(), Box<dyn std::error::Error>> {
    let parsed = parse(
        "[[first-term]]First:: Def.\n[[second]] *Bold* term:: Def.\n[[own,Own]]Other:: Def.\n",
        &Options::default(),
    )?;
    fn text(content: ContentRef<'_>) -> String {
        content
            .inlines()
            .map(|node| match node.kind() {
                acdc_parser::ast::InlineKind::Plain(plain) => plain.text().to_string(),
                acdc_parser::ast::InlineKind::Bold(bold) => format!("<b>{}</b>", text(bold.content())),
                _ => "?".to_string(),
            })
            .collect()
    }
    let label = |id: &str| parsed.document().references().get(id).and_then(|target| target.xreflabel()).map(text);
    assert_eq!(label("first-term").as_deref(), Some("First"));
    assert_eq!(label("second").as_deref(), Some("<b>Bold</b> term"));
    assert_eq!(label("own").as_deref(), Some("Own"));
    Ok(())
}
