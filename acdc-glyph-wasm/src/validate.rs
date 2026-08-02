//! Glyph-specific reference validation over the parsed AsciiDoc AST.
//!
//! Emits non-fatal diagnostics that `acdc-parser` itself does NOT — kept in
//! this binding crate (not the published parser) so the parser's fixture
//! suite and other consumers (CLI / converters / LSP) are untouched:
//!
//!  - `DuplicateAnchorId` — the same anchor id is defined more than once.
//!    (Unresolved cross-references are reported by acdc-parser directly via
//!    `WarningKind::UnresolvedReference`, so they are NOT duplicated here.)
//!
//! Pure AST analysis over acdc's PUBLIC types — mirrors the walk the
//! `acdc-lsp` definition module uses, but routes results into Glyph's
//! `WarningJson` stream instead of LSP diagnostics. Locations are raw
//! (post-include-expansion) source positions, identical to acdc's own
//! warnings; Glyph's `asciidoc.ts` translates them back to main-source lines.

use std::collections::{HashMap, HashSet};

use acdc_parser::{
    Block, BlockMetadata, DelimitedBlockType, Document, InlineMacro, InlineNode, Location, Section,
};

/// A reference diagnostic discovered by the post-parse walk. Maps 1:1 to a
/// `WarningJson` in the wasm envelope. `kind` is a stable category token
/// (no `Debug` braces) consumed as the diagnostic category by Glyph.
pub(crate) struct RefWarning {
    pub(crate) kind: &'static str,
    pub(crate) message: String,
    pub(crate) line: Option<usize>,
    pub(crate) column: Option<usize>,
}

/// Walk the document AST and return reference warnings (see module docs).
pub(crate) fn collect_reference_warnings(doc: &Document) -> Vec<RefWarning> {
    let mut warnings: Vec<RefWarning> = Vec::new();

    // Pass 1: collect every defined anchor id, emitting a DuplicateAnchorId
    // warning the first time a second definition of the same id is seen.
    let mut anchors: HashMap<String, Location> = HashMap::new();
    for block in &doc.blocks {
        collect_block_anchors(block, &mut anchors, &mut warnings);
    }

    // Unresolved cross-references are now reported by acdc-parser itself
    // (`WarningKind::UnresolvedReference`, from its cross-reference catalog),
    // so this crate no longer duplicates that check — it emits ONLY the
    // glyph-specific `DuplicateAnchorId` (Pass 1), which upstream has no
    // equivalent for.
    warnings
}

/// Record an anchor id, emitting a DuplicateAnchorId warning when an id with a
/// DIFFERENT source span was already recorded. Identical-span re-inserts (a
/// node reached twice by the walk) are ignored so they never self-report.
fn insert_anchor(
    id: String,
    loc: &Location,
    anchors: &mut HashMap<String, Location>,
    warnings: &mut Vec<RefWarning>,
) {
    if let Some(existing) = anchors.get(&id) {
        if existing.absolute_start == loc.absolute_start
            && existing.absolute_end == loc.absolute_end
        {
            return;
        }
        warnings.push(RefWarning {
            kind: "DuplicateAnchorId",
            message: format!("duplicate anchor id: '{id}' is already defined"),
            line: Some(loc.start.line as usize),
            column: Some(loc.start.column as usize),
        });
        return;
    }
    anchors.insert(id, loc.clone());
}

/// Section heading-line span (start → end of title), narrower than the full
/// section span which covers all child content.
fn heading_line_location(section: &Section) -> Location {
    let mut loc = section.location.clone();
    if let Some(last_inline) = section.title.last() {
        let title_loc = last_inline.location();
        loc.absolute_end = title_loc.absolute_end;
        loc.end = title_loc.end.clone();
    }
    loc
}

#[allow(clippy::wildcard_enum_match_arm)] // Block is #[non_exhaustive]
fn collect_block_anchors(
    block: &Block,
    anchors: &mut HashMap<String, Location>,
    warnings: &mut Vec<RefWarning>,
) {
    match block {
        Block::Section(section) => collect_section_anchors(section, anchors, warnings),
        Block::Paragraph(para) => {
            collect_metadata_anchors(&para.metadata, &para.location, anchors, warnings);
            collect_inline_anchors(&para.title, anchors, warnings);
            collect_inline_anchors(&para.content, anchors, warnings);
        }
        Block::DelimitedBlock(delimited) => {
            collect_metadata_anchors(&delimited.metadata, &delimited.location, anchors, warnings);
            collect_inline_anchors(&delimited.title, anchors, warnings);
            collect_delimited_block_anchors(&delimited.inner, anchors, warnings);
        }
        // Lists + admonitions ALSO carry their own `metadata: BlockMetadata`
        // AND `title: Title`. `[[list-id]]\n* item` registers `list-id` on
        // list.metadata.anchors — without this walk, `<<list-id>>` falsely
        // flags as unresolved.
        Block::UnorderedList(list) => {
            collect_metadata_anchors(&list.metadata, &list.location, anchors, warnings);
            collect_inline_anchors(&list.title, anchors, warnings);
            for item in &list.items {
                collect_inline_anchors(&item.principal, anchors, warnings);
                for b in &item.blocks {
                    collect_block_anchors(b, anchors, warnings);
                }
            }
        }
        Block::OrderedList(list) => {
            collect_metadata_anchors(&list.metadata, &list.location, anchors, warnings);
            collect_inline_anchors(&list.title, anchors, warnings);
            for item in &list.items {
                collect_inline_anchors(&item.principal, anchors, warnings);
                for b in &item.blocks {
                    collect_block_anchors(b, anchors, warnings);
                }
            }
        }
        Block::DescriptionList(list) => {
            collect_metadata_anchors(&list.metadata, &list.location, anchors, warnings);
            collect_inline_anchors(&list.title, anchors, warnings);
            for item in &list.items {
                for anchor in &item.anchors {
                    insert_anchor(anchor.id.to_string(), &anchor.location, anchors, warnings);
                }
                collect_inline_anchors(&item.term, anchors, warnings);
                collect_inline_anchors(&item.principal_text, anchors, warnings);
                for b in &item.description {
                    collect_block_anchors(b, anchors, warnings);
                }
            }
        }
        Block::Admonition(adm) => {
            collect_metadata_anchors(&adm.metadata, &adm.location, anchors, warnings);
            collect_inline_anchors(&adm.title, anchors, warnings);
            for b in &adm.blocks {
                collect_block_anchors(b, anchors, warnings);
            }
        }
        // Media + structural blocks all carry `metadata: BlockMetadata` (can have
        // `[#id]` / `[[id]]`) and a title (which can hold inline anchors). Walk
        // both so e.g. `image::hero.png[id=hero]` registers `hero` and
        // `<<hero>>` resolves.
        Block::Image(img) => {
            collect_metadata_anchors(&img.metadata, &img.location, anchors, warnings);
            collect_inline_anchors(&img.title, anchors, warnings);
        }
        Block::Audio(a) => {
            collect_metadata_anchors(&a.metadata, &a.location, anchors, warnings);
            collect_inline_anchors(&a.title, anchors, warnings);
        }
        Block::Video(v) => {
            collect_metadata_anchors(&v.metadata, &v.location, anchors, warnings);
            collect_inline_anchors(&v.title, anchors, warnings);
        }
        Block::PageBreak(pb) => {
            collect_metadata_anchors(&pb.metadata, &pb.location, anchors, warnings);
            collect_inline_anchors(&pb.title, anchors, warnings);
        }
        Block::DiscreteHeader(h) => {
            collect_metadata_anchors(&h.metadata, &h.location, anchors, warnings);
            collect_inline_anchors(&h.title, anchors, warnings);
        }
        Block::CalloutList(cl) => {
            collect_metadata_anchors(&cl.metadata, &cl.location, anchors, warnings);
            collect_inline_anchors(&cl.title, anchors, warnings);
            for item in &cl.items {
                collect_inline_anchors(&item.principal, anchors, warnings);
            }
        }
        // ThematicBreak is the one block type that carries anchors DIRECTLY (no
        // metadata wrapper) — see acdc-parser model.
        Block::ThematicBreak(tb) => {
            for anchor in &tb.anchors {
                insert_anchor(anchor.id.to_string(), &anchor.location, anchors, warnings);
            }
            collect_inline_anchors(&tb.title, anchors, warnings);
        }
        // `[[id]]\ntoc::[]` puts `id` on the TOC block's BlockMetadata — the
        // generated TOC itself is a valid xref target.
        Block::TableOfContents(toc) => {
            collect_metadata_anchors(&toc.metadata, &toc.location, anchors, warnings);
        }
        _ => {}
    }
}

fn collect_section_anchors(
    section: &Section,
    anchors: &mut HashMap<String, Location>,
    warnings: &mut Vec<RefWarning>,
) {
    // `generate_id_string` already resolves the section's primary id, which is
    // the explicit `[#id]` / last `[[id]]` anchor when present (see
    // Section::explicit_id). Inserting it AND then iterating metadata.anchors
    // would double-count that same anchor → a false DuplicateAnchorId. Skip
    // the anchor(s) already represented by the primary id.
    //
    // Synonym dedup mirrors collect_metadata_anchors: `[[a]]\n[[a]]\n== S`
    // pushes both `a` entries into section.metadata.anchors. Without
    // `seen_in_block` the second insert would false-flag DuplicateAnchorId
    // against itself. Also skip `metadata.id` (the `[id=foo]` attribute form)
    // since it's the same anchor declared two ways.
    let id = Section::generate_id_string(&section.metadata, &section.title);
    let primary_id_attr: Option<&str> = section.metadata.id.as_ref().map(|a| a.id);
    let mut seen_in_block: HashSet<&str> = HashSet::new();
    for anchor in &section.metadata.anchors {
        if anchor.id == id.as_str() || Some(anchor.id) == primary_id_attr {
            continue;
        }
        if !seen_in_block.insert(anchor.id) {
            continue;
        }
        insert_anchor(anchor.id.to_string(), &anchor.location, anchors, warnings);
    }
    insert_anchor(id, &heading_line_location(section), anchors, warnings);
    // Heading titles can carry inline anchors (`== [[hdr]] My Heading`); skipping
    // them would false-flag `<<hdr>>` as unresolved.
    collect_inline_anchors(&section.title, anchors, warnings);
    for child in &section.content {
        collect_block_anchors(child, anchors, warnings);
    }
}

fn collect_metadata_anchors(
    metadata: &BlockMetadata,
    _block_location: &Location,
    anchors: &mut HashMap<String, Location>,
    warnings: &mut Vec<RefWarning>,
) {
    // Two layers of intra-block dedup so synonyms don't false-trigger
    // `DuplicateAnchorId` against themselves:
    //   1. acdc empirically pushes `[#foo]\n[[foo]]` as TWO entries with the
    //      same id into metadata.anchors (verified at parse time on
    //      2026-05-29). Treat them as one anchor — both reference the same
    //      target. `seen_in_block` tracks ids already inserted from THIS
    //      block's metadata so a same-id repeat is silently skipped.
    //   2. If `metadata.id` is set (the `[id=foo]` attribute-list form),
    //      skip any metadata.anchors entry with the same id — they're the
    //      same anchor declared two ways.
    // Then insert `metadata.id` at its OWN anchor location (NOT the block's
    // whole-span location) — the previous use of `block_location` produced
    // misleading diagnostic positions and broke same-id dedup across walks.
    let primary_id: Option<&str> = metadata.id.as_ref().map(|a| a.id);
    let mut seen_in_block: HashSet<&str> = HashSet::new();
    for anchor in &metadata.anchors {
        if Some(anchor.id) == primary_id {
            continue;
        }
        if !seen_in_block.insert(anchor.id) {
            continue;
        }
        insert_anchor(anchor.id.to_string(), &anchor.location, anchors, warnings);
    }
    if let Some(id_anchor) = &metadata.id {
        insert_anchor(
            id_anchor.id.to_string(),
            &id_anchor.location,
            anchors,
            warnings,
        );
    }
}

#[allow(clippy::wildcard_enum_match_arm)] // DelimitedBlockType is #[non_exhaustive]
fn collect_delimited_block_anchors(
    inner: &DelimitedBlockType,
    anchors: &mut HashMap<String, Location>,
    warnings: &mut Vec<RefWarning>,
) {
    match inner {
        DelimitedBlockType::DelimitedExample(blocks)
        | DelimitedBlockType::DelimitedOpen(blocks)
        | DelimitedBlockType::DelimitedSidebar(blocks)
        | DelimitedBlockType::DelimitedQuote(blocks) => {
            for block in blocks {
                collect_block_anchors(block, anchors, warnings);
            }
        }
        DelimitedBlockType::DelimitedListing(inlines)
        | DelimitedBlockType::DelimitedLiteral(inlines)
        | DelimitedBlockType::DelimitedPass(inlines)
        | DelimitedBlockType::DelimitedVerse(inlines)
        | DelimitedBlockType::DelimitedComment(inlines) => {
            collect_inline_anchors(inlines, anchors, warnings);
        }
        // Table cells hold `Vec<Block>` — asciidoc-style cells (`a|`) can carry
        // `[[id]]` block anchors. Walk every cell across header / body rows /
        // footer so `<<id-in-cell>>` from elsewhere resolves.
        DelimitedBlockType::DelimitedTable(t) => {
            for row in t.header.iter().chain(t.rows.iter()).chain(t.footer.iter()) {
                for col in &row.columns {
                    for b in &col.content {
                        collect_block_anchors(b, anchors, warnings);
                    }
                }
            }
        }
        _ => {}
    }
}

#[allow(clippy::wildcard_enum_match_arm)] // InlineNode + InlineMacro are #[non_exhaustive]
fn collect_inline_anchors(
    inlines: &[InlineNode],
    anchors: &mut HashMap<String, Location>,
    warnings: &mut Vec<RefWarning>,
) {
    for inline in inlines {
        match inline {
            InlineNode::InlineAnchor(anchor) => {
                insert_anchor(anchor.id.to_string(), &anchor.location, anchors, warnings);
            }
            // Formatted-text spans (`[#sid]*bold*`, `[.role#rid]_italic_`, etc.)
            // carry an explicit id on the span struct itself. acdc populates
            // `id: Option<&str>` from the attribute prefix — verified 2026-05-29
            // via probe. Must register so `<<sid>>` resolves.
            InlineNode::BoldText(b) => {
                if let Some(id) = b.id {
                    insert_anchor(id.to_string(), &b.location, anchors, warnings);
                }
                collect_inline_anchors(&b.content, anchors, warnings);
            }
            InlineNode::ItalicText(i) => {
                if let Some(id) = i.id {
                    insert_anchor(id.to_string(), &i.location, anchors, warnings);
                }
                collect_inline_anchors(&i.content, anchors, warnings);
            }
            InlineNode::MonospaceText(m) => {
                if let Some(id) = m.id {
                    insert_anchor(id.to_string(), &m.location, anchors, warnings);
                }
                collect_inline_anchors(&m.content, anchors, warnings);
            }
            InlineNode::HighlightText(h) => {
                if let Some(id) = h.id {
                    insert_anchor(id.to_string(), &h.location, anchors, warnings);
                }
                collect_inline_anchors(&h.content, anchors, warnings);
            }
            InlineNode::SubscriptText(s) => {
                if let Some(id) = s.id {
                    insert_anchor(id.to_string(), &s.location, anchors, warnings);
                }
                collect_inline_anchors(&s.content, anchors, warnings);
            }
            InlineNode::SuperscriptText(s) => {
                if let Some(id) = s.id {
                    insert_anchor(id.to_string(), &s.location, anchors, warnings);
                }
                collect_inline_anchors(&s.content, anchors, warnings);
            }
            // Curved quotes / apostrophes wrap further inline content AND carry
            // their own `id` field — same convention as the bold/italic spans.
            InlineNode::CurvedQuotationText(q) => {
                if let Some(id) = q.id {
                    insert_anchor(id.to_string(), &q.location, anchors, warnings);
                }
                collect_inline_anchors(&q.content, anchors, warnings);
            }
            InlineNode::CurvedApostropheText(a) => {
                if let Some(id) = a.id {
                    insert_anchor(id.to_string(), &a.location, anchors, warnings);
                }
                collect_inline_anchors(&a.content, anchors, warnings);
            }
            // Inline macros that carry nested inlines: footnote/link/url/mailto
            // (display text), inline-image (its own metadata id + title), xref
            // (display text can contain `[[id]]`).
            InlineNode::Macro(m) => match m {
                InlineMacro::Footnote(f) => {
                    // `footnote:my-fn[content]` declares an inline anchor with
                    // id `my-fn` — asciidoctor renders `<a id="my-fn">`. The
                    // EMPTY-content form `footnote:my-fn[]` is a REFERENCE to
                    // the prior declaration (back-link), NOT a redeclaration:
                    // registering it false-flags `DuplicateAnchorId` against
                    // its own original. Only treat non-empty-content as the
                    // anchor declaration.
                    if let Some(id) = f.id {
                        if !f.content.is_empty() {
                            insert_anchor(id.to_string(), &f.location, anchors, warnings);
                        }
                    }
                    collect_inline_anchors(&f.content, anchors, warnings);
                }
                InlineMacro::Link(l) => collect_inline_anchors(&l.text, anchors, warnings),
                InlineMacro::Url(u) => collect_inline_anchors(&u.text, anchors, warnings),
                InlineMacro::Mailto(ma) => collect_inline_anchors(&ma.text, anchors, warnings),
                InlineMacro::Image(img) => {
                    collect_metadata_anchors(&img.metadata, &img.location, anchors, warnings);
                    collect_inline_anchors(&img.title, anchors, warnings);
                }
                InlineMacro::CrossReference(xref) => {
                    collect_inline_anchors(&xref.text, anchors, warnings)
                }
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::collect_reference_warnings;
    use acdc_parser::{Options, parse};

    fn warn_kinds(src: &str) -> Vec<(String, String)> {
        let opts = Options::default();
        let parsed = parse(src, &opts).expect("parse succeeds");
        collect_reference_warnings(parsed.document())
            .into_iter()
            .map(|w| (w.kind.to_string(), w.message))
            .collect()
    }

    // Unresolved-cross-reference validation moved to acdc-parser
    // (`WarningKind::UnresolvedReference`), covered by the parser's own tests;
    // this crate only owns the glyph-specific `DuplicateAnchorId` check below.

    #[test]
    fn duplicate_explicit_anchor_warns() {
        let src = "= Doc\n\n[[dup]]\nFirst para.\n\n[[dup]]\nSecond para.\n";
        let kinds = warn_kinds(src);
        let dups: Vec<_> = kinds
            .iter()
            .filter(|(k, _)| k == "DuplicateAnchorId")
            .collect();
        assert_eq!(dups.len(), 1, "expected 1 duplicate-anchor, got: {kinds:?}");
        assert!(
            dups.first().is_some_and(|(_, m)| m.contains("dup")),
            "message should name the id: {kinds:?}"
        );
    }

    #[test]
    fn unique_anchors_no_duplicate_warning() {
        let src = "= Doc\n\n[[a]]\nPara A.\n\n[[b]]\nPara B.\n";
        let kinds = warn_kinds(src);
        assert!(
            kinds.iter().all(|(k, _)| k != "DuplicateAnchorId"),
            "unique anchors should not warn, got: {kinds:?}"
        );
    }

    #[test]
    fn synonym_anchor_pair_does_not_false_duplicate() {
        // `[#foo]\n[[foo]]` is a SYNONYM (one anchor declared two ways), not
        // two duplicates. acdc empirically populates metadata.anchors with
        // BOTH entries sharing the same id "foo" — verified at parse time
        // 2026-05-29. collect_metadata_anchors must dedup within the block.
        let src = "= Doc\n\n[#foo]\n[[foo]]\nPara.\n";
        let kinds = warn_kinds(src);
        assert!(
            kinds.iter().all(|(k, _)| k != "DuplicateAnchorId"),
            "synonym anchor pair must not false-flag DuplicateAnchorId, got: {kinds:?}"
        );
    }

    #[test]
    fn section_synonym_anchor_pair_does_not_false_duplicate() {
        // Section parallel to `synonym_anchor_pair_does_not_false_duplicate`:
        // section metadata.anchors can carry `[[a]]\n[[a]]` synonyms (verified
        // 2026-05-29). `Section::generate_id_string` picks one as primary; the
        // walk must dedup the remaining same-id entries within the section
        // block, else a syntactically valid synonym pair false-flags as
        // DuplicateAnchorId.
        let src = "= Doc\n\n[[a]]\n[[a]]\n[[b]]\n== Section\n\nbody\n";
        let kinds = warn_kinds(src);
        assert!(
            kinds.iter().all(|(k, _)| k != "DuplicateAnchorId"),
            "section synonym anchors must not false-flag DuplicateAnchorId, got: {kinds:?}"
        );
    }

    #[test]
    fn footnote_reference_form_does_not_false_duplicate() {
        // `footnote:fn1[body]` declares the footnote; `footnote:fn1[]` later in
        // the doc is a REFERENCE to that footnote (Asciidoctor semantics —
        // emits a back-link, NOT a re-declaration). Empty-content form must
        // NOT register as an anchor, else the reference false-flags
        // DuplicateAnchorId against the original declaration.
        let src = "= Doc\n\nFirst footnote:fn1[the body].\n\nSecond ref footnote:fn1[].\n";
        let kinds = warn_kinds(src);
        assert!(
            kinds.iter().all(|(k, _)| k != "DuplicateAnchorId"),
            "footnote reference form must not false-flag DuplicateAnchorId, got: {kinds:?}"
        );
    }
}
