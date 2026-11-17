//! Reading text that the parser splits into several plain nodes: a
//! reference's text is a node of its own, standing at the whole reference.

use acdc_parser::ast::{InlineKind, InlineRef, LocationRef};

/// The joined text of nodes that are all plain text.
#[allow(dead_code)]
pub(crate) fn plain_text<'a>(nodes: impl IntoIterator<Item = InlineRef<'a>>) -> Option<String> {
    let mut joined = String::new();
    for node in nodes {
        let InlineKind::Plain(text) = node.kind() else {
            return None;
        };
        joined.push_str(text.text());
    }
    Some(joined)
}

/// Nodes that are all plain text and follow each other in `source` with no
/// gap but references that expanded to nothing: their joined text and the
/// first and last node's locations.
#[allow(dead_code)]
pub(crate) fn plain_run<'a>(
    nodes: impl IntoIterator<Item = InlineRef<'a>>,
    source: &str,
) -> Option<(String, LocationRef<'a>, LocationRef<'a>)> {
    let mut run: Option<(String, LocationRef<'a>, LocationRef<'a>)> = None;
    for node in nodes {
        let InlineKind::Plain(text) = node.kind() else {
            return None;
        };
        let location = node.location();
        run = Some(match run {
            None => (text.text().to_string(), location, location),
            Some((mut joined, first, last)) => {
                let end = last.absolute_end();
                let next = end + source.get(end..)?.chars().next()?.len_utf8();
                let gap = source.get(next..location.absolute_start())?;
                let reference = gap.strip_prefix('{').and_then(|gap| gap.strip_suffix('}'));
                if !gap.is_empty() && reference.is_none_or(|name| name.contains(['{', '}'])) {
                    return None;
                }
                joined.push_str(text.text());
                (joined, first, location)
            }
        });
    }
    run
}
