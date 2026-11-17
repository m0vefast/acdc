// The `peg` macro adds 5 hidden parameters to every rule function, so even
// rules with just 3 explicit params exceed clippy's 7-argument threshold.
#![allow(clippy::too_many_arguments)]

use crate::{
    Anchor, Attribution, Audio, Author, Block, BlockMetadata, CalloutList, CalloutListItem,
    CalloutMarker, CiteTitle, Comment, CommentKind, CompatibilityMode, DelimitedBlock,
    DelimitedBlockType, DiscreteHeading, Document, DocumentAttribute, DocumentAttributes, Error,
    Header, Image, InlineNode, RevisionLocations, ListItem, ListItemCheckedStatus, Location, PageBreak,
    Paragraph, Plain, Position, Section, Source, TableOfContents, ThematicBreak, Title,
    Video, Warning, WarningKind,
    document_attribute::{AttributeDeclaration, RawAttributeValue, join_wrapped_value},
    grammar::{
        ParserState,
        attributes::{
            BlockAttributeMode, block_attribute_list_allowed, resolve_block_anchor_reftext,
            style_overrides_block_form,
        },
        document::{
            author::{derive_author_attrs, register_author_attrs},
            delimited::{DelimitedKind, DelimitedParams, build_delimited_block},
            doctype::{is_book_doctype, is_manpage_doctype},
            header::{ParsedHeader, header_span, merge_header_metadata},
            lists::{
                ItemHead, ListMarker, assemble_principal_text, calculate_item_end, find_dlist_marker,
            },
            manpage::{
                ManpageNameSection, derive_manpage_header_attrs, derive_name_section_attrs,
                prepare_manpage_name_attributes,
            },
            metadata::{
                AttributeBlockForm, AttributeOrAnchorLine, BlockMetadataLine, MetadataComment,
                ParsedAttributeList, RawAttributeList, TitleSource, assign_block_caption,
                finish_block_parsing_metadata, media_attributes_ahead, merge_macro_metadata,
                metadata_comment, metadata_marks_discrete_heading, order_block_prefix_events,
                parse_block_attribute_list, push_metadata_anchor, resolve_block_content_attributes,
                resolve_title, route_block_positions, route_media_attributes, select_heading_style,
                thematic_break_anchors,
            },
            paragraph::{ParagraphParams, build_paragraph, paragraph_text_end},
            references::{
                ParsedSectionHeader, ReferenceCatalog, collect_header_references,
                collect_references, finalize_inline_semantics, normalize_bibliography_lists,
                register_document_references, register_section_header, resolve_source_target,
                resolve_xref_target,
            },
            revision::{IgnoredRevisionFields, RevisionInfo, RevisionSpans, process_revision_info},
            sections::{
                DocumentTitleParts, apply_leveloffset, document_title_parts, expected_child_level,
                heading_title, without_closing_marker,
            },
            table::{
                PreparedTableCellParagraph, TableClosing, TableParseParams, parse_table_block_impl,
            },
        },
        helpers::{
            BlockParsingMetadata, PositionWithOffset, is_valid_bibliography_id, restore_url_path,
            title_looks_like_description_list,
        },
        inline_preprocessing,
        inline_preprocessor::InlinePreprocessorParserState,
        inline_processing::{adjust_and_log_parse_error, process_inlines},
        setext,
        state::BlockContext,
    },
    model::{
        ListLevel, SectionKind, SectionLevel, Substitution, caption, section, substitution::HEADER,
    },
};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    rc::Rc,
};

peg::parser! {
    pub(crate) grammar document_parser(state: &mut ParserState<'input>) for str {
        use crate::model::apply_attribute_substitutions;

        // Each action gets the byte range of its preceding sequence.
        // Resolve line and column positions only when a node needs a location.
        inject span_start(_input, l, _r) -> usize { l }
        inject span_end(_input, _l, r) -> usize { r }

        // The document location excludes leading and trailing empty lines.
        pub(crate) rule document() -> Result<Document<'input>, Error>
        = eol()* start:position() comments_before_header:(comment:leading_comment() eol()* { comment })* header_result:header() prepare_manpage_front_matter() header_attributes:header_attribute_snapshot() blocks:blocks(0, None, None) end:position!() (eol()* / ![_]) {
            let ParsedHeader { header, untitled_location: untitled_header_location } = header_result?;
            let mut blocks: Vec<Block<'_>> = comments_before_header.into_iter().collect::<Result<Vec<_>, Error>>()?.into_iter().chain(blocks?).collect();
            let carried_titles = Block::deliver_carried_titles(&mut blocks, &mut None);

            // Ensure end offset is on a valid UTF-8 boundary
            let mut document_end_offset = end;
            if document_end_offset > state.input.len() {
                document_end_offset = state.input.len();
            }
            // If not on a boundary, round forward to the next boundary
            while document_end_offset < state.input.len() && !state.input.is_char_boundary(document_end_offset) {
                document_end_offset += 1;
            }
            let document_end_offset = if document_end_offset == 0 {
                0
            } else {
                crate::grammar::utf8_utils::safe_decrement_offset(state.input, document_end_offset)
            };

            // Ensure the invariant: absolute_start <= absolute_end
            let (absolute_start, absolute_end) = if start.offset > document_end_offset {
                // This can happen with whitespace-only input where eol()* consumes all content
                // In this case, treat as an empty document at the start position
                (start.offset, start.offset)
            } else {
                (start.offset, document_end_offset)
            };

            // Special case for truly empty input: TCK expects column 0
            // Only for zero-byte input, not whitespace-only
            let (start_position, end_position) = if state.input.is_empty() || (absolute_start == 0 && absolute_end == 0) {
                // Whitespace-only documents should use column 1
                (Position::new(1, 0), Position::new(1, 0))
            } else {
                (
                    start.position,
                    state.line_map.offset_to_position(absolute_end, state.input)
                )
            };

            // Warn when a top-level section skips level 1 (e.g. a document that
            // jumps straight to `=== Heading`). Matches asciidoctor's "section
            // title out of sequence" check.
            //
            // The document root sits at level 0 — so every top-level section is
            // expected at level 1 — once it is "anchored" by a document title or
            // by preamble body content (a paragraph, list, ...) before the first
            // section. When anchored, asciidoctor flags *each* top-level section
            // deeper than level 1 (not just the first). A document that opens
            // directly with a section (no title, no preamble) is not anchored:
            // that first section sets the base level and neither it nor its
            // same-or-shallower siblings are out of sequence. Comments are
            // transparent and never anchor. Sections nested under another section
            // are validated separately, in the `section` rule itself.
            //
            // `toc_entries` is populated while parsing and is empty exactly when
            // the document has no sections — checking it first lets section-less
            // documents skip the body scan entirely. Otherwise we walk only the
            // top-level blocks (preamble + sibling sections, never nested content)
            // and stop at the first section in the un-anchored case.
            if !state.toc_entries.is_empty() {
                let mut anchored = header.as_ref().is_some_and(|h| !h.title.is_empty());
                let mut seen_section = false;
                for block in &blocks {
                    if let Block::Section(section) = block {
                        if !anchored {
                            // Un-anchored leading section: it establishes the base
                            // level, so neither it nor its siblings can be out of
                            // sequence. Nothing left to check.
                            break;
                        }
                        if section.level > 1 {
                            let location = state
                                .create_error_source_location(section.location.clone());
                            state.add_warning(Warning::new(
                                WarningKind::SectionLevelOutOfSequence {
                                    expected: 1,
                                    got: section.level,
                                },
                                Some(location),
                            ));
                        }
                        seen_section = true;
                    } else if !seen_section && !matches!(block, Block::Comment(_)) {
                        // Preamble content before the first section anchors the
                        // document at level 0.
                        anchored = true;
                    }
                }
            }

            // Assign caption ordinals over the finished tree. Numbering here cannot be
            // disturbed by PEG backtracking, and it runs before the reference catalog so that
            // catalog can later carry a target's caption label and ordinal.
            normalize_bibliography_lists(state, &mut blocks);
            let mut header = header;
            crate::grammar::reference_text::resolve_tree(state, &mut header, &mut blocks)?;
            crate::grammar::document::table::finalize_footnotes(&mut blocks);
            caption::renumber_captions(&mut blocks);

            let header_has_anchor = header.as_ref().is_some_and(|header| {
                header.metadata.id.is_some() || !header.metadata.anchors.is_empty()
            });
            let mut references = ReferenceCatalog {
                entries: HashMap::with_capacity(state.toc_entries.len() + usize::from(header_has_anchor)),
                next_suffix: HashMap::new(),
                natural_targets: HashMap::new(),
            };
            let mut xrefs = Vec::new();
            if let Some(header) = &header {
                collect_header_references(state, header, &header_attributes, &mut references, &mut xrefs);
            }
            collect_references(state, &mut blocks, &mut references, &mut xrefs);
            let is_book = is_book_doctype(&header_attributes);
            section::number_parsed_sections(
                &mut blocks,
                &mut state.toc_entries,
                &mut references.entries,
                is_book,
            );
            let toc_entries = state.toc_entries.clone();

            let mut document = Document {
                header,
                untitled_header_location,
                // Source remapping assigns each boundary its own file after parsing.
                // A document can start in the entry file and end in an included file.
                location: Location {
                    absolute_start,
                    absolute_end,
                    start: start_position,
                    end: end_position,
                },
                attributes: header_attributes,
                blocks,
                footnotes: state.footnote_tracker.borrow().footnotes.clone(),
                toc_entries,
                references: references.entries,
            };
            register_document_references(state, &mut document, &xrefs, &mut references.natural_targets)?;
            let reference_ids = document.references.keys().copied().collect::<HashSet<_>>();

            // An internal `<<id>>` whose target is absent from the catalog is an
            // unresolved (broken) reference. Inter-document/external targets
            // (those addressing another resource) are not validated here.
            for xref in xrefs {
                let source_target = resolve_source_target(state, xref.target, xref.source_syntax);
                let target = source_target.unwrap_or_else(|| resolve_xref_target(
                    xref.target,
                    xref.resolve_natural_target,
                    &reference_ids,
                    &references.natural_targets,
                ));
                let target_is_local = source_target.is_some()
                    || xref.source_syntax.target_is_local(target);
                if target_is_local && xref.automatic
                    && let Some(reference) = document.references.get_mut(target)
                    && reference.is_bibliography()
                {
                    reference.automatic_citation = true;
                }
                if target_is_local && !reference_ids.contains(target)
                {
                    let source_location = state.create_error_source_location(xref.location);
                    state.add_warning(Warning::new(
                        WarningKind::UnresolvedReference {
                            target: target.to_string(),
                        },
                        Some(source_location),
                    ));
                }
            }
            finalize_inline_semantics(
                state,
                &mut document,
                &reference_ids,
                &references.natural_targets,
            )?;
            // asciidoctor numbers a note when it converts it: a carried title's notes follow
            // the heading it was written above, and a title its block's own title replaced
            // has none.
            // asciidoctor `parse_manpage_header`: `manname-id` is the id of the section the
            // name came from.
            if state.manname_from_section
                && let Some(id) = document.blocks.iter().find_map(|block| {
                    if let Block::Section(section) = block { Some(section.id) } else { None }
                }).flatten()
            {
                document.attributes.set_text("manname-id".into(), id.into());
            }
            if carried_titles {
                let location = document.location.clone();
                document.rebuild_footnotes().map_err(|error| {
                    Error::Parse(Box::new(state.create_error_source_location(location)), error.to_string())
                })?;
            }
            Ok(document)
        }

        rule header_attribute_snapshot() -> DocumentAttributes<'input>
        = {
            crate::document_attribute::normalize_toc_attributes(Rc::make_mut(&mut state.document_attributes));
            DocumentAttributes::clone(&state.document_attributes)
        }

        rule prepare_manpage_front_matter()
        = manpage_name_section_required() section:&manpage_name_section() {
            prepare_manpage_name_attributes(state, Some(section));
        }
        / {
            prepare_manpage_name_attributes(state, None);
        }

        rule manpage_name_section_required()
        = {?
            if is_manpage_doctype(&state.document_attributes)
                && !(state.document_attributes.text("manname").is_some()
                    && state.document_attributes.text("manpurpose").is_some())
            {
                Ok(())
            } else {
                Err("manpage name section is not required")
            }
        }

        // The first manpage section supplies header attributes. Inspect it without
        // consuming it so the regular block grammar still builds the AST.
        rule manpage_name_section() -> ManpageNameSection<'input>
        = eol()*
          metadata_attributes:(attribute:manpage_name_metadata() eol()* { attribute })*
          title:manpage_level_one_title()
          eol()*
          lines:manpage_name_body_line()+
        {?
            derive_name_section_attrs(lines)
                .map(|attributes| ManpageNameSection {
                    title,
                    attributes,
                    metadata_attributes: metadata_attributes.into_iter().flatten().collect(),
                })
                .ok_or("non-conforming manpage name section body")
        }

        rule manpage_level_one_title() -> &'input str
        = level:section_level(0, None) whitespace()+ title:$([^'\n']+) (eol() / ![_]) {?
            (level.1 == 1)
                .then_some(title.trim())
                .ok_or("not a level-one manpage name section")
        }
        / title:setext_title_text() eol()
          level:setext_section_level(title.trim().chars().count(), None) {?
            (level == 1 && !title_looks_like_description_list(title))
                .then_some(title.trim())
                .ok_or("not a level-one Setext manpage name section")
        }

        rule manpage_name_metadata() -> Option<AttributeDeclaration<'input>>
        = manpage_comment_block() { None }
        / "//" !"/" [^'\n']* (eol() / ![_]) { None }
        / "[[" (!"]]" [^'\n'])+ "]]" (eol() / ![_]) { None }
        / block_attribute_start()
          open_square_bracket() attribute_list_content(BlockAttributeMode::Block) (eol() / ![_]) { None }
        / "." ![' ' | '\t' | '\n' | '\r' | '.'] [^'\n']* (eol() / ![_]) { None }
        / attribute:document_attribute_match() (eol() / ![_]) { Some(attribute) }

        rule manpage_comment_block()
        = delimiter:$(['/']*<4,>) eol()
          (!manpage_comment_delimiter(delimiter) [^'\n']* eol())*
          manpage_comment_delimiter(delimiter)

        rule manpage_comment_delimiter(delimiter: &str)
        = candidate:$(['/']*<4,>) (eol() / ![_]) {?
            (candidate == delimiter)
                .then_some(())
                .ok_or("not the matching comment delimiter")
        }

        rule manpage_name_body_line() -> Option<&'input str>
        = "//" !"/" [^'\n']* (eol() / ![_]) { None }
        / line:$([^'\n']+) (eol() / ![_]) { Some(line) }

        // After the document title, the first empty line ends the header in
        // both modes. Before the title, Asciidoctor mode ignores empty lines
        // between attribute entries, comments and block metadata, so a document
        // without a title keeps every leading attribute entry in its header.
        // Draft mode follows the language docs: the first empty line ends the
        // header, with or without a title. Empty lines inside an atomic comment
        // block do not end the surrounding header.
        pub(in crate::grammar::document) rule header() -> Result<ParsedHeader<'input>, Error>
            = start:position!()
            leading_end:header_leading_lines()
            metadata:header_metadata()
            titled:(
                title_authors:title_authors()
                (eol() (document_attribute() / header_comment()))*
                end:position!()
                { (title_authors, end) }
            )?
            (eol()*<,2> / ![_])
        {
            if let Some((((title_start, title_text), (authors, revision)), end)) = titled {
                let mut metadata = metadata?;
                let has_id = Section::explicit_id(&metadata).is_some();
                let (title, subtitle, trailing_anchor) = document_title_parts(state, title_start, title_text, has_id)?;
                metadata.anchors.extend(trailing_anchor);
                crate::grammar::reference_text::resolve_metadata(state, &mut metadata)?;
                let location = header_span(state, start, end);
                let mut header = Header {
                    metadata,
                    title,
                    subtitle,
                    authors,
                    revision,
                    location,
                };

                derive_author_attrs(
                    state.arena,
                    &mut header,
                    Rc::make_mut(&mut state.document_attributes),
                    &state.options,
                );

                // Derive manpage attributes from header if doctype=manpage
                // This must happen during parsing so {mantitle} etc. work in body
                if is_manpage_doctype(&state.document_attributes) {
                    let warning = derive_manpage_header_attrs(
                        Some(&header),
                        Rc::make_mut(&mut state.document_attributes),
                        state.options.strict,
                        state.current_file.as_deref().map(std::path::PathBuf::as_path),
                    )?;
                    if let Some(warning) = warning {
                        state.add_warning(Warning::new(
                            warning,
                            Some(state.create_error_source_location(header.location.clone())),
                        ));
                    }
                }

                Ok(ParsedHeader { header: Some(header), untitled_location: None })
            } else {
                tracing::debug!("No document title found in the document header.");
                let untitled_location = leading_end.map(|end| header_span(state, start, end));
                Ok(ParsedHeader { header: None, untitled_location })
            }
        }

        // Attribute entries and comments at the start of the header. Returns the
        // end of the last line. Trailing empty lines are left for the caller.
        rule header_leading_lines() -> Option<usize>
        = ends:(header_leading_gap() end:header_leading_line() { end })* { ends.last().copied() }

        rule header_leading_gap()
        = (asciidoctor_mode() eol()*)?

        // Once an attribute entry matches, the rest of the line always matches,
        // so backtracking cannot apply the entry twice.
        rule header_leading_line() -> usize
        = (document_attribute() / header_comment()) end:position!() (eol() / ![_]) { end }

        rule asciidoctor_mode()
        = {?
            (state.options.compatibility_mode == crate::CompatibilityMode::Asciidoctor)
                .then_some(())
                .ok_or("draft mode")
        }

        /// Parse block metadata lines (anchors and attributes) that can appear before a document title.
        /// Only consumes metadata if followed by a document title to avoid stealing attributes
        /// meant for the first block when there's no document title.
        ///
        /// Attribute entries and comments can sit between those lines in either
        /// order. Asciidoctor mode also accepts empty lines between them; in Draft
        /// mode an empty line ends the header. A side-effect-free lookahead checks
        /// for the title first, so attribute entries are applied once, in source
        /// order, and only as header attributes.
        rule header_metadata() -> Result<BlockMetadata<'input>, Error>
            = &(header_metadata_line_match()* document_title_match())
              lines:(
                  header_empty_line() { None }
                  / document_attribute() (eol() / ![_]) { None }
                  / header_comment() (eol() / ![_]) { None }
                  / line:header_metadata_line() { Some(line) }
              )*
              &document_title_match()
            {
                merge_header_metadata(state, lines.into_iter().flatten())
            }
            / { Ok(BlockMetadata::default()) }

        rule header_metadata_line() -> AttributeOrAnchorLine<'input>
            = anchor:anchor(0) { AttributeOrAnchorLine::Anchor(anchor) }
            / attr:attributes_line() { AttributeOrAnchorLine::Attributes(Box::new(attr)) }

        rule header_empty_line()
            = asciidoctor_mode() eol()

        // Match a line above the document title without applying it.
        rule header_metadata_line_match()
            = header_empty_line()
            / document_attribute_match() (eol() / ![_])
            / comment() (eol() / ![_])
            / comment_block_match() (eol() / ![_])
            / attribute_or_anchor_line_match()

        rule title_authors() -> ((usize, &'input str), (Vec<Author<'input>>, Option<RevisionLocations>))
        // Attribute entries and comments keep each metadata slot open. Consume
        // them outside optional metadata parsing so backtracking cannot assign
        // an entry twice; a blank line still ends the header.
        = title:document_title_raw()
          (eol() (document_attribute() / header_comment()))*
          authors:(eol() authors:authors_and_revision() { authors })?
          &(eol() / ![_])
        {
            tracing::debug!("Found title and authors in the document header.");
            (title, authors.unwrap_or_default())
        }

        pub(in crate::grammar::document) rule document_title(has_id: bool) -> DocumentTitleParts<'input>
        = raw:document_title_raw() {?
            document_title_parts(state, raw.0, raw.1, has_id).map_err(|_| "could not process document title")
        }

        // The title's start and text, its inlines not yet processed: the header
        // processes them once its attribute entries are read, as asciidoctor
        // converts the doctitle after the header (`= {product}` then
        // `:product: Glyph` reads "Glyph").
        rule document_title_raw() -> (usize, &'input str)
        = document_title_atx()
        / document_title_setext()

        // Match a document title without processing its inlines. Lookaheads use
        // this because inline processing registers footnotes and other state.
        rule document_title_match()
        = &atx_heading_prefix() document_title_token() whitespace()
        / title:setext_title_text() eol() underline:$("="+) &(eol() / ![_]) {?
            setext::is_document_title(state.options.compatibility_mode, title, underline)
                .then_some(())
                .ok_or("not a setext document title")
        }

        /// ATX-style document title: `= Title` or `# Title`
        rule document_title_atx() -> (usize, &'input str)
        = &atx_heading_prefix() token:$(document_title_token()) whitespace()+ start:position!() title:$([^'\n']*)
        {
            tracing::debug!("Processing ATX document title");
            (start, &title[..without_closing_marker(title, token)])
        }

        rule setext_title_text() -> &'input str
        = !block_metadata_line_match() title:$([^'\n']+) {?
            setext::is_title(title).then_some(title).ok_or("not a Setext title")
        }

        /// Setext-style document title: Title underlined with `=` characters
        ///
        /// ```text
        /// Document Title
        /// ==============
        /// ```
        ///
        /// The underline must be within one character of the title width.
        /// Recognized in Asciidoctor mode.
        rule document_title_setext() -> (usize, &'input str)
        = title:setext_title_text() eol()
          underline:$("="+) &(eol() / ![_])
        {?
            if !setext::is_document_title(state.options.compatibility_mode, title, underline) {
                return Err("not a setext document title");
            }
            tracing::debug!("Processing setext document title");
            Ok((span_start, title))
        }

        rule document_title_token() = "=" / "#"

        rule authors_and_revision() -> (Vec<Author<'input>>, Option<RevisionLocations>)
        = authors:author_line()
          (eol() (document_attribute() / header_comment()))*
          revision:(eol() revision:revision_pre_substitution() { revision })?
          { (authors, revision.flatten()) }

        rule author_line() -> Vec<Author<'input>>
            = !document_attribute_match() !comment()
              start:position!() author_line:$([^'\n']+) end:position!() {?
                let substituted_cow = apply_attribute_substitutions(author_line.trim(), HEADER, &state.document_attributes);
                // Intern any owned substitution result so the downstream
                // `authors()` parse can yield `Author<'input>` that outlives
                // this action block.
                let substituted: &'input str = match substituted_cow {
                    Cow::Borrowed(s) => s,
                    Cow::Owned(s) => state.intern_str(&s),
                };
                tracing::debug!("Processing author line with substitution");

                let mut temp_state =
                    ParserState::for_inline_parsing(substituted, state, state.inline_ctx);

                // `asciidoctor` always consumes the line after the title as the author
                // line; when it doesn't parse as structured "firstname [middle] [last]
                // [<email>]" authors (e.g. it contains parentheses, commas, or an
                // "Author:" prefix), the whole line becomes a single author's full name.
                let authors = if let Ok(authors) = document_parser::authors(substituted, &mut temp_state) {
                    tracing::debug!("Parsed authors from line");
                    authors
                } else {
                    tracing::debug!("Author line did not parse structurally; using whole line as a single author");
                    let location = state.create_error_source_location(state.create_location(start, end));
                    state.add_warning(Warning::new(
                        WarningKind::NonStandardAuthorLine { line: substituted.to_string() },
                        Some(location),
                    ));
                    vec![Author::new(state.arena, substituted, None, None)]
                };
                // Following entries may reference author metadata, but cannot
                // change the substitutions already applied to this author line.
                register_author_attrs(&authors, Rc::make_mut(&mut state.document_attributes));
                Ok(authors)
            }

        pub(in crate::grammar::document) rule authors() -> Vec<Author<'input>>
            = authors:(author() ++ (";" whitespace()*)) {
                authors
            }

        /// Parse an author in various formats:
        /// - "First Middle Last <email>"
        /// - "First Last <email>"
        /// - "First <email>"
        /// - "First Last"
        pub(in crate::grammar::document) rule author() -> Author<'input>
            = name:author_name() email:author_email()? {
                let mut author = name;
                if let Some(email_addr) = email {
                    author.email = Some(email_addr);
                }
                author
            }

        /// Parse author name in format: "First [Middle] Last" or just "First"
        rule author_name() -> Author<'input>
        = first:name_part() whitespace()+ middle:name_part() whitespace()+ last:$(name_part() ++ whitespace()) {
            Author::new(state.arena, first, Some(middle), Some(last))
        }
        / first:name_part() whitespace()+ last:name_part() {
            Author::new(state.arena, first, None, Some(last))
        }
        / first:name_part() {
            Author::new(state.arena, first, None, None)
        }

        /// Parse email address in format: " <email@domain>"
        rule author_email() -> &'input str
            = whitespace()* "<" email:$([^'>']*) ">" { email }

        rule name_part() -> &'input str
            = name:$([c if c.is_alphanumeric() || c == '.' || c == '-' || c == '\'']+ ("_" [c if c.is_alphanumeric() || c == '.' || c == '-' || c == '\'']+)*) {
                name
            }

        pub(in crate::grammar::document) rule revision() -> RevisionSpans
            = "v"? number_start:position!() number:$(digits() ++ ".") date:revision_date()? remark:revision_remark()? {
                let revision_info = RevisionInfo {
                    number: Cow::Owned(number.to_string()),
                    date: date.map(|(_, d)| Cow::Owned(d.to_string())),
                    remark: remark.map(|(_, r)| Cow::Owned(r.to_string())),
                };
                if revision_info.number.is_empty() {
                    return RevisionSpans::default();
                }
                let span = |start: usize, text: &str| (start, start + text.len());
                let revision_location = state.create_block_location(span_start, span_end, 0);
                let ignored: IgnoredRevisionFields = {
                    let document_attributes = Rc::make_mut(&mut state.document_attributes);
                    process_revision_info(revision_info, document_attributes)
                };
                if ignored.number {
                    state.add_generic_warning_at(
                        "Revision number found in revision line but ignoring due to being set through attribute entries.".to_string(),
                        revision_location.clone(),
                    );
                }
                if ignored.date {
                    state.add_generic_warning_at(
                        "Revision date found in revision line but ignoring due to being set through attribute entries.".to_string(),
                        revision_location.clone(),
                    );
                }
                if ignored.remark {
                    state.add_generic_warning_at(
                        "Revision remark found in revision line but ignoring due to being set through attribute entries.".to_string(),
                        revision_location,
                    );
                }
                RevisionSpans {
                    number: (!ignored.number).then(|| span(number_start, number)),
                    date: date.filter(|_| !ignored.date).map(|(start, d)| span(start, d)),
                    remark: remark.filter(|_| !ignored.remark).map(|(start, r)| span(start, r)),
                }
            }

        /// Parse revision line with attribute reference support
        rule revision_pre_substitution() -> Option<RevisionLocations>
            = line_start:position!() rev_line:$([^'\n']+) {?
                let substituted_cow = apply_attribute_substitutions(rev_line.trim(), HEADER, &state.document_attributes);
                // The fields sit in the source only when no reference was replaced.
                let written_at = matches!(substituted_cow, Cow::Borrowed(_))
                    .then(|| line_start + rev_line.len() - rev_line.trim_start().len());
                let substituted: &'input str = match substituted_cow {
                    Cow::Borrowed(s) => s,
                    Cow::Owned(s) => state.intern_str(&s),
                };
                tracing::debug!("Processing revision line with substitution");

                let mut temp_state =
                    ParserState::for_inline_parsing(substituted, state, state.inline_ctx);

                match document_parser::revision(substituted, &mut temp_state) {
                    Ok(spans) => {
                        for key in ["revnumber", "revdate", "revremark"] {
                            if let Some(value) = temp_state.document_attributes.text(key) {
                                let value = state.intern_str(value);
                                Rc::make_mut(&mut state.document_attributes)
                                    .insert_text(key.into(), value.into());
                            }
                        }
                        tracing::debug!("Parsed revision from line");
                        Ok(written_at.map(|base| {
                            let at = |span: Option<(usize, usize)>| span.map(|(s, e)| state.create_block_location(s, e, base));
                            RevisionLocations { number: at(spans.number), date: at(spans.date), remark: at(spans.remark) }
                        }))
                    }
                    Err(_) => Err("line did not parse as revision")
                }
            }

        rule revision_date() -> (usize, &'input str)
            = ", " start:position!() date:$([^ (':'|'\n')]+) {
                (start, date)
            }

        rule revision_remark() -> (usize, &'input str)
            = ": " start:position!() remark:$([^'\n']+) {
                (start, remark)
            }

        rule document_attribute() -> ()
        = start:position!() att:document_attribute_match() end:position!() (&eol() / ![_])
        {
            tracing::debug!("Found document attribute in the document header");
            let location = state.create_block_location(start, end, 0);
            state.apply_document_attribute(&att, true, location);
        }

        pub(in crate::grammar::document) rule blocks(offset: usize, parent_section_level: Option<SectionLevel>, direct_parent_section_kind: Option<SectionKind>) -> Result<Vec<Block<'input>>, Error>
        = blocks:(!trailing_block_metadata_match() block:block(offset, parent_section_level, direct_parent_section_kind) { block })*
          trailing:trailing_block_metadata(offset)?
        {
            let mut blocks = blocks.into_iter().collect::<Result<Vec<_>, Error>>()?;
            if let Some(trailing) = trailing {
                blocks.extend(trailing?);
            }
            Ok(order_block_prefix_events(blocks))
        }

        pub(in crate::grammar::document) rule compound_blocks(offset: usize, parent_section_level: Option<SectionLevel>) -> Result<Vec<Block<'input>>, Error>
        = content:blocks(offset, parent_section_level, None) eol()* { content }

        rule trailing_block_metadata_match()
        = eol()* &("[" / ".") block_metadata_sequence_match() ![_]

        // Unused block metadata has no inline effects, and its attribute entries
        // none either (`body_attribute_block`).
        rule trailing_block_metadata(offset: usize) -> Result<Vec<Block<'input>>, Error>
        = &trailing_block_metadata_match() eol()* events:(
            event:body_attribute_block(offset) (eol() / ![_]) eol()* { Some(event) }
            / comment:metadata_comment_line(offset) eol()* { Some(metadata_comment(state, comment)) }
            / (attribute_or_anchor_line_match() / title_line_match()) eol()* { None }
        )+
        {
            events.into_iter().flatten().collect()
        }

        pub(in crate::grammar::document) rule nested_document_blocks(offset: usize, initial_attributes: &mut Vec<(crate::AttributeName<'input>, crate::DocumentAttributeAssignment<'input>)>) -> Result<Vec<Block<'input>>, Error>
        = eol()*
          header:(
              comment:comment_line_block(offset) eol()* { comment }
              / attribute:document_attribute_block(offset) (eol() / ![_]) { attribute }
          )*
          header:normalize_nested_header(header, initial_attributes)
          body:blocks(offset, None, None)
        {
            let mut blocks = header?;
            blocks.extend(body?);
            Ok(order_block_prefix_events(blocks))
        }

        rule normalize_nested_header(header: Vec<Result<Block<'input>, Error>>, initial_attributes: &mut Vec<(crate::AttributeName<'input>, crate::DocumentAttributeAssignment<'input>)>) -> Result<Vec<Block<'input>>, Error>
        = { crate::grammar::document::table::normalize_nested_header(state, header, initial_attributes) }

        /// Parse normal cell paragraphs; only blank lines separate them.
        pub(in crate::grammar::document) rule blocks_for_table_cell(offset: usize, prepared: &mut Option<Vec<PreparedTableCellParagraph<'input>>>) -> Result<Vec<Block<'input>>, Error>
        = eol()*
        blocks:(
            start:position!()
            content:$(([^'\n']+ / !(eol()*<2,> / eol()* ![_]) [_])+)
            end:position!()
            eol()*
            {
                // Block-looking lines are text in normal cells. Keep inline
                // substitutions without invoking document block recognition.
                if let Some(prepared) = prepared.as_mut() {
                    let mut paragraph = PreparedTableCellParagraph::new(state, start, end, offset, content)?;
                    let parsed = paragraph.parse(state);
                    if let Ok(block) = &parsed {
                        paragraph.retain_simple_content(block);
                    }
                    prepared.push(paragraph);
                    parsed
                } else {
                    let (content, _) = process_inlines(
                        state,
                        &BlockParsingMetadata::new(state.options.compatibility_mode),
                        start,
                        end,
                        offset,
                        content,
                    )?;
                    Ok(Block::Paragraph(Paragraph::new(
                        content,
                        state.create_block_location(start, end, offset),
                    )))
                }
            }
        )*
        {
            blocks.into_iter().collect()
        }

        rule block(offset: usize, parent_section_level: Option<SectionLevel>, direct_parent_section_kind: Option<SectionKind>) -> Result<Block<'input>, Error>
        = eol()*
        // First check: if we're at a same-or-higher-level section, fail the entire block
        // This prevents section content from consuming sibling/parent sections as paragraphs
        !same_or_higher_level_section(offset, parent_section_level)
        block:(
            comment_line_block(offset) /
            body_attribute_block(offset) /
            // A discrete heading is introduced by an attribute line (`[discrete]`,
            // `[#id,discrete]`, `[float]`, …), which an anchor or a dot-title may
            // precede, so only attempt it when the block starts with `[` or `.`. The
            // rule itself backtracks to `section`/`block_generic` when the metadata
            // isn't a discrete marker.
            &("[" / ".") dh:discrete_header(offset) { dh } /
            section:section(offset, parent_section_level, direct_parent_section_kind) { section } /
            // Legacy underlined sections are recognized in Asciidoctor mode.
            section_setext:section_setext(offset, parent_section_level, direct_parent_section_kind) { section_setext } /
            block_generic(offset, parent_section_level)
        )
        { block }

        /// Single-line comment that becomes a block in the AST.
        /// Line comments begin with `//` (but not `///` or `////` which are block comment delimiters).
        rule comment_line_block(offset: usize) -> Result<Block<'input>, Error>
        = "//" !("/") content:$([^'\n']*) end:position!() (eol() / ![_])
        {
            Ok(Block::Comment(Comment {
                kind: CommentKind::Line,
                prefix_events: None,
                content,
                location: state.create_block_location(span_start, end, offset),
            }))
        }

        rule leading_comment() -> Result<Block<'input>, Error>
        = comment_line_block(0)
        / start:position!() block:comment_block(start, 0, &BlockParsingMetadata::new(state.options.compatibility_mode)) { block }

        // Consume a block comment as a whole so its contents cannot become
        // header attributes, author information or revision information.
        rule header_comment()
        = comment()
        / start:position!() result:comment_block(start, 0, &BlockParsingMetadata::new(state.options.compatibility_mode)) {?
            result.map(|_| ()).map_err(|_| "invalid header comment")
        }

        /// Like `comment_line_block` but leaves the trailing newline unconsumed
        /// (lookahead instead of consume). Used in list continuations so that a
        /// `+` continuation following the comment can still match, since
        /// continuation markers expect a leading newline before the `+`.
        rule comment_line_block_keep_eol(offset: usize) -> Result<Block<'input>, Error>
        = "//" !("/") content:$([^'\n']*) end:position!() &(eol() / ![_])
        {
            Ok(Block::Comment(Comment {
                kind: CommentKind::Line,
                prefix_events: None,
                content,
                location: state.create_block_location(span_start, end, offset),
            }))
        }

        // Check if the upcoming content is a section at same or higher level (which
        // should not be parsed as content)
        //
        // This rule skips optional metadata (anchors, attributes, etc.) before checking
        // the section level, so that `[[anchor]]\n== Section` is correctly identified as
        // a sibling section.
        //
        // Checks both ATX-style (= or #) and setext-style (underlined) sections.
        rule same_or_higher_level_section(offset: usize, parent_section_level: Option<SectionLevel>) -> ()
        // Standalone document attributes must take effect before comparing section levels.
        = check_section_blocks() !document_attribute_match()
          (metadata:$(block_metadata_sequence_match())
          {? if metadata_marks_discrete_heading(metadata, state) { Err("discrete heading belongs to its parent") } else { Ok(()) } })
          (
            // ATX-style section check - require whitespace after marker to avoid matching
            // description list items like `#term::` as sections
            level:section_level(offset, parent_section_level) &whitespace()
            {?
                if let Some(parent_level) = parent_section_level {
                    let upcoming_level = level.1 + 1; // Convert to 1-based
                    if upcoming_level <= parent_level {
                        Ok(()) // This IS a same or higher level section
                    } else {
                        Err("not a same or higher level section")
                    }
                } else {
                    Err("no parent section level to compare")
                }
            }
            /
            // Setext-style section check (title followed by underline)
            &setext_section_lookahead(parent_section_level)
          )

        /// Lookahead rule to detect setext sections at same or higher level.
        /// Used by same_or_higher_level_section to properly terminate sections.
        /// Excludes description list items (e.g., `term:: content`) which would otherwise
        /// match as setext titles.
        rule setext_section_lookahead(parent_section_level: Option<SectionLevel>) -> ()
        = title:setext_title_text() eol() underline:$(['-' | '~' | '^' | '+']+) &(eol() / ![_])
        {?
            if title_looks_like_description_list(title) {
                return Err("title looks like a description list item");
            }
            if !setext::is_enabled(state.options.compatibility_mode) {
                return Err("setext mode not enabled");
            }

            let title_width = title.trim().chars().count();
            let underline_width = underline.chars().count();
            if !setext::width_ok(title_width, underline_width) {
                return Err("underline width out of tolerance");
            }

            let underline_char = underline.chars().next().ok_or("empty underline")?;
            let level = setext::char_to_level(underline_char).ok_or("invalid setext char")?;

            // Level 0 (=) is document title, not section — unless doctype is book (parts)
            if level == 0 && !is_book_doctype(&state.document_attributes) {
                return Err("not a section, seems like you're trying to define a document title");
            }

            if let Some(parent_level) = parent_section_level {
                if level < parent_level {
                    Ok(()) // This IS a same or higher level setext section
                } else {
                    Err("not a same or higher level section")
                }
            } else {
                Err("no parent section level to compare")
            }
        }

        // Marker-only lines can close inline formatting. Require a separator and
        // title before recognizing a heading or consuming heading metadata.
        rule atx_heading_prefix()
        = ("=" / "#")*<1,6> whitespace()+ !eol() &[_]

        // Recognize heading syntax before metadata can register title macros.
        rule atx_heading_match()
        = block_metadata_sequence_match() atx_heading_prefix()

        rule discrete_header(offset: usize) -> Result<Block<'input>, Error>
        = &atx_heading_match() block_metadata:(bm:heading_metadata(offset, None) {?
            let bm = bm.map_err(|_| {
                tracing::error!("error parsing block metadata in discrete_header");
                "block metadata parse error"
            })?;
            // Backtrack to the regular `section` rule unless the attribute line
            // actually marks this as a discrete heading; a discrete heading is
            // exempt from section-level sequencing, so it must not reach `section`.
            if !bm.discrete {
                return Err("not a discrete heading");
            }
            Ok(bm)
        })
        section_level:section_level(offset, None) whitespace()+
        title_start:position!() title:section_title(offset, &block_metadata, section_level.0) title_end:position!() &(eol()*<1,2> / ![_])
        {
            let (title, _, trailing_anchor) = title?;
            let mut metadata = block_metadata.metadata;
            metadata.anchors.extend(trailing_anchor);
            tracing::debug!(title_start, title_end, "parsing discrete header block");

            let level = section_level.1;
            // `span_end` lands at title_end here because the trailing `&(...)` is a
            // zero-width lookahead.
            let location = state.create_block_location(span_start, span_end, offset);

            // `float` is a legacy alias for the `discrete` block style (older
            // AsciiDoc called these "floating titles"). Surface its use so authors
            // can migrate to `discrete`. Only the style form reaches this rule.
            if metadata.style == Some("float") {
                let warning_location = state.create_error_source_location(
                    state.create_block_location(span_start, span_end, offset),
                );
                state.add_warning(Warning::new(
                    WarningKind::LegacyFloatDiscreteHeading,
                    Some(warning_location),
                ));
            }

            Ok(Block::DiscreteHeading(DiscreteHeading {
                metadata,
                title,
                level,
                id: None,
                id_generated: false,
                id_settings: crate::model::section::SectionIdSettings::from_attributes(state.arena, &state.document_attributes),
                location,
            }))
        }

        // A body attribute entry takes effect as the next block converts
        // (`Document#playback_attributes`). With no block after it in a
        // compound block's lines, Asciidoctor attaches it to none, and the
        // content after that block never sees it. (At the end of the document
        // nothing follows to see it.)
        rule body_attribute_block(offset: usize) -> Result<Block<'input>, Error>
        = ({? (state.block_context == BlockContext::Compound && state.options.compatibility_mode == crate::CompatibilityMode::Asciidoctor).then_some(()).ok_or("document content") })
          &(document_attribute_match() (eol() / ![_]) no_block_follows()) att:document_attribute_match()
        {
            let location = state.create_block_location(span_start, span_end, offset);
            Ok(Block::DocumentAttribute(DocumentAttribute::rejected(Cow::Borrowed(att.name), location)))
        }
        / document_attribute_block(offset)

        rule no_block_follows()
        = (eol() / block_metadata_line_match() / metadata_comment_match() (eol() / ![_]))* ![_]

        rule document_attribute_block(offset: usize) -> Result<Block<'input>, Error>
        = att:document_attribute_match()
        {
            let name = Cow::Borrowed(att.name);
            let location = state.create_block_location(span_start, span_end, offset);
            let event = state
                .apply_document_attribute(&att, false, location.clone())
                .unwrap_or_else(|| DocumentAttribute::rejected(name, location));
            Ok(Block::DocumentAttribute(event))
        }

        rule section(offset: usize, parent_section_level: Option<SectionLevel>, direct_parent_section_kind: Option<SectionKind>) -> Result<Block<'input>, Error>
        = check_section_blocks() &atx_heading_match()
        block_metadata:(bm:section_metadata(offset, parent_section_level) {?
            let bm = bm.map_err(|_| {
                tracing::error!("error parsing block metadata in section");
                "block metadata parse error"
            })?;
            // A heading the metadata marks discrete is `discrete_header`'s, never a section.
            if bm.discrete {
                return Err("a discrete heading is no section");
            }
            Ok(bm)
        })
        section_level_start:position!()
        section_level:section_level(offset, parent_section_level)
        section_level_end:position!()
        whitespace()+
        title_start:position!()
        section_header:(title:section_title(offset, &block_metadata, section_level.0) title_end:position!() &(eol()*<1,2> / ![_]) {
            let (title, natural_title, trailing_anchor) = title?;
            let location = state.create_block_location(section_level_start, title_end, offset);
            let mut header = register_section_header(
                state,
                &block_metadata,
                title,
                natural_title,
                section_level.1,
                location,
                direct_parent_section_kind,
            );
            header.trailing_anchor = trailing_anchor;
            Ok::<ParsedSectionHeader<'input>, Error>(header)
        })
        content:section_content(offset, Some(expected_child_level(
            section_level.1,
            SectionKind::from_style(block_metadata.metadata.style),
        )), Some(SectionKind::from_style(block_metadata.metadata.style)))?
        {
            let ParsedSectionHeader { title, numbering, reference_text, id_settings, heading_location, trailing_anchor } = section_header?;
            tracing::debug!(offset, "parsing section block");

            if let Some(parent_level) = parent_section_level {
                if section_level.1 < parent_level || section_level.1 > 5 {
                    return Err(Error::NestedSectionLevelMismatch(
                        Box::new(state.create_error_source_location(state.create_block_location(section_level_start, section_level_end, offset))),
                        section_level.1+1,
                        parent_level + 1,
                    ));
                }
                // A section that skips a level (deeper than one below its parent)
                // is "out of sequence". asciidoctor warns but still renders it at
                // its literal level rather than aborting, so we do the same.
                if section_level.1 > parent_level {
                    let location = state.create_error_source_location(
                        state.create_block_location(section_level_start, section_level_end, offset),
                    );
                    state.add_warning(Warning::new(
                        WarningKind::SectionLevelOutOfSequence {
                            expected: parent_level,
                            got: section_level.1,
                        },
                        Some(location),
                    ));
                }
            }

            let blocks = content.unwrap_or(Ok(Vec::new()))?;
            let location = state.create_block_location(span_start, state.end_before_line_breaks(span_start, span_end, offset), offset);
            let carried_title = block_metadata.title;

            // Classify the section before the post-parse numbering pass applies
            // special-section rules to the complete section tree.
            let kind = SectionKind::from_style(block_metadata.metadata.style);
            let level = section_level.1;
            let mut metadata = block_metadata.metadata;
            metadata.anchors.extend(trailing_anchor);

            let mut section = Section::parsed(
                metadata,
                title,
                level,
                blocks,
                kind,
                numbering,
                location,
            );
            section.reference_text = Some(reference_text);
            section.id_settings = id_settings;
            section.heading_location = heading_location;
            section.carried_title = carried_title;
            Ok(Block::Section(section))
        }

        /// Setext-style section header: Title underlined with `-`, `~`, `^`, or `+`
        ///
        /// ```text
        /// Section Title
        /// -------------
        /// ```
        ///
        /// The underline character determines the section level:
        /// - `-` = Level 1
        /// - `~` = Level 2
        /// - `^` = Level 3
        /// - `+` = Level 4
        ///
        /// The underline must be within one character of the title width.
        /// Recognized in Asciidoctor mode.
        /// Parse a setext section level from the underline character.
        /// Returns the level (1-4) corresponding to -, ~, ^, +
        rule setext_section_level(title_width: usize, parent_section_level: Option<SectionLevel>) -> u8
        = underline:$(['-' | '~' | '^' | '+']+) &(eol() / ![_])
        {?
            if !setext::is_enabled(state.options.compatibility_mode) {
                return Err("setext mode not enabled");
            }

            let underline_width = underline.chars().count();

            if !setext::width_ok(title_width, underline_width) {
                return Err("underline width out of tolerance");
            }

            let underline_char = underline.chars().next().ok_or("empty underline")?;
            let level = setext::char_to_level(underline_char).ok_or("invalid setext underline character")?;

            // Document title (level 0) uses =, not allowed here — unless doctype is book (parts)
            if level == 0 && !is_book_doctype(&state.document_attributes) {
                return Err("use = underline for document title, not section");
            }

            if let Some(parent_level) = parent_section_level
                && (level < parent_level || level > parent_level + 1 || level > 5)
            {
                return Err("section level mismatch with parent");
            }

            Ok(level)
        }

        rule setext_section_match(parent_section_level: Option<SectionLevel>)
        = block_metadata_sequence_match()
          title:setext_title_text() eol()
          setext_section_level(title.trim().chars().count(), parent_section_level)

        /// Parse a setext-style section (title followed by underline).
        /// Excludes description list items (e.g., `term:: content`) which would otherwise
        /// match as setext titles.
        rule section_setext(offset: usize, parent_section_level: Option<SectionLevel>, direct_parent_section_kind: Option<SectionKind>) -> Result<Block<'input>, Error>
        = check_section_blocks() !check_line_is_description_list(offset)
        &setext_section_match(parent_section_level)
        block_metadata:(bm:section_metadata(offset, parent_section_level) {?
            bm.map_err(|_| {
                tracing::error!("error parsing block metadata in section_setext");
                "block metadata parse error"
            })
        })
        title_start:position!() title:setext_title_text() title_end:position!() eol()
        setext_level:setext_section_level(title.trim().chars().count(), parent_section_level)
        section_header:({
            let (title, natural_title, trailing_anchor) =
                heading_title(state, &block_metadata, title_start, offset, title, "")?;
            let location = state.create_block_location(title_start, title_end, offset);
            let mut header = register_section_header(
                state,
                &block_metadata,
                title,
                natural_title,
                setext_level,
                location,
                direct_parent_section_kind,
            );
            header.trailing_anchor = trailing_anchor;
            Ok::<ParsedSectionHeader<'input>, Error>(header)
        })
        content:section_content(offset, Some(expected_child_level(
            setext_level,
            SectionKind::from_style(block_metadata.metadata.style),
        )), Some(SectionKind::from_style(block_metadata.metadata.style)))?
        {
            let ParsedSectionHeader { title, numbering, reference_text, id_settings, heading_location, trailing_anchor } = section_header?;
            let blocks = content.unwrap_or(Ok(Vec::new()))?;
            let location = state.create_block_location(span_start, state.end_before_line_breaks(span_start, span_end, offset), offset);
            let carried_title = block_metadata.title;

            // Classify the section by its style (see the ATX section rule).
            let kind = SectionKind::from_style(block_metadata.metadata.style);
            let mut metadata = block_metadata.metadata;
            metadata.anchors.extend(trailing_anchor);

            let mut section = Section::parsed(
                metadata,
                title,
                setext_level,
                blocks,
                kind,
                numbering,
                location,
            );
            section.reference_text = Some(reference_text);
            section.id_settings = id_settings;
            section.heading_location = heading_location;
            section.carried_title = carried_title;
            Ok(Block::Section(section))
        }

        rule block_metadata(offset: usize, parent_section_level: Option<SectionLevel>) -> Result<BlockParsingMetadata<'input>, Error>
        = metadata:block_metadata_with_title(offset, parent_section_level, true, false) { metadata }

        // Headings display their own text, so ignore macros in preceding dot-titles.
        rule heading_metadata(offset: usize, parent_section_level: Option<SectionLevel>) -> Result<BlockParsingMetadata<'input>, Error>
        = metadata:block_metadata_with_title(offset, parent_section_level, false, false) { metadata }

        // In Asciidoctor mode a section's dot-title is the next content block's
        // (`Block::deliver_carried_titles`); Draft mode drops it unparsed.
        rule section_metadata(offset: usize, parent_section_level: Option<SectionLevel>) -> Result<BlockParsingMetadata<'input>, Error>
        = metadata:block_metadata_with_title(offset, parent_section_level, false, state.options.compatibility_mode == crate::CompatibilityMode::Asciidoctor) { metadata }

        rule block_metadata_with_title(offset: usize, parent_section_level: Option<SectionLevel>, parse_title: bool, keep_title: bool) -> Result<BlockParsingMetadata<'input>, Error>
        = meta_start:position!() lines:block_metadata_lines(offset)
          block_start:position!() media_ahead:media_block_ahead() form:attribute_block_form(offset)
        {
            let mut attributes = ParsedAttributeList::default();
            let meta_end = lines.last().map_or(meta_start, |(_, end)| *end);

            for (line, _) in lines {
                match line {
                    BlockMetadataLine::Anchor(value) => push_metadata_anchor(state, &mut attributes.metadata, value)?,
                    BlockMetadataLine::Attributes(raw) => {
                        let parsed = parse_block_attribute_list(
                            state, raw.source, raw.start, BlockAttributeMode::Block, offset,
                        )?;
                        attributes.merge(parsed, state.options.compatibility_mode);
                    },
                    BlockMetadataLine::DocumentAttribute(declaration, location) => {
                        if let Some(event) = state.apply_document_attribute(
                            &declaration,
                            false,
                            location,
                        ) {
                            attributes.metadata.push_prefix_event(Block::DocumentAttribute(event));
                        }
                    },
                    BlockMetadataLine::Comment(comment) => {
                        let comment = metadata_comment(state, comment)?;
                        attributes.metadata.push_prefix_event(comment);
                    },
                    BlockMetadataLine::Title { source, start, end } => {
                        attributes.title = Some(TitleSource::Dot { source, start, end });
                    }
                }
            }
            let media_ahead = media_ahead && !style_overrides_block_form(state.options.compatibility_mode, attributes.metadata.style);
            let media_attributes = if media_ahead {
                media_attributes_ahead(state, block_start, offset).map(|raw| -> Result<_, Error> {
                    let parsed = parse_block_attribute_list(
                        state, raw.source, raw.start, BlockAttributeMode::Macro, offset,
                    )?;
                    attributes.title = parsed.title.or_else(|| attributes.title.take());
                    Ok(Box::new(parsed.metadata))
                }).transpose()?
            } else { None };
            if parse_title {
                if !media_ahead {
                    route_block_positions(state, &mut attributes, form);
                }
            } else {
                attributes.metadata.style = select_heading_style(
                    attributes.metadata.first_source.as_ref().map(|source| source.source),
                    attributes.metadata.style,
                    state.options.compatibility_mode,
                );
            }
            let image_ahead = media_ahead && state.input.get(block_start + offset..).is_some_and(|tail| tail.starts_with("image::"));
            let image_alt = resolve_block_content_attributes(state, &mut attributes, media_attributes.as_deref(), image_ahead)?;
            // Titles use the completed metadata attributes, including entries
            // after the title line. Only the last authored title is displayed.
            let title = if let Some(source) = attributes.title.take().filter(|_| parse_title || keep_title) {
                // A section's title is the next block's and asciidoctor numbers its notes when
                // that block draws it, after the heading's: it takes no number here, and the
                // document numbers it once delivered (`Block::deliver_carried_titles`).
                let carried = (!parse_title).then(|| std::mem::replace(
                    &mut state.footnote_tracker,
                    Rc::new(std::cell::RefCell::new(crate::grammar::state::FootnoteTracker::provisional())),
                ));
                let resolved = resolve_title(state, source, offset);
                if let Some(tracker) = carried {
                    state.footnote_tracker = tracker;
                }
                match resolved {
                    Ok(title) => title,
                    Err(error) => {
                        state.add_warning(crate::Warning::new(
                            crate::WarningKind::ContentRecovery {
                                message: format!("failed to parse block title, skipping: {error:?}").into(),
                            },
                            error.source_location().cloned(),
                        ));
                        Title::default()
                    }
                }
            } else {
                Title::default()
            };
            if meta_start != meta_end {
                attributes.metadata.location = Some(state.create_block_location(meta_start, meta_end, offset));
            }
            let mut result = finish_block_parsing_metadata(
                state,
                attributes,
                title,
                parent_section_level,
            )?;
            result.media_attributes = media_attributes;
            result.image_alt = image_alt;
            Ok(result)
        }

        // Comments carry metadata forward only after an authored metadata line.
        rule block_metadata_lines(offset: usize) -> Vec<(BlockMetadataLine<'input>, usize)>
        = &block_metadata_line_match() lines:(
            line:(block_metadata_line(offset) / comment:metadata_comment_line(offset) { BlockMetadataLine::Comment(comment) })
            end:position!() eol()* { (line, end) }
        )* { lines }
        / { Vec::new() }

        rule block_metadata_line(offset: usize) -> BlockMetadataLine<'input>
        = &['['] anchor:anchor(offset) { BlockMetadataLine::Anchor(anchor) }
        / &['['] attr:raw_attributes_line() { BlockMetadataLine::Attributes(attr) }
        / &[':'] doc_attr:document_attribute_line(offset) { BlockMetadataLine::DocumentAttribute(doc_attr.0, doc_attr.1) }
        / &['.'] title:title_line() { title }

        rule metadata_comment_line(offset: usize) -> MetadataComment<'input>
        = start:position!() "//" !"/" content:$([^'\n']*) end:position!() (eol() / ![_]) {
            MetadataComment::Line(Comment { kind: CommentKind::Line, prefix_events: None, content, location: state.create_block_location(start, end, offset) })
        }
        / start:position!() params:comment_block_params(start, offset) { MetadataComment::Delimited(params) }

        rule block_metadata_sequence_match()
        = (block_metadata_line_match() eol()* (metadata_comment_match() eol()*)*)*

        rule metadata_comment_match()
        = comment_line()
        / comment_block_match()

        // A delimited comment block, through its closing delimiter when present.
        rule comment_block_match()
        = delimiter:comment_delimiter() (eol() / ![_])
          until_block_close(delimiter) (eol() &block_close_delim(delimiter))? block_close_delim(delimiter)?

        // Reuse block-start rules before selected attributes can register inline content.
        rule attribute_block_form(offset: usize) -> AttributeBlockForm<'input>
        = form:&(open:block_open() (eol() / ![_]) { AttributeBlockForm::Delimited(open.0, open.2) }) { form }
        / &((table_delimiter() / comma_table_delimiter() / colon_table_delimiter()) (eol() / ![_])) { AttributeBlockForm::Table }
        / &(media_block_ahead_true() / at_list_item_start() / at_callout_list_item()
            / check_line_is_description_list(offset) / toc_block_ahead() / ("<<<" &eol()*<2,2>) / "> "
            / thematic_break_marker()) { AttributeBlockForm::NonParagraph }
        / { AttributeBlockForm::Paragraph }

        rule media_block_ahead_true()
        = ("image::" / "audio::" / "video::") [^'[' | '\n' | '\r']+ "[" attribute_list_content(BlockAttributeMode::Macro) block_macro_end()

        rule toc_block_ahead()
        = "toc::" "[" attribute_list_content(BlockAttributeMode::Macro) block_macro_end()

        rule block_macro_end()
        = [' ' | '\t' | '\r']* &(eol() / ![_])

        // Match metadata in lookahead without expanding attributes or titles.
        rule block_metadata_line_match()
        = &['['] attribute_or_anchor_line_match()
        / &[':'] document_attribute_match() (eol() / ![_])
        / &['.'] title_line_match()

        rule title_line_match()
        = period() ![' ' | '\t' | '\n' | '\r' | '.'] [^'\n']+ (eol() / ![_])

        rule title_line() -> BlockMetadataLine<'input>
        = period() start:position!() title:$(![' ' | '\t' | '\n' | '\r' | '.'] [^'\n']*) end:position!() eol()
        {
            tracing::debug!(start, end, "Found title line in block metadata");
            BlockMetadataLine::Title { source: title, start, end }
        }

        // A document attribute line in block metadata context
        // This allows document attributes to be set between block attributes and the block content
        // Uses the same parsing logic as document attributes in the header
        rule document_attribute_line(offset: usize) -> (AttributeDeclaration<'input>, Location)
        = start:position!() attr:document_attribute_match() end:position!() eol()
        {
            tracing::debug!("Found document attribute in block metadata");
            (attr, state.create_block_location(start, end, offset))
        }

        rule check_section_blocks()
        = {? (state.block_context == BlockContext::Document).then_some(()).ok_or("section blocks are not allowed in compound content") }

        rule section_level(offset: usize, parent_section_level: Option<SectionLevel>) -> (&'input str, SectionLevel)
        = level:$(("=" / "#")*<1,6>)
        {
            let base_level: SectionLevel = level.len().try_into().unwrap_or(1) - 1;
            let byte_offset = span_start + offset;
            (level, apply_leveloffset(base_level, byte_offset, &state.leveloffset_ranges, &state.document_attributes))
        }

        rule at_line_start(offset: usize)
        = pos:position!()
        {?
            let absolute_pos = pos + offset;
            let at_line_start = absolute_pos == 0 || {
                let prev_byte_pos = absolute_pos.saturating_sub(1);
                state.input.as_bytes().get(prev_byte_pos).is_some_and(|&b| b == b'\n')
            };

            if !at_line_start {
                return Err("expected line start");
            }

            Ok(())
        }

        rule section_title(offset: usize, block_metadata: &BlockParsingMetadata<'input>, marker: &str) -> Result<(Title<'input>, &'input str, Option<Anchor<'input>>), Error>
        = title:$([^'\n']*)
        {
            tracing::debug!(title_start = span_start, title_end = span_end, "Found section title");
            heading_title(state, block_metadata, span_start, offset, title, marker)
        }

        rule section_content(offset: usize, parent_section_level: Option<SectionLevel>, direct_parent_section_kind: Option<SectionKind>) -> Result<Vec<Block<'input>>, Error>
        = blocks(offset, parent_section_level, direct_parent_section_kind) / { Ok(vec![]) }

        rule block_generic(offset: usize, parent_section_level: Option<SectionLevel>) -> Result<Block<'input>, Error>
        = start:position!()
        block_metadata:(bm:block_metadata(offset, parent_section_level) {?
            bm.map_err(|_| {
                tracing::error!("error parsing block metadata in block_generic");
                "block metadata parse error"
            })
        })
        block:(
            delimited_block:delimited_block(start, offset, &block_metadata) { delimited_block }
            / verbatim:styled_verbatim_paragraph(start, offset, &block_metadata) { verbatim }
            / image:image(start, offset, &block_metadata) { image }
            / audio:audio(start, offset, &block_metadata) { audio }
            / video:video(start, offset, &block_metadata) { video }
            / toc:toc(start, offset, &block_metadata) { toc }
            / excessive_list_item(start, offset, &block_metadata)
            / thematic_break:thematic_break(start, offset, &block_metadata) { thematic_break }
            / page_break:page_break(start, offset, &block_metadata) { page_break }
            / list:list(start, offset, &block_metadata) { list }
            / quoted_paragraph:quoted_paragraph(start, offset, &block_metadata) { quoted_paragraph }
            / markdown_blockquote:markdown_blockquote(start, offset, &block_metadata) { markdown_blockquote }
            / paragraph:paragraph(start, offset, &block_metadata) { paragraph }
        ) {
            let mut block = block?;
            assign_block_caption(state, &mut block);
            Ok(block)
        }

        // Block parsing for continuation context - lists inside continuations cannot consume
        // further continuations (those belong to the parent item that started the continuation)
        // Comments before attached content do not consume its continuation.
        // Keep comment-only and next-item cases on the existing path.
        rule block_in_continuation(offset: usize, parent_section_level: Option<SectionLevel>) -> Result<Block<'input>, Error>
        = continuation_comments() !eol() !at_list_item_start()
          !at_callout_list_item() !check_line_is_description_list(offset)
          !("+" whitespace()* (eol() / ![_]))
          block:continuation_block(offset, parent_section_level) { block }
        / block:continuation_block(offset, parent_section_level) { block }

        rule continuation_comments()
        = eol()* (
            comment_line()+
            / delimiter:comment_delimiter() eol()
              until_block_close(delimiter) eol()? block_close_delim(delimiter) eol()?
        )

        rule continuation_block(offset: usize, parent_section_level: Option<SectionLevel>) -> Result<Block<'input>, Error>
        = !trailing_block_metadata_match() start:position!()
        block_metadata:(bm:block_metadata(offset, parent_section_level) {?
            bm.map_err(|_| {
                tracing::error!("error parsing block metadata in block_in_continuation");
                "block metadata parse error"
            })
        })
        block:(
            // A `//` line comment or `////` block comment in a continuation
            // produces a comment node (which renders to nothing), matching
            // asciidoctor. Absorb optional leading blank lines: a trailing `+`
            // leaves a blank-line newline before the comment, while an immediate
            // comment sits directly at the delimiter. Without this, the `+` would
            // backtrack and leak a stray `+` paragraph. Must precede `paragraph`,
            // which would otherwise gobble the `//` line.
            comment:(eol()* comment_start:position!() c:(
                comment_line_block_keep_eol(offset)
                / comment_block(comment_start, offset, &block_metadata)
            ) { c }) { comment }
            / delimited_block:delimited_block(start, offset, &block_metadata) { delimited_block }
            / verbatim:styled_verbatim_paragraph(start, offset, &block_metadata) { verbatim }
            / image:image(start, offset, &block_metadata) { image }
            / audio:audio(start, offset, &block_metadata) { audio }
            / video:video(start, offset, &block_metadata) { video }
            / toc:toc(start, offset, &block_metadata) { toc }
            / excessive_list_item(start, offset, &block_metadata)
            / thematic_break:thematic_break(start, offset, &block_metadata) { thematic_break }
            / page_break:page_break(start, offset, &block_metadata) { page_break }
            / list:list(start, offset, &block_metadata) { list }
            / quoted_paragraph:quoted_paragraph(start, offset, &block_metadata) { quoted_paragraph }
            / markdown_blockquote:markdown_blockquote(start, offset, &block_metadata) { markdown_blockquote }
            / !(eol()+ (whitespace()* ordinary_list_marker() whitespace() / check_line_is_description_list(offset)))
              paragraph:paragraph(start, offset, &block_metadata) { paragraph }
        ) {
            let mut block = block?;
            assign_block_caption(state, &mut block);
            Ok(block)
        }

        rule delimited_block(
            start: usize,
            offset: usize,
            block_metadata: &BlockParsingMetadata<'input>,
        ) -> Result<Block<'input>, Error>
        = generic_delimited_block(start, offset, block_metadata)
        / table_block(start, offset, block_metadata)

        // Every non-table delimited block shares one open/content/optional-close
        // skeleton. `block_open` recognizes which kind a delimiter introduces and
        // `build_delimited_block` constructs the right block — the same split tables
        // use (`*_table_block` rules + `parse_table_block_impl`). The optional close
        // and the `(eol() / ![_])` after the open delimiter let an opener that runs
        // to end of input still produce a block, closed at EOF (asciidoctor's
        // recovery; `build_delimited_block` emits the unterminated warning).
        rule generic_delimited_block(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = open_start:position!() open:block_open() (eol() / ![_])
              content_start:position!()
              source_text:$(until_block_close(open.1) (eol() &block_close_delim(open.1))?) body_end:position!()
              close:(close_start:position!() close_delim:block_close_delim(open.1) { (close_start, close_delim) })?
        {
            let content = if close.is_some() {
                source_text.strip_suffix('\n').unwrap_or(source_text)
            } else {
                source_text
            };
            let content_end = body_end - (source_text.len() - content.len());
            build_delimited_block(state, block_metadata, &DelimitedParams {
                kind: open.0, open_delim: open.1, content, source_text,
                open_start, start, content_start, content_end, end: span_end, offset, close,
            })
        }

        // Recognize a non-table delimited-block opening delimiter, returning its
        // kind, the literal delimiter, and (for a Markdown ``` fence) an optional
        // language. Listing (`-`×4+) is tried before open (`--`) so `----` is a
        // listing, not a too-short open block.
        rule block_open() -> (DelimitedKind, &'input str, Option<&'input str>)
            = d:comment_delimiter()  { (DelimitedKind::Comment, d, None) }
            / d:example_delimiter()  { (DelimitedKind::Example, d, None) }
            / d:listing_delimiter()  { (DelimitedKind::Listing, d, None) }
            / d:literal_delimiter()  { (DelimitedKind::Literal, d, None) }
            / d:open_delimiter()     { (DelimitedKind::Open, d, None) }
            / d:sidebar_delimiter()  { (DelimitedKind::Sidebar, d, None) }
            / d:pass_delimiter()     { (DelimitedKind::Passthrough, d, None) }
            / d:quote_delimiter()    { (DelimitedKind::Quote, d, None) }
            / d:markdown_code_delimiter() lang:markdown_language()? { (DelimitedKind::Listing, d, lang) }

        // Content up to (but not including) a closing delimiter line exactly equal
        // to `expected`, or end of input. Generic over delimiter type; the exact
        // comparison in `block_close_delim` keeps a different-length or
        // different-character run from closing the block.
        rule until_block_close(expected: &str) -> &'input str
            = &block_close_delim(expected) { "" }
            / content:$((!(eol() block_close_delim(expected)) (eol() / [^'\n']+))*) { content }

        // Match a complete delimiter line equal to `expected`.
        // Separate alternatives prevent mixed delimiter characters from matching.
        rule block_close_delim(expected: &str) -> &'input str
            = delim:$("="+ / "/"+ / "-"+ / "."+ / "*"+ / "_"+ / "+"+ / "~"+ / "`"+)
              &(eol() / ![_])
              {? if delim == expected { Ok(delim) } else { Err("delimiter mismatch") } }

        // A `////` comment block specifically. The generic `delimited_block` covers
        // this in normal flow, but a list/description-list continuation needs to
        // match *only* a comment block (to absorb it after a `+`), so this gated
        // entry point reuses the shared skeleton and builder.
        rule comment_block(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = params:comment_block_params(start, offset) { build_delimited_block(state, block_metadata, &params) }

        rule comment_block_params(start: usize, offset: usize) -> DelimitedParams<'input>
            = open_start:position!() open_delim:comment_delimiter() (eol() / ![_])
              content_start:position!()
              source_text:$(until_block_close(open_delim) (eol() &block_close_delim(open_delim))?) body_end:position!()
              close:(close_start:position!() close_delim:block_close_delim(open_delim) { (close_start, close_delim) })?
        {
            let content = if close.is_some() {
                source_text.strip_suffix('\n').unwrap_or(source_text)
            } else {
                source_text
            };
            let content_end = body_end - (source_text.len() - content.len());
            DelimitedParams {
                kind: DelimitedKind::Comment, open_delim, content, source_text,
                open_start, start, content_start, content_end, end: span_end, offset, close,
            }
        }

        // Delimiter recognition rules
        rule comment_delimiter() -> &'input str = delim:$("/"*<4,>) { delim }
        rule example_delimiter() -> &'input str = delim:$("="*<4,>) { delim }
        rule listing_delimiter() -> &'input str = delim:$("-"*<4,>) { delim }
        rule literal_delimiter() -> &'input str = delim:$("."*<4,>) { delim }
        rule open_delimiter() -> &'input str = delim:$("-"*<2,2> / "~"*<4,>) { delim }
        rule sidebar_delimiter() -> &'input str = delim:$("*"*<4,>) { delim }
        rule table_delimiter() -> &'input str = delim:$((['|' | ',' | ':' | '!'] "="*<3,>)) { delim }

        // Opening rules select the table's default separator. Content scanning
        // compares the complete opening token, retaining nested tables with a
        // different delimiter in the outer table's body.
        rule pipe_table_delimiter() -> &'input str = delim:$("|" "="*<3,>) { delim }
        rule excl_table_delimiter() -> &'input str = delim:$("!" "="*<3,>) { delim }
        rule comma_table_delimiter() -> &'input str = delim:$("," "="*<3,>) { delim }
        rule colon_table_delimiter() -> &'input str = delim:$(":" "="*<3,>) { delim }

        rule pass_delimiter() -> &'input str = delim:$("+"*<4,>) { delim }
        rule markdown_code_delimiter() -> &'input str = delim:$("`"*<3,>) { delim }
        rule quote_delimiter() -> &'input str = delim:$("_"*<4,>) { delim }

        // A table closes only on an exact delimiter on its own line.
        // Different lengths and delimiter-looking cell text remain in its body.
        rule matching_table_delimiter(open_delim: &str) -> &'input str
        = delim:table_delimiter() &(eol() / ![_]) {?
            (delim == open_delim).then_some(delim).ok_or("matching table delimiter")
        }

        rule until_table_delimiter(open_delim: &str) -> &'input str
        = content:$((!(eol() matching_table_delimiter(open_delim)) [_])*) { content }

        rule markdown_language() -> &'input str
        = lang:$((['a'..='z'] / ['A'..='Z'] / ['0'..='9'] / "_" / "+" / "-")+) { lang }

        // Table block dispatcher - tries each delimiter-specific variant in order.
        // This enables nested tables: |=== outer can contain !=== inner because
        // each rule only looks for its own closing delimiter.
        //
        // Terminated variants are tried first; unterminated fallbacks only match
        // when an opening delimiter runs to end-of-input without a close.
        rule table_block(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = pipe_table_block(start, offset, block_metadata)
            / excl_table_block(start, offset, block_metadata)
            / comma_table_block(start, offset, block_metadata)
            / colon_table_block(start, offset, block_metadata)
            / unterminated_pipe_table_block(start, offset, block_metadata)
            / unterminated_excl_table_block(start, offset, block_metadata)
            / unterminated_comma_table_block(start, offset, block_metadata)
            / unterminated_colon_table_block(start, offset, block_metadata)

        rule pipe_table_block(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = table_start:position!() open_delim:pipe_table_delimiter() eol()
              content_start:position!() content:until_table_delimiter(open_delim) content_end:position!()
              eol() close_start:position!() close_delim:matching_table_delimiter(open_delim)
        {
            parse_table_block_impl(
                &TableParseParams {
                    start, offset, table_start, content_start, content_end, end: span_end,
                    open_delim, content, default_separator: "|",
                    closing: TableClosing::Terminated { close_delim, close_start },
                },
                state,
                block_metadata,
            )
        }

        rule excl_table_block(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = table_start:position!() open_delim:excl_table_delimiter() eol()
              content_start:position!() content:until_table_delimiter(open_delim) content_end:position!()
              eol() close_start:position!() close_delim:matching_table_delimiter(open_delim)
        {
            parse_table_block_impl(
                &TableParseParams {
                    start, offset, table_start, content_start, content_end, end: span_end,
                    open_delim, content, default_separator: "!",
                    closing: TableClosing::Terminated { close_delim, close_start },
                },
                state,
                block_metadata,
            )
        }

        rule comma_table_block(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = table_start:position!() open_delim:comma_table_delimiter() eol()
              content_start:position!() content:until_table_delimiter(open_delim) content_end:position!()
              eol() close_start:position!() close_delim:matching_table_delimiter(open_delim)
        {
            parse_table_block_impl(
                &TableParseParams {
                    start, offset, table_start, content_start, content_end, end: span_end,
                    open_delim, content, default_separator: ",",
                    closing: TableClosing::Terminated { close_delim, close_start },
                },
                state,
                block_metadata,
            )
        }

        rule colon_table_block(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = table_start:position!() open_delim:colon_table_delimiter() eol()
              content_start:position!() content:until_table_delimiter(open_delim) content_end:position!()
              eol() close_start:position!() close_delim:matching_table_delimiter(open_delim)
        {
            parse_table_block_impl(
                &TableParseParams {
                    start, offset, table_start, content_start, content_end, end: span_end,
                    open_delim, content, default_separator: ":",
                    closing: TableClosing::Terminated { close_delim, close_start },
                },
                state,
                block_metadata,
            )
        }

        // Unterminated table fallbacks: match an opening table delimiter
        // that runs to end-of-input without a closing delimiter. These
        // alternatives are tried only after all terminated variants fail,
        // so a document with a valid close never takes this path. When
        // taken, `parse_table_block_impl` emits an `UnterminatedTable`
        // warning and still produces a table, matching asciidoctor's
        // recovery behavior.
        //
        // The `(eol() / ![_])` after the open delimiter accepts both
        // `|===\n...` and `|===<EOF>`: the preprocessor's `normalize`
        // strips a single trailing newline (mirroring `str::lines`), so a
        // file ending with just `|===\n` reaches the grammar as `|===`.
        rule unterminated_pipe_table_block(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = table_start:position!() open_delim:pipe_table_delimiter() (eol() / ![_])
              content_start:position!() content:until_table_delimiter(open_delim) content_end:position!()
              ![_]
        {
            parse_table_block_impl(
                &TableParseParams {
                    start, offset, table_start, content_start, content_end, end: span_end,
                    open_delim, content, default_separator: "|",
                    closing: TableClosing::Unterminated,
                },
                state,
                block_metadata,
            )
        }

        rule unterminated_excl_table_block(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = table_start:position!() open_delim:excl_table_delimiter() (eol() / ![_])
              content_start:position!() content:until_table_delimiter(open_delim) content_end:position!()
              ![_]
        {
            parse_table_block_impl(
                &TableParseParams {
                    start, offset, table_start, content_start, content_end, end: span_end,
                    open_delim, content, default_separator: "!",
                    closing: TableClosing::Unterminated,
                },
                state,
                block_metadata,
            )
        }

        rule unterminated_comma_table_block(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = table_start:position!() open_delim:comma_table_delimiter() (eol() / ![_])
              content_start:position!() content:until_table_delimiter(open_delim) content_end:position!()
              ![_]
        {
            parse_table_block_impl(
                &TableParseParams {
                    start, offset, table_start, content_start, content_end, end: span_end,
                    open_delim, content, default_separator: ",",
                    closing: TableClosing::Unterminated,
                },
                state,
                block_metadata,
            )
        }

        rule unterminated_colon_table_block(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = table_start:position!() open_delim:colon_table_delimiter() (eol() / ![_])
              content_start:position!() content:until_table_delimiter(open_delim) content_end:position!()
              ![_]
        {
            parse_table_block_impl(
                &TableParseParams {
                    start, offset, table_start, content_start, content_end, end: span_end,
                    open_delim, content, default_separator: ":",
                    closing: TableClosing::Unterminated,
                },
                state,
                block_metadata,
            )
        }

        rule toc(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = &toc_block_ahead() "toc::" attributes:macro_attributes() end:position!()
          block_macro_end()
        {
            let metadata_from_attributes = attributes.metadata;
            let mut metadata = block_metadata.metadata.clone();
            merge_macro_metadata(&mut metadata, &metadata_from_attributes, state.options.compatibility_mode);
            metadata.move_positional_attributes_to_attributes();
            tracing::debug!("Found Table of Contents block");
            Ok(Block::TableOfContents(TableOfContents {
                title: block_metadata.title.clone(),
                metadata,
                location: state.create_block_location(start, end, offset),
            }))
        }

        rule structural_block_boundaries(block_metadata: &BlockParsingMetadata<'input>)
        = {?
            if style_overrides_block_form(state.options.compatibility_mode, block_metadata.metadata.style) {
                Err("verbatim paragraph style")
            } else {
                Ok(())
            }
        }

        // Select the media title before parsing macros in a preceding dot-title.
        rule media_block_ahead() -> bool
        = &media_block_ahead_true() { true }
        / { false }

        rule source_match()
        = ("https" / "http" / "ftp" / "irc") "://" url_path_match()
        / "mailto:" email_address()
        / path_match()

        rule image(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = structural_block_boundaries(block_metadata)
          &media_block_ahead_true() "image::" source:source() attributes:cached_macro_attributes(block_metadata) end:position!()
          block_macro_end()
        {
            let metadata_from_attributes = attributes.metadata;
            let title = block_metadata.title.clone();
            let mut metadata = block_metadata.metadata.clone();
            route_media_attributes(&mut metadata, &metadata_from_attributes, state.options.compatibility_mode, false);
            let alt = block_metadata.image_alt.clone();
            metadata.alt_source = None;
            metadata.attributes.remove("alt");
            metadata.move_positional_attributes_to_attributes();
            Ok(Block::Image(Image {
                title,
                alt,
                source,
                metadata,
                location: state.create_block_location(start, end, offset),

            }))
        }

        rule audio(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = &media_block_ahead_true() "audio::" source:source() attributes:cached_macro_attributes(block_metadata) end:position!()
          block_macro_end()
        {
            let metadata_from_attributes = attributes.metadata;
            let title = block_metadata.title.clone();
            let mut metadata = block_metadata.metadata.clone();
            merge_macro_metadata(&mut metadata, &metadata_from_attributes, state.options.compatibility_mode);
            metadata.first_source = None;
            metadata.move_positional_attributes_to_attributes();
            Ok(Block::Audio(Audio {
                title,
                source,
                metadata,
                location: state.create_block_location(start, end, offset),
            }))
        }

        // Preserve commas until the converter interprets a provider's playlist syntax.
        rule video(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = &media_block_ahead_true() "video::" target:$([^'[' | '\n' | '\r']+) attributes:cached_macro_attributes(block_metadata) end:position!()
          block_macro_end()
        {
            let target = apply_attribute_substitutions(target, &[Substitution::Attributes], &state.document_attributes);
            let source = Source::from_str_borrowed(state.intern_str(&target))?;
            let metadata_from_attributes = attributes.metadata;
            let title = block_metadata.title.clone();
            let mut metadata = block_metadata.metadata.clone();
            route_media_attributes(&mut metadata, &metadata_from_attributes, state.options.compatibility_mode, true);
            metadata.move_positional_attributes_to_attributes();
            Ok(Block::Video(Video {
                title,
                source,
                metadata,
                location: state.create_block_location(start, end, offset),
            }))
        }

        // A whole line (`ExtLayoutBreakRx`, `MarkdownThematicBreakRx`): `'''` and
        // longer, or one of `-*_` three times with equal runs of spaces between.
        rule thematic_break_marker()
        = ("'''" "'"* / " "*<,3> markdown_thematic_break()) &(whitespace()* (eol() / ![_]))

        rule markdown_thematic_break()
        = line:$(['-' | '*' | '_'] " "* ['-' | '*' | '_'] " "* ['-' | '*' | '_']) {?
            let bytes = line.as_bytes();
            let gap = (bytes.len() - 3) / 2;
            (bytes.len() == 3 + 2 * gap && bytes[1 + gap] == bytes[0] && bytes[2 + 2 * gap] == bytes[0])
                .then_some(()).ok_or("not a uniform thematic break")
        }

        rule thematic_break(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = thematic_break_marker()
        {
            tracing::debug!("Found thematic break block");
            Ok(Block::ThematicBreak(ThematicBreak {
                anchors: thematic_break_anchors(&block_metadata.metadata),
                title: block_metadata.title.clone(),
                location: state.create_block_location(start, span_end, offset),
            }))
        }

        rule page_break(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
            = "<<<" "<"* &(whitespace()* (eol() / ![_]))
        {
            tracing::debug!("Found page break block");
            let mut metadata = block_metadata.metadata.clone();
            metadata.move_positional_attributes_to_attributes();

            Ok(Block::PageBreak(PageBreak {
                title: block_metadata.title.clone(),
                metadata,
                location: state.create_block_location(start, span_end, offset),
            }))
        }

        rule list(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = excessive_list_item(start, offset, block_metadata)
        / callout_list(start, offset, block_metadata)
        / ordinary_list(start, offset, block_metadata, false)
        / description_list(start, offset, block_metadata)

        rule raw_unordered_list_marker() -> &'input str = $("*"+ / "-")

        // `OrderedListRx`: `\.\.*|\d+\.|[a-zA-Z]\.|[IVXivx]+\)`.
        rule raw_ordered_list_marker() -> &'input str
        = $("."+ / digits() "." / ['a'..='z' | 'A'..='Z'] "." / ['I' | 'V' | 'X' | 'i' | 'v' | 'x']+ ")")

        rule unordered_list_marker() -> &'input str
        = marker:raw_unordered_list_marker() {?
            ListLevel::try_from(ListItem::parse_depth_from_marker(marker).unwrap_or(1))
                .map(|_| marker).map_err(|_| "supported list depth")
        }

        rule ordered_list_marker() -> &'input str
        = marker:raw_ordered_list_marker() {?
            ListLevel::try_from(ListItem::parse_depth_from_marker(marker).unwrap_or(1))
                .map(|_| marker).map_err(|_| "supported list depth")
        }

        rule excessive_list_marker()
        = whitespace()* marker:(raw_unordered_list_marker() / raw_ordered_list_marker()) &whitespace() {?
            (ListItem::parse_depth_from_marker(marker).unwrap_or(1) > usize::from(ListLevel::MAX))
                .then_some(()).ok_or("excessive list depth")
        }

        rule excessive_list_item(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = &excessive_list_marker() content:$([^'\n']+) {
            let location = state.create_block_location(span_start, span_end, offset);
            state.add_warning(Warning::new(
                WarningKind::ContentRecovery {
                    message: "list marker depth exceeds 255; preserving item as literal text".into(),
                },
                Some(state.create_error_source_location(location.clone())),
            ));
            Ok(Block::Paragraph(Paragraph {
                metadata: block_metadata.metadata.clone(),
                title: block_metadata.title.clone(),
                content: vec![InlineNode::PlainText(Plain { content, location, escaped: false })],
                location: state.create_block_location(start, span_end, offset),
                source_text: Some(content),
            }))
        }

        rule description_list_marker() -> &'input str = $("::::" / ":::" / "::" / ";;")

        rule callout_list_marker() -> &'input str = $("<" (digits() / ".") ">")

        // This restricted form excludes titles and document attributes, which
        // asciidoctor does not assign to an automatically nested list.
        rule nested_list_metadata(offset: usize, parent_section_level: Option<SectionLevel>) -> Result<BlockParsingMetadata<'input>, Error>
        = meta_start:position!() lines:(line:(
            anchor:anchor(offset) { AttributeOrAnchorLine::Anchor(anchor) }
            / attr:attributes_line() { AttributeOrAnchorLine::Attributes(Box::new(attr)) }
        ) end:position!() eol()* { (line, end) })+
        {
            let mut attributes = ParsedAttributeList::default();
            let meta_end = lines.last().map_or(meta_start, |(_, end)| *end);
            for (line, _) in lines {
                match line {
                    AttributeOrAnchorLine::Anchor(anchor) => push_metadata_anchor(state, &mut attributes.metadata, anchor)?,
                    AttributeOrAnchorLine::Attributes(parsed) => {
                        attributes.merge(*parsed, state.options.compatibility_mode);
                    }
                }
            }
            attributes.metadata.location = Some(state.create_block_location(meta_start, meta_end, offset));
            finish_block_parsing_metadata(
                state,
                attributes,
                Title::default(),
                parent_section_level,
            )
        }

        rule parsed_nested_list_metadata(offset: usize, parent_section_level: Option<SectionLevel>) -> BlockParsingMetadata<'input>
        = metadata:nested_list_metadata(offset, parent_section_level) {?
            metadata.map_err(|_| {
                tracing::error!("error parsing nested list metadata");
                "nested list metadata parse error"
            })
        }

        rule block_attribute_start()
        = !double_open_square_bracket()
        / &("[[" !"[") {?
            (state.options.compatibility_mode == crate::CompatibilityMode::Draft)
                .then_some(()).ok_or("draft anchor shorthand")
        }

        rule attributes_line_match()
        = block_attribute_start()
          open_square_bracket() attribute_list_content(BlockAttributeMode::Block) (eol() / ![_])

        rule anchor_line_match()
        = double_open_square_bracket() [^'\'' | ',' | ']' | '[' | ' ' | '\t' | '\n' | '\r']+
          (comma() block_anchor_reftext(0) / double_close_square_bracket()) (eol() / ![_])

        rule attribute_or_anchor_line_match()
        = anchor_line_match() / attributes_line_match()

        rule nested_list_metadata_gap()
        = eol() / comment_line()

        // Helper rule to check if we're at the start of a new list item (lookahead)
        rule at_list_item_start() = excessive_list_marker() / whitespace()* ordinary_list_marker() whitespace()

        rule ordinary_list_marker() -> ListMarker<'input>
        = marker:unordered_list_marker() { ListMarker::Unordered(marker) }
        / marker:ordered_list_marker() { ListMarker::Ordered(marker) }

        // Helper rule to check if we're at a list separator (forces list termination)
        // Matches either a line comment (//) or empty block attributes ([]) on their own line
        // Note: Separator must be preceded by at least one blank line (2+ newlines)
        // Without a blank line before it, a comment is just skipped, not a separator
        rule at_list_separator()
        = eol()*<2,> at_list_separator_content()

        // Helper rule to check for separator content at current position (no leading newlines)
        // Used by continuation_lines to stop at separators
        rule at_list_separator_content()
        = "//" [^'\n']* (&eol() / ![_])  // Line comment separator
        / whitespace()* "[" whitespace()* "]" whitespace()* (&eol() / ![_])  // Empty block attributes

        rule ordinary_list_principal_continuation(offset: usize) -> (usize, &'input str)
        = eol()
          !(
              &eol()
              / ![_]
              / &at_list_item_start()
              / &("+" whitespace()* (eol() / ![_]))
              / &at_list_separator_content()
              / &block_delimiter_line()
              // A block attribute line starts the next block
              // (`StartOfBlockOrListProc`); Asciidoctor folds that block back
              // into the text only when it is an adjacent paragraph
              // (`ListItem#fold_first`).
              / &attribute_or_anchor_line_match() !folded_paragraph_metadata(offset)
              / at_callout_list_item()
              / check_line_is_description_list(offset)
          )
          start:position!() line:$((!eol() [_])*) { (start, line) }

        // An unordered or ordered list (`Parser.parse_list`): Asciidoctor's
        // reader decides which lines each item holds (`lists::ordinary_list`).
        rule ordinary_list(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>, is_nested: bool) -> Result<Block<'input>, Error>
        = &(whitespace()* ordinary_list_marker() whitespace())
          list:#{|input, pos| {
              let (list, end) = crate::grammar::document::lists::ordinary_list(state, input.len(), start, pos, offset, block_metadata, is_nested);
              peg::RuleResult::Matched(end, list)
          }}
        { list }

        rule unordered_marker(marker: ListMarker<'input>)
        = {? matches!(marker, ListMarker::Unordered(_)).then_some(()).ok_or("ordered marker") }

        // Metadata lines followed directly by paragraph text: Asciidoctor reads
        // them as that paragraph's, then folds the paragraph into the item.
        rule folded_paragraph_metadata(offset: usize)
        = (anchor_line_match() / !non_paragraph_style_line() attributes_line_match())+
          !(eol() / ![_] / at_list_item_start() / block_delimiter_line()
            / "+" whitespace()* (eol() / ![_]) / at_callout_list_item()
            / check_line_is_description_list(offset) / attribute_or_anchor_line_match()
            / ("image" / "video" / "audio" / "toc") "::" / thematic_break_marker())

        // `PARAGRAPH_STYLES` besides `normal`, the admonitions and the stem
        // styles: each turns the paragraph after it into another block.
        rule non_paragraph_style_line()
        = "[" style:$(['a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-']+) &[',' | ']' | '#' | '.' | '%'] {?
            matches!(style, "comment" | "example" | "literal" | "listing" | "open" | "pass" | "quote"
                | "sidebar" | "source" | "verse" | "abstract" | "partintro" | "stem" | "latexmath"
                | "asciimath" | "NOTE" | "TIP" | "IMPORTANT" | "WARNING" | "CAUTION")
                .then_some(()).ok_or("paragraph style")
        }

        // An item's marker line and the paragraph lines adjacent to it, which
        // Asciidoctor folds into the item's text (`ListItem#fold_first`).
        pub(in crate::grammar::document) rule ordinary_item_head(offset: usize) -> ItemHead<'input>
        = whitespace()* marker_start:position!() marker:ordinary_list_marker() after_marker:position!() whitespace()
          checked:(unordered_marker(marker) checked:checklist_item() { checked })?
          first_start:position!()
          first:$((!(eol()) [_])*)
          rest:ordinary_list_principal_continuation(offset)*
          end:position!()
          [_]*
        {
            ItemHead { checked, marker: marker.text(), marker_start, after_marker, first: (first_start, first), rest, end }
        }

        // Only a column-one marker with nonempty text starts a callout item.
        // This probe is also used by list boundaries without parsing child macros.
        rule at_callout_list_item()
        = &(callout_list_marker() whitespace()+ [^' ' | '\t' | '\r' | '\n'])

        // Stop child lists and attached paragraphs before the next parent item,
        // leaving its marker for callout_list_rest_item. The offset check keeps
        // markers in reparsed delimited blocks outside this boundary.
        rule at_callout_parent_item(offset: usize)
        = at_callout_list_item() {?
            (state.callout_list_offset == Some(offset)).then_some(()).ok_or("not a callout parent")
        }

        // Save the enclosing scope for callout_list to restore after parsing its
        // items. Keep pending verbatim references local for validation,
        // since child listings add their own references to the shared catalog.
        rule enter_callout_list(offset: usize) -> (Option<usize>, Vec<CalloutMarker>)
        = {
            let parent = state.callout_list_offset.replace(offset);
            (parent, std::mem::take(&mut state.pending_callouts))
        }

        rule callout_list(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        // Check the item boundary before changing callout scope.
        = at_callout_list_item() structural_block_boundaries(block_metadata)
        context:enter_callout_list(offset)
        first:callout_list_item(offset, block_metadata)
        rest:(callout_list_rest_item(offset, block_metadata))*
        {
            let (parent, callouts) = context;
            state.callout_list_offset = parent;
            state.pending_callouts.clear();
            tracing::debug!("Found callout list block");
            let mut content = vec![first?];
            for item in rest {
                content.push(item?);
            }
            let end = content.last().map_or(span_end, |(_, _, item_end)| *item_end);

            let mut auto_number = 0;
            let mut items: Vec<CalloutListItem> = Vec::with_capacity(content.len());

            for (expected_number, (mut item, marker, _end)) in (1..).zip(content) {
                let actual_number = if marker == "<.>" {
                    auto_number += 1;
                    auto_number.to_string()
                } else {
                    marker.trim_start_matches('<').trim_end_matches('>').to_string()
                };
                if actual_number != expected_number.to_string() {
                    state.add_generic_warning_at(
                        format!(
                            "callout list item index: expected {expected_number}, got {actual_number}"
                        ),
                        item.location.clone(),
                    );
                }
                // List labels use their ordinal even when the source marker is
                // invalid or mixes explicit and automatic numbering.
                item.callout.number = expected_number;

                // Check if the EXPECTED callout exists in the verbatim block
                // (This warns when sequence is broken and the expected number is missing)
                let callout_exists = callouts.iter()
                    .any(|c| c.number == expected_number);
                if !callout_exists {
                    state.add_generic_warning_at(
                        format!("no callout found for <{expected_number}>"),
                        item.location.clone(),
                    );
                }
                items.push(item);
            }

            Ok(Block::CalloutList(CalloutList {
                title: block_metadata.title.clone(),
                metadata: block_metadata.metadata.clone(),
                items,
                location: state.create_location(start+offset, end+offset),
            }))
        }

        rule callout_list_rest_item(offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<(CalloutListItem<'input>, String, usize), Error>
        = eol()+ item:callout_list_item(offset, block_metadata)
        {?
            Ok(item)
        }

        // Ordinary lists can contain callouts, but a marker belonging to an
        // active callout parent must be left for that parent's next item.
        rule nested_callout_list(offset: usize, metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = !at_callout_parent_item(offset) start:position!()
          child_metadata:nested_list_content_metadata(metadata)
          list:callout_list(start, offset, &child_metadata) { list }

        // Probe for an implicit child without consuming input. A parent marker
        // takes precedence even when its text contains a description delimiter.
        rule callout_child_marker(offset: usize)
        = !at_callout_parent_item(offset) (
            &(whitespace()* (unordered_list_marker() / ordered_list_marker()) whitespace())
            / check_line_is_description_list(offset)
        )

        // Attach an implicit child before callout_list_item handles explicit `+`
        // blocks. Metadata must be adjacent to the principal text; a blank line
        // before metadata starts a separate list.
        rule callout_list_nested(offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = eol() child:(
            &((attribute_or_anchor_line_match() eol()*)+ callout_child_marker(offset))
            start:position!()
            metadata:parsed_nested_list_metadata(offset, block_metadata.parent_section_level)
            list:callout_child_list(start, offset, &metadata) { list }
            / eol()* callout_child_marker(offset) start:position!()
            metadata:nested_list_content_metadata(block_metadata)
            list:callout_child_list(start, offset, &metadata) { list }
        ) { child }

        // Inherit section context and text settings without copying the parent's
        // ID, title, or style onto a child that has no metadata of its own.
        rule nested_list_content_metadata(parent: &BlockParsingMetadata<'input>) -> BlockParsingMetadata<'input>
        = {
            BlockParsingMetadata {
                parent_section_level: parent.parent_section_level,
                substitutions: parent.substitutions,
                hardbreaks: parent.hardbreaks,
                ..BlockParsingMetadata::default()
            }
        }

        // Disable child continuations so `+` blocks remain for the owning callout item.
        rule callout_child_list(start: usize, offset: usize, metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = ordinary_list(start, offset, metadata, false)
        / description_list(start, offset, metadata)

        rule callout_list_item(offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<(CalloutListItem<'input>, String, usize), Error>
        = at_callout_list_item()
        marker:callout_list_marker()
        whitespace()
        first_line_start:position!()
        // Parse first line (principal text)
        first_line:$((!(eol()) [_])*)
        // Parse continuation lines that are part of the same paragraph
        // Stop at list markers, explicit continuations, blank lines, or block attributes.
        continuation_lines:(
            eol()
            !at_callout_list_item()
            !check_line_is_description_list(offset)
            !(whitespace()* (unordered_list_marker() / ordered_list_marker() / "[" / "+" whitespace()* eol() / eol()))
            line:$((!(eol()) [_])*)
            { line }
        )*
        first_line_end:position!()
        principal:list_item_principal(offset, block_metadata, first_line_start, first_line_end, first_line, &continuation_lines)
        nested:(!at_list_separator() child:callout_list_nested(offset, block_metadata) { child })?
        explicit_continuations:(!at_list_separator() cont:(
            list_explicit_continuation_immediate(offset, block_metadata)
            / list_explicit_continuation_ancestor(offset, block_metadata)
        ) { cont })*
        list_dangling_continuation()?
        {
            let principal = principal?;
            let item_end = calculate_item_end(first_line.is_empty() && continuation_lines.is_empty(), span_start, first_line_end);

            let blocks = nested
                .into_iter()
                .chain(explicit_continuations)
                .collect::<Result<Vec<_>, _>>()?;

            let location = state.create_location(span_start+offset, item_end+offset);

            // Create a placeholder callout - will be resolved in callout_list
            // We pass the marker string to the parent rule for resolution
            let callout = if marker == "<.>" {
                CalloutMarker::auto(0, location.clone()) // Number will be resolved later
            } else {
                CalloutMarker::explicit(0, location.clone())
            };

            let actual_end = if blocks.is_empty() { item_end } else { state.end_before_line_breaks(item_end, span_end, offset) - 1 };

            Ok((CalloutListItem {
                callout,
                principal,
                blocks: order_block_prefix_events(blocks),
                location: state.create_location(span_start+offset, actual_end+offset),
            }, marker.to_string(), actual_end))
        }

        // Run before child parsing to register footnotes in source order;
        // the item's final action runs after its children are parsed.
        rule list_item_principal(offset: usize, metadata: &BlockParsingMetadata<'input>, start: usize, end: usize, first: &'input str, rest: &[&'input str]) -> Result<Vec<InlineNode<'input>>, Error>
        = {
            let text = assemble_principal_text(state, first, rest);
            if text.trim().is_empty() {
                Ok(Vec::new())
            } else {
                process_inlines(state, metadata, start, end, offset, text).map(|(nodes, _)| nodes)
            }
        }

        rule checklist_item() -> ListItemCheckedStatus
            = checked:(("[x]" / "[*]" / draft_uppercase_check()) { ListItemCheckedStatus::Checked } / "[ ]" { ListItemCheckedStatus::Unchecked }) whitespace()
        {
            checked
        }

        // Asciidoctor's markers are `[ ]`, `[x]` and `[*]` (`Parser.parse_list_item`).
        rule draft_uppercase_check()
        = "[X]" {? if state.options.compatibility_mode == CompatibilityMode::Asciidoctor { Err("uppercase check") } else { Ok(()) } }

        rule check_line_is_description_list(offset: usize)
        = pos:position!() {?
            if find_dlist_marker(state.input.as_bytes(), pos + offset) {
                Ok(())
            } else {
                Err("no dlist marker on current line")
            }
        }

        // A description list (`Parser.parse_description_list`): Asciidoctor's
        // reader decides which lines each item holds (`lists::description_list`).
        rule description_list(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = !at_list_item_start() !at_callout_list_item()
          list:#{|input, pos| match crate::grammar::document::lists::description_list(state, input.len(), start, pos, offset, block_metadata) {
              Some((list, end)) => peg::RuleResult::Matched(end, list),
              None => peg::RuleResult::Failed,
          }}
        { list }

        // The paragraph lines adjacent to an item's first line, which
        // Asciidoctor folds into its text (`ListItem#fold_first`).
        pub(in crate::grammar::document) rule folded_lines(offset: usize) -> (Vec<(usize, &'input str)>, usize)
        = lines:ordinary_list_principal_continuation(offset)* end:position!() [_]* { (lines, end) }

        // Parse a single immediate continuation (0 empty lines before +)
        // These attach to the current (most recent) list item per AsciiDoc spec.
        // Uses block_in_continuation to prevent lists inside continuations from consuming
        // further continuations that belong to the parent item.
        // Pattern: exactly one newline before + (content\n+\nblock)
        rule list_explicit_continuation_immediate(offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = eol() !eol() "+" eol()
          block:block_in_continuation(offset, block_metadata.parent_section_level)
        {
            tracing::debug!("List immediate continuation block (0 empty lines)");
            block
        }

        // Parse a single ancestor continuation (1+ empty lines before +)
        // Per AsciiDoc spec: each empty line before + moves attachment up one nesting level.
        // 1 empty line = parent, 2 empty lines = grandparent, etc.
        // Uses block_in_continuation to prevent lists inside continuations from consuming
        // further continuations that belong to the parent item.
        // Pattern: two or more newlines before + (content\n\n+\nblock)
        rule list_explicit_continuation_ancestor(offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = eol() eol()+ "+" eol()
          block:block_in_continuation(offset, block_metadata.parent_section_level)
        {
            tracing::debug!("List ancestor continuation block (1+ empty lines)");
            block
        }

        // Asciidoctor drops a continuation with no attachable block, including one
        // followed only by unused metadata. Leave that metadata for the block sequence
        // so any document-attribute events are retained.
        rule list_dangling_continuation()
        = eol()+ "+" whitespace()* eol()? &(eol() / ![_] / trailing_block_metadata_match())
        {
            tracing::debug!("Dropped dangling list continuation marker");
        }

        // Parse a quoted paragraph: "content" followed by `-- attribution[, citation]`
        //
        // This matches the AsciiDoc shorthand syntax for blockquotes:
        // ```
        // "I hold it that a little rebellion now and then is a good thing."
        // -- Thomas Jefferson, Papers of Thomas Jefferson
        // ```
        rule quoted_paragraph(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = "\"" content_start:position!() quoted_content:$((!"\"" [_])+) "\""
          eol()
          "-- " attr_start:position!() attribution_line:$([^'\n']+)
        {
            tracing::debug!("found quoted paragraph");

            // Parse attribution line: "Author Name, Source Title" or just "Author Name"
            // Intern the slices into the parser arena so downstream inline parsing
            // can produce nodes with the `'input` lifetime.
            let (attr_str, cite_str): (&'input str, Option<&'input str>) = match attribution_line.split_once(',') {
                Some((attr, cite)) => (state.intern_str(attr.trim()), Some(state.intern_str(cite.trim()))),
                None => (state.intern_str(attribution_line.trim()), None),
            };

            let attr_end_offset = attr_start + attr_str.len();
            let (attr_inlines, _) = process_inlines(
                state,
                block_metadata,
                attr_start,
                attr_end_offset,
                offset,
                attr_str,
            )?;

            let cite_inlines = if let Some(cite) = cite_str {
                let cite_offset_in_line = attribution_line.find(',').unwrap_or(0) + 1;
                let cite_raw_start = attr_start + cite_offset_in_line + (attribution_line[cite_offset_in_line..].len() - attribution_line[cite_offset_in_line..].trim_start().len());
                let cite_pos = PositionWithOffset {
                    offset: cite_raw_start,
                    position: state.line_map.offset_to_position(cite_raw_start, state.input),
                };
                let (cite_inlines, _) = process_inlines(
                    state,
                    block_metadata,
                    cite_pos.offset,
                    cite_raw_start + cite.len(),
                    offset,
                    cite,
                )?;
                Some(cite_inlines)
            } else {
                None
            };

            let blocks = document_parser::blocks(quoted_content, state, content_start + offset, block_metadata.parent_section_level, None).unwrap_or_else(|e| {
                adjust_and_log_parse_error(&e, quoted_content, content_start + offset, state, "Error parsing content as blocks in quoted paragraph");
                Ok(Vec::new())
            })?;

            let mut metadata = block_metadata.metadata.clone();
            metadata.style = Some("quote");
            metadata.attribution = Some(Attribution::new(attr_inlines));
            if let Some(inlines) = cite_inlines {
                metadata.citetitle = Some(CiteTitle::new(inlines));
            }

            Ok(Block::DelimitedBlock(DelimitedBlock {
                source_text: None,
                metadata,
                delimiter: "\"",
                inner: DelimitedBlockType::DelimitedQuote(blocks),
                title: block_metadata.title.clone(),
                location: state.create_block_location(start, span_end, offset),
                open_delimiter_location: None,
                close_delimiter_location: None,
            }))
        }

        /// Parse a markdown-style blockquote: lines starting with `> `
        ///
        /// This matches the Markdown-compatible syntax for blockquotes:
        /// ```
        /// > I hold it that a little rebellion now and then is a good thing,
        /// > and as necessary in the political world as storms in the physical.
        /// > -- Thomas Jefferson, Papers of Thomas Jefferson: Volume 11
        /// ```
        ///
        /// The content after `> ` on each line is joined and parsed as blocks.
        /// Attribution is extracted from a line matching `> -- Author[, Citation]`.
        rule markdown_blockquote(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = content_start:position!() lines:markdown_blockquote_content_line()+ attribution:markdown_blockquote_attribution()?
        {
            tracing::debug!("found markdown blockquote");

            // `start` is before the block's metadata lines; the content counts from the first `>`.
            let content: &'input str = state.intern_join(lines.iter().map(|(_, line)| line), "\n");

            let mut metadata = block_metadata.metadata.clone();
            metadata.style = Some("quote");
            if let Some((author, author_start, citation)) = attribution {
                let author: &'input str = state.intern_str(&author);
                let author_pos = PositionWithOffset {
                    offset: author_start,
                    position: state.line_map.offset_to_position(author_start, state.input),
                };
                let attr_end_offset = author_start + author.len();
                let (attr_inlines, _) = process_inlines(
                    state,
                    block_metadata,
                    author_pos.offset,
                    attr_end_offset,
                    offset,
                    author,
                )?;
                metadata.attribution = Some(Attribution::new(attr_inlines));

                if let Some((cite, cite_start)) = citation {
                    let cite: &'input str = state.intern_str(&cite);
                    let cite_pos = PositionWithOffset {
                        offset: cite_start,
                        position: state.line_map.offset_to_position(cite_start, state.input),
                    };
                    let (cite_inlines, _) = process_inlines(
                        state,
                        block_metadata,
                        cite_pos.offset,
                        cite_start + cite.len(),
                        offset,
                        cite,
                    )?;
                    metadata.citetitle = Some(CiteTitle::new(cite_inlines));
                }
            }

            let location = state.create_block_location(start, state.end_before_line_breaks(start, span_end, offset), offset);

            let blocks = if content.trim().is_empty() {
                Vec::new()
            } else {
                let origin = crate::grammar::document::content_origin::ContentOrigin::quote(&lines, content_start + offset);
                origin.parse_blocks(content, state, |state| {
                    document_parser::blocks(content, state, 0, block_metadata.parent_section_level, None).unwrap_or_else(|e| {
                        adjust_and_log_parse_error(&e, content, 0, state, "Error parsing content as blocks in markdown blockquote");
                        Ok(Vec::new())
                    })
                })?
            };

            Ok(Block::DelimitedBlock(DelimitedBlock {
                source_text: None,
                metadata,
                delimiter: ">",
                inner: DelimitedBlockType::DelimitedQuote(blocks),
                title: block_metadata.title.clone(),
                location,
                open_delimiter_location: None,
                close_delimiter_location: None,
            }))
        }

        /// Match a content line of a markdown-style blockquote
        /// A line is content if:
        /// 1. It's followed by another `>` line (so `> -- ...` mid-blockquote is content)
        /// 2. OR it doesn't start with `-- ` (so it can't be attribution)
        rule markdown_blockquote_content_line() -> (usize, &'input str)
        = "> " content:$([^'\n']*) eol() &">" { (2, content) }
        / "> " !("-- ") content:$([^'\n']*) (eol() / ![_]) { (2, content) }
        / ">" eol() &">" { (1, "") }
        / ">" eol() { (1, "") }
        / ">" ![_] { (1, "") }

        /// Match an attribution line: `> -- Author[, Citation]`
        /// Only matches at the END of a blockquote (not followed by more `>` lines)
        /// Returns (author, author_start, Option<(citation, cite_start)>)
        rule markdown_blockquote_attribution() -> (String, usize, Option<(String, usize)>)
        = "> -- " author_start:position!() author:$([^(',' | '\n')]+) ", " cite_start:position!() citation:$([^'\n']+) ((eol() !">") / ![_]) {
            (author.trim().to_string(), author_start, Some((citation.trim().to_string(), cite_start)))
        }
        / "> -- " author_start:position!() author:$([^'\n']+) ((eol() !">") / ![_]) {
            (author.trim().to_string(), author_start, None)
        }

        // Asciidoctor lets verbatim styles override list and macro syntax.
        // Draft matches those forms before interpreting the style.
        rule styled_verbatim_paragraph(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = !structural_block_boundaries(block_metadata) block:paragraph(start, offset, block_metadata) { block }

        // Asciidoctor keeps apparent headings, delimiters, and metadata inside
        // a styled verbatim paragraph until a blank line or list continuation.
        rule verbatim_paragraph_content() -> &'input str
        = content:$((!(
            eol()*<2,>
            / eol()* ![_]
            / eol() "+" whitespace()* (eol() / ![_])
        ) [_])+) { content }

        // A line opening a delimited block ends a paragraph and, without a list
        // continuation, a list item (`Parser.read_lines_for_list_item`).
        rule block_delimiter_line()
        = example_delimiter() &(eol() / ![_])
        / listing_delimiter()
        / literal_delimiter()
        / sidebar_delimiter()
        / quote_delimiter()
        / pass_delimiter()
        / table_delimiter()
        / markdown_code_delimiter()
        / comment_delimiter()
        / open_delimiter() &(whitespace()* eol())

        rule paragraph_text_run(offset: usize)
        = !at_line_start(offset) #{|input, start| {
            let end = paragraph_text_end(input, start);
            if end == start {
                peg::RuleResult::Failed
            } else {
                peg::RuleResult::Matched(end, ())
            }
        }}

        rule paragraph(start: usize, offset: usize, block_metadata: &BlockParsingMetadata<'input>) -> Result<Block<'input>, Error>
        = admonition:(structural_block_boundaries(block_metadata) value:admonition() { value })?
        content_start:position!()
        content:(
          !structural_block_boundaries(block_metadata) text:verbatim_paragraph_content() { text }
          / $((
            "[[" (!eol() [_])*
            // Only line starts and newlines need block-boundary lookahead.
            // Stop at '[' to retain the anchor-line alternative above.
            / paragraph_text_run(offset)
            / !(
            eol()*<2,>
            / eol()* ![_]
            / eol() &attributes_line()
            / eol() &anchor_line_match()
            / eol() block_delimiter_line()
            / eol() excessive_list_marker()
            / eol() at_callout_parent_item(offset)
            / eol() list_item_paragraph_boundary(start, offset)
            / eol() &("+" whitespace()* (eol() / ![_]))  // Only a standalone plus continues a list.
            ) [_]
        )+))
        {
            build_paragraph(state, block_metadata, ParagraphParams {
                start, content_start, end: span_end, offset, content, admonition,
            })
        }

        // In a list item's own lines, a paragraph no blank line precedes ends at
        // any list item line (`read_paragraph_lines` with `break_at_list`).
        rule list_item_paragraph_boundary(start: usize, offset: usize)
        = ({?
            let at = start + offset;
            let after_blank = state.input.as_bytes().get(at.wrapping_sub(2)..at) == Some(b"\n\n");
            (state.list_item_offset == Some(offset) && !after_blank).then_some(()).ok_or("not adjacent list item content")
          })
          (whitespace()* ordinary_list_marker() whitespace()
           / "<" (digits() / ".") ">" whitespace()
           / check_line_is_description_list(offset))

        rule admonition() -> (String, usize, usize)
            = variant:$("NOTE" / "WARNING" / "TIP" / "IMPORTANT" / "CAUTION") ": "
        {
            (variant.to_string(), span_start, span_end)
        }

        // Lookahead rule that warns about anchor ID-like patterns containing whitespace.
        //
        // This uses negative lookahead and emits a warning if it detects whitespace. It
        // does not consume the input.
        rule warn_anchor_id_with_whitespace() -> ()
        = &(
            id:$([^'\'' | ',' | ']' | '.' | '#']+)
            {?
                if id.chars().any(char::is_whitespace) {
                    let location = state.create_block_location(span_start, span_end, 0);
                    state.add_generic_warning_at(
                        format!("anchor id '{id}' contains whitespace which is not allowed, treating as literal text"),
                        location,
                    );
                }
                // Always fail so the lookahead doesn't match - we just want the side
                // effect
                Err::<(), &'static str>("")
            }
        )

        rule anchor(offset: usize) -> Anchor<'input>
        = result:(
            // Double-bracket [[id]] syntax - allows dots in ID since no role shorthand
            // possible.
            //
            // Whitespace is excluded per AsciiDoc documentation at
            // https://docs.asciidoctor.org/asciidoc/latest/attributes/id/#valid-id-characters
            double_open_square_bracket() warn_anchor_id_with_whitespace()? id:$([^'\'' | ',' | ']' | '[' | ' ' | '\t' | '\n' | '\r']+) comma() reftext:block_anchor_reftext(offset) {
                (id, Some(reftext))
            } /
            double_open_square_bracket() warn_anchor_id_with_whitespace()? id:$([^'\'' | ',' | ']' | '[' | ' ' | '\t' | '\n' | '\r']+) double_close_square_bracket() {
                (id, None)
            } /
            // Single-bracket [#id] shorthand - exclude '.', '%' as they start role/option
            // shorthands.
            //
            // Only the bare `[#id]` form is an anchor here; `[#id,...]` is NOT — the
            // comma introduces further block attributes (e.g. `[#id,discrete]`), so it
            // must fall through to the attribute-line parser where `#id` becomes the id
            // and the rest are positional/named attributes. Unlike `[[id,reftext]]`, a
            // single-bracket comma does not set a reftext (matching asciidoctor).
            //
            // Whitespace is excluded per AsciiDoc documentation at
            // https://docs.asciidoctor.org/asciidoc/latest/attributes/id/#valid-id-characters
            open_square_bracket() "#" warn_anchor_id_with_whitespace()? id:$([^'\'' | ',' | ']' | '.' | '%' | ' ' | '\t' | '\n' | '\r']+) close_square_bracket() {
                (id, None)
            }
        )
        end:position!()
        eol()
        {
            let (id, reftext) = result;
            let substituted_id = state.intern_cow(apply_attribute_substitutions(id, HEADER, &state.document_attributes));
            Anchor {
                id: substituted_id,
                xreflabel: None,
                location: state.create_block_location(span_start, end, offset),
                reftext_source: reftext,
                bibliography_label: None,
                bibliography: false,
            }
        }

        rule block_anchor_reftext(offset: usize) -> usize
        = start:position!() text:$([^'\n' | '\r']+) {?
            text.strip_suffix("]]")
                .and_then(|source| resolve_block_anchor_reftext(source, &state.document_attributes, state.options.compatibility_mode))
                .map(|(value, source_map)| {
                    let source_end = start + offset + text.len() - 2;
                    let text = state.intern_cow(value);
                    crate::grammar::reference_text::capture_block(state, crate::grammar::attributes::ResolvedContentAttribute {
                        text,
                        quote: crate::grammar::attributes::AttributeQuote::Unquoted,
                        source_map: source_map.with_origin(crate::grammar::inline_preprocessor::SourceMap::default(), start + offset),
                        source_range: start + offset..source_end,
                        attributes: Rc::clone(&state.document_attributes),
                    })
                })
                .ok_or("block anchor reference text")
        }

        rule inline_anchor(offset: usize) -> InlineNode<'input>
        = double_open_square_bracket()
        // Whitespace is excluded - IDs must not contain spaces
        warn_anchor_id_with_whitespace()?
        id:$([^'\'' | ',' | ']' | '[' | ' ' | '\t' | '\n' | '\r']+)
        reftext:(
            comma() start:position!() reftext:$([^']']+) {
                Some((start, reftext))
            } /
            {
                None
            }
        )
        double_close_square_bracket()
        {?
            let substituted_id = state.intern_cow(apply_attribute_substitutions(id, HEADER, &state.document_attributes));
            let source = reftext.map(|(start, text)| crate::grammar::reference_text::capture_inline(state, text, start + offset, true));
            let xreflabel = source.map(|id| crate::grammar::reference_text::resolve(state, id)).transpose().map_err(|_| "reference label")?;
            Ok(InlineNode::InlineAnchor(Anchor {
                id: substituted_id,
                xreflabel,
                location: state.create_block_location(span_start, span_end, offset),
                reftext_source: source,
                bibliography_label: None,
                bibliography: false,
            }))
        }

        rule inline_anchor_match() -> ()
        = double_open_square_bracket() [^'\'' | ',' | ']' | '[' | ' ' | '\t' | '\n' | '\r']+ (comma() [^']']+)? double_close_square_bracket()

        rule invalid_bibliography_anchor(offset: usize) -> InlineNode<'input>
        = syntax:$("[[[" [^']' | '\n']* "]]]") {?
            let body = &syntax[3..syntax.len() - 3];
            let id = body.split_once(',').map_or(body, |(id, _)| id);
            if is_valid_bibliography_id(id) {
                Err("valid bibliography anchor")
            } else {
                Ok(InlineNode::PlainText(Plain {
                    content: syntax,
                    location: state.create_block_location(span_start, span_end, offset),
                    escaped: false,
                }))
            }
        }

        // `BlockAttributeLineRx` takes `[]` too: an empty attribute list.
        rule attributes_line() -> ParsedAttributeList<'input>
            = attributes:attributes() eol() {
                attributes
            }

        rule raw_attributes_line() -> RawAttributeList<'input>
            = block_attribute_start() attributes:raw_attribute_list(BlockAttributeMode::Block) eol() { attributes }

        rule raw_attribute_list(context: BlockAttributeMode) -> RawAttributeList<'input>
            = open_square_bracket() start:position!() source:attribute_list_content(context)
            { RawAttributeList { source, start } }

        pub(in crate::grammar::document) rule attributes() -> ParsedAttributeList<'input>
            = block_attribute_start()
              open_square_bracket()
              content_start:position!()
              content:attribute_list_content(BlockAttributeMode::Block)
            {?
                parse_block_attribute_list(
                    state,
                    content,
                    content_start,
                    BlockAttributeMode::Block,
                    0,
                ).map_err(|_| "cannot map block attribute locations")
            }

        /// Macro attribute parsing - simpler than block attributes.
        ///
        /// Does NOT support shorthand syntax (.role, #id, %option).
        /// Shorthands are only valid in block-level attributes, not inside macro brackets.
        ///
        /// Asciidoctor behavior:
        /// - `image::photo.jpg[.role]` -> alt=".role" (literal text, NOT a role)
        /// - `image::photo.jpg[Diablo 4 picture of Lilith.]` -> alt="Diablo 4 picture of Lilith."
        rule macro_attributes() -> ParsedAttributeList<'input>
            = macro_attributes_for()

        rule cached_macro_attributes(block_metadata: &BlockParsingMetadata<'input>) -> ParsedAttributeList<'input>
            = attributes:raw_attribute_list(BlockAttributeMode::Macro) {?
                if let Some(metadata) = &block_metadata.media_attributes {
                    Ok(ParsedAttributeList::from_metadata((**metadata).clone()))
                } else {
                    parse_block_attribute_list(state, attributes.source, attributes.start, BlockAttributeMode::Macro, 0)
                        .map_err(|_| "cannot map block macro attribute locations")
                }
            }

        rule macro_attributes_for() -> ParsedAttributeList<'input>
            = open_square_bracket()
              content_start:position!()
              content:attribute_list_content(BlockAttributeMode::Macro)
            {?
                parse_block_attribute_list(
                    state,
                    content,
                    content_start,
                    BlockAttributeMode::Macro,
                    0,
                ).map_err(|_| "cannot map block macro attribute locations")
            }

        rule open_square_bracket() = "["
        rule close_square_bracket() = "]"
        rule attribute_list_content(context: BlockAttributeMode) -> &'input str
            = content:$((!last_close_square_bracket() [^'\n' | '\r'])*) close_square_bracket() {?
                block_attribute_list_allowed(content, &state.document_attributes, state.options.compatibility_mode, context)
                    .then_some(content)
                    .ok_or("attribute list without outer whitespace")
            }
        rule last_close_square_bracket()
            = &("]" [^']' | '\n' | '\r']* (eol() / ![_]))
        rule double_open_square_bracket() = "[["
        rule double_close_square_bracket() = "]]"
        rule comma() = ","
        rule period() = "."
        /// URL rule matches both web URLs (proto://) and mailto: URLs
        rule url() -> String =
        proto:$("https" / "http" / "ftp" / "irc") "://" path:url_path() { format!("{proto}://{path}") }
        / "mailto:" email:email_address() { format!("mailto:{email}") }

        /// Email address pattern (RFC 822 simplified)
        ///
        /// Local part: alphanumeric plus . _ % + -
        /// Domain: alphanumeric plus . - (must contain TLD, must end with alphanumeric)
        ///
        /// - Domain must contain at least one dot (e.g., `foo@bar` is not valid,
        ///   `foo@bar.com` is)
        ///
        /// - Domain must end with alphanumeric (prevents capturing trailing punctuation
        ///   like `user@example.com.` - the dot stays outside the email for sentence
        ///   endings)
        rule email_address() -> String
        = local:$(
            // Quoted local part: "Jane Doe"@example.com
            // Quotes allow spaces and special chars in the local part (RFC 5321).
            "\"" [^'"']+ "\""
            // Unquoted local part (no spaces allowed)
            / ['a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '%' | '+' | '-']+
        )
        "@"
        // Format: alphanumeric+ (separator alphanumeric+)*
        // This ensures domain ends with alphanumeric (not . or -) and has proper structure.
        // e.g., `example.com.` -> matches `example.com`, trailing dot stays outside
        domain:$(
            ['a'..='z' | 'A'..='Z' | '0'..='9']+
            (['.' | '-'] ['a'..='z' | 'A'..='Z' | '0'..='9']+)*
        )
        {?
            // Require TLD - domain must contain at least one dot. This prevents `foo@bar`
            // from becoming a mailto link.
            if !domain.contains('.') {
                return Err("email domain must have TLD (contain a dot)");
            }

            Ok(format!("{local}@{domain}"))
        }

        /// URL target content following `://`.
        /// Supports query parameters, fragments, and percent escapes while excluding
        /// brackets that delimit the macro attributes.
        /// Spaces must be internal to the target.
        rule url_path() -> String = path:$(url_path_match())
        {?
            let inline_state = InlinePreprocessorParserState::new_all_enabled(
                path,
                state.line_map.clone(),
                state.input,
                state.arena,
            );
            let processed = inline_preprocessing::run(path, &state.document_attributes, &inline_state)
            .map_err(|_| {
                tracing::error!("could not preprocess url path");
                "could not preprocess url path"
            })?;
            let result = restore_url_path(processed);
            let warnings = inline_state.drain_warnings();
            drop(inline_state);
            for warning in warnings {
                state.add_inline_preprocessor_warning(warning);
            }
            Ok(result)
        }

        rule url_path_char() = ['A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | ':' | '/' | '?' | '#' | '@' | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '=' | '%' | '\\' ]
        rule internal_url_path_spaces() = [' ']+ &url_path_char()
        rule url_path_match() = url_path_char() (url_path_char() / internal_url_path_spaces())*

        /// URL for bare autolinks — avoids capturing trailing sentence punctuation
        /// (., ;, !, etc.) by only consuming punctuation when more URL chars follow.
        rule bare_url() -> String =
        proto:$("https" / "http" / "ftp" / "irc") "://" path:bare_url_path()
        { format!("{proto}://{path}") }

        /// URL path for bare autolinks. Like url_path() but:
        /// - Trailing punctuation (. , ; ! ? : ' *) only consumed when followed by more URL chars.
        /// - `)` only consumed as part of a balanced `(...)` group, preventing capture of
        ///   sentence-level parens like `(see http://example.com)`.
        rule bare_url_path() -> String = path:$(
            bare_url_safe_char()
            ( bare_url_safe_char()
            / bare_url_paren_group()
            / "("
            / bare_url_trailing_char() &bare_url_char()
            )*
        )
        {?
            let inline_state = InlinePreprocessorParserState::new_all_enabled(
                path,
                state.line_map.clone(),
                state.input,
                state.arena,
            );
            let processed = inline_preprocessing::run(path, &state.document_attributes, &inline_state)
                .map_err(|_| {
                    tracing::error!("could not preprocess bare url path");
                    "could not preprocess bare url path"
                })?;
            let result = restore_url_path(processed);
            let warnings = inline_state.drain_warnings();
            drop(inline_state);
            for warning in warnings {
                state.add_inline_preprocessor_warning(warning);
            }
            Ok(result)
        }

        /// Balanced parenthesized group in a URL path.
        /// Handles nested parens: `http://example.com/wiki/Foo_(bar_(baz))`
        /// Only `)` consumed via this rule — unbalanced `)` is never captured.
        rule bare_url_paren_group()
        = "(" (bare_url_safe_char() / bare_url_trailing_char() / bare_url_paren_group() / "(")* ")"

        /// URL chars that are safe to end a bare URL — won't be confused with sentence punctuation.
        /// Excludes `(` and `)` which are handled separately via `bare_url_paren_group`.
        rule bare_url_safe_char() = ['A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '~'
            | '/' | '#' | '@' | '$' | '&'
            | '+' | '=' | '%' | '\\']

        /// URL chars that are valid mid-URL but should not end a bare URL.
        /// Excludes `)` which is only consumed via balanced `bare_url_paren_group`.
        rule bare_url_trailing_char() = ['.' | ',' | ';' | '!' | '?' | ':' | '\'' | '*']

        /// Any valid URL path char (for lookahead in trailing char rule).
        /// Includes `(` because it can start a paren group.
        /// Excludes `)` so that trailing chars before `)` aren't greedily consumed
        /// (e.g., `http://example.com.)` keeps both `.` and `)` outside).
        rule bare_url_char() = bare_url_safe_char() / bare_url_trailing_char() / "("

        /// Fragment identifier for URLs and cross-references (e.g., `#section-id`)
        /// Only used by `xref:` and `link:` macros — other macros (`image::`, `video::`, etc.) do not support fragments
        rule path_fragment() -> String
            = "#" fragment:$(['a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-']+)
        {
            format!("#{fragment}")
        }

        /// Filesystem path accepted by block macros.
        ///
        /// ASCII input uses a conservative filename set. Non-ASCII Unicode characters
        /// are accepted unchanged, and `{`/`}` permit `AsciiDoc` attribute substitution.
        /// Existing percent escapes and internal spaces are preserved.
        rule path() -> String = path:$(path_match())
        {?
            let inline_state = InlinePreprocessorParserState::new_all_enabled(
                path,
                state.line_map.clone(),
                state.input,
                state.arena,
            );
            let processed = inline_preprocessing::run(path, &state.document_attributes, &inline_state)
            .map_err(|_| {
                tracing::error!("could not preprocess path");
                "could not preprocess path"
            })?;
            let result = processed.text.into_owned();
            let warnings = inline_state.drain_warnings();
            drop(inline_state);
            for warning in warnings {
                state.add_inline_preprocessor_warning(warning);
            }
            Ok(result)
        }

        rule path_char() = ['A'..='Z' | 'a'..='z' | '0'..='9' | '{' | '}' | '_' | '-' | '.' | '/' | '\\' | '%' | '\'' | '"' | '\u{80}'..='\u{10FFFF}' ]
        rule internal_path_spaces() = [' ']+ &path_char()
        rule path_match() = path_char() (path_char() / internal_path_spaces())*


        rule source() -> Source<'input>
            = source:
        (
            u:url() {?
                let interned = state.intern_str(&u);
                Source::from_str_borrowed(interned).map_err(|_| "failed to parse URL")
            }
            / p:path() {?
                let interned = state.intern_str(&p);
                Source::from_str_borrowed(interned).map_err(|_| "failed to parse path")
            }
        )
        { source }

        rule digits() = ['0'..='9']+

        rule whitespace() = quiet!{ " " / "\t" }
        rule eol() = quiet!{ "\n" }

        rule comment_line() = quiet!{ comment() (eol() / ![_]) }
        rule comment() = quiet!{ "//" !"/" [^'\n']* (&eol() / ![_]) }

        // Separator whitespace belongs to the declaration, not its value.
        rule document_attribute_value() -> Cow<'input, str>
        = whitespace()+ value:(
            lines:wrapped_value_lines() { Cow::Owned(join_wrapped_value(&lines)) }
            / single_line:$([^'\n']+) { Cow::Borrowed(single_line) }
        ) { value }

        // A value line that ends with ` \` continues on the next nonblank
        // line. The value ends with the first line without that marker, or
        // before a blank line. Continuation lines are trimmed.
        rule wrapped_value_lines() -> Vec<&'input str>
        = first:$((!(" \\" (eol() / ![_])) [^'\n'])+ " \\") &(eol() / ![_])
          continued:(eol() line:continuing_value_line() { line })*
          last:(eol() line:final_value_line() { line })?
        {
            let mut lines = Vec::with_capacity(continued.len() + 2);
            lines.push(first);
            lines.extend(continued);
            lines.extend(last);
            lines
        }

        rule continuing_value_line() -> &'input str
        = line:$([^'\n']+) {?
            let line = line.trim();
            line.ends_with(" \\").then_some(line).ok_or("value line without continuation")
        }

        rule final_value_line() -> &'input str
        = line:$([^'\n']+) {?
            let line = line.trim();
            (!line.is_empty()).then_some(line).ok_or("blank line")
        }

        // Document attribute parsing
        // Works identically in both header and block metadata contexts.
        // As in asciidoctor, the closing `:` must be followed by whitespace or
        // the end of the line, so `:name:text` is ordinary text.
        rule document_attribute_match() -> AttributeDeclaration<'input>
        = ":"
        key_entry:(
            "!" key:$([^':' | '\n']+) { (false, key) }
            / key:$([^('!' | ':' | '\n')]+) "!" { (false, key) }
            / key:$([^':' | '\n']+) { (true, key) }
        )
        ":" &([' ' | '\t' | '\n'] / ![_])
        value:document_attribute_value()?
        {
            let (set, key) = key_entry;
            let (attr_value, ignored_value) = if !set {
                (RawAttributeValue::Unset, value)
            } else if let Some(v) = value {
                (RawAttributeValue::Text(v), None)
            } else {
                (RawAttributeValue::Set, None)
            };
            AttributeDeclaration { name: key, value: attr_value, ignored_value }
        }
        / expected!("document attribute key starting with ':'")

        rule position() -> PositionWithOffset = offset:position!() {
            PositionWithOffset {
                offset,
                position: state.line_map.offset_to_position(offset, state.input)
            }
        }

    }
}
