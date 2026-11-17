//! Lists nest by marker identity, as Asciidoctor reads them
//! (`Parser.read_lines_for_list_item`, `is_sibling_list_item?`): an item
//! ends at a sibling of its own list or of any enclosing list, and any other
//! list item line nests. Expected shapes were produced by Asciidoctor 2.0.26.

use acdc_parser::{
    Options,
    ast::{BlockKind, BlockRef, ListItemRef},
    parse,
};

type Error = Box<dyn std::error::Error>;

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn source_of(source: &str, start: usize, end: usize) -> &str {
    source.get(start..=end).unwrap_or_default()
}

fn item_shape(source: &str, item: ListItemRef<'_>) -> String {
    let inlines: Vec<_> = item.principal().inlines().collect();
    let text = match (inlines.first(), inlines.last()) {
        (Some(first), Some(last)) => collapse(source_of(
            source,
            first.location().absolute_start(),
            last.location().absolute_end(),
        )),
        _ => String::new(),
    };
    let nested = blocks_shape(source, item.blocks());
    if nested.is_empty() { text } else { format!("{text} {nested}") }
}

fn blocks_shape<'a>(source: &str, blocks: impl Iterator<Item = BlockRef<'a>>) -> String {
    blocks
        .filter_map(|block| match block.kind() {
            BlockKind::UnorderedList(list) => Some(format!(
                "ul[{}]",
                list.items().map(|item| item_shape(source, item)).collect::<Vec<_>>().join(",")
            )),
            BlockKind::OrderedList(list) => Some(format!(
                "ol[{}]",
                list.items().map(|item| item_shape(source, item)).collect::<Vec<_>>().join(",")
            )),
            BlockKind::Paragraph(_) => {
                let location = block.location();
                Some(format!(
                    "p({})",
                    collapse(source_of(source, location.absolute_start(), location.absolute_end()))
                ))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[rstest::rstest]
#[case(". a\n.. b\n. c\n", "ol[a ol[b],c]")]
#[case("... a\n. b\n", "ol[a ol[b]]")]
#[case(".. a\n. b\n.. c\n", "ol[a ol[b],c]")]
#[case(". a\n1. b\n", "ol[a ol[b]]")]
#[case("1. a\n2. b\n. c\n", "ol[a,b ol[c]]")]
#[case("* a\n*** b\n** c\n", "ul[a ul[b ul[c]]]")]
#[case("*** a\n* b\n", "ul[a ul[b]]")]
#[case(". s\na. x\nb. y\n. t\n", "ol[s ol[x,y],t]")]
#[case("a. x\nb. y\n", "ol[x,y]")]
#[case("i) x\nii) y\n", "ol[x,y]")]
#[case("I) x\nA. y\n", "ol[x ol[y]]")]
#[case("* a\n. b\n* c\n", "ul[a ol[b],c]")]
#[case(". a\n* b\n. c\n", "ol[a ul[b],c]")]
#[case("- a\n* b\n- c\n", "ul[a ul[b],c]")]
#[case(". a\n\n.. b\n\n. c\n", "ol[a ol[b],c]")]
#[case(". a\n+\n.. b\n", "ol[a ol[b]]")]
#[case("I. Intro\n", "ol[Intro]")]
#[case("x. y\n", "ol[y]")]
#[case("* a\n** b\n* c\n** d\n", "ul[a ul[b],c ul[d]]")]
#[case(". a\n.. b\n... c\n.. d\n. e\n", "ol[a ol[b ol[c],d],e]")]
#[case("* a\n\n** b\n\n* c\n", "ul[a ul[b],c]")]
#[case("1. a\n2. b\n3. c\n", "ol[a,b,c]")]
#[case("a. x\nb. y\n\n. z\n", "ol[x,y ol[z]]")]
#[case(". a\n\n\n* b\n", "ol[a ul[b]]")]
#[case("* a\n- b\n* c\n- d\n", "ul[a ul[b],c ul[d]]")]
#[case("iv) x\nv) y\n", "ol[x,y]")]
#[case("* a\n. b\na. c\n. d\n* e\n", "ul[a ol[b ol[c],d],e]")]
#[case(". a\n.. b\n+\npara\n. c\n", "ol[a ol[b p(para)],c]")]
#[case(". a\n.. b\n\n+\npara\n. c\n", "ol[a ol[b] p(para),c]")]
#[case("* a\n** b\n\n*** c\n\n* d\n", "ul[a ul[b ul[c]],d]")]
#[case("A. one\nB. two\n\nC. three\n", "ol[one,two,three]")]
#[case("1. a\n\n1. b\n", "ol[a,b]")]
#[case(". a\n\n// sep\n\n. b\n", "ol[a] ol[b]")]
#[case("* a\n\n[]\n* b\n", "ul[a] ul[b]")]
#[case("* a\n\npara\n", "ul[a] p(para)")]
#[case("* a\npara\n* b\n", "ul[a para,b]")]
#[case(". a\nb. x\n", "ol[a ol[x]]")]
#[case("** a\n* b\n** c\n* d\n", "ul[a ul[b],c ul[d]]")]
fn lists_nest_as_asciidoctor_reads_them(#[case] source: &str, #[case] expected: &str) -> Result<(), Error> {
    let parsed = parse(source, &Options::default())?;
    assert_eq!(blocks_shape(source, parsed.document().blocks()), expected, "{source:?}");
    Ok(())
}

#[test]
fn a_list_whose_item_fails_to_parse_still_ends() {
    for source in ["* https://e.com:a\n", "a:: https://e.com:a\n"] {
        let _ = parse(source, &Options::default());
    }
}
