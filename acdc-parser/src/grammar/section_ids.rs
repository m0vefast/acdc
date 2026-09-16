//! Section and discrete heading id allocation.
//!
//! asciidoctor gives every section and discrete heading an id from one document-wide
//! catalog filled in parse order: an explicit id is kept as written, a generated id
//! takes the first free `_2`, `_3`, … suffix. When a heading is reached the catalog
//! holds the document id, every earlier section and discrete heading id, the ids of
//! blocks already parsed (a block registers after its content), anchors in paragraph
//! text, and the anchor that opens a list item, a description list term, a PSV table
//! cell outside `a` and `l` cells, or a bibliography entry. Other anchors — later in
//! list item, term or cell text, in verse blocks and block titles — and formatted span
//! ids are cataloged only at conversion, so they do not reserve a name.

use std::collections::HashSet;

use bumpalo::Bump;

use crate::{
    AnchorKind, Block, BlockMetadata, ColumnStyle, DelimitedBlock, DelimitedBlockType, Header,
    InlineNode, ListItem, Section, SectionKind, Table, TableRow, TocEntry,
};

/// Assign `id` to every section and discrete heading in `blocks`, and give each TOC
/// entry the id of the section it was registered for.
pub(crate) fn assign_section_ids<'a>(
    arena: &'a Bump,
    header: Option<&Header<'a>>,
    blocks: &mut [Block<'a>],
    toc_entries: &mut [TocEntry<'a>],
) {
    let mut ids = SectionIds {
        arena,
        reserved: HashSet::new(),
        toc_entries,
        next_toc_entry: 0,
    };
    if let Some(id) = header.and_then(|header| Section::explicit_id(&header.metadata)) {
        ids.reserved.insert(id);
    }
    ids.blocks(blocks);
}

struct SectionIds<'a, 't> {
    arena: &'a Bump,
    reserved: HashSet<&'a str>,
    toc_entries: &'t mut [TocEntry<'a>],
    next_toc_entry: usize,
}

impl<'a> SectionIds<'a, '_> {
    fn blocks(&mut self, blocks: &mut [Block<'a>]) {
        for block in blocks {
            self.block(block, false);
        }
    }

    /// `in_bibliography_section`: `block` sits directly in a `[bibliography]` section.
    fn block(&mut self, block: &mut Block<'a>, in_bibliography_section: bool) {
        match block {
            Block::Section(section) => {
                section.id = self.assign(&section.metadata, &section.title);
                self.set_toc_entry_id(section);
                let bibliography = section.kind == SectionKind::Bibliography;
                for block in &mut section.content {
                    self.block(block, bibliography);
                }
                return;
            }
            Block::DiscreteHeader(header) => {
                header.id = self.assign(&header.metadata, &header.title);
                return;
            }
            Block::Paragraph(paragraph) => self.paragraph_anchors(&paragraph.content),
            Block::Admonition(admonition) => self.blocks(&mut admonition.blocks),
            Block::DelimitedBlock(delimited) => self.delimited(delimited),
            Block::UnorderedList(list) => {
                let bibliography = list
                    .metadata
                    .style
                    .map_or(in_bibliography_section, |style| style == "bibliography");
                self.list_items(&mut list.items, bibliography);
            }
            Block::OrderedList(list) => self.list_items(&mut list.items, false),
            Block::CalloutList(list) => {
                for item in &mut list.items {
                    self.leading_anchor(&item.principal, false);
                    self.blocks(&mut item.blocks);
                }
            }
            Block::DescriptionList(list) => {
                for item in &mut list.items {
                    self.leading_anchor(&item.term, false);
                    self.blocks(&mut item.description);
                }
            }
            Block::ThematicBreak(_)
            | Block::PageBreak(_)
            | Block::Image(_)
            | Block::Audio(_)
            | Block::Video(_)
            | Block::TableOfContents(_)
            | Block::DocumentAttribute(_)
            | Block::Comment(_) => {}
        }
        if let Some(anchor) = block.anchor() {
            self.reserved.insert(anchor.id);
        }
    }

    fn delimited(&mut self, delimited: &mut DelimitedBlock<'a>) {
        match &mut delimited.inner {
            DelimitedBlockType::DelimitedExample(blocks)
            | DelimitedBlockType::DelimitedOpen(blocks)
            | DelimitedBlockType::DelimitedSidebar(blocks)
            | DelimitedBlockType::DelimitedQuote(blocks) => self.blocks(blocks),
            DelimitedBlockType::DelimitedTable(table) => {
                let psv = Table::is_psv(
                    Table::format_attribute(&delimited.metadata),
                    delimited.delimiter,
                );
                if let Some(row) = &mut table.header {
                    self.table_row(row, psv, true);
                }
                for row in &mut table.rows {
                    self.table_row(row, psv, false);
                }
                if let Some(row) = &mut table.footer {
                    self.table_row(row, psv, false);
                }
            }
            DelimitedBlockType::DelimitedListing(_)
            | DelimitedBlockType::DelimitedLiteral(_)
            | DelimitedBlockType::DelimitedPass(_)
            | DelimitedBlockType::DelimitedVerse(_)
            | DelimitedBlockType::DelimitedComment(_)
            | DelimitedBlockType::DelimitedStem(_) => {}
        }
    }

    /// asciidoctor reads every header row cell as a normal cell, whatever its style.
    fn table_row(&mut self, row: &mut TableRow<'a>, psv: bool, header: bool) {
        for cell in &mut row.columns {
            match cell.style {
                Some(ColumnStyle::AsciiDoc) if !header => self.blocks(&mut cell.content),
                Some(ColumnStyle::Literal) if !header => {}
                _ if psv => {
                    if let Some(Block::Paragraph(paragraph)) = cell.content.first() {
                        self.leading_anchor(&paragraph.content, false);
                    }
                }
                _ => {}
            }
        }
    }

    fn list_items(&mut self, items: &mut [ListItem<'a>], bibliography: bool) {
        for item in items {
            if item.checked.is_none() {
                self.leading_anchor(&item.principal, bibliography);
            }
            self.blocks(&mut item.blocks);
        }
    }

    /// The `[[id]]` that opens the text, or the `[[[id]]]` that opens a bibliography entry.
    fn leading_anchor(&mut self, inlines: &[InlineNode<'a>], bibliography: bool) {
        if let Some(InlineNode::InlineAnchor(anchor)) = inlines.first()
            && matches!(anchor.kind, AnchorKind::Bibliography) == bibliography
        {
            self.reserved.insert(anchor.id);
        }
    }

    fn paragraph_anchors(&mut self, inlines: &[InlineNode<'a>]) {
        for inline in inlines {
            match inline {
                InlineNode::InlineAnchor(anchor) => {
                    self.reserved.insert(anchor.id);
                }
                InlineNode::BoldText(text) => self.paragraph_anchors(&text.content),
                InlineNode::ItalicText(text) => self.paragraph_anchors(&text.content),
                InlineNode::MonospaceText(text) => self.paragraph_anchors(&text.content),
                InlineNode::HighlightText(text) => self.paragraph_anchors(&text.content),
                InlineNode::SubscriptText(text) => self.paragraph_anchors(&text.content),
                InlineNode::SuperscriptText(text) => self.paragraph_anchors(&text.content),
                InlineNode::CurvedQuotationText(text) => self.paragraph_anchors(&text.content),
                InlineNode::CurvedApostropheText(text) => self.paragraph_anchors(&text.content),
                InlineNode::PlainText(_)
                | InlineNode::RawText(_)
                | InlineNode::VerbatimText(_)
                | InlineNode::StandaloneCurvedApostrophe(_)
                | InlineNode::LineBreak(_)
                | InlineNode::Macro(_)
                | InlineNode::CalloutRef(_) => {}
            }
        }
    }

    /// The explicit id as written; otherwise the generated id, numbered past every id
    /// already in use.
    fn assign(&mut self, metadata: &BlockMetadata<'a>, title: &[InlineNode<'a>]) -> &'a str {
        if let Some(id) = Section::explicit_id(metadata) {
            self.reserved.insert(id);
            return id;
        }
        let generated = Section::generated_id_from_title(title);
        let mut id = generated.clone();
        let mut suffix = 2;
        while self.reserved.contains(id.as_str()) {
            id = format!("{generated}_{suffix}");
            suffix += 1;
        }
        let id = self.arena.alloc_str(&id);
        self.reserved.insert(id);
        id
    }

    /// The grammar registers a TOC entry as it parses each section title, so the entry
    /// for `section` is the next one carrying that identical title.
    fn set_toc_entry_id(&mut self, section: &Section<'a>) {
        let Some(pending) = self.toc_entries.get_mut(self.next_toc_entry..) else {
            return;
        };
        if let Some((offset, entry)) = pending
            .iter_mut()
            .enumerate()
            .find(|(_, entry)| entry.title == section.title)
        {
            entry.id = section.id;
            self.next_toc_entry += offset + 1;
        }
    }
}
