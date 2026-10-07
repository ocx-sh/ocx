// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only: the classification tests of the `ocx_package` family. Its types declare their own codes with `#[derive(Classify)]`.

use ocx_package::dependency_pinning::DependencyPinningError;
use ocx_package::error::Error as PackageError;
use ocx_package::libc_lint::LibcLintError;
use ocx_package::metadata::dependency::DependencyError as MetadataDependencyError;
use ocx_package::metadata::template::TemplateError;
use ocx_package::prune::PruneError;
use ocx_package::publisher::CopyErrorKind;
use ocx_package::publisher::PublishGateError;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exit::tests::assert_detail;
    use ocx_exit::{ClassifyExitCode, ExitCode};
    use ocx_package::metadata::dependency::DependencyName;

    use ocx_package::metadata::Metadata;

    use ocx_package::metadata::validation::ValidMetadata;

    use std::path::PathBuf;

    /// `error` answers `code` and `slug`, and the registry files `slug` under `code`; for a type no ladder rung
    /// downcasts, whose code the whole-ladder helper cannot read.
    fn assert_slug_code_and_row<K: ocx_exit::ClassifyErrorKind + ClassifyExitCode + std::fmt::Debug>(
        error: &K,
        slug: &str,
        code: ExitCode,
    ) {
        assert_eq!(
            crate::exit::detail_slug(ocx_exit::ClassifyErrorKind::kind_detail(error)),
            slug,
            "slug for {error:?}"
        );
        assert_eq!(error.classify(), Some(code), "exit code for {error:?}");
        let registry = crate::exit::detail_registry();
        let row = registry
            .iter()
            .find(|row| row.slug == slug)
            .unwrap_or_else(|| panic!("{error:?} produces `{slug}`, which no DETAILS table lists"));
        assert_eq!(row.exit_code, code, "`{slug}` is registered under another exit code");
    }

    // ── moved from ocx_package::dependency_pinning with the impl ──

    /// Every arm of `DependencyPinningError::classify`, per variant.
    ///
    /// Recovered from `7adaea62:crates/ocx_lib/src/package/dependency_pinning.rs:254`
    /// and the pin rows at `:257`/`:260`/`:263` — not from the arms below. WP-10
    /// left the four `expect_err`/`matches!` tests in `ocx_lib` and took their
    /// `classify_error` lines with it, so at HEAD only the delegating `Index`
    /// arm was still asserted anywhere.
    #[test]
    fn every_dependency_pinning_variant_classifies_as_it_did_before_the_split() {
        fn identifier() -> Box<ocx_oci::PackageRef> {
            Box::new(
                "example.com/dep:1.0"
                    .parse::<ocx_oci::PackageRef>()
                    .expect("the fixture identifier parses"),
            )
        }

        let not_found = DependencyPinningError::DependencyNotFound {
            identifier: identifier(),
        };
        assert_eq!(
            not_found.classify(),
            Some(ExitCode::NotFound),
            "a dependency the index does not carry is 79"
        );
        assert_eq!(crate::exit::classify_library_error(&not_found), ExitCode::NotFound);

        // The three sidecar-content faults share one arm; asserted per variant
        // so splitting the arm cannot move one of them unnoticed.
        let data_faults: Vec<DependencyPinningError> = vec![
            DependencyPinningError::NoCompatiblePlatform {
                identifier: identifier(),
                platform: "linux/amd64".to_owned(),
                available: vec!["linux/arm64".to_owned()],
            },
            DependencyPinningError::AmbiguousPlatform {
                identifier: identifier(),
                platform: "linux/amd64".to_owned(),
                candidates: vec!["sha256:aa".to_owned(), "sha256:bb".to_owned()],
            },
            DependencyPinningError::DirectDigestPinInAnyTarget {
                identifier: identifier(),
            },
        ];
        for error in &data_faults {
            let rendered = error.to_string();
            assert_eq!(
                error.classify(),
                Some(ExitCode::DataError),
                "an unresolvable or forged dependency pin is 65: {rendered}"
            );
            assert_eq!(
                crate::exit::classify_library_error(error),
                ExitCode::DataError,
                "and it must still reach main as 65: {rendered}"
            );
        }

        let index = DependencyPinningError::Index(ocx_index::error::Error::PolicyResolutionBlocked {
            identifier: "pkg:1.0.0".to_string(),
            policy: "offline",
            block: ocx_index::error::PolicyBlock::UnpinnedTag,
        });
        assert_eq!(
            index.classify(),
            None,
            "Index must delegate so an offline/frozen block still answers 81"
        );
        assert_eq!(crate::exit::classify_library_error(&index), ExitCode::PolicyBlocked);
    }

    // ── moved from ocx_package::error with the impl ──

    // ── moved from ocx_package::libc_lint with the impl ──

    #[test]
    fn classify_maps_claim_failures_to_data_error_and_read_failures_to_io_error() {
        use ExitCode;

        let undeclared = LibcLintError::UndeclaredLibc {
            path: PathBuf::from("bin/bazel").into_boxed_path(),
            interpreter: "/lib64/ld-linux-x86-64.so.2".into(),
            required: "libc.glibc".into(),
            platform: "linux/amd64".into(),
            suggestion: "linux/amd64+libc.glibc".into(),
        };
        assert_eq!(
            undeclared.classify(),
            Some(ExitCode::DataError),
            "a false os.features claim is the publish-time mirror of the resolve-time \
             FeatureMismatch, which is also DataError (65)"
        );

        let unrecognized = LibcLintError::UnrecognizedInterpreter {
            path: PathBuf::from("bin/tool"),
            interpreter: "/lib/ld-uClibc.so.0".to_string(),
        };
        assert_eq!(unrecognized.classify(), Some(ExitCode::DataError));

        let unreadable = LibcLintError::Read {
            path: PathBuf::from("bin/tool"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert_eq!(unreadable.classify(), Some(ExitCode::IoError));

        assert_eq!(
            LibcLintError::from(ocx_package::error::Error::Index(
                ocx_index::error::Error::PolicyResolutionBlocked {
                    identifier: "pkg:1.0.0".to_string(),
                    policy: "offline",
                    block: ocx_index::error::PolicyBlock::UnpinnedTag,
                }
            ))
            .classify(),
            None,
            "Scan must delegate classification to its inner package-tier cause"
        );

        // The chain walk must reach the inner error as its own type; a boxed source hides it and exits 1.
        let scan = LibcLintError::from(ocx_package::error::Error::File(ocx_util::error::FileError::new(
            PathBuf::from("content/bin"),
            std::io::Error::from(std::io::ErrorKind::NotFound),
        )));
        assert_eq!(crate::exit::classify_error(&scan), ExitCode::IoError);
    }

    // ── moved from ocx_package::metadata::authoring::dependency with the impl ──

    // ── moved from ocx_package::metadata::template with the impl ──

    // ── moved from ocx_package::metadata::template::scanner with the impl ──

    // ── moved from ocx_package::metadata::validation with the impl ──

    #[test]
    fn over_cap_namespace_payload_exits_with_data_error() {
        use ExitCode;
        use ocx_package::metadata::integrations::MAX_INTEGRATION_NAMESPACE_BYTES;

        let meta = metadata_with_integrations(serde_json::json!({
            "com.example": string_value_of_exact_bytes(MAX_INTEGRATION_NAMESPACE_BYTES + 1),
        }));
        let error = ValidMetadata::try_from(meta).expect_err("over cap must fail");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
    }

    // ── moved from ocx_package::publisher::copy with the impl ──

    // ── moved from ocx_package::publisher::publish_gate with the impl ──

    fn pinned(digest_hex: &str) -> ocx_oci::PinnedPackageRef {
        let identifier: ocx_oci::PackageRef = format!("example.com/dep:1.0@sha256:{digest_hex}")
            .parse()
            .expect("the fixture identifier parses");
        ocx_oci::PinnedPackageRef::try_from(identifier).expect("a digest-bearing identifier is pinnable")
    }

    /// Every arm of `PublishGateError::classify`, per variant.
    ///
    /// Recovered from `7adaea62:crates/ocx_lib/src/publisher/publish_gate.rs`,
    /// not read off the arm it guards — the two `DataError` variants were the
    /// pair the pre-split baseline pin dropped (block-form right-hand
    /// side), so between the pin's blind spot and WP-10's strip of
    /// `index_pinned_dependency_rejected` /
    /// `any_target_rejects_pin_not_advertised_as_any`, this impl survived every
    /// mutation with nothing red.
    ///
    /// Exhaustive on purpose: a blanket sample would leave a later variant
    /// silently inheriting whichever code the sample happened to pick.
    #[test]
    fn every_publish_gate_variant_classifies_as_it_did_before_the_split() {
        let hex = "a".repeat(64);

        let pinned_to_index = PublishGateError::DependencyPinnedToIndex {
            identifier: Box::new(pinned(&hex)),
        };
        assert_eq!(
            pinned_to_index.classify(),
            Some(ExitCode::DataError),
            "a dependency pinned to an image index is a sidecar-content fault (65)"
        );
        assert_eq!(
            crate::exit::classify_library_error(&pinned_to_index),
            ExitCode::DataError,
            "and it must still reach main as 65 through the chain walker"
        );

        let forged_any = PublishGateError::AnyPinNotAdvertisedAsAny {
            identifier: Box::new(
                "example.com/dep:1.0"
                    .parse::<ocx_oci::PackageRef>()
                    .expect("the fixture identifier parses"),
            ),
            digest: format!("sha256:{hex}"),
        };
        assert_eq!(
            forged_any.classify(),
            Some(ExitCode::DataError),
            "a forged `any` provenance claim is the same class of sidecar fault (65)"
        );
        assert_eq!(crate::exit::classify_library_error(&forged_any), ExitCode::DataError);

        let absent = PublishGateError::DependencyManifestNotFound {
            identifier: Box::new(pinned(&hex)),
        };
        assert_eq!(absent.classify(), Some(ExitCode::NotFound));
        assert_eq!(crate::exit::classify_library_error(&absent), ExitCode::NotFound);

        // The two delegating arms answer `None` so the walker descends to the
        // cause — auth 80, network 69, a missing dependency tag 79.
        let verification = PublishGateError::Verification {
            identifier: Box::new(pinned(&hex)),
            source: ocx_oci::client::error::ClientError::Authentication("bad creds".into()),
        };
        assert_eq!(
            verification.classify(),
            None,
            "Verification must delegate, not answer for the cause it wraps"
        );
        assert_eq!(
            crate::exit::classify_library_error(&verification),
            ExitCode::AuthError,
            "delegation is only correct if the inner ClientError is what answers"
        );

        let provenance_unavailable = PublishGateError::AnyPinProvenanceUnavailable {
            identifier: Box::new(
                "example.com/dep:1.0"
                    .parse::<ocx_oci::PackageRef>()
                    .expect("the fixture identifier parses"),
            ),
            source: ocx_oci::client::error::ClientError::Registry(Box::new(std::io::Error::other("503"))),
        };
        assert_eq!(provenance_unavailable.classify(), None);
        assert_eq!(
            crate::exit::classify_library_error(&provenance_unavailable),
            ExitCode::Unavailable,
            "the wrapper delegates: a registry that would not answer is 69, not a data fault"
        );

        let routing = PublishGateError::Routing {
            identifier: Box::new(pinned(&hex)),
            source: ocx_index::error::Error::Ssrf {
                source: ocx_oci::ssrf::PhysicalDialRefused {
                    namespace: "ocx.sh".to_string(),
                    source: ocx_oci::ssrf::SsrfError::ForbiddenTarget {
                        host: "127.0.0.1".to_string(),
                        ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                    },
                },
            },
        };
        assert_eq!(routing.classify(), None, "Routing must delegate to the index error");
        assert_eq!(
            crate::exit::classify_library_error(&routing),
            ExitCode::ConfigError,
            "an SSRF refusal of the routed target is the index's 78, not a gate verdict"
        );

        let not_in_index = PublishGateError::Routing {
            identifier: Box::new(pinned(&hex)),
            source: ocx_index::error::Error::NotInIndex {
                identifier: "ocx.sh/dep:1.0".to_string(),
                namespace: "ocx.sh".to_string(),
                base_url: "https://index.ocx.sh".to_string(),
            },
        };
        assert_eq!(
            crate::exit::classify_library_error(&not_in_index),
            ExitCode::NotFound,
            "a name the authoritative index does not hold is 79 through the delegation"
        );
    }

    // ── moved from ocx_package::metadata::template::scanner with the impl ──

    // fixture from ocx_lib (package/metadata/validation)
    fn metadata_with_integrations(integrations: serde_json::Value) -> Metadata {
        let doc = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "integrations": integrations,
        });
        serde_json::from_value(doc).expect("valid bundle metadata")
    }

    // fixture from ocx_lib (package/metadata/validation)
    /// A JSON string value whose compact serialization is exactly `target`
    /// bytes (an ASCII payload — one byte per char plus the two quote bytes).
    ///
    /// `assert_eq!`, not `debug_assert_eq!`: the whole C-006 boundary pair rests
    /// on this helper being byte-exact, and `task rust:test` runs nextest in
    /// `--release`, where a `debug_assert` never executes at all.
    fn string_value_of_exact_bytes(target: usize) -> serde_json::Value {
        let value = serde_json::Value::String("a".repeat(target - 2));
        assert_eq!(serde_json::to_vec(&value).unwrap().len(), target);
        value
    }

    // ── moved from ocx_package::error with the impl ──

    #[test]
    fn integration_interpolation_dependency_not_installed_classifies_as_not_found() {
        // The one IntegrationInterpolation source that classifies away from
        // DataError — DependencyNotInstalled maps to NotFound (79), matching
        // an env value carrying the same token today.
        let hex = "a".repeat(64);
        let identifier: ocx_oci::PackageRef = format!("ocx.sh/ninja:1@sha256:{hex}").parse().unwrap();
        let pinned = ocx_oci::PinnedPackageRef::try_from(identifier).unwrap();

        let err = PackageError::IntegrationInterpolation {
            namespace: "com.example".to_owned(),
            source: TemplateError::DependencyNotInstalled {
                ref_name: DependencyName::try_from("ninja").unwrap(),
                dep_identifier: Box::new(pinned),
            },
        };
        assert_eq!(err.classify(), Some(ExitCode::NotFound));
    }

    #[test]
    fn integration_interpolation_unknown_dependency_ref_classifies_as_data_error() {
        // IntegrationInterpolation delegates classification to its source —
        // UnknownDependencyRef -> DataError (65), same as an env value with
        // the same token.
        let err = PackageError::IntegrationInterpolation {
            namespace: "com.example".to_owned(),
            source: TemplateError::UnknownDependencyRef {
                ref_name: DependencyName::try_from("ninja").unwrap(),
                declared: vec![],
            },
        };
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    #[test]
    fn integration_namespace_invalid_classifies_as_data_error() {
        let err = PackageError::IntegrationNamespaceInvalid {
            namespace: String::new(),
            reason: "empty",
        };
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    #[test]
    fn integration_too_large_classifies_as_data_error() {
        let err = PackageError::IntegrationTooLarge {
            namespace: "com.example".to_owned(),
            size: 9000,
            max: 8192,
        };
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    #[test]
    fn integrations_too_large_classifies_as_data_error() {
        let err = PackageError::IntegrationsTooLarge {
            size: 40_000,
            max: 32_768,
        };
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    // ── moved from ocx_package::metadata::template with the impl ──

    // recovered from ocx_package::publisher::copy
    /// The kind slugs are a frozen consumer contract, and every kind must carry
    /// an exit code — the `--json` envelope's `detail` field is what a script
    /// dispatches on instead of parsing stderr.
    ///
    /// Written as an exhaustive `match` rather than a list of asserts so that
    /// adding a variant is a compile error here, not a silently absent slug.
    #[test]
    fn every_copy_error_kind_has_a_frozen_slug_and_an_exit_code() {
        use ocx_exit::ExitCode;

        let kinds = [
            CopyErrorKind::IndexNamedByDigest,
            CopyErrorKind::PlatformRequired,
            CopyErrorKind::PlatformAmbiguous,
            CopyErrorKind::NoMatchingPlatform {
                requested: "linux/riscv64".to_string(),
                available: "linux/amd64".to_string(),
            },
        ];
        for kind in &kinds {
            let (slug, code) = match kind {
                CopyErrorKind::IndexNamedByDigest => ("index_named_by_digest", ExitCode::UsageError),
                CopyErrorKind::PlatformRequired => ("platform_required", ExitCode::UsageError),
                CopyErrorKind::PlatformAmbiguous => ("platform_ambiguous", ExitCode::UsageError),
                CopyErrorKind::NoMatchingPlatform { .. } => ("no_matching_platform", ExitCode::UsageError),
                CopyErrorKind::Registry(_) => unreachable!("the pass-through arm is not in this fixture"),
            };
            assert_slug_code_and_row(kind, slug, code);
        }
    }

    /// The pass-through arm reports its cause's slug, the error whose code `CopyError` exits with.
    #[test]
    fn a_copy_registry_failure_reports_its_causes_slug() {
        let kind = CopyErrorKind::from(ocx_oci::client::error::ClientError::Registry(Box::new(
            std::io::Error::other("503"),
        )));
        assert_eq!(
            crate::exit::detail_slug(ocx_exit::ClassifyErrorKind::kind_detail(&kind)),
            "registry_unavailable"
        );
        let CopyErrorKind::Registry(cause) = &kind else {
            panic!("a client error converts into the pass-through arm, got {kind:?}");
        };
        assert_detail(cause, "registry_unavailable");
    }

    /// The exit-code half of
    /// `ocx_package::metadata::template::tests::self_env_refusals_name_their_own_variant`.
    ///
    /// The resolver that mints these two values is private to `ocx_lib`, so
    /// that test names the variant each refusal produces and this one names
    /// the code each variant carries. Split, not dropped: a `${self.env.*}`
    /// refusal is bad metadata the publisher wrote, so it exits 65 and never
    /// 78 — a caller retrying on a config error would loop forever.
    #[test]
    fn self_env_refusals_exit_with_data_error() {
        for error in [
            TemplateError::UndefinedSelfEnvRef {
                key: "MISSING".to_string(),
                declared_before: vec!["A".to_string()],
            },
            TemplateError::AmbiguousSelfEnvRef { key: "A".to_string() },
        ] {
            assert_eq!(error.classify(), Some(ExitCode::DataError), "for {error}");
        }
    }

    // ── prune ──

    fn prune_ssrf_error() -> ocx_index::error::Error {
        ocx_index::error::Error::Ssrf {
            source: ocx_oci::ssrf::PhysicalDialRefused {
                namespace: "ocx.sh".to_string(),
                source: ocx_oci::ssrf::SsrfError::ForbiddenTarget {
                    host: "127.0.0.1".to_string(),
                    ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                },
            },
        }
    }

    /// Reds on: a package or prune slug, delegated, walked or split by a guard, naming another cause
    /// than the exit code does.
    #[test]
    fn package_details_name_the_cause_that_decides_the_code() {
        use ocx_oci::client::error::ClientError;

        let identifier = || {
            Box::new(
                "example.com/dep:1.0"
                    .parse::<ocx_oci::PackageRef>()
                    .expect("the fixture identifier parses"),
            )
        };
        let not_found = DependencyPinningError::DependencyNotFound {
            identifier: identifier(),
        };
        assert_detail(&not_found, "dependency_not_found");
        let blocked = DependencyPinningError::Index(ocx_index::error::Error::PolicyResolutionBlocked {
            identifier: "pkg:1.0.0".to_string(),
            policy: "offline",
            block: ocx_index::error::PolicyBlock::UnpinnedTag,
        });
        assert_detail(&blocked, "index_resolution_blocked");
        let verification = PublishGateError::Verification {
            identifier: Box::new(pinned(&"a".repeat(64))),
            source: ClientError::Authentication("bad creds".into()),
        };
        assert_detail(&verification, "registry_auth_failed");
        let invalid_name = MetadataDependencyError::InvalidName {
            name: "Bad Name".to_string(),
        };
        // No ladder rung downcasts this type: only a wrapper's delegation reaches it.
        assert_slug_code_and_row(&invalid_name, "dependency_declaration_invalid", ExitCode::DataError);

        let refused = |durable: &[&str], not_in_index: &[&str]| PruneError::Refused {
            package: "ocx.sh/acme/tool".to_string(),
            url: "https://index.example".to_string(),
            durable: durable.iter().map(|tag| (*tag).to_string()).collect(),
            not_in_index: not_in_index.iter().map(|tag| (*tag).to_string()).collect(),
        };
        assert_detail(&refused(&["release"], &["fresh"]), "prune_refused_durable");
        assert_detail(&refused(&[], &["fresh"]), "prune_refused_pending");
        let unsupported = PruneError::Registry(ClientError::DeleteUnsupported {
            registry: "registry.example".to_string(),
            status: 405,
        });
        assert_detail(&unsupported, "registry_delete_unsupported");
        let denied = PruneError::DeleteDenied {
            repository: "registry.example/acme/tool".to_string(),
            tag: "snap".to_string(),
            source: ClientError::Authentication("token lacks delete".into()),
        };
        assert_detail(&denied, "registry_auth_failed");
        let pointer = PruneError::RepositoryPointer {
            package: "ocx.sh/acme/tool".to_string(),
            source: prune_ssrf_error(),
        };
        assert_detail(&pointer, "ssrf_forbidden_target");
        let unreadable = PruneError::RootUnreadable {
            package: "ocx.sh/acme/tool".to_string(),
            url: "https://index.example".to_string(),
            source: ocx_index::error::Error::IndexHttpFailed {
                url: "https://index.example/p/acme/tool.json".to_string(),
                status: Some(500),
                source: "unexpected status 500".into(),
            },
        };
        assert_detail(&unreadable, "index_http_failed");
    }

    /// A refusal the caller can only fix by naming something else exits 64, never 65 or 1.
    #[test]
    fn prune_argument_faults_exit_with_usage_error() {
        for error in [
            PruneError::NotAPrereleaseFamily {
                value: "0.5.0".to_string(),
            },
            PruneError::DigestTag {
                value: "sha256:aa".to_string(),
            },
            PruneError::PackageNotBare {
                package: "ocx.sh/acme/tool:1".to_string(),
            },
        ] {
            assert_eq!(error.classify(), Some(ExitCode::UsageError), "for {error}");
            assert_eq!(crate::exit::classify_library_error(&error), ExitCode::UsageError);
        }
    }

    #[test]
    fn prune_package_the_index_does_not_hold_exits_79() {
        let error = PruneError::NotInIndex {
            package: "ocx.sh/acme/tool".to_string(),
            url: "https://index.example".to_string(),
        };
        assert_eq!(error.classify(), Some(ExitCode::NotFound));
        assert_eq!(crate::exit::classify_library_error(&error), ExitCode::NotFound);
    }

    #[test]
    fn prune_without_an_index_exits_81() {
        let error = PruneError::NoIndex {
            package: "registry.example/acme/tool".to_string(),
        };
        assert_eq!(error.classify(), Some(ExitCode::PolicyBlocked));
        assert_eq!(crate::exit::classify_library_error(&error), ExitCode::PolicyBlocked);
    }

    /// A durable tag is 81 even when a not-yet-announced tag was refused beside it: a retry
    /// cannot fix the durable one, so 75 would send a wrapper into a loop.
    #[test]
    fn prune_durable_refusal_wins_over_a_pending_one() {
        let refused = |durable: &[&str], not_in_index: &[&str]| PruneError::Refused {
            package: "ocx.sh/acme/tool".to_string(),
            url: "https://index.example".to_string(),
            durable: durable.iter().map(|tag| (*tag).to_string()).collect(),
            not_in_index: not_in_index.iter().map(|tag| (*tag).to_string()).collect(),
        };

        assert_eq!(refused(&["release"], &[]).classify(), Some(ExitCode::PolicyBlocked));
        assert_eq!(
            refused(&["release"], &["fresh"]).classify(),
            Some(ExitCode::PolicyBlocked),
            "81 wins over 75"
        );
        assert_eq!(refused(&[], &["fresh"]).classify(), Some(ExitCode::TempFail));
        assert_eq!(
            crate::exit::classify_library_error(&refused(&["release"], &["fresh"])),
            ExitCode::PolicyBlocked
        );
    }

    #[test]
    fn prune_tag_still_served_after_its_delete_exits_75() {
        let error = PruneError::StillPresent {
            repository: "registry.example/acme/tool".to_string(),
            tag: "snap".to_string(),
        };
        assert_eq!(error.classify(), Some(ExitCode::TempFail));
        assert_eq!(crate::exit::classify_library_error(&error), ExitCode::TempFail);
    }

    #[test]
    fn prune_credential_without_delete_rights_exits_80() {
        let error = PruneError::DeleteDenied {
            repository: "registry.example/acme/tool".to_string(),
            tag: "snap".to_string(),
            source: ocx_oci::client::error::ClientError::Authentication("token lacks delete".into()),
        };
        assert_eq!(error.classify(), Some(ExitCode::AuthError));
        assert_eq!(crate::exit::classify_library_error(&error), ExitCode::AuthError);
    }

    /// The pass-through arm keeps each registry failure on the code the rest of `ocx` gives it.
    #[test]
    fn prune_registry_failures_keep_their_client_error_codes() {
        let unsupported = PruneError::Registry(ocx_oci::client::error::ClientError::DeleteUnsupported {
            registry: "registry.example".to_string(),
            status: 405,
        });
        assert_eq!(unsupported.classify(), Some(ExitCode::Unsupported));
        assert_eq!(crate::exit::classify_library_error(&unsupported), ExitCode::Unsupported);

        let transient = PruneError::Registry(ocx_oci::client::error::ClientError::RegistryTransient(
            "simulated 503".into(),
        ));
        assert_eq!(transient.classify(), Some(ExitCode::TempFail));

        let unreachable = PruneError::Registry(ocx_oci::client::error::ClientError::Registry("unreachable".into()));
        assert_eq!(unreachable.classify(), Some(ExitCode::Unavailable));
    }

    #[test]
    fn prune_forbidden_repository_pointer_exits_78() {
        let error = PruneError::RepositoryPointer {
            package: "ocx.sh/acme/tool".to_string(),
            source: prune_ssrf_error(),
        };
        assert_eq!(error.classify(), Some(ExitCode::ConfigError));
        assert_eq!(crate::exit::classify_library_error(&error), ExitCode::ConfigError);
    }

    #[test]
    fn prune_unreadable_root_takes_the_index_error_code() {
        let unreadable = |source| PruneError::RootUnreadable {
            package: "ocx.sh/acme/tool".to_string(),
            url: "https://index.example".to_string(),
            source,
        };
        let failed = |status| {
            unreadable(ocx_index::error::Error::IndexHttpFailed {
                url: "https://index.example/p/acme/tool.json".to_string(),
                status: Some(status),
                source: format!("unexpected status {status}").into(),
            })
        };
        let terminal = failed(500);
        assert_eq!(terminal.classify(), Some(ExitCode::Unavailable));
        assert_eq!(crate::exit::classify_library_error(&terminal), ExitCode::Unavailable);

        let transient = failed(503);
        assert_eq!(transient.classify(), Some(ExitCode::TempFail));
        assert_eq!(crate::exit::classify_library_error(&transient), ExitCode::TempFail);

        let forbidden = unreadable(prune_ssrf_error());
        assert_eq!(forbidden.classify(), Some(ExitCode::ConfigError));
    }
}
