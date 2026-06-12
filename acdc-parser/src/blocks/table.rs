use crate::{ColumnStyle, HorizontalAlignment, Table, VerticalAlignment};

/// A cell part with its unescaped content and original start position.
struct CellPart {
    /// Unescaped content (e.g., `\|` becomes `|`)
    content: String,
    /// Start position in the original line
    start: usize,
    /// Pre-resolved cell specifier — set by the inline-spec recovery pass
    /// when the previous cell ended with bare style/span/duplication
    /// markers immediately before `|` (e.g., `a|`, `2+|`, `.2+|`, `2*|`).
    ///
    /// Why a full `CellSpecifier` instead of just `ColumnStyle`:
    /// the recovery pass also handles span/dup specs. Previously the
    /// span/dup branch synthesized `cur.content = "{candidate} {trimmed}"`
    /// AND shifted `cur.start` back. The synthesized leading space
    /// "collapsed" the removed `|` separator into one byte — making the
    /// byte-position math non-contiguous and producing an off-by-one in
    /// `cell_start` for the recovered cell (pinned by
    /// `span_dup_recovery_preserves_cur_start_byte_offset`). Storing the
    /// full spec on this field lets the per-part pass apply it without
    /// touching CUR.content / CUR.start — preserves contiguous source
    /// mapping for downstream cursor/position consumers.
    forced_spec: Option<CellSpecifier>,
}

/// Check whether `line` contains an UNESCAPED occurrence of `separator`.
/// For PSV (`|`) and DSV (`:`), `\|`/`\:` is an escape — the separator is
/// content, not a delimiter. Naïve `line.contains(separator)` over-counts
/// escapes as real separators, causing continuation lines that LOOK like
/// they have a separator (because content contains `\|`) to be split into
/// fake cells, dropping the original cell's content silently.
fn line_has_unescaped_separator(line: &str, separator: &str) -> bool {
    if separator.len() != 1 {
        return line.contains(separator);
    }
    let sep_char = separator.chars().next().unwrap();
    // PSV / DSV only escape with `\`.
    if !matches!(sep_char, '|' | ':') {
        return line.contains(separator);
    }
    let mut chars = line.char_indices().peekable();
    while let Some((_idx, ch)) = chars.next() {
        if ch == '\\' {
            // Skip next char (whatever it is — escape consumes it).
            if let Some(&(_, next_ch)) = chars.peek() {
                if next_ch == sep_char {
                    chars.next();
                    continue;
                }
            }
            // Lone `\` — treat as literal, continue scanning.
        } else if ch == sep_char {
            return true;
        }
    }
    false
}

/// Split a line by separator, respecting backslash escapes.
///
/// For PSV (`|`) and DSV (`:`), a backslash before the separator escapes it.
/// Returns parts with their original byte positions for accurate source mapping.
fn split_escaped(line: &str, separator: char) -> Vec<CellPart> {
    let mut parts = Vec::new();
    let mut current_content = String::new();
    let mut part_start = 0;
    let mut chars = line.char_indices().peekable();

    while let Some((byte_idx, ch)) = chars.next() {
        if ch == '\\' {
            // Check if next char is the separator
            if let Some(&(_, next_ch)) = chars.peek() {
                if next_ch == separator {
                    // Escaped separator - add literal separator, skip the backslash
                    current_content.push(separator);
                    chars.next(); // consume the separator
                    continue;
                }
            }
            // Not an escape - add backslash literally
            current_content.push(ch);
        } else if ch == separator {
            // Unescaped separator - end current part
            parts.push(CellPart {
                content: std::mem::take(&mut current_content),
                start: part_start,
                forced_spec: None,
            });
            part_start = byte_idx + ch.len_utf8();
        } else {
            current_content.push(ch);
        }
    }

    // Add final part
    parts.push(CellPart {
        content: current_content,
        start: part_start,
        forced_spec: None,
    });

    parts
}

/// Parse a CSV table body using the `csv` crate for full RFC 4180 compliance.
///
/// This handles multi-line quoted values, escaped quotes, and all CSV edge cases.
/// Returns rows with cells containing their content and accurate byte positions.
fn parse_csv_table(text: &str, base_offset: usize) -> Vec<Vec<CellPart>> {
    let text_bytes = text.as_bytes();
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true) // allow variable column counts
        .from_reader(text_bytes);

    let mut rows = Vec::new();

    for result in reader.records() {
        let Ok(record) = result else {
            continue;
        };

        // Get the byte position where this record starts in the input
        let record_start = record
            .position()
            .map_or(0, |p| usize::try_from(p.byte()).unwrap_or(0));

        let mut cells = Vec::new();
        let mut scan_pos = record_start;

        for field in &record {
            // Find actual field position by scanning the original text
            let (field_content_start, next_pos) =
                find_csv_field_position(text_bytes, scan_pos, field);

            cells.push(CellPart {
                content: field.to_string(),
                start: base_offset + field_content_start,
                forced_spec: None,
            });

            scan_pos = next_pos;
        }

        rows.push(cells);
    }

    rows
}

/// Find the actual byte position of a CSV field's content in the original text.
///
/// Returns `(content_start, next_scan_position)` where:
/// - `content_start` is where the field's actual content begins (after opening quote if quoted)
/// - `next_scan_position` is where to start scanning for the next field
fn find_csv_field_position(text: &[u8], start: usize, expected_content: &str) -> (usize, usize) {
    let Some(&first_byte) = text.get(start) else {
        return (start, start);
    };

    if first_byte == b'"' {
        // Quoted field: content starts after the opening quote
        let content_start = start + 1;
        // Find the closing quote (handle escaped quotes "")
        let end_pos = find_closing_quote(text, start + 1);
        // Next field starts after closing quote and comma (or newline)
        let next_pos = skip_to_next_field(text, end_pos);
        (content_start, next_pos)
    } else {
        // Unquoted field: content starts at current position
        let content_start = start;
        // Find end of field (comma or newline)
        let end_pos = find_unquoted_field_end(text, start, expected_content.len());
        // Next field starts after the separator
        let next_pos = skip_to_next_field(text, end_pos);
        (content_start, next_pos)
    }
}

/// Find the closing quote of a quoted CSV field, handling escaped quotes (`""`).
fn find_closing_quote(text: &[u8], start: usize) -> usize {
    let mut pos = start;
    while let Some(&byte) = text.get(pos) {
        if byte == b'"' {
            // Check if this is an escaped quote ("")
            if text.get(pos + 1) == Some(&b'"') {
                // Escaped quote - skip both and continue
                pos += 2;
            } else {
                // Closing quote found
                return pos;
            }
        } else {
            pos += 1;
        }
    }
    // No closing quote found - return end of text
    text.len()
}

/// Find the end of an unquoted CSV field.
fn find_unquoted_field_end(text: &[u8], start: usize, content_len: usize) -> usize {
    // The field ends at comma, CR, LF, or content_len bytes (whichever comes first)
    let mut pos = start;
    let mut remaining = content_len;
    while let Some(&byte) = text.get(pos) {
        if byte == b',' || byte == b'\n' || byte == b'\r' {
            return pos;
        }
        if remaining == 0 {
            return pos;
        }
        remaining = remaining.saturating_sub(1);
        pos += 1;
    }
    text.len()
}

/// Skip past the current field separator to find the start of the next field.
fn skip_to_next_field(text: &[u8], pos: usize) -> usize {
    let mut pos = pos;
    // Skip closing quote if present
    if text.get(pos) == Some(&b'"') {
        pos += 1;
    }
    // Skip comma or newline characters
    while let Some(&byte) = text.get(pos) {
        if byte == b',' {
            return pos + 1;
        }
        if byte == b'\r' || byte == b'\n' {
            // Skip CRLF or just LF
            if byte == b'\r' && text.get(pos + 1) == Some(&b'\n') {
                return pos + 2;
            }
            return pos + 1;
        }
        pos += 1;
    }
    pos
}

/// Determine if this is a CSV format table.
fn is_csv_format(separator: &str) -> bool {
    separator == ","
}

/// Split a line into cell parts using the appropriate method for the separator.
///
/// Note: CSV format is handled separately via `parse_csv_table()` for multi-line support.
fn split_line(line: &str, separator: &str) -> Vec<CellPart> {
    if let Some(sep_char) = separator.chars().next() {
        if separator.len() == 1 {
            split_escaped(line, sep_char)
        } else {
            // Multi-char separator - no escape handling
            split_multi_char(line, separator)
        }
    } else {
        // Empty separator - return whole line as one part
        vec![CellPart {
            content: line.to_string(),
            start: 0,
            forced_spec: None,
        }]
    }
}

/// Split by multi-character separator (no escape handling).
fn split_multi_char(line: &str, separator: &str) -> Vec<CellPart> {
    let mut parts = Vec::new();
    let mut last_end = 0;
    for (idx, _) in line.match_indices(separator) {
        parts.push(CellPart {
            content: line.get(last_end..idx).unwrap_or("").to_string(),
            start: last_end,
            forced_spec: None,
        });
        last_end = idx + separator.len();
    }
    parts.push(CellPart {
        content: line.get(last_end..).unwrap_or("").to_string(),
        start: last_end,
        forced_spec: None,
    });
    parts
}

/// Context for parsing cell specifiers, controlling which specifier types are valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseContext {
    /// First part before separator in PSV tables - style-only specifiers allowed (e.g., `s|`)
    FirstPart,
    /// Inline cell content - style-only specifiers NOT allowed (prevents "another" → 'a' style)
    InlineContent,
}

/// Represents a parsed cell specifier with span, alignment, and style information.
///
/// In `AsciiDoc`, cell specifiers appear before the cell separator with format:
/// `[halign][valign][colspan][.rowspan][op][style]|`
///
/// Examples:
/// - `2+|content` → colspan=2
/// - `.3+|content` → rowspan=3
/// - `2.3+|content` → colspan=2, rowspan=3
/// - `^.>2+s|content` → center, bottom, colspan=2, strong style
/// - `3*|content` → duplicate cell 3 times
#[derive(Debug, Clone, Copy)]
pub(crate) struct CellSpecifier {
    pub colspan: usize,
    pub rowspan: usize,
    pub halign: Option<HorizontalAlignment>,
    pub valign: Option<VerticalAlignment>,
    pub style: Option<ColumnStyle>,
    /// If true, this is a duplication specifier (`*`) rather than a span (`+`).
    pub is_duplication: bool,
    /// For duplication, this is the count (e.g., `3*` means 3 copies).
    pub duplication_count: usize,
}

impl Default for CellSpecifier {
    fn default() -> Self {
        Self {
            colspan: 1,
            rowspan: 1,
            halign: None,
            valign: None,
            style: None,
            is_duplication: false,
            duplication_count: 1,
        }
    }
}

/// Parse a single style letter into a `ColumnStyle`.
fn parse_style_byte(byte: u8) -> Option<ColumnStyle> {
    match byte {
        b'a' => Some(ColumnStyle::AsciiDoc),
        b'd' => Some(ColumnStyle::Default),
        b'e' => Some(ColumnStyle::Emphasis),
        b'h' => Some(ColumnStyle::Header),
        b'l' => Some(ColumnStyle::Literal),
        b'm' => Some(ColumnStyle::Monospace),
        b's' => Some(ColumnStyle::Strong),
        _ => None,
    }
}

impl CellSpecifier {
    /// Parse a cell specifier from the beginning of cell content.
    ///
    /// Returns the specifier and the offset where actual content begins.
    /// Full pattern: `[halign][valign][colspan][.rowspan][+|*][style]`
    ///
    /// The `mode` parameter controls whether style-only specifiers
    /// (e.g., `s|` for strong without any alignment or span) are accepted:
    /// - `ParseContext::FirstPart`: Accept style-only specifiers (first part before separator)
    /// - `ParseContext::InlineContent`: Reject style-only (prevents "another" → 'a' style)
    ///
    /// Examples:
    /// - `"2+rest"` → colspan=2
    /// - `".3+rest"` → rowspan=3
    /// - `"2.3+rest"` → colspan=2, rowspan=3
    /// - `"^.>2+srest"` → center, bottom, colspan=2, strong style
    /// - `"3*rest"` → `duplication_count`=3
    /// - `"plain"` → defaults (no specifier found)
    #[must_use]
    pub fn parse(content: &str, mode: ParseContext) -> (Self, usize) {
        let bytes = content.as_bytes();
        let mut pos = 0;

        // Phase 1: Parse optional alignment markers
        let (halign, valign, align_end) = Self::parse_alignments(bytes, pos);
        pos = align_end;

        // Phase 2: Parse optional colspan (digits)
        let (colspan, colspan_end) = Self::parse_number(content, bytes, pos);
        pos = colspan_end;

        // Phase 3: Parse optional rowspan (dot followed by digits)
        let (rowspan, rowspan_end) = Self::parse_rowspan(content, bytes, pos);
        pos = rowspan_end;

        // Phase 4: Check for operator and build result
        Self::build_result(bytes, pos, colspan, rowspan, halign, valign, mode)
    }

    /// Parse alignment markers at the current position.
    /// Returns `(halign, valign, new_position)`.
    fn parse_alignments(
        bytes: &[u8],
        mut pos: usize,
    ) -> (
        Option<HorizontalAlignment>,
        Option<VerticalAlignment>,
        usize,
    ) {
        let mut halign: Option<HorizontalAlignment> = None;
        let mut valign: Option<VerticalAlignment> = None;

        loop {
            match bytes.get(pos) {
                Some(b'<') => {
                    halign = Some(HorizontalAlignment::Left);
                    pos += 1;
                }
                Some(b'^') => {
                    halign = Some(HorizontalAlignment::Center);
                    pos += 1;
                }
                Some(b'>') => {
                    halign = Some(HorizontalAlignment::Right);
                    pos += 1;
                }
                Some(b'.') => {
                    // Could be vertical alignment (.< .^ .>) or rowspan (.N)
                    match bytes.get(pos + 1) {
                        Some(b'<') => {
                            valign = Some(VerticalAlignment::Top);
                            pos += 2;
                        }
                        Some(b'^') => {
                            valign = Some(VerticalAlignment::Middle);
                            pos += 2;
                        }
                        Some(b'>') => {
                            valign = Some(VerticalAlignment::Bottom);
                            pos += 2;
                        }
                        _ => break, // Not vertical alignment, might be rowspan
                    }
                }
                _ => break,
            }
        }

        (halign, valign, pos)
    }

    /// Parse a number (for colspan) at the current position.
    /// Returns `(parsed_value, new_position)`.
    fn parse_number(content: &str, bytes: &[u8], mut pos: usize) -> (Option<usize>, usize) {
        let start = pos;
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        let value = if pos > start {
            content
                .get(start..pos)
                .and_then(|s| s.parse::<usize>().ok())
        } else {
            None
        };
        (value, pos)
    }

    /// Parse rowspan (dot followed by digits) at the current position.
    /// Returns `(parsed_value, new_position)`.
    fn parse_rowspan(content: &str, bytes: &[u8], mut pos: usize) -> (Option<usize>, usize) {
        if bytes.get(pos) != Some(&b'.') {
            return (None, pos);
        }

        let dot_pos = pos;
        pos += 1;
        let start = pos;
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }

        if pos > start {
            let value = content
                .get(start..pos)
                .and_then(|s| s.parse::<usize>().ok());
            (value, pos)
        } else {
            // Dot without following digits - not a rowspan specifier
            (None, dot_pos)
        }
    }

    /// Build the final result based on parsed components.
    fn build_result(
        bytes: &[u8],
        mut pos: usize,
        colspan: Option<usize>,
        rowspan: Option<usize>,
        halign: Option<HorizontalAlignment>,
        valign: Option<VerticalAlignment>,
        context: ParseContext,
    ) -> (Self, usize) {
        let has_span_or_dup = colspan.is_some() || rowspan.is_some();
        let is_duplication = bytes.get(pos) == Some(&b'*');
        let is_span = bytes.get(pos) == Some(&b'+');

        if (is_span || is_duplication) && has_span_or_dup {
            pos += 1;

            // Parse optional style letter after operator
            let style = bytes.get(pos).and_then(|&b| parse_style_byte(b));
            if style.is_some() {
                pos += 1;
            }

            let spec = if is_duplication {
                Self {
                    colspan: 1,
                    rowspan: 1,
                    halign,
                    valign,
                    style,
                    is_duplication: true,
                    duplication_count: colspan.unwrap_or(1),
                }
            } else {
                Self {
                    colspan: colspan.unwrap_or(1),
                    rowspan: rowspan.unwrap_or(1),
                    halign,
                    valign,
                    style,
                    is_duplication: false,
                    duplication_count: 1,
                }
            };
            (spec, pos)
        } else if (halign.is_some() || valign.is_some()) && context == ParseContext::FirstPart {
            // Alignment without span operator - still valid (only in FirstPart context)
            let style = bytes.get(pos).and_then(|&b| parse_style_byte(b));
            if style.is_some() {
                pos += 1;
            }
            (
                Self {
                    colspan: 1,
                    rowspan: 1,
                    halign,
                    valign,
                    style,
                    is_duplication: false,
                    duplication_count: 1,
                },
                pos,
            )
        } else if context == ParseContext::FirstPart {
            // Check for style-only specifier (e.g., `s|` for strong)
            // Only accepted in FirstPart context (first-part in PSV tables)
            let style = bytes.get(pos).and_then(|&b| parse_style_byte(b));
            if let Some(style) = style {
                pos += 1;
                (
                    Self {
                        colspan: 1,
                        rowspan: 1,
                        halign: None,
                        valign: None,
                        style: Some(style),
                        is_duplication: false,
                        duplication_count: 1,
                    },
                    pos,
                )
            } else {
                (Self::default(), 0)
            }
        } else {
            // No valid specifier found
            (Self::default(), 0)
        }
    }
}

/// A parsed table cell with position, span, alignment, and style information.
///
/// `start` marks the document offset of the cell as a whole (the byte just
/// past the cell specifier and separator). `content_start` marks the offset
/// of the first byte of `content` in the document — they diverge when the
/// cell's content begins on a continuation line, e.g.:
///
/// ```text
/// a|              <- start points here (end of `a|`)
/// !===            <- content_start points here
/// ```
///
/// Recursive parsers consuming the cell content (in particular the
/// `AsciiDoc`-style `a|` cell) must use `content_start` so that diagnostics
/// resolve to the line of the offending token, not the cell's style prefix.
#[derive(Debug, Clone)]
pub(crate) struct ParsedCell {
    pub content: String,
    pub start: usize,
    pub content_start: usize,
    pub end: usize,
    pub colspan: usize,
    pub rowspan: usize,
    pub halign: Option<HorizontalAlignment>,
    pub valign: Option<VerticalAlignment>,
    pub style: Option<ColumnStyle>,
    pub is_duplication: bool,
    pub duplication_count: usize,
}

/// Check if a blank line after the first row indicates a header.
/// A header is indicated only if the first non-empty line after the blank
/// contains a separator. If it's a continuation line (no separator), it's content
/// that attaches to the previous cell, not a header indicator.
fn detect_header_after_first_row(lines: &[&str], start_idx: usize, separator: &str) -> bool {
    for &line in lines.iter().skip(start_idx) {
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            return trimmed.contains(separator);
        }
    }
    false
}

/// Count the colspan-weighted cell contribution of a single PSV/DSV line.
/// Used by the multi-line row collector to detect when accumulated cells
/// have filled the row (so the next `|`-prefixed line starts a new row).
///
/// For PSV (`|` / `!`):
/// - `parts[0]` (text before first separator) is skipped for content; if
///   it's itself a bare specifier like `.2+`, capture its colspan as
///   pending for `parts[1]`.
/// - Each subsequent part contributes its colspan (1 if no leading spec).
/// - A trailing empty part (line ending with the separator) is skipped.
///
/// Continuation lines (no separator at all) contribute 0 — they will be
/// absorbed into the previous cell's content by `parse_row_with_positions`.
fn count_cell_colspans(line: &str, separator: &str) -> usize {
    // Escape-aware: a line with only `\|` (no real separators) contributes 0
    // (it's continuation content, not a new cell-start).
    if !line_has_unescaped_separator(line, separator) {
        return 0;
    }
    let parts = split_line(line, separator);
    if parts.is_empty() {
        return 0;
    }
    let mut total: usize = 0;
    let mut pending_colspan: Option<usize> = None;

    let start_idx = if matches!(separator, "|" | "!") {
        let p0 = parts[0].content.trim();
        if !p0.is_empty() {
            let (spec, spec_len) = CellSpecifier::parse(p0, ParseContext::FirstPart);
            if spec_len > 0 && spec_len == p0.len() {
                pending_colspan = Some(spec.colspan);
            }
        }
        1
    } else {
        // DSV (`:`): parts[0] is content
        0
    };

    let total_parts = parts.len();
    for (i, part) in parts.iter().enumerate().skip(start_idx) {
        let trimmed = part.content.trim();
        // Trailing empty part (line ends with separator) — usually a
        // bare line ender, no cell. BUT: when the previous part ends with
        // a cell style char (`a`, `s`, `m`, `l`, `v`, `e`, `h`, `d`) the
        // trailing empty IS a real cell (an `a|`-style cell whose content
        // arrives on subsequent multi-line continuation). Without counting
        // it, the row's accumulated_cols is short by one, so the ncols-
        // aware break at the outer line-iterating loop doesn't fire when
        // expected — the row absorbs the NEXT row's lines and ends up
        // over-counting cells, triggering the document-level "row exceeds
        // ncols → drop" branch. This silently loses /quote-cb-style rows
        // in fixtures with `a|` modifier + multi-line content.
        if trimmed.is_empty() && i + 1 == total_parts && total_parts > 1 {
            // Mirror the stricter `is_single_line_row` check above
            // (`parse_rows_with_positions`): the trailing-`a|` heuristic
            // must require the previous part to END in a STANDALONE cell
            // specifier token (whitespace-separated, parsing as a complete
            // FirstPart spec) — NOT just any text whose last char happens
            // to be a style letter (false positives like "Same" ending in
            // 'e' caused multi-line accumulation to over-count cells and
            // bleed the next row's content into this row).
            let prev_ends_with_style = i > 0
                && parts.get(i - 1).is_some_and(|p| {
                    let pt = p.content.trim_end();
                    let last_token = pt
                        .rsplit_once(char::is_whitespace)
                        .map_or(pt, |(_, after)| after);
                    if last_token.is_empty() {
                        return false;
                    }
                    let (_, spec_len) =
                        CellSpecifier::parse(last_token, ParseContext::FirstPart);
                    spec_len > 0 && spec_len == last_token.len()
                });
            if !prev_ends_with_style {
                continue;
            }
            // Otherwise: count as 1 cell (continuation cell).
            total += 1;
            continue;
        }
        let cs = if let Some(pending) = pending_colspan.take() {
            pending
        } else {
            let (spec, spec_len) = CellSpecifier::parse(trimmed, ParseContext::InlineContent);
            if spec_len > 0 { spec.colspan } else { 1 }
        };
        total += cs;
    }
    total
}

/// Check if a line starting with a cell specifier followed by separator indicates a new row.
/// This detects patterns like `a|`, `s|`, `2+|`, `^|`, `^.>2+s|` at the start of a line.
/// Count unescaped occurrences of `separator` in `line`. Used by
/// the row-collection loop to detect inline-row format (multi-separator
/// line) vs continuation paragraphs of a multi-line `a|` cell.
fn count_unescaped_separators(line: &str, separator: &str) -> usize {
    let sep_ch = separator.chars().next().unwrap_or('|');
    let mut count = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            chars.next();
            continue;
        }
        if ch == sep_ch {
            count += 1;
        }
    }
    count
}

fn is_new_row_start(line: &str, separator: &str) -> bool {
    // Only applies to PSV tables (| separator)
    if separator != "|" {
        return false;
    }
    // Escape-aware separator find. `\|` is content, not a row-start delimiter.
    let mut chars = line.char_indices().peekable();
    let mut sep_pos: Option<usize> = None;
    while let Some((idx, ch)) = chars.next() {
        if ch == '\\' {
            // Skip escaped char (whatever it is).
            chars.next();
            continue;
        }
        if ch == '|' {
            sep_pos = Some(idx);
            break;
        }
    }
    let Some(sep_pos) = sep_pos else { return false };

    let before_sep = line[..sep_pos].trim();
    if before_sep.is_empty() {
        return false;
    }

    // Check if the content before separator is a valid cell specifier
    let (_, spec_len) = CellSpecifier::parse(before_sep, ParseContext::FirstPart);
    spec_len > 0 && spec_len == before_sep.len()
}

/// Handle continuation lines that appear after blank lines.
/// These should be appended to the previous row's last cell.
///
/// The caller enters this only after skipping at least one blank line, so
/// the first appended line starts a *new paragraph* of the last cell, not a
/// continuation of the cell's existing final line. We therefore preserve
/// the blank-line boundary with `\n\n`; subsequent lines within the same
/// paragraph join with a single `\n`. Preserving the boundary matters most
/// for `a`-cells, where the content is re-parsed as `AsciiDoc` blocks and
/// the paragraph break may carry a nested table's own structure.
///
/// Returns `true` if any continuation lines were consumed — caller uses this
/// to know whether the prior blank line was an intra-cell paragraph break
/// (consumed = yes, the "blank" was inside a cell's content) vs an actual
/// row terminator (consumed = no, the blank cleanly separated rows).
fn handle_cross_row_continuation(
    lines: &[&str],
    i: &mut usize,
    current_offset: &mut usize,
    rows: &mut [Vec<ParsedCell>],
    separator: &str,
) -> bool {
    let mut starting_new_paragraph = true;
    let mut consumed_any = false;
    while let Some(&next_line) = lines.get(*i) {
        let trimmed = next_line.trim_end();
        // If line has UNESCAPED separator or is empty, break - normal row
        // processing. `contains(separator)` alone treats `\|` (escaped pipe,
        // legitimate listing-block content like `\| 标题1 \| 标题2`) as a
        // row separator and prematurely terminates continuation, splitting
        // multi-line `a|` cell content (containing nested AsciiDoc listings
        // with escaped pipes) into separate "rows" that downstream cell
        // sub-doc parsing then mis-detects as `break + paragraph + paragraph`.
        // count_unescaped_separators respects `\|` per the same #116 fix.
        if trimmed.is_empty() || count_unescaped_separators(trimmed, separator) > 0 {
            break;
        }
        // Continuation line - append to previous row's last cell
        if let Some(last_row) = rows.last_mut() {
            if let Some(last_cell) = last_row.last_mut() {
                if last_cell.content.is_empty() {
                    // See `parse_row_with_positions`: first content byte
                    // anchors `content_start` for cell-internal diagnostics.
                    last_cell.content_start = *current_offset;
                } else if starting_new_paragraph {
                    last_cell.content.push_str("\n\n");
                } else {
                    last_cell.content.push('\n');
                }
                last_cell.content.push_str(trimmed);
                last_cell.end = *current_offset + trimmed.len().saturating_sub(1);
            }
        }
        consumed_any = true;
        starting_new_paragraph = false;
        *current_offset += next_line.len() + 1;
        *i += 1;
    }
    consumed_any
}

impl Table<'_> {
    pub(crate) fn parse_rows_with_positions(
        text: &str,
        separator: &str,
        has_header: &mut bool,
        base_offset: usize,
        ncols: Option<usize>,
    ) -> Vec<Vec<ParsedCell>> {
        // CSV format needs special handling for multi-line quoted values
        if is_csv_format(separator) {
            return Self::parse_csv_rows_with_positions(text, has_header, base_offset);
        }

        let mut rows: Vec<Vec<ParsedCell>> = Vec::new();
        let mut current_offset = base_offset;
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        // Track whether the prior iteration's row was added via a path where
        // a following blank+multi-line-row should merge in (newline-cell-
        // layout within the same row) vs start a new row.
        //
        // Spec model:
        // - INLINE-row (one logical line, multiple cells): blank terminates row.
        // - NEWLINE-cell-layout row (each cell as its own line group):
        //   the row continues across blank-separated cell groups until
        //   `ncols` worth of cells fill, OR a non-block cell-start line
        //   begins (which would mean inline-mode resumed). Blank between
        //   cell groups of the SAME row is intra-row.
        //
        // Heuristic: the prior row qualifies for "newline-cell-layout
        // continuation merge" ONLY IF its last cell's source spans multiple
        // physical lines (the cell is itself a multi-line group, signalling
        // we're in newline-cell-layout mode). A single-line cell (e.g.
        // `|香蕉` alone) is an INCOMPLETE inline row, not newline-cell-
        // layout — a blank terminates it and the next group starts a new row.
        let mut prior_row_blank_terminated = false;
        let mut prior_row_last_cell_multiline = false;

        tracing::debug!(
            ?has_header,
            ?ncols,
            total_lines = lines.len(),
            "Starting table parsing"
        );

        while let Some(&line_ref) = lines.get(i) {
            let line = line_ref.trim_end();
            tracing::trace!(i, ?line, is_empty = line.is_empty(), "Processing line");

            // If we are in the first row and it is empty, we should not have a header
            if i == 0 && line.is_empty() {
                *has_header = false;
                current_offset += line.len() + 1;
                i += 1;
                continue;
            }

            // Collect lines for this row (until we hit an empty line or end)
            let mut row_lines = Vec::new();
            let row_start_offset = current_offset;

            // Check if this is a single-line-per-row table (line has multiple separators)
            // vs multi-line-per-row table (one cell per line, rows separated by empty lines)
            //
            // PSV (`|`) supports multi-line cells; DSV (`:`) / TSV (`\t`) do not (every
            // line is a complete row). For PSV, count UNESCAPED separators via
            // `split_line` so that lines containing `\|` (e.g. AsciiDoc reference
            // docs describing `a|` syntax inside backticks: `` `a\|` ``) aren't
            // mis-classified as single-line rows. Previously the naïve
            // `first_line.matches(separator).count()` over-counted escaped
            // separators, triggering "unterminated table block" cascades — see
            // Glyph render-fidelity tests for the user-visible failure mode.
            let first_line = line_ref.trim_end();
            let is_single_line_row = if matches!(separator, "|" | "!") {
                if first_line.contains(separator) {
                    let parts = split_line(first_line, separator);
                    // `split_line` for `|` returns leading empty + N content parts.
                    // > 2 parts ⇒ at least 2 real cell boundaries ⇒ single-line row.
                    // BUT: if the line ENDS with a cell modifier followed by
                    // a separator and NO content (e.g. `a| label a| more a|`
                    // ending in `a|` with no trailing content), the last
                    // cell's content arrives on subsequent lines as
                    // multi-line continuation. Asciidoctor handles this as
                    // a multi-line row. Treating it as single-line drops
                    // the continuation lines into a detached "row" whose
                    // first line has no separator — parse_row_with_positions
                    // then skips them entirely (no last_cell to append to),
                    // and the `a|` cell ends up with empty content.
                    //
                    // Distinguish from `|placeholder||` (3 cells, last 2
                    // empty, FULLY single-line): the second-to-last part
                    // ends with a cell style/spec character (`a`, `s`,
                    // `m`, `l`, `v`, `e`, `h`, `d`, or a colspan digit
                    // followed by `+`). For `|placeholder||`, the second-
                    // to-last part is empty (no modifier char preceding
                    // the trailing `||`).
                    let trailing_modifier_with_empty = parts.len() >= 2
                        && parts.last().is_some_and(|p| p.content.trim().is_empty())
                        && parts.get(parts.len() - 2).is_some_and(|p| {
                            let trimmed = p.content.trim_end();
                            // The trailing `a|` / `2+|` / `.2+|` / `^|` etc.
                            // marker that signals a multi-line continuation
                            // cell must be a STANDALONE token at the end —
                            // separated from prior content by whitespace —
                            // AND parse as a complete cell specifier under
                            // FirstPart grammar. The previous ends_with-char
                            // heuristic mis-fired on plain text words ending
                            // in a style letter (e.g. "Same" ends with 'e',
                            // "Total" ends with 'l') — those have NO trailing-
                            // spec semantics and a row like `| Same | Same |`
                            // is a fully-formed single-line row whose last
                            // cell happens to be empty. Mis-detecting them as
                            // multi-line continuation makes the collector
                            // absorb the NEXT row into this one (see
                            // `asciidoc-comprehensive-table-edit-all-cells`
                            // Tables #25/#26/#37 case 1 regressions).
                            let last_token = trimmed
                                .rsplit_once(char::is_whitespace)
                                .map_or(trimmed, |(_, after)| after);
                            if last_token.is_empty() {
                                return false;
                            }
                            let (_, spec_len) =
                                CellSpecifier::parse(last_token, ParseContext::FirstPart);
                            spec_len > 0 && spec_len == last_token.len()
                        });
                    parts.len() > 2 && !trailing_modifier_with_empty
                } else {
                    false
                }
            } else {
                // DSV / TSV: every line is a row (no multi-line cells).
                first_line.matches(separator).count() > 0
            };

            if is_single_line_row {
                // Single-line row format: each line is a complete row
                row_lines.push(first_line);
                current_offset += line_ref.len() + 1;
                i += 1;
            } else {
                // Multi-line row format: collect lines until empty line, a new
                // row start (line begins with a `.N+|` / `2+|` / `a|` / etc.
                // spec), OR — when `ncols` is known — until accumulated cells
                // already fill the row and the next line is a `|cell`-prefix
                // continuation that actually belongs to the next row. The
                // ncols-aware break is what makes layouts like
                //     .2+|food
                //     |apple .2+|10
                //     |banana
                // parse correctly: without it, `|banana` (which `is_new_row_
                // start` rejects because parts[0]="") gets bundled into the
                // food row, producing an over-full row that downstream layout
                // can't reconcile.
                let mut accumulated_cols: usize = 0;
                // Track whether row_lines includes "continuation paragraph"
                // lines (non-separator-starting lines that are content of a
                // multi-line `a|` cell). When true, a subsequent line that
                // looks like an inline row (starts with `|` AND has multiple
                // separators) is a NEW row, NOT continuation — even when
                // accumulated_cols < ncols. Asciidoctor treats trailing-`a|`
                // cells as extending until the next clear row marker; the
                // remaining columns (3-5 in a 6-col table with 3 `a|` cells)
                // render as empty/phantom, not filled from continuation.
                let mut row_has_continuation_paragraph = false;
                while let Some(&current_line) = lines.get(i) {
                    let trimmed = current_line.trim_end();
                    if trimmed.is_empty() {
                        break;
                    }
                    // If we already have content and this line starts a new row, break
                    if !row_lines.is_empty() && is_new_row_start(trimmed, separator) {
                        break;
                    }
                    // Trailing-`a|` multi-line cell continuation: when a
                    // previous line was a continuation paragraph and the
                    // current line is a clear inline-row (multi-`|` line),
                    // break — the `a|` cell content ended; this is a new
                    // row. See `row_has_continuation_paragraph` doc above.
                    if row_has_continuation_paragraph
                        && matches!(separator, "|" | "!")
                        && trimmed.starts_with(separator)
                        && count_unescaped_separators(trimmed, separator) > 1
                    {
                        break;
                    }
                    if !row_lines.is_empty()
                        && matches!(separator, "|" | "!")
                        && trimmed.starts_with(separator)
                        && let Some(expected) = ncols
                        && accumulated_cols >= expected
                    {
                        break;
                    }
                    if !row_lines.is_empty()
                        && matches!(separator, "|" | "!")
                        && !trimmed.starts_with(separator)
                    {
                        row_has_continuation_paragraph = true;
                    }
                    accumulated_cols += count_cell_colspans(trimmed, separator);
                    row_lines.push(trimmed);
                    current_offset += current_line.len() + 1; // +1 for newline
                    i += 1;
                }
            }

            if !row_lines.is_empty() {
                let columns =
                    Self::parse_row_with_positions(&row_lines, separator, row_start_offset);

                // For multi-line tables with explicit ncols, check if we need to merge
                // this cell group with existing incomplete row (for nested table support).
                // BUT: only merge when NO blank line separated the rows — a blank line
                // is the unambiguous row terminator per AsciiDoc spec, so the new cells
                // belong to a NEW row regardless of prior row's completeness.
                // Merge only when:
                // (a) current row is multi-line (each cell-group → iteration),
                // (b) prior row is incomplete (< ncols),
                // (c) NOT (blank-line-after-single-line-row terminated prior).
                // (c) is the spec-correct row boundary for inline rows that
                // happen to be incomplete (e.g. authored `|R2C1|R2C2` in a
                // 3-col table). Multi-line prior rows allow blanks as
                // intra-row separators (newline cell layout).
                // Merge only when the blank-separated rows are part of the
                // SAME logical row in newline-cell-layout — signalled by the
                // prior row's last cell being multi-line. Single-line prior
                // (incomplete inline row) → blank terminates, push as new.
                let allow_merge = !prior_row_blank_terminated || prior_row_last_cell_multiline;
                let mut merged_this_iter = false;
                if !is_single_line_row
                    && allow_merge
                    && let Some(expected_cols) = ncols
                    && let Some(last_row) = rows.last_mut()
                {
                    let last_row_cols: usize = last_row.iter().map(|c| c.colspan).sum();
                    if last_row_cols < expected_cols {
                        // Last row is incomplete, merge these cells into it
                        last_row.extend(columns);
                        tracing::trace!(
                            last_row_cols,
                            expected_cols,
                            "Merged cells into incomplete row"
                        );
                        merged_this_iter = true;
                    } else {
                        rows.push(columns);
                    }
                } else {
                    rows.push(columns);
                }
                // suppress unused warning
                let _ = merged_this_iter;
            }

            // After processing the first row, check if blank line indicates header.
            // Only check when ncols is not specified (no merge needed) or when
            // the row is complete (has enough columns).
            let first_row_col_count: usize = rows
                .first()
                .map_or(0, |r| r.iter().map(|c| c.colspan).sum());
            let first_row_complete = ncols.is_none_or(|n| first_row_col_count >= n);
            if rows.len() == 1
                && first_row_complete
                && let Some(&next_line) = lines.get(i)
                && next_line.trim_end().is_empty()
                && detect_header_after_first_row(&lines, i, separator)
            {
                tracing::debug!("Detected table header via blank line after first row");
                *has_header = true;
            }

            // Skip empty lines and track if we skipped any
            let mut skipped_blank_line = false;
            while let Some(&empty_line) = lines.get(i) {
                if !empty_line.trim_end().is_empty() {
                    break;
                }
                skipped_blank_line = true;
                current_offset += empty_line.len() + 1;
                i += 1;
            }
            // Handle continuation lines only if we skipped a blank line.
            // Without a blank line, there's no cross-row continuation scenario.
            let mut continuation_consumed = false;
            if skipped_blank_line {
                continuation_consumed = handle_cross_row_continuation(
                    &lines,
                    &mut i,
                    &mut current_offset,
                    &mut rows,
                    separator,
                );
            }
            // Pass the blank-line-terminator signal to the next iteration so
            // the merge-into-incomplete-row logic respects spec row boundaries.
            // BUT: if continuation actually consumed lines, the blank was an
            // intra-cell paragraph break (the consumed lines belonged to the
            // prior row's last cell) — NOT a true row terminator.
            prior_row_blank_terminated = skipped_blank_line && !continuation_consumed;
            // Track AFTER continuation runs — multi-line cell content from
            // `handle_cross_row_continuation` only appears in the cell's
            // `content` field AFTER that helper appends it. Checking earlier
            // would always see just the first source line.
            prior_row_last_cell_multiline = rows
                .last()
                .and_then(|r| r.last())
                .map(|c| c.content.contains('\n'))
                .unwrap_or(false);
        }

        rows
    }

    /// Parse CSV table rows using the `csv` crate for RFC 4180 compliance.
    ///
    /// This handles multi-line quoted values correctly by processing the entire
    /// table body at once rather than line-by-line.
    fn parse_csv_rows_with_positions(
        text: &str,
        has_header: &mut bool,
        base_offset: usize,
    ) -> Vec<Vec<ParsedCell>> {
        // Check for header indicator: first row followed by blank line
        // For CSV, we need to detect this before parsing since the csv crate
        // consumes the text as a stream.
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() >= 2 {
            // Find where first CSV record ends - look for first complete record
            // A simple heuristic: if line 1 (0-indexed) is empty, we have a header
            if let Some(&line) = lines.get(1) {
                if line.trim().is_empty() {
                    *has_header = true;
                }
            }
        }

        let csv_rows = parse_csv_table(text, base_offset);
        let mut rows = Vec::new();

        for csv_row in csv_rows {
            let mut cells = Vec::new();
            for part in csv_row {
                let content = part.content.trim();
                let start = part.start;
                let end = if content.is_empty() {
                    start
                } else {
                    start + content.len().saturating_sub(1)
                };

                cells.push(ParsedCell {
                    content: content.to_string(),
                    start,
                    content_start: start,
                    end,
                    colspan: 1,
                    rowspan: 1,
                    halign: None,
                    valign: None,
                    style: None,
                    is_duplication: false,
                    duplication_count: 1,
                });
            }
            if !cells.is_empty() {
                rows.push(cells);
            }
        }

        rows
    }

    fn parse_row_with_positions(
        row_lines: &[&str],
        separator: &str,
        row_start_offset: usize,
    ) -> Vec<ParsedCell> {
        let mut columns: Vec<ParsedCell> = Vec::new();
        let mut current_offset = row_start_offset;

        for line in row_lines {
            // Check if line contains an UNESCAPED separator. A line with only
            // escaped separators (e.g. `content with \|`) is content, NOT a
            // new cell-start — should be appended to the previous cell as
            // continuation.
            if !line_has_unescaped_separator(line, separator) {
                // Continuation line: append to last cell's content
                if let Some(last_cell) = columns.last_mut() {
                    if last_cell.content.is_empty() {
                        // First content for this cell arrives from a
                        // continuation line — anchor `content_start` here so
                        // diagnostics from recursive parses (e.g. a-cells)
                        // resolve to the offending token, not the cell's
                        // style prefix line.
                        last_cell.content_start = current_offset;
                    } else {
                        last_cell.content.push('\n');
                    }
                    last_cell.content.push_str(line);
                    // Update end position to include this line
                    last_cell.end = current_offset + line.len().saturating_sub(1);
                }
                current_offset += line.len() + 1; // +1 for newline
                continue;
            }

            // Split the line by separator, handling escapes appropriately
            let mut parts = split_line(line, separator);

            // PSV adjacent-anchor recovery: AsciiDoc allows two anchors on the
            // same source line — `.2+|food .2+|apple |10`. After naïve `|`
            // split that becomes `[".2+", "food .2+", "apple ", "10"]` and the
            // trailing `.2+` is stuck in the previous cell's content. Walk
            // adjacent pair-wise: if part[i-1] ends with a whitespace-
            // separated valid CellSpecifier token, strip it from part[i-1]
            // and prepend it to part[i] so the normal per-cell prefix parser
            // picks it up below.
            //
            // GATE — two legitimate shapes fire recovery:
            //
            // (1) Line starts with a SPEC (parts[0] is itself a parseable cell
            //     spec — the canonical `.2+|food .2+|apple` shape).
            // (2) Line starts with the CELL DELIMITER `|` (parts[0]="") AND the
            //     trailing-anchor candidate contains `.` (i.e. is `.N+` or
            //     `N.M+`). This handles multi-line row layouts where an anchor
            //     cell sits on its own line and subsequent `|cell`-prefixed
            //     lines complete the row:
            //         .2+|food
            //         |apple .2+|10
            //         |banana
            //     Without (2), the second line's `.2+` stays stuck in apple's
            //     content and the 10 cell loses its rowspan. The `.`-required
            //     filter rejects natural-text false positives like
            //     `|rated 5*|next`, `|5G coverage 2+|GB`, `|food 1.5+ items|x`
            //     (since `rfind(whitespace)` lands on the LAST token before
            //     `|`, which in those cases is `5*`, `2+`, or `items` — none
            //     containing `.` adjacent to a `+`).
            let p0_trimmed = if matches!(separator, "|" | "!") {
                parts[0].content.trim()
            } else {
                ""
            };
            let line_starts_with_spec = if matches!(separator, "|" | "!") {
                if p0_trimmed.is_empty() {
                    false
                } else {
                    let (_, spec_len) =
                        CellSpecifier::parse(p0_trimmed, ParseContext::FirstPart);
                    spec_len > 0 && spec_len == p0_trimmed.len()
                }
            } else {
                false
            };
            let multi_line_continuation =
                matches!(separator, "|" | "!") && p0_trimmed.is_empty();
            if matches!(separator, "|" | "!")
                && parts.len() > 1
                && (line_starts_with_spec || multi_line_continuation)
            {
                for i in 1..parts.len() {
                    let (left_slice, right_slice) = parts.split_at_mut(i);
                    let prev = &mut left_slice[i - 1];
                    let cur = &mut right_slice[0];
                    let trimmed_end = prev.content.trim_end();
                    let Some(last_ws) = trimmed_end.rfind(|c: char| c.is_whitespace()) else {
                        continue;
                    };
                    // `rfind` returns the byte index of the START of the whitespace
                    // char. For multi-byte whitespace (e.g. U+00A0 NBSP = 2 bytes,
                    // U+3000 ideographic space = 3 bytes) `last_ws + 1` would land
                    // mid-codepoint and the slice would panic. Advance past the
                    // entire whitespace char by its actual UTF-8 length.
                    let ws_char_len = trimmed_end[last_ws..]
                        .chars()
                        .next()
                        .map_or(1, char::len_utf8);
                    let candidate_start = last_ws + ws_char_len;
                    let candidate = &trimmed_end[candidate_start..];
                    // Defensive filters for natural-text false positives apply
                    // ONLY in multi_line_continuation mode (parts[0] empty —
                    // line starts with bare `|`). There, requiring `+`/`*`
                    // AND `.` rules out things like "rated 5*", "5G coverage
                    // 2+", "version 1.5" being misread as cell specifiers.
                    //
                    // In line_starts_with_spec mode (this row's first part
                    // IS already a recognized spec, e.g. `a|`, `2+|`,
                    // `.2+|`), the row's intent is cell-spec usage already
                    // established. Bare style letters as inline candidates
                    // are legitimate per Asciidoctor's PSV inline-spec
                    // grammar — e.g., `a| cell1 a| cell2 a| cell3`. The
                    // CellSpecifier::parse(..., FirstPart) check below is
                    // strict enough on its own.
                    if multi_line_continuation {
                        if !candidate.contains('+') && !candidate.contains('*') {
                            continue;
                        }
                        if !candidate.contains('.') {
                            continue;
                        }
                    }
                    let (spec, spec_len) = CellSpecifier::parse(candidate, ParseContext::FirstPart);
                    if spec_len == 0 || spec_len != candidate.len() {
                        continue;
                    }
                    // Bare style letters (no span operator, no halign/valign)
                    // can't survive a prepend-then-reparse round trip because
                    // the per-part pass uses InlineContent grammar (which
                    // rejects style-only specifiers to avoid "another" → 'a'
                    // false positives). Stash the recognized style on the
                    // CellPart so the per-part pass applies it directly.
                    //
                    // Span/duplication specs (`2+`, `3*`, etc.) still go via
                    // prepend because InlineContent grammar DOES accept them
                    // (has_span_or_dup branch in CellSpecifier::parse).
                    // Recovery applies to BOTH style-only (`a|`) AND span/dup
                    // (`2+|`, `.2+|`, `2*|`) candidates uniformly: the
                    // recognized CellSpecifier is stashed on CUR.forced_spec
                    // and PREV's tail is trimmed to excise the candidate.
                    // CUR.content + CUR.start are LEFT ALONE — preserves the
                    // contiguous byte-position mapping that downstream
                    // cell_start math depends on.
                    //
                    // Old behavior was bifurcated:
                    //   - style-only: cur.content was trim_start'd → off-by-1
                    //     (pre-fix is_style_only_recovery bug)
                    //   - span/dup: cur.content = format!("{cand} {trimmed}")
                    //     + cur.start.saturating_sub(cur_start_shift) → the
                    //     synthesized " " between cand and trimmed collapses
                    //     the removed `|` separator into 1 byte, off-by-1 in
                    //     downstream cell_start (pre-fix span_dup_recovery
                    //     bug). Both fixed uniformly here.
                    let new_prev_len = last_ws;
                    let new_prev = prev.content[..new_prev_len].trim_end().to_string();
                    prev.content = new_prev;
                    cur.forced_spec = Some(spec);
                }
            }

            // Handle span specifier at the start of line (before first separator)
            // e.g., "2+| content" -> part 0 is "2+", applies to part 1
            let mut pending_spec: Option<CellSpecifier> = None;

            // Determine if first part should be treated as content or specifier/skip
            // For PSV (|): first part is before the leading separator, skip it or treat as specifier
            // For CSV (,) and DSV (:): first part is actual cell content

            for (i, part) in parts.iter().enumerate() {
                if i == 0 && matches!(separator, "|" | "!") {
                    // First part is before first separator (PSV format only)
                    let trimmed = part.content.trim();
                    if !trimmed.is_empty() {
                        // Check if this looks like a specifier (e.g., "2+", "3*", "^.>", "s")
                        // Style-only specifiers (e.g., "s" for strong) are valid here
                        let (spec, spec_len) =
                            CellSpecifier::parse(trimmed, ParseContext::FirstPart);
                        if spec_len > 0 && spec_len == trimmed.len() {
                            // Entire first part is a specifier, apply to next cell
                            pending_spec = Some(spec);
                        }
                        // If not a complete specifier, it's just content before first separator
                        // which we skip for PSV
                    }
                    continue;
                }

                let cell_content_trimmed = part.content.trim();

                // Use pending specifier if we have one, otherwise parse from content.
                // Style-only specifiers are NOT valid from inline content parsing -
                // this prevents treating content like "another" as having an 'a' (AsciiDoc) style.
                //
                // `forced_spec` (set by the inline-spec recovery pass for
                // bare-spec cases like `a| cell1 a| cell2` or `2+|`) bypasses
                // both pending_spec and InlineContent parse — the recovery
                // pass already validated the spec under FirstPart grammar
                // AND stashed it without mutating CUR.content / CUR.start,
                // so downstream byte-position math stays contiguous.
                let (spec, spec_offset) = if let Some(forced) = part.forced_spec.clone() {
                    (forced, 0)
                } else if let Some(pending) = pending_spec.take() {
                    (pending, 0)
                } else {
                    CellSpecifier::parse(cell_content_trimmed, ParseContext::InlineContent)
                };

                // The actual cell content starts after the specifier
                let cell_content = if spec_offset > 0 {
                    cell_content_trimmed
                        .get(spec_offset..)
                        .unwrap_or("")
                        .trim_start()
                } else {
                    cell_content_trimmed
                };

                // Calculate where cell_content starts within part.content
                // Pattern: leading_ws + spec_offset + post_spec_ws
                let leading_ws = part.content.len() - part.content.trim_start().len();
                let post_spec_ws = if spec_offset > 0 {
                    let after_spec = cell_content_trimmed.get(spec_offset..).unwrap_or("");
                    after_spec.len() - after_spec.trim_start().len()
                } else {
                    0
                };
                let content_start_offset = leading_ws + spec_offset + post_spec_ws;

                // Calculate positions using actual content boundaries
                let cell_start = current_offset + part.start + content_start_offset;
                let cell_end = if cell_content.is_empty() {
                    cell_start
                } else {
                    // End is start + content length - 1 (inclusive end position)
                    cell_start + cell_content.len().saturating_sub(1)
                };

                columns.push(ParsedCell {
                    content: cell_content.to_string(),
                    start: cell_start,
                    content_start: cell_start,
                    end: cell_end,
                    colspan: spec.colspan,
                    rowspan: spec.rowspan,
                    halign: spec.halign,
                    valign: spec.valign,
                    style: spec.style,
                    is_duplication: spec.is_duplication,
                    duplication_count: spec.duplication_count,
                });
            }

            current_offset += line.len() + 1; // +1 for newline
        }

        columns
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn split_escaped_psv_no_escapes() {
        let parts = split_escaped("| cell1 | cell2 |", '|');
        let [p0, p1, p2, p3] = parts.as_slice() else {
            panic!("expected 4 parts, got {}", parts.len());
        };
        assert_eq!(p0.content, "");
        assert_eq!(p1.content, " cell1 ");
        assert_eq!(p2.content, " cell2 ");
        assert_eq!(p3.content, "");
    }

    #[test]
    fn split_escaped_psv_with_escape() {
        let parts = split_escaped(r"| cell with \| pipe | normal |", '|');
        let [p0, p1, p2, p3] = parts.as_slice() else {
            panic!("expected 4 parts, got {}", parts.len());
        };
        assert_eq!(p0.content, "");
        assert_eq!(p1.content, " cell with | pipe ");
        assert_eq!(p2.content, " normal ");
        assert_eq!(p3.content, "");
    }

    #[test]
    fn split_escaped_dsv_no_escapes() {
        let parts = split_escaped("cell1:cell2:cell3", ':');
        let [p0, p1, p2] = parts.as_slice() else {
            panic!("expected 3 parts, got {}", parts.len());
        };
        assert_eq!(p0.content, "cell1");
        assert_eq!(p1.content, "cell2");
        assert_eq!(p2.content, "cell3");
    }

    #[test]
    fn split_escaped_dsv_with_escape() {
        let parts = split_escaped(r"cell with \: colon:normal", ':');
        let [p0, p1] = parts.as_slice() else {
            panic!("expected 2 parts, got {}", parts.len());
        };
        assert_eq!(p0.content, "cell with : colon");
        assert_eq!(p1.content, "normal");
    }

    #[test]
    fn split_escaped_backslash_not_before_separator() {
        // Backslash before non-separator should be preserved
        let parts = split_escaped(r"cell\n with backslash|next", '|');
        let [p0, p1] = parts.as_slice() else {
            panic!("expected 2 parts, got {}", parts.len());
        };
        assert_eq!(p0.content, r"cell\n with backslash");
        assert_eq!(p1.content, "next");
    }

    #[test]
    fn split_escaped_multiple_escapes() {
        let parts = split_escaped(r"\|start\|middle\|end", '|');
        let [p0] = parts.as_slice() else {
            panic!("expected 1 part, got {}", parts.len());
        };
        assert_eq!(p0.content, "|start|middle|end");
    }

    #[test]
    fn split_escaped_positions_tracked() {
        let parts = split_escaped("ab|cd|ef", '|');
        let [p0, p1, p2] = parts.as_slice() else {
            panic!("expected 3 parts, got {}", parts.len());
        };
        assert_eq!(p0.start, 0);
        assert_eq!(p1.start, 3); // after "ab|"
        assert_eq!(p2.start, 6); // after "ab|cd|"
    }

    /// Trailing content after a completed row — no leading separator — is
    /// attached to the last cell as a continuation paragraph. The blank
    /// line between the row and the trailing text must survive into the
    /// cell's string content so downstream rendering produces a second
    /// paragraph (matching asciidoctor's `<p class="tableblock">...</p>`
    /// pair).
    #[test]
    fn trailing_text_becomes_continuation_paragraph_of_last_cell() {
        let input = "| A | B\n\nTrailing\n";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(input, "|", &mut has_header, 0, None);
        let [row] = rows.as_slice() else {
            panic!("expected 1 row, got {}", rows.len());
        };
        let [a, b] = row.as_slice() else {
            panic!("expected 2 cells, got {}", row.len());
        };
        assert_eq!(a.content, "A");
        // The blank line boundary must be preserved so the cell content,
        // when later parsed as blocks, yields a second paragraph rather
        // than a single joined line.
        assert_eq!(b.content, "B\n\nTrailing");
    }

    /// The outer parse must not collapse a blank line that lives *inside*
    /// an `a`-cell's content. If it did, a nested table's own trailing
    /// continuation paragraph would disappear when the cell is re-parsed
    /// as `AsciiDoc` blocks.
    #[test]
    fn a_cell_preserves_blank_line_inside_nested_table_content() {
        let input = "a|\n!===\n! Inner A ! Inner B\n\nTrailing in inner cell\n!===\n";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(input, "|", &mut has_header, 0, Some(1));
        let [row] = rows.as_slice() else {
            panic!("expected 1 row, got {}", rows.len());
        };
        let [cell] = row.as_slice() else {
            panic!("expected 1 cell, got {}", row.len());
        };
        assert_eq!(
            cell.content,
            "!===\n! Inner A ! Inner B\n\nTrailing in inner cell\n!===",
        );
    }

    /// Adjacent-anchor recovery must not panic when the whitespace separating
    /// the two specifiers is a multi-byte codepoint (NBSP U+00A0, ideographic
    /// space U+3000, etc.). `rfind` returns the byte START of the whitespace
    /// char; advancing by 1 byte instead of `char::len_utf8` would land
    /// mid-codepoint and trigger a UTF-8 slice panic.
    #[test]
    fn adjacent_anchor_recovery_handles_multibyte_whitespace() {
        // U+00A0 (NBSP) between `food` and the second specifier. The leading
        // `|` is required for recovery to fire from row index 1 (i=1 candidate
        // = the trailing token of `food\u{00A0}.2+`); without it the test
        // would not exercise the same path as the canonical recovery case.
        // Note: `Table::parse_rows_with_positions` is called via the PSV
        // separator path which expects the row not to start with the spec —
        // we strip the leading `|` to match production input shape (one cell
        // per `|`-prefixed group).
        let input = ".2+|food\u{00A0}.2+|apple |10\n";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(input, "|", &mut has_header, 0, None);
        // Tightened from `!rows.is_empty()` — multibyte regression silently
        // dropped the trailing `.2+`, leaving 2 cells with rowspan=1.
        assert_eq!(rows[0].len(), 3, "expected 3 cells: food, apple, 10");
        assert_eq!(rows[0][0].content, "food");
        assert_eq!(rows[0][0].rowspan, 2);
        assert_eq!(rows[0][1].content, "apple");
        assert_eq!(rows[0][1].rowspan, 2);
    }

    /// Recovery must NOT fire when the trailing token looks like an anchor
    /// specifier syntactically but is plain text — e.g. `5*` (a star rating)
    /// or `2+` (a version string). False-positive recovery here corrupts
    /// natural-language content: `rated 5*` becomes `rated` + duplicate-5
    /// `next`, silently inflating cells.
    #[test]
    fn no_recovery_on_star_rating_in_text() {
        let input = "|rated 5*|next\n";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(input, "|", &mut has_header, 0, None);
        let row = &rows[0];
        assert_eq!(
            row.first().map(|c| c.content.as_str()),
            Some("rated 5*"),
            "must not steal `5*` from text content",
        );
        assert!(
            !row.get(1).is_some_and(|c| c.is_duplication),
            "next cell must not be duplicated"
        );
    }

    #[test]
    fn no_recovery_on_version_number_in_text() {
        let input = "|5G coverage 2+|GB\n";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(input, "|", &mut has_header, 0, None);
        assert_eq!(
            rows[0].first().map(|c| c.content.as_str()),
            Some("5G coverage 2+"),
        );
        assert_eq!(
            rows[0].get(1).map(|c| c.colspan),
            Some(1),
            "next cell colspan must stay 1",
        );
    }

    /// Recovery MUST still fire on the legitimate two-anchor-per-line shape —
    /// `.N+|x .N+|y` — because that's the entire reason this code path exists.
    /// Legitimate AD rowspan syntax has NO leading `|` (the prefix `.N+` opens
    /// the line directly, and its `|` is the cell delimiter).
    #[test]
    fn recovery_still_fires_on_legitimate_double_anchor() {
        let input = ".2+|food .2+|apple |10\n";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(input, "|", &mut has_header, 0, None);
        // Expect three cells: food (rs=2), apple (rs=2), 10.
        assert_eq!(rows[0].len(), 3, "expected 3 cells: food, apple, 10");
        assert_eq!(rows[0][0].content, "food");
        assert_eq!(rows[0][0].rowspan, 2);
        assert_eq!(rows[0][1].content, "apple");
        assert_eq!(rows[0][1].rowspan, 2);
    }

    /// Multi-line layout: an anchor cell on its own line followed by `|cell`
    /// lines that complete the row. AsciiDoc spec allows this; `is_new_row_start`
    /// only fires on `.N+|`-style anchored lines, so without ncols-aware row
    /// completion detection, all `|cell` lines get bundled into the anchor
    /// row → over-full row → downstream layout corruption.
    ///
    /// Input shape (3-col table):
    ///   .2+|food
    ///   |apple .2+|10
    ///   |banana
    ///   .3+|drink |cola |30
    ///   |tea |15
    ///   |coffee |12
    ///
    /// Expected: 5 rows. Row 0: food(rs=2), apple, 10(rs=2). Row 1: banana
    /// (col 1 only; col 0/2 are phantoms from rowspan). Row 2: drink(rs=3),
    /// cola, 30. Row 3: tea, 15. Row 4: coffee, 12.
    #[test]
    fn multi_line_anchor_cell_with_ncols() {
        let input = ".2+|food\n|apple .2+|10\n|banana\n.3+|drink |cola |30\n|tea |15\n|coffee |12\n";
        let mut has_header = false;
        let rows =
            Table::parse_rows_with_positions(input, "|", &mut has_header, 0, Some(3));
        // The food half must be present (this is the regression — without the
        // multi-line layout fix, all the food/apple/10/banana cells get
        // bundled into one over-full row and the visible table loses food).
        assert!(rows.len() >= 2, "expected at least 2 rows, got {}", rows.len());
        // Row 0: food (rs=2, colspan=1), apple (cs=1), 10 (rs=2, cs=1).
        assert_eq!(rows[0].len(), 3, "row 0 should have 3 cells: food, apple, 10");
        assert_eq!(rows[0][0].content, "food");
        assert_eq!(rows[0][0].rowspan, 2);
        assert_eq!(rows[0][1].content, "apple");
        assert_eq!(rows[0][2].content, "10");
        assert_eq!(rows[0][2].rowspan, 2);
        // Row 1: just banana (col 1; col 0 and col 2 are phantoms).
        assert_eq!(rows[1].len(), 1, "row 1 should have 1 cell: banana");
        assert_eq!(rows[1][0].content, "banana");
    }

    /// Inline `a|` cell-style after first cell on a row.
    ///
    /// Asciidoctor's PSV grammar accepts `a|` (and other bare style letters)
    /// as inline cell-spec markers after whitespace, not just at the start
    /// of a line. Pre-fix, acdc rejected style-only candidates without `+`
    /// or `*` everywhere → only the first `a|` was recognized; subsequent
    /// `a|` separators on the same row were silently dropped to plain `|`
    /// (with the `a` character leaking into the previous cell's content).
    ///
    /// Post-fix: the `+`/`*` defensive filter only applies in
    /// `multi_line_continuation` mode (rows starting with bare `|`). Rows
    /// that already begin with a recognized spec (`a|`, `2+|`, etc.) trust
    /// the strict CellSpecifier::parse check to accept bare style letters.
    #[test]
    fn inline_a_cell_style_after_first_cell() {
        // 3 cells on one row, each prefixed with `a|`. Each cell should be
        // asciidoc-styled; the cell contents should NOT have trailing "a"
        // leaked in from the next cell's spec.
        let input = "a| first a| second a| third\n";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(input, "|", &mut has_header, 0, None);
        assert_eq!(rows.len(), 1, "expected 1 row");
        assert_eq!(rows[0].len(), 3, "expected 3 cells, got {}", rows[0].len());
        assert_eq!(rows[0][0].content.trim(), "first");
        assert_eq!(rows[0][1].content.trim(), "second");
        assert_eq!(rows[0][2].content.trim(), "third");
        for (i, cell) in rows[0].iter().enumerate() {
            assert_eq!(
                cell.style,
                Some(ColumnStyle::AsciiDoc),
                "cell {i} must have AsciiDoc style",
            );
        }
    }

    /// Regression guard — false-positive defenses still fire in
    /// `multi_line_continuation` mode (line starts with bare `|`). The
    /// existing `no_recovery_on_star_rating_in_text` test covers this for
    /// `5*`; this guards `a` specifically (the style letter most likely to
    /// appear at the end of natural English text). Pre-fix relied on
    /// missing `+`/`*` to reject `a`; post-fix relies on
    /// `multi_line_continuation` predicate.
    #[test]
    fn no_recovery_on_trailing_a_in_multiline_continuation_text() {
        let input = "|some text ending in a|next\n";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(input, "|", &mut has_header, 0, None);
        assert_eq!(
            rows[0].first().map(|c| c.content.as_str()),
            Some("some text ending in a"),
            "natural-text trailing `a` must NOT be stolen as next cell's style",
        );
        // Next cell must be plain (no style applied).
        assert!(
            rows[0].get(1).is_some_and(|c| c.style.is_none()),
            "next cell must have no style (a was content, not spec)",
        );
    }

    /// `count_unescaped_separators` helper: `\X` skips the next char, so
    /// `\|` (escaped pipe) does NOT count as a separator. This is the
    /// primitive used by `handle_cross_row_continuation` to decide whether
    /// a continuation line is "still part of the previous row" vs "starts
    /// a new row". Without escape-handling, lines containing `\| Col1 \|
    /// Col2 \|` (a markdown-style listing-block header inside an `a|`
    /// cell) would break continuation and split the listing's body off
    /// into a phantom row.
    #[test]
    fn count_unescaped_separators_skips_escaped_pipe() {
        assert_eq!(
            count_unescaped_separators("\\| Col1 \\| Col2 \\|", "|"),
            0,
            "all 3 pipes are escaped — must count 0",
        );
        assert_eq!(
            count_unescaped_separators("| real | sep |", "|"),
            3,
            "all 3 pipes unescaped — must count 3",
        );
        assert_eq!(
            count_unescaped_separators("\\| escaped | unescaped", "|"),
            1,
            "first pipe escaped, second not — must count 1",
        );
        // Same for bang separator (variant for nested tables).
        assert_eq!(
            count_unescaped_separators("\\! nested \\!", "!"),
            0,
            "bang separator escape must also work",
        );
    }

    /// `count_cell_colspans` must NOT over-count trailing empty cell as a
    /// continuation cell when the previous part's content is PLAIN TEXT
    /// happening to end with a letter that looks like a style char.
    ///
    /// Bug: pre-fix `count_cell_colspans` checked `prev.trim_end().ends_with(<style letter>)`
    /// — which fires for ANY content ending with `a|s|m|l|v|e|h|d`, even
    /// when the prev part is plain prose like `" m"` (not a spec).
    /// `parse_row_with_positions` correctly parses such input as 3 cells
    /// (no spec), but `count_cell_colspans` returns 4 — mismatched
    /// accumulated_cols causes premature row-boundary break in multi-line
    /// row collection.
    ///
    /// Fix: the heuristic only fires when `total_parts == 2` (a STANDALONE
    /// spec line like `a|` with no other cells). Multi-cell lines have a
    /// trailing `|` that is just a delimiter, never a continuation cell.
    #[test]
    fn count_cell_colspans_does_not_overcount_plain_m_at_line_end() {
        // Input "a| b| m|" — 3 cells via parse_row_with_positions ("b",
        // "m", and one empty continuation). count_cell_colspans must
        // return 3 (matching parse_row, NOT 4 from over-firing heuristic).
        let line = "a| b| m|";
        let n = count_cell_colspans(line, "|");
        assert_eq!(
            n, 3,
            "trailing `m|` is plain content + delimiter, not continuation cell; \
             over-count by 1 breaks multi-line row collection (got {n})",
        );
    }

    /// Sanity baseline — standalone-spec line `a|` MUST still be counted
    /// as 1 continuation cell (this is the legitimate use case the
    /// heuristic was originally designed for).
    #[test]
    fn count_cell_colspans_counts_standalone_a_pipe_as_one() {
        let n = count_cell_colspans("a|", "|");
        assert_eq!(n, 1, "standalone `a|` is a continuation cell (got {n})");
    }

    /// Sanity baseline — `|2+ a|` (inline span+style spec, standalone)
    /// counts as colspan-weighted 2 + 1 trailing continuation = 3,
    /// MATCHING `parse_row_with_positions` which produces 2 cells with
    /// `colspan=2` and `colspan=1` (total columns spanned = 3).
    #[test]
    fn count_cell_colspans_matches_parse_row_for_inline_span_style() {
        let line = "|2+ a|";
        let count = count_cell_colspans(line, "|");
        // parse_row produces 2 cells: [colspan=2 empty content, colspan=1
        // empty content]. Sum of colspans = 3 → count_cell_colspans must
        // also report 3 to keep accumulated_cols in sync with parse_row.
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(line, "|", &mut has_header, 0, None);
        let parsed_colspan_sum: usize =
            rows[0].iter().map(|c| c.colspan).sum();
        assert_eq!(
            count, parsed_colspan_sum,
            "count_cell_colspans({line:?}) must equal sum of parse_row colspans \
             (got count={count}, parse_row sum={parsed_colspan_sum})"
        );
        assert_eq!(count, 3);
    }

    /// Inline-spec recovery SPAN/DUP branch byte-offset correctness.
    ///
    /// When the recovery loop encounters a span/duplication candidate like
    /// `2+`, `3*`, `.2+`, `2.3+` between cells, the existing implementation
    /// prepends the candidate to CUR.content and shifts CUR.start back by
    /// `cur_start_shift` (the byte count excised from PREV's tail).
    ///
    /// The synthesis: cur.content = "{candidate} {cur.content.trim_start()}"
    /// turns cur into a string whose source-byte mapping is NON-CONTIGUOUS
    /// (the synthesized space between candidate and content "collapses" the
    /// removed `|` separator and the original leading space into one byte).
    ///
    /// Concrete failure:
    ///   input = "a| first 2+| second"
    ///   parts[2] originally: {content:" second", start:12}
    ///   After recovery:      {content:"2+ second", start:9}
    ///   Downstream: cell_start = part.start(9) + content_start_offset(3) = 12
    ///   TRUE position of 's' in original line = byte 13
    ///   OFF BY 1.
    ///
    /// This test PINS the correct byte offset. Pre-fix asserts FAIL.
    #[test]
    fn span_dup_recovery_preserves_cur_start_byte_offset() {
        let input = "a| first 2+| second";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            input,
            "|",
            &mut has_header,
            /* base_offset */ 0,
            None,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].len(),
            2,
            "expected 2 cells (after 'a' row spec), got {}",
            rows[0].len()
        );
        // Cell 0: "first" content_start = byte 3 (where 'f' is).
        let c0 = &rows[0][0];
        assert_eq!(c0.content.trim(), "first");
        assert_eq!(c0.start, 3, "cell 0 'first' start must be byte 3");
        // Cell 1: "second" with colspan=2. content_start = byte 13 (where 's' is).
        let c1 = &rows[0][1];
        assert_eq!(c1.content.trim(), "second");
        assert_eq!(
            c1.colspan, 2,
            "cell 1 must have colspan=2 (from inline '2+' spec)"
        );
        assert_eq!(
            c1.start, 13,
            "cell 1 'second' content_start must be byte 13 (was {} — \
             span/dup recovery off-by-one bug shifts onto synthesized \
             separator)",
            c1.start,
        );
    }

    /// Inline-spec recovery `is_style_only` branch must NOT shift CUR.start.
    ///
    /// Regression: a previous version of the recovery loop subtracted
    /// `cur_start_shift` (the bytes excised from PREV's tail) from CUR.start
    /// in BOTH the prepend path AND the style-only path. In the style-only
    /// path that produced a CUR.start pointing onto the spec letter's byte
    /// in PREV's span — corrupting source-position mapping by the
    /// excised-WS+letter byte count.
    ///
    /// Concrete failure (pre-fix):
    ///   input = "a| first a| second"  (line-relative bytes)
    ///   parts[2] (cell "second"):
    ///     SOURCE-correct cell_start = 12 ('s' of "second")
    ///     PRE-fix      cell_start  = 9  ('a' of " a" in PREV's span)  ← BUG
    ///     POST-fix     cell_start  = 12 ✓
    #[test]
    fn is_style_only_recovery_preserves_cur_start_byte_offset() {
        let input = "a| first a| second";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            input,
            "|",
            &mut has_header,
            /* base_offset */ 0,
            None,
        );
        assert_eq!(rows.len(), 1, "expected 1 row");
        assert_eq!(rows[0].len(), 2, "expected 2 cells, got {}", rows[0].len());

        // Cell 0: "first" — content starts at byte 3 in the line.
        // (`a| first a| second`
        //   0123456789012345678 — 'f' at index 3)
        let c0 = &rows[0][0];
        assert_eq!(c0.content.trim(), "first");
        assert_eq!(
            c0.start, 3,
            "cell 0 'first' content_start must be byte 3 (was {})",
            c0.start,
        );

        // Cell 1: "second" — content starts at byte 12 in the line.
        // Pre-fix this was 9 (pointed at the spec letter 'a' in PREV's span).
        let c1 = &rows[0][1];
        assert_eq!(c1.content.trim(), "second");
        assert_eq!(
            c1.start, 12,
            "cell 1 'second' content_start must be byte 12 (was {} — \
             pre-fix bug shifted onto PREV's spec-letter byte)",
            c1.start,
        );
        // Both cells must carry AsciiDoc style (one from row spec, one from
        // recovery-stashed forced_style).
        assert_eq!(c0.style, Some(ColumnStyle::AsciiDoc));
        assert_eq!(c1.style, Some(ColumnStyle::AsciiDoc));
    }
}
