// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the package-manager, launch, patch and record error family — the `ocx_package_manager` rung of the
//! ladder, here rather than in that crate because classification is `ocx_cli`'s alone.

use ocx_exit::ExitCode;

use ocx_package_manager::error::DependencyError;
use ocx_package_manager::error::Error as PackageManagerError;
use ocx_package_manager::error::PackageErrorKind;
use ocx_package_manager::launch::LaunchError;
use ocx_package_manager::patch::PatchError;
use ocx_package_manager::record::RecordsError;

use super::{ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for LaunchError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Defer to the wrapped `io::Error` so a permission-denied spawn
            // still reaches 77 and everything else falls through to the generic
            // failure — the behaviour the three exec sites already had before
            // they were folded into this seam.
            Self::Spawn { .. } => None,
            // A frame reached the seam with nothing to record. That is a wiring
            // fault in ocx, not something an operator can configure their way
            // out of, so it takes the generic code rather than a diagnostic one
            // that would send them looking at their config.
            Self::IncompleteRecordInputs { .. } => Some(ExitCode::Failure),
            // The same 74 an unwritable sink takes under the same posture: from
            // a wrapper script's side both are one condition — this invocation
            // could not be recorded and the operator said not to run it.
            Self::ExemptionRefused { .. } => Some(ExitCode::IoError),
            // The wrapped error owns the split between 74 (unwritable sink) and
            // 78 (malformed template), so the classification is delegated to it
            // rather than restated here and left to drift.
            Self::Records(e) => e.classify(),
        }
    }
}

impl ClassifyExitCode for PatchError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Network fetch failures delegate to the inner OCI client error's
            // classification so auth (80), unavailable (69), etc. propagate.
            Self::FetchFailed { source } => source.classify(),
            // Blob-write failures are I/O errors.
            Self::BlobWriteFailed { .. } => Some(ExitCode::IoError),
            // A deliberate local policy refused the resolution — not a fault.
            Self::PolicyBlocked { .. } => Some(ExitCode::PolicyBlocked),
            // Descriptor shape / version / parse issues = malformed data.
            Self::InvalidDescriptorJson { .. }
            | Self::UnsupportedVersion { .. }
            | Self::UnsupportedSnapshotVersion { .. }
            | Self::UnexpectedManifest { .. }
            | Self::UnexpectedArtifactType { .. }
            | Self::WrongLayerCount { .. }
            | Self::UnexpectedLayerMediaType { .. }
            | Self::LayerSizeExceeded { .. }
            | Self::LayerDigestMismatch { .. }
            | Self::ManifestDigestMismatch { .. }
            | Self::DescriptorTooLarge { .. } => Some(ExitCode::DataError),
        }
    }
}

impl ClassifyExitCode for PackageManagerError {
    fn classify(&self) -> Option<ExitCode> {
        // Batch variants wrap `Vec<PackageError>` with no `#[source]`, so the
        // chain walker never reaches the inner `PackageErrorKind`. Classify
        // the first package error directly — preserves per-package semantics
        // for single-failure cases.
        match self {
            // Box<PackageError>: deref to access kind directly.
            Self::SelfCheckFailed(pe) => pe.kind.classify(),
            Self::FindFailed(es)
            | Self::InstallFailed(es)
            | Self::UninstallFailed(es)
            | Self::DeselectFailed(es)
            | Self::ResolveFailed(es)
            | Self::InspectFailed(es)
            | Self::SelectFailed(es) => es.first().and_then(|pe| pe.kind.classify()),
            // ── E1: the variants this tier minted at WP-34 ──────────────
            // Each one is spelled exactly as `ocx_lib::Error`'s arm for the
            // same name, so `STANDS_IN_FOR`'s `SameDelegation` compares equal
            // and the exit code cannot have moved (DEC-23). Delegating arms
            // read `e.classify()`, never `e.classify()?` — the normaliser
            // strips a leading `return` and a binder and nothing else.
            Self::OfflineMode => Some(ExitCode::PolicyBlocked),
            Self::InternalFile(_, _) => Some(ExitCode::IoError),
            Self::LayerLayout(_) => None,
            Self::SymlinkWalk(e) => e.classify(),
            Self::InternalPathInvalid(_) => Some(ExitCode::Failure),
            Self::SerializationFailure(_) => Some(ExitCode::DataError),
            Self::UnsupportedMediaType(_, _) => Some(ExitCode::DataError),
            Self::MetadataBlobTooLarge { .. } => Some(ExitCode::DataError),
            Self::Platform(e) => e.classify(),
            Self::Project(e) => e.classify(),
            Self::ProjectRegistry(e) => e.classify(),
            Self::OciClient(e) => e.classify(),
            Self::Archive(e) => e.classify(),
            Self::Package(e) => e.as_ref().classify(),
            Self::OciIndex(e) => e.classify(),
            Self::FileStructure(e) => e.classify(),
            Self::Digest(e) => e.classify(),
            Self::Patch(e) => e.as_ref().classify(),
            Self::Dependency(e) => e.classify(),
            Self::PinnedIdentifier(e) => e.classify(),
            Self::Singleflight(e) => e.classify(),
            Self::LauncherUnsafeCharacter { .. } => Some(ExitCode::DataError),
            Self::ToolchainHomeNotAbsolute { .. } => Some(ExitCode::DataError),
            Self::LauncherPathNotUtf8 { .. } => Some(ExitCode::DataError),
            Self::Sign(e) => e.as_ref().classify(),
            Self::Verify(e) => e.as_ref().classify(),
        }
    }
}

impl ClassifyExitCode for PackageErrorKind {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::NotFound | Self::SymlinkNotFound(_) | Self::BlobNotFound(_) => ExitCode::NotFound,
            Self::OfflineManifestMissing(_) => ExitCode::PolicyBlocked,
            Self::SelectionAmbiguous(_)
            | Self::SymlinkRequiresTag
            | Self::DigestMissing
            | Self::EntrypointCollision { .. }
            | Self::FeatureMismatch { .. }
            // Every shim refusal is a claim the package's own metadata makes
            // and cannot honour, or a wire value that does not satisfy the
            // name grammar — malformed input, never a missing package.
            | Self::ShimNamesNotEnumerable { .. }
            | Self::ShimNameInvalid(_)
            | Self::ShimNameNotClaimed(_)
            | Self::ShimClaimUnfulfilled(_) => ExitCode::DataError,
            Self::TaskPanicked => ExitCode::Failure,
            // Required companion failure: delegate to the inner error's
            // classification so the exit code reflects the root cause (e.g.
            // NotFound if the companion is not published, Unavailable for
            // network errors, etc.). The `Box<PackageErrorKind>` source
            // implements `ClassifyExitCode` so we can recurse without walking
            // `std::error::Error::source()`.
            Self::RequiredCompanionFailed { source, .. } => return source.classify(),
            // Patch discovery errors delegate to the classify_error chain walker
            // so the inner PatchError's source chain (e.g. ClientError::Authentication
            // → AuthError, ClientError::Registry → Unavailable) is fully inspected.
            Self::PatchDiscovery(inner) => {
                return Some(super::classify_library_error(inner as &(dyn std::error::Error + 'static)));
            }
            // Delegate rather than restate 78 here: `ToolchainPathError` owns
            // its own wildcard-free `classify`, so a variant added there
            // compile-errors at that match and inherits whatever code its
            // author chose — one source of truth for the code, exactly as
            // `RequiredCompanionFailed` above defers to its source.
            Self::ToolchainPath(inner) => return inner.classify(),
            // Internal wraps a full `ocx_lib::Error` — walk through classify_error
            // so the inner chain is inspected via the generic entry point.
            Self::Internal(inner) => return Some(super::classify_library_error(inner)),
        })
    }
}

impl ClassifyExitCode for DependencyError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::Conflict { .. } => Some(ExitCode::DataError),
            // Defer to the wrapped singleflight error's source chain so the
            // underlying classifiable variant (e.g. `EntrypointCollision`)
            // wins over the generic "setup failed" wrapper. The chain walker
            // re-enters `try_classify` on the next cause.
            Self::SetupFailed(_) => None,
        }
    }
}

impl ClassifyExitCode for RecordsError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Io { .. } | Self::Serialize(_) => ExitCode::IoError,
            // A bad template is a config parse error, not an I/O fault — the
            // operator fixes it by editing `[records] name`, and 74 would send
            // them looking at disk permissions instead.
            Self::TemplateUnknownPlaceholder { .. }
            | Self::TemplateNotUnique
            | Self::NameNotAFilename { .. }
            // Same reasoning: a posture with nothing to write to is a fault in
            // the config chain, fixed by adding a `dir` or dropping `required`.
            | Self::RequiredWithoutSink => ExitCode::ConfigError,
            // Not `SymlinkWalkError::Ancestor`'s 64: that precedent refuses a
            // path the user typed as an *argument*, where usage is the fault.
            // This sink arrives from `[records] dir` / `OCX_RECORDS_DIR` /
            // `--records-dir` — configuration, which the operator fixes by
            // editing a file, the same as a bad template.
            Self::SinkSymlink { .. } => ExitCode::ConfigError,
        })
    }
}

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, PackageManagerError);
    downcast_arm!(cause, PackageErrorKind);
    downcast_arm!(cause, DependencyError);
    downcast_arm!(cause, PatchError);
    downcast_arm!(cause, RecordsError);
    downcast_arm!(cause, LaunchError);
    None
}

#[cfg(test)]
mod tests {
    use ocx_exit::ExitCode;

    /// The chain walk the binary performs, over one error.
    fn classify<E: std::error::Error + 'static>(err: E) -> ExitCode {
        crate::exit::classify_library_error(&err as &(dyn std::error::Error + 'static))
    }

    use super::*;
    use ocx_oci::client::error::ClientError;
    use ocx_oci::identifier::Identifier;

    use ocx_package_manager::patch::snapshot::PatchSnapshot;

    use std::path::PathBuf;
    use tempfile::TempDir;

    // ── moved from ocx_package_manager::launch with the impl ──

    #[test]
    fn an_incomplete_frame_takes_the_generic_exit_code() {
        let error = LaunchError::IncompleteRecordInputs {
            command: "ocx exec".to_string(),
        };

        // A wiring fault in ocx, not something an operator can configure their
        // way out of, so it must not take a diagnostic code that would send them
        // looking at their config.
        assert_eq!(error.classify(), Some(ExitCode::Failure));
    }

    #[test]
    fn a_spawn_failure_defers_its_exit_code_to_the_io_error() {
        let error = LaunchError::Spawn {
            resolved: PathBuf::from("/store/cmake/content/bin/cmake"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };

        // `None` hands the decision to the chain walker, which reaches the
        // `io::Error` and its 77 — the behaviour the three exec sites had before
        // they were folded into this seam.
        assert_eq!(error.classify(), None);
    }

    // ── moved from ocx_package_manager::composer with the impl ──

    // ── moved from ocx_package_manager::error with the impl ──

    // ── moved from ocx_package_manager::launcher::body with the impl ──

    /// The refusal is a `DataError` (65) at the CLI boundary — the same code its
    /// sibling `LauncherUnsafeCharacter` carries, because both are a
    /// well-formed request naming an unusable value.
    #[test]
    fn the_absoluteness_refusal_classifies_as_a_data_error() {
        let error = ocx_package_manager::Error::ToolchainHomeNotAbsolute {
            value: "rel/proj".to_string(),
        };
        assert_eq!(
            error.classify(),
            Some(ExitCode::DataError),
            "a relative toolchain home must exit 65, not fall through to the generic failure code"
        );
    }

    /// The refusal is a `DataError` (65), the same code both its siblings
    /// carry: a well-formed request naming a value that cannot be rendered.
    #[test]
    fn the_utf8_refusal_classifies_as_a_data_error() {
        assert_eq!(
            ocx_package_manager::Error::LauncherPathNotUtf8 {
                path: "/w/pro\u{fffd}j".to_string(),
            }
            .classify(),
            Some(ExitCode::DataError),
            "an unrenderable baked path must exit 65, not fall through to the generic code"
        );
    }

    // ── moved from ocx_package_manager::tasks::install with the impl ──

    // ── moved from ocx_package_manager::tasks::patch_discovery with the impl ──

    // ── moved from ocx_package_manager::tasks::sbom with the impl ──

    // ── moved from ocx_package_manager::patch::snapshot with the impl ──

    /// A snapshot file carrying a superseded `version` is refused with an error
    /// naming `ocx patch freeze`. There is no reader for the older shape: a
    /// snapshot is derived state, re-resolved offline in seconds.
    #[tokio::test(flavor = "multi_thread")]
    async fn superseded_snapshot_version_is_refused_with_a_freeze_remedy() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("patches.snapshot.json");
        std::fs::write(
            &path,
            r#"{"version":1,"companions":{"example.com/ca-bundle":"sha256:cc"},"descriptors":{}}"#,
        )
        .unwrap();

        let error = PatchSnapshot::read(&path)
            .await
            .expect_err("a superseded snapshot version must not be read");

        let message = format!("{error}");
        assert!(
            message.contains("ocx patch freeze"),
            "the refusal must name the command that rewrites the snapshot; got: {message}"
        );
        assert!(
            message.contains("version 1"),
            "the refusal must name the version it found; got: {message}"
        );
        assert_eq!(
            ClassifyExitCode::classify(&error),
            Some(ExitCode::DataError),
            "a stale persisted format is malformed input for this binary (65)"
        );
    }

    // ── moved from ocx_package_manager::record::policy with the impl ──

    /// `[records] required = true` with no sink is **78**, not 74.
    ///
    /// Recovered from the pin row `crates/ocx_lib/src/record/error.rs:98`
    /// (`| Self::RequiredWithoutSink => ExitCode::ConfigError`). WP-10 took the
    /// assertion out of
    /// `record/policy.rs::an_explicit_required_true_without_a_sink_is_refused`.
    ///
    /// A posture with nothing to write to is a fault in the config chain, fixed
    /// by adding a `dir` or dropping `required`; 74 would send the operator
    /// looking at disk permissions instead. Its `Io` sibling is asserted beside
    /// it so the split is what is pinned, not a constant.
    #[test]
    fn an_explicit_required_true_without_a_sink_is_refused() {
        assert_eq!(
            RecordsError::RequiredWithoutSink.classify(),
            Some(ExitCode::ConfigError)
        );
        assert_eq!(
            crate::exit::classify_library_error(&RecordsError::RequiredWithoutSink),
            ExitCode::ConfigError,
            "the remedy is an edit to the config chain, so it must not arrive as the sink's 74"
        );

        let unwritable = RecordsError::Io {
            path: PathBuf::from("/var/records/run.json"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert_eq!(
            unwritable.classify(),
            Some(ExitCode::IoError),
            "the contrast is what makes the 78 above a statement rather than a constant"
        );
    }

    // ── moved from ocx_package_manager::tasks::attest with the impl ──

    // ── moved from ocx_package_manager::error with the impl ──

    use ocx_package_manager::error::PackageError;

    /// A batch classifies from its input-order-first entry, not from whichever
    /// task happened to finish first.
    #[test]
    fn inspect_failed_classifies_from_first_error() {
        let errors = vec![
            PackageError::new(
                ocx_oci::Identifier::new_registry("a", "example.com"),
                PackageErrorKind::NotFound,
            ),
            PackageError::new(
                ocx_oci::Identifier::new_registry("b", "example.com"),
                PackageErrorKind::SymlinkRequiresTag,
            ),
        ];
        assert_eq!(
            PackageManagerError::InspectFailed(errors).classify(),
            Some(ExitCode::NotFound)
        );
    }

    #[test]
    fn select_failed_classifies_from_first_error() {
        let errors = vec![PackageError::new(
            ocx_oci::Identifier::new_registry("a", "example.com"),
            PackageErrorKind::SelectionAmbiguous(vec![]),
        )];
        assert_eq!(
            PackageManagerError::SelectFailed(errors).classify(),
            Some(ExitCode::DataError)
        );
    }

    // ── moved from ocx_package_manager::error with the impl ──

    /// The install/`pull_layer` route's exit-code contract, end to end. A
    /// registry-served hostile layer refuses inside `pull_layer`; the archive
    /// traversal error surfaces wrapped as `ClientError::internal(...)`, is folded
    /// into a `PackageErrorKind` at the `?` site, then batched into `InstallFailed`.
    /// The batch must classify to `DataError` (65) — the same code the
    /// operator-chosen `--extract` route already returns. A hardcoded `Failure`
    /// in `ClientError::Internal`'s `classify` made `ocx package install` exit 1
    /// on this security refusal, which is what
    /// `test_a_registry_served_hostile_layer_is_refused_on_install` observed
    /// before the delegation fix. `classify_error` on the `anyhow`-boxed batch
    /// mirrors what `main.rs` runs.
    #[test]
    fn install_batch_with_a_client_wrapped_traversal_refusal_classifies_as_data_error() {
        // The bare archive refusal, exactly as `pull_layer_with_caps` now hands
        // it over: E1 made the archive tier raise its own error, so the wide
        // `ocx_lib::Error::Archive` wrapper is no longer on this route and a
        // test that still built one would stop pinning the real chain.
        let traversal = ocx_util::archive::Error::SymlinkEscape {
            link: std::path::PathBuf::from("escape"),
            target: std::path::PathBuf::from("../../../../etc"),
        };
        // The exact wrap the registry layer-pull route produces: the archive
        // refusal boxed under `ClientError::internal`, folded into a
        // `PackageErrorKind` the way `From<ClientError>` does at the `?` site.
        let kind = PackageErrorKind::from(ClientError::internal(traversal));
        let entry = PackageError::new(
            ocx_oci::Identifier::new_registry("cmake", "example.com").clone_with_tag("1.1.0"),
            kind,
        );
        let boxed = anyhow::Error::from(PackageManagerError::InstallFailed(vec![entry]));
        assert_eq!(crate::exit::classify_library_error(boxed.as_ref()), ExitCode::DataError);
    }

    // ── moved from ocx_package_manager::tasks::patch_discovery with the impl ──

    /// `PackageErrorKind::RequiredCompanionFailed` delegates exit-code
    /// classification to the inner `source` error.
    ///
    /// This ensures the exit code reflects the root cause (e.g. `NotFound`
    /// → exit 79) rather than a generic companion-failure code.
    ///
    /// Traces: STUB MANIFEST §3 classification arm delegates to `source.classify()`.
    #[test]
    fn required_companion_failed_exit_code_delegates_to_source() {
        use crate::exit::ClassifyExitCode;
        use ocx_exit::ExitCode;
        use ocx_package_manager::error::PackageErrorKind;

        // Source = NotFound → should classify to NotFound (79).
        let kind = PackageErrorKind::RequiredCompanionFailed {
            companion: Identifier::parse("patches.corp.com/ca:latest").expect("valid"),
            source: Box::new(PackageErrorKind::NotFound),
        };
        let code = kind.classify();
        assert_eq!(
            code,
            Some(ExitCode::NotFound),
            "RequiredCompanionFailed with NotFound source must classify as NotFound"
        );
    }

    // recovered from crate::exit::classify
    /// `DependencyError::SetupFailed` itself returns `None` from `classify()` so the
    /// chain walker continues via `source()`. With `singleflight::Error::Failed` carrying
    /// `#[source]` and `SharedError::source()` exposing the wrapped error directly, the
    /// walker reaches the inner `PackageErrorKind::EntrypointCollision` and recovers
    /// `ExitCode::DataError`. Before the chain fix, the walker stopped at the wrapper
    /// and fell through to `ExitCode::Failure` — masking the typed discriminant.
    #[test]
    fn dependency_setup_failed_singleflight_collision_classifies_to_data_error() {
        use ocx_package::metadata::entrypoint::EntrypointName;
        use ocx_util::singleflight;

        let name = EntrypointName::try_from("cmake").unwrap();
        let hex = "a".repeat(64);
        let id_a: ocx_oci::Identifier = format!("ocx.sh/foo:1.0@sha256:{hex}").parse().unwrap();
        let id_b: ocx_oci::Identifier = format!("ocx.sh/bar:1.0@sha256:{hex}").parse().unwrap();
        let inner = PackageErrorKind::EntrypointCollision {
            name,
            owners: vec![
                ocx_oci::PinnedIdentifier::try_from(id_a).unwrap(),
                ocx_oci::PinnedIdentifier::try_from(id_b).unwrap(),
            ],
        };
        let shared = singleflight::SharedError::for_test(inner);
        let err = DependencyError::SetupFailed(singleflight::Error::Failed(shared));
        assert_eq!(classify(err), ExitCode::DataError);
    }
}
