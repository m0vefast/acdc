use crate::{ColumnStyle, HorizontalAlignment, Table, VerticalAlignment};

pub(crate) const MAX_TABLE_COLUMNS: usize = 100;
pub(crate) const MAX_TABLE_ROWS: usize = 1_000;

/// Round 14 architectural fix: encapsulate separator dispatch into a single
/// type. Rounds 10-13 each found a sibling helper that independently decided
/// "byte length vs codepoint count" or "escape-aware vs literal contains" —
/// every helper re-derived the dispatch from `&str` and could have its own
/// bug. The `Separator` type computes the dispatch ONCE at construction;
/// helpers consume `&Separator` and `match` on `kind`, so the compiler
/// enforces all three cases are handled (no silent "byte length" path).
///
/// `Empty` is rejected at the document-grammar entry by `validate_separator`
/// (`grammar/document.rs`); a defensive arm here ensures helpers don't panic
/// if the invariant is ever violated upstream.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SepKind {
    /// Empty — invalid; helpers return their "no-separator" answer.
    Empty,
    /// Single codepoint — supports escape-aware `\<sep>` handling. Single
    /// ASCII byte falls under this case (1 byte == 1 codepoint).
    SingleCodepoint(char),
    /// Multi-codepoint literal string — `line.contains(raw)` / `line.find(raw)`
    /// substring semantics; no `\<sep>` escape handling (asciidoctor PSV
    /// escape semantics are defined only for single-char separators).
    MultiCodepoint,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Separator<'a> {
    pub raw: &'a str,
    pub kind: SepKind,
}

impl<'a> Separator<'a> {
    pub(crate) fn new(raw: &'a str) -> Self {
        let mut chars = raw.chars();
        let kind = match chars.next() {
            None => SepKind::Empty,
            Some(c) if chars.next().is_none() => SepKind::SingleCodepoint(c),
            Some(_) => SepKind::MultiCodepoint,
        };
        Self { raw, kind }
    }
    pub(crate) fn single_byte(&self) -> Option<u8> {
        // Round 66 fix: explicit variant arms (no wildcard) so future SepKind
        // variants trigger compile error here — preserving the architectural
        // promise documented at SepKind (compiler-enforced exhaustion). The
        // dead `as_str` and `is_empty` accessors were removed at the same time
        // per "shared infra needs a consumer at land time" — both lacked
        // callers in the diff.
        match self.kind {
            SepKind::SingleCodepoint(c) if c.is_ascii() => Some(c as u8),
            SepKind::SingleCodepoint(_) | SepKind::Empty | SepKind::MultiCodepoint => None,
        }
    }
}

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
/// For any single-char separator, `\<sep>` is an escape — the separator
/// byte is content, not a delimiter. Naïve `line.contains(separator)`
/// over-counts escapes as real separators, causing continuation lines that
/// LOOK like they have a separator (because content contains `\<sep>`) to
/// be split into fake cells, dropping the original cell's content silently.
///
/// Round 5: the historical `matches!(sep_char, '|' | ':')` allowlist meant
/// `[separator=,]` / `[separator=?]` etc. would fall back to naïve
/// `line.contains`, causing escape-detection asymmetry with
/// `count_unescaped_separators` (which always honors `\<sep>`). Aligned to
/// the universal `\<sep>` escape rule per asciidoctor PSV semantics — every
/// PSV separator honors `\<sep>`.
fn line_has_unescaped_separator(line: &str, separator: &Separator<'_>) -> bool {
    // Round 14 architectural fix: dispatch is now on the `Separator` type's
    // `kind` field, computed once at construction. Each match arm is
    // exhaustively required by the compiler — no silent byte-length path.
    let sep_char = match separator.kind {
        SepKind::Empty => return false,
        SepKind::MultiCodepoint => return line.contains(separator.raw),
        SepKind::SingleCodepoint(c) => c,
    };
    let mut chars = line.char_indices().peekable();
    while let Some((_idx, ch)) = chars.next() {
        if ch == '\\' {
            // Skip next char ONLY if it IS the separator (selective escape).
            if let Some(&(_, next_ch)) = chars.peek() {
                if next_ch == sep_char {
                    chars.next();
                }
            }
            // Lone `\` (or `\` before non-separator) — treat as literal.
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
fn parse_csv_table(text: &str, base_offset: usize, sep_byte: u8) -> Vec<Vec<CellPart>> {
    let text_bytes = text.as_bytes();
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true) // allow variable column counts
        .delimiter(sep_byte)
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
                find_csv_field_position(text_bytes, scan_pos, field, sep_byte);

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
fn find_csv_field_position(
    text: &[u8],
    start: usize,
    expected_content: &str,
    sep_byte: u8,
) -> (usize, usize) {
    let Some(&first_byte) = text.get(start) else {
        return (start, start);
    };

    if first_byte == b'"' {
        // Quoted field: content starts after the opening quote
        let content_start = start + 1;
        // Find the closing quote (handle escaped quotes "")
        let end_pos = find_closing_quote(text, start + 1);
        // Next field starts after closing quote and separator (or newline)
        let next_pos = skip_to_next_field(text, end_pos, sep_byte);
        (content_start, next_pos)
    } else {
        // Unquoted field: content starts at current position
        let content_start = start;
        // Find end of field (separator or newline)
        let end_pos = find_unquoted_field_end(text, start, expected_content.len(), sep_byte);
        // Next field starts after the separator
        let next_pos = skip_to_next_field(text, end_pos, sep_byte);
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
fn find_unquoted_field_end(text: &[u8], start: usize, content_len: usize, sep_byte: u8) -> usize {
    // The field ends at the separator byte, CR, LF, or content_len bytes
    // (whichever comes first).
    let mut pos = start;
    let mut remaining = content_len;
    while let Some(&byte) = text.get(pos) {
        if byte == sep_byte || byte == b'\n' || byte == b'\r' {
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
fn skip_to_next_field(text: &[u8], pos: usize, sep_byte: u8) -> usize {
    let mut pos = pos;
    // Skip closing quote if present
    if text.get(pos) == Some(&b'"') {
        pos += 1;
    }
    // Skip separator or newline characters
    while let Some(&byte) = text.get(pos) {
        if byte == sep_byte {
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

// `is_psv_table` was a runtime body-sniff that tried to recover whether the
// table was PSV vs DSV from the separator byte + first body line. It was
// deleted because the recovery is fundamentally impossible: `[separator=:]`
// PSV with body `a:b:c` and `[format=dsv]` with body `:foo:bar` are
// indistinguishable here, but the asciidoctor semantics are OPPOSITE. User
// intent is now plumbed from `document.rs` (the only caller) as an
// `is_psv: bool` parameter to `parse_rows_with_positions` — single source of
// truth, decided where the intent is actually known.

/// Split a line into cell parts using the appropriate method for the separator.
///
/// Note: CSV format is handled separately via `parse_csv_table()` for multi-line support.
fn split_line(line: &str, separator: &Separator<'_>) -> Vec<CellPart> {
    // Round 14: dispatch via `Separator::kind`; compiler ensures all arms
    // covered, no silent byte-length branch.
    match separator.kind {
        SepKind::SingleCodepoint(sep_char) => split_escaped(line, sep_char),
        SepKind::MultiCodepoint => split_multi_char(line, separator.raw),
        SepKind::Empty => vec![CellPart {
            content: line.to_string(),
            start: 0,
            forced_spec: None,
        }],
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
    ///
    /// Alignment is accepted in EITHER position relative to the span:
    /// - asciidoctor-standard SPAN-FIRST: `[colspan][.rowspan][+|*][halign][valign][style]`
    ///   (e.g. `2+^`, `.3+^.^`) — the only form asciidoctor emits/parses.
    /// - acdc-historical ALIGN-FIRST: `[halign][valign][colspan][.rowspan][+|*][style]`
    ///   (e.g. `^2+`, `^.^.3+`) — still accepted for back-compat.
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
        Self::build_result(
            bytes, pos, align_end, colspan, rowspan, halign, valign, mode,
        )
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

        // asciidoctor grammar accepts AT MOST ONE halign then AT MOST ONE valign
        // (`[<^>]?(\.[<^>])?`). A repeated run (`<<`, `^^`, `<>`) is NOT alignment
        // — it stays literal content. Parsing each at most once (no loop) keeps
        // e.g. `|foo <<|bar` with `<<` as content, matching asciidoctor, instead
        // of over-consuming the run and deleting it from the cell.
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
            _ => {}
        }
        // Optional vertical alignment: `.` followed by an align char (`.<`, `.^`,
        // `.>`). A bare `.` or `.<digits>` is a rowspan, not valign — leave it.
        if bytes.get(pos) == Some(&b'.') {
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
                _ => {}
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
        align_end: usize,
        colspan: Option<usize>,
        rowspan: Option<usize>,
        halign: Option<HorizontalAlignment>,
        valign: Option<VerticalAlignment>,
        context: ParseContext,
    ) -> (Self, usize) {
        let has_span_or_dup = colspan.is_some() || rowspan.is_some();
        let is_duplication = bytes.get(pos) == Some(&b'*');
        let is_span = bytes.get(pos) == Some(&b'+');

        // A span/dup spec (`2+`, `3*`, `.2+`, `2.3+`) is ONLY recognized in
        // FirstPart context. asciidoctor NEVER reads a cell specifier from text
        // that follows a `|` — a spec lives before the delimiter (captured here
        // via FirstPart at line-start, or stashed on `forced_spec` by the
        // flush-against-`|` mid-row recovery, which also validates under
        // FirstPart). Parsing it from a cell's OWN interior content
        // (InlineContent) silently deleted a leading `N+`/`N*`/`.N+` from real
        // text — `| 2+2 |` became `2` (data loss on well-formed input like a
        // `2+2` math cell). Gating on FirstPart keeps such content intact and
        // also fixes the mirrored colspan over-count in `count_cell_colspans`
        // (its InlineContent probe now reports spec_len 0 → colspan 1).
        if (is_span || is_duplication) && has_span_or_dup && context == ParseContext::FirstPart {
            pos += 1;

            // asciidoctor-standard SPAN-FIRST order allows alignment AFTER the
            // span operator (`2+^`, `.3+^.^`, `2.3+>.<`). Parse it here and merge
            // with any leading alignment (`^2+`, acdc's historical order) so BOTH
            // orders are accepted. asciidoctor only emits/accepts the span-first
            // form, so this is what makes acdc read standard tables.
            let (h, v, align_end) = Self::parse_alignments(bytes, pos);
            pos = align_end;
            let halign = halign.or(h);
            let valign = valign.or(v);

            // Parse optional style letter after operator (and trailing alignment)
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
            // Alignment without a span operator. The colspan/rowspan DIGITS
            // parsed above are NOT part of the spec — a number is only a span
            // when an operator (`+`/`*`) follows it. Restart from `align_end`
            // (just past the alignment) so a token like `<5` / `^100` / `>50`
            // reports a spec_len covering ONLY the alignment; the caller's
            // full-consume gate then rejects the whole token and it stays
            // literal content, matching asciidoctor (span-first grammar never
            // reads align-then-digit as a spec). Without this the discarded
            // digit was still counted as consumed, so mid-row recovery stole
            // `<5`/`^100` and deleted it from the cell.
            let mut pos = align_end;
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

/// A table whose declared dimensions exceed the parser's resource bounds.
///
/// Upstream (`df12e5b`, "bound table dimensions") introduced this to stop a
/// crafted `999999999+|` / `999999999*|` specifier from driving an unbounded
/// allocation. Glyph parses vault files that may be shared or downloaded, so
/// the bound is adopted verbatim (same constants, same `resource` strings).
///
/// Upstream validates a flat `RawCell` stream before grouping. Glyph replaced
/// that grouping stage with a row-oriented model (no `RawCell`/`scan_cells`),
/// so the equivalent check runs over the produced `ParsedCell` rows in
/// `validate_table_limits`. The ordering property that matters is preserved:
/// `duplication_count` is expanded downstream in `grammar/document.rs`, so
/// validating on exit from `parse_rows_with_positions` still happens BEFORE
/// any expansion allocates.
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

/// Enforce upstream's table resource bounds on a parsed row set.
///
/// Merges upstream's `validate_cell_specifiers` (per-cell span/duplication
/// bounds) and `validate_row_widths` (row and column counts) into one pass,
/// because Glyph's row model produces both facts at the same point.
fn validate_table_limits(rows: &[Vec<ParsedCell>]) -> Result<(), TableLimitViolation> {
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
        validate_cell_specifiers(row)?;
    }
    Ok(())
}

/// Per-cell specifier bounds: span and duplication counts.
///
/// Split out of `validate_table_limits` so it can run BEFORE an implicit column
/// count is derived from the first row. `101*| x` in a table with no `[cols=]`
/// must report "cell duplication", which is what the author actually typed; a
/// derived width of 101 would otherwise trip the column-count bound first and
/// report a number that appears nowhere in the source.
fn validate_cell_specifiers(cells: &[ParsedCell]) -> Result<(), TableLimitViolation> {
    for cell in cells {
        if cell.colspan > MAX_TABLE_COLUMNS {
            return Err(TableLimitViolation::new(
                "column span",
                cell.colspan,
                MAX_TABLE_COLUMNS,
                cell.start,
                cell.end,
            ));
        }
        if cell.rowspan > MAX_TABLE_ROWS {
            return Err(TableLimitViolation::new(
                "row span",
                cell.rowspan,
                MAX_TABLE_ROWS,
                cell.start,
                cell.end,
            ));
        }
        if cell.duplication_count > MAX_TABLE_COLUMNS {
            return Err(TableLimitViolation::new(
                "cell duplication",
                cell.duplication_count,
                MAX_TABLE_COLUMNS,
                cell.start,
                cell.end,
            ));
        }
    }
    Ok(())
}

/// Check if a blank line after the first row indicates a header.
/// A header is indicated only if the first non-empty line after the blank
/// contains an UNESCAPED separator. If it's a continuation line (no separator
/// or only escaped ones), it's content that attaches to the previous cell,
/// not a header indicator.
///
/// LIMITATION: this heuristic still false-positives on body lines whose
/// natural prose contains the separator byte UNESCAPED — e.g. for
/// `[separator=:]` the line `Note: see 12:30:45 timestamp` returns true and
/// promotes the first row to header. Honors `\sep` escapes so authors with
/// content that needs to NOT be header-promoted can opt out with backslash;
/// a strict fix would re-parse the line as a candidate row and verify it
/// produces at least `ncols` cells under the cell-spec grammar, mirroring
/// asciidoctor's check. Documenting the gap because Round 8's escape-aware
/// switch was once over-claimed as a general fix.
fn detect_header_after_first_row(
    lines: &[&str],
    start_idx: usize,
    separator: &Separator<'_>,
) -> bool {
    for &line in lines.iter().skip(start_idx) {
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            return line_has_unescaped_separator(trimmed, separator);
        }
    }
    false
}

/// Count the colspan-weighted cell contribution of a single PSV/DSV line.
/// Used by the multi-line row collector to detect when accumulated cells
/// have filled the row (so the next separator-prefixed line starts a new row).
///
/// When `is_psv` (set by the caller from `document.rs` based on user intent —
/// works for ANY separator under user-asserted PSV semantics, not just `|`/`!`):
/// - `parts[0]` (text before first separator) is skipped for content; if
///   it's itself a bare specifier like `.2+`, capture its colspan as
///   pending for `parts[1]`.
/// - Each subsequent part contributes its colspan (1 if no leading spec).
/// - A trailing empty part (line ending with the separator) is skipped.
///
/// Continuation lines (no separator at all) contribute 0 — they will be
/// absorbed into the previous cell's content by `parse_row_with_positions`.
fn count_cell_colspans(line: &str, separator: &Separator<'_>, is_psv: bool) -> usize {
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

    // PSV: `parts[0]` is the empty slice BEFORE the leading separator. It is
    // NOT a cell — skip it from col counting. DSV / TSV / CSV keep `parts[0]`
    // as content. `is_psv` is plumbed from `parse_table_block_impl` in
    // document.rs based on user intent (`[separator=]`/`[format=]`/fence).
    let start_idx = if is_psv {
        let p0 = parts[0].content.trim();
        if !p0.is_empty() {
            let (spec, spec_len) = CellSpecifier::parse(p0, ParseContext::FirstPart);
            if spec_len > 0 && spec_len == p0.len() {
                // A duplication spec (`k*`) occupies k columns, not one: `k` is
                // parsed into `duplication_count` while `colspan` stays 1, and
                // `grammar/document.rs` expands the cell into k columns later.
                // Counting it as one made the ncols-aware row break under-count,
                // so `[cols="100*"]` with `100*| x` per line absorbed 100 SOURCE
                // LINES into a single logical row and then expanded each cell
                // ×100 — a 100-column table became one row of 10 000 columns.
                // Invisible until upstream's table resource limits started
                // measuring the column count.
                pending_colspan = Some(spec.colspan * spec.duplication_count);
            }
        }
        1
    } else {
        // DSV (`:` default / TSV `\t`): parts[0] is content
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
                    let (_, spec_len) = CellSpecifier::parse(last_token, ParseContext::FirstPart);
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
            // asciidoctor parity: a span/dup token consuming the WHOLE cell
            // (`|2+|`) is literal (colspan 1); only a PREFIX spec (content
            // after it, e.g. `2+wide`) contributes its colspan.
            if spec_len > 0 && spec_len < trimmed.len() {
                spec.colspan
            } else {
                1
            }
        };
        total += cs;
    }
    total
}

/// Count unescaped occurrences of `separator` in `line`. Used by
/// the row-collection loop to detect inline-row format (multi-separator
/// line) vs continuation paragraphs of a multi-line `a|` cell.
/// Columns one parsed cell occupies in its row.
///
/// A duplication spec (`k*`) yields k columns, but `k` is parsed into
/// `duplication_count` while `colspan` stays 1 — `grammar/document.rs` performs
/// the expansion later. EVERY column-accounting site must agree on this or row
/// grouping and expansion contradict each other: summing bare `colspan` made
/// `last_row_cols` read `100*| x` as ONE column, judge the row incomplete
/// against `[cols="100*"]`, and merge the following row into it. Every source
/// line collapsed into a single logical row that then expanded to
/// 100 x line-count columns. `count_cell_colspans` performs the same
/// multiplication on the pre-parse side; the two are a pair.
///
/// MEASURED (2026-08-02): at the one call site below, `duplication_count` is
/// always 1 — `parse_rows_with_positions` expands a `k*` cell into k separate
/// cells before returning, so the factor is unreachable there today and no test
/// can exercise it. It is kept because this function states what a cell
/// OCCUPIES, and the expansion point has already moved once; a future move back
/// would silently reintroduce the under-count if this said `colspan` alone. The
/// factor that does real work is the matching one in `count_cell_colspans`,
/// which `column_accounting_sites_agree_for_every_spec_shape` pins.
const fn occupied_columns(cell: &ParsedCell) -> usize {
    cell.colspan * cell.duplication_count
}

fn count_unescaped_separators(line: &str, separator: &Separator<'_>) -> usize {
    // Round 14: dispatch via `Separator::kind`; compiler-enforced exhaustion.
    let sep_ch = match separator.kind {
        SepKind::Empty => return 0,
        SepKind::MultiCodepoint => return line.matches(separator.raw).count(),
        SepKind::SingleCodepoint(c) => c,
    };
    // Round 34 fix: SELECTIVE backslash-consumption — only swallow next char
    // when it IS the separator. Previously greedy consumption (`chars.next()`
    // regardless) diverged from `line_has_unescaped_separator` /
    // `split_escaped` (both selective per asciidoctor `(?<!\\)<sep>` regex).
    // Under input `\\|` (literal backslash + escaped pipe), greedy gave 1
    // while selective gives 0 — asciidoctor parses 2 cells. Aligning the
    // three helpers to one escape rule eliminates the cross-helper drift
    // pinned by Round 34 Expert A.
    let mut count = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next_ch) = chars.peek() {
                if next_ch == sep_ch {
                    chars.next();
                }
            }
            // Lone `\` (or `\` before non-separator) — treat as literal.
        } else if ch == sep_ch {
            count += 1;
        }
    }
    count
}

/// Check if a line starting with a cell specifier followed by separator
/// indicates a new row. Detects patterns like `a|`, `s|`, `2+|`, `^|`,
/// `^.>2+s|` at the start of a line.
fn is_new_row_start(line: &str, separator: &Separator<'_>, is_psv: bool) -> bool {
    // Cell-spec prefix detection is PSV-only. `is_psv` is plumbed from
    // `parse_table_block_impl` in document.rs; centralizing the gate here
    // keeps the predicate consistent with every other PSV-only behavior in
    // this file (count, single-line detection, multi-line collector,
    // adjacent-anchor recovery).
    if !is_psv {
        return false;
    }
    // Round 14: dispatch via `Separator::kind`; compiler-enforced exhaustion.
    let sep_ch = match separator.kind {
        SepKind::Empty => return false,
        SepKind::MultiCodepoint => {
            let Some(pos) = line.find(separator.raw) else {
                return false;
            };
            return validate_row_start_at(line, pos);
        }
        SepKind::SingleCodepoint(c) => c,
    };
    let sep_pos: Option<usize> = {
        // Round 34 fix: SELECTIVE backslash-consumption — only swallow next
        // char when it IS the separator. Aligns with the universal escape
        // discipline used by `line_has_unescaped_separator` / `split_escaped`
        // / `count_unescaped_separators` (Round 34) so all four helpers
        // agree on `\\<sep>` (literal backslash + escaped sep = 0 unescaped
        // seps) instead of disagreeing per-helper.
        let mut chars = line.char_indices().peekable();
        let mut found: Option<usize> = None;
        while let Some((idx, ch)) = chars.next() {
            if ch == '\\' {
                if let Some(&(_, next_ch)) = chars.peek() {
                    if next_ch == sep_ch {
                        chars.next();
                    }
                }
                // Lone `\` — treat as literal.
            } else if ch == sep_ch {
                found = Some(idx);
                break;
            }
        }
        found
    };
    let Some(sep_pos) = sep_pos else { return false };
    validate_row_start_at(line, sep_pos)
}

/// Helper for `is_new_row_start`: given the byte position of the first
/// separator in `line`, return whether the bytes BEFORE it are a complete
/// cell specifier. Extracted so both single-codepoint (escape-aware) and
/// multi-codepoint (literal find) paths share the same validation.
fn validate_row_start_at(line: &str, sep_pos: usize) -> bool {
    let raw_before = &line[..sep_pos];
    // asciidoctor: a cell specifier must be FLUSH against the separator — ANY
    // whitespace between the spec and `|` makes the token literal cell CONTENT,
    // not a spec, so the line CONTINUES the current row rather than starting a new
    // one. `d |e` / `2+ |x` (space before `|`) are continuations (`d`/`2+` join
    // the previous cell); `d|e` / `2+|x` (flush) are spec-led new rows. The old
    // `.trim()` stripped the trailing space and mis-read `d ` / `2+ ` as flush
    // specs, which dropped the pre-`|` content of such continuation lines. Leading
    // indent is still allowed (only the char immediately before `|` matters).
    if raw_before.ends_with(char::is_whitespace) {
        return false;
    }
    let before_sep = raw_before.trim_start();
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
/// Peeking from `from`, is the next NON-blank line a row CONTINUATION rather than
/// the start of a new row? asciidoctor's rule: a blank line inside a table only
/// terminates the current row when what follows STARTS a new row (a
/// separator-led `|cell` line or a spec-led `2+|`/`a|` line). If the next
/// non-blank line instead begins with content (no leading separator, not a
/// spec), it CONTINUES the current row — the blank is an intra-cell paragraph
/// break, and the continuation's leading text joins the row's last (open) cell
/// while a mid-line `|` opens new cells in the SAME row. Returns false at
/// end-of-input (nothing to continue → the blank terminates). This replaces the
/// old line-structure heuristic (every blank terminates the row), which silently
/// dropped the post-blank leading content of any unfilled row.
fn next_nonblank_is_row_continuation(
    lines: &[&str],
    from: usize,
    separator: &Separator<'_>,
    is_psv: bool,
) -> bool {
    if !is_psv {
        return false;
    }
    for line in lines.iter().skip(from) {
        let t = line.trim_end();
        if t.trim().is_empty() {
            continue;
        }
        return !t.trim_start().starts_with(separator.raw)
            && !is_new_row_start(t, separator, is_psv);
    }
    false
}

fn handle_cross_row_continuation(
    lines: &[&str],
    i: &mut usize,
    current_offset: &mut usize,
    rows: &mut [Vec<ParsedCell>],
    separator: &Separator<'_>,
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

/// Re-flow a PSV table's cells into rows of exactly `ncols`, honoring colspan,
/// cell-duplication (`N*`) width, and rowspan grid occupancy — asciidoctor's
/// row model: cells stream left-to-right / top-to-bottom into an `ncols`-wide
/// grid; source line breaks and blank lines carry NO row-boundary meaning
/// (only the cell COUNT does). This replaces the historical line-structure
/// heuristic's row GROUPING (which mis-split rows like an `a|` block sitting in
/// a non-last column, spilling the row's remaining inline cells onto their own
/// row). The cells themselves are untouched — only their grouping into rows —
/// so byte offsets / content / specs are preserved exactly.
fn grid_reflow(rows: Vec<Vec<ParsedCell>>, ncols: usize) -> Vec<Vec<ParsedCell>> {
    if ncols == 0 {
        return rows;
    }
    // Expand `N*` cell-duplication into N INDEPENDENT copies up front so the
    // copies stream/wrap across grid rows exactly like asciidoctor: a dup cell
    // straddling a smaller `[cols=N]` (`|a 3*|b` in a 2-col table) wraps into
    // `[a,b]` / `[b,b]` instead of forming one over-wide 4-column row. (colspan
    // stays ATOMIC — asciidoctor drops an over-wide colspan cell, and the width
    // math below honors that.) Each copy carries duplication_count 1 so the
    // downstream builder does not expand it a second time.
    let mut cells = rows
        .into_iter()
        .flatten()
        .flat_map(|cell| {
            let n = cell.duplication_count.max(1);
            (0..n).map(move |_| {
                let mut copy = cell.clone();
                copy.duplication_count = 1;
                copy.is_duplication = false;
                copy
            })
        })
        .peekable();
    if cells.peek().is_none() {
        return Vec::new();
    }
    let mut out: Vec<Vec<ParsedCell>> = Vec::new();
    // Rowspans declared in an EARLIER row that still cover part of this row:
    // (covered_width, remaining_rows_below). This is asciidoctor's COUNT model
    // (`table.rb` closes a row when column_visits + active_rowspan_width ==
    // colcount) — a pure width count, NOT column positions. A positional
    // skip-model diverges when a colspan cell's WIDTH crosses a rowspan-covered
    // column: it packs cells past the covered column into one over-wide row that
    // the builder then drops, silently losing cells asciidoctor keeps (a
    // mid-column rowspan with colspan cells in the covered rows). Counting width
    // — the covered columns consume the row's budget — closes the row exactly
    // where asciidoctor does.
    let mut active: Vec<(usize, usize)> = Vec::new();
    while cells.peek().is_some() {
        // Saturating throughout so a pathological line (many ~19-digit colspans)
        // can't integer-overflow / panic under debug overflow-checks; a giant
        // width just keeps everything on one row (release already wrapped).
        let occupied: usize = active
            .iter()
            .map(|&(w, _)| w)
            .fold(0, usize::saturating_add);
        let mut cur: Vec<ParsedCell> = Vec::new();
        let mut placed = 0usize; // sum of this row's own cell widths
        while occupied.saturating_add(placed) < ncols {
            let Some(cell) = cells.next() else {
                break; // out of cells — this row is the (partial) last row
            };
            let width = cell
                .duplication_count
                .max(1)
                .saturating_mul(cell.colspan.max(1));
            if cell.rowspan > 1 {
                active.push((width, cell.rowspan - 1));
            }
            cur.push(cell);
            placed = placed.saturating_add(width);
        }
        if cur.is_empty() {
            // Fully-phantom grid row: every column is covered by a rowspan from
            // an EARLIER row. asciidoctor's model consumes+DISCARDS exactly ONE
            // filler cell here (the cell "falls into" the covered row and is
            // lost). This is BOUNDED — every phantom row consumes one cell, so
            // total rows <= cell count; an oversized rowspan (`.9999999+`) can no
            // longer amplify into millions of empty rows (that was an O(rowspan)
            // OOM/hang on ~20 bytes of user-pasted input). When no filler cell
            // remains, the table ends here (a trailing rowspan just widens its
            // `rowspan` attribute, matching asciidoctor).
            if cells.next().is_none() {
                break;
            }
            // Fall through: EMIT the (empty) phantom row + age. Emitting — not
            // silently dropping — is what keeps the two rowspan trackers in
            // lockstep: the downstream builder (document.rs) must SEE this grid
            // row to age its own tracker on it. The builder then drops the empty
            // row from output; the covering `rowspan` attribute overlaps the next
            // real row, matching asciidoctor. Silently discarding it desynced the
            // trackers → the builder aged one grid row late → false over-counts
            // (dropping real cells) on the row after a full-width rowspan.
        }
        out.push(cur); // empty for a phantom row, real cells otherwise
        active.retain_mut(|(_, rem)| {
            if *rem == 0 {
                false
            } else {
                *rem -= 1;
                true
            }
        });
    }
    out
}

impl Table<'_> {
    pub(crate) fn parse_rows_with_positions(
        text: &str,
        separator: &Separator<'_>,
        is_psv: bool,
        is_csv: bool,
        has_header: &mut bool,
        base_offset: usize,
        ncols: Option<usize>,
    ) -> Result<Vec<Vec<ParsedCell>>, TableLimitViolation> {
        // Upstream bounds the RESOLVED column count; Glyph resolves `ncols`
        // internally further down, so the attacker-controlled input — an
        // explicit `[cols=999999]` — is bounded here, and every width that the
        // row model actually produces is bounded by `validate_table_limits`
        // on the way out.
        if let Some(n) = ncols
            && n > MAX_TABLE_COLUMNS
        {
            return Err(TableLimitViolation::new(
                "column count",
                n,
                MAX_TABLE_COLUMNS,
                base_offset,
                base_offset,
            ));
        }

        // CSV format needs special handling for multi-line quoted values.
        // The user-intent gates `is_psv` and `is_csv` are MUTUALLY EXCLUSIVE
        // (both come from `document.rs`'s format/separator resolution table).
        // CSV implies RFC 4180 multi-line-quoted semantics — route into the
        // CSV stack regardless of separator byte (so `[%csv,separator=;]`
        // works end-to-end). DSV / TSV / PSV fall through to the row-by-row
        // parser below.
        if is_csv {
            let rows =
                Self::parse_csv_rows_with_positions(text, separator, has_header, base_offset);
            validate_table_limits(&rows)?;
            return Ok(rows);
        }

        let mut rows: Vec<Vec<ParsedCell>> = Vec::new();
        let mut current_offset = base_offset;
        let lines: Vec<&str> = text.lines().collect();
        // PSV/DSV/CSV/TSV decision is carried from the caller (document.rs)
        // based on user intent (`[separator=X]` always means PSV;
        // `[format=dsv|csv|tsv]` always means non-PSV; defaults derive from
        // the default separator byte). NEVER recover from `separator` alone
        // here — `[separator=:]` and `[format=dsv]` both arrive with
        // separator=":" but have opposite PSV semantics. The historical
        // runtime body-sniff `is_psv_table` is gone; intent is plumbed.
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

        // asciidoctor implicit-header rule: the header fires iff the FIRST
        // PHYSICAL (non-empty) line of the table body is IMMEDIATELY followed by
        // a blank line AND the next non-blank line STARTS A NEW ROW (not a
        // continuation of the first row's last cell). This is a first-PHYSICAL-
        // LINE test, NOT a first-collected-ROW test: the collector can MERGE
        // several physical lines into one logical first row (`|x`⏎`l|lit`⏎⏎`|c |d`
        // → merged `[x,lit]`), and a blank after that merged row's LAST line must
        // NOT promote it to a header — asciidoctor only inspects the line right
        // after the first. And a blank the first row ABSORBS as an intra-cell
        // paragraph break (`|a |b`⏎⏎`c |d`, where `c |d` continues cell `b`) is
        // not a header trigger either. Computed once here, independent of the
        // collector's row-merging, so neither interaction corrupts it.
        if !*has_header
            && let Some(f) = lines.iter().position(|l| !l.trim().is_empty())
            && lines.get(f + 1).is_some_and(|l| l.trim_end().is_empty())
            && detect_header_after_first_row(&lines, f + 1, separator)
            && !next_nonblank_is_row_continuation(&lines, f + 1, separator, is_psv)
        {
            *has_header = true;
        }

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
            // PSV intent (any user-asserted separator, including `:` via
            // `[separator=:]` or `[format=psv]`) supports multi-line cells;
            // pure DSV/TSV (no PSV intent) do not — every line is a complete
            // row. For PSV, count UNESCAPED separators via
            // `split_line` so that lines containing `\|` (e.g. AsciiDoc reference
            // docs describing `a|` syntax inside backticks: `` `a\|` ``) aren't
            // mis-classified as single-line rows. Previously the naïve
            // `first_line.matches(separator).count()` over-counted escaped
            // separators, triggering "unterminated table block" cascades — see
            // Glyph render-fidelity tests for the user-visible failure mode.
            let first_line = line_ref.trim_end();
            // PSV (any sep that isn't CSV `,` / TSV `\t` / ambiguous `:`) supports
            // multi-line cells. DSV/TSV/CSV treat every line as one row, so the
            // "is this a complete single-line row" check is PSV-only.
            // Historical limit was hard-coded `"|" | "!"`; the gate is not a
            // grammar constraint — `split_line` + `CellSpecifier::parse` work
            // identically for any sep char (the spec is char-agnostic, only
            // depends on what follows after the leading separator). See
            // asciidoctor 2.x docs/tables/data-format/ — `[separator=X]` for
            // arbitrary single-char X gets full PSV cell-spec support.
            let is_single_line_row = if is_psv {
                // Round 14 architectural fix: escape-aware. PSV `\<sep>` is
                // an escape, not a delimiter; raw `contains` would let a
                // line whose only seps are escaped (e.g. `\:foo`) trigger
                // single-line-row detection and drop content downstream.
                if line_has_unescaped_separator(first_line, separator) {
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
                    let single_by_shape = parts.len() > 2 && !trailing_modifier_with_empty;
                    // Lookahead: even a complete-looking single-line row is a
                    // MULTI-LINE cell-flow start when the NEXT line is a
                    // CONTINUATION — a non-empty line that neither starts a new
                    // row (spec-led, `is_new_row_start`) nor begins with the
                    // separator, i.e. its leading text continues this row's last
                    // cell. Covers `|a |m1` / `m2 |c` (mid-column) and
                    // `|a |b |m1` / `m2` (last-column). A next line that BEGINS
                    // with the separator (`|R2C1|R2C2` / `|R3C1...`) is a new
                    // row, so the current line stays single-line (short row).
                    // asciidoctor flows cell CONTENT across such continuation
                    // lines; classifying the current line single-line splits the
                    // flow and drops the continuation.
                    // Peek PAST blank lines (not just the immediate next line): a
                    // blank followed by a CONTINUATION means the row's last cell
                    // stays open across the blank (asciidoctor's cell model), so this
                    // line is a MULTI-LINE cell-flow start, not a single-line row.
                    // `|a |b`⏎⏎`c |d` → the `c |d` continues cell `b`; classifying
                    // `|a |b` single-line would split the flow and drop `c`. A blank
                    // followed by a separator-led/spec-led line (or EOF) keeps this
                    // line single-line (`|a |b`⏎⏎`|c |d` → header / new row).
                    let next_is_continuation =
                        next_nonblank_is_row_continuation(&lines, i + 1, separator, is_psv);
                    single_by_shape && !next_is_continuation
                } else {
                    false
                }
            } else {
                // DSV / TSV: every line is a row (no multi-line cells).
                first_line.matches(separator.raw).count() > 0
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
                        // A blank line terminates the row ONLY when what follows
                        // STARTS a new row (a separator-led or spec-led line). If the
                        // next non-blank line is a CONTINUATION (leading content, no
                        // leading separator, not a spec), the blank is an intra-cell
                        // paragraph break: keep the row open so the continuation's
                        // leading text joins the row's last (open) cell and a mid-line
                        // `|` opens new cells in the SAME row (asciidoctor's cell
                        // model). This covers ANY unfilled row — simple cells as well
                        // as `a|`/`l|` — not the old line-structure heuristic that
                        // dropped the post-blank leading content of an unfilled row.
                        if next_nonblank_is_row_continuation(&lines, i + 1, separator, is_psv) {
                            row_lines.push(trimmed);
                            current_offset += current_line.len() + 1;
                            i += 1;
                            continue;
                        }
                        break;
                    }
                    // If we already have content and this line starts a new row, break
                    if !row_lines.is_empty() && is_new_row_start(trimmed, separator, is_psv) {
                        break;
                    }
                    // Trailing-`a|` multi-line cell continuation: when a
                    // previous line was a continuation paragraph and the
                    // current line is a clear inline-row (multi-`|` line),
                    // break — the `a|` cell content ended; this is a new
                    // row. See `row_has_continuation_paragraph` doc above.
                    // All three gates below are PSV-only multi-line-row
                    // heuristics (continuation paragraphs, ncols-aware row
                    // break, new-row detection). Use the captured `is_psv`
                    // plumbed from `parse_table_block_impl` in document.rs —
                    // recomputing the PSV decision from `separator` alone
                    // here would re-introduce the historical cross-site drift.
                    if row_has_continuation_paragraph
                        && is_psv
                        && trimmed.starts_with(separator.raw)
                        && count_unescaped_separators(trimmed, separator) > 1
                    {
                        break;
                    }
                    if !row_lines.is_empty()
                        && is_psv
                        && trimmed.starts_with(separator.raw)
                        && let Some(expected) = ncols
                        && accumulated_cols >= expected
                    {
                        break;
                    }
                    if !row_lines.is_empty() && is_psv && !trimmed.starts_with(separator.raw) {
                        row_has_continuation_paragraph = true;
                    }
                    accumulated_cols += count_cell_colspans(trimmed, separator, is_psv);
                    row_lines.push(trimmed);
                    current_offset += current_line.len() + 1; // +1 for newline
                    i += 1;
                }
            }

            if !row_lines.is_empty() {
                let columns =
                    Self::parse_row_with_positions(&row_lines, separator, is_psv, row_start_offset);

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
                    let last_row_cols: usize = last_row.iter().map(occupied_columns).sum();
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

            // (Implicit-header detection is done once, up front, from the FIRST
            // PHYSICAL LINE — see the `implicit-header rule` block near the top of
            // this function. It is deliberately NOT re-evaluated here per collected
            // row: the collector's row-merging would otherwise fire it on the blank
            // after a merged multi-line first row, promoting a non-header to a
            // header, diverging from asciidoctor.)

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

        // PSV row model is cell-count-driven (asciidoctor): re-flow the cells
        // into an `ncols`-wide grid so source line structure never dictates row
        // boundaries. When `[cols=]` is absent, `ncols` is the FIRST LINE's grid
        // width. DSV/TSV keep one-line-per-row semantics (no re-flow).
        if is_psv && !rows.is_empty() {
            // Implicit column count (no `[cols=]`) = the FIRST LOGICAL ROW's grid
            // WIDTH (asciidoctor's rule), summing each cell's duplication *
            // colspan. The first logical row is the first non-empty line PLUS
            // every following line that does NOT start a new row — a wrapped cell
            // continues onto a non-separator-led / non-spec-led line. It stops at
            // the first separator-led or spec-led line, a blank line, or EOF.
            //
            // It must NOT come from the post-collection `rows[0]` (the collector
            // merges bare-`|` lines like `|b`/`|X|Y|Z` into `rows[0]`, inflating
            // the width: `|a`/`|b`/`|c` is 3 one-col rows, not one 3-col row). And
            // it must NOT come from just the first PHYSICAL line (a wrapped first
            // cell — `|a |b is` / `long |c` — is one 3-col logical row, not 2).
            // grid_reflow then re-splits the merged cells by this width,
            // reproducing asciidoctor's row shape.
            let mut first_cell_violation: Option<TableLimitViolation> = None;
            let n = ncols.unwrap_or_else(|| {
                let all: Vec<&str> = text.lines().collect();
                let start = all
                    .iter()
                    .position(|l| !l.trim().is_empty())
                    .unwrap_or(all.len());
                let mut logical_row: Vec<&str> = Vec::new();
                let mut j = start;
                while j < all.len() {
                    let t = all[j].trim_end();
                    if t.trim().is_empty() {
                        // A blank continues the first logical row only when the next
                        // non-blank line is a CONTINUATION (the same peek rule the
                        // collector uses), so a wrapped first cell — `a|`/`l|` OR a
                        // simple cell — doesn't prematurely stop the width count.
                        if j > start
                            && next_nonblank_is_row_continuation(&all, j + 1, separator, is_psv)
                        {
                            logical_row.push(all[j]);
                            j += 1;
                            continue;
                        }
                        break;
                    }
                    if j > start
                        && (t.trim_start().starts_with(separator.raw)
                            || is_new_row_start(t, separator, is_psv))
                    {
                        break;
                    }
                    logical_row.push(all[j]);
                    j += 1;
                }
                let first_cells =
                    Self::parse_row_with_positions(&logical_row, separator, is_psv, 0);
                // Report what the author typed, before a derived width can
                // trip the column-count bound with a number they never wrote.
                first_cell_violation = validate_cell_specifiers(&first_cells).err();
                if first_cell_violation.is_some() {
                    return 1;
                }
                // Saturating so a pathological first line (multiple ~19-digit
                // colspans) can't integer-overflow / panic; a giant ncols just
                // makes grid_reflow keep everything on one row.
                first_cells
                    .iter()
                    .map(|c| c.duplication_count.max(1).saturating_mul(c.colspan.max(1)))
                    .fold(0usize, usize::saturating_add)
                    .max(1)
            });
            if let Some(violation) = first_cell_violation {
                return Err(violation);
            }
            // MUST precede `grid_reflow`: that function materializes one copy
            // per `duplication_count` with no bound of its own, so a hostile
            // `999999999*| x` allocates ~1e9 cells and the process is OOM-killed
            // before the exit check below ever runs. Upstream validated the cell
            // stream BEFORE grouping for exactly this reason; Glyph's row model
            // moved the exit check after grouping, which reopened the hole for
            // any table carrying an explicit `[cols=N]` (the implicit-width path
            // is already covered by `first_cell_violation` above). Measured: the
            // 999999999 case went from SIGKILL to a rejection in microseconds.
            for row in &rows {
                validate_cell_specifiers(row)?;
            }
            rows = grid_reflow(rows, n);
        }

        validate_table_limits(&rows)?;
        Ok(rows)
    }

    /// Parse CSV table rows using the `csv` crate for RFC 4180 compliance.
    ///
    /// This handles multi-line quoted values correctly by processing the entire
    /// table body at once rather than line-by-line.
    fn parse_csv_rows_with_positions(
        text: &str,
        separator: &Separator<'_>,
        has_header: &mut bool,
        base_offset: usize,
    ) -> Vec<Vec<ParsedCell>> {
        // Round 13 Expert A P1-2 fix: RFC 4180-aware header detection. The
        // prior `lines.get(1).trim().is_empty()` heuristic split on `\n`
        // without honoring quoted multi-line records. A first record whose
        // value contains an embedded `\n` would make `lines[1]` non-empty
        // (header missed) or empty-inside-the-quote (header falsely set).
        // Use the csv crate to find the first record's actual end byte
        // offset, then check whether the next non-empty source line is
        // preceded by a blank line.
        //
        // CSV separator override: a single byte is required by RFC 4180 grammar
        // (the csv crate's delimiter API is byte-level). Validation at
        // `document.rs::validate_separator` already rejected non-ASCII /
        // multi-byte values, so `separator.len() == 1 && is_ascii()` is the
        // invariant here. Defensive unwrap for the no-attr path (`,` byte).
        // Round 14: `Separator::single_byte()` returns the ASCII byte iff the
        // separator is one ASCII codepoint; multi-byte / non-ASCII rejected
        // upstream by `validate_separator`. Defensive fallback to `,`.
        let sep_byte = separator.single_byte().unwrap_or(b',');
        {
            let mut hr_reader = csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .delimiter(sep_byte)
                .from_reader(text.as_bytes());
            let mut records = hr_reader.records();
            if let Some(Ok(first)) = records.next() {
                let first_end = first
                    .position()
                    .map(|p| usize::try_from(p.byte()).unwrap_or(0))
                    .unwrap_or(0);
                // first_end points at the START byte of the record. Walk
                // forward to the end of THIS record by consuming bytes up to
                // and including a non-quoted newline.
                let bytes = text.as_bytes();
                let mut pos = first_end;
                let mut in_quote = false;
                while pos < bytes.len() {
                    let b = bytes[pos];
                    if in_quote {
                        if b == b'"' {
                            if bytes.get(pos + 1) == Some(&b'"') {
                                pos += 2;
                                continue;
                            }
                            in_quote = false;
                        }
                    } else if b == b'"' {
                        in_quote = true;
                    } else if b == b'\n' {
                        pos += 1;
                        break;
                    }
                    pos += 1;
                }
                // Check whether the immediately-following line is blank.
                let after_first = &text[pos.min(text.len())..];
                let trimmed_start = after_first.trim_start_matches(|c: char| c == '\r');
                if trimmed_start.starts_with('\n') || (trimmed_start.is_empty() && pos < text.len())
                {
                    *has_header = true;
                }
            }
        }
        let csv_rows = parse_csv_table(text, base_offset, sep_byte);
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
        separator: &Separator<'_>,
        is_psv: bool,
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
            // Adjacent-anchor recovery is a PSV-only fix-up. All four gates
            // below consume the same captured `is_psv` from the function's
            // entry — there is exactly one PSV decision per table, not 11.
            let p0_trimmed = if is_psv { parts[0].content.trim() } else { "" };
            // When `!is_psv`, `p0_trimmed` is already forced to "" above, so
            // the inner branches degrade naturally without re-guarding on
            // `is_psv` — clippy-pedantic flags the double-guard as
            // collapsible. Keep the predicate compact and let the outer
            // `if is_psv && parts.len() > 1 && (...)` gate be the single
            // authoritative entry to the recovery block.
            let line_starts_with_spec = if p0_trimmed.is_empty() {
                false
            } else {
                let (_, spec_len) = CellSpecifier::parse(p0_trimmed, ParseContext::FirstPart);
                spec_len > 0 && spec_len == p0_trimmed.len()
            };
            // Any PSV line that is NOT a spec-led new-row is a CONTINUATION —
            // whether bare-`|`-led (`|a |b`) OR carrying wrapped content before a
            // mid-row cell (`wraps here 2+|Wide`). Its mid-row FLUSH span/dup/align
            // specs must be recovered (asciidoctor does — the `2+` there is a
            // colspan for `Wide`, well-formed, 0 warnings), while its bare trailing
            // STYLE letters must NOT be stolen (natural-text safety). The old
            // `parts[0].is_empty()` definition missed the content-bearing case, so
            // `2+`/`3*` on a wrapped continuation line leaked into the previous
            // cell and mis-counted implicit ncols.
            let multi_line_continuation = is_psv && !line_starts_with_spec;
            if is_psv && parts.len() > 1 {
                for i in 1..parts.len() {
                    let (left_slice, right_slice) = parts.split_at_mut(i);
                    let prev = &mut left_slice[i - 1];
                    let cur = &mut right_slice[0];
                    let trimmed_end = prev.content.trim_end();
                    // asciidoctor: the specifier must be FLUSH against the `|`
                    // (no whitespace between the spec and the delimiter). A
                    // space before `|` — `x 2+ |` — makes the token literal
                    // cell content, not a colspan/duplication spec.
                    if trimmed_end.len() != prev.content.len() {
                        continue;
                    }
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
                    // Recover a trailing mid-row spec when it carries an
                    // UNAMBIGUOUS marker: a span/dup operator (`+`/`*`) OR a
                    // halign/valign char (`<`/`^`/`>`, incl. `.^` etc.). Those
                    // chars before a FLUSH `|` are vanishingly rare in prose, so
                    // `|apple ^|red` correctly centers `red` (the Glyph mid-row-
                    // alignment bug — previously only `+`/`*` were accepted, so
                    // `^` leaked into the previous cell).
                    //
                    // A BARE style letter (`a`, `m`, …) is deliberately NOT stolen
                    // in `multi_line_continuation` mode: it's indistinguishable
                    // from a natural word ending (`...ending in a|next`). asciidoctor
                    // steals it (mangling the prose); acdc keeps the safer read —
                    // see `no_recovery_on_trailing_a_in_multiline_continuation_text`.
                    if multi_line_continuation
                        && !candidate.contains('+')
                        && !candidate.contains('*')
                        && !candidate.contains('<')
                        && !candidate.contains('^')
                        && !candidate.contains('>')
                    {
                        continue;
                    }
                    // asciidoctor's trailing-spec grammar is strictly SPAN-FIRST:
                    // span digits/operator come BEFORE any alignment. A candidate
                    // whose LEADING alignment is followed by a span/dup operator
                    // (`>2*`, `^2+`, `.^3+`) is align-FIRST — asciidoctor never
                    // reads it as a spec, so it stays literal content. acdc honors
                    // align-first only as a documented LINE-START back-compat; a
                    // mid-row token RECOVERED FROM CONTENT must not (else
                    // `Speedup was >2*|x` loses `>2*` and mis-spans / drops the row).
                    // Align-ONLY (`^`, `.^`) and span-first (`2+`, `2+^`) are
                    // unaffected (no leading align, or no trailing operator).
                    let (_, _, align_end) =
                        CellSpecifier::parse_alignments(candidate.as_bytes(), 0);
                    if align_end > 0
                        && candidate
                            .get(align_end..)
                            .is_some_and(|rest| rest.contains('+') || rest.contains('*'))
                    {
                        continue;
                    }
                    let (spec, spec_len) = CellSpecifier::parse(candidate, ParseContext::FirstPart);
                    if spec_len == 0 || spec_len != candidate.len() {
                        continue;
                    }
                    // Bare style letters (no span operator, no halign/valign)
                    // can't survive a prepend-then-reparse round trip because
                    // the per-part pass uses InlineContent grammar (which
                    // rejects style-only AND span/dup specifiers to avoid
                    // "another" → 'a' / "2+2" → colspan-2 false positives).
                    // Stash the recognized spec on the CellPart so the per-part
                    // pass applies it directly instead of re-parsing it.
                    //
                    // Recovery applies to BOTH style-only (`a|`) AND span/dup
                    // (`2+|`, `.2+|`, `2*|`) candidates uniformly: the candidate
                    // is validated under FirstPart grammar (which DOES accept
                    // span/dup), the recognized CellSpecifier is stashed on
                    // CUR.forced_spec, and PREV's tail is trimmed to excise the
                    // candidate.
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

            // Determine if first part should be treated as content or specifier/skip.
            // For PSV: first part is before leading separator OR a cell-
            // spec like `.3+`/`2+`/`a` — skip/absorb. For DSV/TSV/CSV the
            // first part is content. Use `is_psv` directly — the
            // `psv_skip_first` alias is redundant.
            for (i, part) in parts.iter().enumerate() {
                if i == 0 && is_psv {
                    // First part is before first separator (PSV format only)
                    let trimmed = part.content.trim();
                    if !trimmed.is_empty() {
                        // Check if this looks like a specifier (e.g., "2+", "3*", "^.>", "s")
                        // Style-only specifiers (e.g., "s" for strong) are valid here.
                        // asciidoctor requires the spec to be FLUSH against the first
                        // `|`: a trailing space (`d |e`, `2+ |x`) makes the token
                        // literal CONTENT that continues the previous cell, NOT a
                        // spec for the next one. Without the flush guard, `.trim()`
                        // read `d ` as the style spec `d` and dropped the content.
                        let flush = !part.content.trim_start().ends_with(char::is_whitespace);
                        let (spec, spec_len) =
                            CellSpecifier::parse(trimmed, ParseContext::FirstPart);
                        if flush && spec_len > 0 && spec_len == trimmed.len() {
                            // Entire first part is a specifier, apply to next cell
                            pending_spec = Some(spec);
                        } else if let Some(last_cell) = columns.last_mut() {
                            // Non-spec content before the first separator on a
                            // MID-ROW line is CONTINUATION of the previous
                            // multi-line cell — asciidoctor keeps it attached
                            // rather than dropping it. Example (`[cols="1l,1d"]`):
                            //     |m1
                            //     m2 | tail
                            // → cell1 = `m1\nm2`, cell2 = `tail`. Row grouping
                            // (count_cell_colspans) already placed this line in
                            // the current row because `|m1` underfills ncols. This
                            // fires only when `parts[0]` is non-empty (the line
                            // does NOT start with the separator — a `|`-led line
                            // like `|rated 5*|next` has an empty `parts[0]` and is
                            // untouched) AND a previous cell exists (on a row's
                            // FIRST line `columns` is empty, so genuine before-
                            // first-separator content is still dropped).
                            let leading_ws = part.content.len() - part.content.trim_start().len();
                            let cont_start = current_offset + part.start + leading_ws;
                            if last_cell.content.is_empty() {
                                last_cell.content_start = cont_start;
                            } else {
                                last_cell.content.push('\n');
                            }
                            last_cell.content.push_str(trimmed);
                            let cont_end = cont_start + trimmed.len().saturating_sub(1);
                            if cont_end > last_cell.end {
                                last_cell.end = cont_end;
                            }
                        }
                        // else (no previous cell): row's first line — genuine
                        // before-first-separator content, skipped for PSV.
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
                    let (s, off) =
                        CellSpecifier::parse(cell_content_trimmed, ParseContext::InlineContent);
                    // asciidoctor parity: a span/dup token that consumes the
                    // ENTIRE cell (`|2+|`) is literal content, not a specifier —
                    // only a PREFIX spec (content follows it in the same cell,
                    // e.g. `2+wide`) applies. Reject whole-cell consumption so
                    // the token survives as the cell's text (never dropped).
                    if off > 0 && off == cell_content_trimmed.len() {
                        (CellSpecifier::default(), 0)
                    } else {
                        (s, off)
                    }
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
        let rows = Table::parse_rows_with_positions(
            input,
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            None,
        )
        .expect("table should parse");
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
        let rows = Table::parse_rows_with_positions(
            input,
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            Some(1),
        )
        .expect("table should parse");
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
        let rows = Table::parse_rows_with_positions(
            input,
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            None,
        )
        .expect("table should parse");
        // Tightened from `!rows.is_empty()` — multibyte regression silently
        // dropped the trailing `.2+`, leaving 2 cells with rowspan=1.
        assert_eq!(rows[0].len(), 3, "expected 3 cells: food, apple, 10");
        assert_eq!(rows[0][0].content, "food");
        assert_eq!(rows[0][0].rowspan, 2);
        assert_eq!(rows[0][1].content, "apple");
        assert_eq!(rows[0][1].rowspan, 2);
    }

    /// asciidoctor parity (cron #11 decision): a `5*`/`2+` token that is
    /// whitespace-separated from cell content and flush against the next `|`
    /// IS a duplication/colspan specifier — asciidoctor 2.0.26 parses
    /// `|rated 5*|next` as `rated` + `next`×5 and `|5G coverage 2+|GB` as
    /// `5G coverage` + `GB` spanning two columns. Glyph is a strict AsciiDoc
    /// editor (L2a MUST match asciidoctor), so the specifier is honored even
    /// when the preceding content reads like natural language. (The `.`-flush
    /// literal case `|2+|` and the trailing-space case `x 2+ |` stay literal —
    /// see the cell-spec fixtures.)
    #[test]
    fn star_rating_syntax_parses_as_duplication_spec() {
        let input = "|rated 5*|next\n";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            input,
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            None,
        )
        .expect("table should parse");
        let row = &rows[0];
        assert_eq!(
            row.first().map(|c| c.content.as_str()),
            Some("rated"),
            "`5*` is a duplication specifier, not part of the cell text",
        );
        // The `5*` dup is recognized and expanded into 5 independent `next`
        // cells (grid_reflow expands duplication up front so copies can wrap
        // across grid rows). Implicit ncols = 1 + 5 = 6, so all land in one row.
        let next_copies = row.iter().filter(|c| c.content == "next").count();
        assert_eq!(next_copies, 5, "`5*` duplicates `next` into 5 cells");
        assert!(
            row.iter().all(|c| c.duplication_count == 1),
            "expanded copies carry duplication_count 1"
        );
    }

    #[test]
    fn version_number_syntax_parses_as_colspan_spec() {
        let input = "|5G coverage 2+|GB\n";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            input,
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            None,
        )
        .expect("table should parse");
        assert_eq!(
            rows[0].first().map(|c| c.content.as_str()),
            Some("5G coverage"),
            "`2+` is a colspan specifier, not part of the cell text",
        );
        assert_eq!(
            rows[0].get(1).map(|c| c.colspan),
            Some(2),
            "next cell spans two columns via `2+`",
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
        let rows = Table::parse_rows_with_positions(
            input,
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            None,
        )
        .expect("table should parse");
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
        let input =
            ".2+|food\n|apple .2+|10\n|banana\n.3+|drink |cola |30\n|tea |15\n|coffee |12\n";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            input,
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            Some(3),
        )
        .expect("table should parse");
        // The food half must be present (this is the regression — without the
        // multi-line layout fix, all the food/apple/10/banana cells get
        // bundled into one over-full row and the visible table loses food).
        assert!(
            rows.len() >= 2,
            "expected at least 2 rows, got {}",
            rows.len()
        );
        // Row 0: food (rs=2, colspan=1), apple (cs=1), 10 (rs=2, cs=1).
        assert_eq!(
            rows[0].len(),
            3,
            "row 0 should have 3 cells: food, apple, 10"
        );
        assert_eq!(rows[0][0].content, "food");
        assert_eq!(rows[0][0].rowspan, 2);
        assert_eq!(rows[0][1].content, "apple");
        assert_eq!(rows[0][2].content, "10");
        assert_eq!(rows[0][2].rowspan, 2);
        // Row 1: just banana (col 1; col 0 and col 2 are phantoms).
        assert_eq!(rows[1].len(), 1, "row 1 should have 1 cell: banana");
        assert_eq!(rows[1][0].content, "banana");
    }

    /// An `a|` block cell in a NON-last column must not split the row — inline
    /// cells that follow it on later lines flow into the SAME row until `ncols`
    /// (asciidoctor's cell-accumulation model). Regression: the Glyph pricing
    /// table `|Format |A |B` / `a|`⏎`- x`⏎`- y` / `|D |E` rendered as 3 rows
    /// (D/E spilled onto their own row) instead of one row.
    #[test]
    fn a_block_cell_in_non_last_column_keeps_row_intact() {
        let input = "|F |A\na|\n- x\n- y\n|D\n";
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            input,
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            Some(4),
        )
            .expect("table should parse");
        let shape: Vec<usize> = rows.iter().map(std::vec::Vec::len).collect();
        assert_eq!(rows.len(), 1, "expected one 4-col row, got shape {shape:?}");
        assert_eq!(rows[0].len(), 4, "row should have 4 cells: F, A, a|, D");
        assert_eq!(rows[0][0].content, "F");
        assert_eq!(rows[0][1].content, "A");
        assert!(
            rows[0][2].content.contains('x') && rows[0][2].content.contains('y'),
            "col2 is the a| list, got {:?}",
            rows[0][2].content
        );
        assert_eq!(rows[0][3].content, "D");
    }

    /// An alignment-only cell spec (`^`, `<`, `>`) mid-row — flush against the
    /// `|` — must be recognized as the NEXT cell's spec (asciidoctor grammar),
    /// not leaked into the previous cell's content. Regression: `|a ^|b |c`
    /// dropped the `^` (Glyph mid-row alignment bug) because the trailing-spec
    /// recovery required a `+`/`*` span operator.
    #[test]
    fn mid_row_alignment_only_spec_recognized() {
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            "|a ^|b |c\n",
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            Some(3),
        )
            .expect("table should parse");
        assert_eq!(rows.len(), 1, "one 3-col row");
        assert_eq!(rows[0].len(), 3);
        assert_eq!(rows[0][0].content, "a");
        assert_eq!(rows[0][1].content, "b");
        assert_eq!(
            rows[0][1].halign,
            Some(HorizontalAlignment::Center),
            "cell b centered via mid-row `^`"
        );
        assert_eq!(rows[0][2].content, "c");
    }

    /// Implicit column count (no `[cols=]`) must use the first row's grid WIDTH
    /// including duplication (`N*`), not a bare colspan sum. `3*|Same` is a
    /// 3-column-wide first row → the table is 2 rows, not re-chunked to 4. The
    /// `3*` is expanded up front into 3 independent `Same` cells (so a dup cell
    /// can wrap across grid rows); each expanded copy has duplication_count 1.
    #[test]
    fn grid_reflow_ncols_honors_duplication_width() {
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            "3*|Same\n|X |Y |Z\n",
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            None, // implicit ncols — derived from row 0's width
        )
            .expect("table should parse");
        let shape: Vec<usize> = rows.iter().map(std::vec::Vec::len).collect();
        assert_eq!(rows.len(), 2, "2 rows (Same×3, then X/Y/Z); got {shape:?}");
        assert_eq!(
            rows[0]
                .iter()
                .map(|c| c.content.as_str())
                .collect::<Vec<_>>(),
            vec!["Same", "Same", "Same"],
            "the `3*` dup is expanded into 3 independent Same cells"
        );
        assert!(rows[0].iter().all(|c| c.duplication_count == 1));
        assert_eq!(
            rows[1]
                .iter()
                .map(|c| c.content.as_str())
                .collect::<Vec<_>>(),
            vec!["X", "Y", "Z"]
        );
    }

    /// A grid row wholly covered by a rowspan from above: asciidoctor consumes
    /// EXACTLY ONE cell as the phantom filler and DISCARDS it. grid_reflow does
    /// the same AND emits the (now empty) phantom row — the emit is what lets the
    /// downstream builder age its own rowspan tracker on this grid row (it drops
    /// the empty row from output; the covering `rowspan` then overlaps the next
    /// real row, matching asciidoctor). `2.2+|Big` (colspan==ncols, rowspan 2) +
    /// `a|b` → `[[Big],[],[b]]`: `a` is the discarded filler, `[]` is the phantom
    /// row Big spans, `b` lands after. BOUNDED (one cell consumed per phantom row)
    /// — see `grid_reflow_oversized_rowspan_is_bounded`.
    #[test]
    fn grid_reflow_full_width_rowspan_discards_one_phantom_filler() {
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            "2.2+|Big\n|a |b\n",
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            Some(2),
        )
            .expect("table should parse");
        let shape: Vec<usize> = rows.iter().map(std::vec::Vec::len).collect();
        assert_eq!(rows.len(), 3, "Big row, phantom row, b row; got {shape:?}");
        assert_eq!(rows[0][0].content, "Big");
        assert!(
            rows[1].is_empty(),
            "grid row 1 is the emitted phantom Big spans"
        );
        // `a` (the filler for Big's covered row) is discarded; `b` survives.
        let contents: Vec<&str> = rows.iter().flatten().map(|c| c.content.as_str()).collect();
        assert!(contents.contains(&"Big") && contents.contains(&"b"));
        assert!(!contents.contains(&"a"), "phantom filler `a` is discarded");
    }

    /// P0 REGRESSION PIN — an oversized rowspan must NOT amplify into O(rowspan)
    /// phantom rows. `.999+|X` in a 1-col table followed by one trailing cell used
    /// to emit ~998 empty rows (unbounded — a ~10-byte OOM/hang, exploitable via a
    /// pasted table). grid_reflow now consumes+discards one filler cell per fully
    /// covered row, so the row count is bounded by the CELL count, not the rowspan.
    #[test]
    fn grid_reflow_oversized_rowspan_is_bounded() {
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            ".999+|X\n|Y\n",
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            Some(1),
        )
            .expect("table should parse");
        assert!(
            rows.len() <= 2,
            "row count bounded by cells (2), not rowspan (999); got {}",
            rows.len()
        );
        assert_eq!(rows[0][0].content, "X");
    }

    /// Implicit `ncols` (no `[cols=]`) is the FIRST LOGICAL ROW's width, which
    /// includes a wrapped first cell's continuation line. `|a |b is` / `long |c`
    /// is one 3-column logical row (asciidoctor ncols=3), NOT `|a |b` (ncols 2)
    /// from just the first physical line.
    #[test]
    fn implicit_ncols_uses_first_logical_row_not_physical_line() {
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            "|a |b is\nlong |c\n",
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            None, // implicit — derived from the first LOGICAL row
        )
            .expect("table should parse");
        let shape: Vec<usize> = rows.iter().map(std::vec::Vec::len).collect();
        assert_eq!(rows.len(), 1, "one 3-col logical row; got {shape:?}");
        assert_eq!(rows[0].len(), 3, "cells a / (b is long) / c");
        assert_eq!(rows[0][0].content, "a");
        assert_eq!(rows[0][2].content, "c");
    }

    /// A wrapped FIRST cell whose continuation carries the row's other cells must
    /// still count toward implicit `ncols`. `| header\nmore | Col2` → 2 columns.
    #[test]
    fn implicit_ncols_wrapped_first_cell_counts_continuation() {
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            "| header\nmore | Col2\n\n| x | y\n",
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            None,
        )
            .expect("table should parse");
        let shape: Vec<usize> = rows.iter().map(std::vec::Vec::len).collect();
        assert_eq!(
            rows.len(),
            2,
            "2 rows of 2 cols (ncols=2), not 4×1; got {shape:?}"
        );
        assert_eq!(
            rows[1]
                .iter()
                .map(|c| c.content.as_str())
                .collect::<Vec<_>>(),
            vec!["x", "y"]
        );
    }

    /// A wrapped first cell whose CONTINUATION line carries a flush colspan spec
    /// must have that spec recovered — `|Long desc\nwraps here 2+|Wide` →
    /// asciidoctor gives `[desc wraps here, Wide:c2]`, ncols=3 (well-formed). The
    /// `2+` must NOT leak into the wrapped cell content, and implicit ncols is 3.
    #[test]
    fn continuation_line_flush_span_spec_is_recovered() {
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            "|Long desc\nwraps here 2+|Wide\n|a |b |c\n",
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            None,
        )
            .expect("table should parse");
        let shape: Vec<usize> = rows.iter().map(std::vec::Vec::len).collect();
        assert_eq!(rows.len(), 2, "2 rows (ncols=3); got {shape:?}");
        assert_eq!(rows[0].len(), 2, "wrapped desc + Wide");
        assert!(
            !rows[0][0].content.contains("2+"),
            "the `2+` spec must not leak into the wrapped cell content: {:?}",
            rows[0][0].content
        );
        assert_eq!(rows[0][1].content, "Wide");
        assert_eq!(
            rows[0][1].colspan, 2,
            "Wide gets colspan 2 from the recovered `2+`"
        );
        assert_eq!(
            rows[1]
                .iter()
                .map(|c| c.content.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    /// asciidoctor is span-first ONLY: an align-FIRST token (`>2*`, `^2+` — align
    /// before the span operator) is literal content, never a spec. The mid-row
    /// recovery must NOT steal it — from a `|`-led line OR a content-bearing
    /// continuation line — else `Speedup was >2*|x` loses `>2*` and the next cell
    /// is spuriously spanned/duplicated (dropping the row when it over-counts).
    #[test]
    fn align_first_with_span_not_stolen_mid_row() {
        for (src, marker, next) in [
            // content-bearing continuation line (the broadened-gate regression)
            ("|base\nmore ^2+|exp\n", "^2+", "exp"),
            // `|`-led single line (the pre-existing steal — same root)
            ("|Speedup was >2*|confirmed\n", ">2*", "confirmed"),
        ] {
            let mut has_header = false;
            let rows = Table::parse_rows_with_positions(
                src,
                &Separator::new("|"),
                true,
                false,
                &mut has_header,
                0,
                Some(2),
            )
                .expect("table should parse");
            assert!(
                rows[0][0].content.contains(marker),
                "align-first `{marker}` must stay content in {src:?}: got {:?}",
                rows[0][0].content
            );
            assert_eq!(rows[0][1].content, next);
            assert_eq!(
                rows[0][1].colspan, 1,
                "`{marker}` must not span the next cell"
            );
            assert_eq!(
                rows[0][1].duplication_count, 1,
                "`{marker}` must not duplicate the next cell"
            );
        }
    }

    /// A repeated alignment run flush against `|` (`<<`, `^^`, `<>`) is NOT a
    /// cell spec — it stays literal content (asciidoctor keeps `foo <<`). Only a
    /// SINGLE halign/valign is a spec, so mid-row recovery must not steal the run.
    #[test]
    fn mid_row_repeated_align_run_stays_content() {
        for run in ["<<", "^^", ">>", "<>"] {
            let mut has_header = false;
            let src = format!("|foo {run}|bar\n");
            let rows = Table::parse_rows_with_positions(
                &src,
                &Separator::new("|"),
                true,
                false,
                &mut has_header,
                0,
                Some(2),
            )
                .expect("table should parse");
            assert_eq!(
                rows[0][0].content,
                format!("foo {run}"),
                "run `{run}` must stay in cell content, not be deleted"
            );
            assert_eq!(rows[0][1].content, "bar");
            assert!(
                rows[0][1].halign.is_none(),
                "cell 2 gets no spurious alignment from `{run}`"
            );
        }
    }

    /// An align char followed by DIGITS (`<5`, `^100`, `>50`) flush against `|`
    /// is NOT a cell spec — the digits are only a span when an operator follows.
    /// Mid-row recovery must keep the whole token as content (asciidoctor does),
    /// not delete it and mis-align the next cell.
    #[test]
    fn mid_row_align_then_digit_stays_content() {
        for tok in ["<5", "^100", ">50"] {
            let mut has_header = false;
            let src = format!("|latency {tok}|ms\n");
            let rows = Table::parse_rows_with_positions(
                &src,
                &Separator::new("|"),
                true,
                false,
                &mut has_header,
                0,
                Some(2),
            )
                .expect("table should parse");
            assert_eq!(
                rows[0][0].content,
                format!("latency {tok}"),
                "`{tok}` must stay in cell content, not be deleted"
            );
            assert_eq!(rows[0][1].content, "ms");
            assert!(
                rows[0][1].halign.is_none(),
                "cell 2 gets no spurious alignment from `{tok}`"
            );
        }
    }

    /// A span/dup token in a cell's INTERIOR content (after the `|`) is literal
    /// text, never a specifier — asciidoctor only reads a spec BEFORE the `|`.
    /// `| 2+2 | z` keeps `2+2`; pre-fix acdc's InlineContent parse stripped the
    /// leading `2+` prefix, silently deleting bytes → cell content `2` (data loss
    /// on well-formed input such as a `2+2` math cell). Also covers `3*n` (dup),
    /// `.2+r` (rowspan), `2.3+w` (both).
    #[test]
    fn cell_interior_span_dup_prefix_is_literal_content() {
        for (src, kept, colspan, rowspan) in [
            ("| 2+2 | z\n", "2+2", 1, 1),
            ("| 3*n | z\n", "3*n", 1, 1),
            ("| .2+r | z\n", ".2+r", 1, 1),
            ("| 2.3+w | z\n", "2.3+w", 1, 1),
        ] {
            let mut has_header = false;
            let rows = Table::parse_rows_with_positions(
                src,
                &Separator::new("|"),
                true,
                false,
                &mut has_header,
                0,
                Some(2),
            )
                .expect("table should parse");
            assert_eq!(
                rows[0][0].content, kept,
                "interior `{kept}` must stay intact, not be parsed as a spec"
            );
            assert_eq!(
                rows[0][0].colspan, colspan,
                "no phantom colspan from `{kept}`"
            );
            assert_eq!(
                rows[0][0].rowspan, rowspan,
                "no phantom rowspan from `{kept}`"
            );
            assert_eq!(rows[0][1].content, "z");
        }
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
        let rows = Table::parse_rows_with_positions(
            input,
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            None,
        )
        .expect("table should parse");
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
        let rows = Table::parse_rows_with_positions(
            input,
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            None,
        )
        .expect("table should parse");
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
            count_unescaped_separators("\\| Col1 \\| Col2 \\|", &Separator::new("|")),
            0,
            "all 3 pipes are escaped — must count 0",
        );
        assert_eq!(
            count_unescaped_separators("| real | sep |", &Separator::new("|")),
            3,
            "all 3 pipes unescaped — must count 3",
        );
        assert_eq!(
            count_unescaped_separators("\\| escaped | unescaped", &Separator::new("|")),
            1,
            "first pipe escaped, second not — must count 1",
        );
        // Same for bang separator (variant for nested tables).
        assert_eq!(
            count_unescaped_separators("\\! nested \\!", &Separator::new("!")),
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
        let n = count_cell_colspans(line, &Separator::new("|"), true);
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
        let n = count_cell_colspans("a|", &Separator::new("|"), true);
        assert_eq!(n, 1, "standalone `a|` is a continuation cell (got {n})");
    }

    /// count_cell_colspans MUST agree with parse_row_with_positions on the same
    /// line — the invariant that keeps `accumulated_cols` in sync. `|2+ a|`: the
    /// `2+` sits AFTER the `|`, so it is literal content (asciidoctor never reads
    /// a spec from cell-interior text — verified `|2+ a|` → cells `['2+', '_']`,
    /// colspan sum 2), and the trailing ` a|` is a style-`a` cell. So parse_row
    /// produces 2 cells `[colspan=1 "2+", colspan=1 empty]` = 2, and the counter
    /// must also report 2. (Pre-fix acdc wrongly parsed the interior `2+` as a
    /// colspan-2 spec and both reported 3 — a self-baselined divergence from
    /// asciidoctor, closed by gating span/dup parsing on FirstPart.)
    #[test]
    fn count_cell_colspans_matches_parse_row_for_inline_span_style() {
        let line = "|2+ a|";
        let count = count_cell_colspans(line, &Separator::new("|"), true);
        let mut has_header = false;
        let rows = Table::parse_rows_with_positions(
            line,
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            None,
        )
        .expect("table should parse");
        let parsed_colspan_sum: usize = rows[0].iter().map(|c| c.colspan).sum();
        assert_eq!(
            count, parsed_colspan_sum,
            "count_cell_colspans({line:?}) must equal sum of parse_row colspans \
             (got count={count}, parse_row sum={parsed_colspan_sum})"
        );
        assert_eq!(
            count, 2,
            "interior `2+` is content (colspan 1) + style-`a` cell"
        );
    }

    /// The two column-accounting sites must agree for EVERY cell-spec shape.
    ///
    /// `count_cell_colspans` drives the ncols-aware row break while collecting
    /// lines; `occupied_columns` drives the merge-into-incomplete-row decision
    /// after parsing. If they disagree, a row is split or merged against a width
    /// the other half of the code never agreed to — the failure that let
    /// `[cols="100*"]` with `100*| x` per line collapse 100 SOURCE LINES into one
    /// logical row and then expand it to 10 000 columns.
    ///
    /// The sibling test above pins one span case with bare `colspan`, which is
    /// exactly the accounting that is WRONG for duplication: `k*` parses to
    /// `colspan: 1, duplication_count: k` (the two are mutually exclusive — one
    /// is always 1), so summing `colspan` alone reports 1 for a cell that
    /// occupies k columns. Enumerating the shapes here keeps that hole closed.
    #[test]
    fn column_accounting_sites_agree_for_every_spec_shape() {
        let sep = Separator::new("|");
        // (line, expected occupied columns)
        let cases: &[(&str, usize)] = &[
            ("| a | b", 2),          // plain cells
            ("2+| a | b", 3),        // colspan 2 + 1
            ("3*| same", 3),         // duplication 3
            ("2*| a | b", 3),        // duplication 2 + 1
            ("100*| same", 100),     // the boundary the resource limit sits on
            (".2+| a | b", 2),       // rowspan does not widen the row
            ("^2+| a", 2),           // alignment + colspan
            ("3*s| a", 3),           // duplication + style letter
            ("2.3+| a", 2),          // colspan 2, rowspan 3
        ];
        for (line, expected) in cases {
            let counted = count_cell_colspans(line, &sep, true);
            let mut has_header = false;
            let rows =
                Table::parse_rows_with_positions(line, &sep, true, false, &mut has_header, 0, None)
                    .expect("table should parse");
            let occupied: usize = rows[0].iter().map(occupied_columns).sum();
            assert_eq!(
                counted, occupied,
                "{line:?}: the pre-parse count ({counted}) and the post-parse \
                 occupancy ({occupied}) must agree"
            );
            assert_eq!(
                occupied, *expected,
                "{line:?}: expected {expected} occupied columns, got {occupied}"
            );
        }
    }

    /// A `k*` cell must not make the collector swallow the following source
    /// lines. Pins the row SHAPE, not just the accounting: with `[cols="4*"]`
    /// each `4*| x` line is a complete row of its own, so three lines are three
    /// rows — the under-count merged them into one.
    #[test]
    fn duplication_cells_do_not_merge_following_rows() {
        let mut has_header = false;
        let input = "4*| a\n4*| b\n4*| c\n";
        let rows = Table::parse_rows_with_positions(
            input,
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            0,
            Some(4),
        )
        .expect("table should parse");
        assert_eq!(rows.len(), 3, "three `4*` lines are three rows, got {rows:?}");
        for row in &rows {
            let occupied: usize = row.iter().map(occupied_columns).sum();
            assert_eq!(occupied, 4, "each row fills the declared 4 columns");
        }
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
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            /* base_offset */ 0,
            None,
        )
        .expect("table should parse");
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
            &Separator::new("|"),
            true,
            false,
            &mut has_header,
            /* base_offset */ 0,
            None,
        )
        .expect("table should parse");
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

    // asciidoctor-standard SPAN-FIRST cell specifiers (`2+^`, `.3+^.^`) — the
    // form asciidoctor emits. acdc historically only parsed ALIGN-FIRST
    // (`^2+`); both orders must now round-trip to the same specifier.
    #[test]
    fn cell_specifier_accepts_span_first_alignment() {
        // colspan + halign, span-first vs align-first
        let (sf, _) = CellSpecifier::parse("2+^", ParseContext::FirstPart);
        assert_eq!(sf.colspan, 2);
        assert_eq!(sf.halign, Some(HorizontalAlignment::Center));
        let (af, _) = CellSpecifier::parse("^2+", ParseContext::FirstPart);
        assert_eq!((af.colspan, af.halign), (sf.colspan, sf.halign));

        // rowspan + halign + valign, span-first (`.3+^.^`)
        let (sf, _) = CellSpecifier::parse(".3+^.^", ParseContext::FirstPart);
        assert_eq!(sf.rowspan, 3);
        assert_eq!(sf.halign, Some(HorizontalAlignment::Center));
        assert_eq!(sf.valign, Some(VerticalAlignment::Middle));
        let (af, _) = CellSpecifier::parse("^.^.3+", ParseContext::FirstPart);
        assert_eq!(
            (af.rowspan, af.halign, af.valign),
            (sf.rowspan, sf.halign, sf.valign)
        );

        // colspan.rowspan + right/top, span-first (`2.3+>.<`)
        let (sf, _) = CellSpecifier::parse("2.3+>.<", ParseContext::FirstPart);
        assert_eq!((sf.colspan, sf.rowspan), (2, 3));
        assert_eq!(sf.halign, Some(HorizontalAlignment::Right));
        assert_eq!(sf.valign, Some(VerticalAlignment::Top));

        // span-first alignment followed by a style letter (`2+^m`)
        let (sf, len) = CellSpecifier::parse("2+^m", ParseContext::FirstPart);
        assert_eq!(sf.colspan, 2);
        assert_eq!(sf.halign, Some(HorizontalAlignment::Center));
        assert!(sf.style.is_some());
        assert_eq!(len, 4);
    }
}

