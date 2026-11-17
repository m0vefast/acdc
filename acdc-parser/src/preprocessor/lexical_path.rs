use std::path::{Path, PathBuf};

/// Lexically normalize a POSIX path the way Go's `path.Clean` does: drop `.` and
/// empty components and collapse `..` without consulting the filesystem. A leading
/// `/` is kept, a leading `..` on a relative path is kept, and a path that collapses
/// to nothing is `.`.
///
/// String-level on purpose: on `wasm32-unknown-unknown` `std::path` falls back to
/// Windows semantics, so `/a/b.adoc` would not count as absolute there.
pub(crate) fn normalize_lexical(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    let is_absolute = raw.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in raw.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(parts.last(), Some(&last) if last != "..") {
                    parts.pop();
                } else if !is_absolute {
                    parts.push("..");
                }
            }
            _ => parts.push(seg),
        }
    }
    if parts.is_empty() {
        return PathBuf::from(if is_absolute { "/" } else { "." });
    }
    let body = parts.join("/");
    if is_absolute {
        PathBuf::from(format!("/{body}"))
    } else {
        PathBuf::from(body)
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_lexical;
    use std::path::Path;

    #[test]
    fn collapses_dots_like_path_clean() {
        let cases = [
            ("a/./b", "a/b"),
            ("a/../b", "b"),
            ("../a", "../a"),
            ("a/../../b", "../b"),
            ("/a/../../b", "/b"),
            ("/", "/"),
            ("", "."),
            ("a/..", "."),
            ("a//b/", "a/b"),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_lexical(Path::new(input)), Path::new(expected), "{input}");
        }
    }
}
