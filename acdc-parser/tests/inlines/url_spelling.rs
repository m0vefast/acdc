//! A URL is a link as written: Asciidoctor's link patterns match the
//! spelling, they do not validate it (2.0.26 draws `https://e.com:a` as
//! `<a href="https://e.com:a">`).

use acdc_parser::{
    Options,
    ast::{BlockKind, InlineKind, SourceKind},
    parse,
};

type Error = Box<dyn std::error::Error>;

#[rstest::rstest]
#[case("x https://e.com:a y\n", "https://e.com:a")]
#[case("x http://x:a: N\n", "http://x:a")]
#[case("x ftp://e:_ y\n", "ftp://e:_")]
#[case("* https://e.com:a\n", "https://e.com:a")]
fn a_url_that_does_not_parse_is_still_a_link(#[case] source: &str, #[case] url: &str) -> Result<(), Error> {
    let parsed = parse(source, &Options::default())?;
    let mut found = Vec::new();
    let mut visit = |inlines: Vec<InlineKind<'_>>| {
        for inline in inlines {
            if let InlineKind::Autolink(link) = inline
                && let SourceKind::Url(spelling) = link.target().kind()
            {
                found.push(spelling.to_string());
            }
        }
    };
    for block in parsed.document().blocks() {
        match block.kind() {
            BlockKind::Paragraph(paragraph) => visit(paragraph.content().inlines().map(|inline| inline.kind()).collect()),
            BlockKind::UnorderedList(list) => {
                for item in list.items() {
                    visit(item.principal().inlines().map(|inline| inline.kind()).collect());
                }
            }
            _ => {}
        }
    }
    assert_eq!(found, vec![url.to_string()], "{source:?}");
    Ok(())
}
