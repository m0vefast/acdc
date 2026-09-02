use crate::{ColumnStyle, Error, TableColumn, blocks::table::ParsedCell, model::SectionLevel};

use super::{
    ParserState, document_parser, inline_processing::adjust_and_log_parse_error, location_walk,
};

pub(crate) fn parse_table_cell<'a>(
    content: &'a str,
    state: &mut ParserState<'a>,
    cell_start_offset: usize,
    parent_section_level: Option<SectionLevel>,
    cell: &ParsedCell,
) -> Result<TableColumn<'a>, Error> {
    // Markdown blockquotes are only parsed when cell has AsciiDoc style ('a' prefix).
    // This matches asciidoctor behavior where `> text` is only a blockquote in 'a' style cells.
    let mut blocks = if cell.style == Some(ColumnStyle::AsciiDoc) {
        document_parser::blocks(content, state, cell_start_offset, parent_section_level)
    } else {
        document_parser::blocks_for_table_cell(
            content,
            state,
            cell_start_offset,
            parent_section_level,
        )
    }
    .unwrap_or_else(|error| {
        adjust_and_log_parse_error(
            &error,
            content,
            cell_start_offset,
            state,
            "Failed parsing table cell content as blocks",
        );
        Ok(Vec::new())
    })?;
    compensate_escape_collapses(&mut blocks, content, state, cell_start_offset);
    Ok(TableColumn::with_format(
        blocks,
        cell.colspan,
        cell.rowspan,
        cell.halign,
        cell.valign,
        cell.style,
    ))
}

/// Shift every location in the cell's blocks past a collapsed `\<sep>` escape.
///
/// `split_escaped` hands this parse the UNESCAPED content (`\|` → `|`) anchored
/// at the cell's ORIGINAL offset, so the grammar's positions undercount by one
/// byte per collapsed escape before them — every location after the first `\|`
/// in a cell came back short, cumulatively. Rather than threading an offset map
/// through the whole cell grammar, re-align the collapsed content against the
/// original source (the same post-parse-remap shape as `source_remap`) and add
/// the per-position escape count back.
fn compensate_escape_collapses(
    blocks: &mut [crate::Block<'_>],
    collapsed: &str,
    state: &ParserState<'_>,
    cell_start_offset: usize,
) {
    let Some(original_tail) = state.input.get(cell_start_offset..) else {
        return;
    };
    let Some(escapes) = escape_collapse_positions(collapsed, original_tail) else {
        return; // unknown transform (e.g. CSV quoting) — leave locations alone
    };
    if escapes.is_empty() {
        return;
    }
    // Reported (collapsed-coordinate) absolute position and line/column of each
    // kept separator char. Escapes never span newlines, so LINE numbers never
    // drift — only columns on the escape's own line, and absolute offsets.
    let base_pos = state
        .line_map
        .offset_to_position(cell_start_offset, state.input);
    let mut marks: Vec<(usize, u32, u32)> = Vec::with_capacity(escapes.len()); // (abs, line, col)
    let mut line = base_pos.line;
    let mut line_start = 0usize; // collapsed byte index of current line start
    let mut next_nl = collapsed.find('\n');
    for &q in &escapes {
        while let Some(nl) = next_nl {
            if nl < q {
                line += 1;
                line_start = nl + 1;
                next_nl = collapsed[nl + 1..].find('\n').map(|k| nl + 1 + k);
            } else {
                break;
            }
        }
        // Columns count Unicode scalars, not bytes.
        let cps_in_line = u32::try_from(collapsed[line_start..q].chars().count()).unwrap_or(0);
        let col = if line == base_pos.line {
            base_pos.column + cps_in_line
        } else {
            cps_in_line + 1
        };
        marks.push((cell_start_offset + q, line, col));
    }
    location_walk::walk_blocks(blocks, &mut |loc| {
        // A char-pointing boundary AT the kept separator moves past its own
        // backslash too (<=); an exclusive end just before it does not (<).
        let start_shift = marks
            .iter()
            .take_while(|(abs, ..)| *abs <= loc.absolute_start)
            .count();
        let end_shift = marks
            .iter()
            .take_while(|(abs, ..)| *abs < loc.absolute_end)
            .count();
        loc.absolute_start += start_shift;
        loc.absolute_end += end_shift;
        let col_shift = |line: u32, col: u32| {
            u32::try_from(
                marks
                    .iter()
                    .filter(|(_, l, c)| *l == line && *c <= col)
                    .count(),
            )
            .unwrap_or(0)
        };
        loc.start.column += col_shift(loc.start.line, loc.start.column);
        loc.end.column += col_shift(loc.end.line, loc.end.column);
    });
}

/// Collapsed-coordinate index of every `\<char>` collapse, by re-aligning the
/// collapsed cell content against the original source it came from. `None` when
/// the texts diverge in any other way (an unmodeled transform) — the caller
/// then leaves locations untouched rather than guessing.
fn escape_collapse_positions(collapsed: &str, original_tail: &str) -> Option<Vec<usize>> {
    let cb = collapsed.as_bytes();
    let ob = original_tail.as_bytes();
    let mut escapes = Vec::new();
    let mut i = 0usize; // original
    let mut j = 0usize; // collapsed
    while j < cb.len() {
        if i < ob.len() && ob[i] == cb[j] {
            i += 1;
            j += 1;
        } else if i + 1 < ob.len() && ob[i] == b'\\' && ob[i + 1] == cb[j] {
            escapes.push(j);
            i += 2;
            j += 1;
        } else {
            return None;
        }
    }
    Some(escapes)
}
