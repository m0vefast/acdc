use crate::{ColumnStyle, HorizontalAlignment, Table, VerticalAlignment};

pub(crate) const MAX_TABLE_COLUMNS: usize = 100;
const MAX_TABLE_ROWS: usize = 1_000;

/// A cell part with its unescaped content and original start position.
struct CellPart {
    /// Unescaped content (e.g., `\|` becomes `|`)
    content: String,
    /// Start position in the original line
    start: usize,
    /// Content offsets each preceded by one source byte the splitter removed.
    removed: Vec<usize>,
}

impl CellPart {
    fn plain(content: String, start: usize) -> Self {
        Self {
            content,
            start,
            removed: Vec::new(),
        }
    }
}

/// Trim `kept`, a prefix of a part's content whose first byte stands at
/// `part_source`, into a cell; its removals move with the trimmed content.
fn trimmed_cell(
    kept: &str,
    removed: &[usize],
    part_source: usize,
    start: Option<usize>,
    spec: CellSpecifier,
) -> ParsedCell {
    let leading = kept.len() - kept.trim_start().len();
    let content = kept.trim();
    // asciidoctor `Table::Cell` literal: `rstrip`, then drop leading line feeds only.
    let lead = if content.is_empty() {
        0
    } else {
        kept[..leading].trim_start_matches('\n').len()
    };
    let content_start =
        part_source + leading + removed.iter().take_while(|&&at| at < leading).count();
    let removed: Vec<usize> = removed
        .iter()
        .filter(|&&at| at >= leading && at < leading + content.len())
        .map(|at| at - leading)
        .collect();
    let end = match content.len().checked_sub(1) {
        Some(last) => content_start + last + removed.iter().take_while(|&&at| at <= last).count(),
        None => content_start,
    };
    ParsedCell {
        content: content.to_string(),
        start: start.unwrap_or(content_start),
        content_start,
        end,
        removed,
        spec,
        lead,
    }
}

/// Split a line by separator, respecting backslash escapes.
///
/// For PSV (`|`) and DSV (`:`), a backslash before the separator escapes it.
/// Returns parts with their original byte positions for accurate source mapping.
fn split_escaped(line: &str, separator: char) -> Vec<CellPart> {
    let mut parts = Vec::new();
    let mut current = CellPart::plain(String::new(), 0);
    let mut chars = line.char_indices().peekable();

    while let Some((byte_idx, ch)) = chars.next() {
        if ch == '\\' {
            // Check if next char is the separator
            if let Some(&(_, next_ch)) = chars.peek()
                && next_ch == separator
            {
                current.removed.push(current.content.len());
                current.content.push(separator);
                chars.next();
                continue;
            }
            // Not an escape - add backslash literally
            current.content.push(ch);
        } else if ch == separator {
            // Unescaped separator - end current part
            let next = CellPart::plain(String::new(), byte_idx + ch.len_utf8());
            parts.push(std::mem::replace(&mut current, next));
        } else {
            current.content.push(ch);
        }
    }

    // Add final part
    parts.push(current);

    parts
}

/// Split a single line into cell parts for line-per-row formats (DSV, TSV, or a
/// custom multi-character separator). Single-character separators honor `\`
/// escapes; multi-character separators are matched literally.
fn split_row_line(line: &str, separator: &str) -> Vec<CellPart> {
    match separator.chars().next() {
        Some(sep_char) if separator.len() == 1 => split_escaped(line, sep_char),
        Some(_) => split_multi_char(line, separator),
        None => vec![CellPart::plain(line.to_string(), 0)],
    }
}

/// Split by a multi-character separator (no escape handling).
fn split_multi_char(line: &str, separator: &str) -> Vec<CellPart> {
    let mut parts = Vec::new();
    let mut last_end = 0;
    for (idx, _) in line.match_indices(separator) {
        parts.push(CellPart::plain(
            line.get(last_end..idx).unwrap_or("").to_string(),
            last_end,
        ));
        last_end = idx + separator.len();
    }
    parts.push(CellPart::plain(
        line.get(last_end..).unwrap_or("").to_string(),
        last_end,
    ));
    parts
}

/// Parse a CSV-style table body using the `csv` crate for full RFC 4180
/// compliance. Used for both CSV (`,`) and TSV (`\t`) — `delimiter` selects
/// which.
///
/// This handles multi-line quoted values, escaped quotes, and all CSV edge cases.
/// Returns rows with cells containing their content and accurate byte positions.
fn parse_csv_table(text: &str, base_offset: usize, delimiter: u8) -> Vec<Vec<CellPart>> {
    let text_bytes = text.as_bytes();
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true) // allow variable column counts
        .delimiter(delimiter)
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
                find_csv_field_position(text_bytes, scan_pos, field, delimiter);

            cells.push(CellPart {
                content: field.to_string(),
                start: base_offset + field_content_start,
                removed: if field_content_start > scan_pos {
                    doubled_quotes(text_bytes, field_content_start)
                } else {
                    Vec::new()
                },
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
fn find_csv_field_position(
    text: &[u8],
    start: usize,
    expected_content: &str,
    delimiter: u8,
) -> (usize, usize) {
    let Some(&first_byte) = text.get(start) else {
        return (start, start);
    };

    if first_byte == b'"' {
        // Quoted field: content starts after the opening quote
        let content_start = start + 1;
        // Find the closing quote (handle escaped quotes "")
        let end_pos = find_closing_quote(text, start + 1);
        // Next field starts after closing quote and delimiter (or newline)
        let next_pos = skip_to_next_field(text, end_pos, delimiter);
        (content_start, next_pos)
    } else {
        // Unquoted field: content starts at current position
        let content_start = start;
        // Find end of field (delimiter or newline)
        let end_pos = find_unquoted_field_end(text, start, expected_content.len(), delimiter);
        // Next field starts after the separator
        let next_pos = skip_to_next_field(text, end_pos, delimiter);
        (content_start, next_pos)
    }
}

/// Content offsets of a quoted field where `""` collapsed to `"`.
fn doubled_quotes(text: &[u8], content_start: usize) -> Vec<usize> {
    let mut removed = Vec::new();
    let (mut pos, mut local) = (content_start, 0);
    while let Some(&byte) = text.get(pos) {
        if byte == b'"' {
            if text.get(pos + 1) != Some(&b'"') {
                break;
            }
            removed.push(local);
            pos += 1;
        }
        pos += 1;
        local += 1;
    }
    removed
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
fn find_unquoted_field_end(text: &[u8], start: usize, content_len: usize, delimiter: u8) -> usize {
    // The field ends at the delimiter, CR, LF, or content_len bytes (whichever comes first)
    let mut pos = start;
    let mut remaining = content_len;
    while let Some(&byte) = text.get(pos) {
        if byte == delimiter || byte == b'\n' || byte == b'\r' {
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
fn skip_to_next_field(text: &[u8], pos: usize, delimiter: u8) -> usize {
    let mut pos = pos;
    // Skip closing quote if present
    if text.get(pos) == Some(&b'"') {
        pos += 1;
    }
    // Skip the delimiter or newline characters
    while let Some(&byte) = text.get(pos) {
        if byte == delimiter {
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

/// How a table body is read (asciidoctor `Table::ParserContext`). The separator
/// only names the delimiter; it never changes the format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TableFormat {
    /// A stream of cells with specifiers, filled into rows by column count.
    Psv,
    /// RFC 4180 quoting, one record per row (also TSV).
    Csv,
    /// One row per line, no quoting, no specifiers.
    Dsv,
}

/// Represents a parsed cell specifier with span, alignment, and style information.
///
/// In `AsciiDoc`, cell specifiers appear before the cell separator with format:
/// `[colspan][.rowspan][op][halign][valign][style]|`. Alignment without a span
/// can appear directly before the style and separator.
///
/// Examples:
/// - `2+|content` → colspan=2
/// - `.3+|content` → rowspan=3
/// - `2.3+|content` → colspan=2, rowspan=3
/// - `2+^.>s|content` → center, bottom, colspan=2, strong style
/// - `3*|content` → duplicate cell 3 times
#[derive(Debug, Clone, Copy)]
pub(crate) struct CellSpecifier {
    pub(crate) colspan: usize,
    pub(crate) rowspan: usize,
    pub(crate) halign: Option<HorizontalAlignment>,
    pub(crate) valign: Option<VerticalAlignment>,
    pub(crate) style: Option<ColumnStyle>,
    /// If true, this is a duplication specifier (`*`) rather than a span (`+`).
    pub(crate) is_duplication: bool,
    /// For duplication, this is the count (e.g., `3*` means 3 copies).
    pub(crate) duplication_count: usize,
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

/// The style slot takes any lowercase letter; only a known one sets a style
/// (`CellSpecEndRx` `([a-z])?$`, `Parser.parse_cellspec`).
fn style_slot(bytes: &[u8], pos: usize) -> (Option<ColumnStyle>, usize) {
    match bytes.get(pos) {
        Some(&b) if b.is_ascii_lowercase() => (parse_style_byte(b), pos + 1),
        _ => (None, pos),
    }
}

impl CellSpecifier {
    /// Parse a cell specifier from the beginning of cell content.
    ///
    /// Returns the specifier and the offset where actual content begins. A
    /// specifier is only ever recognized as the line-leading token before a
    /// cell delimiter, so style-only specifiers (e.g. `s|` for strong) are
    /// always accepted here.
    /// Full pattern: `[colspan][.rowspan][+|*][halign][valign][style]`
    ///
    /// Examples:
    /// - `"2+rest"` → colspan=2
    /// - `".3+rest"` → rowspan=3
    /// - `"2.3+rest"` → colspan=2, rowspan=3
    /// - `"2+^.>srest"` → center, bottom, colspan=2, strong style
    /// - `"3*rest"` → `duplication_count`=3
    /// - `"plain"` → defaults (no specifier found)
    #[must_use]
    fn parse(content: &str) -> (Self, usize) {
        let bytes = content.as_bytes();
        let mut pos = 0;

        // Parse optional alignment markers
        let (halign, valign, align_end) = Self::parse_alignments(bytes, pos);
        pos = align_end;

        // Parse optional `colspan` (digits)
        let (colspan, colspan_end) = Self::parse_number(content, bytes, pos);
        pos = colspan_end;

        // Parse optional `rowspan` (dot followed by digits)
        let (rowspan, rowspan_end) = Self::parse_rowspan(content, bytes, pos);
        pos = rowspan_end;

        // Check for operator and build result
        Self::build(bytes, pos, colspan, rowspan, halign, valign)
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

    /// Parse `rowspan` (dot followed by digits) at the current position.
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
    fn build(
        bytes: &[u8],
        mut pos: usize,
        colspan: Option<usize>,
        rowspan: Option<usize>,
        halign: Option<HorizontalAlignment>,
        valign: Option<VerticalAlignment>,
    ) -> (Self, usize) {
        let has_span_or_dup = colspan.is_some() || rowspan.is_some();
        let is_duplication = bytes.get(pos) == Some(&b'*');
        let is_span = bytes.get(pos) == Some(&b'+');

        if (is_span || is_duplication) && has_span_or_dup {
            pos += 1;

            // Asciidoctor places alignment after the span operator. Keep
            // accepting the older acdc alignment-first order as well.
            let (after_span_horizontal, after_span_vertical, alignment_end) =
                Self::parse_alignments(bytes, pos);
            pos = alignment_end;
            let halign = after_span_horizontal.or(halign);
            let valign = after_span_vertical.or(valign);

            let (style, style_end) = style_slot(bytes, pos);
            pos = style_end;

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
        } else if let (style, style_end) = style_slot(bytes, pos)
            && (halign.is_some() || valign.is_some() || style_end > pos)
        {
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
                style_end,
            )
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
#[derive(Clone, Debug)]
pub(crate) struct ParsedCell {
    pub(crate) content: String,
    pub(crate) start: usize,
    pub(crate) content_start: usize,
    pub(crate) end: usize,
    /// Content offsets each preceded by one removed source byte (an escape).
    pub(crate) removed: Vec<usize>,
    pub(crate) spec: CellSpecifier,
    /// Bytes of whitespace just before `content_start` that a literal PSV cell keeps.
    pub(crate) lead: usize,
}

#[derive(Debug)]
pub(crate) struct TableLimitViolation {
    pub(crate) resource: &'static str,
    pub(crate) requested: usize,
    pub(crate) limit: usize,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl TableLimitViolation {
    const fn new(
        resource: &'static str,
        requested: usize,
        limit: usize,
        start: usize,
        end: usize,
    ) -> Self {
        Self {
            resource,
            requested,
            limit,
            start,
            end,
        }
    }
}

/// Check if a blank line after the first row indicates a header.
/// A header is indicated only if the first non-empty line after the blank
/// contains a separator. If it's a continuation line (no separator), it's content
/// that attaches to the previous cell, not a header indicator.
fn detect_header_after_first_row(lines: &[&str], start_idx: usize, format: TableFormat, separator: &str) -> bool {
    for &line in lines.iter().skip(start_idx) {
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            return if format == TableFormat::Psv { opens_cell(trimmed, separator) } else { trimmed.contains(separator) };
        }
    }
    false
}

/// `Parser.parse_cellspec line, :start`: the line opens a cell when the text
/// before its first separator is a cell specifier (or blank). Otherwise it
/// continues the last cell, which cancels the implicit header (`parse_table`).
fn opens_cell(line: &str, separator: &str) -> bool {
    line.split_once(separator).is_some_and(|(before, _)| {
        let spec = before.trim_start_matches([' ', '\t']);
        spec.is_empty() || CellSpecifier::parse(spec).1 == spec.len()
    })
}

/// Split a part's content into `(content_before_spec, trailing_specifier)`.
///
/// A cell specifier (`2+`, `.3+`, `^`, `a`, ...) is recognized as the trailing token
/// immediately before the delimiter that ends the part, provided it is separated from the
/// cell content by whitespace (or a newline, or the start of the table body). This
/// matches asciidoctor: `cell 2+|next` makes `next` span two columns, while `|2+|`, where
/// `2+` sits flush against the opening delimiter, keeps `2+` as literal cell content. A
/// space *between* the specifier and the delimiter (`2+ |`) likewise makes it literal.
///
/// `allow_at_start` lets a specifier flush against the very start of the table
/// body (the leading part, before the first delimiter) be recognized, so
/// `2+|cell` and `a|cell` at the start of the body work.
fn extract_trailing_spec(content: &str, allow_at_start: bool) -> (&str, Option<CellSpecifier>) {
    // The specifier sits immediately before the delimiter: trailing whitespace
    // means the `|` is a plain cell break, not a spec terminator.
    if content.is_empty() || content.ends_with([' ', '\t']) {
        return (content, None);
    }
    // The candidate is the trailing run of non-whitespace characters.
    let token_start = content.rfind([' ', '\t', '\n']).map_or(0, |i| i + 1);
    let candidate = &content[token_start..];
    let (spec, spec_len) = CellSpecifier::parse(candidate);
    if spec_len == 0 || spec_len != candidate.len() {
        return (content, None);
    }
    // Only recognized when separated from the cell content: preceded by
    // whitespace/newline (`token_start > 0`) or sitting at the very start of the
    // table body (`allow_at_start`). A spec flush against the opening delimiter
    // (`|2+|`) is literal content.
    if token_start > 0 || allow_at_start {
        (&content[..token_start], Some(spec))
    } else {
        (content, None)
    }
}

/// Scan a PSV table body into a flat stream of cells.
///
/// The whole body is split on the separator (respecting escapes), so
/// newlines and blank lines are insignificant for cell boundaries — a cell's
/// content runs until the next unescaped delimiter, spanning as many physical
/// lines as needed. This mirrors asciidoctor's PSV model and is what makes
/// multi-line cells and rows split across lines parse correctly.
fn scan_cells(text: &str, separator: &str, base_offset: usize) -> Vec<ParsedCell> {
    let Some(sep_char) = separator.chars().next() else {
        return Vec::new();
    };
    let parts = split_escaped(text, sep_char);
    let last_idx = parts.len().saturating_sub(1);

    let mut cells = Vec::new();
    let mut carried_spec: Option<CellSpecifier> = None;

    for (i, part) in parts.iter().enumerate() {
        // A trailing specifier only exists if another delimiter follows this part.
        let (kept, trailing_spec) = if i < last_idx {
            extract_trailing_spec(&part.content, i == 0)
        } else {
            (part.content.as_str(), None)
        };

        // The leading part (before the first delimiter) is not a cell — only
        // its trailing line-leading specifier matters.
        if i == 0 {
            carried_spec = trailing_spec;
            continue;
        }

        let cell_start = base_offset + part.start;
        cells.push(trimmed_cell(
            kept,
            &part.removed,
            cell_start,
            Some(cell_start),
            carried_spec.take().unwrap_or_default(),
        ));
        carried_spec = trailing_spec;
    }

    cells
}

/// Without `cols`, the columns are the first row's column visits: one per
/// cell, a colspan's extra columns, one per repeat
/// (`Table::ParserContext#close_cell` while `@colcount == -1`). The first row
/// closes when a later line opens a cell, which closes the open cell at its end
/// of line (`close_open_cell`), or at the end of the table.
fn first_row_column_count(cells: &[ParsedCell], text: &str, base_offset: usize, separator: &str) -> usize {
    let separator_len = separator.chars().next().map_or(1, char::len_utf8);
    let opens_line = |cell: &ParsedCell| {
        let delimiter = cell.start - base_offset - separator_len;
        let line_start = text[..delimiter].rfind('\n').map_or(0, |n| n + 1);
        let spec = text[line_start..delimiter].trim_start_matches([' ', '\t']);
        spec.is_empty() || CellSpecifier::parse(spec).1 == spec.len()
    };
    let row_end = cells.iter().skip(1).position(opens_line).map_or(cells.len(), |n| n + 1);
    cells[..row_end]
        .iter()
        .map(|cell| if cell.spec.is_duplication { cell.spec.duplication_count } else { cell.spec.colspan })
        .sum()
}

fn validate_cell_specifiers(cells: &[ParsedCell]) -> Result<(), TableLimitViolation> {
    for cell in cells {
        if cell.spec.colspan > MAX_TABLE_COLUMNS {
            return Err(TableLimitViolation::new(
                "column span",
                cell.spec.colspan,
                MAX_TABLE_COLUMNS,
                cell.start,
                cell.end,
            ));
        }
        if cell.spec.rowspan > MAX_TABLE_ROWS {
            return Err(TableLimitViolation::new(
                "row span",
                cell.spec.rowspan,
                MAX_TABLE_ROWS,
                cell.start,
                cell.end,
            ));
        }
        if cell.spec.duplication_count > MAX_TABLE_COLUMNS {
            return Err(TableLimitViolation::new(
                "cell duplication",
                cell.spec.duplication_count,
                MAX_TABLE_COLUMNS,
                cell.start,
                cell.end,
            ));
        }
    }
    Ok(())
}

fn validate_row_widths(rows: &[Vec<ParsedCell>]) -> Result<(), TableLimitViolation> {
    if rows.len() > MAX_TABLE_ROWS {
        let (start, end) = rows
            .get(MAX_TABLE_ROWS)
            .and_then(|row| row.first().zip(row.last()))
            .map_or((0, 0), |(first, last)| (first.start, last.end));
        return Err(TableLimitViolation::new(
            "row count",
            rows.len(),
            MAX_TABLE_ROWS,
            start,
            end,
        ));
    }
    for row in rows {
        if row.len() > MAX_TABLE_COLUMNS {
            let (start, end) = row
                .first()
                .zip(row.last())
                .map_or((0, 0), |(first, last)| (first.start, last.end));
            return Err(TableLimitViolation::new(
                "column count",
                row.len(),
                MAX_TABLE_COLUMNS,
                start,
                end,
            ));
        }
    }
    Ok(())
}

/// Group a flat cell stream into rows of `ncols` columns, as asciidoctor's
/// `Table::ParserContext#close_cell` does (`table.rb:617-728`).
///
/// Rows are counted, not laid out: each cell adds its colspan to the row's
/// visits, and a `.n+` rowspan adds its colspan to each of the next `n - 1`
/// rows (`@active_rowspans`). A row closes once visits plus the rowspans
/// reaching it equal `ncols`; one that overruns it is dropped whole. A `k*`
/// cell is placed copy by copy (`repeatcol`), so its copies may wrap onto the
/// next row. A trailing row left incomplete is dropped too. A row dropped for
/// overrunning `ncols` stays as an empty row, since the rowspans still pass
/// over it. The inclusive `(start, end)` document offsets of a dropped row are
/// written to `dropped` so the caller can warn about it.
fn group_cells_into_rows(
    cells: Vec<ParsedCell>,
    ncols: usize,
    dropped: &mut Option<(usize, usize)>,
) -> Result<Vec<Vec<ParsedCell>>, TableLimitViolation> {
    let ncols = ncols.max(1);
    let mut rows = Vec::new();
    let mut active: std::collections::VecDeque<usize> = std::collections::VecDeque::from([0]);
    let mut visits = 0;
    let mut row: Vec<ParsedCell> = Vec::new();

    for cell in cells {
        let copies = if cell.spec.is_duplication {
            cell.spec.duplication_count.max(1)
        } else {
            1
        };
        let colspan = cell.spec.colspan.max(1);
        for _ in 0..copies {
            if cell.spec.rowspan > 1 {
                if active.len() < cell.spec.rowspan {
                    active.resize(cell.spec.rowspan, 0);
                }
                for taken in active.iter_mut().skip(1).take(cell.spec.rowspan - 1) {
                    *taken += colspan;
                }
            }
            visits += colspan;
            push_cell_copy(&mut row, &cell);
            let filled = visits + active.front().copied().unwrap_or(0);
            if filled < ncols {
                continue;
            }
            let row_cells = std::mem::take(&mut row);
            if filled == ncols {
                if rows.len() == MAX_TABLE_ROWS {
                    let (start, end) = row_cells
                        .first()
                        .zip(row_cells.last())
                        .map_or((0, 0), |(first, last)| (first.start, last.end));
                    return Err(TableLimitViolation::new(
                        "row count",
                        rows.len() + 1,
                        MAX_TABLE_ROWS,
                        start,
                        end,
                    ));
                }
                rows.push(row_cells);
            } else {
                if dropped.is_none()
                    && let (Some(first), Some(last)) = (row_cells.first(), row_cells.last())
                {
                    *dropped = Some((first.start, last.end));
                }
                // The dropped row still ages the rowspans; an empty row marks it.
                rows.push(Vec::new());
            }
            visits = 0;
            active.pop_front();
            if active.is_empty() {
                active.push_back(0);
            }
        }
    }

    if let (Some(first), Some(last)) = (row.first(), row.last()) {
        *dropped = Some((first.start, last.end));
    }
    Ok(rows)
}

/// Adds one copy of `cell` to `row`: a further copy of a `k*` cell already
/// ending the row widens that entry, so each row holds one entry per source
/// cell and its copies are expanded downstream.
fn push_cell_copy(row: &mut Vec<ParsedCell>, cell: &ParsedCell) {
    if cell.spec.is_duplication
        && let Some(last) = row.last_mut()
        && last.start == cell.start
    {
        last.spec.duplication_count += 1;
        return;
    }
    let mut copy = cell.clone();
    copy.spec.duplication_count = 1;
    row.push(copy);
}

impl Table<'_> {
    pub(crate) fn parse_rows_with_positions(
        text: &str,
        format: TableFormat,
        separator: &str,
        has_header: &mut bool,
        base_offset: usize,
        ncols: Option<usize>,
        dropped: &mut Option<(usize, usize)>,
    ) -> Result<Vec<Vec<ParsedCell>>, TableLimitViolation> {
        // CSV (and TSV) quoted values may span lines. The CSV reader takes a
        // one-byte delimiter; a longer separator is read line-per-row.
        if let (TableFormat::Csv, &[delimiter]) = (format, separator.as_bytes()) {
            let rows =
                Self::parse_csv_rows_with_positions(text, has_header, base_offset, delimiter);
            validate_row_widths(&rows)?;
            return Ok(rows);
        }

        let lines: Vec<&str> = text.lines().collect();

        // Implicit header: the first physical line is immediately followed by a
        // blank line (with a real row after it). A leading blank line means
        // there is no header. We only ever *set* the header here — an explicit
        // `options="header"` from the caller is left untouched.
        if lines.first().is_some_and(|l| l.trim().is_empty()) {
            *has_header = false;
        } else if lines.get(1).is_some_and(|l| l.trim().is_empty())
            && detect_header_after_first_row(&lines, 1, format, separator)
        {
            tracing::debug!("Detected table header via blank line after first row");
            *has_header = true;
        }

        // PSV, whatever its separator, is a flat cell stream: newlines are
        // insignificant and cells flow into rows by column count. DSV is
        // line-per-row.
        if format != TableFormat::Psv {
            let rows = Self::parse_delimited_rows(&lines, separator, base_offset);
            validate_row_widths(&rows)?;
            return Ok(rows);
        }

        let cells = scan_cells(text, separator, base_offset);
        validate_cell_specifiers(&cells)?;
        let ncols = ncols
            .filter(|&n| n > 0)
            .unwrap_or_else(|| first_row_column_count(&cells, text, base_offset, separator));
        if ncols > MAX_TABLE_COLUMNS {
            return Err(TableLimitViolation::new(
                "column count",
                ncols,
                MAX_TABLE_COLUMNS,
                base_offset,
                base_offset,
            ));
        }

        tracing::debug!(?has_header, ncols, "Starting table parsing");

        group_cells_into_rows(cells, ncols, dropped)
    }

    /// Parse a line-per-row table body (DSV, TSV, or a custom separator).
    ///
    /// Each non-empty line is one row; the separator splits cells within the
    /// line. These formats do not use cell specifiers.
    fn parse_delimited_rows(
        lines: &[&str],
        separator: &str,
        base_offset: usize,
    ) -> Vec<Vec<ParsedCell>> {
        let mut rows = Vec::new();
        let mut offset = base_offset;
        for line in lines {
            if !line.trim().is_empty() {
                let cells: Vec<ParsedCell> = split_row_line(line, separator)
                    .into_iter()
                    .map(|part| {
                        trimmed_cell(
                            &part.content,
                            &part.removed,
                            offset + part.start,
                            None,
                            CellSpecifier::default(),
                        )
                    })
                    .collect();
                if !cells.is_empty() {
                    rows.push(cells);
                }
            }
            offset += line.len() + 1; // +1 for newline
        }
        rows
    }

    /// Parse CSV-style (CSV/TSV) table rows using the `csv` crate for RFC 4180
    /// compliance. `delimiter` is the field separator byte.
    ///
    /// This handles multi-line quoted values correctly by processing the entire
    /// table body at once rather than line-by-line.
    fn parse_csv_rows_with_positions(
        text: &str,
        has_header: &mut bool,
        base_offset: usize,
        delimiter: u8,
    ) -> Vec<Vec<ParsedCell>> {
        // Check for header indicator: first row followed by blank line
        // For CSV, we need to detect this before parsing since the csv crate
        // consumes the text as a stream.
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() >= 2 {
            // Find where first CSV record ends - look for first complete record
            // A simple heuristic: if line 1 (0-indexed) is empty, we have a header
            if let Some(&line) = lines.get(1)
                && line.trim().is_empty()
            {
                *has_header = true;
            }
        }

        let csv_rows = parse_csv_table(text, base_offset, delimiter);
        let mut rows = Vec::new();

        for csv_row in csv_rows {
            let mut cells = Vec::new();
            for part in csv_row {
                cells.push(trimmed_cell(
                    &part.content,
                    &part.removed,
                    part.start,
                    None,
                    CellSpecifier::default(),
                ));
            }
            if !cells.is_empty() {
                rows.push(cells);
            }
        }

        rows
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn parse_rows(
        input: &str,
        format: TableFormat,
        separator: &str,
        has_header: &mut bool,
        ncols: Option<usize>,
    ) -> Vec<Vec<ParsedCell>> {
        match Table::parse_rows_with_positions(input, format, separator, has_header, 0, ncols, &mut None) {
            Ok(rows) => rows,
            Err(error) => panic!("table should parse: {error:?}"),
        }
    }

    #[test]
    fn combined_span_accepts_canonical_and_legacy_alignment_order() {
        for source in ["2.2+^.^e", "^.^2.2+e"] {
            let (specifier, consumed) = CellSpecifier::parse(source);

            assert_eq!(consumed, source.len());
            assert_eq!(specifier.colspan, 2);
            assert_eq!(specifier.rowspan, 2);
            assert_eq!(specifier.halign, Some(HorizontalAlignment::Center));
            assert_eq!(specifier.valign, Some(VerticalAlignment::Middle));
            assert_eq!(specifier.style, Some(ColumnStyle::Emphasis));
            assert!(!specifier.is_duplication);
        }
    }

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
        let rows = parse_rows(input, TableFormat::Psv, "|", &mut has_header, None);
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
        let rows = parse_rows(input, TableFormat::Psv, "|", &mut has_header, Some(1));
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

    /// A cell whose content continues on the next line (no separator on that
    /// line) belongs to the same cell — newlines do not delimit cells.
    #[test]
    fn multiline_cell_content_stays_in_one_cell() {
        let input = "| a | b\nstill b\n| c | d\n";
        let mut has_header = false;
        let rows = parse_rows(input, TableFormat::Psv, "|", &mut has_header, Some(2));
        let contents: Vec<Vec<&str>> = rows
            .iter()
            .map(|r| r.iter().map(|c| c.content.as_str()).collect())
            .collect();
        assert_eq!(contents, vec![vec!["a", "b\nstill b"], vec!["c", "d"]]);
    }

    /// A logical row split so later cells start with a delimiter on their own
    /// line still groups into one row by column count.
    #[test]
    fn row_split_across_lines_groups_by_column_count() {
        let input = "| a\n| b\n| c\n| d\n";
        let mut has_header = false;
        let rows = parse_rows(input, TableFormat::Psv, "|", &mut has_header, Some(2));
        let contents: Vec<Vec<&str>> = rows
            .iter()
            .map(|r| r.iter().map(|c| c.content.as_str()).collect())
            .collect();
        assert_eq!(contents, vec![vec!["a", "b"], vec!["c", "d"]]);
    }

    /// Rowspans reserve their column in following rows, so those rows need
    /// fewer new cells — the flat stream must group accordingly.
    #[test]
    fn rowspan_aware_grouping() {
        let input = ".2+| spans | b | c\n| e | f\n| g | h | i\n";
        let mut has_header = false;
        let rows = parse_rows(input, TableFormat::Psv, "|", &mut has_header, Some(3));
        let contents: Vec<Vec<&str>> = rows
            .iter()
            .map(|r| r.iter().map(|c| c.content.as_str()).collect())
            .collect();
        assert_eq!(
            contents,
            vec![vec!["spans", "b", "c"], vec!["e", "f"], vec!["g", "h", "i"]]
        );
        assert_eq!(rows[0][0].spec.rowspan, 2);
    }

    /// TSV fields use the same RFC 4180 quoting as CSV: a `"…"`-quoted value can
    /// span lines and the quotes are stripped (matching asciidoctor), rather
    /// than being split at the newline.
    #[test]
    fn tsv_quoted_field_spans_lines() {
        let input = "a\t\"line1\nline2\"\nb\tc\n";
        let mut has_header = false;
        let rows = parse_rows(input, TableFormat::Csv, "\t", &mut has_header, None);
        let contents: Vec<Vec<&str>> = rows
            .iter()
            .map(|r| r.iter().map(|c| c.content.as_str()).collect())
            .collect();
        assert_eq!(contents, vec![vec!["a", "line1\nline2"], vec!["b", "c"]]);
    }
}
