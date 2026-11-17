//! Bracket boundaries shared by inline preprocessing and parsing.

/// asciidoctor's `(CC_ALL*?[^\\])\]`: the first `]` after at least one byte that no backslash
/// precedes. With `nest_later_macros`, a macro asciidoctor substitutes after kbd, btn and menu
/// (`Substitutors#sub_macros`) opened inside takes the `]` that would close them; plain
/// brackets never nest.
pub(super) fn unescaped_bracket_end(
    input: &str,
    start: usize,
    nest_later_macros: bool,
) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut depth = 0usize;
    for index in start..bytes.len() {
        match bytes[index] {
            b'[' if nest_later_macros && opens_later_macro(&input[start..index]) => depth += 1,
            b']' if index > start && bytes[index - 1] != b'\\' => {
                if depth == 0 {
                    return Some(index);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

const LATER_MACROS: [&str; 10] = [
    "footnote",
    "footnoteref",
    "link",
    "image",
    "icon",
    "xref",
    "mailto",
    "indexterm",
    "indexterm2",
    "anchor",
];

/// Whether the text before a `[` ends in a later macro's name and target, or a URL.
fn opens_later_macro(head: &str) -> bool {
    let token_start = head
        .char_indices()
        .rev()
        .find(|&(_, c)| c.is_whitespace() || c == '[' || c == ']')
        .map_or(0, |(index, c)| index + c.len_utf8());
    let token = &head[token_start..];
    token.match_indices(':').any(|(colon, _)| {
        let name_start = token[..colon]
            .char_indices()
            .rev()
            .find(|&(_, c)| !c.is_ascii_alphanumeric())
            .map_or(0, |(index, c)| index + c.len_utf8());
        let name = &token[name_start..colon];
        LATER_MACROS.contains(&name)
            || (token[colon..].starts_with("://")
                && !name.is_empty()
                && name.bytes().all(|byte| byte.is_ascii_alphabetic()))
    })
}

/// Find the closing bracket after `start`, preserving nested and escaped brackets.
/// `start` is the first byte after the opening bracket. Missing closes return `None`.
pub(super) fn escaped_bracket_end(input: &str, start: usize) -> Option<usize> {
    let mut bytes = input.get(start..)?.bytes().enumerate().peekable();
    let mut depth = 0usize;
    // Document-controlled bracket depth must not consume call stack space.
    while let Some((offset, byte)) = bytes.next() {
        match byte {
            b'\\'
                if bytes
                    .peek()
                    .is_some_and(|(_, next)| matches!(next, b'[' | b']')) =>
            {
                bytes.next();
            }
            b'[' => depth += 1,
            b']' if depth == 0 => return Some(start + offset),
            b']' => depth -= 1,
            _ => {}
        }
    }
    None
}
