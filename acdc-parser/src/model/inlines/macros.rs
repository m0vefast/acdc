use serde::Serialize;

use crate::{ElementAttributes, InlineNode, LineIndex, Location, Source, StemNotation, Substitution};

pub const ICON_SIZES: &[&str] = &["1x", "2x", "3x", "4x", "5x", "lg", "fw"];

/// A `Pass` represents a passthrough macro in a document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Pass<'a> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<&'a str>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub substitutions: Vec<Substitution>,
    pub location: Location,
    #[serde(skip)]
    pub kind: PassthroughKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Default)]
pub enum PassthroughKind {
    #[default]
    Single,
    Double,
    Triple,
    Macro,
    /// Character replacement attribute expanded as passthrough (e.g., `{plus}` → `+`).
    /// The location spans the `{attr}` reference, not delimiters.
    AttributeRef,
}

/// A `Footnote` represents an inline footnote in a document.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Footnote<'a> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<&'a str>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<InlineNode<'a>>,
    #[serde(skip)]
    pub number: u32,
    pub location: Location,
}

/// An `Icon` represents an inline icon in a document.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Icon<'a> {
    pub target: Source<'a>,
    pub attributes: ElementAttributes<'a>,
    pub location: Location,
}

/// A `Link` represents an inline link in a document.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Link<'a> {
    #[serde(skip_serializing)]
    pub text: Vec<InlineNode<'a>>,
    pub target: Source<'a>,
    pub attributes: ElementAttributes<'a>,
    pub location: Location,
}

impl<'a> Link<'a> {
    /// Where the target is written in the parsed text, as a byte range: the bytes right
    /// after `link:`, when they are exactly the parsed target. `None` when they are not
    /// (an attribute reference was substituted into the target), so a consumer never
    /// rewrites text other than what it read.
    ///
    /// A method rather than a stored location: `Link` is the largest inline variant, and
    /// a field here would grow every inline node the parser allocates.
    #[must_use]
    pub fn target_span(&self, lines: &LineIndex<'_>) -> Option<std::ops::Range<usize>> {
        let source = lines.text();
        let whole = lines.range(&self.location)?;
        let target = self.target.to_string();
        let start = whole.start.checked_add("link:".len())?;
        let end = start.checked_add(target.len())?;
        (end <= whole.end && source.get(whole.start..start)? == "link:" && source.get(start..end)? == target)
            .then_some(start..end)
    }
    /// Creates a new `Link` with the given target.
    #[must_use]
    pub fn new(target: Source<'a>, location: Location) -> Self {
        Self {
            text: Vec::new(),
            target,
            attributes: ElementAttributes::default(),
            location,
        }
    }

    /// Sets the link text as inline nodes.
    #[must_use]
    pub fn with_text(mut self, text: Vec<InlineNode<'a>>) -> Self {
        self.text = text;
        self
    }

    /// Sets the link attributes.
    #[must_use]
    pub fn with_attributes(mut self, attributes: ElementAttributes<'a>) -> Self {
        self.attributes = attributes;
        self
    }
}

/// An `Url` represents an inline URL in a document.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Url<'a> {
    #[serde(skip_serializing)]
    pub text: Vec<InlineNode<'a>>,
    pub target: Source<'a>,
    pub attributes: ElementAttributes<'a>,
    pub location: Location,
}

/// An `Mailto` represents an inline `mailto:` in a document.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Mailto<'a> {
    #[serde(skip_serializing)]
    pub text: Vec<InlineNode<'a>>,
    pub target: Source<'a>,
    pub attributes: ElementAttributes<'a>,
    pub location: Location,
}

/// A `Button` represents an inline button in a document.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Button<'a> {
    pub label: &'a str,
    pub location: Location,
}

/// A `Menu` represents an inline menu in a document.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Menu<'a> {
    pub target: &'a str,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<&'a str>,
    pub location: Location,
}

/// A `Keyboard` represents an inline keyboard shortcut in a document.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Keyboard<'a> {
    pub keys: Vec<Key<'a>>,
    pub location: Location,
}

impl<'a> Keyboard<'a> {
    /// Creates a new `Keyboard` with the given keys.
    #[must_use]
    pub fn new(keys: Vec<Key<'a>>, location: Location) -> Self {
        Self { keys, location }
    }
}

// TODO(nlopes): this could perhaps be an enum instead with the allowed keys
pub type Key<'a> = &'a str;

/// How an author wrote a cross reference. asciidoctor reads the two forms
/// differently: `<<guide.adoc>>` is an id, `xref:guide.adoc[]` a document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum XrefForm {
    /// `<<target>>` or `<<target,text>>`.
    #[default]
    Shorthand,
    /// `xref:target[text]`.
    Macro,
}

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
    /// extension asciidoctor drops, which is what it compares with the document's own
    /// name and with the files the document includes.
    Document { page: &'a str, name: &'a str, fragment: Option<&'a str> },
    /// A file that is not a document.
    File { page: &'a str, fragment: Option<&'a str> },
}

/// asciidoctor's `Helpers.extname?`: a `.` with no `/` after it.
fn has_extension(page: &str) -> bool {
    page.rfind('.').is_some_and(|dot| !page[dot..].contains('/'))
}

/// What a cross reference target, written as `form`, names (`sub_inline_xrefs`):
/// - a shorthand without `#` is an id; with `#` it names a document;
/// - the macro names a document for `.adoc`, or for no extension with a `#`; any other
///   extension is a file; no extension and no `#` is an id;
/// - an empty page with a `#` is an id in this document.
///
/// Public so a consumer holding only the target text — a clicked link, a shared test
/// vector — reads the same rule the parsed node does.
#[must_use]
pub fn xref_target_kind(target: &str, form: XrefForm) -> XrefTarget<'_> {
    let (page, fragment) = match target.split_once('#') {
        Some((page, fragment)) => (page, Some(fragment)),
        None => (target, None),
    };
    let written_fragment = fragment.filter(|f| !f.is_empty());
    if page.is_empty() && fragment.is_some() {
        return XrefTarget::Id { id: written_fragment };
    }
    match form {
        XrefForm::Shorthand if fragment.is_none() => XrefTarget::Id { id: Some(target) },
        XrefForm::Shorthand => {
            let name = ASCIIDOC_EXTENSIONS.iter().find_map(|ext| page.strip_suffix(ext)).unwrap_or(page);
            XrefTarget::Document { page, name, fragment: written_fragment }
        }
        XrefForm::Macro => match page.strip_suffix(".adoc") {
            Some(name) => XrefTarget::Document { page, name, fragment: written_fragment },
            None if has_extension(page) => XrefTarget::File { page, fragment: written_fragment },
            None if fragment.is_some() => XrefTarget::Document { page, name: page, fragment: written_fragment },
            None => XrefTarget::Id { id: Some(target) },
        },
    }
}

/// A `CrossReference` represents an inline cross-reference (xref) in a document.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CrossReference<'a> {
    pub target: &'a str,
    #[serde(skip_serializing)]
    pub text: Vec<InlineNode<'a>>,
    pub form: XrefForm,
    pub location: Location,
    /// Where `target` itself is written, so a consumer can rewrite exactly it.
    pub target_location: Location,
}

impl<'a> CrossReference<'a> {
    /// Creates a new shorthand `CrossReference` with the given target.
    #[must_use]
    pub fn new(target: &'a str, location: Location) -> Self {
        Self {
            target,
            text: Vec::new(),
            form: XrefForm::Shorthand,
            target_location: location.clone(),
            location,
        }
    }

    /// What this reference names — [`xref_target_kind`] over its target and form.
    #[must_use]
    pub fn target_kind(&self) -> XrefTarget<'a> {
        xref_target_kind(self.target, self.form)
    }

    /// Sets the cross-reference display text as inline nodes.
    #[must_use]
    pub fn with_text(mut self, text: Vec<InlineNode<'a>>) -> Self {
        self.text = text;
        self
    }
}

/// An `Autolink` represents an inline autolink in a document.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Autolink<'a> {
    pub url: Source<'a>,
    /// Whether the autolink was written with angle brackets (e.g., `<user@example.com>`).
    /// When true, the renderer should preserve the brackets in the output.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bracketed: bool,
    pub location: Location,
}

/// A `Stem` represents an inline mathematical expression.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Stem<'a> {
    pub content: &'a str,
    pub notation: StemNotation,
    pub location: Location,
}

/// The kind of index term, encoding both visibility and structure.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub enum IndexTermKind<'a> {
    /// Visible in output, single term only.
    Flow(&'a str),
    /// Hidden from output, supports hierarchical entries.
    Concealed {
        term: &'a str,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secondary: Option<&'a str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tertiary: Option<&'a str>,
    },
}

/// An `IndexTerm` represents an index term in a document.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct IndexTerm<'a> {
    /// The kind and content of this index term.
    pub kind: IndexTermKind<'a>,
    pub location: Location,
}

impl IndexTerm<'_> {
    /// Returns the primary term.
    #[must_use]
    pub fn term(&self) -> &str {
        match &self.kind {
            IndexTermKind::Flow(term) | IndexTermKind::Concealed { term, .. } => term,
        }
    }

    /// Returns the secondary term, if any.
    #[must_use]
    pub fn secondary(&self) -> Option<&str> {
        match &self.kind {
            IndexTermKind::Flow(_) => None,
            IndexTermKind::Concealed { secondary, .. } => *secondary,
        }
    }

    /// Returns the tertiary term, if any.
    #[must_use]
    pub fn tertiary(&self) -> Option<&str> {
        match &self.kind {
            IndexTermKind::Flow(_) => None,
            IndexTermKind::Concealed { tertiary, .. } => *tertiary,
        }
    }

    /// Returns whether this term is visible in the output.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        matches!(self.kind, IndexTermKind::Flow(_))
    }
}
