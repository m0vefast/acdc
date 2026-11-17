use serde::Serialize;

use crate::XrefForm;

/// The extensions asciidoctor drops from a shorthand reference's document name
/// (`ASCIIDOC_EXTENSIONS` in asciidoctor 2.0.26).
pub const ASCIIDOC_EXTENSIONS: [&str; 5] = [".adoc", ".asciidoc", ".asc", ".ad", ".txt"];

/// What a cross reference names, as asciidoctor 2.0.26's `sub_inline_xrefs` reads its
/// target. Resolving a document or file against a location is the consumer's job; which
/// of the three the target is, and how its parts split, is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum XrefTarget<'a> {
    /// An id in this document. `None` names the document itself (`<<#>>`).
    Id { id: Option<&'a str> },
    /// Another document. `page` is the path as written; `name` is `page` less the
    /// extension asciidoctor drops.
    Document {
        page: &'a str,
        name: &'a str,
        fragment: Option<&'a str>,
    },
    /// A file that is not a document.
    File {
        page: &'a str,
        fragment: Option<&'a str>,
    },
}

/// asciidoctor's `Helpers.extname?`: a `.` with no `/` after it.
fn has_extension(page: &str) -> bool {
    page.rfind('.')
        .and_then(|dot| page.get(dot..))
        .is_some_and(|ext| !ext.contains('/'))
}

/// What a cross reference target, written as `form`, names (`sub_inline_xrefs`):
/// - a shorthand without `#` is an id; with `#` it names a document;
/// - the macro names a document for `.adoc`, or for no extension with a `#`; any other
///   extension is a file; no extension and no `#` is an id;
/// - an empty page with a `#` is an id in this document.
#[must_use]
pub fn xref_target_kind(target: &str, form: XrefForm) -> XrefTarget<'_> {
    let (page, fragment) = match target.split_once('#') {
        Some((page, fragment)) => (page, Some(fragment)),
        None => (target, None),
    };
    let written_fragment = fragment.filter(|f| !f.is_empty());
    if page.is_empty() && fragment.is_some() {
        return XrefTarget::Id {
            id: written_fragment,
        };
    }
    match form {
        XrefForm::Shorthand if fragment.is_none() => XrefTarget::Id { id: Some(target) },
        XrefForm::Shorthand => {
            let name = ASCIIDOC_EXTENSIONS
                .iter()
                .find_map(|ext| page.strip_suffix(ext))
                .unwrap_or(page);
            XrefTarget::Document {
                page,
                name,
                fragment: written_fragment,
            }
        }
        XrefForm::Macro => match page.strip_suffix(".adoc") {
            Some(name) => XrefTarget::Document {
                page,
                name,
                fragment: written_fragment,
            },
            None if has_extension(page) => XrefTarget::File {
                page,
                fragment: written_fragment,
            },
            None if fragment.is_some() => XrefTarget::Document {
                page,
                name: page,
                fragment: written_fragment,
            },
            None => XrefTarget::Id { id: Some(target) },
        },
    }
}
