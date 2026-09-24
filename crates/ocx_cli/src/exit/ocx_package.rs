// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the package, metadata and publisher error family — the `ocx_package` rung of the
//! ladder, here rather than in that crate because classification is `ocx_cli`'s alone.

use ocx_exit::ExitCode;

use ocx_oci::layer_ref::LayerRefParseError;
use ocx_package::bin_scan::BinScanError;
use ocx_package::dependency_pinning::DependencyPinningError;
use ocx_package::error::Error as PackageError;
use ocx_package::libc_lint::LibcLintError;
use ocx_package::metadata::authoring::AuthoringError;
use ocx_package::metadata::dependency::DependencyError as MetadataDependencyError;
use ocx_package::metadata::template::TemplateError;
use ocx_package::publisher::CopyError;
use ocx_package::publisher::CopyErrorKind;
use ocx_package::publisher::PublishGateError;

use super::{ClassifyErrorKind, ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for CopyError {
    /// Delegates to the kind, except for the pass-through arm.
    ///
    /// [`CopyErrorKind::Registry`] means "no copy-specific code fits this", so
    /// it asks the cause it wraps — a registry 404 keeps 79, a missing
    /// Referrers API keeps 84, a 401 keeps 80, rather than all three being
    /// flattened. Without this impl every structural refusal would classify as
    /// the generic `Failure` (1) and a pipeline could not tell a bad invocation
    /// from an unreachable registry.
    ///
    /// It must delegate explicitly rather than return `None` and leave it to
    /// the chain walker: the arm is `#[error(transparent)]`, which forwards
    /// `source()` *past* the inner error instead of to it, so a walk starting
    /// here never visits the one value that knows its own code.
    fn classify(&self) -> Option<ExitCode> {
        use ClassifyErrorKind;
        match &self.kind {
            CopyErrorKind::Registry(cause) => cause.classify(),
            kind => Some(kind.exit_code()),
        }
    }
}

impl ClassifyErrorKind for CopyErrorKind {
    fn exit_code(&self) -> ExitCode {
        use ExitCode;
        match self {
            Self::IndexNamedByDigest
            | Self::PlatformRequired
            | Self::PlatformAmbiguous
            // Naming a platform the source does not publish is an invocation
            // fault: the index is intact and the request is what has to change.
            | Self::NoMatchingPlatform { .. } => ExitCode::UsageError,
            // Never reached through `CopyError::classify`, which asks the
            // wrapped cause for its own code instead. Only a caller that
            // classifies a bare kind, with the cause discarded, lands here —
            // and at that point there is nothing left to be more specific.
            Self::Registry(_) => ExitCode::Failure,
        }
    }

    fn kind_detail(&self) -> &'static str {
        // Frozen contract: the snake_case parallel of the variant name.
        // Exhaustive — adding a variant forces a new arm here.
        match self {
            Self::IndexNamedByDigest => "index_named_by_digest",
            Self::PlatformRequired => "platform_required",
            Self::PlatformAmbiguous => "platform_ambiguous",
            Self::NoMatchingPlatform { .. } => "no_matching_platform",
            Self::Registry(_) => "registry",
        }
    }
}

impl ClassifyExitCode for LayerRefParseError {
    fn classify(&self) -> Option<ExitCode> {
        // A layer-ref string comes from the CLI (publish side); a bad one is a
        // usage error (64), whether a bare digest or a malformed layout tail.
        Some(ExitCode::UsageError)
    }
}

impl ClassifyExitCode for PublishGateError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            PublishGateError::DependencyPinnedToIndex { .. } | PublishGateError::AnyPinNotAdvertisedAsAny { .. } => {
                Some(ExitCode::DataError)
            }
            PublishGateError::DependencyManifestNotFound { .. } => Some(ExitCode::NotFound),
            // Delegate to the inner cause (auth → 80, network → 69, a missing
            // dependency tag → 79 via the wrapped `ocx_lib::Error`).
            PublishGateError::Verification { .. } | PublishGateError::AnyPinProvenanceUnavailable { .. } => None,
            // The index error answers: an SSRF refusal is 78, an index outage
            // 69, a malformed root 65 — none of them is the gate's to decide.
            PublishGateError::Routing { .. } => None,
        }
    }
}

impl ClassifyExitCode for BinScanError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Input-data trouble: the declared claim disagrees with the
            // content tree, the scanned set itself is malformed, or this
            // host cannot produce a trustworthy scan for the target platform.
            Self::UndeclaredBinary { .. }
            | Self::DeclaredNotExecutable { .. }
            | Self::Binary(_)
            | Self::UnsupportedHostScan { .. } => Some(ExitCode::DataError),
            // Delegate to the inner cause via the chain walker.
            Self::Scan(_) => None,
        }
    }
}

impl ClassifyExitCode for DependencyPinningError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            DependencyPinningError::DependencyNotFound { .. } => Some(ExitCode::NotFound),
            DependencyPinningError::NoCompatiblePlatform { .. }
            | DependencyPinningError::AmbiguousPlatform { .. }
            | DependencyPinningError::DirectDigestPinInAnyTarget { .. } => Some(ExitCode::DataError),
            // Delegate to the inner index/client cause via the chain walker
            // (offline/frozen policy blocks classify to 81 there).
            DependencyPinningError::Index(_) => None,
        }
    }
}

impl ClassifyExitCode for PackageError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::VersionInvalid(_)
            | Self::UnsupportedLogoFormat(_)
            | Self::InvalidLogoContent { .. }
            | Self::BuildMeta(_)
            | Self::EmptyPushSet
            | Self::UnknownEnvModifier { .. }
            | Self::MissingListSeparator { .. }
            | Self::ReservedEnvKey { .. }
            | Self::InvalidListSeparator { .. }
            | Self::SeparatorEdgedListValue { .. }
            | Self::IntegrationNamespaceInvalid { .. }
            | Self::IntegrationTooLarge { .. }
            | Self::IntegrationsTooLarge { .. } => Some(ExitCode::DataError),
            Self::RequiredPathMissing(_) => Some(ExitCode::NotFound),
            Self::EnvVarInterpolation { source, .. } => source.classify(),
            Self::EntrypointArgInterpolation { source, .. } => source.classify(),
            Self::IntegrationInterpolation { source, .. } => source.classify(),
            // The four E1 stand-ins. `From<PackageError> for ocx_lib::Error`
            // reconstructs each into the crate-wide variant it replaced, and
            // that `From` is the only producer of `Error::Package`, so none of
            // them reaches this classifier today. They are written to agree
            // with that reconstruction rather than left to a catch-all: `File`
            // answers what `Error::InternalFile` answers, `SerializationFailure`
            // what `Error::SerializationFailure` answers, and the two wrappers
            // delegate exactly as `Error::OciClient` / `Error::OciIndex` do. An
            // arm that agrees by construction cannot drift into a second
            // opinion about the same value.
            Self::File(_) => Some(ExitCode::IoError),
            Self::Archive(e) => e.classify(),
            Self::Digest(e) => e.classify(),
            Self::Platform(e) => e.classify(),
            Self::SerializationFailure(_) => Some(ExitCode::DataError),
            Self::OciClient(e) => e.classify(),
            Self::Index(e) => e.classify(),
        }
    }
}

impl ClassifyExitCode for LibcLintError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Input-data trouble, and deliberately the same code the
            // *resolution* side already returns for the mirror-image failure:
            // `SelectResult::FeatureMismatch` -> `PackageErrorKind::
            // FeatureMismatch` -> `DataError`. One number for both ends of
            // the os.features contract, whether the mismatch is caught at
            // publish time or at install time. Matches the sibling compile
            // step (`BinScanError`) too.
            Self::UndeclaredLibc { .. }
            | Self::AgnosticPlatformClaim { .. }
            | Self::UnparseableElf { .. }
            | Self::UnrecognizedInterpreter { .. }
            | Self::UnresolvableScanScope { .. }
            | Self::ModifierBearingScanScope { .. } => Some(ExitCode::DataError),
            // A file we could not read is an I/O fault, not bad data.
            Self::Read { .. } => Some(ExitCode::IoError),
            // Delegate to the inner cause via the chain walker.
            Self::Scan(_) => None,
        }
    }
}

impl ClassifyExitCode for AuthoringError {
    fn classify(&self) -> Option<ExitCode> {
        // Malformed / incomplete metadata is input-data trouble: DataError (65).
        Some(ExitCode::DataError)
    }
}

impl ClassifyExitCode for MetadataDependencyError {
    fn classify(&self) -> Option<ExitCode> {
        // Every variant is malformed/oversized input data: DataError (65).
        Some(ExitCode::DataError)
    }
}

impl ClassifyExitCode for TemplateError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::UnknownDependencyRef { .. }
            | Self::AmbiguousDependencyRef { .. }
            | Self::UnknownToken { .. }
            | Self::UnknownField { .. }
            | Self::UnknownModifier { .. }
            | Self::ModifierNotApplicable { .. }
            | Self::UndefinedSelfEnvRef { .. }
            | Self::AmbiguousSelfEnvRef { .. }
            | Self::DisallowedToken { .. }
            | Self::ResolvedValueTooLarge { .. } => ExitCode::DataError,
            Self::DependencyNotInstalled { .. } => ExitCode::NotFound,
        })
    }
}

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, CopyError);
    downcast_arm!(cause, PackageError);
    downcast_arm!(cause, AuthoringError);
    downcast_arm!(cause, DependencyPinningError);
    downcast_arm!(cause, BinScanError);
    downcast_arm!(cause, LibcLintError);
    downcast_arm!(cause, TemplateError);
    downcast_arm!(cause, LayerRefParseError);
    downcast_arm!(cause, PublishGateError);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_package::metadata::dependency::DependencyName;

    use ocx_package::metadata::Metadata;

    use ocx_package::metadata::validation::ValidMetadata;

    use std::path::PathBuf;

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
            path: PathBuf::from("bin/bazel"),
            interpreter: "/lib64/ld-linux-x86-64.so.2".to_string(),
            required: "libc.glibc".to_string(),
            platform: "linux/amd64".to_string(),
            suggestion: "linux/amd64+libc.glibc".to_string(),
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
            LibcLintError::Scan(ocx_package::error::Error::Index(
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
    /// pair `classify_baseline_7adaea62.json` dropped (block-form right-hand
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
            assert_eq!(kind.kind_detail(), slug, "slug for {kind:?}");
            assert_eq!(kind.exit_code(), code, "exit code for {kind:?}");
        }
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
}
