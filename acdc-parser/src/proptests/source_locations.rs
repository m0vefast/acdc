//! Source-location consistency: every inline names the source bytes it was read
//! from. The oracle is the source text itself, never a stored snapshot of acdc's
//! own output, so a wrong offset cannot pass by having been recorded before.
//!
//! For every inline of every content field (titles, paragraphs, cells, terms,
//! nested formatting), over the parser fixtures and the AST differential's
//! seeded documents:
//! - its offsets lie in the source on character boundaries, and its line and
//!   column are the ones the source gives for those offsets (columns count
//!   characters from the line start; the end is the last character's start);
//! - siblings come in source order without overlapping, except nodes that share
//!   one range because one source span produced them (an attribute reference);
//! - a nested inline lies inside the inline that owns it;
//! - plain text is the source bytes of its range, up to the transformations
//!   AsciiDoc defines between the two, each counted in the report;
//! - every block, list item, term and definition ends on its last character,
//!   never on the line break after it or past the input.
//!
//! `ADOC_LOC_SEEDS` sets how many seeds run (default 1500), starting at
//! `ADOC_LOC_SEED_FROM` (default 1).

use std::collections::BTreeMap;

use proptest::strategy::ValueTree;

use super::documents::{diff_document, env_u64, fixture_sources, seeded_tree};
use crate::{
    Options,
    ast::{ContentOwnerRef, InlineKind, LocationRef, StructuralRef},
    parse,
};

#[derive(Default)]
struct Report {
    findings: BTreeMap<String, (usize, Vec<String>)>,
    allowed: BTreeMap<&'static str, usize>,
}

impl Report {
    /// Counts every occurrence; keeps the details of the first distinct documents
    /// (`ADOC_LOC_EXAMPLES`, default 3).
    fn fail(&mut self, signature: String, detail: impl FnOnce() -> String) {
        let limit = usize::try_from(env_u64("ADOC_LOC_EXAMPLES", 3)).unwrap_or(usize::MAX);
        let (count, examples) = self.findings.entry(signature).or_default();
        *count += 1;
        if examples.len() < limit {
            let detail = detail();
            let doc = detail.split(':').next().unwrap_or_default().to_string();
            if !examples.iter().any(|e| e.split(':').next() == Some(doc.as_str())) {
                examples.push(detail);
            }
        }
    }

    fn allow(&mut self, rule: &'static str) {
        *self.allowed.entry(rule).or_default() += 1;
    }
}

/// The line and character column a text gives for a byte offset.
fn position_at(text: &str, offset: usize) -> Option<(usize, usize)> {
    let before = text.get(..offset)?;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    Some((before.matches('\n').count() + 1, text[line_start..offset].chars().count() + 1))
}

/// The text acdc's byte offsets count: every line without its trailing
/// whitespace and `\r` (`Preprocessor::normalize`). Line and column are the
/// same in it and in the source; byte offsets are not.
fn preprocessed(src: &str) -> String {
    src.lines().map(str::trim_end).collect::<Vec<_>>().join("\n")
}

/// The byte a line and character column name in a text.
fn byte_at(text: &str, line: usize, column: usize) -> Option<usize> {
    let line_start = if line == 1 { 0 } else { text.match_indices('\n').nth(line - 2)?.0 + 1 };
    let line_text = &text[line_start..];
    let line_end = line_text.find('\n').unwrap_or(line_text.len());
    let line_chars = line_text[..line_end].chars().count();
    let column_offset = if column == line_chars + 1 {
        line_end
    } else {
        line_text[..line_end].char_indices().nth(column - 1)?.0
    };
    Some(line_start + column_offset)
}

/// The source bytes a location names by line and column, from its start through
/// its last character: what every consumer reads.
fn named_bytes<'s>(src: &'s str, loc: LocationRef<'_>) -> Option<&'s str> {
    let (start, end) = (loc.start(), loc.end());
    let first = byte_at(src, start.line() as usize, start.column() as usize)?;
    let last = byte_at(src, end.line() as usize, end.column() as usize)?;
    if last < first {
        return Some("");
    }
    let width = src.get(last..)?.chars().next().map_or(0, char::len_utf8);
    src.get(first..last + width)
}

/// The readings of the source's escapes AsciiDoc defines: a leading backslash
/// escaping the whole construct (its content stays raw); every escaping
/// backslash dropped (`\\*b*` is `*b*`); and a backslash making the next
/// character stand for itself (`\\` is `\`).
fn escape_readings(raw: &str) -> [String; 3] {
    let construct = raw.strip_prefix('\\').map_or_else(|| raw.to_string(), str::to_string);
    let mut dropped = String::with_capacity(raw.len());
    let mut literal = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        let escapes = c == '\\' && chars.peek().is_some_and(|n| !n.is_whitespace());
        if !escapes {
            dropped.push(c);
        }
    }
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match chars.peek() {
            Some(&next) if c == '\\' && !next.is_whitespace() => {
                literal.push(next);
                chars.next();
            }
            _ => literal.push(c),
        }
    }
    [construct, dropped, literal]
}

/// An include not read draws `link:TARGET[role=include]` for its whole line.
fn is_drawn_include(bytes: &str, text: &str) -> bool {
    bytes
        .strip_prefix("include::")
        .filter(|rest| rest.ends_with(']') && !rest.contains('\n'))
        .and_then(|rest| rest.split_once('['))
        .is_some_and(|(target, _)| text == format!("link:{target}[role=include]"))
}

/// `footnote:ID[]` names a footnote defined elsewhere; its text is `[ID]`.
fn is_footnote_reference_text(bytes: &str, text: &str) -> bool {
    bytes
        .strip_prefix("footnote:")
        .and_then(|rest| rest.strip_suffix("[]"))
        .is_some_and(|id| text == format!("[{id}]"))
}

/// What the parser removed inside one node, when the text is the source with
/// whole lines dropped, line prefixes (indentation, `> `) stripped, or an
/// unprocessed include drawn as a link. One range then covers bytes the text
/// does not have, so columns past the first removal do not map linearly.
fn removal_inside_node(bytes: &str, text: &str) -> Option<&'static str> {
    let source: Vec<&str> = bytes.split('\n').collect();
    let mut kinds: Vec<&'static str> = Vec::new();
    let mut s = 0;
    for line in text.split('\n') {
        loop {
            let candidate = source.get(s)?;
            s += 1;
            if *candidate == line {
                break;
            }
            if let Some(target) = candidate.strip_prefix("include::").and_then(|r| r.split_once('[')).map(|(t, _)| t)
                && line == format!("link:{target}[role=include]")
            {
                kinds.push("include drawn as a link");
                break;
            }
            if let Some(prefix) = candidate.strip_suffix(line)
                && !prefix.is_empty()
            {
                if prefix.trim().is_empty() {
                    kinds.push("indentation stripped");
                    break;
                }
                if prefix.trim_end() == ">" {
                    kinds.push("`> ` prefix stripped");
                    break;
                }
            }
            kinds.push("whole lines dropped");
        }
    }
    if s != source.len() || kinds.is_empty() {
        return None;
    }
    kinds.sort_unstable();
    kinds.dedup();
    Some(match kinds.as_slice() {
        [one] => one,
        _ => "several removals",
    })
}

/// How a plain node's text relates to the bytes its range names, when they differ.
fn mismatch_shape(bytes: &str, text: &str) -> &'static str {
    if bytes.contains(text) {
        "range wider than its text"
    } else if text.contains(bytes) {
        "range narrower than its text"
    } else {
        "range names other bytes"
    }
}

fn kind_name(kind: &InlineKind<'_>) -> String {
    let debug = format!("{kind:?}");
    debug.split(['(', ' ', '{']).next().unwrap_or_default().to_string()
}

/// The source byte range a location names by line and column (end inclusive).
fn source_range(src: &str, loc: LocationRef<'_>) -> Option<(usize, usize)> {
    let first = byte_at(src, loc.start().line() as usize, loc.start().column() as usize)?;
    let last = byte_at(src, loc.end().line() as usize, loc.end().column() as usize)?;
    Some((first, last))
}

/// Every block names source characters: its end is its last character, never the
/// line break after it or a place past the input.
fn check_blocks(label: &str, src: &str, parsed: &crate::ParseResult, report: &mut Report) {
    for node in parsed.document().descendants() {
        let (loc, name) = match node {
            StructuralRef::Block(block) => (block.location(), format!("{:?}", block.kind())),
            StructuralRef::ListItem(item) => (item.location(), "ListItem".into()),
            StructuralRef::CalloutListItem(item) => (item.location(), "CalloutListItem".into()),
            StructuralRef::DescriptionListItem(item) => (item.location(), "DescriptionListItem".into()),
            StructuralRef::DescriptionListTerm(term) => (term.location(), "DescriptionListTerm".into()),
            StructuralRef::DescriptionListDefinition(def) => (def.location(), "DescriptionListDefinition".into()),
            _ => continue,
        };
        if loc.start().include_chain().is_some() || loc.end().include_chain().is_some() {
            continue;
        }
        let name = name.split(['(', ' ', '{']).next().unwrap_or_default().to_string();
        let end = (loc.end().line() as usize, loc.end().column() as usize);
        match byte_at(src, end.0, end.1) {
            Some(at) if src.as_bytes().get(at).is_some_and(|&b| b != b'\n') => {}
            _ => report.fail(format!("block {name}: end is not its last character"), || {
                format!("{label}: {:?}..={end:?}", (loc.start().line(), loc.start().column()))
            }),
        }
    }
}

fn check(label: &str, src: &str, report: &mut Report) {
    let Ok(parsed) = parse(src, &Options::default()) else { return };
    check_blocks(label, src, &parsed, report);
    let normalized = preprocessed(src);
    let excerpt = |start: usize| {
        let line_start = src[..start.min(src.len())].rfind('\n').map_or(0, |i| i + 1);
        let line_end = src[line_start..].find('\n').map_or(src.len(), |i| line_start + i);
        format!("{label}: {:?}", &src[line_start..line_end])
    };
    for field in parsed.document().content_occurrences() {
        let parent = match field.owner() {
            ContentOwnerRef::Inline(owner) => Some(owner.location()),
            _ => None,
        };
        let mut previous: Option<(usize, usize)> = None;
        for inline in field.content().inlines() {
            let loc = inline.location();
            let kind = inline.kind();
            let name = kind_name(&kind);
            if loc.start().include_chain().is_some() || loc.end().include_chain().is_some() {
                report.allow("included file (its own source)");
                continue;
            }
            let named = (loc.start().line() as usize, loc.start().column() as usize);
            let named_end = (loc.end().line() as usize, loc.end().column() as usize);
            let (Some(bytes), Some((start, end))) = (named_bytes(src, loc), source_range(src, loc)) else {
                report.fail(format!("{name}: line/column outside the source"), || {
                    format!("{label}: {named:?}..={named_end:?}")
                });
                continue;
            };
            let (abs_start, abs_end) = (loc.absolute_start(), loc.absolute_end());
            if abs_end >= abs_start {
                let at = |offset| position_at(&normalized, offset);
                if at(abs_start) != Some(named) {
                    report.fail(format!("{name}: start offset disagrees with its line/column"), || {
                        format!("{} — named {named:?}, offset {abs_start} is {:?}", excerpt(start), at(abs_start))
                    });
                }
                if at(abs_end) != Some(named_end) {
                    report.fail(format!("{name}: end offset disagrees with its line/column"), || {
                        format!("{} — named {named_end:?}, offset {abs_end} is {:?}", excerpt(start), at(abs_end))
                    });
                }
            }
            if let Some((prev_start, prev_end)) = previous
                && (start, end) != (prev_start, prev_end)
                && start <= prev_end
            {
                let shared = src.get(..=start).and_then(|head| head.rfind('{')).filter(|&open| {
                    !src[open..start].contains('}') && src[start..].find('}').is_some_and(|close| start + close >= prev_end)
                });
                let place = if shared.is_some() { "inside one attribute reference" } else { "elsewhere" };
                report.fail(format!("{name}: overlaps or precedes the sibling before it ({place})"), || {
                    format!("{} — {start}..={end} after {prev_start}..={prev_end}", excerpt(start))
                });
            }
            previous = Some((start, end));
            if let Some((owner_start, owner_end)) = parent.and_then(|owner| source_range(src, owner))
                && (start < owner_start || end > owner_end)
            {
                report.fail(format!("{name}: outside the inline that owns it"), || {
                    format!("{} — {start}..={end} in {owner_start}..={owner_end}", excerpt(start))
                });
            }
            if let InlineKind::Plain(text) = kind {
                let text = text.text();
                if bytes == text {
                    report.allow("plain text: exact source bytes");
                } else if escape_readings(bytes).iter().any(|reading| reading == text) {
                    report.allow("plain text: backslash escapes removed");
                } else if bytes
                    .trim_start_matches('\\')
                    .strip_prefix('{')
                    .and_then(|name| name.strip_suffix('}'))
                    .is_some_and(|name| !name.contains(['{', '}']))
                {
                    report.allow("plain text: stands for one whole attribute reference");
                } else if is_drawn_include(bytes, text) {
                    report.allow("plain text: stands for one whole include drawn as a link");
                } else if is_footnote_reference_text(bytes, text) {
                    report.allow("plain text: footnote reference drawn as [ID]");
                } else if bytes.len() == text.len() && bytes.replace('\n', " ") == text {
                    report.allow("plain text: line break folded to a space");
                } else if let Some(kind) = std::iter::once(bytes.to_string())
                    .chain(escape_readings(bytes))
                    .find_map(|reading| removal_inside_node(&reading, text))
                {
                    report.fail(format!("Plain: source removed inside one node ({kind})"), || {
                        format!("{} — source {bytes:?}, text {text:?}", excerpt(start))
                    });
                } else {
                    report.fail(format!("Plain: {}", mismatch_shape(bytes, text)), || {
                        format!("{} — source {bytes:?}, text {text:?}", excerpt(start))
                    });
                }
            }
        }
    }
}

#[test]
fn inline_locations_name_their_source_bytes() {
    let mut report = Report::default();
    let fixtures = fixture_sources();
    for (name, src) in &fixtures {
        check(name, src, &mut report);
    }
    let seeds = env_u64("ADOC_LOC_SEEDS", 1500);
    let seed_from = env_u64("ADOC_LOC_SEED_FROM", 1);
    let strategy = diff_document();
    for seed in seed_from..seed_from + seeds {
        check(&format!("seed {seed}"), &seeded_tree(&strategy, seed).current(), &mut report);
    }
    let allowed: Vec<String> = report.allowed.iter().map(|(rule, n)| format!("  {n} × {rule}")).collect();
    println!("{} fixtures + {seeds} seeds\n{}", fixtures.len(), allowed.join("\n"));
    let mut findings: Vec<_> = report.findings.iter().collect();
    findings.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    let lines: Vec<String> = findings
        .iter()
        .map(|(sig, (n, examples))| format!("{n} × {sig}\n    {}", examples.join("\n    ")))
        .collect();
    assert!(lines.is_empty(), "{} location signatures:\n{}", lines.len(), lines.join("\n"));
}
