//! Section levels and nesting checks.

use crate::{
    Anchor, DocumentAttributes, Error, InlineNode, Location, Section, Subtitle, Title, Warning, WarningKind,
    grammar::{ParserState, helpers::BlockParsingMetadata, inline_processing::process_inlines},
    model::{LeveloffsetRange, SectionKind, SectionLevel},
};

/// The length of an ATX title without a closing marker the same as its opening
/// one (`AtxSectionTitleRx`: `(?:[ \t]+\1)?$`).
pub(super) fn without_closing_marker(title: &str, marker: &str) -> usize {
    match title.strip_suffix(marker) {
        Some(body) if !marker.is_empty() && body.ends_with([' ', '\t']) => {
            let kept = body.trim_end_matches([' ', '\t']);
            if kept.is_empty() { title.len() } else { kept.len() }
        }
        _ => title.len(),
    }
}

/// Where a heading title's trailing ` [[id]]` or ` [[id, reftext]]` starts:
/// Asciidoctor's `InlineSectionAnchorRx`, applied when the heading has no ID
/// of its own (`Parser.parse_section_title`).
fn trailing_anchor_start(title: &str) -> Option<usize> {
    let body = title.strip_suffix("]]")?;
    let start = body.rfind(" [[")?;
    let inner = &body[start + 3..];
    let id = inner.split_once(',').map_or(inner, |(id, _)| id);
    let mut chars = id.chars();
    let first = chars.next()?;
    let valid = (first.is_alphabetic() || matches!(first, '_' | ':'))
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'));
    (valid && !inner.contains("]]")).then_some(start)
}

/// A heading title's inlines and natural reference text: a closing marker the
/// same as `marker` dropped, then a trailing section anchor split off as the
/// heading's own anchor (the order of `Parser.parse_section_title`).
pub(super) fn heading_title<'a>(
    state: &mut ParserState<'a>,
    block_metadata: &BlockParsingMetadata<'a>,
    start: usize,
    offset: usize,
    title: &'a str,
    marker: &str,
) -> Result<(Title<'a>, &'a str, Option<Anchor<'a>>), Error> {
    let title = &title[..without_closing_marker(title, marker)];
    let has_id = Section::explicit_id(&block_metadata.metadata).is_some();
    let (kept, anchor) = split_trailing_anchor(state, block_metadata, start, offset, title, has_id)?;
    let (content, natural) = process_inlines(state, block_metadata, start, start + kept, offset, &title[..kept])?;
    Ok((Title::new(content), natural, anchor))
}

pub(super) type DocumentTitleParts<'a> = (Title<'a>, Option<Subtitle<'a>>, Option<Anchor<'a>>);

/// A document title's inlines, its subtitle after the last `": "`, and its
/// trailing anchor, split off first as for any heading
/// (`Parser.parse_section_title`, then `Document#doctitle` partition).
pub(super) fn document_title_parts<'a>(
    state: &mut ParserState<'a>,
    start: usize,
    title: &'a str,
    has_id: bool,
) -> Result<DocumentTitleParts<'a>, Error> {
    let block_metadata = BlockParsingMetadata::new(state.options.compatibility_mode);
    let (kept, anchor) = split_trailing_anchor(state, &block_metadata, start, 0, title, has_id)?;
    let title = &title[..kept];
    let (mut inlines, _) = process_inlines(state, &block_metadata, start, start + title.len(), 0, title)?;
    let subtitle = split_subtitle(state, &mut inlines);
    Ok((Title::new(inlines), subtitle.map(Subtitle::new), anchor))
}

/// Asciidoctor takes the subtitle after the last `: ` of the formatted title
/// (`Document#doctitle(partition: true)`). In an inline tree that is a split
/// only when the last `: ` lies in top-level text written as it reads; one
/// inside a formatted span or a substituted value leaves the title whole.
fn split_subtitle<'a>(state: &ParserState<'a>, inlines: &mut Vec<InlineNode<'a>>) -> Option<Vec<InlineNode<'a>>> {
    let (index, node) = inlines.iter().enumerate().rev().find_map(|(i, node)| {
        let loc = node.location();
        let source = state.input.get(loc.absolute_start..=loc.absolute_end)?;
        source.contains(": ").then_some((i, node))
    })?;
    let InlineNode::PlainText(plain) = node else { return None };
    let loc = plain.location.clone();
    if state.input.get(loc.absolute_start..=loc.absolute_end) != Some(plain.content) {
        return None;
    }
    let colon = plain.content.rfind(": ")?;
    let left = plain.content[..colon].trim_end();
    let right_raw = &plain.content[colon + 1..];
    let right = right_raw.trim_start();
    let right_start = loc.absolute_start + colon + 1 + (right_raw.len() - right.len());
    let mut rest = inlines.split_off(index + 1);
    if right.is_empty() && rest.is_empty() {
        inlines.extend(rest);
        return None;
    }
    inlines.pop();
    if !left.is_empty() {
        let end = loc.absolute_start + left.len() - left.chars().next_back().map_or(1, char::len_utf8);
        inlines.push(InlineNode::PlainText(crate::Plain::new(left, state.create_location(loc.absolute_start, end))));
    }
    if !right.is_empty() {
        let end = right_start + right.len() - right.chars().next_back().map_or(1, char::len_utf8);
        rest.insert(0, InlineNode::PlainText(crate::Plain::new(right, state.create_location(right_start, end))));
    }
    Some(rest)
}

/// The title length left once a trailing ` [[id]]` is taken as the heading's own
/// anchor, which happens only when the heading has no ID of its own.
pub(super) fn split_trailing_anchor<'a>(
    state: &mut ParserState<'a>,
    block_metadata: &BlockParsingMetadata<'a>,
    start: usize,
    offset: usize,
    title: &'a str,
    has_id: bool,
) -> Result<(usize, Option<Anchor<'a>>), Error> {
    let Some(cut) = (!has_id).then(|| trailing_anchor_start(title)).flatten() else {
        return Ok((title.len(), None));
    };
    let (nodes, _) = process_inlines(state, block_metadata, start + cut + 1, start + title.len(), offset, &title[cut + 1..])?;
    Ok(match <[InlineNode<'a>; 1]>::try_from(nodes) {
        Ok([InlineNode::InlineAnchor(anchor)]) => (cut, Some(anchor)),
        _ => (title.len(), None),
    })
}

/// Apply the effective `leveloffset` at this source position.
/// Keep the result within the supported levels, 0 through 5.
pub(super) fn apply_leveloffset(
    base_level: SectionLevel,
    byte_offset: usize,
    leveloffset_ranges: &[LeveloffsetRange],
    _document_attributes: &DocumentAttributes,
) -> SectionLevel {
    let total_offset = crate::model::calculate_leveloffset_at(leveloffset_ranges, byte_offset);

    if total_offset != 0 {
        let adjusted = isize::from(base_level).saturating_add(total_offset);

        let clamped = adjusted.clamp(0, 5);

        SectionLevel::try_from(clamped)
            .inspect_err(|_| {
                tracing::error!(clamped, "not a valid section after applying leveloffset");
            })
            .unwrap_or(0)
    } else {
        base_level
    }
}

/// Expected `parent_section_level` (one-based, i.e. own level + 1) for a
/// section's nested content.
///
/// A level-0 special section stands at level 1, so its
/// first subsection must be level 2 (`===`). A level-1 (`==`) heading closes the
/// special section instead of nesting under it. Every other section expects
/// children one level deeper than itself.
pub(super) fn expected_child_level(level: SectionLevel, kind: SectionKind) -> SectionLevel {
    crate::model::section::effective_numbering_level(level, kind) + 1
}

pub(super) fn warn_for_nested_special_section(
    state: &ParserState<'_>,
    direct_parent_section_kind: Option<SectionKind>,
    heading_location: Location,
) {
    let kind = match direct_parent_section_kind {
        Some(SectionKind::Bibliography) => WarningKind::NestedSectionInBibliography,
        Some(SectionKind::Index) => WarningKind::NestedSectionInIndex,
        _ => return,
    };
    let location = state.create_error_source_location(heading_location);
    state.add_warning(Warning::new(kind, Some(location)));
}
