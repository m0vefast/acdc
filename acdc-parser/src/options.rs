use std::borrow::Cow;

pub use crate::safe_mode::SafeMode;

use crate::file_resolver::DynFileResolver;
use crate::{AttributeValue, DocumentAttributes};

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Options<'a> {
    pub safe_mode: SafeMode,
    pub timings: bool,
    pub document_attributes: DocumentAttributes<'a>,
    /// Strict mode - fail on non-conformance instead of warn-and-continue.
    ///
    /// When enabled, issues that would normally result in a warning and fallback
    /// behavior will instead cause parsing to fail. For example:
    /// - Non-conforming manpage titles (not matching `name(volume)` format)
    pub strict: bool,
    /// Enable Setext-style (underlined) header parsing.
    ///
    /// When enabled, headers can use the legacy two-line syntax:
    /// ```text
    /// Document Title
    /// ==============
    /// ```
    #[cfg(feature = "setext")]
    pub setext: bool,
    /// Pluggable file reader for the include preprocessor. `None` falls back
    /// to `std::fs::read` on native targets; WASM embeddings MUST set one
    /// (otherwise every `include::` silently no-ops because wasm32-unknown-
    /// unknown has no `std::fs`). See `crate::file_resolver::FileResolver`.
    pub file_resolver: Option<DynFileResolver>,
    /// Virtual current-file path for include resolution when calling `parse`
    /// (not `parse_file`). Relative include targets (`include::ch1.adoc[]`)
    /// resolve against this path's parent directory. Required when using a
    /// `file_resolver` from WASM, where there's no real `std::fs` path.
    /// Defaults to `None`, which disables include processing for `parse`
    /// callers without a file path (matches pre-resolver behavior).
    pub virtual_current_file: Option<std::path::PathBuf>,
}

impl<'a> Options<'a> {
    /// Create a new `OptionsBuilder` for fluent configuration.
    ///
    /// # Example
    ///
    /// ```
    /// use acdc_parser::{Options, SafeMode};
    ///
    /// let options = Options::builder()
    ///     .with_safe_mode(SafeMode::Safe)
    ///     .with_timings()
    ///     .with_attribute("toc", "left")
    ///     .build();
    /// ```
    #[must_use]
    pub fn builder() -> OptionsBuilder<'a> {
        OptionsBuilder::default()
    }

    /// Create a new `Options` with default settings.
    ///
    /// Equivalent to `Options::default()`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new `Options` with the given document attributes.
    ///
    /// # Example
    ///
    /// ```
    /// use acdc_parser::{Options, DocumentAttributes, AttributeValue};
    ///
    /// let mut attrs = DocumentAttributes::default();
    /// attrs.insert("toc".into(), AttributeValue::String("left".into()));
    ///
    /// let options = Options::with_attributes(attrs);
    /// ```
    #[must_use]
    pub fn with_attributes(document_attributes: DocumentAttributes<'a>) -> Self {
        Self {
            document_attributes,
            ..Default::default()
        }
    }

    /// Consume the options, producing an independent `'static` copy.
    #[must_use]
    pub fn into_static(self) -> Options<'static> {
        Options {
            safe_mode: self.safe_mode,
            timings: self.timings,
            document_attributes: self.document_attributes.into_static(),
            strict: self.strict,
            #[cfg(feature = "setext")]
            setext: self.setext,
            file_resolver: self.file_resolver,
            virtual_current_file: self.virtual_current_file,
        }
    }

    /// Inject Asciidoctor-style runtime built-in attributes into
    /// `document_attributes` based on the current configuration.
    ///
    /// Adds:
    /// - `safe-mode-name`, `safe-mode-level`, and the matching `safe-mode-{name}` marker
    /// - `asciidoctor`, `asciidoctor-version`
    /// - `backend`, `backend-html5`, `basebackend`, `basebackend-html`,
    ///   `filetype`, `filetype-html` (acdc currently only emits HTML5 from this path)
    /// - `doctype`, `doctype-article` (default doctype)
    ///
    /// User-provided attributes (set explicitly via the builder or
    /// `with_attribute*`) are preserved — built-ins use `insert_default`,
    /// which is a no-op if the key already exists and never marks the entry
    /// as `explicit` (so they stay out of serialized ASG output).
    ///
    /// Idempotent: calling more than once is harmless.
    #[must_use]
    pub fn with_runtime_builtins(mut self) -> Self {
        let (safe_name, safe_level): (&'static str, &'static str) = match self.safe_mode {
            SafeMode::Unsafe => ("unsafe", "0"),
            SafeMode::Safe => ("safe", "1"),
            SafeMode::Server => ("server", "10"),
            SafeMode::Secure => ("secure", "20"),
        };
        let attrs = &mut self.document_attributes;
        let str_val = |s: &'static str| AttributeValue::String(Cow::Borrowed(s));
        // Safe-mode trio + marker
        attrs.insert_default(Cow::Borrowed("safe-mode-name"), str_val(safe_name));
        attrs.insert_default(Cow::Borrowed("safe-mode-level"), str_val(safe_level));
        attrs.insert_default(
            Cow::Borrowed(match self.safe_mode {
                SafeMode::Unsafe => "safe-mode-unsafe",
                SafeMode::Safe => "safe-mode-safe",
                SafeMode::Server => "safe-mode-server",
                SafeMode::Secure => "safe-mode-secure",
            }),
            str_val(""),
        );
        // Implementation identity. Version intentionally tagged so docs that
        // gate on Asciidoctor version don't accidentally treat acdc as a
        // specific Asciidoctor release.
        attrs.insert_default(Cow::Borrowed("asciidoctor"), str_val(""));
        attrs.insert_default(
            Cow::Borrowed("asciidoctor-version"),
            str_val(concat!(env!("CARGO_PKG_VERSION"), "-acdc")),
        );
        // Backend / filetype / doctype defaults — currently html5/article only.
        attrs.insert_default(Cow::Borrowed("backend"), str_val("html5"));
        attrs.insert_default(Cow::Borrowed("backend-html5"), str_val(""));
        attrs.insert_default(Cow::Borrowed("basebackend"), str_val("html"));
        attrs.insert_default(Cow::Borrowed("basebackend-html"), str_val(""));
        attrs.insert_default(Cow::Borrowed("filetype"), str_val("html"));
        attrs.insert_default(Cow::Borrowed("filetype-html"), str_val(""));
        attrs.insert_default(Cow::Borrowed("doctype"), str_val("article"));
        attrs.insert_default(Cow::Borrowed("doctype-article"), str_val(""));
        self
    }
}

/// Builder for `Options` that provides an API for configuration.
///
/// Create an `OptionsBuilder` using `Options::builder()`.
///
/// # Example
///
/// ```
/// use acdc_parser::{Options, SafeMode};
///
/// let options = Options::builder()
///     .with_safe_mode(SafeMode::Safe)
///     .with_timings()
///     .with_attribute("toc", "left")
///     .with_attribute("sectnums", true)
///     .build();
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct OptionsBuilder<'a> {
    safe_mode: SafeMode,
    timings: bool,
    document_attributes: DocumentAttributes<'a>,
    strict: bool,
    #[cfg(feature = "setext")]
    setext: bool,
    file_resolver: Option<DynFileResolver>,
    virtual_current_file: Option<std::path::PathBuf>,
}

impl<'a> OptionsBuilder<'a> {
    /// Set the safe mode for parsing.
    ///
    /// # Example
    ///
    /// ```
    /// use acdc_parser::{Options, SafeMode};
    ///
    /// let options = Options::builder()
    ///     .with_safe_mode(SafeMode::Safe)
    ///     .build();
    /// ```
    #[must_use]
    pub fn with_safe_mode(mut self, safe_mode: SafeMode) -> Self {
        self.safe_mode = safe_mode;
        self
    }

    /// Enable timing information during parsing.
    ///
    /// # Example
    ///
    /// ```
    /// use acdc_parser::Options;
    ///
    /// let options = Options::builder()
    ///     .with_timings()
    ///     .build();
    /// ```
    #[must_use]
    pub fn with_timings(mut self) -> Self {
        self.timings = true;
        self
    }

    /// Enable strict mode.
    ///
    /// When enabled, issues that would normally result in a warning and fallback
    /// behavior will instead cause parsing to fail.
    ///
    /// # Example
    ///
    /// ```
    /// use acdc_parser::Options;
    ///
    /// let options = Options::builder()
    ///     .with_strict()
    ///     .build();
    /// ```
    #[must_use]
    pub fn with_strict(mut self) -> Self {
        self.strict = true;
        self
    }

    /// Add a document attribute with a string value.
    ///
    /// This is a convenience method that accepts various types for the value:
    /// - `&str` becomes `AttributeValue::String`
    /// - `bool` becomes `AttributeValue::Bool`
    /// - `()` becomes `AttributeValue::None`
    ///
    /// # Example
    ///
    /// ```
    /// use acdc_parser::Options;
    ///
    /// let options = Options::builder()
    ///     .with_attribute("toc", "left")
    ///     .with_attribute("sectnums", true)
    ///     .build();
    /// ```
    #[must_use]
    pub fn with_attribute(
        mut self,
        name: impl Into<Cow<'a, str>>,
        value: impl Into<AttributeValue<'a>>,
    ) -> Self {
        self.document_attributes.insert(name.into(), value.into());
        self
    }

    /// Set all document attributes at once.
    ///
    /// # Example
    ///
    /// ```
    /// use acdc_parser::{Options, DocumentAttributes, AttributeValue};
    ///
    /// let mut attrs = DocumentAttributes::default();
    /// attrs.insert("toc".into(), AttributeValue::String("left".into()));
    ///
    /// let options = Options::builder()
    ///     .with_attributes(attrs)
    ///     .build();
    /// ```
    #[must_use]
    pub fn with_attributes(mut self, document_attributes: DocumentAttributes<'a>) -> Self {
        self.document_attributes = document_attributes;
        self
    }

    /// Enable Setext-style (underlined) header parsing.
    ///
    /// When enabled, headers can use the legacy two-line syntax where
    /// the title is underlined with `=`, `-`, `~`, `^`, or `+` characters.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use acdc_parser::Options;
    ///
    /// let options = Options::builder()
    ///     .with_setext()
    ///     .build();
    /// ```
    #[cfg(feature = "setext")]
    #[must_use]
    pub fn with_setext(mut self) -> Self {
        self.setext = true;
        self
    }

    /// Install a custom file reader for the include preprocessor.
    ///
    /// Native targets get `std::fs::read` by default. WASM embeddings MUST
    /// supply a resolver — `wasm32-unknown-unknown` has no `std::fs`, so
    /// without one, every `include::file.adoc[]` directive silently no-ops.
    ///
    /// When using a resolver, also set [`with_virtual_current_file`] so
    /// relative include targets have a parent directory to anchor against.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::borrow::Cow;
    /// use std::collections::HashMap;
    /// use std::path::{Path, PathBuf};
    /// use acdc_parser::{DynFileResolver, FileResolver, FileResolverError, Options};
    ///
    /// struct InMemoryFiles(HashMap<PathBuf, Vec<u8>>);
    /// impl FileResolver for InMemoryFiles {
    ///     fn read(&self, path: &Path) -> Result<Cow<'_, [u8]>, FileResolverError> {
    ///         match self.0.get(path) {
    ///             Some(bytes) => Ok(Cow::Borrowed(bytes)),
    ///             None => Err(FileResolverError::not_found(path)),
    ///         }
    ///     }
    /// }
    ///
    /// let mut files = HashMap::new();
    /// files.insert(PathBuf::from("ch1.adoc"), b"== Chapter 1\n".to_vec());
    /// let options = Options::builder()
    ///     .with_file_resolver(DynFileResolver::new(InMemoryFiles(files)))
    ///     .with_virtual_current_file("main.adoc")
    ///     .build();
    /// ```
    ///
    /// [`with_virtual_current_file`]: Self::with_virtual_current_file
    #[must_use]
    pub fn with_file_resolver(mut self, resolver: DynFileResolver) -> Self {
        self.file_resolver = Some(resolver);
        self
    }

    /// Set a virtual current-file path used to resolve relative `include::`
    /// targets when calling `parse` (not `parse_file`). Required when using
    /// a `file_resolver` from WASM. The file doesn't need to physically
    /// exist — only its parent dir is read, to anchor relative includes.
    #[must_use]
    pub fn with_virtual_current_file<P: Into<std::path::PathBuf>>(
        mut self,
        path: P,
    ) -> Self {
        self.virtual_current_file = Some(path.into());
        self
    }

    /// Build the `Options` from this builder.
    ///
    /// # Example
    ///
    /// ```
    /// use acdc_parser::{Options, SafeMode};
    ///
    /// let options = Options::builder()
    ///     .with_safe_mode(SafeMode::Safe)
    ///     .build();
    /// ```
    #[must_use]
    pub fn build(self) -> Options<'a> {
        Options {
            safe_mode: self.safe_mode,
            timings: self.timings,
            document_attributes: self.document_attributes,
            strict: self.strict,
            #[cfg(feature = "setext")]
            setext: self.setext,
            file_resolver: self.file_resolver,
            virtual_current_file: self.virtual_current_file,
        }
    }
}
