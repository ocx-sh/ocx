// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The one signer-rounds decision shared by install and the verify commands: the index-declared publisher
//! round and the operator-policy round, and the refusals that precede them.

use crate::error::{PackageError, PackageErrorKind};
use ocx_config::{Config, SignatureRequirement};
use ocx_index::{DeclaredPublishers, Index};
use ocx_oci::PackageRef;
use ocx_sign::verify::{VerifyError, VerifyErrorKind};
use ocx_trust::{CompiledPolicy, PublishersRefusal, TrustPolicy, parse_publishers};

/// The publisher-side refusals, in the operations tier so the `ocx index update` hint lives next to the decision.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum RoundsError {
    /// No listed publisher signed the package, or the two publisher lists share no entry.
    ///
    /// `reason` is interpolated rather than a `#[source]`: this variant classifies itself, so no walker
    /// descends and its only consumer is the text.
    #[error("{}", mismatch_message(.package, .root, .reason.as_deref()))]
    #[exit(
        PermissionDenied,
        slug = "publisher_mismatch",
        summary = "No publisher listed by the index signed the package"
    )]
    PublisherMismatch {
        /// The package the round refused.
        package: String,
        /// The repository whose root `ocx index update` refreshes (the default registry is left implicit).
        root: String,
        /// The verification failure the publisher round remapped; `None` when the committed and fetched
        /// publisher lists share no entry, so no signature was checked.
        reason: Option<Box<VerifyErrorKind>>,
    },
    /// `--no-verify` or `OCX_NO_VERIFY` was given where the system config requires signatures.
    #[error(
        "--no-verify / OCX_NO_VERIFY is refused for {registry}: the system config file {config_path} sets `signatures` for it, \
         and only an operator can change that"
    )]
    #[exit(
        PolicyBlocked,
        slug = "no_verify_refused",
        summary = "--no-verify was refused because the system config requires signatures"
    )]
    NoVerifyRefused {
        /// The registry whose `signatures` setting forbids the bypass.
        registry: String,
        /// The system config file that sets it.
        config_path: String,
    },
    /// The index lists publishers only a newer ocx understands.
    #[error(
        "the index lists no publisher of {package} this ocx understands: {}; upgrade ocx or set \
         `signatures = \"off\"` for {registry}",
        reasons.join("; ")
    )]
    #[exit(
        Unsupported,
        slug = "publishers_unrecognized",
        summary = "The index lists publishers only a newer ocx understands"
    )]
    PublishersUnrecognized {
        /// The package the round refused.
        package: String,
        /// The package's logical registry, whose `signatures` setting opts out.
        registry: String,
        /// Why each entry is not understood.
        reasons: Vec<String>,
    },
    /// The index's `publishers` entries are malformed.
    #[error(
        "the index entry for {package} lists no valid publisher: {}; report the entry to its maintainer or refresh \
         it with `ocx index update {root}`",
        reasons.join("; ")
    )]
    #[exit(
        DataError,
        slug = "publishers_invalid",
        summary = "The index lists publishers that are malformed"
    )]
    PublishersInvalid {
        /// The package the round refused.
        package: String,
        /// The repository whose root `ocx index update` refreshes (the default registry is left implicit).
        root: String,
        /// Why each entry is malformed.
        reasons: Vec<String>,
    },
    /// `signatures = "required"` for the registry and the package declares no publishers.
    #[error(
        "registry {registry} requires declared publishers and {package} declares none; fetch its root with \
         `ocx index update {root}`"
    )]
    #[exit(
        PolicyBlocked,
        slug = "publishers_required",
        summary = "The registry requires index-declared publishers and the package has none"
    )]
    PublishersRequired {
        /// The package the round refused.
        package: String,
        /// The package's logical registry.
        registry: String,
        /// The repository whose root `ocx index update` refreshes (the default registry is left implicit).
        root: String,
    },
}

fn mismatch_message(package: &str, root: &str, reason: Option<&VerifyErrorKind>) -> String {
    match reason {
        Some(reason) => format!(
            "no publisher listed by the index signed {package} ({reason}); install a release a listed publisher \
             signed or refresh the root with `ocx index update {root}`"
        ),
        None => format!(
            "the locally committed and the fetched publisher lists share no entry for {package}; refresh the root \
             with `ocx index update {root}`"
        ),
    }
}

/// The repository `ocx index update` refreshes for `package`; the default registry is left implicit.
fn index_update_target(package: &PackageRef) -> String {
    if package.registry() == ocx_oci::DEFAULT_REGISTRY {
        package.repository().to_string()
    } else {
        format!("{}/{}", package.registry(), package.repository())
    }
}

impl RoundsError {
    /// A [`Self::PublisherMismatch`] for `package`, naming the repository root to refresh.
    pub fn publisher_mismatch(package: &PackageRef, reason: VerifyErrorKind) -> Self {
        Self::PublisherMismatch {
            package: package.to_string(),
            root: index_update_target(package),
            reason: Some(Box::new(reason)),
        }
    }

    /// A [`Self::PublisherMismatch`] for a committed and a fetched publisher list that share no entry.
    pub fn publishers_diverged(package: &PackageRef) -> Self {
        Self::PublisherMismatch {
            package: package.to_string(),
            root: index_update_target(package),
            reason: None,
        }
    }
}

/// The rounds a covered package must pass; both must verify.
#[derive(Debug)]
pub struct SignerRounds {
    /// The publisher round; `None` when undeclared or `signatures = "off"`.
    pub publishers: Option<CompiledPolicy>,
    /// The operator-policy round (`resolve_tiered` output), possibly empty.
    pub operator: Vec<CompiledPolicy>,
}

/// Where a round's policies came from, which decides how its failure reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundKind {
    /// The index-declared publishers; "nobody I list signed it" is a `PublisherMismatch`.
    Publisher,
    /// The matched `[[trust.policy]]` entries (operator tier, else project tier).
    Operator,
    /// A signer flag (`--certificate-identity`, `--key`) that replaces every other round.
    Flags,
}

/// One ANY-of policy set a signature must satisfy.
#[derive(Debug, Clone)]
pub struct Round {
    /// Where the policies came from.
    pub kind: RoundKind,
    /// The ANY-of policies of this round.
    pub policies: Vec<CompiledPolicy>,
}

impl Round {
    /// The round a signer flag defines.
    #[must_use]
    pub fn flags(policies: Vec<CompiledPolicy>) -> Self {
        Self {
            kind: RoundKind::Flags,
            policies,
        }
    }
}

impl SignerRounds {
    /// The rounds to run, publisher first, an empty operator set omitted.
    #[must_use]
    pub fn ordered(&self) -> Vec<Round> {
        self.publishers
            .iter()
            .map(|publishers| Round {
                kind: RoundKind::Publisher,
                policies: vec![publishers.clone()],
            })
            .chain((!self.operator.is_empty()).then(|| Round {
                kind: RoundKind::Operator,
                policies: self.operator.clone(),
            }))
            .collect()
    }
}

/// `round`'s failure as the caller sees it: only a [`RoundKind::Publisher`] round remaps.
pub(super) fn remap_round_failure(round: &Round, err: PackageError) -> PackageError {
    match round.kind {
        RoundKind::Publisher => {
            let identifier = err.identifier.clone();
            PackageError::new(
                identifier.clone(),
                publisher_round_error(&identifier, &round.policies, err.kind),
            )
        }
        RoundKind::Operator | RoundKind::Flags => err,
    }
}

/// A publisher round that found nobody it lists is `PublisherMismatch`; every other failure keeps its kind.
///
/// `SignatureInvalid` counts only when the round holds a key publisher, where "untrusted key" and "bad signature"
/// share that kind; a keyless-only round keeps 65 for a genuinely invalid signature.
pub(super) fn publisher_round_error(
    resolved: &ocx_oci::PackageRef,
    round: &[CompiledPolicy],
    kind: PackageErrorKind,
) -> PackageErrorKind {
    let has_key_publisher = round
        .iter()
        .flat_map(|policy| &policy.backends)
        .any(|backend| matches!(backend, ocx_trust::PolicyBackend::Key(_)));
    let mismatch =
        |reason| PackageErrorKind::Internal(crate::Error::Rounds(RoundsError::publisher_mismatch(resolved, reason)));
    match kind {
        PackageErrorKind::Internal(crate::Error::Verify(verify_error)) => match verify_error.kind {
            kind @ (VerifyErrorKind::IdentityMismatch | VerifyErrorKind::IssuerMismatch) => mismatch(kind),
            kind @ VerifyErrorKind::SignatureInvalid if has_key_publisher => mismatch(kind),
            _ => PackageErrorKind::Internal(crate::Error::Verify(verify_error)),
        },
        other => other,
    }
}

/// What a package's signature check requires.
#[derive(Debug)]
pub enum RoundsDecision {
    /// Nothing covers the package.
    Uncovered,
    /// One or both rounds apply.
    Covered(SignerRounds),
    /// The package is refused before any signature is read.
    Refused(RoundsError),
}

/// Inputs of [`signer_rounds`].
pub struct SignerRoundsInput<'a> {
    /// The package's logical identifier (the root key, never the physical repository).
    pub logical: &'a PackageRef,
    /// The index the declared publishers are read from, locally.
    pub index: &'a Index,
    /// The merged `config.toml` tiers (`signatures` per registry).
    pub config: &'a Config,
    /// Operator-tier `[[trust.policy]]` entries.
    pub operator_policies: &'a [TrustPolicy],
    /// Project-tier `[[trust.policy]]` entries.
    pub project_policies: &'a [TrustPolicy],
}

/// Decide which signer rounds apply to `input.logical`, in order.
///
/// Reads local index state only; never issues a request.
///
/// # Errors
/// A matched, malformed operator policy (`TrustPolicyInvalid`, exit 78), or an unreadable committed index root
/// (fails closed rather than reading as "undeclared").
pub async fn signer_rounds(input: SignerRoundsInput<'_>) -> Result<RoundsDecision, crate::Error> {
    decide_rounds(input, PublishersSource::Effective).await
}

/// [`signer_rounds`] for an install the user named by digest or an `ocx.lock` pin: only the locally committed
/// root declares publishers, so a root the resolve fetched for routing adds no check.
///
/// # Errors
/// As [`signer_rounds`].
pub async fn signer_rounds_for_pinned(input: SignerRoundsInput<'_>) -> Result<RoundsDecision, crate::Error> {
    decide_rounds(input, PublishersSource::CommittedOnly).await
}

#[derive(Clone, Copy)]
enum PublishersSource {
    Effective,
    CommittedOnly,
}

async fn decide_rounds(input: SignerRoundsInput<'_>, source: PublishersSource) -> Result<RoundsDecision, crate::Error> {
    let logical = input.logical;
    let requirement = input.config.signature_requirement(logical.registry());

    let declared = match (&requirement, source) {
        (SignatureRequirement::Off, _) => DeclaredPublishers::Undeclared,
        (_, PublishersSource::Effective) => input.index.declared_publishers(logical).await?,
        (_, PublishersSource::CommittedOnly) => input.index.committed_publishers(logical).await?,
    };

    let publishers = match declared {
        DeclaredPublishers::Diverged => {
            return Ok(RoundsDecision::Refused(RoundsError::publishers_diverged(logical)));
        }
        DeclaredPublishers::Declared(value) => match parse_publishers(&value).compile() {
            Ok(policy) => Some(policy),
            Err(PublishersRefusal::Unrecognised { reasons }) => {
                return Ok(RoundsDecision::Refused(RoundsError::PublishersUnrecognized {
                    package: logical.to_string(),
                    registry: logical.registry().to_owned(),
                    reasons,
                }));
            }
            Err(PublishersRefusal::Invalid { reasons }) => {
                return Ok(RoundsDecision::Refused(RoundsError::PublishersInvalid {
                    package: logical.to_string(),
                    root: index_update_target(logical),
                    reasons,
                }));
            }
        },
        DeclaredPublishers::Undeclared if requirement == SignatureRequirement::Required => {
            return Ok(RoundsDecision::Refused(RoundsError::PublishersRequired {
                package: logical.to_string(),
                registry: logical.registry().to_owned(),
                root: index_update_target(logical),
            }));
        }
        DeclaredPublishers::Undeclared => None,
    };

    let target = format!("{}/{}", logical.registry(), logical.repository());
    let operator =
        ocx_trust::resolve_tiered(input.operator_policies, input.project_policies, &target).map_err(|source| {
            crate::Error::Verify(Box::new(VerifyError::new(
                logical.clone(),
                VerifyErrorKind::TrustPolicyInvalid(source),
            )))
        })?;

    if publishers.is_none() && operator.is_empty() {
        return Ok(RoundsDecision::Uncovered);
    }
    Ok(RoundsDecision::Covered(SignerRounds { publishers, operator }))
}

#[cfg(test)]
mod tests {
    use ocx_exit::{ClassifyErrorKind, ClassifyExitCode, Detail, ExitCode};
    use ocx_index::{DeclaredPublishers, Index};
    use serde_json::{Value, json};

    use super::*;
    use crate::tasks::verify::tests::{REGISTRY, REPO, SingleTagSource, tagged_id};

    fn config(body: &str) -> Config {
        toml::from_str(body).expect("config parses")
    }

    async fn decide(config: &Config) -> RoundsDecision {
        let index = Index::from_impl(SingleTagSource);
        let logical = tagged_id();
        signer_rounds(SignerRoundsInput {
            logical: &logical,
            index: &index,
            config,
            operator_policies: &[],
            project_policies: &[],
        })
        .await
        .expect("no operator policy is malformed")
    }

    /// The message names the package, the failure kind and the repository root `ocx index update` refreshes.
    #[test]
    fn c013_publisher_mismatch_message_names_the_package_and_the_refresh() {
        let default = PackageRef::new_registry("cmake", ocx_oci::DEFAULT_REGISTRY).clone_with_tag("3.28");
        let message = RoundsError::publisher_mismatch(&default, VerifyErrorKind::IdentityMismatch).to_string();
        assert!(message.contains("signed ocx.sh/cmake:3.28 ("), "{message}");
        assert!(
            message.contains(&VerifyErrorKind::IdentityMismatch.to_string()),
            "{message}"
        );
        let issuer = RoundsError::publisher_mismatch(&default, VerifyErrorKind::IssuerMismatch).to_string();
        assert!(
            issuer.contains(&VerifyErrorKind::IssuerMismatch.to_string()),
            "{issuer}"
        );
        assert_ne!(message, issuer);
        assert!(message.contains("`ocx index update cmake`"), "{message}");

        let other = PackageRef::new_registry("cmake", "example.com").clone_with_tag("3.28");
        let message = RoundsError::publisher_mismatch(&other, VerifyErrorKind::IdentityMismatch).to_string();
        assert!(message.contains("`ocx index update example.com/cmake`"), "{message}");
    }

    /// No publisher round, no operator policy and no refusal is `Uncovered`.
    #[tokio::test]
    async fn c010_an_undeclared_package_with_no_policy_is_uncovered() {
        assert!(matches!(decide(&Config::default()).await, RoundsDecision::Uncovered));
    }

    /// `required` and no declared publishers refuses with `PublishersRequired` naming the registry.
    #[tokio::test]
    async fn c010_required_without_publishers_is_refused() {
        let config = config(&format!("[registries.\"{REGISTRY}\"]\nsignatures = \"required\"\n"));
        match decide(&config).await {
            RoundsDecision::Refused(RoundsError::PublishersRequired { registry, .. }) => assert_eq!(registry, REGISTRY),
            other => panic!("expected PublishersRequired, got {other:?}"),
        }
    }

    /// `off` reads no declaration: a source that would fail to read still yields `Uncovered`.
    #[tokio::test]
    async fn c010_off_is_uncovered_without_reading_publishers() {
        let index = Index::from_impl(DeclaringSource {
            declared: DeclaredPublishers::Declared(json!([keyless_publisher()])),
            unreadable: true,
        });
        let config = config(&format!("[registries.\"{REGISTRY}\"]\nsignatures = \"off\"\n"));
        let logical = tagged_id();
        let decision = signer_rounds(SignerRoundsInput {
            logical: &logical,
            index: &index,
            config: &config,
            operator_policies: &[],
            project_policies: &[],
        })
        .await
        .expect("off never reads the committed root");
        assert!(matches!(decision, RoundsDecision::Uncovered), "got {decision:?}");
    }

    /// A registry key spelled with other letter case still selects that registry's setting.
    #[tokio::test]
    async fn c010_a_non_canonical_registry_key_spelling_still_applies() {
        let upper = REGISTRY.to_ascii_uppercase();
        let required = config(&format!("[registries.\"{upper}\"]\nsignatures = \"required\"\n"));
        match decide(&required).await {
            RoundsDecision::Refused(RoundsError::PublishersRequired { registry, .. }) => assert_eq!(registry, REGISTRY),
            other => panic!("expected PublishersRequired, got {other:?}"),
        }
    }

    // ── The ADR check table, one row per case ────────────────────────────────

    /// A source whose committed root declares `declared` (or fails to read, when `unreadable`).
    #[derive(Clone)]
    struct DeclaringSource {
        declared: DeclaredPublishers,
        unreadable: bool,
    }

    #[async_trait::async_trait]
    impl ocx_index::IndexImpl for DeclaringSource {
        async fn list_repositories(&self, _: &str) -> ocx_index::error::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> ocx_index::error::Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            _: &ocx_oci::PackageRef,
            _: ocx_index::IndexOperation,
        ) -> ocx_index::error::Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(
            &self,
            _: &ocx_oci::PackageRef,
            _: ocx_index::IndexOperation,
        ) -> ocx_index::error::Result<Option<ocx_oci::Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> ocx_index::error::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn declared_publishers(&self, _: &ocx_oci::PackageRef) -> ocx_index::error::Result<DeclaredPublishers> {
            if self.unreadable {
                return Err(ocx_index::error::Error::PathInvalid("committed-root".into()));
            }
            Ok(self.declared.clone())
        }
        fn box_clone(&self) -> Box<dyn ocx_index::IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn keyless_publisher() -> Value {
        json!({"kind": "keyless", "identity": "release@example.com", "oidc_issuer": "https://example.com"})
    }

    fn operator_policy(issuer: Option<&str>) -> TrustPolicy {
        TrustPolicy {
            scope: Some(ocx_trust::ScopeSpec::Prefix(format!("{REGISTRY}/{REPO}"))),
            builder: None,
            signers: vec![ocx_trust::SignerSpec::Keyless(ocx_trust::KeylessMatcher {
                identity: Some("ops@example.com".into()),
                identity_regexp: None,
                oidc_issuer: issuer.map(str::to_owned),
                claims: Default::default(),
            })],
            system_locked: false,
        }
    }

    async fn decide_with(
        declared: Option<Value>,
        signatures: &str,
        operator: &[TrustPolicy],
    ) -> Result<RoundsDecision, crate::Error> {
        let declared = declared.map_or(DeclaredPublishers::Undeclared, DeclaredPublishers::Declared);
        decide_declared(declared, signatures, operator).await
    }

    async fn decide_declared(
        declared: DeclaredPublishers,
        signatures: &str,
        operator: &[TrustPolicy],
    ) -> Result<RoundsDecision, crate::Error> {
        let index = Index::from_impl(DeclaringSource {
            declared,
            unreadable: false,
        });
        let config = config(&format!("[registries.\"{REGISTRY}\"]\nsignatures = \"{signatures}\"\n"));
        let logical = tagged_id();
        signer_rounds(SignerRoundsInput {
            logical: &logical,
            index: &index,
            config: &config,
            operator_policies: operator,
            project_policies: &[],
        })
        .await
    }

    fn covered(decision: RoundsDecision) -> SignerRounds {
        match decision {
            RoundsDecision::Covered(rounds) => rounds,
            other => panic!("expected Covered, got {other:?}"),
        }
    }

    fn refusal(decision: RoundsDecision) -> RoundsError {
        match decision {
            RoundsDecision::Refused(error) => error,
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    fn verdict(error: &RoundsError) -> (ExitCode, &'static str) {
        let Detail::Fixed(entry) = error.kind_detail() else {
            panic!("RoundsError names its slug directly");
        };
        (error.classify().expect("every RoundsError row has a code"), entry.slug)
    }

    /// Row: present / none / `if_declared` and `required` → a listed publisher's signature, nothing else.
    #[tokio::test]
    async fn c010_declared_without_operator_policy_needs_a_publisher() {
        for signatures in ["if_declared", "required"] {
            let rounds = covered(
                decide_with(Some(json!([keyless_publisher()])), signatures, &[])
                    .await
                    .unwrap(),
            );
            assert!(rounds.publishers.is_some(), "{signatures}: publisher round");
            assert!(rounds.operator.is_empty(), "{signatures}: no operator round");
        }
    }

    /// Row: present / covers / `if_declared` and `required` → both rounds.
    #[tokio::test]
    async fn c010_declared_and_operator_covered_needs_both_rounds() {
        for signatures in ["if_declared", "required"] {
            let rounds = covered(
                decide_with(
                    Some(json!([keyless_publisher()])),
                    signatures,
                    &[operator_policy(Some("https://example.com"))],
                )
                .await
                .unwrap(),
            );
            assert!(rounds.publishers.is_some(), "{signatures}: publisher round");
            assert_eq!(rounds.operator.len(), 1, "{signatures}: operator round");
        }
    }

    /// Row: absent / covers / `if_declared` → the operator round alone.
    #[tokio::test]
    async fn c010_undeclared_but_operator_covered_needs_the_operator() {
        let rounds = covered(
            decide_with(None, "if_declared", &[operator_policy(Some("https://example.com"))])
                .await
                .unwrap(),
        );
        assert!(rounds.publishers.is_none());
        assert_eq!(rounds.operator.len(), 1);
    }

    /// Row: absent / any / `required` → 81 `publishers_required`, with or without an operator policy.
    #[tokio::test]
    async fn c010_required_refuses_81_even_when_an_operator_policy_covers() {
        for operator in [vec![], vec![operator_policy(Some("https://example.com"))]] {
            let error = refusal(decide_with(None, "required", &operator).await.unwrap());
            assert!(matches!(&error, RoundsError::PublishersRequired { registry, .. } if registry == REGISTRY));
            assert_eq!(verdict(&error), (ExitCode::PolicyBlocked, "publishers_required"));
        }
    }

    /// Row: present / covers / `off` → the operator round alone.
    #[tokio::test]
    async fn c010_off_with_declared_publishers_needs_the_operator_only() {
        let rounds = covered(
            decide_with(
                Some(json!([keyless_publisher()])),
                "off",
                &[operator_policy(Some("https://example.com"))],
            )
            .await
            .unwrap(),
        );
        assert!(rounds.publishers.is_none());
        assert_eq!(rounds.operator.len(), 1);
    }

    /// Row: present / none / `off` → nothing.
    #[tokio::test]
    async fn c010_off_with_declared_publishers_and_no_policy_is_uncovered() {
        let decision = decide_with(Some(json!([keyless_publisher()])), "off", &[])
            .await
            .unwrap();
        assert!(matches!(decision, RoundsDecision::Uncovered));
    }

    /// Row: present, nothing usable → 82 when any entry is merely unrecognised, 65 when all are malformed.
    #[tokio::test]
    async fn c010_nothing_usable_refuses_82_or_65() {
        for signatures in ["if_declared", "required"] {
            let unrecognised = refusal(
                decide_with(Some(json!([{"kind": "future"}])), signatures, &[])
                    .await
                    .unwrap(),
            );
            assert!(matches!(unrecognised, RoundsError::PublishersUnrecognized { .. }));
            assert_eq!(
                verdict(&unrecognised),
                (ExitCode::Unsupported, "publishers_unrecognized")
            );

            let invalid = refusal(
                decide_with(Some(json!([{"kind": "keyless"}])), signatures, &[])
                    .await
                    .unwrap(),
            );
            assert!(matches!(invalid, RoundsError::PublishersInvalid { .. }));
            assert_eq!(verdict(&invalid), (ExitCode::DataError, "publishers_invalid"));
        }
    }

    /// A usable entry beside an unrecognised one is a round, never a refusal.
    #[tokio::test]
    async fn c010_a_usable_entry_beside_an_unrecognised_one_is_a_round() {
        let declared = json!([{"kind": "future"}, keyless_publisher()]);
        let rounds = covered(decide_with(Some(declared), "if_declared", &[]).await.unwrap());
        assert_eq!(rounds.publishers.expect("publisher round").backends.len(), 1);
    }

    /// A non-array `publishers` value (`null` included) is malformed, not undeclared.
    #[tokio::test]
    async fn c010_a_non_array_value_refuses_65() {
        let error = refusal(decide_with(Some(Value::Null), "if_declared", &[]).await.unwrap());
        assert_eq!(verdict(&error), (ExitCode::DataError, "publishers_invalid"));
    }

    /// hands an emptied intersection back as `Diverged`: no listed publisher can have signed → 77.
    #[tokio::test]
    async fn c010_an_empty_intersection_refuses_77_publisher_mismatch() {
        let error = refusal(
            decide_declared(DeclaredPublishers::Diverged, "if_declared", &[])
                .await
                .unwrap(),
        );
        assert!(matches!(error, RoundsError::PublisherMismatch { .. }));
        assert_eq!(verdict(&error), (ExitCode::PermissionDenied, "publisher_mismatch"));
    }

    /// The two lists sharing no entry says so and points at the refresh, not at a signer that was never checked.
    #[tokio::test]
    async fn c010_a_diverged_list_says_the_lists_share_no_entry() {
        let error = refusal(
            decide_declared(DeclaredPublishers::Diverged, "if_declared", &[])
                .await
                .unwrap(),
        );
        let message = error.to_string();
        assert!(
            message.contains("committed and the fetched publisher lists share no entry"),
            "{message}"
        );
        assert!(message.contains("`ocx index update example.com/widget`"), "{message}");
        assert!(!message.contains("no publisher listed"), "{message}");
    }

    /// Each publisher refusal names the package and the way out.
    #[tokio::test]
    async fn c010_refusal_messages_name_the_package_and_the_way_out() {
        let package = tagged_id().to_string();
        let unrecognised = refusal(
            decide_with(Some(json!([{"kind": "future"}])), "if_declared", &[])
                .await
                .unwrap(),
        )
        .to_string();
        assert!(unrecognised.contains(&package), "{unrecognised}");
        assert!(unrecognised.contains("upgrade ocx"), "{unrecognised}");
        assert!(
            unrecognised.contains(&format!("`signatures = \"off\"` for {REGISTRY}")),
            "{unrecognised}"
        );

        let invalid = refusal(
            decide_with(Some(json!([{"kind": "keyless"}])), "if_declared", &[])
                .await
                .unwrap(),
        )
        .to_string();
        assert!(invalid.contains(&package), "{invalid}");
        assert!(invalid.contains("report the entry to its maintainer"), "{invalid}");
        assert!(invalid.contains("`ocx index update example.com/widget`"), "{invalid}");

        let required = refusal(decide_with(None, "required", &[]).await.unwrap()).to_string();
        assert!(required.contains(&package), "{required}");
        assert!(required.contains(REGISTRY), "{required}");
    }

    /// A declared `[]` is malformed (65), not a mismatch.
    #[tokio::test]
    async fn c004_a_declared_empty_list_refuses_65_publishers_invalid() {
        let error = refusal(decide_with(Some(json!([])), "if_declared", &[]).await.unwrap());
        assert_eq!(verdict(&error), (ExitCode::DataError, "publishers_invalid"));
    }

    /// Row: no root known (lock or digest install on an empty home) is undeclared, so a `required` registry
    /// Refuses it with 81 naming the registry, and `if_declared` installs with the operator policy.
    #[tokio::test]
    async fn c010_no_root_known_is_undeclared() {
        let required = refusal(decide_with(None, "required", &[]).await.unwrap());
        assert_eq!(verdict(&required), (ExitCode::PolicyBlocked, "publishers_required"));
        let optional = decide_with(None, "if_declared", &[]).await.unwrap();
        assert!(matches!(optional, RoundsDecision::Uncovered));
    }

    /// An unreadable committed root fails closed; it is never read as "undeclared".
    #[tokio::test]
    async fn c010_an_unreadable_committed_root_is_an_error() {
        let index = Index::from_impl(DeclaringSource {
            declared: DeclaredPublishers::Undeclared,
            unreadable: true,
        });
        let logical = tagged_id();
        let result = signer_rounds(SignerRoundsInput {
            logical: &logical,
            index: &index,
            config: &Config::default(),
            operator_policies: &[],
            project_policies: &[],
        })
        .await;
        let error = result.expect_err("an unreadable committed root must not read as undeclared");
        assert!(error.to_string().contains("committed-root"), "got {error}");
    }

    /// A matched, malformed operator policy propagates as `TrustPolicyInvalid` (78), publisher round or not.
    #[tokio::test]
    async fn c010_a_malformed_matched_operator_policy_is_an_error() {
        let result = decide_with(None, "if_declared", &[operator_policy(None)]).await;
        match result {
            Err(crate::Error::Verify(error)) => {
                assert!(
                    matches!(error.kind, VerifyErrorKind::TrustPolicyInvalid(_)),
                    "got {:?}",
                    error.kind
                );
            }
            other => panic!("expected Verify(TrustPolicyInvalid), got {other:?}"),
        }
    }
}
