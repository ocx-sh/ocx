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

#[derive(Clone)]
pub struct Auth {
    cache: std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, crate::native::Auth>>>,
}

impl Auth {
    pub fn new() -> Self {
        Auth {
            cache: std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    pub async fn get(&self, registry: impl AsRef<str>) -> Result<crate::native::Auth> {
        let registry = registry.as_ref();
        if let Some(auth) = self.get_cached_auth(registry).await {
            return Ok(auth);
        }
        let auth = self.get_impl(registry).await?;
        self.set_cached_auth(registry, auth.clone()).await;
        Ok(auth)
    }

    async fn get_impl(&self, registry: impl AsRef<str>) -> Result<crate::native::Auth> {
        let registry = registry.as_ref();
        if let Some(auth) = get_env_auth(registry)? {
            return Ok(auth);
        }
        if let Some(auth) = get_docker_auth(registry)? {
            return Ok(auth);
        }
        Ok(crate::native::Auth::Anonymous)
    }

    /// Retrieves authentication for the specified registry, falling back to anonymous if retrieval fails.
    pub async fn get_or_fallback(&self, registry: impl AsRef<str>) -> crate::native::Auth {
        let registry = registry.as_ref();
        match self.get(registry).await {
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

    async fn get_cached_auth(&self, registry: impl AsRef<str>) -> Option<crate::native::Auth> {
        let registry = registry.as_ref();
        let cache = self.cache.read().await;
        cache.get(registry).cloned()
    }

    async fn set_cached_auth(&self, registry: impl AsRef<str>, auth: crate::native::Auth) {
        let registry = registry.as_ref().to_string();
        let mut cache = self.cache.write().await;
        cache.insert(registry, auth);
    }
}

impl Default for Auth {
    fn default() -> Self {
        Self::new()
    }
}

/// Retrieves Docker credentials for the specified registry using the native Docker credential helper.
///
/// Canonicalizes the registry argument through `auth::registry_url::canonicalize_registry`
/// so the same key form is used for read (here) and write (`auth::store::DockerCredentialStore`).
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
        | Err(DockerError::NotFound)
        | Err(DockerError::HelperFailure {
            helper: _,
            stdout: _,
            stderr: _,
        }) => {
            log::debug!("No native Docker credentials found for registry '{}'", registry);
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

    let type_env = format!("OCX_AUTH_{}_TYPE", registry_slug);
    let token_env = format!("OCX_AUTH_{}_TOKEN", registry_slug);
    let user_env = format!("OCX_AUTH_{}_USER", registry_slug);

    let auth_type = match ocx_util::env::var(&type_env) {
        Some(auth_type) => Some(AuthType::try_from(auth_type)?),
        None => None,
    };
    let auth_user = ocx_util::env::var(&user_env);
    let auth_token = ocx_util::env::var(&token_env);

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

    #[test]
    fn test_get_docker_auth_no_credentials() {
        let _env = ocx_util::env::overrides::lock();
        let auth = get_docker_auth("nonexistent.registry").unwrap();
        assert!(auth.is_none());
    }

    #[test]
    fn test_get_env_auth_basic() {
        let env = ocx_util::env::overrides::lock();
        env.set("OCX_AUTH_test_registry_TYPE", "basic");
        env.set("OCX_AUTH_test_registry_USER", "TEST_USER");
        env.set("OCX_AUTH_test_registry_TOKEN", "TEST_TOKEN");

        let auth = get_env_auth("test.registry").unwrap();
        assert!(
            matches!(auth, Some(crate::native::Auth::Basic(user, token)) if user == "TEST_USER" && token == "TEST_TOKEN")
        );

        env.remove("OCX_AUTH_test_registry_TYPE");

        let auth = get_env_auth("test.registry").unwrap();
        assert!(
            matches!(auth, Some(crate::native::Auth::Basic(user, token)) if user == "TEST_USER" && token == "TEST_TOKEN")
        );
    }

    #[test]
    fn test_get_env_auth_token() {
        let env = ocx_util::env::overrides::lock();
        env.set("OCX_AUTH_test_registry_TYPE", "token");
        env.set("OCX_AUTH_test_registry_TOKEN", "TEST_TOKEN");

        let auth = get_env_auth("test.registry").unwrap();
        assert!(matches!(auth, Some(crate::native::Auth::Bearer(token)) if token == "TEST_TOKEN"));

        env.remove("OCX_AUTH_test_registry_TYPE");

        let auth = get_env_auth("test.registry").unwrap();
        assert!(matches!(auth, Some(crate::native::Auth::Bearer(token)) if token == "TEST_TOKEN"));
    }
}
