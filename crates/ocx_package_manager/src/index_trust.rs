// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Judges a source's signed index catalog against the signers a registry is configured to trust.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use ocx_config::{IndexExpiryMode, IndexSignatureMode, RegistryConfig};
use ocx_index::{CatalogDocument, CatalogVerdict, CatalogVerifier};
use ocx_sign::detached::{INDEX_CATALOG_PREDICATE_TYPE, verify_statement};
use ocx_sign::sign::SignerIdentity;
use ocx_sign::sign::state::SigningStatePaths;
use ocx_sign::verify::{ResolvedRoot, TrustRoot, VerifyErrorKind, resolve_policy_trust_roots};
use ocx_trust::{
    CompiledPolicy, PolicyBackend, PublisherEntry, SigstoreAuthority, SigstoreInstances, parse_publishers,
};
use tokio::sync::OnceCell;

/// The one file in a source's catalog the signature covers, and the statement's subject name.
const CATALOG_SUBJECT: &str = "c/index.json";
/// The longest an `expires` may lie ahead; the signer's own maximum is 366 days.
const MAX_EXPIRES_AHEAD_DAYS: i64 = 400;

/// The one signer the built-in `ocx.sh` list holds: the workflow that renders and deploys the index.
pub struct BuiltInIndexSigner {
    pub identity: &'static str,
    pub issuer: &'static str,
    /// Fulcio certificate claims, as `(claim name, expected value)`.
    pub claims: &'static [(&'static str, &'static str)],
}

/// The registry the built-in signer list applies to.
pub const OCX_SH_REGISTRY: &str = "ocx.sh";

/// The built-in `ocx.sh` signer. It names no Sigstore instance, so the public one verifies it.
pub const OCX_SH_INDEX_SIGNER: BuiltInIndexSigner = BuiltInIndexSigner {
    identity: "https://github.com/ocx-sh/index/.github/workflows/render-deploy.yml@refs/heads/main",
    issuer: "https://token.actions.githubusercontent.com",
    claims: &[
        ("source_repository_identifier", "1297393033"),
        ("source_repository_owner_identifier", "263156444"),
    ],
};

/// Resolves the convention path of an instance's trusted root.
pub type HomeRootOf = Box<dyn Fn(&SigstoreAuthority) -> Option<PathBuf> + Send + Sync>;

/// Where the Sigstore trust roots of a check come from.
pub struct IndexTrustRoots {
    /// The convention path of an instance's trusted root.
    pub home_root_of: HomeRootOf,
    pub state: SigningStatePaths,
    pub rekor_cache_key: String,
    pub offline: bool,
}

/// The signing setup a registry's index source runs under.
pub struct IndexSigning {
    /// One policy per list, empty when no entry of the list is one this build understands.
    pub signers: Vec<CompiledPolicy>,
    pub mode: IndexSignatureMode,
    pub expiry: IndexExpiryMode,
}

/// Resolves a registry's signer list, signature mode and expiry setting.
///
/// The signer list is `index_publishers` when set, otherwise the built-in list for `ocx.sh`, otherwise none. The
/// mode is `index_signature` when set, otherwise `warn` with a list and `off` without one. A list with no usable
/// entry resolves to no signers, which the verifier reports as unrecognised.
///
/// # Errors
///
/// [`ocx_config::error::Error::IndexPublishersMissing`] for `enforce` with no list, and
/// [`ocx_config::error::Error::IndexPublishersInvalid`] for a malformed entry.
pub fn resolve_index_signing(
    registry: &str,
    config: Option<&RegistryConfig>,
) -> Result<IndexSigning, ocx_config::error::Error> {
    let builtin = registry == OCX_SH_REGISTRY;
    if let Some(config) = config {
        config.validate_index_signing(registry, builtin)?;
    }
    let configured = config.and_then(|config| config.index_publishers.as_ref());
    let signers = match configured {
        Some(entries) => {
            let value =
                serde_json::to_value(entries).map_err(|error| ocx_config::error::Error::IndexPublishersInvalid {
                    registry: registry.to_owned(),
                    reason: error.to_string(),
                })?;
            // A list with no usable entry is judged at check time, where its verdict names it.
            parse_publishers(&value).compile().into_iter().collect()
        }
        None if builtin => {
            vec![
                built_in_policy().map_err(|reason| ocx_config::error::Error::IndexPublishersInvalid {
                    registry: registry.to_owned(),
                    reason,
                })?,
            ]
        }
        None => Vec::new(),
    };
    let has_list = configured.is_some() || builtin;
    let mode = config.and_then(|config| config.index_signature).unwrap_or(if has_list {
        IndexSignatureMode::Warn
    } else {
        IndexSignatureMode::Off
    });
    let expiry = config
        .and_then(|config| config.index_expiry)
        .unwrap_or(IndexExpiryMode::Check);
    Ok(IndexSigning { signers, mode, expiry })
}

/// The built-in `ocx.sh` signer as a compiled policy.
fn built_in_policy() -> Result<CompiledPolicy, String> {
    let claims: serde_json::Map<String, serde_json::Value> = OCX_SH_INDEX_SIGNER
        .claims
        .iter()
        .map(|(name, value)| ((*name).to_owned(), serde_json::Value::String((*value).to_owned())))
        .collect();
    let entry = serde_json::json!([{
        "kind": "keyless",
        "identity": OCX_SH_INDEX_SIGNER.identity,
        "oidc_issuer": OCX_SH_INDEX_SIGNER.issuer,
        "claims": claims,
    }]);
    let set = parse_publishers(&entry);
    if let Some(PublisherEntry::Invalid { reason, .. } | PublisherEntry::Unrecognised { reason, .. }) = set
        .entries
        .iter()
        .find(|entry| !matches!(entry, PublisherEntry::Usable(_)))
    {
        return Err(format!("the built-in signer is not usable: {reason}"));
    }
    set.compile().map_err(|refusal| format!("{refusal:?}"))
}

/// A [`CatalogVerifier`] for one registry's index source.
pub struct IndexTrust {
    registry: String,
    signers: Vec<CompiledPolicy>,
    expiry: IndexExpiryMode,
    instances: SigstoreInstances,
    roots_from: Option<Arc<IndexTrustRoots>>,
    roots: OnceCell<Vec<ResolvedRoot>>,
}

impl IndexTrust {
    /// `signers` is the resolved list for `registry`; `instances` holds the Sigstore tables its keyless entries name.
    ///
    /// Without [`Self::with_trust_roots`] only key entries can verify: a keyless entry has no root to check against.
    pub fn new(
        registry: String,
        signers: Vec<CompiledPolicy>,
        expiry: IndexExpiryMode,
        instances: SigstoreInstances,
    ) -> Self {
        Self {
            registry,
            signers,
            expiry,
            instances,
            roots_from: None,
            roots: OnceCell::new(),
        }
    }

    /// Names where the trust roots of the keyless entries' instances come from.
    #[must_use]
    pub fn with_trust_roots(mut self, roots: IndexTrustRoots) -> Self {
        self.roots_from = Some(Arc::new(roots));
        self
    }

    /// The registry whose catalog this verifier judges.
    pub fn registry(&self) -> &str {
        &self.registry
    }

    /// Every entry of every list as one any-of policy, or `None` when no list holds an entry.
    fn merged_policy(&self) -> Option<CompiledPolicy> {
        let backends: Vec<PolicyBackend> = self
            .signers
            .iter()
            .flat_map(|policy| policy.backends.iter().cloned())
            .collect();
        (!backends.is_empty()).then_some(CompiledPolicy {
            builder: None,
            backends,
        })
    }

    /// One trust root per Sigstore instance a keyless entry names, resolved once and kept on success.
    async fn trust_roots(&self) -> Result<&[ResolvedRoot], VerifyErrorKind> {
        let roots = self
            .roots
            .get_or_try_init(|| async {
                let Some(from) = self.roots_from.clone() else {
                    return Ok(Vec::new());
                };
                // Unwrapped: the verdict prints this error's own text, and a wrapper would hide a missing root.
                resolve_policy_trust_roots(
                    &self.signers,
                    Some(&self.instances),
                    &*from.home_root_of,
                    &from.state,
                    &from.rekor_cache_key,
                    from.offline,
                )
                .await
            })
            .await?;
        Ok(roots)
    }

    /// [`CatalogVerifier::check`] at the instant `now`.
    async fn check_at(&self, catalog_bytes: &[u8], signature: Option<&[u8]>, now: DateTime<Utc>) -> CatalogVerdict {
        let Some(policy) = self.merged_policy() else {
            return CatalogVerdict::Unrecognized;
        };
        let Some(signature) = signature else {
            return CatalogVerdict::Unsigned;
        };
        let roots = match self.trust_roots().await {
            Ok(roots) => roots,
            Err(error) => return CatalogVerdict::Invalid(error.to_string()),
        };
        let (signer, verified_by) = match self.verify_against(roots, &policy, catalog_bytes, signature).await {
            Ok(accepted) => accepted,
            Err(verdict) => return verdict,
        };

        let document: CatalogDocument = match serde_json::from_slice(catalog_bytes) {
            Ok(document) => document,
            Err(error) => return CatalogVerdict::Invalid(format!("the catalog does not parse: {error}")),
        };
        if let Some(expires) = document.expires
            && expires - now > Duration::days(MAX_EXPIRES_AHEAD_DAYS)
        {
            return CatalogVerdict::Invalid(format!(
                "the catalog expires {expires}, more than {MAX_EXPIRES_AHEAD_DAYS} days ahead"
            ));
        }
        let expired = document.expires.is_none_or(|expires| now >= expires);
        if expired && self.expiry == IndexExpiryMode::Check {
            return CatalogVerdict::Expired {
                expires: document.expires,
            };
        }
        self.seed_trust_root_cache(&verified_by).await;
        CatalogVerdict::Verified { document, signer }
    }

    /// Records the root that verified the catalog, so a later process skips the TUF fetch.
    ///
    /// Runs after a signature verified and the catalog is not expired, before the index's rollback decision, so
    /// a catalog refused for rollback still seeds genuine TUF material. A root nothing has verified against may
    /// be a hostile mirror's and would overwrite the key `ocx package verify` cached. Never offline; failure
    /// is the cache's own debug log.
    async fn seed_trust_root_cache(&self, verified_by: &ResolvedRoot) {
        let Some(from) = self.roots_from.as_ref() else {
            return;
        };
        if from.offline {
            return;
        }
        verified_by
            .cache_after_verdict(&from.rekor_cache_key, &from.state)
            .await;
    }

    /// Verifies the signature against each instance's root; one root that accepts it is enough, and it is returned
    /// with the signer.
    async fn verify_against(
        &self,
        roots: &[ResolvedRoot],
        policy: &CompiledPolicy,
        catalog_bytes: &[u8],
        signature: &[u8],
    ) -> Result<(String, ResolvedRoot), CatalogVerdict> {
        let keyless_only = !policy
            .backends
            .iter()
            .any(|backend| matches!(backend, PolicyBackend::Key(_)));
        // A key entry needs no root; the empty one stands in when no keyless instance supplied any.
        let roots = match (roots.is_empty(), keyless_only) {
            (false, _) => roots.to_vec(),
            // Keyless entries only, and none names an instance with a local table: nobody can match.
            (true, true) => return Err(CatalogVerdict::PublisherMismatch),
            (true, false) => vec![ResolvedRoot {
                root: TrustRoot::default(),
                fetched: false,
            }],
        };
        // The registry a refused predicate named: the generic refusal calls a registry mismatch a type mismatch.
        let signed_for = std::sync::Mutex::new(None::<String>);
        let registry_is = |predicate: &serde_json::Value| {
            let signed = predicate.get("registry").and_then(serde_json::Value::as_str);
            let accepted = signed == Some(self.registry.as_str());
            if !accepted && let Ok(mut slot) = signed_for.lock() {
                *slot = Some(signed.unwrap_or("no registry").to_owned());
            }
            accepted
        };
        let mut outcomes = Vec::with_capacity(roots.len());
        for resolved in &roots {
            let outcome = verify_statement(
                catalog_bytes,
                signature,
                CATALOG_SUBJECT,
                INDEX_CATALOG_PREDICATE_TYPE,
                &registry_is,
                policy,
                &resolved.root,
            )
            .await
            .map(|verified| (describe(&verified.signer), resolved.clone()));
            let accepted = outcome.is_ok();
            outcomes.push(outcome);
            if accepted {
                break;
            }
        }

        let mut failure = None;
        for outcome in outcomes {
            match outcome {
                Ok(accepted) => return Ok(accepted),
                Err(VerifyErrorKind::IdentityMismatch | VerifyErrorKind::IssuerMismatch) => {
                    failure.get_or_insert(CatalogVerdict::PublisherMismatch);
                }
                // A refusal that is not a plain mismatch outranks one: it says the signature itself is bad.
                Err(error) => {
                    if matches!(failure, None | Some(CatalogVerdict::PublisherMismatch)) {
                        let registry_refusal = match error {
                            VerifyErrorKind::PredicateTypeMismatch { .. } => {
                                signed_for.lock().ok().and_then(|slot| slot.clone())
                            }
                            _ => None,
                        };
                        let message = match registry_refusal {
                            Some(signed) => format!(
                                "the catalog is signed for registry {signed:?}, but this source is {:?}",
                                self.registry
                            ),
                            None => error.to_string(),
                        };
                        failure = Some(CatalogVerdict::Invalid(message));
                    }
                }
            }
        }
        Err(failure.unwrap_or(CatalogVerdict::PublisherMismatch))
    }
}

/// A display description of a signer: its certificate identity and issuer, or its key hint.
fn describe(signer: &SignerIdentity) -> String {
    match signer {
        SignerIdentity::Keyless {
            certificate_identity,
            certificate_oidc_issuer,
        } => format!("{certificate_identity} ({certificate_oidc_issuer})"),
        SignerIdentity::Key { public_key_hint, .. } => format!("key {public_key_hint}"),
    }
}

#[async_trait]
impl CatalogVerifier for IndexTrust {
    async fn check(&self, catalog_bytes: &[u8], signature: Option<&[u8]>) -> CatalogVerdict {
        self.check_at(catalog_bytes, signature, Utc::now()).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ocx_sign::detached::{SigningEndpoints, sign_statement};
    use ocx_sign::sign::key_backend::PemKeyBackend;
    use ocx_sign::sign::key_signer::KeySigner;
    use ocx_trust::key_ref::KeyBackendKind;
    use url::Url;

    use super::*;

    const GOLDEN_KEY: &[u8] = include_bytes!("../../../test/tests/fixtures/golden/keys/cosign.key");
    const GOLDEN_PUBLIC_KEY: &str = include_str!("../../../test/tests/fixtures/golden/keys/cosign.pub");
    const OTHER_PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEHKf0zqUEudwCix/9rS4Fn7nEjpG3\nIJt/JQukjC1J2EuX6CcB6cX/2wtIoc1arumQbrlNjFWMT6QAz3SEHOOcjA==\n-----END PUBLIC KEY-----\n";
    const ISSUER: &str = "https://token.actions.githubusercontent.com";

    fn now() -> DateTime<Utc> {
        "2026-06-01T00:00:00Z".parse().expect("a fixed instant")
    }

    fn policy_of(entries: serde_json::Value) -> CompiledPolicy {
        parse_publishers(&entries).compile().expect("the entries compile")
    }

    fn key_policy(pem: &str) -> CompiledPolicy {
        policy_of(serde_json::json!([{ "kind": "key", "key_pem": pem }]))
    }

    fn keyless_policy(sigstore: Option<&str>) -> CompiledPolicy {
        let mut entry = serde_json::json!({ "kind": "keyless", "identity": "me", "oidc_issuer": ISSUER });
        if let Some(authority) = sigstore {
            entry["sigstore"] = authority.into();
        }
        policy_of(serde_json::json!([entry]))
    }

    fn catalog(expires: Option<&str>) -> Vec<u8> {
        let mut document = serde_json::json!({ "format_version": 1, "packages": {} });
        if let Some(expires) = expires {
            document["expires"] = expires.into();
        }
        serde_json::to_vec(&document).expect("a catalog serializes")
    }

    async fn sign(payload: &[u8], predicate: serde_json::Value, predicate_type: &str) -> Vec<u8> {
        let backend = PemKeyBackend::from_encrypted_pem(GOLDEN_KEY, b"ocxtest", KeyBackendKind::File)
            .expect("the golden key decrypts");
        let signer = KeySigner::new(Arc::new(backend), None);
        let (fulcio, rekor) = (
            Url::parse("https://fulcio.example").expect("a url"),
            Url::parse("https://rekor.example").expect("a url"),
        );
        let endpoints = SigningEndpoints {
            fulcio_url: &fulcio,
            rekor_url: &rekor,
            token: None,
            trusted_hosts: &[],
        };
        sign_statement(
            payload,
            CATALOG_SUBJECT,
            predicate_type,
            &predicate,
            &signer,
            &endpoints,
        )
        .await
        .expect("a key signer signs")
        .bundle
    }

    async fn sign_catalog(payload: &[u8]) -> Vec<u8> {
        sign(
            payload,
            serde_json::json!({ "registry": "ocx.sh" }),
            INDEX_CATALOG_PREDICATE_TYPE,
        )
        .await
    }

    fn trust(policy: CompiledPolicy, expiry: IndexExpiryMode) -> IndexTrust {
        IndexTrust::new("ocx.sh".to_owned(), vec![policy], expiry, SigstoreInstances::default())
    }

    fn verdict_name(verdict: &CatalogVerdict) -> &'static str {
        match verdict {
            CatalogVerdict::Verified { .. } => "verified",
            CatalogVerdict::Unsigned => "unsigned",
            CatalogVerdict::Invalid(_) => "invalid",
            CatalogVerdict::PublisherMismatch => "mismatch",
            CatalogVerdict::Expired { .. } => "expired",
            CatalogVerdict::Unrecognized => "unrecognized",
        }
    }

    async fn judge(verifier: &IndexTrust, payload: &[u8], bundle: Option<&[u8]>) -> CatalogVerdict {
        verifier.check_at(payload, bundle, now()).await
    }

    fn registry_config(toml_text: &str) -> RegistryConfig {
        toml::from_str(toml_text).expect("a registry table parses")
    }

    #[test]
    fn built_in_signer_pins_the_render_deploy_workflow() {
        assert_eq!(
            OCX_SH_INDEX_SIGNER.identity,
            "https://github.com/ocx-sh/index/.github/workflows/render-deploy.yml@refs/heads/main"
        );
        assert_eq!(
            OCX_SH_INDEX_SIGNER.issuer,
            "https://token.actions.githubusercontent.com"
        );
        assert_eq!(
            OCX_SH_INDEX_SIGNER.claims,
            &[
                ("source_repository_identifier", "1297393033"),
                ("source_repository_owner_identifier", "263156444"),
            ]
        );
    }

    /// The constant compiles to exactly one keyless entry on the public instance, carrying both claims.
    #[test]
    fn built_in_signer_compiles_to_one_public_keyless_entry_with_both_claims() {
        let signing = resolve_index_signing("ocx.sh", None).expect("ocx.sh resolves");
        let [policy] = signing.signers.as_slice() else {
            panic!("one policy expected");
        };
        let [PolicyBackend::Keyless(keyless)] = policy.backends.as_slice() else {
            panic!("one keyless entry expected");
        };
        assert_eq!(keyless.instance, SigstoreAuthority::public());
        assert_eq!(keyless.claims.len(), 2);
    }

    /// `(registry, registry table, expected list length, expected mode)`.
    #[test]
    fn resolution_table() {
        let publishers =
            "index_publishers = [{ kind = \"keyless\", identity = \"me\", oidc_issuer = \"https://i\" }]\n";
        let cases: Vec<(&str, Option<RegistryConfig>, usize, IndexSignatureMode)> = vec![
            // Built-in list for ocx.sh, warn by default.
            ("ocx.sh", None, 1, IndexSignatureMode::Warn),
            // No list for any other registry: off.
            ("example.com", None, 0, IndexSignatureMode::Off),
            // index_publishers replaces the built-in list and implies warn.
            ("ocx.sh", Some(registry_config(publishers)), 1, IndexSignatureMode::Warn),
            (
                "example.com",
                Some(registry_config(publishers)),
                1,
                IndexSignatureMode::Warn,
            ),
            // An explicit mode wins.
            (
                "ocx.sh",
                Some(registry_config("index_signature = \"off\"\n")),
                1,
                IndexSignatureMode::Off,
            ),
            (
                "ocx.sh",
                Some(registry_config("index_signature = \"enforce\"\n")),
                1,
                IndexSignatureMode::Enforce,
            ),
            // An empty list is a list with no usable entry, not a missing one.
            (
                "example.com",
                Some(registry_config(
                    "index_publishers = []\nindex_signature = \"enforce\"\n",
                )),
                0,
                IndexSignatureMode::Enforce,
            ),
            // An entry of a newer kind leaves no usable signer.
            (
                "example.com",
                Some(registry_config(
                    "index_publishers = [{ kind = \"future\" }]\nindex_signature = \"enforce\"\n",
                )),
                0,
                IndexSignatureMode::Enforce,
            ),
        ];
        for (registry, config, listed, mode) in cases {
            let signing = resolve_index_signing(registry, config.as_ref())
                .unwrap_or_else(|error| panic!("{registry} {config:?}: {error}"));
            assert_eq!(signing.signers.len(), listed, "{registry} {config:?}");
            assert_eq!(signing.mode, mode, "{registry} {config:?}");
            assert_eq!(signing.expiry, IndexExpiryMode::Check, "{registry} {config:?}");
        }
    }

    #[test]
    fn enforce_with_no_list_is_refused() {
        let config = registry_config("index_signature = \"enforce\"\n");
        let error = resolve_index_signing("example.com", Some(&config))
            .err()
            .expect("refused");
        assert!(
            matches!(error, ocx_config::error::Error::IndexPublishersMissing { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_malformed_entry_is_refused() {
        let config = registry_config("index_publishers = [{ kind = \"keyless\" }]\n");
        let error = resolve_index_signing("example.com", Some(&config))
            .err()
            .expect("refused");
        assert!(
            matches!(error, ocx_config::error::Error::IndexPublishersInvalid { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn index_expiry_ignore_is_carried_through() {
        let config = registry_config("index_expiry = \"ignore\"\n");
        let signing = resolve_index_signing("ocx.sh", Some(&config)).expect("resolves");
        assert_eq!(signing.expiry, IndexExpiryMode::Ignore);
    }

    #[tokio::test]
    async fn a_signature_by_the_listed_key_over_the_catalog_verifies() {
        let payload = catalog(Some("2026-07-01T00:00:00Z"));
        let bundle = sign_catalog(&payload).await;
        let verdict = judge(
            &trust(key_policy(GOLDEN_PUBLIC_KEY), IndexExpiryMode::Check),
            &payload,
            Some(&bundle),
        )
        .await;
        let CatalogVerdict::Verified { document, signer } = verdict else {
            panic!("expected verified");
        };
        assert!(document.expires.is_some());
        assert!(signer.starts_with("key "), "{signer}");
    }

    #[tokio::test]
    async fn no_signature_is_unsigned() {
        let payload = catalog(Some("2026-07-01T00:00:00Z"));
        let verdict = judge(
            &trust(key_policy(GOLDEN_PUBLIC_KEY), IndexExpiryMode::Check),
            &payload,
            None,
        )
        .await;
        assert_eq!(verdict_name(&verdict), "unsigned");
    }

    #[tokio::test]
    async fn a_list_with_no_entry_is_unrecognized_even_without_a_signature() {
        let verifier = IndexTrust::new(
            "ocx.sh".to_owned(),
            Vec::new(),
            IndexExpiryMode::Check,
            SigstoreInstances::default(),
        );
        let verdict = judge(&verifier, &catalog(None), None).await;
        assert_eq!(verdict_name(&verdict), "unrecognized");
    }

    #[tokio::test]
    async fn a_signature_by_an_unlisted_key_is_invalid() {
        let payload = catalog(Some("2026-07-01T00:00:00Z"));
        let bundle = sign_catalog(&payload).await;
        let verdict = judge(
            &trust(key_policy(OTHER_PUBLIC_KEY), IndexExpiryMode::Check),
            &payload,
            Some(&bundle),
        )
        .await;
        assert_eq!(verdict_name(&verdict), "invalid");
    }

    #[tokio::test]
    async fn a_changed_catalog_is_invalid() {
        let bundle = sign_catalog(&catalog(Some("2026-07-01T00:00:00Z"))).await;
        let changed = catalog(Some("2026-07-02T00:00:00Z"));
        let verdict = judge(
            &trust(key_policy(GOLDEN_PUBLIC_KEY), IndexExpiryMode::Check),
            &changed,
            Some(&bundle),
        )
        .await;
        assert_eq!(verdict_name(&verdict), "invalid");
    }

    /// A signature the same key made for another registry's index is refused for this one.
    #[tokio::test]
    async fn a_signature_for_another_registry_is_invalid() {
        let payload = catalog(Some("2026-07-01T00:00:00Z"));
        let bundle = sign(
            &payload,
            serde_json::json!({ "registry": "other.example" }),
            INDEX_CATALOG_PREDICATE_TYPE,
        )
        .await;
        let verdict = judge(
            &trust(key_policy(GOLDEN_PUBLIC_KEY), IndexExpiryMode::Check),
            &payload,
            Some(&bundle),
        )
        .await;
        assert_eq!(verdict_name(&verdict), "invalid");
        let CatalogVerdict::Invalid(message) = verdict else {
            panic!("invalid");
        };
        assert!(
            message.contains("other.example") && message.contains("ocx.sh"),
            "{message}"
        );
        assert!(!message.contains("predicate type mismatch"), "{message}");
    }

    #[tokio::test]
    async fn a_signature_under_another_predicate_type_is_invalid() {
        let payload = catalog(Some("2026-07-01T00:00:00Z"));
        let bundle = sign(
            &payload,
            serde_json::json!({ "registry": "ocx.sh" }),
            "https://example.com/other/v1",
        )
        .await;
        let verdict = judge(
            &trust(key_policy(GOLDEN_PUBLIC_KEY), IndexExpiryMode::Check),
            &payload,
            Some(&bundle),
        )
        .await;
        assert_eq!(verdict_name(&verdict), "invalid");
    }

    /// Keyless entries naming an instance with no local table match nobody, and no root is dialled for them.
    #[tokio::test]
    async fn a_keyless_entry_of_an_unknown_instance_is_a_publisher_mismatch() {
        let payload = catalog(Some("2026-07-01T00:00:00Z"));
        let bundle = sign_catalog(&payload).await;
        for authority in [None, Some("localhost:5555")] {
            let verdict = judge(
                &trust(keyless_policy(authority), IndexExpiryMode::Check),
                &payload,
                Some(&bundle),
            )
            .await;
            assert_eq!(verdict_name(&verdict), "mismatch", "{authority:?}");
        }
    }

    /// `(expires, mode, expected)` against `now()` = 2026-06-01T00:00:00Z.
    #[tokio::test]
    async fn expiry_boundaries() {
        let cases = [
            // Equal to now and a second before count as expired; a second after is valid.
            (Some("2026-06-01T00:00:00Z"), IndexExpiryMode::Check, "expired"),
            (Some("2026-05-31T23:59:59Z"), IndexExpiryMode::Check, "expired"),
            (Some("2026-06-01T00:00:01Z"), IndexExpiryMode::Check, "verified"),
            // `ignore` turns expired into verified.
            (Some("2026-06-01T00:00:00Z"), IndexExpiryMode::Ignore, "verified"),
            // No `expires` counts as expired.
            (None, IndexExpiryMode::Check, "expired"),
            (None, IndexExpiryMode::Ignore, "verified"),
            // 400 days ahead is accepted; a second more is refused, `ignore` or not.
            (Some("2027-07-06T00:00:00Z"), IndexExpiryMode::Check, "verified"),
            (Some("2027-07-06T00:00:01Z"), IndexExpiryMode::Check, "invalid"),
            (Some("2027-07-06T00:00:01Z"), IndexExpiryMode::Ignore, "invalid"),
        ];
        for (expires, mode, expected) in cases {
            let payload = catalog(expires);
            let bundle = sign_catalog(&payload).await;
            let verdict = judge(&trust(key_policy(GOLDEN_PUBLIC_KEY), mode), &payload, Some(&bundle)).await;
            assert_eq!(verdict_name(&verdict), expected, "{expires:?} {mode:?}");
        }
    }

    #[tokio::test]
    async fn an_expired_verdict_names_when() {
        let payload = catalog(Some("2026-05-31T23:59:59Z"));
        let bundle = sign_catalog(&payload).await;
        let verdict = judge(
            &trust(key_policy(GOLDEN_PUBLIC_KEY), IndexExpiryMode::Check),
            &payload,
            Some(&bundle),
        )
        .await;
        let CatalogVerdict::Expired { expires } = verdict else {
            panic!("expected expired");
        };
        assert_eq!(expires, "2026-05-31T23:59:59Z".parse().ok());
    }

    #[tokio::test]
    async fn a_malformed_expires_in_a_signed_catalog_is_invalid() {
        let payload = br#"{"format_version":1,"packages":{},"expires":"tomorrow"}"#.to_vec();
        let bundle = sign_catalog(&payload).await;
        let verdict = judge(
            &trust(key_policy(GOLDEN_PUBLIC_KEY), IndexExpiryMode::Check),
            &payload,
            Some(&bundle),
        )
        .await;
        assert_eq!(verdict_name(&verdict), "invalid");
    }

    fn root_pinning(rekor_key: &[u8]) -> TrustRoot {
        TrustRoot::from_material(
            vec![vec![0x30, 0x00]],
            std::collections::BTreeMap::from([("c0ffee".to_owned(), vec![0x30, 0x00])]),
            std::collections::BTreeMap::from([("aa".to_owned(), rekor_key.to_vec())]),
        )
    }

    /// A verifier whose resolved roots are `roots`, writing its trust-root cache under `state`.
    fn verifier_over(policy: CompiledPolicy, roots: Vec<ResolvedRoot>, state: &SigningStatePaths) -> IndexTrust {
        let verifier = trust(policy, IndexExpiryMode::Check).with_trust_roots(IndexTrustRoots {
            home_root_of: Box::new(|_| None),
            state: state.clone(),
            rekor_cache_key: "rekor.example".to_owned(),
            offline: false,
        });
        verifier.roots.set(roots).expect("the roots are not resolved yet");
        verifier
    }

    async fn cached_key(state: &SigningStatePaths) -> Option<String> {
        ocx_sign::verify::TrustRootCache::from_cache("rekor.example", state)
            .await
            .expect("the cache reads")
            .and_then(|entry| entry.rekor_public_key_pem)
    }

    async fn catalog_and_bundle() -> (Vec<u8>, Vec<u8>) {
        let payload = catalog(Some("2026-07-01T00:00:00Z"));
        let bundle = sign_catalog(&payload).await;
        (payload, bundle)
    }

    #[tokio::test]
    async fn a_verified_catalog_seeds_the_cache_with_the_root_that_verified_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = SigningStatePaths::new(tmp.path().join("state"));
        let root = root_pinning(&[1, 2, 3]);
        let expected = root.single_rekor_key_pem();
        let verifier = verifier_over(
            key_policy(GOLDEN_PUBLIC_KEY),
            vec![ResolvedRoot { root, fetched: true }],
            &state,
        );
        let (payload, bundle) = catalog_and_bundle().await;
        assert!(cached_key(&state).await.is_none(), "nothing before the verdict");

        let verdict = judge(&verifier, &payload, Some(&bundle)).await;

        assert_eq!(verdict_name(&verdict), "verified");
        assert_eq!(
            cached_key(&state).await,
            expected,
            "a later process reads the proven key"
        );
    }

    #[tokio::test]
    async fn a_failed_verdict_seeds_nothing_and_leaves_a_cached_key_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = SigningStatePaths::new(tmp.path().join("state"));
        let earlier = ocx_sign::verify::TrustRootCache::new(
            "rekor.example".to_owned(),
            vec![vec![0x30, 0x00]],
            std::collections::BTreeMap::new(),
            "EARLIER".to_owned(),
        );
        let verifier = verifier_over(
            key_policy(OTHER_PUBLIC_KEY),
            vec![ResolvedRoot {
                root: root_pinning(&[9, 9]),
                fetched: true,
            }],
            &state,
        );
        let (payload, bundle) = catalog_and_bundle().await;

        let before = judge(&verifier, &payload, Some(&bundle)).await;
        assert_eq!(verdict_name(&before), "invalid");
        assert!(cached_key(&state).await.is_none(), "a refused catalog seeds nothing");

        earlier.write_cache(&state).await.expect("the earlier entry writes");
        let after = judge(&verifier, &payload, Some(&bundle)).await;
        assert_eq!(verdict_name(&after), "invalid");
        assert_eq!(
            cached_key(&state).await.as_deref(),
            Some("EARLIER"),
            "a refused catalog must not overwrite what `ocx package verify` cached"
        );
    }

    /// The key that just verified replaces a different cached one; the cached one verified nothing here.
    #[tokio::test]
    async fn a_verifying_key_replaces_a_different_cached_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = SigningStatePaths::new(tmp.path().join("state"));
        ocx_sign::verify::TrustRootCache::new(
            "rekor.example".to_owned(),
            vec![vec![0x30, 0x00]],
            std::collections::BTreeMap::new(),
            "EARLIER".to_owned(),
        )
        .write_cache(&state)
        .await
        .expect("the earlier entry writes");
        let root = root_pinning(&[4, 5, 6]);
        let expected = root.single_rekor_key_pem();
        let verifier = verifier_over(
            key_policy(GOLDEN_PUBLIC_KEY),
            vec![ResolvedRoot { root, fetched: true }],
            &state,
        );
        let (payload, bundle) = catalog_and_bundle().await;

        assert_eq!(
            verdict_name(&judge(&verifier, &payload, Some(&bundle)).await),
            "verified"
        );
        assert_eq!(cached_key(&state).await, expected);
    }

    /// An operator-named or cached root is not this process's fetch, so a verdict never writes it.
    #[tokio::test]
    async fn a_verified_root_that_was_not_fetched_is_never_cached() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = SigningStatePaths::new(tmp.path().join("state"));
        let verifier = verifier_over(
            key_policy(GOLDEN_PUBLIC_KEY),
            vec![ResolvedRoot {
                root: root_pinning(&[1, 2, 3]),
                fetched: false,
            }],
            &state,
        );
        let (payload, bundle) = catalog_and_bundle().await;

        assert_eq!(
            verdict_name(&judge(&verifier, &payload, Some(&bundle)).await),
            "verified"
        );
        assert!(cached_key(&state).await.is_none());
    }

    /// An expired catalog is a refusal, not a verdict that proves the root.
    #[tokio::test]
    async fn an_expired_catalog_seeds_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = SigningStatePaths::new(tmp.path().join("state"));
        let verifier = verifier_over(
            key_policy(GOLDEN_PUBLIC_KEY),
            vec![ResolvedRoot {
                root: root_pinning(&[1, 2, 3]),
                fetched: true,
            }],
            &state,
        );
        let payload = catalog(Some("2026-05-31T23:59:59Z"));
        let bundle = sign_catalog(&payload).await;

        assert_eq!(
            verdict_name(&judge(&verifier, &payload, Some(&bundle)).await),
            "expired"
        );
        assert!(cached_key(&state).await.is_none());
    }
}
