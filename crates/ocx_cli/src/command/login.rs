// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::{self, Read as _};
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser;
use ocx_lib::auth::login::{OciClientPing, RegistryPing, login};
use ocx_lib::auth::store::{Credential, DockerCredentialStore, StoreOptions};
use ocx_lib::cli::error::UsageError;
use secrecy::SecretString;

use crate::api::data::login::LoginResult;
use crate::options;

/// Authenticate to a registry and persist the credentials via a docker-compatible store.
///
/// Prompts for username on a TTY when `--username` is omitted; password is read via
/// `--password-stdin` (no `-p/--password` flag exists by design — argv-visible secrets
/// leak through `ps`/shell history and are refused at parse).
#[derive(Parser, Clone, Debug)]
pub struct Login {
    /// Username for the registry. Prompted when omitted on a TTY.
    #[clap(short = 'u', long = "username")]
    username: Option<String>,

    /// Read password/token from stdin. Required in non-interactive contexts.
    #[clap(long = "password-stdin")]
    password_stdin: bool,

    /// Allow plain HTTP for the registry (overrides default HTTPS).
    /// Reserved for the future `--verify` Ping path - currently no-op in v1.
    #[clap(long = "insecure", hide = true)]
    #[allow(dead_code)]
    insecure: bool,

    /// Allow plaintext fallback to `~/.docker/config.json` `auths` when no native helper
    /// is configured. Default: refuse, exit `ConfigError(78)` with install hint.
    #[clap(long = "allow-insecure-store")]
    allow_insecure_store: bool,

    /// Reserved for v2 OIDC / browser-OAuth flows. Hidden in `--help`.
    #[clap(long = "auth-type", hide = true)]
    #[allow(dead_code)]
    auth_type: Option<String>,

    /// Verify the credentials against the registry (`GET /v2/`) before storing
    /// them (default). `--no-verify` stores without a round-trip.
    #[clap(flatten)]
    verify: options::Verify,

    /// Registry hostname (e.g. `ghcr.io`, `registry.example.com`).
    /// Optional - falls back to `OCX_DEFAULT_REGISTRY`.
    registry: Option<String>,
}

// NOTE: `--format plain|json` is the project-wide global flag parsed at the `Cli`
// root level (see `crates/ocx_cli/src/cli.rs`). Do NOT declare it on Login / Logout
// structs — clap inherits it from root; `context.api().report(&result)` honors it.

impl Login {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // 1. Resolve registry.
        let registry = self
            .registry
            .clone()
            .unwrap_or_else(|| context.default_registry().to_string());

        // Announce target before any prompt — user must know which registry
        // they're typing credentials for, especially when REGISTRY was
        // omitted and the default kicked in. `UserInterface::status` honors
        // --quiet and color settings; --format json still emits the human
        // line on stderr (stdout stays clean for the structured LoginResult).
        let ui = context.ui();
        ui.status("Logging in", &registry);
        ui.status_break();

        // 2. Read credentials. Validation happens before any registry I/O.
        //
        // `ui.prompt_line` returns Err(Unsupported) when non-interactive,
        // which we map to the same UsageError that the old TTY-check produced.
        // `ui.prompt_secret` does the same for the password path.
        let username = match &self.username {
            Some(u) => u.clone(),
            None => ui.prompt_line("Username: ").map_err(|_| {
                UsageError::new("non-interactive login requires --username (-u) when stdin is not a TTY")
            })?,
        };

        let password_secret = if self.password_stdin {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .context("failed to read password from stdin")?;
            if let Some(stripped) = buf.strip_suffix('\n') {
                buf = stripped.to_string();
            }
            if buf.is_empty() {
                return Err(UsageError::new("--password-stdin received empty input").into());
            }
            SecretString::from(buf)
        } else {
            // Fail early with the actionable hint when stdin/stderr is not a
            // TTY: an interactive password prompt can only error here, and the
            // bare prompt error ("failed to read password") buries the fix.
            if !ui.is_interactive() {
                return Err(
                    UsageError::new("non-interactive login requires --password-stdin (stdin is not a TTY)").into(),
                );
            }
            let pwd = ui.prompt_secret("Password: ").map_err(|e| match e.kind() {
                io::ErrorKind::Unsupported => anyhow::Error::from(UsageError::new(
                    "non-interactive login requires --password-stdin (stdin is not a TTY)",
                )),
                _ => anyhow::Error::from(e).context("failed to read password; pass it via --password-stdin instead"),
            })?;
            if pwd.is_empty() {
                return Err(UsageError::new("password is required").into());
            }
            SecretString::from(pwd)
        };

        // 3. Pre-flag warning for plaintext route. Emitting the warning early
        // means it lands on stderr regardless of where the put eventually
        // routes; the helper-route branch is silent.
        let needs_plaintext_warning = self.allow_insecure_store && !has_helper_configured(&registry).await;
        if needs_plaintext_warning {
            ui.warn("storing credentials as plaintext base64 in ~/.docker/config.json (no native helper)");
        }

        // 4. Build credential + store + run login.
        let cred = Credential::basic(username.clone(), password_secret);
        // Disable native-helper detection: the acceptance contract is that
        // `ocx login` is deterministic — either a helper is configured in
        // `~/.docker/config.json` (`credsStore` / `credHelpers[reg]`) or the
        // user opts into plaintext via `--allow-insecure-store`. Auto-
        // detection (oras-go `DetectDefaultNativeStore`) silently writes
        // `credsStore` on first run, which surprises users on shared dev
        // machines where `pass`/`secretservice` happen to be installed.
        let store = DockerCredentialStore::new(StoreOptions {
            allow_plaintext_put: self.allow_insecure_store,
            detect_default_native_store: false,
        })?;

        // Verify credentials against the registry before storing them (default).
        // `--no-verify` swaps in the no-op Ping to store without a round-trip.
        // Either way the security invariant ("Ping failure ⇒ no put") holds,
        // proven by `MockPing` unit tests in `auth/login.rs`.
        let ping: &dyn RegistryPing = if self.verify.enabled() {
            &OciClientPing
        } else {
            &NoopPing
        };
        login(&registry, &cred, &store, ping).await?;

        // 5. Blank line separates prompts from the success line for legibility,
        // then structured report (registry name reported in its user-supplied
        // form, not the canonicalized form, so users see the same string they
        // typed).
        ui.status_break();
        ui.success("Login succeeded");
        // Plain mode: nothing on stdout (success already on stderr). JSON
        // mode: structured payload on stdout — the machine interface.
        let result = LoginResult { registry, username };
        context.api().report(&result)?;
        Ok(ExitCode::SUCCESS)
    }
}

/// No-op `RegistryPing` used on the `--no-verify` path. Always returns `Ok(())`
/// so `login()` proceeds straight to `store.put` without a registry round-trip.
/// The default (`--verify`) path uses `OciClientPing` instead; the security
/// invariant ("Ping failure → no store") is proven by `MockPing` tests in
/// `auth/login.rs`.
struct NoopPing;

#[async_trait::async_trait]
impl RegistryPing for NoopPing {
    async fn ping(&self, _registry: &str, _cred: &Credential) -> Result<(), ocx_lib::auth::AuthError> {
        Ok(())
    }
}

/// Probe whether a helper is configured for `registry` in the docker config
/// file currently pointed at by `DockerCredentialStore::new`. Used only to
/// gate the plaintext-warning message — never to alter routing logic itself.
async fn has_helper_configured(registry: &str) -> bool {
    // Cheap probe: load the config once via the same constructor path the
    // store uses, then inspect helper-config keys. Failure to read = "no
    // helper configured" so the warning fires (safe default).
    let store = match DockerCredentialStore::new(StoreOptions::default()) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let path = store.config_path().to_path_buf();
    let canonical = ocx_lib::auth::canonicalize_registry(registry);
    tokio::task::spawn_blocking(move || {
        let Ok(bytes) = std::fs::read(&path) else {
            return false;
        };
        if bytes.is_empty() {
            return false;
        }
        let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return false;
        };
        if json
            .pointer(&format!("/credHelpers/{}", json_pointer_escape(&canonical)))
            .is_some()
        {
            return true;
        }
        json.get("credsStore").and_then(|v| v.as_str()).is_some()
    })
    .await
    .unwrap_or(false)
}

/// Escape `/` and `~` per RFC 6901 so JSON-pointer lookups handle registry
/// keys that already contain a forward slash.
fn json_pointer_escape(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn login_clap_rejects_password_value_flag() {
        // CWE-214: argv-visible secrets refused at parse. There is intentionally
        // no `--password VALUE` flag — only `--password-stdin`.
        let result = Login::try_parse_from(["login", "--password", "x", "ghcr.io"]);
        assert!(
            result.is_err(),
            "--password VALUE must be rejected at parse; clap parse should fail",
        );
    }

    #[test]
    fn login_clap_accepts_minimal_invocation() {
        Login::try_parse_from(["login", "ghcr.io"]).expect("bare positional must parse");
    }

    #[test]
    fn login_clap_accepts_full_invocation() {
        Login::try_parse_from([
            "login",
            "-u",
            "user",
            "--password-stdin",
            "--insecure",
            "--allow-insecure-store",
            "--no-verify",
            "ghcr.io",
        ])
        .expect("full flag set must parse");
    }

    #[test]
    fn login_clap_verify_defaults_on() {
        let login = Login::try_parse_from(["login", "ghcr.io"]).expect("parse");
        assert!(login.verify.enabled(), "verification must default on");
    }

    #[test]
    fn login_clap_no_verify_disables() {
        let login = Login::try_parse_from(["login", "--no-verify", "ghcr.io"]).expect("parse");
        assert!(!login.verify.enabled(), "--no-verify must disable verification");
    }

    #[test]
    fn login_clap_registry_is_optional() {
        // Falls back to OCX_DEFAULT_REGISTRY at runtime; clap parse succeeds.
        Login::try_parse_from(["login"]).expect("registry must be optional at parse");
    }

    #[test]
    fn login_clap_flags_after_positional() {
        // clap accepts flags after a positional argument by default. Confirm the
        // contract holds so CI invocations like `ocx login ghcr.io -u user` work.
        let result = Login::try_parse_from(["login", "ghcr.io", "-u", "user", "--password-stdin"]);
        assert!(result.is_ok(), "flags after positional must parse: {result:?}",);
    }

    #[test]
    fn login_clap_auth_type_is_hidden_in_help() {
        let cmd = Login::command();
        let auth_type_arg = cmd
            .get_arguments()
            .find(|a| a.get_id() == "auth_type")
            .expect("auth_type arg present");
        assert!(
            auth_type_arg.is_hide_set(),
            "--auth-type must be hidden in --help (reserved for v2 OIDC)",
        );
    }
}
