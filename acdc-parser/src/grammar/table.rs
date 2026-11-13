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
    // Reported (collapsed-coordinate) absolute position of each kept separator char.
    let marks: Vec<usize> = escapes.iter().map(|q| cell_start_offset + q).collect();
    location_walk::walk_blocks(blocks, &mut |loc| {
        // Both ends point at a char (inclusive): one AT the kept separator moves
        // past its own backslash too.
        loc.absolute_start += marks
            .iter()
            .take_while(|abs| **abs <= loc.absolute_start)
            .count();
        loc.absolute_end += marks
            .iter()
            .take_while(|abs| **abs <= loc.absolute_end)
            .count();
        // Line and column are where the shifted bytes stand.
        loc.start = state
            .line_map
            .offset_to_position(loc.absolute_start, state.input);
        loc.end = state
            .line_map
            .offset_to_position(loc.absolute_end, state.input);
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
