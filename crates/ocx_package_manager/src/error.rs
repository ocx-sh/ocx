// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_exit::{ClassifyExitCode, Pick, Row};
use ocx_package::metadata::entrypoint::EntrypointName;
use ocx_package::metadata::{BinaryError, BinaryName};
use ocx_store::file_structure;

/// Task-level error: one variant per command, one [`PackageError`] per failed package.
///
/// A batch's exit code comes from its first element's [`PackageError::kind`], so it depends on input order.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "PackageManagerError")]
pub enum Error {
    /// A find operation failed for one or more packages.
    #[error("{}", format_batch("find", _0))]
    #[exit(with = first_failure, rows((defer(Failure), slug = "package_task_failed", summary = "A per-package operation failed with an unclassified cause")))]
    FindFailed(Vec<PackageError>),
    /// An install operation failed for one or more packages.
    #[error("{}", format_batch("install", _0))]
    #[exit(with = first_failure, rows((defer(Failure), slug = "package_task_failed", summary = "A per-package operation failed with an unclassified cause")))]
    InstallFailed(Vec<PackageError>),
    /// An uninstall operation failed for one or more packages.
    #[error("{}", format_batch("uninstall", _0))]
    #[exit(with = first_failure, rows((defer(Failure), slug = "package_task_failed", summary = "A per-package operation failed with an unclassified cause")))]
    UninstallFailed(Vec<PackageError>),
    /// A deselect operation failed for one or more packages.
    #[error("{}", format_batch("deselect", _0))]
    #[exit(with = first_failure, rows((defer(Failure), slug = "package_task_failed", summary = "A per-package operation failed with an unclassified cause")))]
    DeselectFailed(Vec<PackageError>),
    /// A resolve operation failed for one or more packages.
    #[error("{}", format_batch("resolve", _0))]
    #[exit(with = first_failure, rows((defer(Failure), slug = "package_task_failed", summary = "A per-package operation failed with an unclassified cause")))]
    ResolveFailed(Vec<PackageError>),
    /// An inspect operation failed for one or more packages.
    #[error("{}", format_batch("inspect", _0))]
    #[exit(with = first_failure, rows((defer(Failure), slug = "package_task_failed", summary = "A per-package operation failed with an unclassified cause")))]
    InspectFailed(Vec<PackageError>),
    /// A select operation failed for one or more packages.
    #[error("{}", format_batch("select", _0))]
    #[exit(with = first_failure, rows((defer(Failure), slug = "package_task_failed", summary = "A per-package operation failed with an unclassified cause")))]
    SelectFailed(Vec<PackageError>),
    /// Patch discovery failed for one or more base packages.
    #[error("{}", format_batch("discover patches for", _0))]
    #[exit(with = first_failure, rows((defer(Failure), slug = "package_task_failed", summary = "A per-package operation failed with an unclassified cause")))]
    DiscoverFailed(Vec<PackageError>),
    /// The self-update check failed before any install was attempted (boxed: the type is recursive).
    #[error("self-update check failed: {}", render_entry(_0))]
    #[exit(delegate = 0.kind)]
    SelfCheckFailed(Box<PackageError>),

    // ── The tier's own root error ───────────────────────────────────────────
    // Each variant's `#[exit(...)]` fixes its exit code; merging or splitting one moves that code.
    /// A network operation was attempted while in offline mode.
    #[error("network operation attempted in offline mode")]
    #[exit(
        PolicyBlocked,
        slug = "offline_mode",
        summary = "A network operation was attempted in offline mode"
    )]
    OfflineMode,

    /// JSON serialization or deserialization failed.
    #[error("JSON serialization error")]
    #[exit(
        DataError,
        slug = "package_manager_serialization",
        summary = "A package document could not be serialized"
    )]
    SerializationFailure(#[from] serde_json::Error),
    /// An OCI index operation failed.
    // No `#[from]`: the hand-written `From` flattens three variants onto this enum's, or their exit code moves.
    #[error(transparent)]
    #[exit(delegate = 0)]
    OciIndex(ocx_index::error::Error),

    /// A dependency graph operation failed.
    #[error(transparent)]
    #[exit(delegate = 0)]
    Dependency(#[from] DependencyError),
    /// A package operation failed.
    #[error(transparent)]
    #[exit(delegate = 0)]
    Package(Box<ocx_package::error::Error>),
    /// An archive operation failed.
    #[error(transparent)]
    #[exit(delegate = 0)]
    Archive(#[from] ocx_util::archive::Error),

    /// A string baked into a generated launcher contains a launcher-unsafe character.
    #[error("launcher-unsafe character {character:?} in {value:?}; {}", launcher_unsafe_hint(*character))]
    #[exit(
        DataError,
        slug = "launcher_unsafe_character",
        summary = "A value baked into a generated launcher contains an unsafe character"
    )]
    LauncherUnsafeCharacter { value: String, character: char },

    /// A path baked verbatim into a generated launcher or sidecar is not valid UTF-8; `path` is its lossy rendering.
    #[error("path baked into a generated launcher is not valid UTF-8: {path:?}")]
    #[exit(
        DataError,
        slug = "launcher_path_not_utf8",
        summary = "A path baked into a generated launcher is not valid UTF-8"
    )]
    LauncherPathNotUtf8 { path: String },

    /// A toolchain home baked into a rendered trampoline or its `.exec` sidecar is not absolute.
    #[error("toolchain home is not an absolute path: {value:?}")]
    #[exit(
        DataError,
        slug = "toolchain_home_not_absolute",
        summary = "The toolchain home is not an absolute path"
    )]
    ToolchainHomeNotAbsolute { value: String },
    /// An OCI signature verification failed.
    #[error(transparent)]
    #[exit(delegate = 0)]
    Verify(#[from] Box<ocx_sign::verify::VerifyError>),
    /// A digest string could not be parsed.
    #[error(transparent)]
    #[exit(delegate = 0)]
    Digest(#[from] ocx_oci::digest::error::DigestError),
    /// A file structure operation failed.
    #[error(transparent)]
    #[exit(delegate = 0)]
    FileStructure(#[from] ocx_store::file_structure::error::Error),

    /// A file I/O error with path context.
    ///
    /// The cause is interpolated, not `#[source]`: both prints it twice under `{err:#}`, and
    /// `#[source]` alone leaves every `to_string()` naming only the path.
    #[error("internal file error for '{path}': {cause}", path = .0.display(), cause = .1)]
    #[exit(
        IoError,
        slug = "package_manager_internal_file",
        summary = "Reading or writing an internal package file failed"
    )]
    InternalFile(std::path::PathBuf, std::io::Error),

    /// An OCI signing operation failed (boxed for `clippy::result_large_err`).
    #[error(transparent)]
    #[exit(delegate = 0)]
    Sign(#[from] Box<ocx_sign::sign::SignError>),

    /// A singleflight coordination error.
    #[error("singleflight coordination failed")]
    #[exit(delegate = 0)]
    Singleflight(#[from] ocx_util::singleflight::Error),
    /// A local materialization (`pull_local`) found a layer absent from the layer store.
    // Never fetched as a fallback: dialling the name as typed would bypass index routing.
    #[error(
        "layer {digest} of '{identifier}' is not staged locally, and a local materialization has no registry to fetch it from"
    )]
    #[exit(
        Failure,
        slug = "layer_not_staged",
        summary = "A layer the package needs was not staged"
    )]
    LayerNotStaged { identifier: String, digest: String },
    /// An OCI client operation failed.
    #[error(transparent)]
    #[exit(delegate = 0)]
    OciClient(#[from] ocx_oci::client::error::ClientError),

    /// A per-layer layout annotation could not be resolved into a placement.
    // A `#[source]` field, never `io::Error::other`: that hides the cause and exits 74 instead of 65.
    #[error("layer layout resolution failed")]
    #[exit(
        chain,
        fallback(
            Failure,
            slug = "layer_layout_failed",
            summary = "Resolving a layer layout failed with an unclassified cause"
        )
    )]
    LayerLayout(#[source] ocx_oci::layer_layout::LayerLayoutError),

    /// An unsupported OCI media type was encountered.
    #[error("unsupported media type '{media_type}', expected media types are: {supported}", media_type = .0, supported = .1.join(", "))]
    #[exit(
        DataError,
        slug = "unsupported_media_type",
        summary = "A manifest carries an unsupported media type"
    )]
    UnsupportedMediaType(String, &'static [&'static str]),

    /// A metadata config blob exceeded the size cap, by declared or fetched length
    /// (`adr_inspect_metadata_closure.md` § "Config-blob size cap").
    #[error("metadata blob size {size} bytes exceeds the {max}-byte cap")]
    #[exit(
        DataError,
        slug = "metadata_blob_too_large",
        summary = "A package metadata blob exceeds the size cap"
    )]
    MetadataBlobTooLarge { size: i64, max: usize },

    // Destinations for the flattening `From`s below; removing one nests it under a wrapper and can move its exit code.
    /// A platform parsing or validation error.
    #[error(transparent)]
    #[exit(delegate = 0)]
    Platform(#[from] ocx_oci::platform::error::PlatformError),
    /// A pinned identifier validation failed.
    #[error(transparent)]
    #[exit(delegate = 0)]
    PinnedIdentifier(#[from] ocx_oci::pinned_package_ref::PinnedIdentifierError),
    /// A path has an unexpected structure.
    #[error("path '{}' has an unexpected structure", .0.display())]
    #[exit(
        Failure,
        slug = "internal_path_invalid",
        summary = "An internal store path has an unexpected structure"
    )]
    InternalPathInvalid(std::path::PathBuf),

    /// A project-tier configuration or lock operation failed.
    #[error(transparent)]
    #[exit(delegate = 0)]
    Project(#[from] ocx_project::error::Error),
    /// A patch-domain operation failed outside per-package discovery (boxed: the enums are mutually recursive).
    #[error(transparent)]
    #[exit(delegate = 0)]
    Patch(Box<crate::patch::PatchError>),

    /// A symlink walk failed.
    #[error(transparent)]
    #[exit(delegate = 0)]
    SymlinkWalk(ocx_util::fs::SymlinkWalkError),

    #[error(transparent)]
    #[exit(delegate = 0)]
    ProjectRegistry(#[from] ocx_project::registry::error::Error),
}

/// A batch answers as its first member when that member decides the exit code, else defers to the walker.
fn first_failure(error: &Error, [row]: [Row; 1]) -> Pick<'_> {
    let (Error::FindFailed(errors)
    | Error::InstallFailed(errors)
    | Error::UninstallFailed(errors)
    | Error::DeselectFailed(errors)
    | Error::ResolveFailed(errors)
    | Error::InspectFailed(errors)
    | Error::SelectFailed(errors)
    | Error::DiscoverFailed(errors)) = error
    else {
        return Pick::row(row);
    };
    match errors.first() {
        Some(first) if first.kind.classify().is_some() => Pick::delegate(&first.kind),
        _ => Pick::row(row),
    }
}

/// An error tied to a specific package.
// `kind` has no `#[source]`: classification never walks `source()`, and it would print the kind twice.
#[derive(Debug, thiserror::Error)]
#[error("{}{kind}", identifier_prefix(identifier))]
#[non_exhaustive]
pub struct PackageError {
    pub identifier: ocx_oci::PackageRef,
    pub kind: PackageErrorKind,
}

impl PackageError {
    pub fn new(identifier: ocx_oci::PackageRef, kind: PackageErrorKind) -> Self {
        Self { identifier, kind }
    }
}

/// The `"<identifier> — "` lead-in of a [`PackageError`] message; empty for the placeholder
/// identifier [`crate::Error::from`] fabricates, which would render as a bare `/`.
fn identifier_prefix(identifier: &ocx_oci::PackageRef) -> String {
    if identifier.registry().is_empty() && identifier.repository().is_empty() {
        String::new()
    } else {
        format!("{identifier} — ")
    }
}

/// Payload for [`PackageErrorKind::OfflineManifestMissing`].
#[derive(Debug)]
pub struct OfflineManifestMissing {
    pub identifier: ocx_oci::PackageRef,
    pub digest: ocx_oci::Digest,
}

/// Payload for the shim refusals naming a package and one of its claimed names.
///
/// `package` may be a dependency deep in the closure, not [`PackageError::identifier`].
#[derive(Debug)]
pub struct ShimClaim {
    pub package: ocx_oci::PinnedPackageRef,
    pub name: BinaryName,
}

/// The cause of a single-package failure.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum PackageErrorKind {
    /// The package was not found in the index or object store.
    #[error("package not found")]
    #[exit(NotFound, slug = "package_not_found", summary = "The package does not exist")]
    NotFound,
    /// Offline mode: the tag pointer is cached locally but the manifest blob is not.
    #[error(
        "manifest {} is not in the local cache; run `ocx install {}` online to populate it",
        _0.digest,
        _0.identifier
    )]
    #[exit(
        PolicyBlocked,
        slug = "offline_manifest_missing",
        summary = "Offline mode and the manifest is not stored locally"
    )]
    OfflineManifestMissing(Box<OfflineManifestMissing>),
    /// A referenced blob was not present in the registry; the identifier carries the blob digest.
    #[error("blob not found: {0}")]
    #[exit(NotFound, slug = "blob_not_found", summary = "The registry has no such blob")]
    BlobNotFound(Box<ocx_oci::PinnedOciIdentifier>),
    /// Multiple candidates matched the platform selection.
    #[error("ambiguous selection: {}", _0.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(", "))]
    #[exit(
        DataError,
        slug = "selection_ambiguous",
        summary = "The selection matches more than one package"
    )]
    SelectionAmbiguous(Vec<ocx_oci::PackageRef>),
    /// A symlink-based path was requested but the identifier carries a digest.
    #[error("symlink resolution requires a tag, not a digest")]
    #[exit(
        DataError,
        slug = "symlink_requires_tag",
        summary = "Symlink resolution needs a tag, not a digest"
    )]
    SymlinkRequiresTag,
    /// The requested install link does not exist, or does not lead into the package store.
    #[error("{}", match _0 {
        crate::composer::LinkSource::Candidate => "no installed candidate".to_string(),
        crate::composer::LinkSource::Current => "no selected version".to_string(),
        crate::composer::LinkSource::Path(path) => format!("no installed package at '{}'", path.display()),
    })]
    #[exit(
        NotFound,
        slug = "symlink_not_found",
        summary = "The package has no candidate or current symlink to resolve"
    )]
    SymlinkNotFound(crate::composer::LinkSource),
    /// `install --link` was given a path that holds something other than an ocx package link.
    #[error("'{}' exists and is not an ocx package link; refusing to replace it", _0.display())]
    #[exit(
        DataError,
        slug = "link_path_occupied",
        summary = "The install link path holds something other than an ocx package link"
    )]
    LinkPathOccupied(std::path::PathBuf),
    /// A spawned task panicked unexpectedly.
    #[error("task panicked unexpectedly")]
    #[exit(Failure, slug = "task_panicked", summary = "A package task panicked")]
    TaskPanicked,
    /// The identifier has no digest after resolution.
    #[error("identifier has no digest after resolution")]
    #[exit(
        DataError,
        slug = "digest_missing",
        summary = "The identifier carries no digest after resolution"
    )]
    DigestMissing,
    /// Two or more packages in the closure's interface surface declare the same entrypoint name.
    #[error(
        "entrypoint name collision: '{name}' declared by {} packages: {}; deselect one before selecting another",
        owners.len(),
        owners.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
    )]
    #[exit(
        DataError,
        slug = "entrypoint_collision",
        summary = "Two packages declare the same entrypoint name"
    )]
    EntrypointCollision {
        name: EntrypointName,
        owners: Vec<ocx_oci::PinnedPackageRef>,
    },

    /// A `required = true` companion failed to install or compose, failing the whole operation.
    #[error("required companion '{companion}' could not be applied")]
    #[exit(delegate = source)]
    RequiredCompanionFailed {
        /// Identifier of the companion package that failed to install.
        companion: ocx_oci::PackageRef,
        /// The underlying package error kind from the companion's install.
        #[source]
        source: Box<PackageErrorKind>,
    },

    /// Patch discovery failed with a domain-level patch error.
    #[error("patch discovery error")]
    #[exit(
        chain = 0,
        fallback(
            Failure,
            slug = "patch_discovery_failed",
            summary = "Discovering patches failed with an unclassified cause"
        )
    )]
    PatchDiscovery(#[source] crate::patch::PatchError),

    /// No candidate sharing the host's os+arch has `os.features` the host provides.
    #[error(
        "feature mismatch: host provides {}; available platforms: {}; pass --platform <os/arch[+features]> to override",
        host_features.join(", "),
        available.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
    )]
    #[exit(
        DataError,
        slug = "feature_mismatch",
        summary = "The package needs a platform feature this host lacks"
    )]
    FeatureMismatch {
        /// The `os.features` values the host reported (e.g. `["libc.glibc"]`).
        host_features: Vec<String>,
        /// The candidates sharing the host os+arch.
        available: Vec<ocx_oci::Platform>,
    },

    /// A closure node declares neither `binaries` nor entry points, so no shim can be generated.
    #[error(
        "cannot defer '{package}': it claims no binaries and no entry points, so its interface names are not enumerable"
    )]
    #[exit(
        DataError,
        slug = "shim_names_not_enumerable",
        summary = "The package's interface names cannot be enumerated"
    )]
    ShimNamesNotEnumerable { package: ocx_oci::PinnedPackageRef },

    /// A shim name is not a valid [`BinaryName`]; raised by both `ocx launcher shim` and `prepare_lazy`.
    #[error("invalid shim name")]
    #[exit(
        DataError,
        slug = "shim_name_invalid",
        summary = "A shim name is not a valid binary name"
    )]
    ShimNameInvalid(#[source] BinaryError),

    /// `ocx launcher shim` was invoked under a name outside the composed name set.
    #[error("'{}' is not an interface name declared by '{}'", _0.name, _0.package)]
    #[exit(
        DataError,
        slug = "shim_name_not_claimed",
        summary = "A shim name is not an interface name the package declares"
    )]
    ShimNameNotClaimed(Box<ShimClaim>),

    /// The package materialized, but a name its metadata claimed is not on the composed `PATH`.
    #[error(
        "'{}' claims the name '{}', but no such executable is present after materialization",
        _0.package,
        _0.name
    )]
    #[exit(
        DataError,
        slug = "shim_claim_unfulfilled",
        summary = "A declared interface name does not exist in the package"
    )]
    ShimClaimUnfulfilled(Box<ShimClaim>),

    /// A group or entry name cannot become a path component of a rendered toolchain tree.
    #[error(transparent)]
    #[exit(delegate = 0)]
    ToolchainPath(#[from] file_structure::ToolchainPathError),

    /// An underlying internal error.
    #[error(transparent)]
    #[exit(
        chain = 0,
        fallback(
            Failure,
            slug = "package_internal",
            summary = "A package operation failed with an unclassified cause"
        )
    )]
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
/// A batch exposes no single `source()`, so without this every cause below a kind is dropped
/// from the message; it walks `kind.source()` because the entry itself reports none.
fn render_entry(entry: &PackageError) -> String {
    use std::error::Error as _;

    let mut out = entry.to_string();
    ocx_util::error::append_chain(&mut out, entry.kind.source());
    out
}

/// Errors from dependency resolution operations.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum DependencyError {
    /// Two or more packages on the active surface resolve the same repository to different digests.
    #[error("conflicting versions for {repository}: {}", identifiers.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", "))]
    #[exit(
        DataError,
        slug = "dependency_conflict",
        summary = "Dependencies pin conflicting versions of one repository"
    )]
    Conflict {
        repository: ocx_oci::Repository,
        identifiers: Vec<ocx_oci::PinnedPackageRef>,
    },
    /// Dependency setup coordination failed.
    #[error("dependency setup failed")]
    #[exit(
        chain,
        fallback(
            Failure,
            slug = "dependency_setup_failed",
            summary = "Setting up a dependency failed with an unclassified cause"
        )
    )]
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
            ocx_oci::PackageRef::new_registry("cmake", "example.com").clone_with_tag("3.28"),
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
                ocx_oci::PackageRef::new_registry("cmake", "example.com"),
                PackageErrorKind::Internal(crate::error::file_error(
                    "/tmp/a",
                    std::io::Error::other("first cause fixture"),
                )),
            ),
            PackageError::new(
                ocx_oci::PackageRef::new_registry("ninja", "example.com"),
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
            ocx_oci::PackageRef::new_registry("ocx/cli", "ocx.sh"),
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
            ocx_oci::PackageRef::new_registry("cmake", "example.com"),
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
            ocx_oci::PackageRef::new_registry("java", "example.com"),
            PackageErrorKind::RequiredCompanionFailed {
                companion: ocx_oci::PackageRef::new_registry("license-server", "patches.corp.com"),
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
            rendered.contains("required companion 'patches.corp.com/license-server' could not be applied"),
            "the message names the companion and covers a resolve-time failure, not only an install; got: {rendered}"
        );
    }
}

/// `Result` for the whole tier.
pub type Result<T> = std::result::Result<T, Error>;

pub fn file_error(path: impl AsRef<std::path::Path>, error: std::io::Error) -> Error {
    Error::InternalFile(path.as_ref().to_path_buf(), error)
}

/// Flatten the index tier's errors onto this tier's own variants.
// Never a derived `#[from]`: wrapping hides the inner `ClientError` from `source()`, so retries stop and exit 75 becomes 69 (`flattening_shape`).
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

/// Flatten the package tier's errors onto this tier's own variants; `Index` re-enters the impl above.
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
    /// Lift a [`PackageErrorKind`] into the tier error under `identifier`.
    pub fn package(identifier: ocx_oci::PackageRef, kind: PackageErrorKind) -> Self {
        match kind {
            PackageErrorKind::Internal(e) => e,
            other => Self::ResolveFailed(vec![PackageError::new(identifier, other)]),
        }
    }
}

impl From<PackageErrorKind> for Error {
    fn from(kind: PackageErrorKind) -> Self {
        Error::package(ocx_oci::PackageRef::new_registry("", ""), kind)
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
    //! Mirrors the pre-split crate's `flattening_shape` guard, which exists because an
    //! earlier split shipped a derive where a flattening conversion belonged: the wrappers
    //! are `#[error(transparent)]`, so `source()` steps over the inner error
    //! without yielding it, retries stop being spent and an exit code moves
    //! with nothing failing at the conversion.
    //!
    //! The chain assertions at the bottom are the other half, and they are
    //! about a change of *meaning* rather than of code. `SessionError::Library`
    //! and `PackageErrorKind::Internal` both spell their field `crate::Error`,
    //! which meant `ocx_lib::Error` before the crate split and means this tier's error
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

    /// `SessionError::Library` re-targeted silently in the crate split — same spelling,
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
