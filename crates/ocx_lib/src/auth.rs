use crate::{env, log, oci, prelude::*};

mod auth_type;
pub use auth_type::AuthType;

#[derive(Clone)]
pub struct Auth {
    cache: std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, oci::native::Auth>>>,
}

impl Auth {
    pub fn new() -> Self {
        Auth {
            cache: std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    pub async fn get(&self, registry: impl AsRef<str>) -> Result<oci::native::Auth> {
        let registry = registry.as_ref();
        if let Some(auth) = self.get_cached_auth(registry).await {
            return Ok(auth);
        }
        let auth = self.get_impl(registry).await?;
        self.set_cached_auth(registry, auth.clone()).await;
        Ok(auth)
    }

    async fn get_impl(&self, registry: impl AsRef<str>) -> Result<oci::native::Auth> {
        let registry = registry.as_ref();
        if let Some(auth) = get_env_auth(registry)? {
            return Ok(auth);
        }
        if let Some(auth) = get_docker_auth(registry)? {
            return Ok(auth);
        }
        Ok(oci::native::Auth::Anonymous)
    }

    /// Retrieves authentication for the specified registry, falling back to anonymous if retrieval fails.
    pub async fn get_or_fallback(&self, registry: impl AsRef<str>) -> oci::native::Auth {
        let registry = registry.as_ref();
        match self.get(registry).await {
            Ok(auth) => auth,
            Err(error) => {
                log::warn!(
                    "Failed to retrieve authentication for registry '{}', falling back to anonymous: {}",
                    registry,
                    error
                );
                oci::native::Auth::Anonymous
            }
        }
    }

    async fn get_cached_auth(&self, registry: impl AsRef<str>) -> Option<oci::native::Auth> {
        let registry = registry.as_ref();
        let cache = self.cache.read().await;
        cache.get(registry).cloned()
    }

    async fn set_cached_auth(&self, registry: impl AsRef<str>, auth: oci::native::Auth) {
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
fn get_docker_auth(registry: impl AsRef<str>) -> Result<Option<oci::native::Auth>> {
    use docker_credential::CredentialRetrievalError as DockerError;
    use docker_credential::DockerCredential;

    let registry = registry.as_ref();
    let auth = match oci::native::get_docker_credential(registry) {
        Ok(DockerCredential::IdentityToken(token)) => Some(oci::native::Auth::Bearer(token)),
        Ok(DockerCredential::UsernamePassword(username, password)) => {
            Some(oci::native::Auth::Basic(username, password))
        }
        Err(DockerError::NoCredentialConfigured)
        | Err(DockerError::ConfigNotFound)
        | Err(DockerError::ConfigReadError)
        | Err(DockerError::HelperFailure {
            helper: _,
            stdout: _,
            stderr: _,
        }) => {
            log::debug!("No native Docker credentials found for registry '{}'", registry);
            None
        }
        Err(error) => {
            log::warn!("Failed to retrieve Docker credentials for registry '{}'", registry);
            return Err(Error::AuthDockerCredentialRetrieval(error));
        }
    };
    Ok(auth)
}

fn get_env_auth(registry: impl AsRef<str>) -> Result<Option<oci::native::Auth>> {
    let registry_slug = registry.to_slug();

    let type_env = format!("OCX_AUTH_{}_TYPE", registry_slug);
    let token_env = format!("OCX_AUTH_{}_TOKEN", registry_slug);
    let user_env = format!("OCX_AUTH_{}_USER", registry_slug);

    let auth_type = match env::var(&type_env) {
        Some(auth_type) => Some(AuthType::try_from(auth_type)?),
        None => None,
    };
    let auth_user = env::var(&user_env);
    let auth_token = env::var(&token_env);

    match auth_type {
        Some(auth_type) => {
            let auth = match auth_type {
                AuthType::Anonymous => oci::native::Auth::Anonymous,
                AuthType::Basic => {
                    let user = auth_user.ok_or_else(|| Error::AuthMissingEnv(auth_type, user_env.clone()))?;
                    let token = auth_token.ok_or_else(|| Error::AuthMissingEnv(auth_type, token_env.clone()))?;
                    oci::native::Auth::Basic(user, token)
                }
                AuthType::Token => {
                    let token = auth_token.ok_or_else(|| Error::AuthMissingEnv(auth_type, token_env.clone()))?;
                    oci::native::Auth::Bearer(token)
                }
            };
            Ok(Some(auth))
        }
        None => match (auth_user, auth_token) {
            (Some(user), Some(token)) => Ok(Some(oci::native::Auth::Basic(user, token))),
            (None, Some(token)) => Ok(Some(oci::native::Auth::Bearer(token))),
            _ => Ok(None),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test;

    #[test]
    fn test_get_docker_auth_no_credentials() {
        let _env = test::env::lock();
        let auth = get_docker_auth("nonexistent.registry").unwrap();
        assert!(auth.is_none());
    }

    #[test]
    fn test_get_env_auth_basic() {
        let env = test::env::lock();
        env.set("OCX_AUTH_test_registry_TYPE", "basic");
        env.set("OCX_AUTH_test_registry_USER", "TEST_USER");
        env.set("OCX_AUTH_test_registry_TOKEN", "TEST_TOKEN");

        let auth = get_env_auth("test.registry").unwrap();
        assert!(
            matches!(auth, Some(oci::native::Auth::Basic(user, token)) if user == "TEST_USER" && token == "TEST_TOKEN")
        );

        env.remove("OCX_AUTH_test_registry_TYPE");

        let auth = get_env_auth("test.registry").unwrap();
        assert!(
            matches!(auth, Some(oci::native::Auth::Basic(user, token)) if user == "TEST_USER" && token == "TEST_TOKEN")
        );
    }

    #[test]
    fn test_get_env_auth_token() {
        let env = test::env::lock();
        env.set("OCX_AUTH_test_registry_TYPE", "token");
        env.set("OCX_AUTH_test_registry_TOKEN", "TEST_TOKEN");

        let auth = get_env_auth("test.registry").unwrap();
        assert!(matches!(auth, Some(oci::native::Auth::Bearer(token)) if token == "TEST_TOKEN"));

        env.remove("OCX_AUTH_test_registry_TYPE");

        let auth = get_env_auth("test.registry").unwrap();
        assert!(matches!(auth, Some(oci::native::Auth::Bearer(token)) if token == "TEST_TOKEN"));
    }
}
