// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the OCI index error family — the `ocx_index` rung of the
//! ladder, here rather than in that crate because classification is `ocx_cli`'s alone.

use ocx_exit::ExitCode;

use ocx_index::error::Error as OciIndexError;

use super::{ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for OciIndexError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            // Both mean "nothing here to install" to a wrapper: no tags at all,
            // or no tag that could carry a version. An exit code is a coarse
            // contract — the message is what disambiguates the two.
            // Same verdict, same code: the package is not installable. Keeping
            // 79 here means the `case $?` contract is untouched by ocx#251 —
            // what changed is the message, which now names the index that was
            // consulted instead of leaving a bare "not found".
            Self::RemoteManifestNotFound(_) | Self::NoIndexableTag(_) | Self::NotInIndex { .. } => ExitCode::NotFound,
            Self::NestedImageIndex { .. } => ExitCode::DataError,
            // E1 (DEC-27) gave this tier its own root error, so seven variants
            // now stand in for variants of the crate-wide one. Each copies the
            // code from the arm it replaced, and `STANDS_IN_FOR` in `exit.rs`
            // names that arm by its baseline `file:line` so the pin re-checks
            // the claim rather than trusting this comment (DEC-55 / DEC-60).
            Self::Store(error) => return error.classify(),
            Self::OciClient(error) => return error.classify(),
            Self::Digest(error) => return error.classify(),
            Self::PinnedIdentifier(error) => return error.classify(),
            Self::File(error) => return error.classify(),
            Self::PathInvalid(_) => ExitCode::Failure,
            Self::SerializationFailure(_) => ExitCode::DataError,
            // Delegate to the full chain walker on the wrapped typed error,
            // not just a single-hop `classify()` on the inner `Error`. Mirrors
            // the `PackageErrorKind::Internal(inner)` pattern so nested causes
            // (e.g. a `ClientError::Authentication` inside a `ocx_lib::Error`)
            // are resolved via the generic `try_classify` ladder.
            Self::SourceWalkFailed(arc) | Self::SourceFetchFailed(arc) => {
                return Some(super::classify_library_error(arc.as_error()));
            }
            // Yield to the chain walker rather than answering here: the variant
            // carries the singleflight error as `#[source]`, and that type
            // classifies itself (a broadcast leader failure defers to the
            // leader's own typed error, a timeout is `TempFail`). Answering
            // `Failure` here would be a terminal `Some` that ends the walk, so
            // every waiter would exit 1 while the leader exits 80/65/69 — the
            // same operation reporting a different code depending on whether it
            // happened to win the singleflight race. Mirrors
            // `PackageManagerError::SetupFailed`, which wraps the same type.
            Self::SingleflightFailed(_) => return None,
            // A deliberate local policy (offline / frozen) refused the
            // resolution — categorically the same as every other policy block.
            Self::PolicyResolutionBlocked { .. } => ExitCode::PolicyBlocked,
            // Malformed / untrusted static-file index input at a trust
            // boundary — the OCI data-error class (65).
            Self::UnsupportedIndexFormat { .. }
            | Self::DispatchObjectDigestMismatch { .. }
            | Self::WalkedDigestMismatch { .. }
            | Self::YankedRefused { .. }
            | Self::MalformedPhysicalRef { .. }
            | Self::RootRepositoryMismatch { .. }
            | Self::MalformedCatalogKey { .. }
            | Self::InvalidImageIndex(_)
            | Self::MalformedIndexDocument { .. } => ExitCode::DataError,
            // A transport-layer failure reaching the static-file index — the
            // resource is unavailable, same class as a registry outage. An
            // absent catalog document is the same class for the same reason:
            // the source is reachable but is not serving what a published index
            // must serve, and every cause of it is external and retryable.
            //
            // Deliberately status-blind: `IndexHttpFailed::status` exists for
            // the retry classifier, not to split this arm. Fanning it out would
            // be a CLI-surface break for a distinction nothing asked for.
            Self::IndexHttpFailed { .. } | Self::CatalogDocumentAbsent { .. } => ExitCode::Unavailable,
            // A misconfigured index-role traffic target — a configuration fault.
            Self::PlainHttpIndexNotAllowed { .. } | Self::InvalidIndexUrl { .. } => ExitCode::ConfigError,
            // A forbidden target is a configuration fault (78) — the fix is
            // `trusted_hosts` config. An unresolvable host was never reached,
            // so no `trusted_hosts` entry can fix it; that is the "registry
            // unreachable" class (69) every other transport failure already
            // reports. `SsrfError::classify` already carries the right verdict
            // for both — delegate instead of flattening to one code.
            Self::Ssrf { source, .. } => return source.classify(),
        })
    }
}

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, OciIndexError);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_exit::ExitCode;

    // ── moved from ocx_lib::file_structure::index_store with the impl ──

    // ── moved from ocx_lib::oci::index::chained_index with the impl ──

    // ── moved from ocx_lib::oci::index::error with the impl ──

    // ── moved from ocx_lib::oci::index::local_index with the impl ──

    // ── moved from ocx_lib::oci::index::ocx_index with the impl ──

    // ── moved from ocx_lib::oci::index::regenerate with the impl ──

    // ── moved from ocx_lib::oci::index::file_transport with the impl ──

    // ── moved from ocx_lib::oci::index::ocx_index with the impl ──

    // ── moved from ocx_lib::oci::index::file_transport with the impl ──

    // ── moved from ocx_lib::oci::index::error with the impl ──

    /// A coordination failure with no leader error to defer to still carries its
    /// own meaning: a singleflight timeout is transient, so it must reach
    /// `TempFail` (75) — the retryable class — rather than collapsing to a
    /// generic failure the way the terminal arm did.
    #[test]
    fn singleflight_timeout_classifies_as_temp_fail() {
        let error = OciIndexError::SingleflightFailed(ocx_util::singleflight::Error::Timeout);
        assert_eq!(crate::exit::classify_library_error(&error), ExitCode::TempFail);
    }

    /// Plan row 12: an SSRF *resolution* failure classifies to `Unavailable`
    /// (69), not `ConfigError` (78).
    ///
    /// A host that does not resolve was never reached, so no `trusted_hosts`
    /// entry can fix it — that is the "registry unreachable" class every other
    /// transport failure already reports. The verdict `SsrfError::classify`
    /// already carries must survive the wrap instead of being flattened to one
    /// code for both variants.
    ///
    /// Paired positive: `ssrf_refusal_classifies_as_config_error` directly
    /// above pins `ForbiddenTarget` on `ConfigError` (78), so this pair fails
    /// on a fix that merely swaps one blanket code for another.
    #[test]
    fn ssrf_resolution_classifies_as_unavailable() {
        let error = OciIndexError::Ssrf {
            source: ocx_oci::ssrf::PhysicalDialRefused {
                namespace: "ocx.sh".to_string(),
                source: ocx_oci::ssrf::SsrfError::Resolution {
                    host: "no-such-registry.invalid".to_string(),
                    source: std::io::Error::new(std::io::ErrorKind::NotFound, "name or service not known"),
                },
            },
        };
        assert_eq!(error.classify(), Some(ExitCode::Unavailable));
    }

    // recovered from ocx_index::error
    /// The exit code of a concurrent resolve must not depend on who reached the
    /// key first.
    ///
    /// Both errors are built in the exact shape `walk_chain` produces: the
    /// leader propagates `SourceWalkFailed(ArcError)`, and the waiter receives
    /// that same error broadcast inside
    /// `SingleflightFailed(Failed(SharedError(..)))`. Testing only the leader
    /// cannot see the bug — the leader arm was always correct; it was the
    /// waiter's terminal `Some(Failure)` that ended the walk before the typed
    /// error underneath was ever reached.
    ///
    /// Chain-walked via [`crate::exit::classify_library_error`] rather than a single-hop
    /// `classify()`, because the whole mechanism is the walk continuing through
    /// `#[source]`.
    #[test]
    fn leader_and_waiter_classify_a_source_walk_failure_identically() {
        use ocx_util::singleflight;

        // One case per exit-code class a source walk realistically produces:
        // a rejected credential, malformed publisher data, an unreachable index.
        // E1: a source walk now carries the index tier's own error, so the
        // cases are built at that type. The client case goes through the tier's
        // `OciClient` variant, which reconstructs as `ocx_lib::Error::OciClient`
        // at the CLI boundary — same error, same code, one wrapper fewer.
        let cases: Vec<(OciIndexError, ExitCode)> = vec![
            (
                OciIndexError::OciClient(ocx_oci::client::error::ClientError::Authentication(Box::new(
                    std::io::Error::other("token refused"),
                ))),
                ExitCode::AuthError,
            ),
            (
                OciIndexError::YankedRefused {
                    identifier: "ocx.sh/kitware/cmake:3.28".to_string(),
                },
                ExitCode::DataError,
            ),
            (
                OciIndexError::IndexHttpFailed {
                    url: "https://index.ocx.sh/c/index.json".to_string(),
                    status: None,
                    source: Box::new(std::io::Error::other("connection reset")),
                },
                ExitCode::Unavailable,
            ),
        ];

        for (inner, expected) in cases {
            let arc = ocx_index::error::ArcError::from(inner);
            let leader = OciIndexError::SourceWalkFailed(arc.clone());
            let waiter = OciIndexError::SingleflightFailed(singleflight::Error::Failed(
                singleflight::SharedError::for_test(OciIndexError::SourceWalkFailed(arc)),
            ));

            let leader_code = crate::exit::classify_library_error(&leader);
            let waiter_code = crate::exit::classify_library_error(&waiter);
            assert_eq!(leader_code, expected, "leader must report the source walk's own code");
            assert_eq!(
                waiter_code, leader_code,
                "waiter must report the same code as the leader for the same failure"
            );
        }
    }
}
