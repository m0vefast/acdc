//! Pluggable file reader for the include preprocessor.
//!
//! Default (native, `std::fs`) behavior is preserved by `DefaultFileResolver`.
//! WASM and other sandboxed embeddings can inject their own resolver (e.g.
//! reading from a JS-side vault cache) so `include::` directives work without
//! native file I/O.
//!
//! Added 2026-05-17 for the Glyph editor's WASM build — `wasm32-unknown-
//! unknown` has no `std::fs`, so the include preprocessor would otherwise
//! silently drop every directive. With this trait, the host JS provides a
//! callback that returns raw file bytes from the vault, and acdc's full
//! Asciidoctor-spec include semantics (lines/tag/leveloffset/indent/nested)
//! work end-to-end.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Errors a `FileResolver` may return.
///
/// Two layers of `#[non_exhaustive]` are deliberate, not redundant:
/// - **Enum-level** `#[non_exhaustive]` blocks external exhaustive matches
///   (downstream must use a wildcard arm) so we can add new variants
///   without a major bump.
/// - **Variant-level** `#[non_exhaustive]` blocks external struct-variant
///   destructuring so we can add new fields to `NotFound` / `Io` without
///   a major bump. External `match X::Io { path, source }` must use `..`.
///
/// External pattern-matching cost: downstream consumers can extract the path
/// via `e.path()` and walk the error chain via `std::error::Error::source`
/// without needing to destructure the variant.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FileResolverError {
    /// Target file does not exist or is not readable. Callers should treat
    /// this as a non-fatal "missing include" and emit a warning rather than
    /// aborting parsing.
    #[error("file not found: {}", path.display())]
    #[non_exhaustive]
    NotFound { path: PathBuf },

    /// Resolver-defined I/O error (permission denied, network failure for a
    /// URL-backed resolver, JS callback threw, etc.). Carries the source
    /// chain so callers can introspect via `std::error::Error::source`.
    #[error("file read error for {}: {source}", path.display())]
    #[non_exhaustive]
    Io {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl FileResolverError {
    /// Convenience constructor for the common case where the resolver only
    /// has a path-like string (e.g. wasm JS callback). Avoids `PathBuf`
    /// allocation friction at simple call sites.
    #[must_use]
    pub fn not_found<P: Into<PathBuf>>(path: P) -> Self {
        Self::NotFound { path: path.into() }
    }

    /// Convenience constructor wrapping any error type into the `Io` variant.
    #[must_use]
    pub fn io<P: Into<PathBuf>, E: std::error::Error + Send + Sync + 'static>(
        path: P,
        source: E,
    ) -> Self {
        Self::Io {
            path: path.into(),
            source: Box::new(source),
        }
    }

    /// The path that the resolver was asked to read, for both variants.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::NotFound { path } | Self::Io { path, .. } => path,
        }
    }
}

/// Pluggable file content provider for the include preprocessor.
///
/// # Thread safety
///
/// Implementations must be `Send + Sync`. The bound exists because the
/// parser caches an empty `Options<'static>` in a `LazyLock` (see
/// `grammar/state::new_quotes_only`) which needs `Options: Sync`, which
/// transitively requires the resolver to be `Sync`. WASM embedders whose
/// handle is `!Send + !Sync` (e.g. `js_sys::Function`) must declare
/// `unsafe impl Send + Sync` — sound on the default `wasm32-unknown-unknown`
/// target which is single-threaded, but read the comment on `JsFileResolver`
/// in `acdc-glyph-wasm` before doing this on a threaded wasm build.
pub trait FileResolver: Send + Sync {
    /// Read the raw bytes at `path`. The preprocessor handles BOM detection
    /// and encoding decoding downstream — return unmodified bytes from the
    /// underlying source (filesystem, vault cache, archive entry, …).
    ///
    /// Returning `Cow<'_, [u8]>` lets in-memory resolvers (the common WASM
    /// case) avoid an allocation on every include, while leaving
    /// filesystem-backed resolvers free to return `Cow::Owned`.
    ///
    /// # Errors
    /// Return `FileResolverError::NotFound` for missing files (preprocessor
    /// will emit a warning and continue). Return `FileResolverError::Io` for
    /// transient/system errors (permission denied, JS callback threw, etc.);
    /// the preprocessor surfaces the source error in its warning message.
    fn read(&self, path: &Path) -> Result<Cow<'_, [u8]>, FileResolverError>;
}

/// Default resolver: uses `std::fs::read`. Available only on targets where
/// `std::fs` works (i.e. NOT `wasm32-unknown-unknown`). For wasm builds, the
/// embedding MUST inject a resolver — `Options::file_resolver` is `None` by
/// default and the preprocessor falls back to direct `std::fs::read` only on
/// non-wasm targets via the `cfg(not(target_arch = "wasm32"))` gate in
/// `read_and_decode_file`.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultFileResolver;

#[cfg(not(target_arch = "wasm32"))]
impl FileResolver for DefaultFileResolver {
    fn read(&self, path: &Path) -> Result<Cow<'_, [u8]>, FileResolverError> {
        // `std::io::ErrorKind` is itself `#[non_exhaustive]` and has dozens
        // of variants; we genuinely only special-case NotFound and route
        // everything else (PermissionDenied, Interrupted, …) through Io.
        #[allow(clippy::wildcard_enum_match_arm)]
        std::fs::read(path).map(Cow::Owned).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => FileResolverError::NotFound {
                path: path.to_path_buf(),
            },
            _ => FileResolverError::Io {
                path: path.to_path_buf(),
                source: Box::new(e),
            },
        })
    }
}

/// Helper newtype so `Options` can derive `Debug` despite holding a trait
/// object. Prints as `<FileResolver>` to avoid leaking implementation
/// internals.
///
/// Construct via [`DynFileResolver::new`] — the inner `Arc` is private
/// and there is intentionally no public `From<Arc<...>>` impl, so future
/// per-instance invariants (validation, instrumentation) added to `new`
/// cannot be bypassed.
///
/// Cloning a `DynFileResolver` (and therefore cloning an `Options` holding
/// one) shares the underlying resolver via `Arc` — stateful resolvers
/// (caches, counters) see calls from all clones.
#[derive(Clone)]
pub struct DynFileResolver(Arc<dyn FileResolver + Send + Sync>);

impl fmt::Debug for DynFileResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<FileResolver>")
    }
}

impl DynFileResolver {
    /// Wrap a concrete resolver. The only public constructor.
    #[must_use]
    pub fn new<R: FileResolver + 'static>(resolver: R) -> Self {
        Self(Arc::new(resolver))
    }

    /// Read bytes via the wrapped resolver.
    ///
    /// # Errors
    /// Propagates whatever the inner resolver returned.
    pub fn read(&self, path: &Path) -> Result<Cow<'_, [u8]>, FileResolverError> {
        self.0.read(path)
    }
}
