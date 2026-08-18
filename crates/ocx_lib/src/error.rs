// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A network operation was attempted while in offline mode.
    #[error("network operation attempted in offline mode")]
    OfflineMode,

    /// A whole verb was refused because its purpose is discovering new
    /// tag→digest bindings and the active policy forbids that.
    ///
    /// Raised by `ocx index update` under `--frozen` — the package tier's own
    /// discovery verb, and the tier `--frozen` scopes to. `policy` is the
    /// lowercase flag label, matching
    /// [`crate::project::error::ProjectErrorKind::PolicyBlocked`].
    #[error("{operation} discovers new digests and cannot run in {policy} mode; re-run it without --{policy}")]
    PolicyBlocked {
        /// The refused verb, written the way a user invokes it.
        operation: &'static str,
        /// The lowercase flag label that blocked the verb.
        policy: &'static str,
    },

    /// A file I/O error with path context.
    ///
    /// The io cause is carried by `#[source]` alone — interpolating it into the
    /// message too would print it twice in a `{err:#}` chain.
    #[error("internal file error for '{path}'", path = .0.display())]
    InternalFile(std::path::PathBuf, #[source] std::io::Error),

    /// A per-layer layout annotation read from a manifest layer descriptor could
    /// not be resolved into a placement.
    ///
    /// Carries the [`LayerLayoutError`](crate::oci::LayerLayoutError) cause via
    /// `#[source]` so exit-code classification descends the chain and reaches it
    /// (→ `DataError` 65 for a malformed/hostile manifest annotation), instead
    /// of the generic `IoError` (74) that wrapping in [`Self::InternalFile`]
    /// would force. `io::Error::source` skips a boxed inner error, so the layout
    /// cause must be carried as a first-class `#[source]` field here, not
    /// smuggled through `io::Error::other`.
    #[error("layer layout resolution failed")]
    LayerLayout(#[source] crate::oci::layer_layout::LayerLayoutError),

    /// A destination-path symlink-safety check failed while assembling package
    /// content — an untrusted output prefix resolved through a symlink.
    ///
    /// Transparent so classification delegates to the inner
    /// [`SymlinkWalkError`](crate::utility::fs::SymlinkWalkError) (an ancestor
    /// symlink is a `UsageError` 64, an I/O failure is `IoError` 74), instead of
    /// the flat `IoError` (74) that wrapping in [`Self::InternalFile`] forced.
    #[error(transparent)]
    SymlinkWalk(crate::utility::fs::SymlinkWalkError),
    /// A path has an unexpected structure.
    #[error("path '{}' has an unexpected structure", .0.display())]
    InternalPathInvalid(std::path::PathBuf),

    /// JSON serialization or deserialization failed.
    #[error("JSON serialization error")]
    SerializationFailure(#[from] serde_json::Error),

    /// An unsupported OCI media type was encountered.
    #[error("unsupported media type '{media_type}', expected media types are: {supported}", media_type = .0, supported = .1.join(", "))]
    UnsupportedMediaType(String, &'static [&'static str]),

    /// A metadata config blob exceeded the size cap enforced by
    /// `package_manager::tasks::common::load_config_metadata`, either by its
    /// declared descriptor size (checked before any blob fetch) or its
    /// actual fetched byte length (checked after fetch, defending against a
    /// registry that declares a small size but serves a larger body). See
    /// `adr_inspect_metadata_closure.md` D5.
    #[error("metadata blob size {size} bytes exceeds the {max}-byte cap")]
    MetadataBlobTooLarge { size: i64, max: usize },

    /// An authentication operation failed.
    #[error(transparent)]
    Auth(#[from] crate::auth::error::AuthError),
    /// A platform parsing or validation error.
    #[error(transparent)]
    Platform(#[from] crate::oci::platform::error::PlatformError),
    /// A project-tier configuration or lock operation failed.
    #[error(transparent)]
    Project(#[from] crate::project::error::Error),
    /// A project GC-ledger (flat symlink store) operation failed.
    #[error(transparent)]
    ProjectRegistry(#[from] crate::project::registry::error::Error),
    /// A package manager operation failed.
    #[error(transparent)]
    PackageManager(#[from] crate::package_manager::error::Error),
    /// An OCI client operation failed.
    #[error(transparent)]
    OciClient(#[from] crate::oci::client::error::ClientError),
    /// An OCI identifier could not be parsed.
    #[error(transparent)]
    Identifier(#[from] crate::oci::identifier::error::IdentifierError),
    /// An archive operation failed.
    #[error(transparent)]
    Archive(#[from] crate::archive::Error),
    /// A compression or decompression operation failed.
    #[error(transparent)]
    Compression(#[from] crate::compression::error::Error),
    /// A CI export operation failed.
    #[error(transparent)]
    Ci(#[from] crate::ci::error::Error),
    /// A configuration error occurred.
    #[error(transparent)]
    Config(#[from] crate::config::error::Error),
    /// A package operation failed.
    #[error(transparent)]
    Package(Box<crate::package::error::Error>),
    /// A shell operation failed.
    #[error(transparent)]
    Shell(#[from] crate::shell::error::Error),
    /// An OCI index operation failed.
    #[error(transparent)]
    OciIndex(#[from] crate::oci::index::error::Error),
    /// A file structure operation failed.
    #[error(transparent)]
    FileStructure(#[from] crate::file_structure::error::Error),
    /// A digest string could not be parsed.
    #[error(transparent)]
    Digest(#[from] crate::oci::digest::error::DigestError),
    /// A patch-domain operation failed outside the per-package discovery path
    /// (which carries its own `PatchError` through `PackageErrorKind`).
    ///
    /// Boxed because `PatchError::BlobWriteFailed` carries a `crate::Error`
    /// back, and the two enums would otherwise be mutually infinite. Same
    /// shape as [`Self::Package`], including the hand-written `From`.
    #[error(transparent)]
    Patch(Box<crate::patch::PatchError>),

    /// A dependency graph operation failed.
    #[error(transparent)]
    Dependency(#[from] crate::package_manager::DependencyError),
    /// A pinned identifier validation failed.
    #[error(transparent)]
    PinnedIdentifier(#[from] crate::oci::pinned_identifier::PinnedIdentifierError),

    /// A singleflight coordination error (leader failure, abandonment, timeout,
    /// or capacity exceeded).
    #[error("singleflight coordination failed")]
    Singleflight(#[from] crate::utility::singleflight::Error),

    /// A string baked into an install-time launcher contains a character that
    /// cannot be safely embedded in the Unix `.sh` template or the Windows
    /// `.shim` sidecar (single-quote, percent, double-quote, NUL, CR, LF).
    /// The unsafe set is owned by `crate::package_manager::launcher`.
    #[error("launcher-unsafe character {character:?} in {value:?}; {}", launcher_unsafe_hint(*character))]
    LauncherUnsafeCharacter { value: String, character: char },

    /// An OCI signing operation failed.
    ///
    /// Boxed because [`crate::oci::sign::SignError`] carries a full
    /// [`crate::oci::Identifier`] plus a kind enum — materializing it
    /// unboxed bloats every `Result<T, Error>` in the workspace past the
    /// `clippy::result_large_err` threshold.
    #[error(transparent)]
    Sign(#[from] Box<crate::oci::sign::SignError>),
    /// An OCI signature verification failed.
    ///
    /// Boxed for the same reason as [`Self::Sign`].
    #[error(transparent)]
    Verify(#[from] Box<crate::oci::verify::VerifyError>),
}

fn launcher_unsafe_hint(c: char) -> &'static str {
    match c {
        '\'' => {
            "single quotes cannot appear in installation paths — relocate $OCX_HOME to a directory whose absolute path has no apostrophe"
        }
        '"' => "double quotes break Windows launcher quoting — relocate the path to a directory without `\"`",
        '%' => "percent triggers cmd.exe variable expansion — relocate the path to a directory without `%`",
        '\n' | '\r' | '\0' => "control characters cannot be embedded in launcher scripts",
        _ => "remove the offending character from the path",
    }
}

impl From<crate::package::error::Error> for Error {
    fn from(e: crate::package::error::Error) -> Self {
        Error::Package(Box::new(e))
    }
}

impl From<crate::patch::PatchError> for Error {
    fn from(e: crate::patch::PatchError) -> Self {
        Error::Patch(Box::new(e))
    }
}

impl Error {
    /// Attach the package `identifier` a failure is about to a single-package
    /// [`PackageErrorKind`](crate::package_manager::error::PackageErrorKind).
    ///
    /// Call this wherever the identifier is in scope. The blanket
    /// `From<PackageErrorKind>` impl below serves callers that have none and
    /// has to fabricate an empty identifier, which then names no package.
    pub fn package(identifier: crate::oci::Identifier, kind: crate::package_manager::error::PackageErrorKind) -> Self {
        use crate::package_manager::error::PackageErrorKind;
        match kind {
            // An internal kind already carries a full `Error`; re-wrapping it in
            // a batch would only nest this type inside itself.
            PackageErrorKind::Internal(e) => e,
            other => {
                // Wrap non-internal kinds in a single-entry ResolveFailed batch.
                // The error message is preserved via `PackageError::Display`.
                let batch_err = crate::package_manager::error::Error::ResolveFailed(vec![
                    crate::package_manager::error::PackageError::new(identifier, other),
                ]);
                Error::PackageManager(batch_err)
            }
        }
    }
}

impl From<crate::package_manager::error::PackageErrorKind> for Error {
    fn from(kind: crate::package_manager::error::PackageErrorKind) -> Self {
        // No identifier in scope: the empty one is rendered as no prefix at all
        // by `PackageError::Display`.
        Error::package(crate::oci::Identifier::new_registry("", ""), kind)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn file_error(path: impl AsRef<std::path::Path>, error: std::io::Error) -> Error {
    Error::InternalFile(path.as_ref().to_path_buf(), error)
}

/// Flatten an error and its `source()` chain into one line, `": {source}"` per
/// link.
///
/// `Display` renders the outermost message only, which for a wrapper variant
/// that adds no `{0}` interpolation (`#[error("failed to fetch managed
/// config")] Fetch(#[from] …)`) names nothing at all. The `{err:#}` chain walk
/// that makes such an error readable happens at the CLI boundary, so anywhere
/// an error is rendered into a value that does NOT reach `main` — a warn line,
/// a machine-readable `reason` field — the chain has to be walked here instead.
///
/// A link whose text the output already ends with is skipped: leaf subsystem
/// errors still interpolate their own source, and this keeps the walk
/// duplicate-free either way.
pub fn render_chain(error: &dyn std::error::Error) -> String {
    let mut out = error.to_string();
    append_chain(&mut out, error.source());
    out
}

/// Append a cause chain to an already-rendered message, `": {source}"` per
/// link — the shared tail of [`render_chain`] and the batch-entry renderer in
/// `package_manager::error`, which starts the walk at a different link.
///
/// A link whose text the output already ends with is skipped: leaf subsystem
/// errors still interpolate their own source, and this keeps the walk
/// duplicate-free either way.
pub fn append_chain(out: &mut String, first_cause: Option<&(dyn std::error::Error + 'static)>) {
    use std::fmt::Write as _;

    let mut cause = first_cause;
    while let Some(source) = cause {
        let text = source.to_string();
        if !out.ends_with(&text) {
            let _ = write!(out, ": {text}");
        }
        cause = source.source();
    }
}

/// Clonable, source-preserving wrapper around [`Error`].
///
/// `crate::Error` is not `Clone` because several of its variants hold
/// `io::Error`, which is not `Clone`. That prevents it from flowing through
/// APIs that must broadcast a single failure to multiple consumers — most
/// notably [`crate::utility::singleflight`], which clones the leader's error
/// to every waiter.
///
/// `ArcError` wraps the typed error in an `Arc` so cloning is cheap and
/// preserves the full error chain (`source()` delegates to the inner
/// `Error`). Callers that need to broadcast a typed `Error` should accept
/// `ArcError` in the variant that carries the failure so downstream code
/// can still walk the chain and (where necessary) downcast to the original
/// variant.
#[derive(Debug, Clone)]
pub struct ArcError(std::sync::Arc<Error>);

impl ArcError {
    /// Returns a reference to the wrapped error.
    pub fn as_error(&self) -> &Error {
        &self.0
    }
}

impl From<Error> for ArcError {
    fn from(error: Error) -> Self {
        Self(std::sync::Arc::new(error))
    }
}

impl std::fmt::Display for ArcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.0, f)
    }
}

impl std::error::Error for ArcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::OfflineMode | Self::PolicyBlocked { .. } => Some(ExitCode::PolicyBlocked),
            Self::InternalFile(_, _) => Some(ExitCode::IoError),
            // Delegate to the wrapped `LayerLayoutError` via the chain walker so
            // a malformed manifest annotation classifies as `DataError` (65),
            // not the generic `IoError` (74) `InternalFile` returns.
            Self::LayerLayout(_) => None,
            // Delegate to the wrapped `SymlinkWalkError` (ancestor symlink → 64,
            // I/O → 74), not the flat `IoError` (74) `InternalFile` returns.
            Self::SymlinkWalk(e) => e.classify(),
            Self::InternalPathInvalid(_) => Some(ExitCode::Failure),
            Self::SerializationFailure(_) | Self::UnsupportedMediaType(_, _) | Self::MetadataBlobTooLarge { .. } => {
                Some(ExitCode::DataError)
            }
            // Transparent wrappers delegate to the inner error's classification.
            Self::Auth(e) => e.classify(),
            Self::Platform(e) => e.classify(),
            Self::Project(e) => e.classify(),
            Self::ProjectRegistry(e) => e.classify(),
            Self::PackageManager(e) => e.classify(),
            Self::OciClient(e) => e.classify(),
            Self::Identifier(e) => e.classify(),
            Self::Archive(e) => e.classify(),
            Self::Compression(e) => e.classify(),
            Self::Ci(e) => e.classify(),
            Self::Config(e) => e.classify(),
            Self::Package(e) => e.as_ref().classify(),
            // Shell errors have no specific exit code yet; defer to chain walker.
            Self::Shell(_) => None,
            Self::OciIndex(e) => e.classify(),
            Self::FileStructure(e) => e.classify(),
            Self::Digest(e) => e.classify(),
            Self::Patch(e) => e.as_ref().classify(),
            Self::Dependency(e) => e.classify(),
            Self::PinnedIdentifier(e) => e.classify(),
            Self::Singleflight(e) => e.classify(),
            Self::LauncherUnsafeCharacter { .. } => Some(ExitCode::DataError),
            Self::Sign(e) => e.as_ref().classify(),
            Self::Verify(e) => e.as_ref().classify(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for the doubled-source-text bug observed in github issue
    /// #286: `Error::InternalFile` both interpolates its `#[source]` io error
    /// into the `#[error("...: {source}")]` message AND marks the same field
    /// `#[source]`. Rendering through `anyhow`'s alternate format (`{:#}`,
    /// the convention this codebase uses to print full error chains —
    /// `quality-rust-errors.md`) walks `source()` on top of the already-
    /// interpolated top-level message, so the io error's text is printed
    /// twice: `"internal file error for '<path>': <text>: <text>"`.
    ///
    /// Guards against that doubling returning: the io cause must be carried by
    /// `#[source]` alone, never also interpolated into the message.
    #[test]
    fn internal_file_display_does_not_duplicate_source_text() {
        let io_error = std::io::Error::other("manifest blob not found in CAS");
        let error = Error::InternalFile(std::path::PathBuf::from("/tmp/example/data"), io_error);
        let wrapped: anyhow::Error = error.into();
        let rendered = format!("{wrapped:#}");
        let occurrences = rendered.matches("manifest blob not found in CAS").count();
        assert_eq!(
            occurrences, 1,
            "the io source text must appear exactly once in the alternate-format chain; got: {rendered}"
        );
    }

    /// The reason `render_chain` exists: a wrapper variant with no `{0}`
    /// interpolation renders a message that names nothing, and the failure it
    /// wraps is only reachable through `source()`. `RefreshUnavailable`'s
    /// machine-readable `reason` is built from exactly this shape.
    #[test]
    fn render_chain_surfaces_a_cause_the_wrapper_display_hides() {
        let error = crate::managed_config::ManagedConfigUpdateError::Fetch(
            crate::managed_config::ManagedConfigFetchError::NoAnyPlatformEntry,
        );
        assert_eq!(
            error.to_string(),
            "failed to fetch managed config",
            "the wrapper's own Display names no cause — that is the bug this guards"
        );
        assert_eq!(
            render_chain(&error),
            "failed to fetch managed config: managed config package has no any/any platform entry"
        );
    }

    /// Leaf subsystem errors still interpolate their own source; appending it a
    /// second time is the doubling bug of github issue #286.
    #[test]
    fn render_chain_does_not_append_an_already_interpolated_link() {
        #[derive(Debug, thiserror::Error)]
        #[error("inner failed")]
        struct Inner;

        #[derive(Debug, thiserror::Error)]
        #[error("outer failed: {0}")]
        struct Outer(#[source] Inner);

        assert_eq!(render_chain(&Outer(Inner)), "outer failed: inner failed");
    }

    /// Regression for github issue #286: converting a
    /// [`crate::package_manager::error::PackageErrorKind`] that has no natural
    /// package identifier (e.g. a `patch test` compose failure, which is not
    /// about any one package) fabricates `Identifier::new_registry("", "")`
    /// and renders it via `PackageError`'s `"{identifier} — {kind}"` — the
    /// bare identifier's `Display` is just `/`, so the message reads
    /// `"failed to resolve package: / — package not found"`, leaking an empty
    /// placeholder the user never supplied.
    ///
    /// Guards against that placeholder returning — and against the obvious
    /// wrong fix of dropping the whole message with it.
    #[test]
    fn package_error_kind_conversion_does_not_leak_bare_identifier_prefix() {
        let error: Error = crate::package_manager::error::PackageErrorKind::NotFound.into();
        let rendered = error.to_string();
        assert!(
            !rendered.contains("/ —"),
            "converted PackageErrorKind must not render a bare empty-identifier prefix; got: {rendered}"
        );
        assert!(
            rendered.contains("package not found"),
            "the kind's own message must survive dropping the identifier prefix; got: {rendered}"
        );
    }
}
