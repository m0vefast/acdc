//! Lists built from the lines Asciidoctor's list reader gives each item.

use crate::{
    Block, BlockMetadata, DescriptionList, DescriptionListDefinition, DescriptionListItem,
    DescriptionListTerm, Error, InlineNode, ListItem, ListItemCheckedStatus, Location, OrderedList,
    Title, UnorderedList,
    grammar::{
        ParserState,
        document::{
            content_origin::ContentOrigin,
            list_reader::{self, LineKind, ListLines, read_description_list, read_list},
        },
        document_parser,
        helpers::BlockParsingMetadata,
        inline_processing::{adjust_and_log_parse_error, process_inlines},
        location_walk,
        state::BlockContext,
    },
    model::ListLevel,
};
use bumpalo::collections::String as BumpString;

/// An unordered or ordered list whose first marker line starts at `pos` in the
/// PEG input, which begins `offset` bytes into `state.input`. Asciidoctor's
/// reader decides each item's lines; the item is then parsed from those lines
/// alone, so a list nested in it ends where the item ends. Returns the list,
/// or the error parsing it, and the PEG input position after the last line it
/// holds: the lines are the list's either way.
pub(super) fn ordinary_list<'a>(
    state: &mut ParserState<'a>,
    input_len: usize,
    start: usize,
    pos: usize,
    offset: usize,
    metadata: &BlockParsingMetadata<'a>,
    is_nested: bool,
) -> (Result<Block<'a>, Error>, usize) {
    let source = state.input;
    let callout = state.callout_list_offset == Some(offset);
    let stop = |line: &str| callout && list_reader::callout_parent_line(line);
    let placeholders = state.placeholder_lines.clone();
    let draft = state.options.compatibility_mode == crate::CompatibilityMode::Draft;
    let lines = read_list(source, pos + offset, input_len + offset, &placeholders, &stop, draft);
    let consumed = lines.consumed_end - offset;
    let ListContent { text, removals, spans, placeholders } = list_content(source, &lines);
    let base = lines.items.first().map_or(pos + offset, |item| item.marker_line.start);
    let content = state.intern_str(&text);
    let origin = ContentOrigin::new(removals, base);
    let items = origin.parse(
        content,
        state,
        |state| {
            state.placeholder_lines = placeholders.into();
            spans.iter().map(|&(from, to)| list_item(state, content, from, to, metadata)).collect::<Result<Vec<_>, Error>>()
        },
        |items, visit| {
            for item in items {
                visit(&mut item.location);
                for node in &mut item.principal {
                    location_walk::walk_inline_locations_mut(node, visit);
                }
                location_walk::walk_block_locations_mut(&mut item.blocks, visit);
            }
        },
    );
    let mut items = match items {
        Ok(items) => items,
        Err(error) => return (Err(error), consumed),
    };
    for (item, read) in items.iter_mut().zip(&lines.items) {
        if !item.blocks.is_empty() {
            extend_over_dropped(state, &mut item.location, read);
        }
    }
    let end = items.last().map_or(pos + offset, |item| item.location.absolute_end);
    let marker = items.first().map_or("", |item| item.marker);
    let title = if is_nested { Title::default() } else { metadata.title.clone() };
    let block_metadata = if is_nested { BlockMetadata::default() } else { metadata.metadata.clone() };
    let location = state.create_location(start + offset, end);
    let unordered = list_reader::is_unordered_marker(marker);
    let block = if unordered {
        Block::UnorderedList(UnorderedList { title, metadata: block_metadata, items, marker, location })
    } else {
        Block::OrderedList(OrderedList { title, metadata: block_metadata, items, marker, location })
    };
    (Ok(block), consumed)
}

/// A description list whose first term line starts at `pos` in the PEG
/// input (`Parser.parse_description_list`): each item holds the lines
/// Asciidoctor's reader gives it, and a term without a definition joins the
/// next one. `None` when the line is no description item.
pub(super) fn description_list<'a>(
    state: &mut ParserState<'a>,
    input_len: usize,
    start: usize,
    pos: usize,
    offset: usize,
    metadata: &BlockParsingMetadata<'a>,
) -> Option<(Result<Block<'a>, Error>, usize)> {
    let source = state.input;
    let callout = state.callout_list_offset == Some(offset);
    let stop = |line: &str| callout && list_reader::callout_parent_line(line);
    let placeholders = state.placeholder_lines.clone();
    let draft = state.options.compatibility_mode == crate::CompatibilityMode::Draft;
    let (delimiter, lines) = read_description_list(source, pos + offset, input_len + offset, &placeholders, &stop, draft)?;
    let consumed = lines.consumed_end - offset;
    let ListContent { text, removals, spans, placeholders } = list_content(source, &lines);
    let base = lines.items.first().map_or(pos + offset, |item| item.marker_line.start);
    let content = state.intern_str(&text);
    let origin = ContentOrigin::new(removals, base);
    let items = origin.parse(
        content,
        state,
        |state| {
            state.placeholder_lines = placeholders.into();
            spans
                .iter()
                .enumerate()
                .map(|(index, &(from, to))| {
                    description_item(state, content, from, to, (index > 0).then_some(delimiter), metadata)
                })
                .collect::<Result<Vec<_>, Error>>()
        },
        |items, visit| {
            for item in items {
                visit(&mut item.location);
                for term in &mut item.terms {
                    visit(&mut term.location);
                    if let Some(location) = &mut term.delimiter_location {
                        visit(location);
                    }
                    for node in &mut term.content {
                        location_walk::walk_inline_locations_mut(node, visit);
                    }
                }
                if let Some(definition) = &mut item.definition {
                    visit(&mut definition.location);
                    for node in definition.principal.iter_mut().flatten() {
                        location_walk::walk_inline_locations_mut(node, visit);
                    }
                    location_walk::walk_block_locations_mut(&mut definition.blocks, visit);
                }
            }
        },
    );
    let block = items.map(|items| {
        let end = items.last().map_or(pos + offset, |item| item.location.absolute_end);
        let location = state.create_location(start + offset, end);
        Block::DescriptionList(DescriptionList {
            title: metadata.title.clone(),
            metadata: metadata.metadata.clone(),
            items: group_description_terms(items),
            location,
        })
    });
    Some((block, consumed))
}

/// An item with blocks ends at the last line the reader gave it (`list_item`'s
/// `to - 1`), a dropped line included: its bytes are not in the content, so
/// that end cannot be reached through the content.
fn extend_over_dropped(state: &ParserState<'_>, location: &mut Location, read: &list_reader::ItemLines) {
    if let Some(last) = read.buffer.last()
        && last.kind == LineKind::Dropped
        && last.end > last.start
        && last.end - 1 > location.absolute_end
    {
        *location = state.create_location(location.absolute_start, last.end - 1);
    }
}

/// One term and its definition from its span of the list content: the item
/// text with the paragraph lines adjacent to it (`ListItem#fold_first`), or
/// for a term without text the paragraph its reader starts with, then every
/// block in the rest of its lines.
fn description_item<'a>(
    state: &mut ParserState<'a>,
    content: &'a str,
    from: usize,
    to: usize,
    sibling: Option<&str>,
    metadata: &BlockParsingMetadata<'a>,
) -> Result<DescriptionListItem<'a>, Error> {
    let line_end = content[from..to].find('\n').map_or(to, |n| from + n);
    let line = &content[from..line_end];
    let found = list_reader::description_line(line, sibling)
        .or_else(|| list_reader::description_line(line, None))
        .ok_or_else(|| Error::Parse(Box::new(state.create_error_source_location(state.create_location(from, line_end))), "description list item".into()))?;
    let term_text = &line[found.term.0..found.term.1];
    let term_start = from + found.term.0;
    let term_end = term_start + term_text.trim_end().len();
    let delimiter_start = from + found.delimiter_start;
    let delimiter_end = delimiter_start + found.delimiter.len();
    let (mut term, _) = process_inlines(state, metadata, term_start, term_end, 0, term_text.trim_end())?;
    // asciidoctor `parse_list_item`: a term's leading anchor without its own label takes the
    // rest of the term as its label (`catalog_inline_anchor $1, ($2 || $'.lstrip)`).
    if state.options.compatibility_mode == crate::CompatibilityMode::Asciidoctor
        && term_text.starts_with("[[")
        && let Some((InlineNode::InlineAnchor(anchor), rest)) = term.split_first_mut()
        && anchor.xreflabel.is_none()
        && !anchor.bibliography
    {
        let mut label = rest.to_vec();
        if let Some(InlineNode::PlainText(text)) = label.first_mut() {
            text.content = text.content.trim_start();
        }
        label.retain(|node| !matches!(node, InlineNode::PlainText(text) if text.content.is_empty()));
        if !label.is_empty() {
            anchor.xreflabel = Some(crate::ContentAttribute::new(label));
        }
    }
    let delimiter_location = state.create_block_location(delimiter_start, delimiter_end, 0);
    let term = DescriptionListTerm {
        anchors: Vec::new(),
        content: term,
        delimiter: found.delimiter,
        delimiter_location: Some(delimiter_location),
        location: state.create_block_location(term_start, delimiter_end, 0),
    };

    let (folded, folded_end) = document_parser::folded_lines(&content[line_end..to], state, line_end)?;
    let folded: Vec<(usize, &str)> = folded.into_iter().map(|(at, line)| (line_end + at, line)).collect();
    let folded_end = line_end + folded_end;
    let principal = match found.text {
        Some(text_at) => Some(folded_principal(state, (from + text_at, &line[text_at..]), &folded, false, metadata)?),
        None => match folded.split_first() {
            Some((first, rest)) => Some(folded_principal(state, *first, rest, true, metadata)?),
            None => None,
        },
    };
    let principal_start = match found.text {
        Some(text_at) => Some(from + text_at),
        None => folded.first().map(|&(at, line)| at + line.len() - line.trim_start_matches([' ', '\t']).len()),
    };
    let principal_end = folded.last().map_or(line_end, |&(at, line)| at + line.trim_end().len());

    let rest_start = folded_end + content[folded_end..to].len() - content[folded_end..to].trim_start_matches('\n').len();
    let blocks = if rest_start < to {
        let outer_context = std::mem::replace(&mut state.block_context, BlockContext::Compound);
        let outer_item = state.list_item_offset.replace(rest_start);
        let parsed = document_parser::compound_blocks(&content[rest_start..to], state, rest_start, metadata.parent_section_level);
        state.list_item_offset = outer_item;
        state.block_context = outer_context;
        parsed.unwrap_or_else(|e| {
            adjust_and_log_parse_error(&e, &content[rest_start..to], rest_start, state, "Error parsing a description list item's blocks");
            Ok(Vec::new())
        })?
    } else {
        Vec::new()
    };
    let blocks = super::metadata::order_block_prefix_events(blocks);

    let actual_end = blocks.last().map_or_else(
        || state.create_block_location(term_start, principal_end.max(delimiter_end), 0).absolute_end,
        |block| block.location().absolute_end,
    );
    let definition = match (principal, principal_start) {
        (Some(principal), Some(at)) => {
            Some(DescriptionListDefinition { principal: Some(principal), blocks, location: state.create_location(at, actual_end) })
        }
        _ if !blocks.is_empty() => {
            let first = blocks.first().map_or_else(Location::default, |block| block.location().clone());
            let location = state.create_location(first.absolute_start, actual_end);
            Some(DescriptionListDefinition { principal: None, blocks, location })
        }
        _ => None,
    };
    let item_end = if definition.is_some() { actual_end } else { term.location.absolute_end };
    Ok(DescriptionListItem { terms: vec![term], definition, location: state.create_location(from, item_end) })
}

/// The item lines as one text, a source removal wherever a line was dropped
/// or stands empty for a `+`, each item's span in that text, and where its
/// placeholder lines start.
struct ListContent {
    text: String,
    removals: Vec<(usize, usize)>,
    spans: Vec<(usize, usize)>,
    placeholders: Vec<usize>,
}

fn list_content(source: &str, lines: &ListLines) -> ListContent {
    let mut content = String::new();
    let mut removals: Vec<(usize, usize)> = Vec::new();
    let remove = |at: usize, len: usize, removals: &mut Vec<(usize, usize)>| {
        if len == 0 {
            return;
        }
        match removals.last_mut() {
            Some((last, total)) if *last == at => *total += len,
            _ => removals.push((at, len)),
        }
    };
    let mut spans = Vec::new();
    let mut placeholders = Vec::new();
    let mut previous_end = None;
    for item in &lines.items {
        let mut item_start = 0;
        for (index, line) in std::iter::once(&item.marker_line).chain(&item.buffer).enumerate() {
            if let Some(previous) = previous_end {
                content.push('\n');
                remove(content.len(), line.start - (previous + 1), &mut removals);
            }
            if index == 0 {
                item_start = content.len();
            }
            content.push_str(line.text(source));
            match line.kind {
                LineKind::Placeholder => {
                    placeholders.push(content.len());
                    remove(content.len(), line.end - line.start, &mut removals);
                }
                LineKind::Dropped => remove(content.len(), line.end - line.start, &mut removals),
                LineKind::Source | LineKind::Continuation => {}
            }
            previous_end = Some(line.end);
        }
        spans.push((item_start, content.len()));
    }
    ListContent { text: content, removals, spans, placeholders }
}

/// One item from its span of the list content: its principal text (the
/// marker line and the paragraph lines adjacent to it, `ListItem#fold_first`)
/// and every block in the rest of its lines (`next_block` over its reader).
fn list_item<'a>(
    state: &mut ParserState<'a>,
    content: &'a str,
    from: usize,
    to: usize,
    metadata: &BlockParsingMetadata<'a>,
) -> Result<ListItem<'a>, Error> {
    let text = &content[from..to];
    let head = document_parser::ordinary_item_head(text, state, from)?;
    let rest: Vec<(usize, &str)> = head.rest.iter().map(|&(at, line)| (from + at, line)).collect();
    let principal = folded_principal(state, (from + head.first.0, head.first.1), &rest, false, metadata)?;
    let ItemHead { checked, marker, marker_start, after_marker, end: first_line_end, .. } = head;
    let empty = head.first.1.is_empty() && head.rest.is_empty();
    let rest_start = from + first_line_end + content[from + first_line_end..to].len()
        - content[from + first_line_end..to].trim_start_matches('\n').len();
    // The item's reader holds blocks only, never a section (`parse_list_item`).
    let blocks = if rest_start < to {
        let outer_context = std::mem::replace(&mut state.block_context, BlockContext::Compound);
        let outer_item = state.list_item_offset.replace(rest_start);
        let parsed = document_parser::compound_blocks(&content[rest_start..to], state, rest_start, metadata.parent_section_level);
        state.list_item_offset = outer_item;
        state.block_context = outer_context;
        parsed.unwrap_or_else(|e| {
            adjust_and_log_parse_error(&e, &content[rest_start..to], rest_start, state, "Error parsing a list item's blocks");
            Ok(Vec::new())
        })?
    } else {
        Vec::new()
    };
    let level = ListLevel::try_from(ListItem::parse_depth_from_marker(marker).unwrap_or(1))?;
    let end = if blocks.is_empty() { calculate_item_end(empty, from + after_marker, from + first_line_end) } else { to - 1 };
    Ok(ListItem {
        principal,
        blocks: super::metadata::order_block_prefix_events(blocks),
        level,
        marker,
        checked,
        location: state.create_location(from + marker_start, end),
    })
}

/// An item's marker line and the adjacent paragraph lines folded into its
/// text, positions local to the item's span.
pub(in crate::grammar) struct ItemHead<'a> {
    pub(super) checked: Option<ListItemCheckedStatus>,
    pub(super) marker: &'a str,
    pub(super) marker_start: usize,
    pub(super) after_marker: usize,
    pub(super) first: (usize, &'a str),
    pub(super) rest: Vec<(usize, &'a str)>,
    pub(super) end: usize,
}

/// The item's text: `first` (the text after the marker, or with `dedent_first`
/// the first folded line) and the folded lines after it, positions in the
/// current input. An indented first folded line reads as a literal paragraph
/// (`Parser.next_block`), whose common indentation `adjust_indentation!`
/// removes before `ListItem#fold_first` folds it in.
fn folded_principal<'a>(
    state: &mut ParserState<'a>,
    first: (usize, &'a str),
    rest: &[(usize, &'a str)],
    dedent_first: bool,
    metadata: &BlockParsingMetadata<'a>,
) -> Result<Vec<InlineNode<'a>>, Error> {
    let indent = |line: &str| line.len() - line.trim_start_matches([' ', '\t']).len();
    let (start, first) = first;
    let lead = if dedent_first { Some(first) } else { rest.first().map(|(_, line)| *line) };
    let gutter = match lead {
        Some(line) if indent(line) > 0 => {
            let dedented = rest.iter().map(|(_, line)| *line).chain(dedent_first.then_some(first));
            dedented.map(indent).min().unwrap_or(0)
        }
        _ => 0,
    };
    let end = rest.last().map_or(start + first.len(), |(at, line)| at + line.len());
    if gutter == 0 {
        let rest: Vec<&str> = rest.iter().map(|(_, line)| *line).collect();
        let text = assemble_principal_text(state, first, &rest);
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        return process_inlines(state, metadata, start, end, 0, text).map(|(nodes, _)| nodes);
    }
    let mut removals = Vec::new();
    let mut text = if dedent_first {
        removals.push((0, gutter));
        String::from(&first[gutter..])
    } else {
        String::from(first)
    };
    for (_, line) in rest {
        text.push('\n');
        removals.push((text.len(), gutter));
        text.push_str(&line[gutter..]);
    }
    let text = state.intern_str(&text);
    ContentOrigin::new(removals, start).parse(
        text,
        state,
        |state| process_inlines(state, metadata, 0, text.len(), 0, text).map(|(nodes, _)| nodes),
        |nodes, visit| {
            for node in nodes {
                location_walk::walk_inline_locations_mut(node, visit);
            }
        },
    )
}

/// The marker a list's items share (`Parser.resolve_list_marker`): a run of
/// dots or stars is itself, any other ordered marker stands for its series.
#[derive(Clone, Copy, Debug)]
pub(in crate::grammar) enum OrdinaryKey<'a> {
    Unordered(&'a str),
    Dots(usize),
    Arabic,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
}

impl<'a> OrdinaryKey<'a> {
    /// `OrderedListMarkerRxMap`, tried in `ORDERED_LIST_STYLES` order with an
    /// unanchored match, so `Iv)` is lower roman.
    pub(crate) fn of(marker: ListMarker<'a>) -> Option<Self> {
        match marker {
            ListMarker::Unordered(marker) => Some(Self::Unordered(marker)),
            ListMarker::Ordered(marker) if marker.starts_with('.') => Some(Self::Dots(marker.len())),
            ListMarker::Ordered(marker) => Some(if let Some(numeral) = marker.strip_suffix(')') {
                if numeral.ends_with(['i', 'v', 'x']) { Self::LowerRoman } else { Self::UpperRoman }
            } else if marker.starts_with(|c: char| c.is_ascii_digit()) {
                Self::Arabic
            } else if marker.starts_with(|c: char| c.is_ascii_lowercase()) {
                Self::LowerAlpha
            } else {
                Self::UpperAlpha
            }),
            ListMarker::Description(_) => None,
        }
    }

    fn same(self, other: OrdinaryKey<'_>) -> bool {
        match (self, other) {
            (Self::Unordered(a), OrdinaryKey::Unordered(b)) => a == b,
            (Self::Dots(a), OrdinaryKey::Dots(b)) => a == b,
            (Self::Arabic, OrdinaryKey::Arabic)
            | (Self::LowerAlpha, OrdinaryKey::LowerAlpha)
            | (Self::UpperAlpha, OrdinaryKey::UpperAlpha)
            | (Self::LowerRoman, OrdinaryKey::LowerRoman)
            | (Self::UpperRoman, OrdinaryKey::UpperRoman) => true,
            _ => false,
        }
    }

    /// `Parser.is_sibling_list_item?`: the same list type and the same marker.
    pub(crate) fn siblings(a: ListMarker<'_>, b: ListMarker<'_>) -> bool {
        match (OrdinaryKey::of(a), OrdinaryKey::of(b)) {
            (Some(a), Some(b)) => a.same(b),
            _ => false,
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::grammar) enum ListMarker<'a> {
    Unordered(&'a str),
    Ordered(&'a str),
    Description(&'a str),
}

impl<'a> ListMarker<'a> {
    pub(super) const fn text(self) -> &'a str {
        match self {
            Self::Unordered(marker) | Self::Ordered(marker) | Self::Description(marker) => marker,
        }
    }

}

/// Join a list item's first line and continuation lines with newlines.
/// Return `first_line` unchanged when there are no continuation lines.
/// Otherwise, store the joined text in the arena.
pub(super) fn assemble_principal_text<'a>(
    state: &ParserState<'a>,
    first_line: &'a str,
    continuation_lines: &[&str],
) -> &'a str {
    if continuation_lines.is_empty() {
        first_line
    } else {
        let mut s = BumpString::new_in(state.arena);
        s.push_str(first_line);
        for line in continuation_lines {
            s.push('\n');
            s.push_str(line);
        }
        s.into_bump_str()
    }
}

/// Calculates the end position for a list item based on its principal text.
/// Returns `start` if empty, otherwise one less than `first_line_end`.
pub(super) const fn calculate_item_end(
    principal_text_is_empty: bool,
    start: usize,
    first_line_end: usize,
) -> usize {
    if principal_text_is_empty {
        start
    } else {
        first_line_end.saturating_sub(1)
    }
}

/// Find a description-list marker after at least one term character on this line.
/// A marker must be followed by a space, a newline, or end of input.
/// This scan avoids a PEG rule call at each byte.
#[inline]
pub(super) fn find_dlist_marker(bytes: &[u8], pos: usize) -> bool {
    let mut i = pos;
    while let Some(&b) = bytes.get(i) {
        if b == b'\n' {
            return false;
        }
        if i > pos && (b == b':' || b == b';') {
            let marker_len = if b == b':' {
                let mut k = 1;
                while k < 4 && bytes.get(i + k) == Some(&b':') {
                    k += 1;
                }
                if k >= 2 { k } else { 0 }
            } else if bytes.get(i + 1) == Some(&b';') {
                2
            } else {
                0
            };
            if marker_len > 0 {
                let after = bytes.get(i + marker_len).copied();
                if matches!(after, None | Some(b'\n' | b' ')) {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

fn group_description_terms(items: Vec<DescriptionListItem<'_>>) -> Vec<DescriptionListItem<'_>> {
    let mut groups: Vec<DescriptionListItem<'_>> = Vec::with_capacity(items.len());
    for item in items {
        if let Some(previous) = groups.last_mut()
            && previous.definition.is_none()
        {
            previous.terms.extend(item.terms);
            previous.definition = item.definition;
            previous.location.absolute_end = item.location.absolute_end;
            previous.location.end = item.location.end;
        } else {
            groups.push(item);
        }
    }
    groups
}
