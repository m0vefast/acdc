use crate::{
    IndexTermKind, InlineMacro, InlineNode, Location, Substitution,
    grammar::{
        ParserState,
        utf8_utils::{RoundDirection, step_char},
    },
};

use super::{WordContext, has_candidate, source_start, structural_token_allowed};

pub(super) fn needs_resolution(node: &InlineNode<'_>, ordinary: bool) -> bool {
    if let InlineNode::PlainText(text) = node {
        return ordinary && !text.escaped && has_candidate(text.content);
    }
    if let InlineNode::RawText(raw) = node {
        return raw.subs.contains(&Substitution::Replacements) && needs_context(raw.content);
    }
    if let InlineNode::Macro(InlineMacro::IndexTerm(term)) = node {
        return matches!(&term.kind, IndexTermKind::Flow(nodes) if nodes.iter().any(|node| needs_resolution(node, ordinary)));
    }
    false
}

pub(super) fn needs_word_context(node: &InlineNode<'_>) -> bool {
    if let InlineNode::PlainText(text) = node {
        return !text.escaped && needs_context(text.content);
    }
    if let InlineNode::RawText(raw) = node {
        return raw.subs.contains(&Substitution::Replacements) && needs_context(raw.content);
    }
    if let InlineNode::Macro(InlineMacro::IndexTerm(term)) = node {
        return matches!(&term.kind, IndexTermKind::Flow(nodes) if nodes.iter().any(needs_word_context));
    }
    false
}

fn needs_context(text: &str) -> bool {
    ["'", "-"]
        .iter()
        .any(|token| text.starts_with(token) || text.ends_with(token))
}

pub(super) fn contexts(
    nodes: &[InlineNode<'_>],
    state: &ParserState<'_>,
    inherited: WordContext,
) -> Option<Vec<WordContext>> {
    if !nodes.iter().any(needs_word_context) {
        return None;
    }
    let mut contexts = vec![WordContext::default(); nodes.len()];
    let mut before = inherited.before;
    for (node, context) in nodes.iter().zip(&mut contexts) {
        context.before = before;
        before = edge(node, false, state).update(before);
    }
    let mut after = inherited.after;
    for (node, context) in nodes.iter().zip(&mut contexts).rev() {
        context.after = after;
        after = edge(node, true, state).update(after);
    }
    Some(contexts)
}

enum Edge {
    Text(char, bool),
    Transparent,
    Boundary,
}

impl Edge {
    fn update(self, inherited: Option<(char, bool)>) -> Option<(char, bool)> {
        match self {
            Self::Text(character, allowed) => Some((character, allowed)),
            Self::Transparent => inherited,
            Self::Boundary => None,
        }
    }
}

fn edge(node: &InlineNode<'_>, first: bool, state: &ParserState<'_>) -> Edge {
    match node {
        InlineNode::PlainText(text) => text_edge(text.content, &text.location, first, state),
        // asciidoctor extracts passthroughs before every substitution: their placeholder is no word.
        InlineNode::RawText(raw) if isolated_passthrough(raw, state) => Edge::Boundary,
        InlineNode::RawText(raw) => text_edge(raw.content, &raw.location, first, state),
        InlineNode::Symbol(symbol) => {
            let text = symbol.kind.text();
            let character = if first {
                text.chars().next()
            } else {
                text.chars().next_back()
            };
            character.map_or(Edge::Transparent, |character| Edge::Text(character, true))
        }
        InlineNode::InlineAnchor(_) | InlineNode::Macro(_)
            if let Some((head, tail)) = source_delimiters(node, state) =>
        {
            let (character, allowed) = if first { head } else { tail };
            Edge::Text(character, allowed)
        }
        InlineNode::InlineAnchor(anchor) if !anchor.is_bibliography() => {
            if anchor_is_markup(state) {
                Edge::Boundary
            } else {
                Edge::Transparent
            }
        }
        InlineNode::Macro(InlineMacro::IndexTerm(term)) => {
            let IndexTermKind::Flow(nodes) = &term.kind else {
                return Edge::Transparent;
            };
            let mut nodes = nodes.iter();
            while let Some(node) = if first {
                nodes.next()
            } else {
                nodes.next_back()
            } {
                let edge = edge(node, first, state);
                if !matches!(edge, Edge::Transparent) {
                    return edge;
                }
            }
            Edge::Transparent
        }
        InlineNode::VerbatimText(_)
        | InlineNode::BoldText(_)
        | InlineNode::ItalicText(_)
        | InlineNode::MonospaceText(_)
        | InlineNode::HighlightText(_)
        | InlineNode::SubscriptText(_)
        | InlineNode::SuperscriptText(_)
        | InlineNode::DoubleQuotedSpan(_)
        | InlineNode::SingleQuotedSpan(_)
        | InlineNode::StandaloneCurvedApostrophe(_)
        | InlineNode::LineBreak(_)
        | InlineNode::InlineAnchor(_)
        | InlineNode::Macro(_)
        | InlineNode::CalloutMarker(_)
        | InlineNode::CharacterReference(_) => Edge::Boundary,
    }
}

/// asciidoctor extracts passthroughs before every substitution: the words around one meet a
/// placeholder, and its own substitutions see only its text.
pub(super) fn isolated_passthrough(raw: &crate::Raw<'_>, state: &ParserState<'_>) -> bool {
    state.options.compatibility_mode == crate::CompatibilityMode::Asciidoctor
        && state
            .passthrough_spans
            .iter()
            .flat_map(|spans| spans.iter())
            .any(|span| span.contains(&raw.location.absolute_start))
}

/// Macros before replacements (asciidoctor): an anchor is already its `<a id>` tag when
/// replacements run, so no token joins across it.
pub(super) fn anchor_is_markup(state: &ParserState<'_>) -> bool {
    state.options.compatibility_mode == crate::CompatibilityMode::Asciidoctor
        && state
            .inline_ctx
            .substitutions
            .precedes(&Substitution::Macros, &Substitution::Replacements)
}

/// Replacements before macros: a macro or inline anchor is still its source when replacements
/// run, so the words around it meet its first and last source characters. Passthroughs and stems
/// are not: asciidoctor extracts them before any substitution.
pub(super) fn source_delimiters(
    node: &InlineNode<'_>,
    state: &ParserState<'_>,
) -> Option<((char, bool), (char, bool))> {
    if !state.label_delimits_replacements() {
        return None;
    }
    let location = match node {
        InlineNode::Macro(InlineMacro::Passthrough(_) | InlineMacro::Stem(_)) => return None,
        InlineNode::Macro(inline) => inline.location(),
        InlineNode::InlineAnchor(anchor) if !anchor.is_bibliography() => &anchor.location,
        _ => return None,
    };
    let end = step_char(state.input, location.absolute_end, RoundDirection::Forward);
    let source = state.input.get(location.absolute_start..end)?;
    Some((
        (source.chars().next()?, true),
        (source.chars().next_back()?, true),
    ))
}

fn text_edge(text: &str, location: &Location, first: bool, state: &ParserState<'_>) -> Edge {
    let character = if first {
        text.char_indices().next()
    } else {
        text.char_indices().next_back()
    };
    let Some((offset, character)) = character else {
        return Edge::Transparent;
    };
    let allowed = !state
        .inline_ctx
        .substitutions
        .precedes(&Substitution::Replacements, &Substitution::Attributes)
        || source_start(text, location, state).is_some_and(|start| {
            structural_token_allowed(
                state,
                &Substitution::Replacements,
                start + offset,
                character.len_utf8(),
            )
        });
    Edge::Text(character, allowed)
}

pub(super) fn protected_edge(
    node: &InlineNode<'_>,
    first: bool,
    state: &ParserState<'_>,
) -> Option<(char, bool)> {
    if !matches!(node, InlineNode::RawText(_) | InlineNode::PlainText(_)) {
        return None;
    }
    if let Edge::Text(character, allowed) = edge(node, first, state) {
        Some((character, allowed))
    } else {
        None
    }
}
