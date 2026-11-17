//! Asciidoctor's list reader, line for line (`Parser.parse_list`,
//! `Parser.read_lines_for_list_item`): which lines each item of an unordered
//! or ordered list holds. An item's lines are what Asciidoctor parses as its
//! content (`next_block` over the item's own reader), so a list nested in it
//! is read again from those lines alone, confined as Asciidoctor confines it.

use super::lists::{ListMarker, OrdinaryKey};

/// `ListContinuationMarker`: a `+` line stays one (`ListContinuationString`)
/// or becomes an empty line standing for it (`ListContinuationPlaceholder`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LineKind {
    Source,
    Continuation,
    Placeholder,
}

/// A source line, `start..end` without its line break.
#[derive(Clone, Copy, Debug)]
pub(super) struct Line {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) kind: LineKind,
}

impl Line {
    pub(super) fn text<'a>(&self, input: &'a str) -> &'a str {
        if self.kind == LineKind::Placeholder { "" } else { &input[self.start..self.end] }
    }

    fn continues(&self) -> bool {
        self.kind != LineKind::Source
    }
}

pub(super) struct ItemLines {
    pub(super) marker_line: Line,
    pub(super) buffer: Vec<Line>,
}

pub(super) struct ListLines {
    pub(super) items: Vec<ItemLines>,
    /// The end of the last line the reader consumed for an item, dropped
    /// continuations included.
    pub(super) consumed_end: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Unordered,
    Ordered,
    Description,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Continuation {
    Inactive,
    Active,
    Frozen,
}

struct Reader<'a> {
    input: &'a str,
    lines: Vec<Line>,
    next: usize,
}

impl<'a> Reader<'a> {
    /// The lines from `start` to `limit`, ending before the first one `stop`
    /// claims for an enclosing reader; an empty line starting at one of
    /// `placeholders` is still a `ListContinuationPlaceholder`.
    fn new(input: &'a str, start: usize, limit: usize, placeholders: &[usize], stop: &dyn Fn(&str) -> bool) -> Self {
        let mut lines = Vec::new();
        let mut at = start;
        while at < limit {
            let end = input[at..limit].find('\n').map_or(limit, |n| at + n);
            if !lines.is_empty() && stop(&input[at..end]) {
                break;
            }
            let kind = if at == end && placeholders.binary_search(&at).is_ok() { LineKind::Placeholder } else { LineKind::Source };
            lines.push(Line { start: at, end, kind });
            at = end + 1;
        }
        Self { input, lines, next: 0 }
    }

    /// The end of the last line read and not given back.
    fn consumed_end(&self) -> Option<usize> {
        self.next.checked_sub(1).map(|last| self.lines[last].end)
    }

    fn text(&self, line: Line) -> &'a str {
        line.text(self.input)
    }

    fn peek(&self) -> Option<Line> {
        self.lines.get(self.next).copied()
    }

    fn read_line(&mut self) -> Option<Line> {
        let line = self.peek()?;
        self.next += 1;
        Some(line)
    }

    fn unshift(&mut self, line: Line) {
        self.next -= 1;
        self.lines[self.next] = line;
    }

    /// `Reader#skip_blank_lines`: `None` once no line is left.
    fn skip_blank_lines(&mut self) -> Option<usize> {
        let mut skipped = 0;
        while let Some(line) = self.peek() {
            if !self.text(line).is_empty() {
                return Some(skipped);
            }
            self.next += 1;
            skipped += 1;
        }
        None
    }

    /// `read_lines_until terminator:, read_last_line: true`.
    fn read_until_terminator(&mut self, terminator: &str) -> Vec<Line> {
        let mut lines = Vec::new();
        while let Some(line) = self.read_line() {
            lines.push(line);
            if self.text(line) == terminator {
                break;
            }
        }
        lines
    }

    /// `read_lines_until preserve_last_line: true, break_on_blank_lines: true,
    /// break_on_list_continuation: true`, also ending before a line `stop`
    /// claims.
    fn read_until_blank_or_continuation(&mut self, stop: &dyn Fn(&str) -> bool) -> Vec<Line> {
        let mut lines: Vec<Line> = Vec::new();
        while let Some(line) = self.read_line() {
            let text = self.text(line);
            if text.is_empty() || (!lines.is_empty() && text == "+") || stop(text) {
                self.unshift(line);
                break;
            }
            lines.push(line);
        }
        lines
    }
}

fn after_marker(line: &str, marker_end: usize) -> Option<usize> {
    let rest = &line[marker_end..];
    let blanks = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    (blanks > 0).then_some(marker_end + blanks)
}

/// `UnorderedListRx`: the marker and where the text starts.
fn unordered_item(line: &str) -> Option<(&str, usize)> {
    let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
    let body = &line[lead..];
    let len = if body.starts_with('-') {
        1
    } else if body.starts_with('*') {
        body.len() - body.trim_start_matches('*').len()
    } else if body.starts_with('\u{2022}') {
        '\u{2022}'.len_utf8()
    } else {
        return None;
    };
    after_marker(line, lead + len).map(|text| (&line[lead..lead + len], text))
}

/// `OrderedListRx`: `\.\.*|\d+\.|[a-zA-Z]\.|[IVXivx]+\)`.
fn ordered_item(line: &str) -> Option<(&str, usize)> {
    let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
    let body = &line[lead..];
    let bytes = body.as_bytes();
    let dots = bytes.iter().take_while(|&&b| b == b'.').count();
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    let numeral = bytes.iter().take_while(|b| b"IVXivx".contains(b)).count();
    let candidates = [
        (dots > 0).then_some(dots),
        (digits > 0 && bytes.get(digits) == Some(&b'.')).then_some(digits + 1),
        (bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b'.')).then_some(2),
        (numeral > 0 && bytes.get(numeral) == Some(&b')')).then_some(numeral + 1),
    ];
    candidates
        .into_iter()
        .flatten()
        .find_map(|len| after_marker(line, lead + len).map(|text| (&line[lead..lead + len], text)))
}

/// A callout item line with text, as `at_callout_list_item` reads it: one an
/// enclosing callout list owns.
pub(super) fn callout_parent_line(line: &str) -> bool {
    callout_item(line) && !line[line.find('>').map_or(0, |at| at + 1)..].trim_start_matches([' ', '\t']).is_empty()
}

pub(super) fn is_unordered_marker(marker: &str) -> bool {
    marker.starts_with(['*', '-', '\u{2022}'])
}

/// `CalloutListRx`.
fn callout_item(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('<') else { return false };
    let Some(close) = rest.find('>') else { return false };
    let number = &rest[..close];
    (number == "." || (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())))
        && rest[close + 1..].starts_with([' ', '\t'])
}

/// A description item line, offsets within the line.
#[derive(Clone, Copy, Debug)]
pub(super) struct DescriptionLine<'a> {
    pub(super) term: (usize, usize),
    pub(super) delimiter: &'a str,
    pub(super) delimiter_start: usize,
    /// Where the item text starts, when the line has any.
    pub(super) text: Option<usize>,
}

/// `DescriptionListRx`, or with `delimiter` the sibling pattern
/// `DescriptionListSiblingRx[delimiter]`, whose term never ends in `:` before
/// a colon delimiter.
pub(super) fn description_line<'a>(line: &'a str, delimiter: Option<&str>) -> Option<DescriptionLine<'a>> {
    if line.starts_with("//") && line.len() > 2 && !line[2..].starts_with('/') {
        return None;
    }
    let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
    let first = line[lead..].chars().next().filter(|c| *c != ' ' && *c != '\t')?;
    let bytes = line.as_bytes();
    let mut at = lead + first.len_utf8();
    while at < bytes.len() {
        let lengths: &[usize] = match (bytes[at], delimiter) {
            (b':', None) => &[4, 3, 2],
            (b';', None) => &[2],
            (_, Some(wanted)) if wanted.as_bytes()[0] == bytes[at] && (bytes[at] == b';' || bytes[at - 1] != b':') => {
                std::slice::from_ref(match wanted.len() { 4 => &4, 3 => &3, _ => &2 })
            }
            _ => &[],
        };
        for &len in lengths {
            let Some(found) = line.get(at..at + len) else { continue };
            if !found.bytes().all(|b| b == bytes[at]) {
                continue;
            }
            let rest = &line[at + len..];
            let text = rest.trim_start_matches([' ', '\t']);
            if rest.is_empty() || text.len() < rest.len() {
                return Some(DescriptionLine {
                    term: (lead, at),
                    delimiter: found,
                    delimiter_start: at,
                    text: (!text.trim_end().is_empty()).then_some(line.len() - text.len()),
                });
            }
        }
        at += line[at..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// `DescriptionListRx`: `Some(has_text)` for a description item line.
fn description_item(line: &str) -> Option<bool> {
    description_line(line, None).map(|found| found.text.is_some())
}

/// `AnyListRx`.
fn any_list(line: &str) -> bool {
    unordered_item(line).is_some() || ordered_item(line).is_some() || description_line(line, None).is_some() || callout_item(line)
}

/// The nested list type a line opens, and for a description item whether it
/// has text (`NESTABLE_LIST_CONTEXTS.find`, or `[:dlist]` once nested).
fn nested_list(line: &str, description_only: bool) -> Option<(Kind, bool)> {
    if !description_only {
        if unordered_item(line).is_some() {
            return Some((Kind::Unordered, true));
        }
        if ordered_item(line).is_some() {
            return Some((Kind::Ordered, true));
        }
    }
    description_item(line).map(|has_text| (Kind::Description, has_text))
}

/// `Parser.is_delimited_block?`: the terminator of the block a line opens.
fn delimited_block(line: &str) -> Option<&str> {
    const HEADS: [&str; 13] = ["--", "----", "....", "====", "****", "____", "++++", "|===", ",===", ":===", "!===", "////", "```"];
    let len = line.len();
    if len < 2 || !line.is_char_boundary(2) || !HEADS.iter().any(|head| head.starts_with(&line[..2])) {
        return None;
    }
    let (tip, line) = if len == 2 {
        (line, line)
    } else {
        let tip = if len < 5 { line } else { line.get(..4)? };
        if tip.starts_with('`') {
            if tip.len() == 4 {
                if tip == "````" || &tip[..3] != "```" {
                    return None;
                }
                (&tip[..3], &tip[..3])
            } else if tip == "```" {
                (tip, line)
            } else {
                return None;
            }
        } else if tip.len() == 3 {
            return None;
        } else {
            (tip, line)
        }
    };
    if !HEADS.contains(&tip) {
        return None;
    }
    let uniform = || {
        let tail = tip.as_bytes()[tip.len() - 1];
        tip.len() == 4 && line.as_bytes()[1..].iter().all(|&b| b == tail)
    };
    (line.len() == tip.len() || uniform()).then_some(line)
}

/// `BlockTitleRx`.
fn block_title(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('.') else { return false };
    let rest = rest.strip_prefix('.').unwrap_or(rest);
    rest.chars().next().is_some_and(|c| !matches!(c, ' ' | '\t' | '.'))
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `BlockAttributeLineRx`.
fn block_attribute_line(line: &str) -> bool {
    let Some(inner) = line.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) else { return false };
    let Some(first) = inner.chars().next() else { return true };
    if word_char(first) || ".#%{,\"'".contains(first) {
        return true;
    }
    let Some(anchor) = inner.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) else { return false };
    let (id, reftext) = anchor.split_once(',').map_or((anchor, None), |(id, rest)| (id, Some(rest)));
    let mut chars = id.chars();
    let valid_id = match chars.next() {
        None => reftext.is_none(),
        Some(c) => (c.is_alphabetic() || c == '_' || c == ':') && chars.all(|c| word_char(c) || matches!(c, '-' | ':' | '.')),
    };
    valid_id && reftext.is_none_or(|rest| !rest.trim_start_matches(' ').is_empty())
}

/// `AttributeEntryRx`.
fn attribute_entry(line: &str) -> bool {
    let Some(rest) = line.strip_prefix(':') else { return false };
    let rest = rest.strip_prefix('!').unwrap_or(rest);
    if !rest.chars().next().is_some_and(word_char) {
        return false;
    }
    let Some(colon) = rest.find(':') else { return false };
    let after = &rest[colon + 1..];
    after.is_empty() || after.starts_with([' ', '\t'])
}

fn literal_line(line: &str) -> bool {
    line.starts_with([' ', '\t'])
}

/// The list item marker on `line`, as the list type reads it.
fn item_marker(line: &str, kind: Kind) -> Option<ListMarker<'_>> {
    match kind {
        Kind::Unordered => unordered_item(line).map(|(marker, _)| ListMarker::Unordered(marker)),
        Kind::Ordered => ordered_item(line).map(|(marker, _)| ListMarker::Ordered(marker)),
        Kind::Description => None,
    }
}

fn is_sibling(line: &str, kind: Kind, trait_: ListMarker<'_>) -> bool {
    match trait_ {
        ListMarker::Description(delimiter) => description_line(line, Some(delimiter)).is_some(),
        _ => item_marker(line, kind).is_some_and(|marker| OrdinaryKey::siblings(trait_, marker)),
    }
}

fn read_lines_for_list_item(reader: &mut Reader<'_>, kind: Kind, trait_: ListMarker<'_>, mut has_text: bool) -> Vec<Line> {
    let dlist = kind == Kind::Description;
    let mut buffer: Vec<Line> = Vec::new();
    let mut continuation = Continuation::Inactive;
    let mut within_nested_list = false;
    let mut detached_continuation = None;
    let mut pending = None;
    let sibling = |line: &str| dlist && is_sibling(line, kind, trait_);

    while let Some(mut this_line) = reader.read_line() {
        pending = Some(this_line);
        if is_sibling(reader.text(this_line), kind, trait_) {
            break;
        }
        if reader.text(this_line) == "+" {
            this_line.kind = LineKind::Continuation;
            pending = Some(this_line);
        }
        let prev_line = buffer.last().copied();

        if prev_line.is_some_and(|line| line.continues()) {
            if continuation == Continuation::Inactive {
                continuation = Continuation::Active;
                has_text = true;
                if !within_nested_list && let Some(last) = buffer.last_mut() {
                    last.kind = LineKind::Placeholder;
                }
            }
            if this_line.continues() {
                if continuation != Continuation::Frozen {
                    continuation = Continuation::Frozen;
                    buffer.push(this_line);
                }
                pending = None;
                continue;
            }
        }

        let text = reader.text(this_line);
        if let Some(terminator) = delimited_block(text) {
            if continuation != Continuation::Active {
                break;
            }
            buffer.push(this_line);
            buffer.extend(reader.read_until_terminator(terminator));
            continuation = Continuation::Inactive;
        } else if dlist && continuation != Continuation::Active && text.starts_with('[') && block_attribute_line(text) {
            let first = reader.next - 1;
            let mut attribute_lines = vec![this_line];
            let mut interrupt = false;
            while let Some(next) = reader.peek() {
                let next_text = reader.text(next);
                if delimited_block(next_text).is_some() {
                    interrupt = true;
                } else if next_text.is_empty() || (next_text.starts_with('[') && block_attribute_line(next_text)) {
                    attribute_lines.push(next);
                    reader.read_line();
                    continue;
                } else if any_list(next_text) && !is_sibling(next_text, kind, trait_) {
                    buffer.append(&mut attribute_lines);
                } else {
                    interrupt = true;
                }
                break;
            }
            if interrupt {
                reader.next = first;
                pending = None;
                break;
            }
        } else if continuation == Continuation::Active && !text.is_empty() {
            if literal_line(text) {
                reader.unshift(this_line);
                buffer.extend(reader.read_until_blank_or_continuation(&sibling));
                continuation = Continuation::Inactive;
            } else if block_title(text) || block_attribute_line(text) || attribute_entry(text) {
                buffer.push(this_line);
            } else {
                if let Some((nested, nested_text)) = nested_list(text, within_nested_list) {
                    within_nested_list = true;
                    if nested == Kind::Description && !nested_text {
                        has_text = false;
                    }
                }
                buffer.push(this_line);
                continuation = Continuation::Inactive;
            }
        } else if prev_line.is_some_and(|line| reader.text(line).is_empty()) {
            if text.is_empty() {
                let next = match reader.skip_blank_lines() {
                    Some(_) => reader.read_line(),
                    None => None,
                };
                let Some(next) = next else {
                    pending = None;
                    break;
                };
                this_line = next;
                pending = Some(this_line);
                if is_sibling(reader.text(this_line), kind, trait_) {
                    break;
                }
            }
            let text = reader.text(this_line);
            if text == "+" {
                detached_continuation = Some(buffer.len());
                buffer.push(Line { kind: LineKind::Continuation, ..this_line });
            } else if has_text {
                if is_sibling(text, kind, trait_) {
                    break;
                } else if let Some((nested, nested_text)) = nested_list(text, false) {
                    buffer.push(this_line);
                    within_nested_list = true;
                    if nested == Kind::Description && !nested_text {
                        has_text = false;
                    }
                } else if literal_line(text) {
                    reader.unshift(this_line);
                    buffer.extend(reader.read_until_blank_or_continuation(&sibling));
                } else {
                    break;
                }
            } else {
                if !within_nested_list {
                    buffer.pop();
                }
                buffer.push(this_line);
                has_text = true;
            }
        } else if this_line.continues() {
            has_text = true;
            buffer.push(this_line);
        } else {
            if !text.is_empty() {
                has_text = true;
            }
            if let Some((nested, nested_text)) = nested_list(text, within_nested_list) {
                within_nested_list = true;
                if nested == Kind::Description && !nested_text {
                    has_text = false;
                }
            }
            buffer.push(this_line);
        }
        pending = None;
    }

    if let Some(line) = pending {
        reader.unshift(line);
    }
    if let Some(at) = detached_continuation
        && let Some(line) = buffer.get_mut(at)
    {
        line.kind = LineKind::Placeholder;
    }
    while let Some(last) = buffer.last().copied() {
        if last.continues() {
            buffer.pop();
            break;
        } else if reader.text(last).is_empty() {
            buffer.pop();
        } else {
            break;
        }
    }
    buffer
}

/// The items of the unordered or ordered list whose first marker line starts
/// at `start`, reading no further than `limit` or a line `stop` claims.
pub(super) fn read_list(input: &str, start: usize, limit: usize, placeholders: &[usize], stop: &dyn Fn(&str) -> bool) -> ListLines {
    let mut reader = Reader::new(input, start, limit, placeholders, stop);
    let first = reader.peek().map(|line| reader.text(line)).unwrap_or_default();
    let kind = if unordered_item(first).is_some() { Kind::Unordered } else { Kind::Ordered };
    let mut items = Vec::new();
    let mut consumed_end = start;
    while let Some(line) = reader.peek() {
        let Some(marker) = item_marker(reader.text(line), kind) else { break };
        reader.read_line();
        let buffer = read_lines_for_list_item(&mut reader, kind, marker, true);
        consumed_end = reader.consumed_end().unwrap_or(consumed_end);
        items.push(ItemLines { marker_line: line, buffer });
        if reader.skip_blank_lines().is_none() {
            break;
        }
    }
    ListLines { items, consumed_end }
}

/// The items of the description list whose first term line starts at
/// `start` (`Parser.parse_description_list`): every later item is a line of
/// the first one's delimiter, with no blank line skipped between them.
pub(super) fn read_description_list<'a>(
    input: &'a str,
    start: usize,
    limit: usize,
    placeholders: &[usize],
    stop: &dyn Fn(&str) -> bool,
) -> Option<(&'a str, ListLines)> {
    let first_end = input[start..limit].find('\n').map_or(limit, |n| start + n);
    description_line(&input[start..first_end], None)?;
    let mut reader = Reader::new(input, start, limit, placeholders, stop);
    let first = description_line(reader.text(reader.peek()?), None)?;
    let delimiter = first.delimiter;
    let mut items = Vec::new();
    let mut consumed_end = start;
    let mut has_text = first.text.is_some();
    while let Some(line) = reader.read_line() {
        let buffer = read_lines_for_list_item(&mut reader, Kind::Description, ListMarker::Description(delimiter), has_text);
        consumed_end = reader.consumed_end().unwrap_or(consumed_end);
        items.push(ItemLines { marker_line: line, buffer });
        match reader.peek().and_then(|next| description_line(reader.text(next), Some(delimiter))) {
            Some(next) => has_text = next.text.is_some(),
            None => break,
        }
    }
    Some((delimiter, ListLines { items, consumed_end }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shapes(src: &str) -> Vec<(String, Vec<String>)> {
        read_list(src, 0, src.len(), &[], &|_| false)
            .items
            .iter()
            .map(|item| {
                (
                    item.marker_line.text(src).to_string(),
                    item.buffer.iter().map(|line| match line.kind {
                        LineKind::Placeholder => "<ph>".to_string(),
                        _ => line.text(src).to_string(),
                    }).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn items_end_at_a_sibling_and_keep_other_markers() {
        assert_eq!(
            shapes("* a\n** b\n- c\n* d\n"),
            vec![("* a".into(), vec!["** b".into(), "- c".into()]), ("* d".into(), vec![])]
        );
    }

    #[test]
    fn a_continuation_becomes_a_placeholder_unless_inside_a_nested_list() {
        assert_eq!(
            shapes(". alpha\n+\npara\n* beta\n+\npara\n- gamma\n"),
            vec![(
                ". alpha".into(),
                vec!["<ph>".into(), "para".into(), "* beta".into(), "+".into(), "para".into(), "- gamma".into()]
            )]
        );
    }

    #[test]
    fn a_detached_continuation_belongs_to_the_item() {
        assert_eq!(
            shapes(". a\n.. b\n\n+\npara\n. c\n"),
            vec![(". a".into(), vec![".. b".into(), "".into(), "<ph>".into(), "para".into()]), (". c".into(), vec![])]
        );
    }

    #[test]
    fn a_blank_line_then_text_ends_the_list() {
        assert_eq!(shapes("* a\n\npara\n"), vec![("* a".into(), vec![])]);
    }

    #[test]
    fn a_delimiter_without_continuation_ends_the_item() {
        assert_eq!(shapes("* a\n----\nx\n----\n"), vec![("* a".into(), vec![])]);
        assert_eq!(
            shapes("* a\n+\n----\nx\n----\n"),
            vec![("* a".into(), vec!["<ph>".into(), "----".into(), "x".into(), "----".into()])]
        );
    }

    #[test]
    fn delimiters_follow_is_delimited_block() {
        for line in ["--", "----", "-----", "....", "```", "```ruby", "|===", "////"] {
            assert!(delimited_block(line).is_some(), "{line}");
        }
        for line in ["---", "----x", "````", "- -", "==", "==="] {
            assert!(delimited_block(line).is_none(), "{line}");
        }
    }
}
