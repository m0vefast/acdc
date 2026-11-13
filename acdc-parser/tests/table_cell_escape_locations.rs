//! Table-cell `\|` escape collapse must not drift inline locations.
//!
//! `split_escaped` hands the cell parser the UNESCAPED content (`\|` → `|`)
//! anchored at the cell's ORIGINAL offset, so every location after a collapsed
//! escape used to come back short by one per escape — the cursor in a cell
//! full of `` `+x\|+` `` snippets clamped at the end of every plain run
//! (completion L850 monotonic-x, L371 char-tight). The post-parse compensation
//! in `parse_table_cell` re-aligns the collapsed content against the original
//! source and shifts every location by the escapes before it.

use acdc_parser::{Block, DelimitedBlockType, InlineNode, Options, SafeMode};

fn inline_texts(doc: &str) -> Vec<(String, u32, u32, usize, usize)> {
    let options = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let parsed = acdc_parser::parse(doc, &options).expect("parse");
    let document = parsed.document();
    let Some(Block::DelimitedBlock(delimited)) = document.blocks.first() else {
        panic!("expected a table, got {:?}", document.blocks.first());
    };
    let DelimitedBlockType::DelimitedTable(table) = &delimited.inner else {
        panic!("expected a table inner");
    };
    let mut out = Vec::new();
    for row in &table.rows {
        for column in &row.columns {
            for block in &column.content {
                let Block::Paragraph(p) = block else { continue };
                for inline in &p.content {
                    let (value, loc) = match inline {
                        InlineNode::PlainText(t) => (t.content.to_string(), &t.location),
                        InlineNode::RawText(r) => (r.content.to_string(), &r.location),
                        InlineNode::MonospaceText(m) => {
                            for kid in &m.content {
                                match kid {
                                    InlineNode::PlainText(t) => out.push((
                                        t.content.to_string(),
                                        t.location.start.column,
                                        t.location.end.column,
                                        t.location.absolute_start,
                                        t.location.absolute_end,
                                    )),
                                    InlineNode::RawText(r) => out.push((
                                        r.content.to_string(),
                                        r.location.start.column,
                                        r.location.end.column,
                                        r.location.absolute_start,
                                        r.location.absolute_end,
                                    )),
                                    _ => {}
                                }
                            }
                            continue;
                        }
                        _ => continue,
                    };
                    out.push((
                        value,
                        loc.start.column,
                        loc.end.column,
                        loc.absolute_start,
                        loc.absolute_end,
                    ));
                }
            }
        }
    }
    out
}

/// Line 2 of the doc, 1-based columns:
/// `|chain `+a\|+` tail `+h\|+` end`
///  1 2-6   8-14   15-20 21-27  28-31
/// The second passthrough's content `h` sits at col 23 (abs 27); every
/// location past the FIRST collapsed `\` (col 11) must carry a +1 shift,
/// and past the second (col 24) a +2 shift.
#[test]
fn escaped_pipe_collapse_does_not_drift_following_locations() {
    let doc = "|===\n|chain `+a\\|+` tail `+h\\|+` end\n|===\n";
    let texts = inline_texts(doc);
    let find = |needle: &str| {
        texts
            .iter()
            .find(|(v, ..)| v == needle)
            .unwrap_or_else(|| panic!("no inline with value {needle:?} in {texts:?}"))
    };

    // Every inline end is INCLUSIVE (its last char), passthrough RawText too.

    // Before any escape: exact.
    let chain = find("chain ");
    assert_eq!((chain.1, chain.3), (2, 6), "prefix text start: {chain:?}");

    // First pass `a\|` → value "a|": content spans source cols 10..12 (`a\|`,
    // the collapsed `\` inside), its last char `|` at col 12 / abs 16.
    let a = find("a|");
    assert_eq!((a.1, a.2), (10, 12), "first pass content cols: {a:?}");
    assert_eq!((a.3, a.4), (5 + 9, 5 + 11), "first pass content abs: {a:?}");

    // Plain run between the two passes: one escape collapsed before it.
    let tail = find(" tail ");
    assert_eq!((tail.1, tail.2), (15, 20), "inter-pass text cols: {tail:?}");
    assert_eq!((tail.3, tail.4), (5 + 14, 5 + 19), "inter-pass text abs: {tail:?}");

    // Second pass `h\|` → source cols 23..25, +1 shift from the first escape.
    let h = find("h|");
    assert_eq!((h.1, h.2), (23, 25), "second pass content cols: {h:?}");
    assert_eq!((h.3, h.4), (5 + 22, 5 + 24), "second pass content abs: {h:?}");

    // Trailing text: +2 (both escapes collapsed before it).
    let end = find(" end");
    assert_eq!((end.1, end.2), (28, 31), "trailing text cols: {end:?}");
    assert_eq!((end.3, end.4), (5 + 27, 5 + 30), "trailing text abs: {end:?}");
}

/// Line and column (1-based, in characters) of byte `at` in `src`, counted from the source.
fn position_of(src: &str, at: usize) -> (u32, u32) {
    let before = &src[..at];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (
        u32::try_from(line).unwrap_or(u32::MAX),
        u32::try_from(col).unwrap_or(u32::MAX),
    )
}

/// After a collapsed escape, a cell's line and column are where its bytes stand, on the
/// escape's line and on the lines after it.
#[test]
fn escaped_cell_line_and_column_are_where_their_bytes_stand() {
    let options = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let mut failures = Vec::new();
    for doc in [
        "|===\n|a\\|b\ncd\n|===\n",
        "|===\n|a\\|\\|b\nc\n|===\n",
        "|===\n|中\\|b `x` e\n|===\n",
    ] {
        let parsed = acdc_parser::parse(doc, &options).expect("parse");
        let Some(Block::DelimitedBlock(delimited)) = parsed.document().blocks.first() else {
            panic!("{doc:?}: expected a table");
        };
        let DelimitedBlockType::DelimitedTable(table) = &delimited.inner else {
            panic!("{doc:?}: expected a table inner");
        };
        let mut locs: Vec<(String, acdc_parser::Location)> = Vec::new();
        for row in &table.rows {
            for column in &row.columns {
                for block in &column.content {
                    let Block::Paragraph(p) = block else { continue };
                    locs.push(("paragraph".into(), p.location.clone()));
                    for inline in &p.content {
                        locs.push((
                            format!("{inline:?}").chars().take(24).collect(),
                            inline.location().clone(),
                        ));
                    }
                }
            }
        }
        for (name, l) in locs {
            let got = ((l.start.line, l.start.column), (l.end.line, l.end.column));
            let want = (
                position_of(doc, l.absolute_start),
                position_of(doc, l.absolute_end),
            );
            if got != want {
                failures.push(format!(
                    "{doc:?} {name} at {}..{}: got {got:?}, want {want:?}",
                    l.absolute_start, l.absolute_end
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
