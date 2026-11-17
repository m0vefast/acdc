//! The table format decides how the body is read; `separator` only names the
//! delimiter (asciidoctor 2.0.26 `Table::ParserContext#initialize`). Expected
//! cells are asciidoctor's own `rows.body` for each source.

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

fn rows(source: &str) -> Result<Vec<Vec<(String, usize)>>, Error> {
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
        .header()
        .into_iter()
        .chain(table.rows())
        .map(|row| row.cells().map(|cell| (cell_text(cell), cell.colspan())).collect())
        .collect())
}

fn plain(rows: &[&[&str]]) -> Vec<Vec<(String, usize)>> {
    rows.iter()
        .map(|row| row.iter().map(|text| ((*text).to_string(), 1)).collect())
        .collect()
}

#[rstest::rstest]
#[case::psv_with_cols("[cols=\"1,1\",separator=:]\n|===\n:a :b\n:c :d\n|===\n")]
#[case::psv_cells_flow_across_lines("[cols=\"1,1\",separator=:]\n|===\n:a\n:b :c\n:d\n|===\n")]
#[case::psv_without_cols("[separator=:]\n|===\n:a :b\n:c :d\n|===\n")]
#[case::dsv_by_attribute("[format=dsv]\n|===\na:b\nc:d\n|===\n")]
#[case::dsv_by_delimiter(":===\na:b\nc:d\n:===\n")]
#[case::dsv_with_separator("[format=dsv,separator=;]\n|===\na;b\nc;d\n|===\n")]
fn a_custom_separator_keeps_the_format(#[case] source: &str) -> Result<(), Error> {
    assert_eq!(rows(source)?, plain(&[&["a", "b"], &["c", "d"]]));
    Ok(())
}

#[test]
fn a_custom_psv_separator_reads_cell_specifiers() -> Result<(), Error> {
    let mut expected = plain(&[&["span"], &["c", "d"]]);
    expected[0][0].1 = 2;
    assert_eq!(rows("[cols=\"1,1\",separator=;]\n|===\n2+;span\n;c ;d\n|===\n")?, expected);
    Ok(())
}

#[test]
fn a_custom_csv_separator_keeps_csv_quoting() -> Result<(), Error> {
    assert_eq!(
        rows("[format=csv,separator=;]\n|===\na;\"b;x\"\nc;d\n|===\n")?,
        plain(&[&["a", "b;x"], &["c", "d"]])
    );
    Ok(())
}
