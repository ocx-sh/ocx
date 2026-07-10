// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package sign` — keyless Sigstore signing of a published package
//! manifest via OCI Referrers.
//!
//! Publishes a Sigstore bundle v0.3 as a referrer manifest for the target,
//! with the bundle body itself in a CAS blob. See
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! for the full pipeline.
//!
//! C-S1-4 override token handling: the CLI resolves `--identity-token-file` >
//! `--identity-token-stdin` > `OCX_IDENTITY_TOKEN` env *before* calling the
//! sign pipeline. There is deliberately NO `--identity-token <VALUE>` flag —
//! raw tokens on the command line would leak into shell history.

use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser;
use tokio::io::AsyncReadExt;
use zeroize::Zeroizing;

use ocx_lib::Error as LibError;
use ocx_lib::oci;
use ocx_lib::oci::endpoint::{DEFAULT_REKOR_URL, validate_sigstore_url};
use ocx_lib::oci::sign::{SignError, SignErrorKind};
use ocx_lib::package_manager::SignOptions;
use ocx_lib::package_manager::error::{PackageError, PackageErrorKind};

use crate::api::data::signature::SignatureReport;
use crate::options;

/// Default public Fulcio CA endpoint (overridable via `--fulcio-url`).
const DEFAULT_FULCIO_URL: &str = "https://fulcio.sigstore.dev";

#[derive(Parser, Clone)]
pub struct PackageSign {
    /// Target platform (single-platform manifest under an image index).
    #[clap(short = 'p', long = "platform", required = true, value_name = "PLATFORM")]
    platform: oci::Platform,

    // C-S1-3 injection seam: these two URL overrides point sign at a private
    // Fulcio/Rekor deployment (validated at the boundary in `execute`).
    /// Fulcio CA endpoint. Defaults to public Fulcio; override for private deployments.
    #[clap(long = "fulcio-url", value_name = "URL", default_value = DEFAULT_FULCIO_URL)]
    fulcio_url: String,

    /// Rekor transparency-log endpoint. Defaults to public Rekor; override for private deployments.
    #[clap(long = "rekor-url", value_name = "URL", default_value = DEFAULT_REKOR_URL)]
    rekor_url: String,

    /// Read the OIDC identity token from this file (highest precedence).
    ///
    /// Use this when the CI system writes the token to a file instead of the
    /// environment (GitHub Actions `$ACTIONS_ID_TOKEN_REQUEST_TOKEN` flow is
    /// env-based; other systems write the token out).
    ///
    /// Security: `--identity-token-file` does not follow symlinks. On Unix the
    /// file is opened with `O_NOFOLLOW` and rejected if not owned by the
    /// effective user or if group/other permission bits are set.
    #[clap(
        long = "identity-token-file",
        value_name = "PATH",
        conflicts_with = "identity_token_stdin"
    )]
    identity_token_file: Option<std::path::PathBuf>,

    /// Read the OIDC identity token from stdin (second precedence).
    ///
    /// Mutually exclusive with `--identity-token-file`. Accepts a newline-terminated
    /// token on stdin; trailing whitespace is trimmed.
    #[clap(long = "identity-token-stdin", conflicts_with = "identity_token_file")]
    identity_token_stdin: bool,

    /// Suppress the interactive browser OAuth fallback (CI / headless).
    ///
    /// When set, ambient detection must succeed or the override flags must
    /// supply a token; there is no interactive recovery path.
    #[clap(long = "no-tty")]
    no_tty: bool,

    /// Bypass the referrers-capability cache for this invocation.
    ///
    /// Default: the per-registry capability probe is cached in
    /// `$OCX_HOME/state/referrers/<registry>.json` to avoid repeated 404
    /// probes. `--no-cache` forces a fresh probe, useful after a registry
    /// upgrades to OCI 1.1.
    #[clap(long = "no-cache")]
    no_cache: bool,

    /// Package identifier to sign (`registry/repo:tag[@digest]`).
    identifier: options::Identifier,
}

impl PackageSign {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        // SSRF hardening (CWE-918): validate user-supplied endpoint URLs at the
        // boundary before they become HTTP client targets. Failures route
        // through `SignErrorKind::InvalidEndpointUrl` → exit 64 (UsageError),
        // so the error envelope's `error.detail` names the offending flag.
        let fulcio_url = validate_sigstore_url(&self.fulcio_url, "--fulcio-url").map_err(|reason| {
            SignError::new(
                identifier.clone(),
                SignErrorKind::InvalidEndpointUrl {
                    endpoint: "--fulcio-url".into(),
                    reason,
                },
            )
        })?;
        let rekor_url = validate_sigstore_url(&self.rekor_url, "--rekor-url").map_err(|reason| {
            SignError::new(
                identifier.clone(),
                SignErrorKind::InvalidEndpointUrl {
                    endpoint: "--rekor-url".into(),
                    reason,
                },
            )
        })?;
        // S1-E policy: offline sign is a deliberate rejection, NOT a passive
        // network-access failure. Route through `SignErrorKind::OfflineSignRefused`
        // so the exit-code classifier returns 77 (PermissionDenied). This
        // short-circuits before we touch the token-resolution path: the
        // acceptance test `test_sign_offline_refused` drives this contract.
        if context.is_offline() {
            return Err(anyhow::Error::from(SignError::new(
                identifier,
                SignErrorKind::OfflineSignRefused,
            )));
        }

        // C-S1-4 token precedence: file > stdin > env. The resolved token is
        // held under `Zeroizing`; never log, never surface in error context.
        let override_token = self.resolve_override_token(&identifier).await?;

        // Route through the PackageManager facade: it owns the pipeline assembly
        // (registry client, index, signer, token provider) and returns a
        // per-package error whose kind preserves the sign exit-code taxonomy.
        // Offline was already refused above via `OfflineSignRefused`.
        let options = SignOptions {
            fulcio_url,
            rekor_url,
            identity_token: override_token,
            no_cache: self.no_cache,
            no_tty: self.no_tty,
        };
        let result = context
            .manager()
            .sign_one(&identifier, &self.platform, options)
            .await
            .map_err(sign_error_into_anyhow)?
            .result;

        let report = SignatureReport::new(
            identifier.to_string(),
            result.subject_digest,
            result.bundle_digest,
            result.referrer_digest,
            &self.platform,
            result.certificate_identity,
            result.certificate_oidc_issuer,
        );
        context.api().report(&report)?;
        Ok(ExitCode::SUCCESS)
    }

    /// Resolve the override OIDC token per C-S1-4 precedence.
    ///
    /// Precedence: `--identity-token-file` > `--identity-token-stdin` >
    /// `OCX_IDENTITY_TOKEN`. Returns `Ok(None)` when no override source
    /// supplies a token — the dispatcher then falls through to ambient
    /// detection or the browser path.
    ///
    /// The file and stdin paths trim trailing whitespace so a trailing newline
    /// written by `echo $TOKEN > tokenfile` doesn't poison the JWT.
    ///
    /// On Unix, `--identity-token-file` does not follow symlinks: the file is
    /// opened with `O_NOFOLLOW` so a symlink at the supplied path is rejected
    /// at `open(2)` time (CWE-367 TOCTOU hardening — a pre-open swap of the
    /// path's target would otherwise win the descriptor-side `fstat` check).
    /// The post-open owner check also rejects token files not owned by the
    /// effective user (CWE-732) so an attacker-writable file cannot be passed
    /// through. Both symlink and owner rejections surface as
    /// [`SignErrorKind::OidcPreCheckFailed`] (exit 77).
    ///
    /// On Unix, the token file is also rejected if any group or other
    /// permission bit is set (`mode & 0o077 != 0`). This enforces `chmod 600`
    /// hygiene: a world- or group-readable token file is a security
    /// misconfiguration and surfaces as
    /// [`SignErrorKind::IdentityTokenFilePermissive`] (exit 77).
    async fn resolve_override_token(&self, identifier: &oci::Identifier) -> anyhow::Result<Option<Zeroizing<String>>> {
        if let Some(path) = &self.identity_token_file {
            // On Windows, ACL-based permission validation is not implemented for
            // Slice 1 (windows-acl integration is out of scope). Refuse explicitly
            // rather than silently skipping the check — a readable-by-others token
            // file is a security misconfiguration and must not be accepted silently.
            #[cfg(windows)]
            {
                return Err(anyhow::Error::from(SignError::new(
                    identifier.clone(),
                    SignErrorKind::OidcPreCheckFailed {
                        reason: "identity-token-file permission validation is not supported on Windows; \
                                 use --identity-token-stdin or OCX_IDENTITY_TOKEN instead"
                            .into(),
                    },
                )));
            }
            // C-S1-4 permission gate (Unix only): open the file once with
            // `O_NOFOLLOW` so a symlink at `path` is rejected at `open(2)`
            // time (CWE-367 — descriptor-side `fstat` is too late, a pre-open
            // swap of the path's target wins the race). Validate permissions
            // and ownership on the open handle, then read from the same
            // handle to eliminate the TOCTOU race between stat and read.
            //
            // Symlink-on-resolved-target (open()'s O_NOFOLLOW only checks the
            // final path component) is a deferred decision — current behavior
            // is "reject the leaf only"; the file the symlink resolves to is
            // unreachable because the open itself fails first.
            #[cfg(unix)]
            {
                // `--identity-token-file` is a sensitive credential location.
                // Error context strings deliberately omit the path so it does
                // not leak into stderr or the JSON envelope (CWE-209) — the
                // structured `IdentityTokenFilePermissive` variant retains the
                // `PathBuf` for callers that need it.
                //
                // `std::fs::OpenOptions::open` and `File::metadata` are
                // synchronous syscalls that block the runtime worker; run
                // them on the blocking pool via `tokio::task::spawn_blocking`
                // and only resume the async task once an owned `std::fs::File`
                // is returned. The reader side then wraps the handle with
                // `tokio::fs::File::from_std` so the actual read happens on
                // the async reactor.
                let path_owned = path.clone();
                let identifier_for_blocking = identifier.clone();
                let join_result = tokio::task::spawn_blocking(move || -> anyhow::Result<std::fs::File> {
                    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
                    let std_file = std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(&path_owned)
                        .map_err(|e| {
                            // `O_NOFOLLOW` on a symlink returns `ELOOP` on every
                            // POSIX target we support (Linux + Darwin/BSD). Map
                            // to a typed pre-check failure so the exit-code
                            // classifier returns 77; other errno values fall
                            // through to the raw I/O context (exit 1).
                            if e.raw_os_error() == Some(libc::ELOOP) {
                                anyhow::Error::from(SignError::new(
                                    identifier_for_blocking.clone(),
                                    SignErrorKind::OidcPreCheckFailed {
                                        reason: "identity-token-file is a symlink; refuse to follow (CWE-367)".into(),
                                    },
                                ))
                            } else {
                                anyhow::Error::new(e).context("failed to open --identity-token-file")
                            }
                        })?;
                    let meta = std_file.metadata().context("failed to stat --identity-token-file")?;
                    // CWE-732: reject token files not owned by the effective
                    // user. A file writable by another uid could have been
                    // swapped to malicious content even with 0600 perms (e.g.
                    // user-namespace games or wrongly-chowned tempfile).
                    // SAFETY: `geteuid` is async-signal-safe and never fails.
                    let euid = unsafe { libc::geteuid() };
                    if meta.uid() != euid {
                        return Err(anyhow::Error::from(SignError::new(
                            identifier_for_blocking.clone(),
                            SignErrorKind::OidcPreCheckFailed {
                                reason: "identity-token-file is not owned by the effective user".into(),
                            },
                        )));
                    }
                    let mode = meta.mode();
                    if mode & 0o077 != 0 {
                        return Err(anyhow::Error::from(SignError::new(
                            identifier_for_blocking,
                            SignErrorKind::IdentityTokenFilePermissive { path: path_owned, mode },
                        )));
                    }
                    Ok(std_file)
                })
                .await
                .context("token-file open task panicked")?;
                let std_file = join_result?;
                let mut file = tokio::fs::File::from_std(std_file);
                // Zeroizing wraps the read buffer so the full-token cleartext is
                // scrubbed on drop, not just the trimmed copy returned below.
                let mut raw = Zeroizing::new(String::new());
                file.read_to_string(&mut raw)
                    .await
                    .context("failed to read --identity-token-file")?;
                return Ok(Some(Zeroizing::new(raw.trim().to_string())));
            }
            // Unreachable on non-unix, non-windows platforms (none currently supported).
            #[allow(unreachable_code)]
            return Ok(None);
        }
        if self.identity_token_stdin {
            // Use tokio's async stdin to avoid blocking the runtime thread.
            // Zeroizing scrubs the full-token cleartext on drop, not just the
            // trimmed copy returned below.
            let mut buf = Zeroizing::new(String::new());
            tokio::io::stdin()
                .read_to_string(&mut buf)
                .await
                .context("failed to read identity token from stdin")?;
            return Ok(Some(Zeroizing::new(buf.trim().to_string())));
        }
        // Credential exemption: not forwarded via OcxConfigView. See subsystem-cli.md.
        if let Ok(token) = std::env::var(ocx_lib::env::keys::OCX_IDENTITY_TOKEN)
            && !token.is_empty()
        {
            return Ok(Some(Zeroizing::new(token)));
        }
        Ok(None)
    }
}

/// Convert a sign-path [`PackageError`] into an `anyhow::Error`, unwrapping the
/// inner [`SignError`] so the `--format json` error envelope's
/// `context.identifier` is populated on every pipeline-stage failure — matching
/// the pre-check paths (offline refusal, URL validation) that already surface a
/// bare `SignError`.
///
/// `ocx_lib::Error::Sign` is `#[error(transparent)]`, so its `source()` forwards
/// straight to the inner `SignErrorKind`, skipping the `SignError` node the
/// envelope's context walk downcasts to. The exit code, `error.kind`, and
/// `error.detail` are unchanged — all three reach the same `SignErrorKind`
/// whether or not the `SignError` node is preserved.
fn sign_error_into_anyhow(err: PackageError) -> anyhow::Error {
    match err.kind {
        PackageErrorKind::Internal(LibError::Sign(sign_error)) => anyhow::Error::new(*sign_error),
        kind => anyhow::Error::new(kind),
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`PackageSign::resolve_override_token`] — C-S1-4 contract.

    use std::str::FromStr;

    use super::*;
    use crate::options;

    fn test_identifier() -> oci::Identifier {
        oci::Identifier::parse("registry.example/pkg:1.0").expect("static parse")
    }

    fn make_sign_cmd(identity_token_file: Option<std::path::PathBuf>, identity_token_stdin: bool) -> PackageSign {
        PackageSign {
            platform: "linux/amd64".parse().expect("platform"),
            fulcio_url: "https://fulcio.sigstore.dev".into(),
            rekor_url: "https://rekor.sigstore.dev".into(),
            identity_token_file,
            identity_token_stdin,
            no_tty: false,
            no_cache: false,
            identifier: options::Identifier::from_str("registry.example/pkg:1.0").expect("ident"),
        }
    }

    /// Write `contents` to a new file in `dir` and set the given Unix mode.
    #[cfg(unix)]
    fn write_with_mode(dir: &std::path::Path, name: &str, contents: &str, mode: u32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn token_file_0644_is_rejected() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = write_with_mode(tmp.path(), "token", "tok123\n", 0o644);
        let cmd = make_sign_cmd(Some(path), false);
        let id = test_identifier();
        let result = cmd.resolve_override_token(&id).await;
        let err = result.expect_err("0644 token file must be rejected");
        // Must classify as IdentityTokenFilePermissive via the error chain.
        let sign_err = err.downcast_ref::<SignError>().expect("SignError in chain");
        assert!(
            matches!(sign_err.kind, SignErrorKind::IdentityTokenFilePermissive { .. }),
            "expected IdentityTokenFilePermissive, got {:?}",
            sign_err.kind
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn token_file_0600_is_accepted() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = write_with_mode(tmp.path(), "token", "my-token\n", 0o600);
        let cmd = make_sign_cmd(Some(path), false);
        let id = test_identifier();
        let token = cmd.resolve_override_token(&id).await.expect("0600 must succeed");
        assert_eq!(token.as_ref().map(|t| t.as_str()), Some("my-token"));
    }

    /// CWE-367 TOCTOU regression: a symlink at the supplied path must be
    /// rejected at `open(2)` time via `O_NOFOLLOW` so an attacker cannot win
    /// the pre-open race between path resolution and the descriptor-side
    /// `fstat` checks. The 0600-mode target file is otherwise valid; the
    /// rejection must come from the symlink itself, not the perm gate.
    #[cfg(unix)]
    #[tokio::test]
    async fn token_file_symlink_is_rejected() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let target = write_with_mode(tmp.path(), "real-token", "secret-token\n", 0o600);
        let link = tmp.path().join("link-to-token");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let cmd = make_sign_cmd(Some(link), false);
        let id = test_identifier();
        let result = cmd.resolve_override_token(&id).await;
        let err = result.expect_err("symlink token file must be rejected");
        let sign_err = err.downcast_ref::<SignError>().expect("SignError in chain");
        // Symlink rejection routes through OidcPreCheckFailed (exit 77),
        // distinct from IdentityTokenFilePermissive which is the perm-bits
        // path. Both share the PermissionDenied exit code but the kind_detail
        // differs — keep them disjoint so error envelopes are unambiguous.
        match &sign_err.kind {
            SignErrorKind::OidcPreCheckFailed { reason } => {
                assert!(
                    reason.contains("symlink"),
                    "OidcPreCheckFailed reason must mention symlink, got: {reason}",
                );
            }
            other => panic!("expected OidcPreCheckFailed (symlink), got {other:?}"),
        }
    }

    /// A pipeline-stage `SignError` wrapped in a `PackageError` (the shape the
    /// sign facade produces) must still surface `context.identifier` in the
    /// `--format json` envelope.
    ///
    /// Regression guard for the `sign_error_into_anyhow` unwrap — mirror of the
    /// verify-side test. `PackageError` omits `#[source]` and `Error::Sign` is
    /// `#[error(transparent)]`, so only the unwrap re-roots the chain on the bare
    /// `SignError` the envelope's context walk downcasts to. If the unwrap
    /// regresses the identifier vanishes and this fails.
    #[test]
    fn sign_error_wrapped_in_package_error_still_populates_envelope_identifier() {
        use crate::error_envelope::render_error_envelope;

        let id = test_identifier();
        let package_error = PackageError::new(
            id.clone(),
            PackageErrorKind::Internal(LibError::Sign(Box::new(SignError::new(
                id,
                SignErrorKind::OidcTokenRejected,
            )))),
        );
        let err = sign_error_into_anyhow(package_error);
        let json = render_error_envelope("package sign", &err).expect("render envelope");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");

        assert_eq!(parsed["exit_code"], 80);
        assert_eq!(parsed["error"]["kind"], "auth_error");
        assert_eq!(
            parsed["error"]["context"]["identifier"], "registry.example/pkg:1.0",
            "identifier must survive the PackageError wrap → sign_error_into_anyhow unwrap",
        );
    }
}
