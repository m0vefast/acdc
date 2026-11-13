use bumpalo::Bump;

use crate::{
    InlineNode, Location, ParseInlineResult, Pass, PassthroughKind, Plain, ProcessedContent, Raw,
    Substitution, parsed::OwnedInput,
};

use super::{
    ParserState,
    inlines::inline_parser,
    location_mapping::{LocationMapper, clamp_inline_node_locations},
};

/// The source bytes a passthrough's content was read from, `[start, end)`: past its opening
/// delimiter (`+`, `++`, `+++`, or `pass:` with its substitutions and `[`) and before its
/// closing one. An attribute-ref passthrough's content is its whole `{name}`.
fn content_span(passthrough: &Pass, input: &str) -> (usize, usize) {
    let (start, end) = (
        passthrough.location.absolute_start,
        passthrough.location.absolute_end,
    );
    let (open, close) = match passthrough.kind {
        PassthroughKind::Single => (1, 1),
        PassthroughKind::Double => (2, 2),
        PassthroughKind::Triple => (3, 3),
        PassthroughKind::Macro => (
            input
                .get(start..end)
                .and_then(|s| s.find('['))
                .map_or(0, |i| i + 1),
            1,
        ),
        PassthroughKind::AttributeRef => (0, 0),
    };
    let from = (start + open).min(end);
    (from, end.saturating_sub(close).max(from))
}

/// A location over source bytes `[from, to]` (inclusive), its line and column read from them.
fn location_at(state: &ParserState<'_>, from: usize, to: usize) -> Location {
    Location {
        absolute_start: from,
        absolute_end: to,
        start: state.line_map.offset_to_position(from, state.input),
        end: state.line_map.offset_to_position(to, state.input),
    }
}

/// Process passthrough content that contains quote substitutions, parsing nested markup
pub(crate) fn process_passthrough_with_quotes<'a>(
    state: &ParserState<'a>,
    content: &'a str,
    passthrough: &Pass,
) -> Vec<InlineNode<'a>> {
    let (from, to) = content_span(passthrough, state.input);
    let source = state.input.get(from..to).unwrap_or("");
    // The whole content's span, inclusive: its first byte to its last character's.
    let whole = location_at(
        state,
        from,
        to - source.chars().last().map_or(0, char::len_utf8),
    );
    let has_quotes = passthrough.substitutions.contains(&Substitution::Quotes);

    // If no quotes processing needed
    if !has_quotes {
        // If SpecialChars substitution is enabled, escape HTML (return PlainText)
        // This applies to: +text+ (Single), ++text++ (Double), pass:c[] (Macro with SpecialChars)
        // Otherwise output raw HTML (return RawText)
        // This applies to: +++text+++ (Triple), pass:[] (Macro without SpecialChars)
        // Use RawText for all passthroughs without Quotes to avoid merging with
        // adjacent PlainText nodes (which would lose the passthrough's substitution info).
        // Carry the passthrough's own subs (minus Quotes, already handled) so the
        // converter applies exactly those instead of the block's subs.
        return vec![InlineNode::RawText(Raw {
            content,
            location: whole,
            subs: passthrough
                .substitutions
                .iter()
                .filter(|s| **s != Substitution::Quotes)
                .cloned()
                .collect(),
        })];
    }

    tracing::debug!(content = ?content, "Parsing passthrough content with quotes");

    let mut nodes = parse_text_for_quotes_in(state.arena, content);
    // Content its substitutions changed (`pass:q,a[{x}]`) has no byte of its own in the
    // source: every node is the span it was read from.
    let verbatim = source == content;
    for node in &mut nodes {
        super::location_walk::walk_inline_locations_mut(node, &mut |loc| {
            *loc = if verbatim {
                location_at(state, from + loc.absolute_start, from + loc.absolute_end)
            } else {
                whole.clone()
            };
        });
    }
    nodes
}

/// Parse text for inline formatting markup (bold, italic, monospace, etc.).
///
/// Public entry point — returns a `ParseInlineResult` that owns the arena
/// the resulting `InlineNode`s borrow from. Callers reach the nodes via
/// `.inlines()`. Each call allocates a fresh arena; memory is reclaimed
/// when the returned value is dropped (no leaks). The returned
/// `ParseInlineResult::warnings()` slice is always empty for this entry
/// point — the quotes-only grammar never raises warnings.
///
/// # Supported Patterns
///
/// - `*bold*` and `**bold**` (constrained/unconstrained)
/// - `_italic_` and `__italic__`
/// - `` `monospace` `` and ``` ``monospace`` ```
/// - `^superscript^` and `~subscript~`
/// - `#highlight#` and `##highlight##`
/// - `` "`curved quotes`" `` and `` '`curved apostrophe`' ``
///
/// # Example
///
/// ```
/// use acdc_parser::parse_text_for_quotes;
///
/// let parsed = parse_text_for_quotes("This has *bold* text.");
/// assert_eq!(parsed.inlines().len(), 3); // "This has ", Bold("bold"), " text."
/// ```
pub fn parse_text_for_quotes(content: &str) -> ParseInlineResult {
    let owner = OwnedInput::new(content.into());
    ParseInlineResult::from_infallible(owner, |owner| {
        parse_text_for_quotes_in(&owner.arena, &owner.source)
    })
}

/// Arena-parameterised variant for internal callers that already have an
/// arena threaded through `ParserState`. Avoids the per-call `Bump`
/// allocation that the public entry point does.
pub(crate) fn parse_text_for_quotes_in<'a>(
    arena: &'a Bump,
    content: &'a str,
) -> Vec<InlineNode<'a>> {
    if content.is_empty() {
        return Vec::new();
    }

    // Fast path: if content has no formatting markers, return as plain text
    // without creating a ParserState or invoking the PEG parser.
    // Covers ~87% of calls in typical documents.
    if !content
        .bytes()
        .any(|b| matches!(b, b'*' | b'_' | b'`' | b'#' | b'^' | b'~' | b'"' | b'\''))
    {
        return whole_text(content);
    }

    let mut state = ParserState::new_quotes_only(content, arena);
    match inline_parser::quotes_only_inlines(content, &mut state) {
        Ok(nodes) => nodes,
        Err(err) => {
            tracing::warn!(
                ?err,
                ?content,
                "quotes-only PEG parse failed, falling back to plain text"
            );
            whole_text(content)
        }
    }
}

/// `content` as one text located over all of it.
fn whole_text(content: &str) -> Vec<InlineNode<'_>> {
    let last = content.len() - content.chars().last().map_or(0, char::len_utf8);
    let before = &content[..last];
    let line = u32::try_from(before.matches('\n').count() + 1).unwrap_or(u32::MAX);
    let column = u32::try_from(before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1)
        .unwrap_or(u32::MAX);
    vec![InlineNode::PlainText(Plain {
        content,
        location: Location {
            absolute_start: 0,
            absolute_end: last,
            start: crate::Position::new(1, 1),
            end: crate::Position::new(line, column),
        },
        escaped: false,
    })]
}

/// Process passthrough placeholders in content, returning expanded `InlineNode`s.
///
/// `content` is a plain text of the preprocessed paragraph at `plain_location` (in its
/// coordinates); each run of text around a placeholder is located through the source map
/// (`map_loc`), as every other inline is, and each passthrough at the bytes it was read from.
pub(crate) fn process_passthrough_placeholders<'a>(
    content: &'a str,
    processed: &'a ProcessedContent<'a>,
    state: &ParserState<'a>,
    plain_location: &Location,
    map_loc: &LocationMapper<'_>,
) -> Result<Vec<InlineNode<'a>>, crate::Error> {
    // The text `text` at byte `at` of `content`, located in the source.
    let text_at = |text: &'a str, at: usize| -> Result<Plain<'a>, crate::Error> {
        let from = plain_location.absolute_start + at;
        let last = text.chars().last().map_or(0, char::len_utf8);
        Ok(Plain {
            content: text,
            location: map_loc(&Location {
                absolute_start: from,
                absolute_end: from + text.len() - last,
                ..Location::default()
            })?,
            escaped: false,
        })
    };

    // Each passthrough produces at most (placeholder-count × small factor) +
    // one trailing-plain. Upper-bound at 2 × placeholders + 1 so a paragraph
    // full of passthroughs doesn't trigger log-N reallocs.
    let mut result = Vec::with_capacity(processed.passthroughs.len() * 2 + 1);
    let mut remaining = content;
    let mut at = 0; // byte of `content` where `remaining` starts

    for (index, passthrough) in processed.passthroughs.iter().enumerate() {
        let placeholder = format!("���{index}���");
        let Some(placeholder_pos) = remaining.find(&placeholder) else {
            continue;
        };
        if placeholder_pos > 0 {
            result.push(InlineNode::PlainText(text_at(
                &remaining[..placeholder_pos],
                at,
            )?));
        }
        if let Some(passthrough_content) = &passthrough.text {
            let has_quotes = passthrough.substitutions.contains(&Substitution::Quotes);
            let remaining_subs: Vec<Substitution> = passthrough
                .substitutions
                .iter()
                .filter(|s| **s != Substitution::Quotes)
                .cloned()
                .collect();
            for mut node in process_passthrough_with_quotes(state, passthrough_content, passthrough)
            {
                // For passthroughs with quotes, convert PlainText to RawText so HTML
                // content passes through unescaped.
                if has_quotes && let InlineNode::PlainText(p) = node {
                    node = InlineNode::RawText(Raw {
                        content: p.content,
                        location: p.location,
                        subs: remaining_subs.clone(),
                    });
                }
                result.push(node);
            }
        }
        let skip_len = placeholder_pos + placeholder.len();
        remaining = &remaining[skip_len..];
        at += skip_len;
    }

    if !remaining.is_empty() {
        let tail = text_at(remaining, at)?;
        if let Some(InlineNode::PlainText(last_plain)) = result.last_mut() {
            last_plain.content =
                state.intern_fmt(format_args!("{}{remaining}", last_plain.content));
            last_plain.location.absolute_end = tail.location.absolute_end;
            last_plain.location.end = tail.location.end;
        } else {
            result.push(InlineNode::PlainText(tail));
        }
    }

    for node in &mut result {
        clamp_inline_node_locations(node, state.input);
    }

    Ok(merge_adjacent_plain_text_nodes(state, result))
}

/// Merge adjacent plain text nodes into single nodes to simplify the output.
/// Arena-interns the concatenated content so the merged node keeps lifetime `'a`.
pub(crate) fn merge_adjacent_plain_text_nodes<'a>(
    state: &ParserState<'a>,
    nodes: Vec<InlineNode<'a>>,
) -> Vec<InlineNode<'a>> {
    // Worst case: no merges possible, so the output matches the input length.
    let mut result: Vec<InlineNode<'a>> = Vec::with_capacity(nodes.len());

    for node in nodes {
        match (result.last_mut(), node) {
            (Some(InlineNode::PlainText(last_plain)), InlineNode::PlainText(current_plain)) => {
                // Merge current plain text with the last one
                last_plain.content = state.intern_fmt(format_args!(
                    "{}{}",
                    last_plain.content, current_plain.content
                ));
                // Extend the location to cover both nodes
                last_plain.location.absolute_end = current_plain.location.absolute_end;
                last_plain.location.end = current_plain.location.end;
            }
            (_, node) => {
                // Not adjacent plain text nodes, add as separate node
                result.push(node);
            }
        }
    }

    result
}

pub(crate) fn replace_passthrough_placeholders(
    content: &str,
    processed: &ProcessedContent,
) -> String {
    let mut result: String = content.into();

    // Replace each passthrough placeholder with its content
    for (index, passthrough) in processed.passthroughs.iter().enumerate() {
        let placeholder = format!("���{index}���");
        if let Some(text) = &passthrough.text {
            result = result.replace(&placeholder, text);
        }
    }

    result
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // Tests verify length before indexing
mod tests {
    use super::*;

    // === Divergence Prevention Tests ===
    //
    // These tests verify that parse_text_for_quotes produces the same structural
    // output as the main PEG parser for common inline formatting patterns.
    // If these tests fail after grammar changes, update parse_text_for_quotes.

    #[test]
    fn test_constrained_bold_pattern() {
        let parsed = parse_text_for_quotes("This is *bold* text.");
        let nodes = parsed.inlines();
        assert_eq!(nodes.len(), 3);
        assert!(matches!(nodes[0], InlineNode::PlainText(_)));
        assert!(
            matches!(&nodes[1], InlineNode::BoldText(b) if matches!(b.content.first(), Some(InlineNode::PlainText(p)) if p.content == "bold"))
        );
        assert!(matches!(nodes[2], InlineNode::PlainText(_)));
    }

    #[test]
    fn test_unconstrained_bold_pattern() {
        let parsed = parse_text_for_quotes("This**bold**word");
        let nodes = parsed.inlines();
        assert_eq!(nodes.len(), 3);
        assert!(
            matches!(&nodes[1], InlineNode::BoldText(b) if matches!(b.content.first(), Some(InlineNode::PlainText(p)) if p.content == "bold"))
        );
    }

    #[test]
    fn test_constrained_italic_pattern() {
        let parsed = parse_text_for_quotes("This is _italic_ text.");
        let nodes = parsed.inlines();
        assert_eq!(nodes.len(), 3);
        assert!(
            matches!(&nodes[1], InlineNode::ItalicText(i) if matches!(i.content.first(), Some(InlineNode::PlainText(p)) if p.content == "italic"))
        );
    }

    #[test]
    fn test_unconstrained_italic_pattern() {
        let parsed = parse_text_for_quotes("This__italic__word");
        let nodes = parsed.inlines();
        assert_eq!(nodes.len(), 3);
        assert!(
            matches!(&nodes[1], InlineNode::ItalicText(i) if matches!(i.content.first(), Some(InlineNode::PlainText(p)) if p.content == "italic"))
        );
    }

    #[test]
    fn test_constrained_monospace_pattern() {
        let parsed = parse_text_for_quotes("Use `code` here.");
        let nodes = parsed.inlines();
        assert_eq!(nodes.len(), 3);
        assert!(
            matches!(&nodes[1], InlineNode::MonospaceText(m) if matches!(m.content.first(), Some(InlineNode::PlainText(p)) if p.content == "code"))
        );
    }

    #[test]
    fn test_superscript_pattern() {
        let parsed = parse_text_for_quotes("E=mc^2^");
        let nodes = parsed.inlines();
        assert_eq!(nodes.len(), 2);
        assert!(
            matches!(&nodes[1], InlineNode::SuperscriptText(s) if matches!(s.content.first(), Some(InlineNode::PlainText(p)) if p.content == "2"))
        );
    }

    #[test]
    fn test_subscript_pattern() {
        let parsed = parse_text_for_quotes("H~2~O");
        let nodes = parsed.inlines();
        assert_eq!(nodes.len(), 3);
        assert!(
            matches!(&nodes[1], InlineNode::SubscriptText(s) if matches!(s.content.first(), Some(InlineNode::PlainText(p)) if p.content == "2"))
        );
    }

    #[test]
    fn test_highlight_pattern() {
        let parsed = parse_text_for_quotes("This is #highlighted# text.");
        let nodes = parsed.inlines();
        assert_eq!(nodes.len(), 3);
        assert!(
            matches!(&nodes[1], InlineNode::HighlightText(h) if matches!(h.content.first(), Some(InlineNode::PlainText(p)) if p.content == "highlighted"))
        );
    }

    #[test]
    fn test_escaped_superscript_not_parsed() {
        // Backslash-escaped markers should not be parsed as formatting
        let parsed = parse_text_for_quotes(r"E=mc\^2^");
        let nodes = parsed.inlines();
        // Should remain as plain text (escape prevents parsing)
        assert!(
            nodes.iter().all(|n| matches!(n, InlineNode::PlainText(_))),
            "Escaped superscript should not be parsed"
        );
    }

    #[test]
    fn test_escaped_subscript_not_parsed() {
        let parsed = parse_text_for_quotes(r"H\~2~O");
        let nodes = parsed.inlines();
        assert!(
            nodes.iter().all(|n| matches!(n, InlineNode::PlainText(_))),
            "Escaped subscript should not be parsed"
        );
    }

    #[test]
    fn test_multiple_formats_in_sequence() {
        let parsed = parse_text_for_quotes("*bold* and _italic_ and `code`");
        let nodes = parsed.inlines();
        assert!(nodes.iter().any(|n| matches!(n, InlineNode::BoldText(_))));
        assert!(nodes.iter().any(|n| matches!(n, InlineNode::ItalicText(_))));
        assert!(
            nodes
                .iter()
                .any(|n| matches!(n, InlineNode::MonospaceText(_)))
        );
    }

    #[test]
    fn test_plain_text_only() {
        let parsed = parse_text_for_quotes("Just plain text here.");
        let nodes = parsed.inlines();
        assert_eq!(nodes.len(), 1);
        assert!(matches!(nodes[0], InlineNode::PlainText(_)));
    }

    #[test]
    fn test_empty_input() {
        let parsed = parse_text_for_quotes("");
        let nodes = parsed.inlines();
        assert!(nodes.is_empty());
    }
}
