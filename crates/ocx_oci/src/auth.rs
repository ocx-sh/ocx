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

/// Reads the `OCX_AUTH_*` credential for a canonicalized registry; blocking, so it runs off the runtime.
type EnvLookup = std::sync::Arc<dyn Fn(&str) -> Result<Option<crate::native::Auth>> + Send + Sync>;

/// Credential resolution with one lookup per registry per process, whatever its outcome.
///
/// A credential helper can be slow or fail, and every request for a registry would pay for it again;
/// the outcome, a failure included, is the registry's answer for the rest of the run.
#[derive(Clone)]
pub struct Auth {
    lookups: ocx_util::singleflight::Group<String, std::result::Result<crate::native::Auth, ocx_env::ReferenceError>>,
    env: EnvLookup,
    docker: DockerLookup,
}

impl Auth {
    pub fn new() -> Self {
        Self::with_lookups(
            std::sync::Arc::new(|registry: &str| get_env_auth(registry)),
            std::sync::Arc::new(|registry: &str| get_docker_auth(registry)),
        )
    }

    #[cfg(test)]
    fn with_docker_lookup(docker: DockerLookup) -> Self {
        Self::with_lookups(std::sync::Arc::new(|registry: &str| get_env_auth(registry)), docker)
    }

    fn with_lookups(env: EnvLookup, docker: DockerLookup) -> Self {
        Auth {
            // Waiters outlive at most one helper budget, so this bound only guards a lost leader.
            lookups: ocx_util::singleflight::Group::new(1024, std::time::Duration::from_secs(120)),
            env,
            docker,
        }
    }

    /// Retrieves authentication for the specified registry, falling back to anonymous if retrieval fails.
    ///
    /// # Errors
    ///
    /// [`AuthError`] when an `OCX_AUTH_*` value is a reference that cannot be resolved, or when the
    /// `OCX_AUTH_*` variables for the registry lack one their type needs, such as a `USER` with no `TOKEN`.
    pub async fn get_or_fallback(&self, registry: impl AsRef<str>) -> Result<crate::native::Auth> {
        use ocx_util::singleflight::Acquisition;

        let registry = registry.as_ref();
        match self.lookups.try_acquire(registry.to_owned()).await {
            Ok(Acquisition::Resolved(outcome)) => Ok(outcome?),
            Ok(Acquisition::Leader(handle)) => {
                let outcome = self.lookup_or_anonymous(registry).await?;
                handle.complete(outcome.clone());
                Ok(outcome?)
            }
            // The leader vanished or the group is full: ask directly rather than fail the request.
            Err(_) => Ok(self.lookup_or_anonymous(registry).await??),
        }
    }

    /// Answers anonymous for every failure except the three that must stop the command: an unresolvable
    /// reference, an `OCX_AUTH_*` credential missing a variable it needs, or a cancelled variable read.
    /// Any of those answered anonymous would pull without the credential the user asked for.
    ///
    /// The inner `Err` is the registry's cached answer; the outer one is a failure no waiter may inherit.
    async fn lookup_or_anonymous(
        &self,
        registry: &str,
    ) -> Result<std::result::Result<crate::native::Auth, ocx_env::ReferenceError>> {
        match self.lookup(registry).await {
            Ok(auth) => Ok(Ok(auth)),
            Err(AuthError::Reference(error)) => Ok(Err(error)),
            Err(
                error @ (AuthError::LookupAborted(_) | AuthError::MissingEnv(..) | AuthError::UserWithoutToken { .. }),
            ) => Err(error),
            Err(error) => {
                log::warn!(
                    "failed to retrieve authentication for registry '{}', falling back to anonymous: {}",
                    registry,
                    error
                );
                Ok(Ok(crate::native::Auth::Anonymous))
            }
        }
    }

    async fn lookup(&self, registry: &str) -> Result<crate::native::Auth> {
        let env = self.env.clone();
        let owned = registry.to_owned();
        match tokio::task::spawn_blocking(move || env(&owned)).await {
            Ok(found) => {
                if let Some(auth) = found? {
                    return Ok(auth);
                }
            }
            // A panic must not read as "no variables set"; it surfaces on the caller's task.
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => return Err(AuthError::LookupAborted(error)),
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
    let user_env = name(ocx_env::OCX_AUTH_USER.declaration());

    // Resolved before `TYPE` is parsed: an unresolvable reference must win over an invalid type, because
    // the latter ends as an anonymous pull and would hide that the credential could not be read.
    let auth_user = ocx_env::OCX_AUTH_USER.get_slot(&registry_slug)?;
    let auth_token = ocx_env::OCX_AUTH_TOKEN
        .get_slot(&registry_slug)?
        .map(ocx_env::Sensitive::into_inner);
    let auth_type = match ocx_env::OCX_AUTH_TYPE.get_slot(&registry_slug) {
        Some(auth_type) => Some(AuthType::try_from(auth_type)?),
        None => None,
    };

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
            // `Ok(None)` here hands the registry to the Docker credential helper the user meant to bypass.
            (Some(_), None) => Err(AuthError::UserWithoutToken {
                user: user_env,
                token: token_env,
                auth_type: name(ocx_env::OCX_AUTH_TYPE.declaration()),
            }),
            (None, None) => Ok(None),
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
                .all(|a| matches!(a, Ok(crate::native::Auth::Bearer(t)) if t == "t"))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    fn unreadable_token() -> AuthError {
        AuthError::Reference(ocx_env::ReferenceError::FileUnreadable {
            variable: "OCX_AUTH_reg_example_TOKEN".into(),
            path: "/missing".into(),
            source: std::sync::Arc::new(std::io::Error::from(std::io::ErrorKind::NotFound)),
        })
    }

    /// An `Auth` whose variable read counts its calls and fails with a reference error.
    fn failing_env_auth() -> (Auth, std::sync::Arc<AtomicUsize>) {
        let reads = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = reads.clone();
        let auth = Auth::with_lookups(
            std::sync::Arc::new(move |_| {
                seen.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(50));
                Err(unreadable_token())
            }),
            std::sync::Arc::new(|_| Ok(None)),
        );
        (auth, reads)
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
                Ok(crate::native::Auth::Anonymous)
            ));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_reference_error_is_not_retried_and_never_anonymous() {
        let _env = ocx_env::overrides::lock();
        let (auth, reads) = failing_env_auth();
        for _ in 0..3 {
            assert!(matches!(
                auth.get_or_fallback("reg.example").await,
                Err(AuthError::Reference(ocx_env::ReferenceError::FileUnreadable { .. }))
            ));
        }
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_waiters_all_get_the_reference_error_from_one_read() {
        let _env = ocx_env::overrides::lock();
        let (auth, reads) = failing_env_auth();
        let (first, second) = tokio::join!(auth.get_or_fallback("reg.example"), auth.get_or_fallback("reg.example"));
        for outcome in [first, second] {
            assert!(
                matches!(
                    outcome,
                    Err(AuthError::Reference(ocx_env::ReferenceError::FileUnreadable { .. }))
                ),
                "expected a reference error, got {outcome:?}"
            );
        }
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_ocx_auth_override_never_asks_the_helper() {
        let env = ocx_env::overrides::lock();
        env.set_raw("OCX_AUTH_reg_example_TYPE", "anonymous");
        let (auth, calls) = counting_auth(|| Ok(Some(crate::native::Auth::Bearer("t".into()))));
        assert!(matches!(
            auth.get_or_fallback("reg.example").await,
            Ok(crate::native::Auth::Anonymous)
        ));
        env.set_raw("OCX_AUTH_other_example_TOKEN", "tok");
        assert!(
            matches!(auth.get_or_fallback("other.example").await, Ok(crate::native::Auth::Bearer(t)) if t == "tok")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_lone_user_is_an_error_naming_the_token_and_never_asks_the_helper() {
        let env = ocx_env::overrides::lock();
        env.set_raw("OCX_AUTH_reg_example_USER", "lone-user-value");
        let (auth, calls) = counting_auth(|| Ok(Some(crate::native::Auth::Bearer("t".into()))));
        let outcome = auth.get_or_fallback("reg.example").await;
        let Err(ref error @ AuthError::UserWithoutToken { .. }) = outcome else {
            panic!("expected a user without token error, got {outcome:?}");
        };
        assert_eq!(
            error.to_string(),
            "'OCX_AUTH_reg_example_USER' is set but 'OCX_AUTH_reg_example_TOKEN' is not; \
             set the token, or set 'OCX_AUTH_reg_example_TYPE=anonymous' to ask for no credentials"
        );
        assert!(!error.to_string().contains("lone-user-value"), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_typed_credential_missing_its_token_is_an_error_not_anonymous() {
        let env = ocx_env::overrides::lock();
        env.set_raw("OCX_AUTH_reg_example_TYPE", "basic");
        env.set_raw("OCX_AUTH_reg_example_USER", "user");
        env.set_raw("OCX_AUTH_other_example_TYPE", "token");
        let (auth, calls) = counting_auth(|| Ok(Some(crate::native::Auth::Bearer("t".into()))));
        for (registry, variable) in [
            ("reg.example", "OCX_AUTH_reg_example_TOKEN"),
            ("other.example", "OCX_AUTH_other_example_TOKEN"),
        ] {
            let outcome = auth.get_or_fallback(registry).await;
            assert!(
                matches!(&outcome, Err(AuthError::MissingEnv(_, name)) if name == variable),
                "expected a missing token error for {registry}, got {outcome:?}"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_unreadable_token_reference_fails_instead_of_pulling_anonymously() {
        let env = ocx_env::overrides::lock();
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing-token");
        env.set_raw("OCX_AUTH_reg_example_TOKEN", format!("file://{}", missing.display()));
        let (auth, calls) = counting_auth(|| Ok(None));
        let outcome = auth.get_or_fallback("reg.example").await;
        assert!(
            matches!(
                outcome,
                Err(AuthError::Reference(ocx_env::ReferenceError::FileUnreadable { .. }))
            ),
            "expected a reference error, got {outcome:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_panicking_variable_read_is_never_answered_anonymous() {
        let _env = ocx_env::overrides::lock();
        let auth = Auth::with_lookups(
            std::sync::Arc::new(|_| panic!("variable read blew up")),
            std::sync::Arc::new(|_| Ok(None)),
        );
        let task = tokio::spawn(async move { auth.get_or_fallback("reg.example").await });
        let joined = task.await;
        assert!(
            matches!(&joined, Err(error) if error.is_panic()),
            "expected the panic to surface, got {joined:?}"
        );
    }

    /// A file holding `content`, kept alive by the returned directory; its `file://` spelling is returned too.
    fn secret_file(name: &str, content: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        let reference = format!("file://{}", path.display());
        (dir, reference)
    }

    #[test]
    fn a_token_file_alone_is_a_bearer_credential() {
        let env = ocx_env::overrides::lock();
        let (_dir, reference) = secret_file("token", "file-token");
        env.set_raw("OCX_AUTH_test_registry_TOKEN", reference);
        let auth = get_env_auth("test.registry").unwrap();
        assert!(matches!(auth, Some(crate::native::Auth::Bearer(token)) if token == "file-token"));
    }

    #[test]
    fn a_user_file_and_a_token_file_make_a_basic_credential() {
        let env = ocx_env::overrides::lock();
        let (_user_dir, user) = secret_file("user", "file-user");
        let (_token_dir, token) = secret_file("token", "file-token");
        env.set_raw("OCX_AUTH_test_registry_USER", user);
        env.set_raw("OCX_AUTH_test_registry_TOKEN", token);
        let auth = get_env_auth("test.registry").unwrap();
        assert!(
            matches!(auth, Some(crate::native::Auth::Basic(user, token)) if user == "file-user" && token == "file-token")
        );
    }

    #[test]
    fn an_explicit_basic_type_reads_the_resolved_files() {
        let env = ocx_env::overrides::lock();
        let (_user_dir, user) = secret_file("user", "file-user");
        let (_token_dir, token) = secret_file("token", "file-token");
        env.set_raw("OCX_AUTH_test_registry_TYPE", "basic");
        env.set_raw("OCX_AUTH_test_registry_USER", user);
        env.set_raw("OCX_AUTH_test_registry_TOKEN", token);
        let auth = get_env_auth("test.registry").unwrap();
        assert!(
            matches!(auth, Some(crate::native::Auth::Basic(user, token)) if user == "file-user" && token == "file-token")
        );
    }

    #[test]
    fn an_env_reference_reads_the_named_variable() {
        let env = ocx_env::overrides::lock();
        env.set_raw("OCX_TEST_REGISTRY_PASSWORD", "from-env");
        env.set_raw("OCX_AUTH_test_registry_TOKEN", "env://OCX_TEST_REGISTRY_PASSWORD");
        let auth = get_env_auth("test.registry").unwrap();
        assert!(matches!(auth, Some(crate::native::Auth::Bearer(token)) if token == "from-env"));
    }

    #[test]
    fn a_trailing_newline_is_stripped_from_a_token_file() {
        let env = ocx_env::overrides::lock();
        let (_dir, reference) = secret_file("token", "file-token\n");
        env.set_raw("OCX_AUTH_test_registry_TOKEN", reference);
        let auth = get_env_auth("test.registry").unwrap();
        assert!(matches!(auth, Some(crate::native::Auth::Bearer(token)) if token == "file-token"));
    }

    #[test]
    fn an_unreadable_reference_wins_over_an_invalid_type() {
        let env = ocx_env::overrides::lock();
        let dir = tempfile::tempdir().unwrap();
        env.set_raw("OCX_AUTH_test_registry_TYPE", "bogus");
        env.set_raw(
            "OCX_AUTH_test_registry_TOKEN",
            format!("file://{}", dir.path().join("missing").display()),
        );
        let error = get_env_auth("test.registry").unwrap_err();
        assert!(
            matches!(
                error,
                AuthError::Reference(ocx_env::ReferenceError::FileUnreadable { .. })
            ),
            "expected a reference error, got {error:?}"
        );
    }

    #[test]
    fn an_invalid_type_is_still_an_invalid_type_when_the_values_resolve() {
        let env = ocx_env::overrides::lock();
        env.set_raw("OCX_AUTH_test_registry_TYPE", "bogus");
        env.set_raw("OCX_AUTH_test_registry_TOKEN", "literal");
        assert!(matches!(
            get_env_auth("test.registry").unwrap_err(),
            AuthError::InvalidType(_)
        ));
    }

    /// The `file://` rest of a secret reference and of a path reference must be the same text, or one
    /// spelling of a path would name a different file depending on which door read it.
    #[test]
    fn secret_and_path_references_agree_on_the_file_spelling() {
        use ocx_env::SecretReference;
        use ocx_util::fs::path::{FileReference, Spelling};

        for (value, rest) in [
            ("file:///abs", "/abs"),
            ("FILE:///abs", "/abs"),
            ("file://C:\\x", "C:\\x"),
            ("file://\\\\host\\share\\x", "\\\\host\\share\\x"),
            ("file://", ""),
            ("file://./weird://name", "./weird://name"),
        ] {
            let path_reference = FileReference::parse(value);
            assert_eq!(path_reference.spelling(), Spelling::FileUrl, "{value:?}");
            assert_eq!(path_reference.as_written(), std::path::Path::new(rest), "{value:?}");
            assert!(
                matches!(SecretReference::parse(value), SecretReference::File(text) if text == rest),
                "{value:?}"
            );
        }
        assert!(matches!(
            SecretReference::parse("file:x"),
            SecretReference::Literal("file:x")
        ));
        assert_eq!(FileReference::parse("file:x").spelling(), Spelling::Bare);
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
