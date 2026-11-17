use crate::{Footnote, ast::ContentRef};

use super::FootnoteRef;

impl<'doc> FootnoteRef<'doc> {
    /// The named footnote key; `None` for an anonymous note.
    #[must_use]
    pub fn id(self) -> Option<&'doc str> {
        self.node.id
    }

    /// The assigned footnote number, shared by references to the same definition.
    #[must_use]
    pub fn number(self) -> u32 {
        self.node.number
    }

    /// False for a reference to no registered definition (asciidoctor's unresolved
    /// `footnoteref`); it then has number 0 and no body.
    #[must_use]
    pub fn is_resolved(self) -> bool {
        !self.node.is_unresolved()
    }

    /// What a text rendering writes between brackets: the number, or an unresolved
    /// reference's key (asciidoctor's `[missing]`).
    #[must_use]
    pub fn mark(self) -> String {
        if self.is_resolved() {
            self.node.number.to_string()
        } else {
            self.node.id.unwrap_or_default().to_owned()
        }
    }

    /// The retained definition body, including an explicitly empty body.
    /// References, including repeated source definitions, return `None`.
    #[must_use]
    pub fn content(self) -> Option<ContentRef<'doc>> {
        self.node.content.as_deref().map(ContentRef::new)
    }
}

impl<'doc> From<&'doc Footnote<'doc>> for FootnoteRef<'doc> {
    fn from(owner: &'doc Footnote<'doc>) -> Self {
        Self::new(owner)
    }
}

impl<'doc> From<FootnoteRef<'doc>> for Footnote<'doc> {
    fn from(read: FootnoteRef<'doc>) -> Self {
        read.node.clone()
    }
}
