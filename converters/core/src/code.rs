use acdc_parser::BlockMetadata;

/// Detect programming language from block metadata.
///
/// Returns the language if:
/// - The block has `style="source"`
/// - The metadata contains a `language` named attribute (set by the parser
///   from the positional `[source,LANG]` shorthand or the explicit
///   `[source,language=LANG]` form)
///
/// Any language string is returned, not just known ones. This ensures
/// `[source,text]` and other arbitrary languages get proper `<code>` wrappers.
#[must_use]
pub fn detect_language<'a, 'b: 'a>(metadata: &'a BlockMetadata<'b>) -> Option<&'a str> {
    let is_source = metadata.style == Some("source");
    if !is_source {
        return None;
    }
    match metadata.attributes.get("language") {
        Some(acdc_parser::AttributeValue::String(s)) if !s.is_empty() => Some(s.as_ref()),
        _ => None,
    }
}

/// Get the default line comment prefix for a programming language.
/// Used for stripping comment guards from callout markers in source blocks.
#[must_use]
pub fn default_line_comment(language: Option<&str>) -> Option<&'static str> {
    match language {
        // Hash comments
        Some(
            "python" | "py" | "ruby" | "rb" | "perl" | "bash" | "shell" | "sh" | "zsh" | "fish"
            | "yaml" | "yml" | "toml" | "dockerfile" | "makefile" | "cmake",
        ) => Some("#"),
        // Double-dash comments (SQL, Lua)
        Some("sql" | "lua") => Some("--"),
        // Semicolon comments
        Some("clojure" | "ini") => Some(";"),
        // XML/HTML comments are multiline, so we return None
        Some("html" | "xml" | "css" | "json") => None,
        // Default: assume C-style (//) for unknown languages and common C-family languages
        _ => Some("//"),
    }
}
