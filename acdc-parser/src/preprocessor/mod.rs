//! The preprocessor module is responsible for processing the input document and expanding include directives.
use std::{borrow::Cow, cell::RefCell, fmt::Write as _, path::Path, rc::Rc};

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};

use crate::{
    Options, Warning, WarningKind,
    error::{Error, Positioning, SourceLocation},
    model::{LeveloffsetRange, Position, SourceRange},
};

mod attribute;
mod conditional;
mod include;
mod tag;

use include::{Include, IncludeResult};

/// Result from preprocessing that includes both the processed text and metadata needed
/// for accurate parsing (like leveloffset ranges).
///
/// `text` borrows from the caller's input when possible (fast path: no include /
/// conditional / multi-line-attribute triggers), enabling zero-copy parsing all
/// the way through the grammar. When any trigger fires, `text` is `Cow::Owned`.
#[derive(Debug, Default)]
pub(crate) struct PreprocessorResult<'a> {
    /// The preprocessed document text.
    pub(crate) text: Cow<'a, str>,
    /// Byte ranges where specific leveloffset values apply.
    /// Used by the parser to adjust section levels.
    pub(crate) leveloffset_ranges: Vec<LeveloffsetRange>,
    /// Byte ranges mapping preprocessed output back to source files.
    /// Used by the parser to produce accurate file/line info in warnings.
    pub(crate) source_ranges: Vec<SourceRange>,
    /// Per-include expansion metadata — one entry per processed `include::`
    /// directive in the ROOT document (nested includes are not reported here;
    /// only top-level so consumers can map root-source lines ↔ expanded
    /// lines unambiguously). Embedders (e.g. the wasm bridge) expose this
    /// to clients that need to translate post-expansion block positions
    /// back to original source positions.
    pub(crate) include_expansions: Vec<IncludeExpansion>,
    /// Source line numbers (1-based) consumed by conditional preprocessor
    /// directives (`ifdef`/`ifndef`/`ifeval`/`endif`) that produced no
    /// corresponding output line. Embedders use these alongside
    /// `include_expansions` so cursor / scroll alignment compensates for
    /// EVERY preprocessor-induced source→output line shift, not just
    /// includes. Only populated for depth==0 (root document).
    pub(crate) conditional_drops: Vec<usize>,
}

/// One root-level `include::` directive's expansion outcome — the 1-based
/// source line of the directive and the number of lines it produced in the
/// preprocessed output (after `[lines=...]`, `[tag=...]`, `[tags=...]`, and
/// any other filtering attributes have been applied).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncludeExpansion {
    /// 1-based line number of the `include::` directive in the root source.
    pub source_line: usize,
    /// Number of lines the expansion occupies in the preprocessed output.
    /// 0 if the include resolved to no content (missing file, empty filter).
    pub expanded_lines: usize,
}

impl PreprocessorResult<'_> {
    /// Materialize any borrowed text into an owned `PreprocessorResult<'static>`.
    /// Used by `process_file` / `process_reader` where the source buffer is a
    /// local and cannot outlive the function.
    fn into_owned(self) -> PreprocessorResult<'static> {
        PreprocessorResult {
            text: Cow::Owned(self.text.into_owned()),
            leveloffset_ranges: self.leveloffset_ranges,
            source_ranges: self.source_ranges,
            include_expansions: self.include_expansions,
            conditional_drops: self.conditional_drops,
        }
    }
}

/// Per-directive context bundling position and file information that
/// would otherwise need to be threaded individually through every
/// directive helper (and push the argument count past clippy's limit).
#[derive(Debug)]
struct DirectiveContext<'a> {
    line_number: &'a mut usize,
    current_offset: usize,
    file_parent: Option<&'a Path>,
}

/// Mutable state accumulated during preprocessing.
struct PreprocessorState {
    lines: Vec<String>,
    byte_offset: usize,
    leveloffset_ranges: Vec<LeveloffsetRange>,
    source_ranges: Vec<SourceRange>,
    /// Per-include expansion entries — pushed only for root-level
    /// directives (depth==0) so consumers see one entry per visible
    /// `include::` line in the user's main source. Nested includes
    /// are folded into their parent's count.
    include_expansions: Vec<IncludeExpansion>,
    /// Source line numbers (1-based) of preprocessor-directive lines that
    /// were consumed from the input but produced no output line. Populated
    /// for ifdef/ifndef/ifeval/endif at depth==0. See `PreprocessorResult`.
    conditional_drops: Vec<usize>,
}

impl PreprocessorState {
    fn push_line(&mut self, line: String) {
        self.byte_offset += line.len() + 1;
        self.lines.push(line);
    }
}

/// BOM (Byte Order Mark) patterns for encoding detection
const BOM_PATTERNS: &[(&[u8], &Encoding, usize, &str)] = &[
    (&[0xEF, 0xBB, 0xBF], UTF_8, 3, "UTF-8"),
    (&[0xFF, 0xFE], UTF_16LE, 2, "UTF-16 LE"),
    (&[0xFE, 0xFF], UTF_16BE, 2, "UTF-16 BE"),
];

/// Reads a file and decodes it based on BOM (Byte Order Mark) or explicit encoding.
///
/// Supports:
/// - UTF-8 with BOM (EF BB BF)
/// - UTF-16 LE with BOM (FF FE)
/// - UTF-16 BE with BOM (FE FF)
/// - UTF-8 without BOM (fallback)
/// - Explicit encoding via `encoding` parameter
///
/// # Errors
/// Returns an error if:
/// - The file cannot be read
/// - The explicit encoding label is unknown
/// - The file is not valid UTF-8 and has no BOM
pub(crate) fn read_and_decode_file(
    file_path: &Path,
    encoding: Option<&str>,
    resolver: Option<&crate::file_resolver::DynFileResolver>,
) -> Result<String, Error> {
    // Resolver injection (added 2026-05-17 for WASM embeddings): if the
    // caller wired a custom file reader via `Options::with_file_resolver`,
    // use it. Otherwise fall back to `std::fs::read` on native targets — on
    // wasm32 this returns an error and the include preprocessor will warn
    // and skip the directive (Asciidoctor reference behavior for missing
    // files).
    //
    // `Error::Io` here is opaque on purpose; `Include::lines` decides whether
    // to treat it as "warn + skip" (matches native `path.exists() == false`)
    // or surface as a hard error. The structured `FileResolverError` is
    // preserved via `std::io::Error::new(kind, e)` (the resolver error
    // becomes the io error's source) so callers can walk the source chain
    // via `std::error::Error::source` for detail.
    let bytes: Cow<'_, [u8]> = if let Some(resolver) = resolver {
        resolver.read(file_path).map_err(|e| {
            // `#[non_exhaustive]` on FileResolverError means we can't list
            // every variant exhaustively (future ones land without a major
            // bump); allow the wildcard explicitly here.
            #[allow(clippy::wildcard_enum_match_arm)]
            let kind = match &e {
                crate::file_resolver::FileResolverError::NotFound { .. } => {
                    std::io::ErrorKind::NotFound
                }
                _ => std::io::ErrorKind::Other,
            };
            let io = std::io::Error::new(kind, e);
            Error::from(io)
        })?
    } else {
        Cow::Owned(std::fs::read(file_path).map_err(Error::from)?)
    };
    let bytes: &[u8] = &bytes;

    // If there was an encoding specified, decode the entire file as that
    if let Some(enc_label) = encoding {
        if let Some(encoding) = Encoding::for_label(enc_label.as_bytes()) {
            let (cow, _, had_errors) = encoding.decode(bytes);
            if had_errors {
                tracing::error!(
                    path = ?file_path.display(),
                    encoding = %enc_label,
                    "decoding encountered errors"
                );
            }
            return Ok(cow.into_owned());
        }
        return Err(Error::UnknownEncoding(enc_label.to_string()));
    }

    // Check for BOM patterns and decode accordingly
    for (bom, encoding, skip, name) in BOM_PATTERNS {
        if bytes.starts_with(bom)
            && let Some(content) = bytes.get(*skip..)
        {
            let (cow, _, had_errors) = encoding.decode(content);
            if had_errors {
                tracing::error!(
                    path = ?file_path.display(),
                    encoding = name,
                    "decoding encountered errors"
                );
            }
            return Ok(cow.into_owned());
        }
    }

    // If no BOM, try decoding as UTF-8 directly
    let (cow, _, had_errors) = UTF_8.decode(bytes);
    if !had_errors {
        return Ok(cow.into_owned());
    }

    // If you get here, the file is not valid UTF-8 (and no BOM)
    Err(Error::UnrecognizedEncodingInFile(
        file_path.display().to_string(),
    ))
}

/// Maximum include-recursion depth before bailing with a warning.
/// Matches Asciidoctor's reference behavior (which uses the same value
/// to detect circular includes and runaway depth from a JS resolver
/// fixture or a typo in vault content). Re-exported at crate root so
/// downstream consumers can compare against it programmatically rather
/// than substring-matching the warning text.
pub const MAX_INCLUDE_DEPTH: usize = 64;

/// Preprocessor shared across `Include` / `Conditional` / `Tag` helpers.
///
/// Carries a warning sink (`Rc<RefCell<Vec<Warning>>>`) that the caller
/// — typically `parse_input` / `parse_inline` in `lib.rs` — also hands
/// to the later `ParserState`. That makes preprocessor warnings
/// (missing includes, unclosed tags, bad attribute lines, if/endif
/// mismatches, ...) reach `ParseResult::warnings()` alongside grammar
/// warnings.
///
/// `depth` tracks include-nesting from the root parse (0 at the outermost
/// call). Nested preprocessor invocations from `Include::read_content_from_file`
/// pass `depth + 1`. When `depth >= MAX_INCLUDE_DEPTH`, the next
/// `process_include` short-circuits with a warning — this prevents stack
/// overflow on cyclic includes (e.g. `a→b→a`) and bounded-but-massive
/// fixtures, both of which would otherwise trap the wasm module.
///
/// All public entry points take the handle explicitly; the struct is
/// purely a carrier so nested `&self` helpers (`process_include`,
/// `process_conditional`, `process_directive_line`) can reach the sink
/// without threading it through every parameter list.
#[derive(Debug)]
pub(crate) struct Preprocessor {
    warnings: Rc<RefCell<Vec<Warning>>>,
    depth: usize,
}

impl Preprocessor {
    /// Push a warning with an attached source location, also emitting it
    /// through `tracing::warn!` as a belt-and-suspenders fallback so
    /// subscribers keep seeing the same messages.
    pub(crate) fn add_warning_at(
        &self,
        message: impl Into<Cow<'static, str>>,
        location: SourceLocation,
    ) {
        let warning = Warning::new(WarningKind::Other(message.into()), Some(location));
        tracing::warn!(?warning);
        self.warnings.borrow_mut().push(warning);
    }
}

impl Preprocessor {
    /// Helper to create a `SourceLocation` from preprocessor context (line-level precision).
    ///
    /// Since the preprocessor operates line-by-line and doesn't track column positions,
    /// we use column=1 as a placeholder. The line number and offset still provide
    /// useful location information for error messages.
    fn create_source_location(line_number: usize, file_parent: Option<&Path>) -> SourceLocation {
        SourceLocation {
            file: file_parent.map(Path::to_path_buf),
            positioning: Positioning::Position(Position {
                line: line_number,
                column: 0, // Preprocessor doesn't track column - use 0 as placeholder
            }),
        }
    }

    /// Normalize line endings and trailing whitespace.
    ///
    /// Fast path: when the input already uses LF line endings, has no trailing
    /// whitespace on any line, and no embedded CR characters, return
    /// `Cow::Borrowed` (optionally trimming a single trailing newline to match
    /// `str::lines`'s drop-trailing-empty behavior). Slow path: rebuild the
    /// buffer line-by-line as before.
    fn normalize(input: &str) -> Cow<'_, str> {
        // Match the original behavior of `input.lines()`: a single trailing
        // newline (`\n` or `\r\n`) is dropped.
        let trimmed = input
            .strip_suffix("\r\n")
            .or_else(|| input.strip_suffix('\n'))
            .unwrap_or(input);
        // Any `\r` (including inside `\r\n` pairs) or trailing whitespace on a
        // line forces a rebuild.
        let needs_rebuild = trimmed.as_bytes().contains(&b'\r')
            || trimmed
                .split('\n')
                .any(|line| matches!(line.as_bytes().last(), Some(b' ' | b'\t')));
        if !needs_rebuild {
            return Cow::Borrowed(trimmed);
        }
        let mut result = String::with_capacity(input.len());
        for (i, line) in input.lines().map(str::trim_end).enumerate() {
            if i > 0 {
                result.push('\n');
            }
            result.push_str(line);
        }
        Cow::Owned(result)
    }

    /// Fast-path detection: returns `true` if the normalized text contains no
    /// preprocessor triggers (include, ifdef/ifndef/ifeval, escaped directives,
    /// multi-line attribute continuations). When true, the slow rebuild loop is
    /// unnecessary and the caller can return the normalized text directly,
    /// preserving any borrow from the original input.
    ///
    /// This is a read-only scan: it must not mutate options. Single-line
    /// attributes (`:attr: value`) are intentionally left to downstream parsing
    /// — the slow path's attribute mutation is only needed to evaluate
    /// conditional directives, which by definition trigger the slow path.
    ///
    /// Note: we deliberately do NOT track verbatim block state here. The slow
    /// path processes `include::` (and other) directives regardless of whether
    /// they appear inside `----` listing blocks — that matches asciidoctor's
    /// behavior and is exercised by fixtures like `include_with_indent.adoc`.
    /// Skipping lines inside verbatim blocks would silently pass such inputs
    /// through unmodified.
    fn try_pass_through(text: &str) -> bool {
        for line in text.lines() {
            // Escaped directives are unescaped by the slow path.
            if line.starts_with("\\include")
                || line.starts_with("\\ifdef")
                || line.starts_with("\\ifndef")
                || line.starts_with("\\ifeval")
            {
                return false;
            }
            // Multi-line attribute continuation would be collapsed by the slow path.
            if line.starts_with(':') && (line.ends_with(" + \\") || line.ends_with(" \\")) {
                return false;
            }
            // Directive lines: include::, ifdef::, ifndef::, ifeval::
            if line.ends_with(']')
                && !line.starts_with('[')
                && line.contains("::")
                && (line.starts_with("include")
                    || line.starts_with("ifdef")
                    || line.starts_with("ifndef")
                    || line.starts_with("ifeval"))
            {
                return false;
            }
        }
        true
    }

    #[tracing::instrument(skip(reader, warnings))]
    pub(crate) fn process_reader<R: std::io::Read>(
        mut reader: R,
        options: &Options,
        warnings: Rc<RefCell<Vec<Warning>>>,
    ) -> Result<PreprocessorResult<'static>, Error> {
        let mut input = String::new();
        reader.read_to_string(&mut input).map_err(|e| {
            tracing::error!(error=?e, "failed to read from reader");
            e
        })?;
        // The local `input` cannot outlive this function, so materialize any
        // borrowed text into an owned result.
        Ok(Self { warnings, depth: 0 }
            .process_inner(&input, None, options)?
            .into_owned())
    }

    #[tracing::instrument(skip(warnings))]
    pub(crate) fn process<'a>(
        input: &'a str,
        options: &Options,
        warnings: Rc<RefCell<Vec<Warning>>>,
    ) -> Result<PreprocessorResult<'a>, Error> {
        // If the caller provided a virtual file path (e.g. WASM embeddings
        // that have no real fs path but need `include::` to work via the
        // FileResolver), use it as the file_parent for relative include
        // resolution. Otherwise inherit the legacy "no file context" path
        // where includes are skipped with a warning.
        let file_path = options.virtual_current_file.as_deref();
        Self { warnings, depth: 0 }.process_inner(input, file_path, options)
    }

    /// Like `process` but lets the caller pass the file path explicitly, used
    /// by `parse_file` where the input has already been read and leaked.
    #[tracing::instrument(skip(file_path, warnings))]
    pub(crate) fn process_with_file<'a>(
        input: &'a str,
        file_path: &Path,
        options: &Options,
        warnings: Rc<RefCell<Vec<Warning>>>,
    ) -> Result<PreprocessorResult<'a>, Error> {
        Self { warnings, depth: 0 }.process_inner(input, Some(file_path), options)
    }

    #[cfg(test)]
    #[tracing::instrument(skip(file_path, warnings))]
    pub(crate) fn process_file<P: AsRef<Path>>(
        file_path: P,
        options: &Options,
        warnings: Rc<RefCell<Vec<Warning>>>,
    ) -> Result<PreprocessorResult<'static>, Error> {
        if file_path.as_ref().parent().is_some() {
            // Use read_and_decode_file to support UTF-8, UTF-16 LE, and UTF-16 BE with BOM
            let input =
                read_and_decode_file(file_path.as_ref(), None, options.file_resolver.as_ref())?;
            Ok(Self { warnings, depth: 0 }
                .process_inner(&input, Some(file_path.as_ref()), options)?
                .into_owned())
        } else {
            Err(Error::InvalidIncludePath(
                Box::new(Self::create_source_location(1, Some(file_path.as_ref()))),
                file_path.as_ref().to_path_buf(),
            ))
        }
    }

    /// Process an include directive.
    ///
    /// Returns the included content along with any leveloffset that applies.
    /// Bails (warn + return Ok(None)) when:
    /// - no `file_parent` context is available (caller didn't set
    ///   `virtual_current_file` and didn't go through `parse_file`)
    /// - include depth has reached `MAX_INCLUDE_DEPTH` — protects against
    ///   cyclic include chains (a→b→a) and runaway depth in JS-resolver
    ///   fixtures, both of which would otherwise blow the wasm stack
    #[tracing::instrument(skip(self))]
    fn process_include(
        &self,
        line: &str,
        line_number: usize,
        current_offset: usize,
        file_parent: Option<&Path>,
        options: &Options,
    ) -> Result<Option<IncludeResult>, Error> {
        if self.depth >= MAX_INCLUDE_DEPTH {
            self.add_warning_at(
                Cow::Owned(format!(
                    "include depth exceeded {MAX_INCLUDE_DEPTH} — possible cyclic include; skipping"
                )),
                Self::create_source_location(line_number, file_parent),
            );
            return Ok(None);
        }
        if let Some(current_file_path) = file_parent {
            if let Some(parent_dir) = current_file_path.parent() {
                let include = Include::parse(
                    parent_dir,
                    line,
                    line_number,
                    current_offset,
                    Some(current_file_path),
                    options,
                    &self.warnings,
                    self.depth,
                )?;
                return Ok(Some(include.lines()?));
            }
        } else {
            // Surface to ParseResult::warnings() — previously only logged via
            // tracing, which doesn't reach the consumer (especially in wasm
            // where tracing has no default subscriber).
            self.add_warning_at(
                Cow::Borrowed(
                    "include directive cannot be processed: no file context (set `virtual_current_file` or use `parse_file`)",
                ),
                Self::create_source_location(line_number, file_parent),
            );
        }
        Ok(None)
    }

    /// Process a conditional directive (ifdef/ifndef/ifeval)
    #[tracing::instrument(skip(self, lines, attributes))]
    fn process_conditional<'a, I: Iterator<Item = &'a str>>(
        &self,
        line: &str,
        lines: &mut std::iter::Peekable<I>,
        ctx: &mut DirectiveContext<'_>,
        attributes: &crate::DocumentAttributes,
    ) -> Result<Option<String>, Error> {
        let mut content = String::new();
        let condition_line_number = *ctx.line_number;
        let condition = conditional::parse_line(
            line,
            condition_line_number,
            ctx.current_offset,
            ctx.file_parent,
        )?;

        while let Some(next_line) = lines.peek() {
            if next_line.is_empty() {
                tracing::trace!(?line, "single line if directive");
                break;
            } else if next_line.starts_with("endif") {
                // Calculate the line number and offset for the endif line
                let endif_line_number = *ctx.line_number + 1;
                let endif_offset =
                    ctx.current_offset + line.len() + content.len() + content.lines().count();
                let endif = conditional::parse_endif(
                    next_line,
                    endif_line_number,
                    endif_offset,
                    ctx.file_parent,
                )?;

                if !endif.closes(&condition) {
                    // Record as a warning in addition to raising the hard
                    // error: the warning lands on `ParseResult::warnings`
                    // even if the caller recovers from the error.
                    self.add_warning_at(
                        "attribute mismatch between if and endif directives",
                        Self::create_source_location(endif_line_number, ctx.file_parent),
                    );
                    return Err(Error::InvalidConditionalDirective(Box::new(
                        Self::create_source_location(endif_line_number, ctx.file_parent),
                    )));
                }
                tracing::trace!(?content, "multiline if directive");
                lines.next();
                *ctx.line_number += 1;
                break;
            }
            let _ = writeln!(content, "{next_line}");
            lines.next();
            *ctx.line_number += 1;
        }

        if condition.is_true(
            attributes,
            &mut content,
            condition_line_number,
            ctx.current_offset,
            ctx.file_parent,
        )? {
            Ok(Some(content))
        } else {
            Ok(None)
        }
    }

    #[tracing::instrument(skip(lines, attribute_content))]
    fn process_continuation<'a, I: Iterator<Item = &'a str>>(
        attribute_content: &mut String,
        lines: &mut std::iter::Peekable<I>,
        line_number: &mut usize,
    ) {
        while let Some(next_line) = lines.peek() {
            let next_line = next_line.trim();
            // If the next line isn't the end of a continuation, or a
            // continuation, we need to break out.
            if next_line.starts_with(':') || next_line.is_empty() {
                break;
            }
            // If we get here, and we get a hard wrap, keep everything as is.
            // If we get here, and we get a soft wrap, then remove the newline.
            // Anything else means we're at the end of the wrapped attribute, so
            // feed it and break.
            if next_line.ends_with(" + \\") {
                attribute_content.push_str(next_line);
                attribute_content.push('\n');
                lines.next();
                *line_number += 1;
            } else if next_line.ends_with(" \\") {
                attribute_content.push_str(next_line.trim_end_matches('\\'));
                lines.next();
                *line_number += 1;
            } else {
                attribute_content.push_str(next_line);
                lines.next();
                *line_number += 1;
                break;
            }
        }
    }

    /// Check if a line is a verbatim or raw block delimiter.
    ///
    /// Verbatim/raw blocks preserve content literally, including comments.
    /// Recognized delimiters:
    /// - `----` (listing/source blocks) - 4+ hyphens
    /// - `....` (literal blocks) - 4+ periods
    /// - `++++` (passthrough blocks) - 4+ plus signs
    /// - ` ``` ` (markdown code fences) - 3+ backticks
    #[tracing::instrument]
    fn is_verbatim_delimiter(line: &str) -> Option<&str> {
        let trimmed = line.trim();

        // Check for markdown code fences (3+ backticks)
        if trimmed.starts_with("```") {
            return Some("```");
        }

        // Check for other delimiters (4+ chars)
        //
        // We need to fetch the same delimiter size to make sure we close the block
        // correctly, and the minimum size is 4.
        let mut chars = trimmed.chars();
        let first_char = chars.next()?;
        if first_char != '-' && first_char != '.' && first_char != '+' {
            return None;
        }
        let mut idx = 1;
        for next_char in chars {
            if next_char == first_char {
                idx += 1;
            } else {
                break;
            }
        }
        if idx >= 4 {
            return trimmed.get(..idx);
        }
        None
    }

    /// Handle the result of processing an include directive.
    /// Records leveloffset and source ranges, and extends output with included lines.
    ///
    /// This also merges nested ranges from included files, adjusting their byte offsets
    /// to be relative to the current output position. This enables proper accumulation
    /// through arbitrarily deep include nesting.
    fn handle_include_result(
        include_result: IncludeResult,
        state: &mut PreprocessorState,
        // The 1-based line number of the `include::` directive in the file
        // currently being preprocessed. For root-level includes this is the
        // user-facing line; for nested includes it's a line within an
        // already-included file and we DON'T record it (only root-level
        // tracking is exposed to consumers).
        directive_source_line: usize,
        // Whether we're at depth 0 (processing the root document) — only
        // then do we record into include_expansions.
        is_root_depth: bool,
    ) {
        let start_offset = state.byte_offset;

        // Calculate the byte length of the included content
        let content_len: usize = include_result
            .lines
            .iter()
            .map(|l| l.len() + 1) // +1 for newline
            .sum();

        // If there's an effective leveloffset, record the range
        if let Some(leveloffset) = include_result.effective_leveloffset {
            if leveloffset != 0 {
                state.leveloffset_ranges.push(LeveloffsetRange::new(
                    start_offset,
                    start_offset + content_len,
                    leveloffset,
                ));
                tracing::trace!(
                    leveloffset,
                    start_offset,
                    end_offset = start_offset + content_len,
                    "Recording leveloffset range for include"
                );
            }
        }

        // Merge nested leveloffset ranges from the included file.
        // Shift their byte offsets to be relative to the current output position.
        // This enables proper accumulation through nested includes (A→B→C).
        for nested_range in include_result.nested_leveloffset_ranges {
            let adjusted_range = LeveloffsetRange::new(
                nested_range.start_offset + start_offset,
                nested_range.end_offset + start_offset,
                nested_range.value,
            );
            tracing::trace!(
                original_start = nested_range.start_offset,
                original_end = nested_range.end_offset,
                adjusted_start = adjusted_range.start_offset,
                adjusted_end = adjusted_range.end_offset,
                leveloffset = adjusted_range.value,
                "Merging nested leveloffset range"
            );
            state.leveloffset_ranges.push(adjusted_range);
        }

        // Record a SourceRange for the included content
        if let Some(file) = include_result.file {
            state.source_ranges.push(SourceRange {
                start_offset,
                end_offset: start_offset + content_len,
                file: file.clone(),
                start_line: 1,
            });
            tracing::trace!(
                ?file,
                start_offset,
                end_offset = start_offset + content_len,
                "Recording source range for include"
            );

            // Merge nested source ranges, adjusting byte offsets
            for nested_range in include_result.nested_source_ranges {
                state.source_ranges.push(SourceRange {
                    start_offset: nested_range.start_offset + start_offset,
                    end_offset: nested_range.end_offset + start_offset,
                    file: nested_range.file,
                    start_line: nested_range.start_line,
                });
            }
        }

        // Record the expansion outcome for ALL root-level directives so
        // consumers see one entry per visible include line in their source —
        // INCLUDING 0-line expansions. The include directive line ITSELF is
        // removed from the preprocessed output, so even an empty expansion
        // shifts every subsequent line back by 1; consumers need to know
        // that happened so they can compensate. For depth > 0, the nested
        // include's lines roll up into the parent include's `content_len`
        // automatically (the parent's count already reflects the
        // post-nested-expansion line total).
        if is_root_depth {
            state.include_expansions.push(IncludeExpansion {
                source_line: directive_source_line,
                expanded_lines: include_result.lines.len(),
            });
        }

        state.byte_offset += content_len;
        state.lines.extend(include_result.lines);
    }

    fn process_directive_line<'a>(
        &self,
        line: &'a str,
        lines: &mut std::iter::Peekable<std::str::Lines<'a>>,
        ctx: &mut DirectiveContext<'_>,
        options: &Options,
        out: &mut PreprocessorState,
    ) -> Result<(), Error> {
        if line.starts_with("\\include")
            || line.starts_with("\\ifdef")
            || line.starts_with("\\ifndef")
            || line.starts_with("\\ifeval")
        {
            out.push_line(line[1..].to_string());
        } else if line.starts_with("ifdef")
            || line.starts_with("ifndef")
            || line.starts_with("ifeval")
        {
            // Translator alignment: every line consumed inside an
            // ifdef..endif region (the directive itself + any content lines
            // + the endif line) must be accounted for so cursor/scroll
            // mapping after the block points at the correct source line.
            //
            // Cases:
            //   single-line   `ifdef::foo[content]`           → 1 source / 1 output  (no drops)
            //   single-line, false                            → 1 source / 0 output  (1 drop at directive)
            //   multi-line, true   (M content lines)          → M+2 source / M+1 output (1 drop)
            //   multi-line, false  (M content lines)          → M+2 source / 0 output (M+2 drops)
            //
            // The kept-multiline "off by 1" comes from how kept content is
            // pushed: a single multi-line string ending in `\n`. After
            // `out.lines.join("\n")` that trailing `\n` collides with the
            // join separator of the NEXT entry, contributing one extra
            // (empty) output line — which silently swallows the endif line
            // so we only need to record the ifdef drop.
            let condition_line = *ctx.line_number;
            let kept = self.process_conditional(line, lines, ctx, &options.document_attributes)?;
            let end_line = *ctx.line_number;
            let source_consumed = end_line - condition_line + 1;
            // Effective output lines contributed by `kept` if pushed via
            // `push_line` + later `join("\n")`. A standalone "abc" produces
            // 1 line; "abc\n" produces 2 (the content line + an empty
            // trailing line from the literal `\n` adjacent to the join's
            // separator).
            let output_lines = kept.as_deref().map_or(0, |c| c.matches('\n').count() + 1);
            if let Some(content) = kept {
                out.push_line(content);
            }
            if self.depth == 0 {
                let drop_count = source_consumed.saturating_sub(output_lines);
                for offset in 0..drop_count {
                    out.conditional_drops.push(condition_line + offset);
                }
            }
        } else if line.starts_with("include") {
            let directive_source_line = *ctx.line_number;
            if let Some(include_result) = self.process_include(
                line,
                directive_source_line,
                ctx.current_offset,
                ctx.file_parent,
                options,
            )? {
                Self::handle_include_result(
                    include_result,
                    out,
                    directive_source_line,
                    self.depth == 0,
                );
            } else if self.depth == 0 {
                // `process_include` returned None: the directive line was
                // consumed but `handle_include_result` never ran, so the
                // include_expansions sink missed a recording. Common causes:
                // no `file_parent` (callers using `parse` without
                // `virtual_current_file` — e.g. the wasm `parse_block`
                // entry point), or depth exceeded. The directive line
                // STILL disappears from the preprocessed output (it never
                // hit `out.push_line`), shifting every subsequent line up
                // by 1 — embedders translating post-expansion positions
                // need a 0-length expansion entry to compensate, exactly
                // like the missing-file case in `handle_include_result`.
                out.include_expansions.push(IncludeExpansion {
                    source_line: directive_source_line,
                    expanded_lines: 0,
                });
            }
        } else {
            out.push_line(line.to_string());
        }
        Ok(())
    }

    #[tracing::instrument]
    fn process_inner<'a>(
        &self,
        input: &'a str,
        file_parent: Option<&Path>,
        options: &Options,
    ) -> Result<PreprocessorResult<'a>, Error> {
        let normalized = Preprocessor::normalize(input);

        // Fast path: no triggers means the slow rebuild would produce byte-identical
        // output. Return the normalized text directly, preserving any borrow from
        // the caller's input so that downstream parsing can be zero-copy.
        if Self::try_pass_through(&normalized) {
            return Ok(PreprocessorResult {
                text: normalized,
                leveloffset_ranges: Vec::new(),
                source_ranges: Vec::new(),
                include_expansions: Vec::new(),
                conditional_drops: Vec::new(),
            });
        }

        // Slow path: at least one trigger fires (include, conditional,
        // multi-line attribute continuation, or escaped directive). Rebuild
        // line-by-line as before. Hold the normalized text as a local so the
        // peekable iterator can borrow from it for the duration of this call.
        let normalized_owned;
        let normalized_ref: &str = match &normalized {
            Cow::Borrowed(s) => s,
            Cow::Owned(s) => {
                normalized_owned = s;
                normalized_owned.as_str()
            }
        };

        let mut options = options.clone();
        let output = Vec::with_capacity(normalized_ref.lines().count());
        let mut lines = normalized_ref.lines().peekable();
        let mut line_number = 1;
        let mut current_offset = 0;
        let mut out = PreprocessorState {
            lines: output,
            byte_offset: 0,
            leveloffset_ranges: Vec::new(),
            source_ranges: Vec::new(),
            include_expansions: Vec::new(),
            conditional_drops: Vec::new(),
        };
        let mut in_verbatim_block = false;
        let mut current_delimiter: Option<&str> = None;

        while let Some(line) = lines.next() {
            if line.starts_with(':') && (line.ends_with(" + \\") || line.ends_with(" \\")) {
                let mut attribute_content = String::with_capacity(line.len() * 2);
                if line.ends_with(" + \\") {
                    attribute_content.push_str(line);
                    attribute_content.push('\n');
                } else if line.ends_with(" \\") {
                    attribute_content.push_str(line.trim_end_matches('\\'));
                }
                Self::process_continuation(&mut attribute_content, &mut lines, &mut line_number);
                attribute::parse_line(&mut options.document_attributes, attribute_content.as_str());
                out.push_line(attribute_content);
                continue;
            } else if line.starts_with(':') {
                attribute::parse_line(&mut options.document_attributes, line.trim());
            }
            if let Some(delimiter_type) = Self::is_verbatim_delimiter(line) {
                if in_verbatim_block && Some(delimiter_type) == current_delimiter {
                    tracing::trace!(?delimiter_type, "Closing verbatim block");
                    in_verbatim_block = false;
                    current_delimiter = None;
                } else if !in_verbatim_block {
                    tracing::trace!(?delimiter_type, "Opening verbatim block");
                    in_verbatim_block = true;
                    current_delimiter = Some(delimiter_type);
                }
                out.push_line(line.to_string());
            } else if line.starts_with("//") {
                out.push_line(line.to_string());
            } else if line.ends_with(']') && !line.starts_with('[') && line.contains("::") {
                let mut ctx = DirectiveContext {
                    line_number: &mut line_number,
                    current_offset,
                    file_parent,
                };
                self.process_directive_line(line, &mut lines, &mut ctx, &options, &mut out)?;
            } else {
                out.push_line(line.to_string());
            }
            current_offset += line.len() + 1;
            line_number += 1;
        }

        Ok(PreprocessorResult {
            text: Cow::Owned(out.lines.join("\n")),
            leveloffset_ranges: out.leveloffset_ranges,
            source_ranges: out.source_ranges,
            include_expansions: out.include_expansions,
            conditional_drops: out.conditional_drops,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process() -> Result<(), Error> {
        let options = Options::default();
        let input = ":attribute: value

ifdef::attribute[]
content
endif::[]
";
        let result = Preprocessor::process(input, &options, Rc::default())?;
        assert_eq!(result.text, ":attribute: value\n\ncontent\n");
        Ok(())
    }

    #[test]
    fn test_good_endif_directive() -> Result<(), Error> {
        let options = Options::default();
        let input = ":asdf:

ifdef::asdf[]
content
endif::asdf[]";
        let result = Preprocessor::process(input, &options, Rc::default())?;
        assert_eq!(result.text, ":asdf:\n\ncontent\n");
        Ok(())
    }

    #[test]
    fn test_bad_endif_directive() {
        let options = Options::default();
        let input = "ifdef::asdf[]
content
endif::another[]";
        let output = Preprocessor::process(input, &options, Rc::default());
        assert!(matches!(
            output,
            Err(Error::InvalidConditionalDirective(..))
        ));
    }

    #[test]
    fn test_utf8_bom_detection() -> Result<(), Error> {
        let path = Path::new("fixtures/preprocessor/utf8_bom.adoc");
        let content = read_and_decode_file(path, None, None)?;

        // Should contain the test content without BOM
        assert!(content.contains("= Test Document"));
        assert!(content.contains("This is a test with special chars: é, ñ, ü."));
        // BOM should be stripped
        assert!(!content.starts_with('\u{FEFF}'));
        Ok(())
    }

    #[test]
    fn test_utf16le_bom_detection() -> Result<(), Error> {
        let path = Path::new("fixtures/preprocessor/utf16le_bom.adoc");
        let content = read_and_decode_file(path, None, None)?;

        // Should correctly decode UTF-16 LE content
        assert!(content.contains("= Test Document"));
        assert!(content.contains("This is a test with special chars: é, ñ, ü."));
        Ok(())
    }

    #[test]
    fn test_utf16be_bom_detection() -> Result<(), Error> {
        let path = Path::new("fixtures/preprocessor/utf16be_bom.adoc");
        let content = read_and_decode_file(path, None, None)?;

        // Should correctly decode UTF-16 BE content
        assert!(content.contains("= Test Document"));
        assert!(content.contains("This is a test with special chars: é, ñ, ü."));
        Ok(())
    }

    #[test]
    fn test_utf8_no_bom() -> Result<(), Error> {
        let path = Path::new("fixtures/preprocessor/utf8_no_bom.adoc");
        let content = read_and_decode_file(path, None, None)?;

        // Should decode regular UTF-8 file
        assert!(content.contains("= Test Document"));
        assert!(content.contains("This is a test with special chars: é, ñ, ü."));
        Ok(())
    }

    #[test]
    fn test_explicit_encoding_override() -> Result<(), Error> {
        // Test that explicit encoding parameter works
        let path = Path::new("fixtures/preprocessor/utf8_no_bom.adoc");
        let content = read_and_decode_file(path, Some("utf-8"), None)?;

        assert!(content.contains("= Test Document"));
        Ok(())
    }

    #[test]
    fn test_unknown_encoding_error() {
        let path = Path::new("fixtures/preprocessor/utf8_no_bom.adoc");
        let result = read_and_decode_file(path, Some("unknown-encoding-12345"), None);

        assert!(matches!(result, Err(Error::UnknownEncoding(_))));
    }

    #[test]
    fn test_include_utf16_file() -> Result<(), Error> {
        // Test that include directive works with UTF-16 LE files
        let warnings = Rc::<RefCell<Vec<Warning>>>::default();
        let path = Path::new("fixtures/preprocessor/main_with_include.adoc");
        let options = Options::default();

        let result = Preprocessor::process_file(path, &options, warnings)?;

        // Should contain content from both main file and included UTF-16 file
        assert!(result.text.contains("= Main Document"));
        assert!(result.text.contains("This is included content."));
        assert!(result.text.contains("With special characters: é, ñ, ü."));
        assert!(result.text.contains("After include."));
        Ok(())
    }

    #[test]
    fn test_include_with_single_tag() -> Result<(), Error> {
        let warnings = Rc::<RefCell<Vec<Warning>>>::default();
        let path = Path::new("fixtures/preprocessor/include_with_tag.adoc");
        let options = Options::default();

        let result = Preprocessor::process_file(path, &options, warnings)?;

        // Should contain the intro tag content
        assert!(result.text.contains("This is the introduction."));
        assert!(result.text.contains("It has multiple lines."));
        // Should NOT contain other content
        assert!(!result.text.contains("untagged content"));
        assert!(!result.text.contains("main content"));
        assert!(!result.text.contains("Debug information"));
        // Should NOT contain tag directives
        assert!(!result.text.contains("tag::intro"));
        assert!(!result.text.contains("end::intro"));
        Ok(())
    }

    #[test]
    fn test_include_with_multiple_tags() -> Result<(), Error> {
        let warnings = Rc::<RefCell<Vec<Warning>>>::default();
        let path = Path::new("fixtures/preprocessor/include_multiple_tags.adoc");
        let options = Options::default();

        let result = Preprocessor::process_file(path, &options, warnings)?;

        // Should contain both intro and main content
        assert!(result.text.contains("This is the introduction."));
        assert!(result.text.contains("This is the main content."));
        // Should NOT contain debug or untagged content
        assert!(!result.text.contains("Debug information"));
        Ok(())
    }

    #[test]
    fn test_include_with_wildcard_excluding_tag() -> Result<(), Error> {
        let warnings = Rc::<RefCell<Vec<Warning>>>::default();
        let path = Path::new("fixtures/preprocessor/include_wildcard_exclude.adoc");
        let options = Options::default();

        let result = Preprocessor::process_file(path, &options, warnings)?;

        // Should contain intro and main content
        assert!(result.text.contains("This is the introduction."));
        assert!(result.text.contains("This is the main content."));
        // Should NOT contain debug content
        assert!(!result.text.contains("Debug information"));
        Ok(())
    }

    #[test]
    fn test_include_with_double_wildcard() -> Result<(), Error> {
        let warnings = Rc::<RefCell<Vec<Warning>>>::default();
        let path = Path::new("fixtures/preprocessor/include_double_wildcard.adoc");
        let options = Options::default();

        let result = Preprocessor::process_file(path, &options, warnings)?;

        // Should contain all content except tag directive lines
        assert!(result.text.contains("untagged content"));
        assert!(result.text.contains("This is the introduction."));
        assert!(result.text.contains("This is the main content."));
        assert!(result.text.contains("Debug information"));
        // Should NOT contain tag directives
        assert!(!result.text.contains("tag::intro"));
        assert!(!result.text.contains("end::intro"));
        Ok(())
    }

    #[test]
    fn test_include_with_nested_tag() -> Result<(), Error> {
        let warnings = Rc::<RefCell<Vec<Warning>>>::default();
        let path = Path::new("fixtures/preprocessor/include_nested_tag.adoc");
        let options = Options::default();

        let result = Preprocessor::process_file(path, &options, warnings)?;

        // Should contain only the nested content
        assert!(result.text.contains("This is nested within main."));
        // Should NOT contain main content outside nested
        assert!(!result.text.contains("This is the main content."));
        assert!(!result.text.contains("Back to main content."));
        Ok(())
    }

    #[test]
    fn test_include_select_untagged_only() -> Result<(), Error> {
        let warnings = Rc::<RefCell<Vec<Warning>>>::default();
        let path = Path::new("fixtures/preprocessor/include_untagged_only.adoc");
        let options = Options::default();

        let result = Preprocessor::process_file(path, &options, warnings)?;

        // Should contain only untagged content
        assert!(result.text.contains("untagged content at the beginning"));
        assert!(result.text.contains("More untagged content"));
        assert!(result.text.contains("Final untagged content"));
        // Should NOT contain any tagged content
        assert!(!result.text.contains("This is the introduction"));
        assert!(!result.text.contains("This is the main content"));
        assert!(!result.text.contains("Debug information"));
        Ok(())
    }

    #[test]
    fn test_include_tag_with_lines() -> Result<(), Error> {
        let warnings = Rc::<RefCell<Vec<Warning>>>::default();
        let path = Path::new("fixtures/preprocessor/include_tag_with_lines.adoc");
        let options = Options::default();

        let result = Preprocessor::process_file(path, &options, warnings)?;

        // When combining tag= and lines=, the lines= attribute refers to
        // line numbers in the ORIGINAL file, not the filtered result.
        // tag=intro selects lines 4-5 (content between tag directives)
        // lines=4 selects only line 4 from the original file
        // The intersection is just line 4: "This is the introduction."
        assert!(result.text.contains("This is the introduction."));
        // Line 5 is not in lines=4, so it should NOT be included
        assert!(!result.text.contains("It has multiple lines."));
        Ok(())
    }

    #[test]
    fn test_include_with_indent() -> Result<(), Error> {
        let warnings = Rc::<RefCell<Vec<Warning>>>::default();
        let path = Path::new("fixtures/preprocessor/include_with_indent.adoc");
        let options = Options::default();

        let result = Preprocessor::process_file(path, &options, warnings)?;

        // min indent is 0, so indent=4 adds 4 spaces preserving relative indentation
        assert!(result.text.contains("    def hello"));
        assert!(result.text.contains("      puts \"Hello\""));
        assert!(result.text.contains("    end"));
        Ok(())
    }

    #[test]
    fn test_include_with_indent_zero() -> Result<(), Error> {
        let warnings = Rc::<RefCell<Vec<Warning>>>::default();
        let path = Path::new("fixtures/preprocessor/include_with_indent_zero.adoc");
        let options = Options::default();

        let result = Preprocessor::process_file(path, &options, warnings)?;

        // min indent is 0, so indent=0 leaves content unchanged
        assert!(result.text.contains("def hello"));
        assert!(result.text.contains("  puts \"Hello\""));
        assert!(result.text.contains("end"));
        Ok(())
    }

    #[test]
    fn test_include_with_indent_and_tag() -> Result<(), Error> {
        let warnings = Rc::<RefCell<Vec<Warning>>>::default();
        let path = Path::new("fixtures/preprocessor/include_with_indent_and_tag.adoc");
        let options = Options::default();

        let result = Preprocessor::process_file(path, &options, warnings)?;

        // Should contain intro tag content, indented by 2 spaces
        assert!(result.text.contains("  This is the introduction."));
        assert!(result.text.contains("  It has multiple lines."));
        // Should NOT contain other content
        assert!(!result.text.contains("main content"));
        assert!(!result.text.contains("Debug information"));
        Ok(())
    }

    #[test]
    fn test_nested_include_relative_paths() -> Result<(), Error> {
        // Tests that nested includes resolve paths relative to their parent file.
        // Structure:
        //   nested_include_main.adoc
        //     -> includes subdir/middle.adoc
        //         -> includes inner.adoc (relative to subdir/)
        let warnings = Rc::<RefCell<Vec<Warning>>>::default();
        let path = Path::new("fixtures/preprocessor/nested_include_main.adoc");
        let options = Options::default();

        let result = Preprocessor::process_file(path, &options, warnings)?;

        // Should contain content from main file
        assert!(result.text.contains("= Nested Include Test"));
        // Should contain content from subdir/middle.adoc
        assert!(result.text.contains("This is middle content."));
        // Should contain content from subdir/inner.adoc (resolved relative to subdir/)
        assert!(
            result.text.contains("This is inner content from subdir."),
            "Nested include failed to resolve relative path. Got: {}",
            result.text
        );
        Ok(())
    }
}
