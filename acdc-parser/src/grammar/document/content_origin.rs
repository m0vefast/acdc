//! Content whose bytes do not stand contiguously in the source: a
//! Markdown-style quote's lines with their `>` prefixes removed, a table cell
//! with its `\|` escapes collapsed. The content is parsed as its own input, so
//! every offset, boundary check and UTF-8 snap reads the text it counts in;
//! what the parse produced is then moved to the source.

use std::{mem, rc::Rc};

use crate::{
    Block, Error, Location, SourceLocation,
    grammar::{LineMap, ParserState, location_walk},
    model::LeveloffsetRange,
    preprocessor::LeveloffsetScope,
};

pub(super) struct ContentOrigin {
    /// Source byte of content offset 0, before any bytes removed there.
    base: usize,
    /// Per removal: the content offset it precedes, bytes removed up to and
    /// including it, and its own length.
    removals: Vec<(usize, usize, usize)>,
}

impl ContentOrigin {
    /// `removals`: content offset and removed length, in content order.
    pub(super) fn new(removals: impl IntoIterator<Item = (usize, usize)>, base: usize) -> Self {
        let mut total = 0;
        let removals = removals
            .into_iter()
            .map(|(at, len)| {
                total += len;
                (at, total, len)
            })
            .collect();
        Self { base, removals }
    }

    /// A quote's lines, each with its prefix length and content.
    pub(super) fn quote(lines: &[(usize, &str)], base: usize) -> Self {
        let mut at = 0;
        Self::new(
            lines.iter().map(|(prefix, content)| {
                let removal = (at, *prefix);
                at += content.len() + 1;
                removal
            }),
            base,
        )
    }

    fn to_source(&self, local: usize) -> usize {
        let removed = self
            .removals
            .iter()
            .take_while(|(at, ..)| *at <= local)
            .last()
            .map_or(0, |(_, removed, _)| *removed);
        self.base + local + removed
    }

    /// The content offset of source byte `source`; a removed byte belongs to
    /// the content offset it precedes.
    fn to_local(&self, source: usize) -> usize {
        let before = self
            .removals
            .iter()
            .take_while(|(at, removed, len)| self.base + at + removed - len <= source)
            .last();
        match before {
            Some((at, removed, _)) => source.saturating_sub(self.base + removed).max(*at),
            None => source.saturating_sub(self.base),
        }
    }

    fn restore(&self, location: &mut Location, state: &ParserState<'_>) {
        location.absolute_start = self.to_source(location.absolute_start);
        location.absolute_end = self.to_source(location.absolute_end);
        location.start = state
            .line_map
            .offset_to_position(location.absolute_start, state.input);
        location.end = state
            .line_map
            .offset_to_position(location.absolute_end, state.input);
    }

    fn restore_diagnostic(&self, at: &mut SourceLocation, state: &ParserState<'_>) {
        let mut location = at.location.clone();
        self.restore(&mut location, state);
        *at = state.create_error_source_location(location);
    }

    /// Parse blocks; see [`Self::parse`].
    pub(super) fn parse_blocks<'a>(
        &self,
        content: &'a str,
        state: &mut ParserState<'a>,
        parse: impl FnOnce(&mut ParserState<'a>) -> Result<Vec<Block<'a>>, Error>,
    ) -> Result<Vec<Block<'a>>, Error> {
        self.parse(content, state, parse, |blocks, visit| {
            location_walk::walk_block_locations_mut(blocks, visit);
        })
    }

    /// Parse `content` with `parse` in the content's own coordinates, then
    /// move what it produced (every location `locations` visits) and what it
    /// recorded beside it — footnotes, table of contents entries, warnings, an
    /// error — to the source.
    pub(super) fn parse<'a, T>(
        &self,
        content: &'a str,
        state: &mut ParserState<'a>,
        parse: impl FnOnce(&mut ParserState<'a>) -> Result<T, Error>,
        locations: impl FnOnce(&mut T, &mut dyn FnMut(&mut Location)),
    ) -> Result<T, Error> {
        let footnotes_before = state.footnote_tracker.borrow().footnotes.len();
        let toc_before = state.toc_entries.len();
        let warnings_before = state.warnings.borrow().len();

        let local_ranges = state
            .leveloffset_ranges
            .iter()
            .map(|range| {
                LeveloffsetRange::new(
                    self.to_local(range.start_offset),
                    self.to_local(range.end_offset),
                    range.value,
                )
            })
            .collect();
        let local_scopes = state
            .leveloffset_scopes
            .iter()
            .map(|scope| LeveloffsetScope {
                range: self.to_local(scope.range.start)..self.to_local(scope.range.end),
                assignment: scope.assignment.clone(),
            })
            .collect();
        let source_end = self.to_source(content.len());
        let mut local_gaps: Vec<usize> = self
            .removals
            .iter()
            .map(|(at, ..)| *at)
            .chain(
                state
                    .text_gaps()
                    .iter()
                    .filter(|&&gap| gap > self.base && gap < source_end)
                    .map(|&gap| self.to_local(gap)),
            )
            .filter(|&gap| gap > 0 && gap < content.len())
            .collect();
        local_gaps.sort_unstable();
        local_gaps.dedup();
        let outer_gaps = mem::replace(
            &mut state.text_gaps,
            (!local_gaps.is_empty()).then(|| local_gaps.into()),
        );
        let outer_input = mem::replace(&mut state.input, content);
        let outer_map = mem::replace(&mut state.line_map, Rc::new(LineMap::new(content)));
        let outer_ranges = mem::replace(&mut state.leveloffset_ranges, local_ranges);
        let outer_scopes = mem::replace(&mut state.leveloffset_scopes, local_scopes);
        let outer_sources = mem::take(&mut state.source_ranges);
        let outer_callouts = state.callout_list_offset.take();
        let outer_list_item = state.list_item_offset.take();
        let outer_placeholders = mem::replace(&mut state.placeholder_lines, Rc::from([]));
        let outer_warnings_start = mem::replace(&mut state.warnings_start, warnings_before);
        state.reset_offset_caches();

        let parsed = parse(state);

        state.warnings_start = outer_warnings_start;
        state.input = outer_input;
        state.line_map = outer_map;
        state.leveloffset_ranges = outer_ranges;
        state.leveloffset_scopes = outer_scopes;
        state.source_ranges = outer_sources;
        state.text_gaps = outer_gaps;
        state.callout_list_offset = outer_callouts;
        state.list_item_offset = outer_list_item;
        state.placeholder_lines = outer_placeholders;
        state.reset_offset_caches();

        let mut value = parsed.map_err(|mut error| {
            if let Some(at) = error.source_location_mut() {
                self.restore_diagnostic(at, state);
            }
            error
        })?;
        locations(&mut value, &mut |location| self.restore(location, state));
        let mut footnotes = mem::take(&mut state.footnote_tracker.borrow_mut().footnotes);
        for footnote in footnotes.iter_mut().skip(footnotes_before) {
            self.restore(&mut footnote.location, state);
            for node in &mut footnote.content {
                location_walk::walk_inline_locations_mut(node, &mut |location| {
                    self.restore(location, state);
                });
            }
        }
        state.footnote_tracker.borrow_mut().footnotes = footnotes;
        let mut toc_entries = mem::take(&mut state.toc_entries);
        for entry in toc_entries.iter_mut().skip(toc_before) {
            self.restore(&mut entry.location, state);
            for node in entry.title.inlines_mut() {
                location_walk::walk_inline_locations_mut(node, &mut |location| {
                    self.restore(location, state);
                });
            }
        }
        state.toc_entries = toc_entries;
        let warnings = state.warnings.borrow_mut().split_off(warnings_before);
        for mut warning in warnings {
            if let Some(at) = &mut warning.location {
                self.restore_diagnostic(at, state);
            }
            state.add_warning(warning);
        }
        Ok(value)
    }
}
