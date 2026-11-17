//! A cell's `\|` escape collapses to `|` in its value; every location after it
//! still names the source bytes it is drawn from.

use acdc_parser::{
    Options, SafeMode,
    ast::{BlockKind, BlockRef, ContentRef, InlineKind, LocationRef},
    parse,
};

type Error = Box<dyn std::error::Error>;

fn options() -> Result<Options<'static>, Error> {
    Ok(Options::builder().with_safe_mode(SafeMode::Unsafe).build()?)
}

fn table_paragraphs<'doc>(
    parsed: &'doc acdc_parser::ParseResult,
) -> Result<Vec<acdc_parser::ast::ParagraphRef<'doc>>, Error> {
    let Some(BlockKind::Table(table)) = parsed.document().blocks().next().map(BlockRef::kind)
    else {
        return Err("expected a table".into());
    };
    Ok(table
        .rows()
        .flat_map(|row| row.cells())
        .flat_map(|cell| cell.blocks())
        .filter_map(|block| match block.kind() {
            BlockKind::Paragraph(p) => Some(p),
            _ => None,
        })
        .collect())
}

fn leaves<'doc>(content: ContentRef<'doc>, out: &mut Vec<(String, LocationRef<'doc>)>) {
    for node in content.inlines() {
        match node.kind() {
            InlineKind::Plain(t) => out.push((t.text().to_string(), node.location())),
            InlineKind::Raw(r) => out.push((r.text().to_string(), node.location())),
            InlineKind::Monospace(m) => leaves(m.content(), out),
            _ => {}
        }
    }
}

/// `|chain `+a\|+` tail `+h\|+` end`: every location past the first collapsed
/// `\` carries a +1 shift, past the second a +2 shift. Ends are inclusive.
#[test]
fn escaped_pipe_collapse_does_not_drift_following_locations() -> Result<(), Error> {
    let doc = "|===\n|chain `+a\\|+` tail `+h\\|+` end\n|===\n";
    let parsed = parse(doc, &options()?)?;
    let mut got = Vec::new();
    for paragraph in table_paragraphs(&parsed)? {
        leaves(paragraph.content(), &mut got);
    }
    let got: Vec<(String, u32, u32, usize, usize)> = got
        .into_iter()
        .map(|(text, l)| {
            (
                text,
                l.start().column(),
                l.end().column(),
                l.absolute_start(),
                l.absolute_end(),
            )
        })
        .collect();
    let want: Vec<(String, u32, u32, usize, usize)> = [
        ("chain ", 2, 7, 6, 11),
        ("a|", 10, 12, 14, 16),
        (" tail ", 15, 20, 19, 24),
        ("h|", 23, 25, 27, 29),
        (" end", 28, 31, 32, 35),
    ]
    .into_iter()
    .map(|(t, a, b, c, d)| (t.to_string(), a, b, c, d))
    .collect();
    assert_eq!(got, want);
    Ok(())
}

/// Line and column (1-based, in characters) of byte `at` in `src`.
fn position_of(src: &str, at: usize) -> (u32, u32) {
    let before = src.get(..at).unwrap_or(src);
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (
        u32::try_from(line).unwrap_or(u32::MAX),
        u32::try_from(col).unwrap_or(u32::MAX),
    )
}

/// After a collapsed escape, a cell's line and column are where its bytes
/// stand, on the escape's line and on the lines after it.
#[test]
fn escaped_cell_line_and_column_are_where_their_bytes_stand() -> Result<(), Error> {
    let options = options()?;
    let mut failures = Vec::new();
    for doc in [
        "|===\n|a\\|b\ncd\n|===\n",
        "|===\n|a\\|\\|b\nc\n|===\n",
        "|===\n|中\\|b `x` e\n|===\n",
    ] {
        let parsed = parse(doc, &options)?;
        let mut locations = Vec::new();
        for paragraph in table_paragraphs(&parsed)? {
            locations.push(("paragraph".to_string(), paragraph.location()));
            for node in paragraph.content().inlines() {
                locations.push((format!("{:?}", node.kind()).chars().take(24).collect(), node.location()));
            }
        }
        for (name, l) in locations {
            let got = (
                (l.start().line(), l.start().column()),
                (l.end().line(), l.end().column()),
            );
            let want = (
                position_of(doc, l.absolute_start()),
                position_of(doc, l.absolute_end()),
            );
            if got != want {
                failures.push(format!(
                    "{doc:?} {name} at {}..{}: got {got:?}, want {want:?}",
                    l.absolute_start(),
                    l.absolute_end()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}
