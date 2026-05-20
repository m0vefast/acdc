//! Anchor and reference types for `AsciiDoc` documents.

use serde::{
    Serialize,
    ser::{SerializeMap, Serializer},
};

use super::location::Location;
use super::title::Title;

/// Section styles that should not receive automatic numbering.
///
/// When `sectnums` is enabled, sections with these styles are excluded from
/// the numbering scheme. Appendix uses letter numbering (A, B, C) which is
/// handled separately.
pub const UNNUMBERED_SECTION_STYLES: &[&str] = &[
    "preface",
    "abstract",
    "dedication",
    "colophon",
    "bibliography",
    "glossary",
    "index",
    "appendix",
];

/// Anchor flavor — distinguishes the visual rendering Asciidoctor expects.
///
/// `Inline` (`[[id]]`) renders as an invisible `<a id>` marker; `Bibliography`
/// (`[[[id]]]`) renders with a visible `[id]` label (or `[reftext]` if the
/// 3-argument form was used). Both forms produce `InlineNode::InlineAnchor`
/// with the same id/xreflabel fields — the kind tag is the only structural
/// difference and downstream consumers need it to switch rendering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AnchorKind {
    #[default]
    Inline,
    Bibliography,
}

impl AnchorKind {
    /// True for the default flavor — used by `#[serde(skip_serializing_if)]`
    /// so existing block-anchor fixtures stay byte-equal.
    #[must_use]
    pub fn is_inline(&self) -> bool {
        matches!(self, AnchorKind::Inline)
    }
}

/// An `Anchor` represents an anchor in a document.
///
/// An anchor is a reference point in a document that can be linked to.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Anchor<'a> {
    pub id: &'a str,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xreflabel: Option<&'a str>,
    #[serde(default, skip_serializing_if = "AnchorKind::is_inline")]
    pub kind: AnchorKind,
    pub location: Location,
}

impl<'a> Anchor<'a> {
    /// Create a new anchor with the given ID and location.
    #[must_use]
    pub fn new(id: &'a str, location: Location) -> Self {
        Self {
            id,
            xreflabel: None,
            kind: AnchorKind::default(),
            location,
        }
    }

    /// Set the cross-reference label.
    #[must_use]
    pub fn with_xreflabel(mut self, xreflabel: Option<&'a str>) -> Self {
        self.xreflabel = xreflabel;
        self
    }

    /// Mark this anchor as a bibliography reference (`[[[id]]]` syntax).
    #[must_use]
    pub fn with_kind(mut self, kind: AnchorKind) -> Self {
        self.kind = kind;
        self
    }
}

/// A `TocEntry` represents a table of contents entry.
///
/// This is collected during parsing from Section.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct TocEntry<'a> {
    /// Unique identifier for this section (used for anchor links)
    pub id: &'a str,
    /// Title of the section
    pub title: Title<'a>,
    /// Section level (1 for top-level, 2 for subsection, etc.)
    pub level: u8,
    /// Optional cross-reference label (from `[[id,xreflabel]]` syntax)
    pub xreflabel: Option<&'a str>,
    /// Whether this section should be numbered when `sectnums` is enabled.
    ///
    /// False for special section styles like `[bibliography]`, `[glossary]`, etc.
    pub numbered: bool,
    /// Optional style from block metadata (e.g., "appendix", "bibliography").
    pub style: Option<&'a str>,
}

impl Serialize for TocEntry<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_map(None)?;
        state.serialize_entry("id", &self.id)?;
        state.serialize_entry("title", &self.title)?;
        state.serialize_entry("level", &self.level)?;
        if self.xreflabel.is_some() {
            state.serialize_entry("xreflabel", &self.xreflabel)?;
        }
        if self.style.is_some() {
            state.serialize_entry("style", &self.style)?;
        }
        state.end()
    }
}
