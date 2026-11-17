//! A single lowercase letter after whitespace, before a cell delimiter, is the
//! next cell's style even when no style has that letter: it is consumed and
//! sets nothing (`CellSpecEndRx` ends in `([a-z])?$`; `Parser.parse_cellspec`
//! keeps the style only when `TableCellStyles` has the letter). Every expected
//! row is Asciidoctor 2.0.26's `rows.body`.

use acdc_parser::{
    Options,
    ast::{BlockKind, InlineKind, TableCellRef},
    parse,
};

type Error = Box<dyn std::error::Error>;

fn cell_text(cell: TableCellRef<'_>) -> String {
    cell.blocks()
        .filter_map(|block| match block.kind() {
            BlockKind::Paragraph(paragraph) => Some(
                paragraph
                    .content()
                    .inlines()
                    .filter_map(|inline| match inline.kind() {
                        InlineKind::Plain(text) => Some(text.text().to_string()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

fn body(source: &str) -> Result<Vec<Vec<(String, usize)>>, Error> {
    let parsed = parse(source, &Options::default())?;
    let table = parsed
        .document()
        .blocks()
        .find_map(|block| match block.kind() {
            BlockKind::Table(table) => Some(table),
            _ => None,
        })
        .ok_or("missing table")?;
    Ok(table
        .rows()
        .map(|row| row.cells().map(|cell| (cell_text(cell), cell.colspan())).collect())
        .collect())
}

#[rstest::rstest]
#[case("|===\n| n| | a\n|===\n", &[("", 1), ("", 1), ("a", 1)])]
#[case("|===\n|x n|y\n|===\n", &[("x", 1), ("y", 1)])]
#[case("|===\n|x 2+z|y |w\n|===\n", &[("x", 1), ("y", 2), ("w", 1)])]
#[case("[cols=2]\n|===\n|x nn|y\n|===\n", &[("x nn", 1), ("y", 1)])]
fn an_unknown_style_letter_is_consumed(
    #[case] source: &str,
    #[case] row: &[(&str, usize)],
) -> Result<(), Error> {
    let want = vec![row.iter().map(|(text, span)| ((*text).to_string(), *span)).collect::<Vec<_>>()];
    assert_eq!(body(source)?, want);
    Ok(())
}
