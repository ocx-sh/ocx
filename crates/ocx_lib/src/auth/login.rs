// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Login / logout orchestration. Two top-level async functions, not methods on
//! `CredentialStore` — keeps the trait minimal at three protocol verbs.

use secrecy::ExposeSecret as _;

use crate::auth::registry_url::canonicalize_registry;
use crate::auth::{AuthError, Credential, CredentialStore};
use crate::oci;

/// Abstraction over the registry probe (`GET /v2/`) so `login()` can validate
/// credentials before calling `store.put`.
///
/// The default impl wraps `oci::Client::ensure_auth`. The trait exists because
/// `Client::ensure_auth` resolves credentials through the cached `auth::Auth`
/// chain rather than accepting an explicit credential — for login we MUST
/// validate the user-supplied credential exactly, not whatever the cache
/// happens to hold.
#[async_trait::async_trait]
pub trait RegistryPing: Send + Sync {
    /// Probe the registry with `cred` applied. Returns `Ok(())` on a
    /// successful authenticated response (2xx).
    ///
    /// The two failure shapes are kept apart on purpose:
    /// [`AuthError::LoginRejected`] means the registry judged the credential
    /// and said no, [`AuthError::ProbeFailed`] means it was never judged
    /// because the request did not complete. Collapsing them tells a CI
    /// wrapper to rotate a credential that was never sent, and the retry with
    /// a fresh one fails identically, forever.
    async fn ping(&self, registry: &str, cred: &Credential) -> Result<(), AuthError>;
}

/// Adapter that talks to a real OCI registry via the patched `oci-client`
/// crate. Constructs a fresh client per call so cached auth state does not
/// pollute the probe.
///
/// Carries the caller's plain-HTTP host set rather than reading the
/// environment itself: `ocx login` must reach the registry over exactly the
/// scheme the rest of the binary would, and that answer is the union of
/// `[registries.<name>].insecure` and `OCX_INSECURE_REGISTRIES`
/// ([`crate::insecure_hosts`]) — a set a library adapter cannot assemble on
/// its own.
pub struct OciClientPing {
    insecure_hosts: Vec<String>,
}

impl OciClientPing {
    /// Probes registries over HTTPS, except the hosts named here.
    pub fn new(insecure_hosts: Vec<String>) -> Self {
        Self { insecure_hosts }
    }

    /// The scheme this probe will use for `registry`.
    ///
    /// Membership is byte-exact, the same comparison
    /// [`crate::insecure_hosts`] documents and the transport makes, so an
    /// unlisted, differently-cased or differently-ported name falls to
    /// [`ClientProtocol::Https`] — the probe fails closed.
    ///
    /// The subject is the *canonicalized* registry, because [`login`]
    /// canonicalizes before calling [`RegistryPing::ping`]. For `host[:port]`
    /// names canonicalization is the identity, so this agrees with every other
    /// gate. Docker Hub is the exception and deliberately unreachable: it
    /// canonicalizes to `https://index.docker.io/v1/`, which no allowance
    /// spelling matches.
    ///
    /// # Never the blanket [`ClientProtocol::Http`]
    ///
    /// Returning `Http` for a listed host would pick the right scheme for the
    /// probe and destroy the transport's auth-realm guard on the way. `Http`
    /// ignores its argument (`ClientProtocol::scheme_for`), so *every* host
    /// reads as plaintext-eligible — and `require_secure_realm` accepts a
    /// plaintext realm on any host that is plaintext-eligible. A registry
    /// declared insecure could then name
    /// `realm="http://collector.example/token"` and this probe would send the
    /// raw Basic password there in the clear (CWE-319/CWE-522). `ocx login` is
    /// the one command in the binary holding a password, so it is the worst
    /// place to widen that set.
    ///
    /// `HttpsExcept` picks the identical scheme for the probe — `"http"` for
    /// exactly the declared hosts, `"https"` for everything else — while
    /// leaving the realm guard scoped to the hosts the operator actually named.
    fn protocol_for(&self, _registry: &str) -> oci_client::client::ClientProtocol {
        oci_client::client::ClientProtocol::HttpsExcept(self.insecure_hosts.clone())
    }
}

#[async_trait::async_trait]
impl RegistryPing for OciClientPing {
    async fn ping(&self, registry: &str, cred: &Credential) -> Result<(), AuthError> {
        use oci_client::Reference;
        use oci_client::client::{Client as RawClient, ClientConfig};

        let raw = RawClient::new(ClientConfig {
            protocol: self.protocol_for(registry),
            // Same per-read idle bound as the main client: a registry that
            // accepts the connection and then goes quiet must not hang `ocx
            // login` forever.
            read_timeout: Some(oci::client::REGISTRY_READ_TIMEOUT),
            ..Default::default()
        });
        let auth = to_registry_auth(cred);
        // Use a placeholder repository — the registry only ever responds to
        // GET /v2/ at this stage, which is repository-agnostic.
        let reference = Reference::with_tag(registry.to_string(), "library/_".into(), "latest".into());
        raw.auth(&reference, &auth, oci::RegistryOperation::Pull)
            .await
            .map_err(|source| probe_error(registry, source))?;
        Ok(())
    }
}

/// Splits a failed probe into "the registry said no" and "the registry never
/// answered", reusing the transport's own taxonomy rather than a second one.
///
/// [`oci::client::native_transport::registry_error`] is the single place that
/// classifies an `OciDistributionError`, and it is where the plain-HTTP
/// remediation is attached to a failed HTTPS connect — so routing through it
/// is what makes `ocx login` against a plaintext registry say what to do
/// instead of blaming the password.
fn probe_error(registry: &str, source: oci_client::errors::OciDistributionError) -> AuthError {
    match oci::client::native_transport::registry_error(source) {
        crate::oci::client::error::ClientError::Authentication(_) => AuthError::LoginRejected {
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

/// Validate credentials against the registry, then store them.
///
/// 1. Canonicalize `registry` via `auth::registry_url::canonicalize_registry`.
/// 2. `GET /v2/` with the credential applied — `Ping`.
/// 3. On Ping success, `store.put(canonical, &cred)`.
/// 4. On Ping failure, return the `Ping`'s error — `AuthError::LoginRejected`
///    when the registry judged the credential, `AuthError::ProbeFailed` when it
///    never did — WITHOUT calling `put`.
///
/// Bad credentials never reach the store. Single most load-bearing security invariant.
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
        OciClientPing::new(hosts.iter().map(|host| (*host).to_string()).collect())
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
