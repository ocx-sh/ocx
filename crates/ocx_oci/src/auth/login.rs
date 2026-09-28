// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Login / logout orchestration.

use secrecy::ExposeSecret as _;

use crate::auth::registry_url::canonicalize_registry;
use crate::auth::{AuthError, Credential, CredentialStore};

/// The registry probe (`GET /v2/`) `login()` validates a credential with before storing it.
///
/// Not `Client::ensure_auth`: that resolves through the cached auth chain, so it would validate the cache,
/// not the supplied credential.
#[async_trait::async_trait]
pub trait RegistryPing: Send + Sync {
    /// Probe the registry with `cred` applied; `Ok(())` on a 2xx.
    ///
    /// # Errors
    /// [`AuthError::LoginRejected`] when the registry refused the credential, [`AuthError::ProbeFailed`] when
    /// it never judged it.
    async fn ping(&self, registry: &str, cred: &Credential) -> Result<(), AuthError>;
}

/// [`RegistryPing`] against a real registry, on a fresh client per call so cached auth cannot pollute the probe.
///
/// Takes the caller's insecure hosts and CA roots rather than reading the environment, or login trusts
/// differently than every later command.
pub struct OciClientPing {
    insecure_hosts: Vec<String>,
    extra_roots: ocx_util::tls::ExtraRoots,
}

impl OciClientPing {
    /// Probes registries over HTTPS, except the hosts named here, trusting
    /// `extra_roots` on top of the bundled set.
    pub fn new(insecure_hosts: Vec<String>, extra_roots: ocx_util::tls::ExtraRoots) -> Self {
        Self {
            insecure_hosts,
            extra_roots,
        }
    }

    // Built via `ClientBuilder`, not `ClientConfig { .. }`, or the probe drifts from the transport every
    // other command dials with.
    fn config(&self, registry: &str) -> oci_client::client::ClientConfig {
        let mut config = crate::ClientBuilder::new()
            .extra_roots(self.extra_roots.clone())
            .config();
        config.protocol = self.protocol_for(registry);
        config
    }

    // Never the blanket `ClientProtocol::Http`: it makes every realm host plaintext-eligible, so an insecure
    // registry could redirect the raw Basic password to any host in the clear.
    fn protocol_for(&self, _registry: &str) -> oci_client::client::ClientProtocol {
        oci_client::client::ClientProtocol::HttpsExcept(self.insecure_hosts.clone())
    }
}

#[async_trait::async_trait]
impl RegistryPing for OciClientPing {
    async fn ping(&self, registry: &str, cred: &Credential) -> Result<(), AuthError> {
        use oci_client::Reference;
        use oci_client::client::Client as RawClient;

        let raw = RawClient::new(self.config(registry));
        let auth = to_registry_auth(cred);
        // Placeholder repository: `GET /v2/` is repository-agnostic.
        let reference = Reference::with_tag(registry.to_string(), "library/_".into(), "latest".into());
        raw.auth(&reference, &auth, crate::RegistryOperation::Pull)
            .await
            .map_err(|source| probe_error(registry, source))?;
        Ok(())
    }
}

// Routed through `registry_error`, or a plaintext registry's failed HTTPS connect loses its remediation and
// blames the password.
fn probe_error(registry: &str, source: oci_client::errors::OciDistributionError) -> AuthError {
    match crate::client::native_transport::registry_error(source) {
        crate::client::error::ClientError::Authentication(_) => AuthError::LoginRejected {
            registry: registry.to_string(),
        },
        other => AuthError::ProbeFailed {
            registry: registry.to_string(),
            source: Box::new(other),
        },
    }
}

fn to_registry_auth(cred: &Credential) -> oci_client::secrets::RegistryAuth {
    use oci_client::secrets::RegistryAuth;
    if !cred.refresh_token.expose_secret().is_empty() {
        return RegistryAuth::Bearer(cred.refresh_token.expose_secret().to_string());
    }
    if !cred.access_token.expose_secret().is_empty() {
        return RegistryAuth::Bearer(cred.access_token.expose_secret().to_string());
    }
    RegistryAuth::Basic(cred.username.clone(), cred.password.expose_secret().to_string())
}

/// Validate credentials against the canonicalized registry, then store them.
///
/// # Errors
/// The probe's error, without calling `put`: bad credentials never reach the store.
pub async fn login(
    registry: &str,
    cred: &Credential,
    store: &dyn CredentialStore,
    client: &dyn RegistryPing,
) -> Result<(), AuthError> {
    let canonical = canonicalize_registry(registry);
    client.ping(&canonical, cred).await?;
    store.put(&canonical, cred).await
}

/// Remove credentials for `registry`. No registry round-trip.
pub async fn logout(registry: &str, store: &dyn CredentialStore) -> Result<(), AuthError> {
    let canonical = canonicalize_registry(registry);
    store.delete(&canonical).await
}

// ─────────────────────────── tests ───────────────────────────
//
// `login()` now takes a `&dyn RegistryPing` so the Ping-then-Put invariant is
// exercised with a `MockPing` against `MockStore`. The 3 previously-ignored
// specifications are now executable.
#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockStore {
        gets: Mutex<Vec<String>>,
        puts: Mutex<Vec<String>>,
        deletes: Mutex<Vec<String>>,
        delete_result: Mutex<Option<AuthError>>,
    }

    #[async_trait::async_trait]
    impl CredentialStore for MockStore {
        async fn get(&self, registry: &str) -> Result<Option<Credential>, AuthError> {
            self.gets.lock().unwrap().push(registry.into());
            Ok(None)
        }
        async fn put(&self, registry: &str, _cred: &Credential) -> Result<(), AuthError> {
            self.puts.lock().unwrap().push(registry.into());
            Ok(())
        }
        async fn delete(&self, registry: &str) -> Result<(), AuthError> {
            self.deletes.lock().unwrap().push(registry.into());
            if let Some(err) = self.delete_result.lock().unwrap().take() {
                return Err(err);
            }
            Ok(())
        }
    }

    struct MockPing {
        result: Mutex<Result<(), AuthError>>,
        calls: Mutex<Vec<String>>,
    }

    impl MockPing {
        fn ok() -> Self {
            Self {
                result: Mutex::new(Ok(())),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn rejected(reg: &str) -> Self {
            Self {
                result: Mutex::new(Err(AuthError::LoginRejected {
                    registry: reg.to_string(),
                })),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl RegistryPing for MockPing {
        async fn ping(&self, registry: &str, _cred: &Credential) -> Result<(), AuthError> {
            self.calls.lock().unwrap().push(registry.to_string());
            let mut guard = self.result.lock().unwrap();
            // Replace the result so callers can re-use the mock for follow-ups.
            std::mem::replace(&mut *guard, Ok(()))
        }
    }

    #[tokio::test]
    async fn logout_calls_store_delete() {
        let store = MockStore::default();
        logout("ghcr.io", &store).await.expect("logout");
        let deletes = store.deletes.lock().unwrap();
        assert!(
            deletes.iter().any(|r| !r.is_empty()),
            "logout must invoke store.delete; saw: {deletes:?}",
        );
    }

    #[tokio::test]
    async fn logout_returns_ok_even_when_store_delete_noop() {
        let store = MockStore::default();
        let result = logout("ghcr.io", &store).await;
        assert!(
            matches!(result, Ok(())),
            "logout must surface Ok(()) for noop deletes (oras-go semantics), got: {result:?}",
        );
    }

    // ─── Ping-then-Put invariants ───

    #[tokio::test]
    async fn login_calls_store_put_only_after_ping_success() {
        let store = MockStore::default();
        let ping = MockPing::ok();
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        login("ghcr.io", &cred, &store, &ping)
            .await
            .expect("login should succeed");
        assert_eq!(
            store.puts.lock().unwrap().as_slice(),
            &["ghcr.io".to_string()],
            "put must be called once with canonical registry after ping success",
        );
    }

    #[tokio::test]
    async fn login_returns_login_rejected_when_ping_fails_and_store_put_not_called() {
        let store = MockStore::default();
        let ping = MockPing::rejected("ghcr.io");
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        let result = login("ghcr.io", &cred, &store, &ping).await;
        assert!(
            matches!(result, Err(AuthError::LoginRejected { ref registry }) if registry == "ghcr.io"),
            "expected LoginRejected, got: {result:?}",
        );
        assert!(
            store.puts.lock().unwrap().is_empty(),
            "put MUST NOT be called when ping fails — load-bearing security invariant",
        );
    }

    #[tokio::test]
    async fn login_canonicalizes_registry_before_put() {
        let store = MockStore::default();
        let ping = MockPing::ok();
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        login("https://ghcr.io/v1/", &cred, &store, &ping).await.expect("login");
        assert_eq!(
            store.puts.lock().unwrap().as_slice(),
            &["ghcr.io".to_string()],
            "canonicalization must apply to the put key",
        );
        assert_eq!(
            ping.calls.lock().unwrap().as_slice(),
            &["ghcr.io".to_string()],
            "ping must see the canonical registry, not the raw scheme/version form",
        );
    }

    // ─── `OciClientPing`'s protocol choice ───

    fn ping_allowing(hosts: &[&str]) -> OciClientPing {
        OciClientPing::new(
            hosts.iter().map(|host| (*host).to_string()).collect(),
            ocx_util::tls::ExtraRoots::default(),
        )
    }

    /// C-006 / S-001 / S-003 at unit tier (review r1): the fork transport's
    /// handshake proof — the ONE path registry traffic takes, which every
    /// acceptance registry hop dials over plain HTTP. `OciClientPing::ping`
    /// against an in-process HTTPS server signed by a minted root answering
    /// `GET /v2/` with a 200 (anonymous, no challenge): with the root the
    /// probe is `Ok`; with the default set the chain ends in `UnknownIssuer`.
    /// Both halves in one test.
    ///
    /// Mutation: drop `.extra_roots(..)` from `OciClientPing::config`, or
    /// break the fork's `convert_certificates` — the first half reds.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_probe_handshake_trusts_the_extra_ca_roots_and_only_then() {
        use ocx_test_support::pki::{TestPki, assert_untrusted_root, error_chain, serve_https};
        use ocx_util::tls::ExtraRoots;

        let pki = TestPki::mint();
        let addr = serve_https(&pki).await;
        let registry = addr.to_string();
        let anonymous = Credential::basic("", SecretString::default());

        OciClientPing::new(
            vec![],
            ocx_util::tls::ExtraRoots::from_pem(pki.root_pem().as_bytes()).expect("a minted root parses"),
        )
        .ping(&registry, &anonymous)
        .await
        .unwrap_or_else(|error| panic!("the seeded probe trusts the minted root: {}", error_chain(&error)));

        let error = OciClientPing::new(vec![], ExtraRoots::default())
            .ping(&registry, &anonymous)
            .await
            .expect_err("without the root the probe must refuse the handshake");
        let chain = error_chain(&error);
        assert_untrusted_root(&chain);
    }

    /// C-006 (ocx#448): the probe's client carries the operator's extra CA
    /// roots after the bundled Mozilla set, and the builder's bounded
    /// timeouts with them — a `ClientConfig { .. }` composed here by hand
    /// carried neither, so `ocx login` against a corp-CA registry failed
    /// `UnknownIssuer` although every later command would have trusted it.
    ///
    /// Mutation: drop `.extra_roots(..)` from `OciClientPing::config` — the
    /// count equals the Mozilla set and this reds.
    #[test]
    fn the_probe_client_carries_the_extra_ca_roots_and_the_shared_timeouts() {
        use ocx_test_support::pki::TestPki;
        use ocx_util::tls::ExtraRoots;

        let pki = TestPki::mint();
        let roots = ExtraRoots::from_pem(pki.root_pem().as_bytes()).expect("a minted root");
        let ping = OciClientPing::new(vec!["registry.corp:5000".to_string()], roots);

        let config = ping.config("ghcr.io");

        let mozilla = webpki_root_certs::TLS_SERVER_ROOT_CERTS.len();
        assert_eq!(config.extra_root_certificates.len(), mozilla + 1);
        assert_eq!(config.extra_root_certificates[mozilla].data, pki.root_der);
        assert_eq!(config.read_timeout, Some(crate::client::REGISTRY_READ_TIMEOUT));
        assert!(
            config.connect_timeout.is_some(),
            "the builder's connect bound comes with it — the hand-rolled config had none"
        );
        assert!(
            matches!(config.protocol, oci_client::client::ClientProtocol::HttpsExcept(ref hosts) if hosts == &["registry.corp:5000".to_string()]),
            "the protocol gate is still this probe's own: {:?}",
            config.protocol
        );
    }

    /// Mirrors what `ClientProtocol::scheme_for` does with the value this gate
    /// returns. Spelled out here rather than called, because `scheme_for` is
    /// private to the fork — so the assertion states the transport rule instead
    /// of borrowing it, and reds if this gate ever returns a variant that
    /// resolves differently.
    fn is_http(ping: &OciClientPing, registry: &str) -> bool {
        use oci_client::client::ClientProtocol;
        match ping.protocol_for(registry) {
            ClientProtocol::Http => true,
            ClientProtocol::Https => false,
            ClientProtocol::HttpsExcept(hosts) => hosts.iter().any(|host| host == registry),
        }
    }

    /// The blanket [`ClientProtocol::Http`] must never be what this gate
    /// returns: it ignores its argument, so every host on earth reads as
    /// plaintext-eligible and the transport's auth-realm guard degenerates to
    /// "accept anything" — sending `ocx login`'s raw Basic password to whatever
    /// host a declared-insecure registry names in its `WWW-Authenticate` realm.
    /// Asserted on the variant, not the scheme, because the scheme is identical
    /// either way and only the variant carries the defect.
    #[test]
    fn the_probe_never_widens_plaintext_eligibility_beyond_the_declared_hosts() {
        let ping = ping_allowing(&["registry.corp:5000"]);

        for registry in ["registry.corp:5000", "ghcr.io"] {
            match ping.protocol_for(registry) {
                oci_client::client::ClientProtocol::HttpsExcept(hosts) => assert_eq!(
                    hosts,
                    vec!["registry.corp:5000".to_string()],
                    "plaintext eligibility must be exactly the declared set — an undeclared \
                     third host is the realm the password would be sent to",
                ),
                other => panic!(
                    "probing {registry} must not widen plaintext eligibility beyond the \
                     declared hosts; got {other:?}",
                ),
            }
        }
    }

    /// The one decision the login gate makes, asserted directly: the six gates
    /// the commit says share a predicate include this one, and before this test
    /// nothing at any layer observed it (every acceptance `ocx login` passes
    /// `--no-verify`, so the `RegistryPing` branch is never taken).
    #[test]
    fn a_listed_host_probes_over_http_and_an_unlisted_one_over_https() {
        let ping = ping_allowing(&["registry.corp:5000"]);

        assert!(
            is_http(&ping, "registry.corp:5000"),
            "a listed host must probe over HTTP"
        );
        assert!(
            !is_http(&ping, "ghcr.io"),
            "an unlisted host must probe over HTTPS — the probe fails closed"
        );
    }

    /// Byte-exact, the same rule `insecure_hosts` documents: a near miss is a
    /// miss, in both directions, and case is not folded. Without this the probe
    /// could diverge from the transport on exactly the spellings an operator
    /// gets wrong.
    #[test]
    fn a_near_miss_of_the_allowed_name_probes_over_https() {
        assert!(
            !is_http(&ping_allowing(&["registry.corp"]), "registry.corp:5000"),
            "a bare-host allowance must not cover the same host on a port"
        );
        assert!(
            !is_http(&ping_allowing(&["registry.corp:5000"]), "registry.corp"),
            "a ported allowance must not cover the bare host"
        );
        assert!(
            !is_http(&ping_allowing(&["Registry.Corp:5000"]), "registry.corp:5000"),
            "the comparison is byte-exact, so case matters"
        );
    }

    /// Docker Hub is the one name where this gate's subject differs from the
    /// transport's, and the divergence is closed in the safe direction: neither
    /// host name an operator would write reaches the canonicalized subject
    /// `login()` actually passes in, so no plaintext probe of Docker Hub can be
    /// granted by accident.
    ///
    /// Recorded as a decision, not a gap. Docker Hub is not served over plain
    /// HTTP, and the alternative — normalizing the login subject back to a host
    /// — would make `ocx login` the one gate that rewrites a name before
    /// comparing it.
    #[test]
    fn a_docker_hub_host_name_cannot_license_a_plaintext_probe() {
        let subject = canonicalize_registry("docker.io");
        assert_eq!(
            subject, "https://index.docker.io/v1/",
            "the canonical form login passes in"
        );

        for spelling in ["docker.io", "index.docker.io"] {
            assert!(
                !is_http(&ping_allowing(&[spelling]), &subject),
                "`{spelling}` must not license a plaintext probe of the canonicalized `{subject}`"
            );
        }
    }
}
