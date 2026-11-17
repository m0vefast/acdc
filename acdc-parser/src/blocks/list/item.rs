use crate::ListItem;

impl ListItem<'_> {
    /// This takes a single line and tries to parse it according to the described
    /// `AsciiDoc` list item rules. It simply identifies the depth of nesting. It handles
    /// the patterns as follows:
    ///
    /// Unordered list:
    /// * -> depth 1
    /// - -> depth 1
    ///
    /// ** -> depth 2
    ///
    /// Ordered list:
    /// . -> depth 1
    /// .. -> depth 2
    /// 1. , 10. , a. , A. , i) , IV) -> depth 1 (an explicit ordinal)
    pub(crate) fn parse_depth_from_marker(marker: &str) -> Option<usize> {
        let trimmed = marker.trim();

        // Check for unordered lists first
        if trimmed.starts_with('*') {
            // Count how many '*' at the start
            let depth = trimmed.chars().take_while(|&c| c == '*').count();
            return Some(depth);
        }

        if trimmed.starts_with('-') {
            // '-' form only depth 1
            return Some(1);
        }

        // Check for ordered lists
        if trimmed.starts_with('.') {
            // Count how many '.' at the start
            let depth = trimmed.chars().take_while(|&c| c == '.').count();
            return Some(depth);
        }

        let explicit = match trimmed.strip_suffix('.') {
            Some(ordinal) => {
                (!ordinal.is_empty() && ordinal.bytes().all(|b| b.is_ascii_digit()))
                    || (ordinal.len() == 1 && ordinal.bytes().all(|b| b.is_ascii_alphabetic()))
            }
            None => trimmed
                .strip_suffix(')')
                .is_some_and(|numeral| !numeral.is_empty() && numeral.bytes().all(|b| b"IVXivx".contains(&b))),
        };
        explicit.then_some(1)
    }
}
