// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only: the classification tests of the `ocx_package_manager` family. Its types declare their own codes with `#[derive(Classify)]`.

use ocx_package_manager::error::DependencyError;
use ocx_package_manager::error::Error as PackageManagerError;
use ocx_package_manager::error::PackageErrorKind;
use ocx_package_manager::launch::LaunchError;
use ocx_package_manager::patch::PatchError;
use ocx_package_manager::record::RecordsError;

#[cfg(test)]
mod tests {
    use ocx_exit::{ClassifyExitCode, ExitCode};

    /// The chain walk the binary performs, over one error.
    fn classify<E: std::error::Error + 'static>(err: E) -> ExitCode {
        crate::exit::classify_library_error(&err as &(dyn std::error::Error + 'static))
    }

    use super::*;
    use ocx_oci::client::error::ClientError;
    use ocx_oci::package_ref::PackageRef;

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
                ocx_oci::PackageRef::new_registry("a", "example.com"),
                PackageErrorKind::NotFound,
            ),
            PackageError::new(
                ocx_oci::PackageRef::new_registry("b", "example.com"),
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
            ocx_oci::PackageRef::new_registry("a", "example.com"),
            PackageErrorKind::SelectionAmbiguous(vec![]),
        )];
        assert_eq!(
            PackageManagerError::SelectFailed(errors).classify(),
            Some(ExitCode::DataError)
        );
    }

    /// A vanished required descriptor names discovery, not install, and exits 79.
    #[test]
    fn discover_failed_classifies_a_vanished_descriptor_as_not_found() {
        let descriptor = ocx_oci::PackageRef::new_registry("global", "patches.example.com");
        let errors = vec![PackageError::new(
            ocx_oci::PackageRef::new_registry("a", "example.com"),
            PackageErrorKind::PatchDiscovery(PatchError::DescriptorVanished {
                identifier: Box::new(descriptor),
            }),
        )];
        let error = PackageManagerError::DiscoverFailed(errors);
        assert!(
            error.to_string().starts_with("failed to discover patches for package"),
            "got: {error}"
        );
        assert_eq!(crate::exit::classify_library_error(&error), ExitCode::NotFound);
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
            ocx_oci::PackageRef::new_registry("cmake", "example.com").clone_with_tag("1.1.0"),
            kind,
        );
        let boxed = anyhow::Error::from(PackageManagerError::InstallFailed(vec![entry]));
        assert_eq!(crate::exit::classify_library_error(boxed.as_ref()), ExitCode::DataError);
    }

    // ── moved from ocx_package_manager::tasks::patch_discovery with the impl ──

    /// Reds on: a package-manager slug, batched, delegated or walked, naming another cause than the
    /// exit code does.
    #[test]
    fn package_manager_details_name_the_cause_that_decides_the_code() {
        use crate::exit::tests::assert_detail;
        use ocx_package::metadata::entrypoint::EntrypointName;
        use ocx_util::singleflight;

        let entry = |kind| PackageError::new(ocx_oci::PackageRef::new_registry("a", "example.com"), kind);
        let batch = vec![
            entry(PackageErrorKind::NotFound),
            entry(PackageErrorKind::SymlinkRequiresTag),
        ];
        assert_detail(&PackageManagerError::InspectFailed(batch), "package_not_found");
        let traversal = || ocx_util::archive::Error::SymlinkEscape {
            link: PathBuf::from("escape"),
            target: PathBuf::from("../../../../etc"),
        };
        assert_detail(
            &PackageErrorKind::from(ClientError::internal(traversal())),
            "archive_symlink_escape",
        );
        let wrapped = PackageErrorKind::from(ClientError::internal(traversal()));
        assert_detail(
            &PackageManagerError::InstallFailed(vec![entry(wrapped)]),
            "archive_symlink_escape",
        );
        let vanished = PackageErrorKind::PatchDiscovery(PatchError::DescriptorVanished {
            identifier: Box::new(ocx_oci::PackageRef::new_registry("global", "patches.example.com")),
        });
        assert_detail(
            &PackageManagerError::DiscoverFailed(vec![entry(vanished)]),
            "patch_descriptor_vanished",
        );
        assert_detail(&PackageManagerError::OfflineMode, "offline_mode");

        let spawn = LaunchError::Spawn {
            resolved: PathBuf::from("/store/cmake/content/bin/cmake"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert_detail(&spawn, "permission_denied");
        let opaque = LaunchError::Spawn {
            resolved: PathBuf::from("/store/cmake/content/bin/cmake"),
            source: std::io::Error::other("exec format error"),
        };
        assert_detail(&opaque, "launch_spawn_failed");
        let incomplete = LaunchError::IncompleteRecordInputs {
            command: "ocx exec".to_string(),
        };
        assert_detail(&incomplete, "record_inputs_incomplete");

        let hex = "a".repeat(64);
        let owner = |name: &str| {
            let reference: ocx_oci::PackageRef = format!("ocx.sh/{name}:1.0@sha256:{hex}")
                .parse()
                .expect("a pinned reference");
            ocx_oci::PinnedPackageRef::try_from(reference).expect("the reference carries a digest")
        };
        let collision = PackageErrorKind::EntrypointCollision {
            name: EntrypointName::try_from("cmake").expect("a valid entrypoint name"),
            owners: vec![owner("foo"), owner("bar")],
        };
        let shared = singleflight::SharedError::for_test(collision);
        let setup = DependencyError::SetupFailed(singleflight::Error::Failed(shared));
        assert_detail(&setup, "entrypoint_collision");
    }

    /// `PackageErrorKind::RequiredCompanionFailed` delegates exit-code
    /// classification to the inner `source` error.
    ///
    /// This ensures the exit code reflects the root cause (e.g. `NotFound`
    /// → exit 79) rather than a generic companion-failure code.
    ///
    /// Traces: STUB MANIFEST §3 classification arm delegates to `source.classify()`.
    #[test]
    fn required_companion_failed_exit_code_delegates_to_source() {
        use ocx_exit::ExitCode;
        use ocx_package_manager::error::PackageErrorKind;

        // Source = NotFound → should classify to NotFound (79).
        let kind = PackageErrorKind::RequiredCompanionFailed {
            companion: PackageRef::parse("patches.corp.com/ca:latest").expect("valid"),
            source: Box::new(PackageErrorKind::NotFound),
        };
        let code = kind.classify();
        assert_eq!(
            code,
            Some(ExitCode::NotFound),
            "RequiredCompanionFailed with NotFound source must classify as NotFound"
        );
    }

    /// A required companion refused for reaching a second digest of a repository the env already
    /// carries exits 65, the same as the conflict on a plain composition.
    #[test]
    fn required_companion_failed_on_a_digest_conflict_classifies_as_data_error() {
        use ocx_package_manager::error::PackageError;

        let conflict = DependencyError::Conflict {
            repository: ocx_oci::Repository::from(&PackageRef::new_registry("dep", "example.com")),
            identifiers: Vec::new(),
        };
        let kind = PackageErrorKind::RequiredCompanionFailed {
            companion: PackageRef::parse("patches.corp.com/ca:latest").expect("valid"),
            source: Box::new(PackageErrorKind::Internal(PackageManagerError::from(conflict))),
        };
        let entry = PackageError::new(PackageRef::new_registry("", ""), kind);
        let boxed = anyhow::Error::from(PackageManagerError::ResolveFailed(vec![entry]));
        assert_eq!(crate::exit::classify_library_error(boxed.as_ref()), ExitCode::DataError);
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
        let id_a: ocx_oci::PackageRef = format!("ocx.sh/foo:1.0@sha256:{hex}").parse().unwrap();
        let id_b: ocx_oci::PackageRef = format!("ocx.sh/bar:1.0@sha256:{hex}").parse().unwrap();
        let inner = PackageErrorKind::EntrypointCollision {
            name,
            owners: vec![
                ocx_oci::PinnedPackageRef::try_from(id_a).unwrap(),
                ocx_oci::PinnedPackageRef::try_from(id_b).unwrap(),
            ],
        };
        let shared = singleflight::SharedError::for_test(inner);
        let err = DependencyError::SetupFailed(singleflight::Error::Failed(shared));
        assert_eq!(classify(err), ExitCode::DataError);
    }
}
