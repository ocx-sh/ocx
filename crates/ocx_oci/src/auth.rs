// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_util::prelude::*;

mod auth_type;
pub mod error;
pub mod login;
pub mod registry_url;
pub mod store;

pub use auth_type::AuthType;
pub use error::AuthError;
pub use registry_url::canonicalize_registry;
pub use store::{Credential, CredentialStore, DockerCredentialStore, StoreOptions};

/// The result of an auth lookup, which can fail no other way.
pub type Result<T> = std::result::Result<T, AuthError>;

/// Looks up the Docker-config credential for a canonicalized registry; blocking, so it runs off the runtime.
type DockerLookup = std::sync::Arc<dyn Fn(&str) -> Result<Option<crate::native::Auth>> + Send + Sync>;

/// Credential resolution with one lookup per registry per process, whatever its outcome.
///
/// A credential helper can be slow or fail, and every request for a registry would pay for it again;
/// the outcome, a failure included, is the registry's answer for the rest of the run.
#[derive(Clone)]
pub struct Auth {
    lookups: ocx_util::singleflight::Group<String, crate::native::Auth>,
    docker: DockerLookup,
}

impl Auth {
    pub fn new() -> Self {
        Self::with_docker_lookup(std::sync::Arc::new(|registry: &str| get_docker_auth(registry)))
    }

    fn with_docker_lookup(docker: DockerLookup) -> Self {
        Auth {
            // Waiters outlive at most one helper budget, so this bound only guards a lost leader.
            lookups: ocx_util::singleflight::Group::new(1024, std::time::Duration::from_secs(120)),
            docker,
        }
    }

    /// Retrieves authentication for the specified registry, falling back to anonymous if retrieval fails.
    pub async fn get_or_fallback(&self, registry: impl AsRef<str>) -> crate::native::Auth {
        use ocx_util::singleflight::Acquisition;

        let registry = registry.as_ref();
        match self.lookups.try_acquire(registry.to_owned()).await {
            Ok(Acquisition::Resolved(auth)) => auth,
            Ok(Acquisition::Leader(handle)) => {
                let auth = self.lookup_or_anonymous(registry).await;
                handle.complete(auth.clone());
                auth
            }
            // The leader vanished or the group is full: ask directly rather than fail the request.
            Err(_) => self.lookup_or_anonymous(registry).await,
        }
    }

    async fn lookup_or_anonymous(&self, registry: &str) -> crate::native::Auth {
        match self.lookup(registry).await {
            Ok(auth) => auth,
            Err(error) => {
                log::warn!(
                    "failed to retrieve authentication for registry '{}', falling back to anonymous: {}",
                    registry,
                    error
                );
                crate::native::Auth::Anonymous
            }
        }
    }

    async fn lookup(&self, registry: &str) -> Result<crate::native::Auth> {
        if let Some(auth) = get_env_auth(registry)? {
            return Ok(auth);
        }
        let docker = self.docker.clone();
        let canonical = registry.to_owned();
        let found = tokio::task::spawn_blocking(move || docker(&canonical))
            .await
            .unwrap_or_else(|error| {
                log::warn!("Docker credential lookup for registry '{registry}' aborted: {error}");
                Ok(None)
            })?;
        Ok(found.unwrap_or(crate::native::Auth::Anonymous))
    }
}

impl Default for Auth {
    fn default() -> Self {
        Self::new()
    }
}

/// Retrieves Docker credentials for the registry via the native credential helper.
///
/// Canonicalizes like `DockerCredentialStore` does on write, or a stored login is never found.
fn get_docker_auth(registry: impl AsRef<str>) -> Result<Option<crate::native::Auth>> {
    use docker_credential::CredentialRetrievalError as DockerError;
    use docker_credential::DockerCredential;

    let registry = canonicalize_registry(registry.as_ref());
    let registry = registry.as_str();
    let auth = match crate::native::get_docker_credential(registry) {
        Ok(DockerCredential::IdentityToken(token)) => Some(crate::native::Auth::Bearer(token)),
        Ok(DockerCredential::UsernamePassword(username, password)) => {
            Some(crate::native::Auth::Basic(username, password))
        }
        Err(DockerError::NoCredentialConfigured)
        | Err(DockerError::ConfigNotFound)
        | Err(DockerError::ConfigReadError)
        | Err(DockerError::NotFound) => {
            log::debug!("No native Docker credentials found for registry '{}'", registry);
            None
        }
        // stdout can carry the secret; only the helper's own complaint is shown.
        Err(DockerError::HelperFailure { helper, stderr, .. }) => {
            let reason = stderr.lines().next().unwrap_or_default();
            log::warn!("credential helper '{helper}' failed for registry '{registry}': {reason}");
            None
        }
        Err(error) => {
            log::warn!("failed to retrieve Docker credentials for registry '{}'", registry);
            return Err(AuthError::DockerCredentialRetrieval(error));
        }
    };
    Ok(auth)
}

fn get_env_auth(registry: impl AsRef<str>) -> Result<Option<crate::native::Auth>> {
    let registry_slug = registry.to_slug();

    let name = |var: &ocx_env::EnvVar| var.name.replace("{REGISTRY}", &registry_slug);
    let token_env = name(ocx_env::OCX_AUTH_TOKEN.declaration());
    let user_env = name(&ocx_env::OCX_AUTH_USER);

    let auth_type = match ocx_env::OCX_AUTH_TYPE.get_slot(&registry_slug) {
        Some(auth_type) => Some(AuthType::try_from(auth_type)?),
        None => None,
    };
    let auth_user = ocx_env::OCX_AUTH_USER.get_slot(&registry_slug);
    let auth_token = ocx_env::OCX_AUTH_TOKEN
        .get_slot(&registry_slug)
        .map(ocx_env::Sensitive::into_inner);

    match auth_type {
        Some(auth_type) => {
            let auth = match auth_type {
                AuthType::Anonymous => crate::native::Auth::Anonymous,
                AuthType::Basic => {
                    let user = auth_user.ok_or_else(|| AuthError::MissingEnv(auth_type, user_env.clone()))?;
                    let token = auth_token.ok_or_else(|| AuthError::MissingEnv(auth_type, token_env.clone()))?;
                    crate::native::Auth::Basic(user, token)
                }
                AuthType::Token => {
                    let token = auth_token.ok_or_else(|| AuthError::MissingEnv(auth_type, token_env.clone()))?;
                    crate::native::Auth::Bearer(token)
                }
            };
            Ok(Some(auth))
        }
        None => match (auth_user, auth_token) {
            (Some(user), Some(token)) => Ok(Some(crate::native::Auth::Basic(user, token))),
            (None, Some(token)) => Ok(Some(crate::native::Auth::Bearer(token))),
            _ => Ok(None),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An `Auth` whose Docker lookup counts its calls and answers `outcome`.
    fn counting_auth(outcome: fn() -> Result<Option<crate::native::Auth>>) -> (Auth, std::sync::Arc<AtomicUsize>) {
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let auth = Auth::with_docker_lookup(std::sync::Arc::new(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(50));
            outcome()
        }));
        (auth, calls)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_gets_ask_the_helper_once() {
        let _env = ocx_env::overrides::lock();
        let (auth, calls) = counting_auth(|| Ok(Some(crate::native::Auth::Bearer("t".into()))));
        let results = futures::future::join_all((0..8).map(|_| auth.get_or_fallback("reg.example"))).await;
        assert!(
            results
                .iter()
                .all(|a| matches!(a, crate::native::Auth::Bearer(t) if t == "t"))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_lookup_is_not_retried() {
        let _env = ocx_env::overrides::lock();
        let (auth, calls) = counting_auth(|| {
            Err(AuthError::DockerCredentialRetrieval(
                docker_credential::CredentialRetrievalError::HelperCommunicationError,
            ))
        });
        for _ in 0..3 {
            assert!(matches!(
                auth.get_or_fallback("reg.example").await,
                crate::native::Auth::Anonymous
            ));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_ocx_auth_override_never_asks_the_helper() {
        let env = ocx_env::overrides::lock();
        env.set_raw("OCX_AUTH_reg_example_TYPE", "anonymous");
        let (auth, calls) = counting_auth(|| Ok(Some(crate::native::Auth::Bearer("t".into()))));
        assert!(matches!(
            auth.get_or_fallback("reg.example").await,
            crate::native::Auth::Anonymous
        ));
        env.set_raw("OCX_AUTH_other_example_TOKEN", "tok");
        assert!(matches!(auth.get_or_fallback("other.example").await, crate::native::Auth::Bearer(t) if t == "tok"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_get_docker_auth_no_credentials() {
        let _env = ocx_env::overrides::lock();
        let auth = get_docker_auth("nonexistent.registry").unwrap();
        assert!(auth.is_none());
    }

    #[test]
    fn test_get_env_auth_basic() {
        let env = ocx_env::overrides::lock();
        env.set_raw("OCX_AUTH_test_registry_TYPE", "basic");
        env.set_raw("OCX_AUTH_test_registry_USER", "TEST_USER");
        env.set_raw("OCX_AUTH_test_registry_TOKEN", "TEST_TOKEN");

        let auth = get_env_auth("test.registry").unwrap();
        assert!(
            matches!(auth, Some(crate::native::Auth::Basic(user, token)) if user == "TEST_USER" && token == "TEST_TOKEN")
        );

        env.remove_raw("OCX_AUTH_test_registry_TYPE");

        let auth = get_env_auth("test.registry").unwrap();
        assert!(
            matches!(auth, Some(crate::native::Auth::Basic(user, token)) if user == "TEST_USER" && token == "TEST_TOKEN")
        );
    }

    #[test]
    fn test_get_env_auth_token() {
        let env = ocx_env::overrides::lock();
        env.set_raw("OCX_AUTH_test_registry_TYPE", "token");
        env.set_raw("OCX_AUTH_test_registry_TOKEN", "TEST_TOKEN");

        let auth = get_env_auth("test.registry").unwrap();
        assert!(matches!(auth, Some(crate::native::Auth::Bearer(token)) if token == "TEST_TOKEN"));

        env.remove_raw("OCX_AUTH_test_registry_TYPE");

        let auth = get_env_auth("test.registry").unwrap();
        assert!(matches!(auth, Some(crate::native::Auth::Bearer(token)) if token == "TEST_TOKEN"));
    }

    #[test]
    fn test_get_env_auth_empty_value_reads_as_unset() {
        let env = ocx_env::overrides::lock();
        env.set_raw("OCX_AUTH_test_registry_TYPE", "");
        env.set_raw("OCX_AUTH_test_registry_USER", "TEST_USER");
        env.set_raw("OCX_AUTH_test_registry_TOKEN", "TEST_TOKEN");
        let auth = get_env_auth("test.registry").unwrap();
        assert!(
            matches!(auth, Some(crate::native::Auth::Basic(user, token)) if user == "TEST_USER" && token == "TEST_TOKEN")
        );

        env.remove_raw("OCX_AUTH_test_registry_TYPE");
        env.set_raw("OCX_AUTH_test_registry_USER", "");
        let auth = get_env_auth("test.registry").unwrap();
        assert!(matches!(auth, Some(crate::native::Auth::Bearer(token)) if token == "TEST_TOKEN"));

        env.remove_raw("OCX_AUTH_test_registry_USER");
        env.set_raw("OCX_AUTH_test_registry_TOKEN", "");
        assert!(get_env_auth("test.registry").unwrap().is_none());
    }
}
