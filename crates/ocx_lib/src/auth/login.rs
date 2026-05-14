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
    /// successful authenticated response (2xx) and `Err` for any failure
    /// (401, 403, network error, DNS failure, etc.) so the caller can map
    /// to `AuthError::LoginRejected`.
    async fn ping(&self, registry: &str, cred: &Credential) -> Result<(), AuthError>;
}

/// Adapter that talks to a real OCI registry via the patched `oci-client`
/// crate. Constructs a fresh client per call so cached auth state does not
/// pollute the probe.
pub struct OciClientPing;

#[async_trait::async_trait]
impl RegistryPing for OciClientPing {
    async fn ping(&self, registry: &str, cred: &Credential) -> Result<(), AuthError> {
        use oci_client::Reference;
        use oci_client::client::{Client as RawClient, ClientConfig, ClientProtocol};

        let protocol = if is_insecure(registry) {
            ClientProtocol::Http
        } else {
            ClientProtocol::Https
        };
        let raw = RawClient::new(ClientConfig {
            protocol,
            ..Default::default()
        });
        let auth = to_registry_auth(cred);
        // Use a placeholder repository — the registry only ever responds to
        // GET /v2/ at this stage, which is repository-agnostic.
        let reference = Reference::with_tag(registry.to_string(), "library/_".into(), "latest".into());
        raw.auth(&reference, &auth, oci::RegistryOperation::Pull)
            .await
            .map_err(|_| AuthError::LoginRejected {
                registry: registry.to_string(),
            })?;
        Ok(())
    }
}

fn is_insecure(registry: &str) -> bool {
    // Heuristic: respect OCX_INSECURE_REGISTRIES env var.
    // Note: `ocx login --insecure` is currently hidden / no-op in v1.
    // Callers that need HTTP must export `OCX_INSECURE_REGISTRIES` directly.
    // When the future `--verify` flag wires real registry Ping, the flag will
    // propagate through `apply_ocx_config` to this env var.
    // `crate::env::var` is used instead of `std::env::var` for test-injectability
    // via the env-lock mechanism used across this crate's unit tests.
    if let Some(list) = crate::env::var("OCX_INSECURE_REGISTRIES") {
        for entry in list.split(',') {
            let entry = entry.trim();
            if !entry.is_empty() && entry == registry {
                return true;
            }
        }
    }
    false
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
/// 4. On Ping failure, return `AuthError::LoginRejected { registry }` WITHOUT calling `put`.
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
}
