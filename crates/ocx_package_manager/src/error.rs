// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_package::metadata::entrypoint::EntrypointName;
use ocx_package::metadata::{BinaryError, BinaryName};
use ocx_store::file_structure;

/// Task-level error for package manager operations.
///
/// Each variant corresponds to a specific command and contains one
/// [`PackageError`] per failed package, preserving the individual cause.
///
/// This type does **not** wrap [`crate::Error`] directly — library errors are
/// always attached to a specific package via [`PackageErrorKind::Internal`].
///
/// # Exit code classification
///
/// Batch classification uses **first error wins**: when a batch variant
/// carries multiple [`PackageError`]s, the process exit code is derived from
/// the first element's [`PackageError::kind`]. This makes the exit code for
/// multi-package operations input-order-dependent — running
/// `ocx install a b c` where `a` fails with `NotFound` and `b` fails with
/// `SelectionAmbiguous` exits with `NotFound`'s code, regardless of how many
/// `SelectionAmbiguous` entries follow. This is the v1 contract; a future
/// priority function (e.g. "worst code wins") may upgrade the policy without
/// touching variant data.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A find operation failed for one or more packages.
    #[error("{}", format_batch("find", _0))]
    FindFailed(Vec<PackageError>),
    /// An install operation failed for one or more packages.
    #[error("{}", format_batch("install", _0))]
    InstallFailed(Vec<PackageError>),
    /// An uninstall operation failed for one or more packages.
    #[error("{}", format_batch("uninstall", _0))]
    UninstallFailed(Vec<PackageError>),
    /// A deselect operation failed for one or more packages.
    #[error("{}", format_batch("deselect", _0))]
    DeselectFailed(Vec<PackageError>),
    /// A resolve operation failed for one or more packages.
    #[error("{}", format_batch("resolve", _0))]
    ResolveFailed(Vec<PackageError>),
    /// An inspect operation failed for one or more packages.
    #[error("{}", format_batch("inspect", _0))]
    InspectFailed(Vec<PackageError>),
    /// A select operation failed for one or more packages.
    #[error("{}", format_batch("select", _0))]
    SelectFailed(Vec<PackageError>),
    /// The self-update check operation failed.
    ///
    /// Distinct from [`InstallFailed`]: a check failure does not imply an
    /// install was attempted.  Carries the [`PackageError`] that caused the
    /// check to abort, boxed to break the recursive-type cycle
    /// (`crate::Error` → `crate::Error` → `PackageError` →
    /// `PackageErrorKind::Internal` → `crate::Error`).
    #[error("self-update check failed: {}", render_entry(_0))]
    SelfCheckFailed(Box<PackageError>),

    // ── The tier's own root error (E1, DEC-27) ──────────────────────────────
    //
    // Copied verbatim from `ocx_lib::Error` at WP-34, so the exit code each
    // one classifies to is the code its predecessor classified to (DEC-23).
    // `ocx_lib::Error` keeps every one of them: this is additive, and its
    // hand-written `From<ocx_package_manager::Error>` reconstructs them on the
    // way back up. `PackageManager` is deliberately absent — on `ocx_lib` it
    // wraps THIS enum, so a copy here would be self-referential; its four call
    // sites construct the tier error directly instead.
    /// A network operation was attempted while in offline mode.
    #[error("network operation attempted in offline mode")]
    OfflineMode,

    /// JSON serialization or deserialization failed.
    #[error("JSON serialization error")]
    SerializationFailure(#[from] serde_json::Error),
    /// An OCI index operation failed.
    ///
    /// No `#[from]`: three of the index tier's variants stand in for variants
    /// of *this* enum and must be reconstructed as those rather than nested
    /// under this one, or the exit code moves. See the hand-written
    /// [`From<ocx_index::error::Error>`](Self::from) below.
    #[error(transparent)]
    OciIndex(ocx_index::error::Error),

    /// A dependency graph operation failed.
    #[error(transparent)]
    Dependency(#[from] DependencyError),
    /// A package operation failed.
    #[error(transparent)]
    Package(Box<ocx_package::error::Error>),
    /// An archive operation failed.
    #[error(transparent)]
    Archive(#[from] ocx_util::archive::Error),

    /// A string baked into an install-time launcher contains a character that
    /// cannot be safely embedded in the Unix `.sh` template or the Windows
    /// `.shim` sidecar (single-quote, percent, double-quote, NUL, CR, LF).
    /// The unsafe set is owned by `crate::launcher`.
    #[error("launcher-unsafe character {character:?} in {value:?}; {}", launcher_unsafe_hint(*character))]
    LauncherUnsafeCharacter { value: String, character: char },

    /// A path baked verbatim into a generated launcher body or a one-line
    /// sidecar is not valid UTF-8.
    ///
    /// Refused at render rather than converted: a lossy conversion substitutes
    /// U+FFFD for every invalid byte, which passes the launcher-unsafe
    /// character set and bakes a path that **does not exist**. The failure then
    /// surfaces as a bare `ENOENT` from `/bin/sh`, naming a path the operator
    /// never wrote and with nothing pointing at the encoding as the cause.
    ///
    /// `path` is the lossy rendering — the only printable form of a value that
    /// is by definition not a `str`. The message says the path is not UTF-8, so
    /// it never claims the bytes it shows are the bytes on disk.
    #[error("path baked into a generated launcher is not valid UTF-8: {path:?}")]
    LauncherPathNotUtf8 { path: String },

    /// A toolchain **home** baked into a rendered trampoline or its `.exec`
    /// sidecar is not an absolute path.
    ///
    /// Refused at render, never written: a relative value resolves against the
    /// *invoking process's* working directory, so the same trampoline would
    /// select two different homes from two directories. A relative
    /// `.exec` sidecar is additionally refused by the shim at runtime as E2
    /// (exit 78) — refusing here turns that confusing runtime failure into one
    /// that names the path and the writer.
    ///
    /// The literal `global` is the one non-path value a `.exec` sidecar may
    /// carry, and it is produced from [`crate`]'s own global
    /// target rather than from a project root — so a project root spelled
    /// `global` is refused here and can never be read back as the global home.
    #[error("toolchain home is not an absolute path: {value:?}")]
    ToolchainHomeNotAbsolute { value: String },
    /// An OCI signature verification failed.
    ///
    /// Boxed for the same reason as [`Self::Sign`].
    #[error(transparent)]
    Verify(#[from] Box<ocx_sign::verify::VerifyError>),
    /// A digest string could not be parsed.
    #[error(transparent)]
    Digest(#[from] ocx_oci::digest::error::DigestError),
    /// A file structure operation failed.
    #[error(transparent)]
    FileStructure(#[from] ocx_store::file_structure::error::Error),

    /// A file I/O error with path context.
    ///
    /// The io cause is interpolated into the message and deliberately **not**
    /// exposed as `#[source]` — the one exception to the "every wrapping
    /// variant carries `#[source]`" rule, and exactly one of the two, never
    /// both. Both together printed it twice in a `{err:#}` chain (#286);
    /// `#[source]` alone left every `to_string()` producer — a warn line, a
    /// machine-readable `reason` — naming the path and nothing else (#433).
    /// Nothing is lost by the omission: `ClassifyExitCode` (in the binary,
    /// `ocx::exit`) answers at this
    /// variant and never descends to the io error, and no other consumer
    /// downcasts through it.
    #[error("internal file error for '{path}': {cause}", path = .0.display(), cause = .1)]
    InternalFile(std::path::PathBuf, std::io::Error),

    /// An OCI signing operation failed.
    ///
    /// Boxed because [`ocx_sign::sign::SignError`] carries a full
    /// [`ocx_oci::Identifier`] plus a kind enum — materializing it
    /// unboxed bloats every `Result<T, Error>` in the workspace past the
    /// `clippy::result_large_err` threshold.
    #[error(transparent)]
    Sign(#[from] Box<ocx_sign::sign::SignError>),

    /// A singleflight coordination error (leader failure, abandonment, timeout,
    /// or capacity exceeded).
    #[error("singleflight coordination failed")]
    Singleflight(#[from] ocx_util::singleflight::Error),
    /// An OCI client operation failed.
    #[error(transparent)]
    OciClient(#[from] ocx_oci::client::error::ClientError),

    /// A per-layer layout annotation read from a manifest layer descriptor could
    /// not be resolved into a placement.
    ///
    /// Carries the [`LayerLayoutError`](ocx_oci::LayerLayoutError) cause via
    /// `#[source]` so exit-code classification descends the chain and reaches it
    /// (→ `DataError` 65 for a malformed/hostile manifest annotation), instead
    /// of the generic `IoError` (74) that wrapping in [`Self::InternalFile`]
    /// would force. `io::Error::source` skips a boxed inner error, so the layout
    /// cause must be carried as a first-class `#[source]` field here, not
    /// smuggled through `io::Error::other`.
    #[error("layer layout resolution failed")]
    LayerLayout(#[source] ocx_oci::layer_layout::LayerLayoutError),

    /// An unsupported OCI media type was encountered.
    #[error("unsupported media type '{media_type}', expected media types are: {supported}", media_type = .0, supported = .1.join(", "))]
    UnsupportedMediaType(String, &'static [&'static str]),

    /// A metadata config blob exceeded the size cap enforced by
    /// `crate::tasks::common::load_config_metadata`, either by its
    /// declared descriptor size (checked before any blob fetch) or its
    /// actual fetched byte length (checked after fetch, defending against a
    /// registry that declares a small size but serves a larger body). See
    /// `adr_inspect_metadata_closure.md` D5.
    #[error("metadata blob size {size} bytes exceeds the {max}-byte cap")]
    MetadataBlobTooLarge { size: i64, max: usize },

    // Three more with no direct constructor in this tier, present so the two
    // flattening conversions below have an exact destination for every variant
    // they map. Without them `Platform`, `PinnedIdentifier` and `PathInvalid`
    // would fall through to a wrapper, and each fall-through becomes a
    // separate "is the code still the same?" question at review time.
    /// A platform parsing or validation error.
    #[error(transparent)]
    Platform(#[from] ocx_oci::platform::error::PlatformError),
    /// A pinned identifier validation failed.
    #[error(transparent)]
    PinnedIdentifier(#[from] ocx_oci::pinned_identifier::PinnedIdentifierError),
    /// A path has an unexpected structure.
    #[error("path '{}' has an unexpected structure", .0.display())]
    InternalPathInvalid(std::path::PathBuf),

    // Two more the tier reaches through `?` rather than by name: `ocx_project`
    // is the tier below, and `patch` is one of the three subtrees that came
    // with this crate.
    /// A project-tier configuration or lock operation failed.
    #[error(transparent)]
    Project(#[from] ocx_project::error::Error),
    /// A patch-domain operation failed outside the per-package discovery path
    /// (which carries its own `PatchError` through `PackageErrorKind`).
    ///
    /// Boxed because `PatchError::BlobWriteFailed` carries a `crate::Error`
    /// back, and the two enums would otherwise be mutually infinite. Same
    /// shape as [`Self::Package`], including the hand-written `From`.
    #[error(transparent)]
    Patch(Box<crate::patch::PatchError>),

    /// A symlink walk failed. Destination for `AssembleError::SymlinkWalk`.
    #[error(transparent)]
    SymlinkWalk(ocx_util::fs::SymlinkWalkError),

    #[error(transparent)]
    ProjectRegistry(#[from] ocx_project::registry::error::Error),
}

/// An error tied to a specific package.
///
/// `kind` deliberately omits `#[source]` — a deviation from the three-layer
/// error pattern in `quality-rust-errors.md`. Exit-code classification for
/// package errors does **not** walk the `source()` chain; it dispatches
/// directly through the `ClassifyExitCode` impls the binary carries
/// (`ocx::exit`) on both `PackageError`
/// and `PackageErrorKind` (see the bottom of this file and
/// `classify_error` in `ocx_cli`'s `exit::classify`). Adding `#[source]` would
/// duplicate the kind into both the `Display` chain and the `source()`
/// chain without improving diagnosability.
#[derive(Debug, thiserror::Error)]
#[error("{}{kind}", identifier_prefix(identifier))]
#[non_exhaustive]
pub struct PackageError {
    pub identifier: ocx_oci::Identifier,
    pub kind: PackageErrorKind,
}

impl PackageError {
    pub fn new(identifier: ocx_oci::Identifier, kind: PackageErrorKind) -> Self {
        Self { identifier, kind }
    }
}

/// The `"<identifier> — "` lead-in of a [`PackageError`] message, empty for the
/// empty identifier [`crate::Error::from`] fabricates when a
/// [`PackageErrorKind`] arrives with no package in scope. Rendering that one
/// would print a bare `/` — a package name the user never supplied.
fn identifier_prefix(identifier: &ocx_oci::Identifier) -> String {
    if identifier.registry().is_empty() && identifier.repository().is_empty() {
        String::new()
    } else {
        format!("{identifier} — ")
    }
}

/// Payload for [`PackageErrorKind::OfflineManifestMissing`]. Boxed in the
/// enum variant to keep `PackageErrorKind` small (avoids the
/// `clippy::result_large_err` lint).
#[derive(Debug)]
pub struct OfflineManifestMissing {
    pub identifier: ocx_oci::Identifier,
    pub digest: ocx_oci::Digest,
}

/// Payload for the shim refusals that name a package **and** one of its claimed
/// interface names ([`ShimNameNotClaimed`](PackageErrorKind::ShimNameNotClaimed),
/// [`ShimClaimUnfulfilled`](PackageErrorKind::ShimClaimUnfulfilled)).
///
/// A size device, not a semantic union: the two refusals are unrelated
/// situations that happen to carry the same pair, and inlining it makes the
/// variant 128 bytes — over the `clippy::result_large_err` ceiling every
/// `Result<_, PackageErrorKind>` in the crate would then trip. Boxed for the
/// same reason [`OfflineManifestMissing`] is.
///
/// The package is carried explicitly rather than read off
/// [`PackageError::identifier`], because the offending node may be a dependency
/// deep in the closure while the identifier is the tool the user asked for.
#[derive(Debug)]
pub struct ShimClaim {
    pub package: ocx_oci::PinnedIdentifier,
    pub name: BinaryName,
}

/// The cause of a single-package failure.
#[derive(Debug, thiserror::Error)]
pub enum PackageErrorKind {
    /// The package was not found in the index or object store.
    #[error("package not found")]
    NotFound,
    /// Offline mode: the tag pointer is cached locally but the manifest
    /// blob is missing from `blobs/`. The caller needs to re-run the
    /// command online to populate the blob cache.
    #[error(
        "manifest {} is not in the local cache; run `ocx install {}` online to populate it",
        _0.digest,
        _0.identifier
    )]
    OfflineManifestMissing(Box<OfflineManifestMissing>),
    /// A referenced blob (layer digest) was not present in the registry.
    ///
    /// The identifier is `registry/repository[:tag]@<blob-digest>` — see
    /// [`ocx_oci::client::error::ClientError::BlobNotFound`] for the
    /// canonical construction contract.
    #[error("blob not found: {0}")]
    BlobNotFound(ocx_oci::PinnedIdentifier),
    /// Multiple candidates matched the platform selection.
    #[error("ambiguous selection: {}", _0.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(", "))]
    SelectionAmbiguous(Vec<ocx_oci::Identifier>),
    /// A symlink-based path was requested but the identifier carries a digest.
    #[error("symlink resolution requires a tag, not a digest")]
    SymlinkRequiresTag,
    /// The requested install symlink does not exist.
    #[error("{}", match _0 {
        file_structure::SymlinkKind::Candidate => "no installed candidate",
        file_structure::SymlinkKind::Current => "no selected version",
    })]
    SymlinkNotFound(file_structure::SymlinkKind),
    /// A spawned task panicked unexpectedly.
    #[error("task panicked unexpectedly")]
    TaskPanicked,
    /// The identifier has no digest after resolution.
    #[error("identifier has no digest after resolution")]
    DigestMissing,
    /// An entrypoint name collision was detected in the interface surface of the
    /// transitive closure. Raised at install time when N≥2 packages in the
    /// interface projection declare the same entrypoint `name`. Reports all
    /// owners so the user can deselect the right one. Supersedes the
    /// 2-owner `EntrypointNameCollision` variant (see `adr_two_env_composition.md`).
    #[error(
        "entrypoint name collision: '{name}' declared by {} packages: {}; deselect one before selecting another",
        owners.len(),
        owners.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
    )]
    EntrypointCollision {
        name: EntrypointName,
        owners: Vec<ocx_oci::PinnedIdentifier>,
    },

    /// A required companion package install failed during patch discovery.
    ///
    /// The base install succeeded, but a companion marked `required = true`
    /// could not be fetched or installed. Fail-closed: the install as a whole
    /// is considered failed so the caller does not run with an incomplete
    /// environment overlay. Optional companions (required = false) are logged
    /// as warnings and do not produce this variant.
    #[error("required companion install failed for '{companion}'")]
    RequiredCompanionFailed {
        /// Identifier of the companion package that failed to install.
        companion: ocx_oci::Identifier,
        /// The underlying package error kind from the companion's install.
        #[source]
        source: Box<PackageErrorKind>,
    },

    /// Patch discovery failed due to a domain-level patch error (fetch, parse,
    /// persist, or structural validation of a `__ocx.patch` descriptor).
    ///
    /// Carries the full [`crate::patch::PatchError`] chain via `#[source]` so
    /// the error chain is preserved for exit-code classification and diagnostics.
    /// This replaces the former `Internal(io::Error::other(patch_error.to_string()))`
    /// workaround that erased the structured source chain.
    #[error("patch discovery error")]
    PatchDiscovery(#[source] crate::patch::PatchError),

    /// No index entry satisfies the host's detected `os.features` requirements.
    ///
    /// Raised by `Index::select` when the host declares a non-empty
    /// `os.features` set (e.g. `libc.glibc`) but every candidate platform in
    /// the index sharing the host's os+arch declares an `os_features` set that
    /// is not a subset of the host's features. This is a general
    /// `os.features` mismatch — libc is the first such feature, but the
    /// matcher is not libc-specific.
    ///
    /// The user can override by passing `--platform` with an explicit
    /// `os/arch[+feature...]` matching one of the available entries — the
    /// available platforms are rendered with their `+feature` suffixes so the
    /// value is copy-pasteable.
    ///
    /// Error string follows API Guidelines: lowercase, no period.
    #[error(
        "feature mismatch: host provides {}; available platforms: {}; pass --platform <os/arch[+features]> to override",
        host_features.join(", "),
        available.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
    )]
    FeatureMismatch {
        /// The `os.features` values the host reported (e.g. `["libc.glibc"]`).
        host_features: Vec<String>,
        /// The candidate [`ocx_oci::Platform`] values sharing the host os+arch, so
        /// the user can see which `--platform <os/arch[+features]>` value to
        /// pass.
        available: Vec<ocx_oci::Platform>,
    },

    /// The closure's interface name set is not enumerable, so no shim can be
    /// generated for it: some node declares neither `binaries` nor entry
    /// points, and a shim store is built by name (plan contract C-009).
    ///
    /// Names the offending node rather than relying on
    /// [`PackageError::identifier`] — the node may be a dependency deep in the
    /// closure while the identifier is the tool the user asked for.
    #[error(
        "cannot defer '{package}': it claims no binaries and no entry points, so its interface names are not enumerable"
    )]
    ShimNamesNotEnumerable { package: ocx_oci::PinnedIdentifier },

    /// A shim name is not a valid [`BinaryName`] (plan contract C-009 / C-011).
    ///
    /// Raised from **both** ends of the shim path, which is why the message
    /// names neither an invocation nor a package: `ocx launcher shim` invoked
    /// under a bad `argv0` (C-011, first leg), and `prepare_lazy` handed a
    /// declared entry point name that does not survive the conversion (C-009 —
    /// every Windows-reserved device name is a valid entry point name and none
    /// is a valid binary name). The offending name and the reason come from the
    /// wrapped [`BinaryError`]; the declaring package does not, so a producer
    /// -side refusal deep in a closure is attributed only by the envelope's own
    /// identifier.
    ///
    /// The grammar forbids `/`, `\` and the Windows-reserved device names at
    /// construction, so this is also what stops a wire value containing a path
    /// separator from bypassing `PATH` resolution entirely.
    #[error("invalid shim name")]
    ShimNameInvalid(#[source] BinaryError),

    /// `ocx launcher shim` was invoked under a well-formed name that is not a
    /// member of the composed name set (plan contract C-011, second leg).
    #[error("'{}' is not an interface name declared by '{}'", _0.name, _0.package)]
    ShimNameNotClaimed(Box<ShimClaim>),

    /// The package materialized, but the name its metadata claimed is not
    /// present on the composed `PATH` (plan contract C-011).
    ///
    /// Reported instead of the bare `ENOENT` an exec would otherwise produce,
    /// so a wrong `binaries` claim is attributed to the publisher rather than
    /// read as a missing package.
    #[error(
        "'{}' claims the name '{}', but no such executable is present after materialization",
        _0.package,
        _0.name
    )]
    ShimClaimUnfulfilled(Box<ShimClaim>),

    /// A group or entry name cannot become a path component of a rendered
    /// toolchain tree (RUL-21, D-V14).
    ///
    /// Lives here rather than on [`crate::Error`] because its only producers
    /// are `package_manager` tasks — `render_toolchain` and `heal_links`,
    /// which `?`-propagate
    /// [`ToolchainHome::entry`](ocx_store::file_structure::ToolchainHome::entry)
    /// and its store-side twin. A top-level variant would mint crate-wide
    /// vocabulary for a leaf with a natural home one layer down, and this
    /// enum is already an entry in `cli/classify.rs`'s downcast ladder, so
    /// exit 78 is reachable from `argv` through it.
    ///
    /// `transparent` because the wrapped error already names the component
    /// (`group` or `entry`), the refused value and the reason — there is no
    /// prefix worth adding.
    #[error(transparent)]
    ToolchainPath(#[from] file_structure::ToolchainPathError),

    /// An underlying internal error (I/O, OCI, network, etc.).
    #[error(transparent)]
    Internal(#[from] crate::Error),
}

impl From<ocx_oci::client::error::ClientError> for PackageErrorKind {
    fn from(e: ocx_oci::client::error::ClientError) -> Self {
        match e {
            ocx_oci::client::error::ClientError::BlobNotFound(pinned) => Self::BlobNotFound(pinned),
            other => Self::Internal(other.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Batch formatter — used by `#[error(...)]` attributes on `Error` variants.
// ---------------------------------------------------------------------------

fn format_batch(verb: &str, errors: &[PackageError]) -> String {
    use std::fmt::Write as _;
    if errors.len() == 1 {
        format!("failed to {verb} package: {}", render_entry(&errors[0]))
    } else {
        let mut s = format!("failed to {verb} {} packages:", errors.len());
        for e in errors {
            let _ = write!(s, "\n  {}", render_entry(e));
        }
        s
    }
}

/// Render one batch entry with its cause chain appended, `": {source}"` per link.
///
/// A batch carries N failures, so it can expose none of them as a single
/// `source()`; the chain walk that `{err:#}` performs at the CLI boundary stops
/// at the batch. Without this, every cause below a `PackageErrorKind` — the io
/// error under `Internal`, the `PatchError` under `PatchDiscovery` — is dropped
/// from the only message the user sees. Walks `kind.source()`, not
/// `entry.source()`: `PackageError` deliberately omits `#[source]` on `kind`
/// (see the type's doc comment), so the entry itself reports no source.
fn render_entry(entry: &PackageError) -> String {
    use std::error::Error as _;

    let mut out = entry.to_string();
    ocx_util::error::append_chain(&mut out, entry.kind.source());
    out
}

/// Errors from dependency resolution operations.
#[derive(Debug, thiserror::Error)]
pub enum DependencyError {
    /// Two or more packages on the active surface resolve the same repository to
    /// different digests. A single environment cannot expose multiple versions
    /// of one package, so composition fails. The identifiers name the conflicting
    /// versions (tag and digest) so the user can tell which were involved.
    #[error("conflicting versions for {repository}: {}", identifiers.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", "))]
    Conflict {
        repository: ocx_oci::Repository,
        identifiers: Vec<ocx_oci::PinnedIdentifier>,
    },
    /// Dependency setup coordination failed (capacity, timeout, or abandoned leader).
    ///
    /// The singleflight cause is carried by `#[from]` (which implies
    /// `#[source]`); interpolating it here too would print it twice.
    #[error("dependency setup failed")]
    SetupFailed(#[from] ocx_util::singleflight::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_oci::client::error::ClientError;

    #[test]
    fn client_blob_not_found_routes_to_typed_kind() {
        let image: ocx_oci::native::Reference = "example.com/foo/bar:1.0".parse().unwrap();
        let blob_str = format!("sha256:{}", "a".repeat(64));
        let blob = ocx_oci::Digest::try_from(blob_str.as_str()).unwrap();
        let e = ClientError::blob_not_found(&image, &blob);
        let kind: PackageErrorKind = e.into();
        match kind {
            PackageErrorKind::BlobNotFound(pinned) => {
                assert_eq!(pinned.registry(), "example.com");
                assert_eq!(pinned.repository(), "foo/bar");
                assert_eq!(pinned.tag(), Some("1.0"));
                assert_eq!(pinned.digest().to_string(), blob_str);
            }
            other => panic!("expected BlobNotFound, got {other:?}"),
        }
    }

    #[test]
    fn other_client_errors_still_route_to_internal() {
        let e = ClientError::ManifestNotFound("example.com/pkg".to_string());
        let kind: PackageErrorKind = e.into();
        assert!(matches!(kind, PackageErrorKind::Internal(_)));
    }

    /// A batch is the only channel its entries have: it carries N failures, so
    /// it exposes none of them as a `source()` and `{err:#}` stops at the batch.
    /// The io cause under `PackageErrorKind::Internal` must therefore be
    /// rendered by the batch itself — exactly once, not dropped and not doubled.
    #[test]
    fn batch_renders_the_io_cause_of_an_internal_entry_exactly_once() {
        let io_error = std::io::Error::other("permission denied by fixture");
        let entry = PackageError::new(
            ocx_oci::Identifier::new_registry("cmake", "example.com").clone_with_tag("3.28"),
            PackageErrorKind::Internal(crate::error::file_error("/tmp/example/data", io_error)),
        );
        let rendered = format!("{:#}", anyhow::Error::from(Error::InstallFailed(vec![entry])));
        assert_eq!(
            rendered.matches("permission denied by fixture").count(),
            1,
            "the io cause must appear exactly once in the batch message; got: {rendered}"
        );
        assert!(
            rendered.contains("cmake"),
            "the batch message must still name the failing package; got: {rendered}"
        );
    }

    /// The multi-entry arm renders every entry's own cause, each once.
    #[test]
    fn multi_entry_batch_renders_each_entrys_cause_once() {
        let entries = vec![
            PackageError::new(
                ocx_oci::Identifier::new_registry("cmake", "example.com"),
                PackageErrorKind::Internal(crate::error::file_error(
                    "/tmp/a",
                    std::io::Error::other("first cause fixture"),
                )),
            ),
            PackageError::new(
                ocx_oci::Identifier::new_registry("ninja", "example.com"),
                PackageErrorKind::Internal(crate::error::file_error(
                    "/tmp/b",
                    std::io::Error::other("second cause fixture"),
                )),
            ),
        ];
        let rendered = format!("{:#}", anyhow::Error::from(Error::InstallFailed(entries)));
        assert_eq!(rendered.matches("first cause fixture").count(), 1, "got: {rendered}");
        assert_eq!(rendered.matches("second cause fixture").count(), 1, "got: {rendered}");
    }

    /// `SelfCheckFailed` carries a single entry rather than a batch, and takes
    /// the same `render_entry` route.
    #[test]
    fn self_check_failed_renders_its_entrys_cause_exactly_once() {
        let entry = PackageError::new(
            ocx_oci::Identifier::new_registry("ocx/cli", "ocx.sh"),
            PackageErrorKind::Internal(crate::error::file_error(
                "/tmp/self-check",
                std::io::Error::other("self-check cause fixture"),
            )),
        );
        let rendered = format!("{:#}", anyhow::Error::from(Error::SelfCheckFailed(Box::new(entry))));
        assert_eq!(
            rendered.matches("self-check cause fixture").count(),
            1,
            "got: {rendered}"
        );
    }

    /// A leaf subsystem error that still interpolates its own `#[source]`
    /// (`archive::Error::Tar` renders `"tar error: <io>"`) must not have that
    /// text appended a second time by the chain walk.
    #[test]
    fn batch_does_not_double_a_leaf_error_that_inlines_its_own_source() {
        let entry = PackageError::new(
            ocx_oci::Identifier::new_registry("cmake", "example.com"),
            PackageErrorKind::Internal(crate::Error::Archive(ocx_util::archive::Error::Tar(
                std::io::Error::other("unexpected end of archive fixture"),
            ))),
        );
        let rendered = format!("{:#}", anyhow::Error::from(Error::InstallFailed(vec![entry])));
        assert_eq!(
            rendered.matches("unexpected end of archive fixture").count(),
            1,
            "a leaf that inlines its own source must not be doubled by the walk; got: {rendered}"
        );
    }

    /// Same contract one layer deeper: `RequiredCompanionFailed` carries its
    /// cause as `#[source]` only, so the batch walk is what surfaces it.
    #[test]
    fn batch_renders_a_required_companion_cause_exactly_once() {
        let entry = PackageError::new(
            ocx_oci::Identifier::new_registry("java", "example.com"),
            PackageErrorKind::RequiredCompanionFailed {
                companion: ocx_oci::Identifier::new_registry("license-server", "patches.corp.com"),
                source: Box::new(PackageErrorKind::NotFound),
            },
        );
        let rendered = format!("{:#}", anyhow::Error::from(Error::ResolveFailed(vec![entry])));
        assert_eq!(
            rendered.matches("package not found").count(),
            1,
            "the companion's cause must appear exactly once; got: {rendered}"
        );
        assert!(
            rendered.contains("license-server"),
            "the message must name the companion; got: {rendered}"
        );
    }
}

/// `Result` for the whole tier, as `ocx_lib::Result` was before the split.
pub type Result<T> = std::result::Result<T, Error>;

/// The `InternalFile` constructor the moved tree calls 57 times.
///
/// Spelled exactly as `ocx_lib::error::file_error` was, so every call site
/// moved unchanged — the module path `crate::error` now resolves here because
/// `package_manager/error.rs` flattened to the crate root.
pub fn file_error(path: impl AsRef<std::path::Path>, error: std::io::Error) -> Error {
    Error::InternalFile(path.as_ref().to_path_buf(), error)
}

/// Flatten the index tier's errors onto this tier's own variants.
///
/// **Hand-written, and a derived `#[from]` here is a defect.** WP-33 shipped
/// exactly that mistake one tier over: the derive wraps where this flattens,
/// and because both wrappers are `#[error(transparent)]` a `source()` walk
/// delegates past the inner `ClientError` without ever yielding it. Retries
/// then stop being spent and the exit code moves from 75 to 69, with nothing
/// failing at the conversion itself. Mirrors `ocx_lib`'s impl one-for-one;
/// `error.rs`'s shape assertions in `ocx_lib` and the ones below hold it.
impl From<ocx_index::error::Error> for Error {
    fn from(error: ocx_index::error::Error) -> Self {
        use ocx_index::error::Error as IndexError;
        match error {
            IndexError::Store(error) => Self::FileStructure(error),
            IndexError::SerializationFailure(error) => Self::SerializationFailure(error),
            IndexError::OciClient(error) => Self::OciClient(error),
            IndexError::Digest(error) => Self::Digest(error),
            IndexError::PinnedIdentifier(error) => Self::PinnedIdentifier(error),
            IndexError::File(error) => Self::InternalFile(error.path, error.cause),
            IndexError::PathInvalid(path) => Self::InternalPathInvalid(path),
            other => Self::OciIndex(other),
        }
    }
}

/// Flatten the package tier's errors onto this tier's own variants.
///
/// `PackageError::Index(error) => error.into()` re-enters the conversion
/// above, so a `ClientError` arriving inside a `PackageError::Index` has to
/// flatten across BOTH impls to land as `Error::OciClient`. That two-hop path
/// is the one no single-impl test can observe, and it is asserted below.
impl From<ocx_package::error::Error> for Error {
    fn from(e: ocx_package::error::Error) -> Self {
        use ocx_package::error::Error as PackageError;
        match e {
            PackageError::File(error) => Self::InternalFile(error.path, error.cause),
            PackageError::OciClient(error) => Self::OciClient(error),
            PackageError::Index(error) => error.into(),
            PackageError::SerializationFailure(error) => Self::SerializationFailure(error),
            PackageError::Digest(error) => Self::Digest(error),
            PackageError::Platform(error) => Self::Platform(error),
            PackageError::Archive(error) => Self::Archive(error),
            other => Self::Package(Box::new(other)),
        }
    }
}

impl Error {
    /// Lift a [`PackageErrorKind`] into the tier error, preserving the
    /// identifier when the caller has one. Ported verbatim from
    /// `ocx_lib::Error::package`; the only change is that the batch error IS
    /// this type now, so there is no wrapper variant to put it in.
    pub fn package(identifier: ocx_oci::Identifier, kind: PackageErrorKind) -> Self {
        match kind {
            // An internal kind already carries a full `Error`; re-wrapping it
            // in a batch would only nest this type inside itself.
            PackageErrorKind::Internal(e) => e,
            other => Self::ResolveFailed(vec![PackageError::new(identifier, other)]),
        }
    }
}

impl From<PackageErrorKind> for Error {
    fn from(kind: PackageErrorKind) -> Self {
        // No identifier in scope: the empty one is rendered as no prefix at all
        // by `PackageError::Display`.
        Error::package(ocx_oci::Identifier::new_registry("", ""), kind)
    }
}

impl From<ocx_util::error::FileError> for Error {
    fn from(error: ocx_util::error::FileError) -> Self {
        Self::InternalFile(error.path, error.cause)
    }
}

impl From<ocx_util::error::SerializationError> for Error {
    fn from(error: ocx_util::error::SerializationError) -> Self {
        Self::SerializationFailure(error.0)
    }
}

/// Flattening, like the two above it: each arm lands on the variant its
/// predecessor landed on rather than nesting under a wrapper.
impl From<ocx_util::error::Error> for Error {
    fn from(error: ocx_util::error::Error) -> Self {
        match error {
            ocx_util::error::Error::File(error) => error.into(),
            ocx_util::error::Error::Serialization(error) => error.into(),
        }
    }
}

impl From<ocx_store::file_structure::PackageDirError> for Error {
    fn from(error: ocx_store::file_structure::PackageDirError) -> Self {
        use ocx_store::file_structure::PackageDirError;
        match error {
            PackageDirError::File(file) => Self::InternalFile(file.path, file.cause),
            PackageDirError::PathInvalid(path) => Self::InternalPathInvalid(path),
        }
    }
}

impl From<ocx_oci::media_type::UnsupportedMediaType> for Error {
    fn from(error: ocx_oci::media_type::UnsupportedMediaType) -> Self {
        Self::UnsupportedMediaType(error.media_type, error.expected)
    }
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

impl From<ocx_store::file_structure::AssembleError> for Error {
    fn from(error: ocx_store::file_structure::AssembleError) -> Self {
        use ocx_store::file_structure::AssembleError;
        match error {
            AssembleError::File(error) => Self::InternalFile(error.path, error.cause),
            AssembleError::SymlinkWalk(error) => Self::SymlinkWalk(error),
            AssembleError::Archive(error) => Self::Archive(error),
        }
    }
}

impl From<crate::patch::PatchError> for Error {
    fn from(e: crate::patch::PatchError) -> Self {
        Error::Patch(Box::new(e))
    }
}

/// Flattening, mirroring `ocx_lib`: `File` lands on `InternalFile` and
/// `Digest` on `Digest`, rather than both nesting under a store wrapper.
impl From<ocx_store::file_structure::DigestFileError> for Error {
    fn from(error: ocx_store::file_structure::DigestFileError) -> Self {
        use ocx_store::file_structure::DigestFileError;
        match error {
            DigestFileError::File(file) => Self::InternalFile(file.path, file.cause),
            DigestFileError::Digest(digest) => Self::Digest(digest),
        }
    }
}

#[cfg(test)]
mod flattening_shape {
    //! The tier's flattening `From` impls, asserted as a SHAPE, and the two
    //! `source()` walks that read the fields those impls feed.
    //!
    //! Mirrors `ocx_lib::error::flattening_shape`, which exists because WP-33
    //! shipped a derive where a flattening conversion belonged: the wrappers
    //! are `#[error(transparent)]`, so `source()` steps over the inner error
    //! without yielding it, retries stop being spent and an exit code moves
    //! with nothing failing at the conversion.
    //!
    //! The chain assertions at the bottom are the other half, and they are
    //! about a change of *meaning* rather than of code. `SessionError::Library`
    //! and `PackageErrorKind::Internal` both spell their field `crate::Error`,
    //! which meant `ocx_lib::Error` before WP-34 and means this tier's error
    //! after it. `Internal` announced the change — six `LibError::` patterns in
    //! the sign commands stopped compiling. The others changed silently, so
    //! their readers are asserted instead of argued: `launch.rs`'s
    //! `successors(.., |cause| cause.source())` walk and `error.rs`'s
    //! `append_chain(.., entry.kind.source())` are what consume them.

    use super::*;
    use ocx_index::error::Error as IndexError;
    use ocx_package::error::Error as PkgError;

    fn client() -> ocx_oci::client::error::ClientError {
        ocx_oci::client::error::ClientError::Authentication(Box::new(std::io::Error::other("bad creds")))
    }
    fn file() -> ocx_util::error::FileError {
        ocx_util::error::FileError::new("x", std::io::Error::other("x"))
    }
    fn serialization() -> serde_json::Error {
        serde_json::from_str::<u8>("x").expect_err("not a u8")
    }
    fn digest() -> ocx_oci::digest::error::DigestError {
        ocx_oci::digest::error::DigestError::Invalid("not-a-digest".into())
    }

    #[test]
    fn index_errors_flatten_onto_this_tiers_own_variants() {
        assert!(matches!(
            Error::from(IndexError::OciClient(client())),
            Error::OciClient(_)
        ));
        assert!(matches!(Error::from(IndexError::Digest(digest())), Error::Digest(_)));
        assert!(matches!(
            Error::from(IndexError::File(file())),
            Error::InternalFile(_, _)
        ));
        assert!(matches!(
            Error::from(IndexError::SerializationFailure(serialization())),
            Error::SerializationFailure(_)
        ));
        assert!(matches!(
            Error::from(IndexError::PathInvalid("bad".into())),
            Error::InternalPathInvalid(_)
        ));
        // A variant with no destination stays wrapped rather than being
        // flattened onto a near-enough neighbour. That is contract too.
        assert!(matches!(
            Error::from(IndexError::RemoteManifestNotFound("x".into())),
            Error::OciIndex(_)
        ));
    }

    #[test]
    fn package_errors_flatten_onto_this_tiers_own_variants() {
        assert!(matches!(Error::from(PkgError::File(file())), Error::InternalFile(_, _)));
        assert!(matches!(
            Error::from(PkgError::OciClient(client())),
            Error::OciClient(_)
        ));
        assert!(matches!(Error::from(PkgError::Digest(digest())), Error::Digest(_)));
        assert!(matches!(
            Error::from(PkgError::SerializationFailure(serialization())),
            Error::SerializationFailure(_)
        ));
        assert!(matches!(Error::from(PkgError::EmptyPushSet), Error::Package(_)));
    }

    /// The case neither test above can see: `PackageError::Index(..)` re-enters
    /// the index conversion, so a `ClientError` two hops down must still land
    /// flat. A derive on either link buries it.
    #[test]
    fn a_client_error_two_conversions_down_still_arrives_flat() {
        let nested = PkgError::Index(IndexError::OciClient(client()));
        assert!(
            matches!(Error::from(nested), Error::OciClient(_)),
            "a ClientError inside a PackageError::Index must flatten across both impls"
        );
    }

    fn chain(error: &dyn std::error::Error) -> Vec<String> {
        std::iter::successors(Some(error), |cause| cause.source())
            .map(ToString::to_string)
            .collect()
    }

    /// `SessionError::Library` re-targeted silently at WP-34 — same spelling,
    /// different type. Its reader is `launch.rs`'s `successors` walk, so the
    /// walk is what gets asserted: the wrapped error must still be reachable
    /// as a link rather than collapsed into the head.
    #[test]
    fn a_session_library_error_still_yields_its_cause_to_a_source_walk() {
        let session = crate::activation::SessionError::Library(Error::OciClient(client()));
        let links = chain(&session);
        assert!(
            links.len() >= 2,
            "the wrapped error must remain a link in the chain, got {links:?}"
        );
        assert!(
            links.iter().any(|l| l.contains("bad creds")),
            "the ClientError's own text must survive the walk, got {links:?}"
        );
    }

    /// The same for `PackageErrorKind::Internal`, whose reader is
    /// `append_chain(.., entry.kind.source())` in this file.
    #[test]
    fn an_internal_package_error_still_yields_its_cause_to_a_source_walk() {
        let kind = PackageErrorKind::Internal(Error::OciClient(client()));
        let links = chain(&kind);
        assert!(
            links.len() >= 2,
            "the wrapped error must remain a link in the chain, got {links:?}"
        );
        assert!(
            links.iter().any(|l| l.contains("bad creds")),
            "the ClientError's own text must survive the walk, got {links:?}"
        );
    }
}
