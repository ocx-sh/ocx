// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared plumbing for the signing and verifying `ocx package` subcommands.
//!
//! Everything here was private to one command before a second one needed it:
//! the C-S1-4 override-token resolution, the offline policy refusal, the
//! report timestamp format, the Sigstore endpoint ladder, the trust-root and
//! identity-policy resolution, the bounded `--predicate` read, and the two
//! `PackageError` unwraps that keep `context.identifier` in the JSON envelope.
//! `sign`, `verify`, `attest`, `sbom` and `push --sbom` all call in.
//!
//! One leaf rather than command-to-command imports, for two reasons: the token
//! resolver is security-critical and must not fork into a second copy, and a
//! helper reached across sibling commands (`push` calling into `attest`,
//! `sbom` calling into `verify`) makes the dependency graph a mesh, where the
//! next command has two plausible places to import each helper from.
//!
//! Named `package_sign_common` rather than for a verb: `<command>_<subcommand>.rs`
//! is a leaf in this crate (`subsystem-cli.md`), so a shared leaf takes the
//! `_common` suffix — the position `patch_common.rs` and `index_common.rs`
//! already hold.

use std::path::Path;

use anyhow::Context as _;
use tokio::io::AsyncReadExt;
use zeroize::Zeroizing;

use ocx_lib::Error as LibError;
use ocx_lib::oci;
use ocx_lib::oci::attest::MAX_PREDICATE_FILE_BYTES;
use ocx_lib::oci::endpoint::{Url, validate_sigstore_url};
use ocx_lib::oci::sign::{KeyRef, SignError, SignErrorKind};
use ocx_lib::oci::verify::{TrustRoot, VerifyError, VerifyErrorKind};
use ocx_lib::package_manager::error::{PackageError, PackageErrorKind};
use ocx_lib::trust::{self, CompiledPolicy};

use crate::api::data::signature::{SignatureLegReport, SignatureReport};

/// Refuse a sign-side operation when the run is offline.
///
/// S1-E policy: an offline sign is a deliberate rejection, NOT a passive
/// network-access failure. The caller passes the refusal kind for its own verb
/// ([`SignErrorKind::OfflineSignRefused`] for `sign`); every one of them
/// classifies to exit 77 (`PermissionDenied`).
///
/// Callers run this before the token-resolution path, so a refused run never
/// touches a credential.
pub(super) fn refuse_when_offline(
    context: &crate::app::Context,
    identifier: &oci::Identifier,
    kind: SignErrorKind,
) -> anyhow::Result<()> {
    if context.is_offline() {
        return Err(anyhow::Error::from(SignError::new(identifier.clone(), kind)));
    }
    Ok(())
}

/// The basename of a credential path, for an error message.
///
/// `--identity-token-file` names a secret's location, and the full path leaks
/// through stderr, the JSON error envelope and any log sink (CWE-209).
/// [`SignErrorKind::IdentityTokenFilePermissive`] already renders only this
/// half, and the I/O failures beside it must not render more — which is why
/// the reads in [`resolve_override_token`] are typed through `file_error` with
/// *this* rather than with the path itself. Typing them at all is what turns an
/// operator's typo into exit 74 instead of exit 1 `internal`.
#[cfg(unix)]
fn redacted_token_path(path: &std::path::Path) -> std::path::PathBuf {
    path.file_name()
        .map_or_else(|| std::path::PathBuf::from("<redacted>"), std::path::PathBuf::from)
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
pub(super) async fn resolve_override_token(
    identity_token_file: Option<&Path>,
    identity_token_stdin: bool,
    identifier: &oci::Identifier,
) -> anyhow::Result<Option<Zeroizing<String>>> {
    // On a non-Unix target (Windows), ACL-based permission validation is not
    // implemented for Slice 1 (windows-acl integration is out of scope).
    // Refuse explicitly rather than silently skipping the check — a
    // readable-by-others token file is a security misconfiguration and must
    // not be accepted silently.
    #[cfg(not(unix))]
    if identity_token_file.is_some() {
        return Err(anyhow::Error::from(SignError::new(
            identifier.clone(),
            SignErrorKind::OidcPreCheckFailed {
                reason: "identity-token-file permission validation is not supported on Windows; \
                         use --identity-token-stdin or OCX_IDENTITY_TOKEN instead"
                    .into(),
            },
        )));
    }
    #[cfg(unix)]
    if let Some(path) = identity_token_file {
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
            let path_owned = path.to_path_buf();
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
                            // Typed, so an operator's typo exits 74 rather than
                            // falling through the downcast ladder to exit 1
                            // `internal` — a missing token file reported as a bug
                            // in ocx. The basename only: the CWE-209 note above is
                            // not relaxed, it is honoured by what is handed in.
                            anyhow::Error::new(ocx_lib::error::file_error(redacted_token_path(&path_owned), e))
                                .context("failed to open --identity-token-file")
                        }
                    })?;
                let meta = std_file
                    .metadata()
                    .map_err(|e| ocx_lib::error::file_error(redacted_token_path(&path_owned), e))
                    .context("failed to stat --identity-token-file")?;
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
                // The regular-file half of `read_bounded`'s pair of guards,
                // asked of the handle rather than re-derived from the path.
                // `/dev/zero` reports length 0 and then yields forever, which
                // no byte ceiling alone refuses; a FIFO blocks instead. Both
                // pass the uid and mode checks above when the operator owns
                // them.
                if !meta.is_file() {
                    return Err(anyhow::Error::new(ocx_lib::error::file_error(
                        redacted_token_path(&path_owned),
                        std::io::Error::other("--identity-token-file is not a regular file"),
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
            // The byte-ceiling half of `read_bounded`'s pair, applied to the
            // handle the checks above validated rather than through a second
            // `open` of the path. `read_bounded` takes a path, so calling it
            // here would drop `O_NOFOLLOW`, reopen a name an attacker may have
            // swapped since the uid/mode gate ran (CWE-367), and land the
            // cleartext in an unzeroized `Vec`. `take` is the same bound over
            // the handle: `cap + 1` is what tells "exactly at the cap" from
            // "over it", and it stops the read rather than only the answer.
            (&mut file)
                .take(MAX_IDENTITY_TOKEN_BYTES + 1)
                .read_to_string(&mut raw)
                .await
                .map_err(|e| ocx_lib::error::file_error(redacted_token_path(path), e))
                .context("failed to read --identity-token-file")?;
            if raw.len() as u64 > MAX_IDENTITY_TOKEN_BYTES {
                return Err(anyhow::Error::new(ocx_lib::error::file_error(
                    redacted_token_path(path),
                    std::io::Error::other(format!(
                        "--identity-token-file is larger than {MAX_IDENTITY_TOKEN_BYTES} bytes"
                    )),
                ))
                .context("failed to read --identity-token-file"));
            }
            return Ok(Some(Zeroizing::new(raw.trim().to_string())));
        }
    }
    if identity_token_stdin {
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

/// The largest an `--identity-token-file` may be.
///
/// An OIDC ID token is a compact JWS — a few kilobytes at the outside, and the
/// providers this path talks to sit well under one. Sixty-four kibibytes is the
/// ceiling `MAX_KEY_PEM_BYTES` already puts on the other credential file an
/// operator names, and it exists only to bound the read of a path that was
/// typed, not to police token shape.
///
/// Unix-only, like its single use site. On Windows `--identity-token-file` is
/// refused before any read — ACL-based permission validation is unimplemented
/// there, and the flag fails closed rather than skipping the check — so there
/// is no read for this ceiling to bound and an ungated constant is dead code
/// that `-D warnings` fails the build on.
#[cfg(unix)]
const MAX_IDENTITY_TOKEN_BYTES: u64 = 64 * 1024;

/// Default public Fulcio CA endpoint.
///
/// The one literal for it: `sign`, `attest` and `push --sbom` all reach it
/// through [`resolve_endpoint`], so there is no second copy to drift. Rekor's
/// twin is [`ocx_lib::oci::endpoint::DEFAULT_REKOR_URL`], which already lives
/// in the library because verify needs it too.
pub(crate) const DEFAULT_FULCIO_URL: &str = "https://fulcio.sigstore.dev";

/// Which Sigstore service an endpoint is being resolved for.
///
/// Names the service once instead of making a call site thread a config field
/// and a matching builtin default that must agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SigstoreEndpoint {
    Fulcio,
    Rekor,
}

/// Resolve a Sigstore endpoint URL: flag > `[trust.sigstore]` > builtin default.
///
/// The config tier is what makes a self-hosted Fulcio/Rekor a fleet-wide
/// setting rather than a flag every invocation has to repeat — and until this
/// existed, `[trust.sigstore].fulcio_url` and `.rekor_url` were documented
/// defaults that nothing read.
///
/// The result is still untrusted: a config-supplied URL is exactly as
/// attacker-reachable as a flag-supplied one (a `[managed]` payload can carry
/// one), so every caller passes it through
/// [`validate_sigstore_url`](ocx_lib::oci::endpoint::validate_sigstore_url)
/// before it becomes an HTTP target. Returning a `String` rather than a
/// validated `Url` is what keeps that step at the call site, where the
/// per-subsystem error wrap (`SignError` vs `VerifyError`) lives.
pub(crate) fn resolve_endpoint(
    flag: Option<&str>,
    configured: Option<&ocx_lib::trust::SigstoreTrust>,
    endpoint: SigstoreEndpoint,
) -> String {
    let from_config = configured.and_then(|sigstore| match endpoint {
        SigstoreEndpoint::Fulcio => sigstore.fulcio_url.as_deref(),
        SigstoreEndpoint::Rekor => sigstore.rekor_url.as_deref(),
    });
    let builtin = match endpoint {
        SigstoreEndpoint::Fulcio => DEFAULT_FULCIO_URL,
        SigstoreEndpoint::Rekor => ocx_lib::oci::endpoint::DEFAULT_REKOR_URL,
    };
    flag.or(from_config).unwrap_or(builtin).to_string()
}

/// Resolve and validate the Fulcio/Rekor pair every signing verb needs.
///
/// One ladder, walked twice: flag > `[trust.sigstore]` > builtin default, then
/// the SSRF guard (CWE-918) on whichever tier supplied the value, because a
/// config-sourced URL is exactly as attacker-reachable as a flag-sourced one —
/// a `[managed]` payload can carry one. Failures route through
/// [`SignErrorKind::InvalidEndpointUrl`] → exit 64, so the envelope's
/// `error.detail` names the offending flag.
///
/// `push --sbom` exposes no endpoint flags and passes `None` for both, landing
/// on the tail of the same ladder: without the config tier an operator on a
/// self-hosted stack could `attest` but not `push --sbom` — the same run, two
/// different Fulcios.
///
/// Fulcio is validated first, so a run with both endpoints wrong names
/// `--fulcio-url` every time rather than whichever the compiler ordered.
///
/// # Errors
///
/// [`SignErrorKind::InvalidEndpointUrl`] (exit 64) naming the rejected flag.
pub(super) fn resolve_sigstore_pair(
    configured: Option<&ocx_lib::trust::SigstoreTrust>,
    identifier: &oci::Identifier,
    fulcio_flag: Option<&str>,
    rekor_flag: Option<&str>,
) -> anyhow::Result<(Url, Url)> {
    let fulcio = resolve_endpoint(fulcio_flag, configured, SigstoreEndpoint::Fulcio);
    let rekor = resolve_endpoint(rekor_flag, configured, SigstoreEndpoint::Rekor);
    let fulcio_url =
        validate_sigstore_url(&fulcio, "--fulcio-url").map_err(invalid_endpoint(identifier, "--fulcio-url"))?;
    let rekor_url =
        validate_sigstore_url(&rekor, "--rekor-url").map_err(invalid_endpoint(identifier, "--rekor-url"))?;
    Ok((fulcio_url, rekor_url))
}

/// Resolve and validate the Rekor endpoint a verifying verb needs.
///
/// The same ladder and the same guard as [`resolve_sigstore_pair`], for the
/// commands that read a transparency log without ever talking to a CA. It is a
/// separate function rather than the pair's second element on purpose: reusing
/// the pair here would make a `[trust.sigstore].fulcio_url` typo fail `verify`
/// and `sbom`, which never contact Fulcio at all.
///
/// The refusal is a [`VerifyErrorKind`], not a `SignErrorKind` — same exit 64,
/// but exit-code classification stays inside the subsystem that failed.
///
/// The config tier matters here for one reason beyond a convenient default:
/// the trust-root cache is keyed by the Rekor instance, so a self-hosted root
/// cached under the public-good key is a cache collision, not a cosmetic
/// mismatch.
///
/// # Errors
///
/// [`VerifyErrorKind::InvalidEndpointUrl`] (exit 64) naming `--rekor-url`.
pub(super) fn resolve_rekor_endpoint(
    configured: Option<&ocx_lib::trust::SigstoreTrust>,
    identifier: &oci::Identifier,
    rekor_flag: Option<&str>,
) -> anyhow::Result<Url> {
    let rekor = resolve_endpoint(rekor_flag, configured, SigstoreEndpoint::Rekor);
    validate_sigstore_url(&rekor, "--rekor-url").map_err(|reason| {
        anyhow::Error::from(VerifyError::new(
            identifier.clone(),
            VerifyErrorKind::InvalidEndpointUrl {
                endpoint: "--rekor-url".into(),
                reason,
            },
        ))
    })
}

/// Format a UTC epoch-seconds timestamp as ISO-8601 (`YYYY-MM-DDThh:mm:ssZ`).
pub(super) fn iso8601(epoch_secs: u64) -> String {
    chrono::DateTime::from_timestamp(epoch_secs as i64, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

// ── moved from `package_verify.rs` (ARCH-3) ─────────────────────────────

/// Build the ANY-of identity constraints the signing certificate must
/// satisfy.
///
/// Flag mode (`--certificate-identity` + `--certificate-oidc-issuer`, kept
/// both-or-neither by clap): a single exact pair that overrides any policy
/// — this preserves the original flag-only verify behaviour unchanged.
/// Policy mode (neither flag): the scope-matched `[[trust.policy]]` set
/// under cross-tier precedence — the operator `config.toml` tiers are
/// authoritative; the project `ocx.toml` only adds trust where the operator
/// has not governed the scope (see [`trust::resolve_tiered`]). A malformed
/// matched policy → [`VerifyErrorKind::TrustPolicyInvalid`] (exit 78); no
/// matching policy → [`VerifyErrorKind::NoIdentityProvided`] (exit 64). The
/// one carve-out is a signer whose `key` names a file that cannot be read: that is a
/// filesystem failure on an operator-supplied path, so it exits 74 `io_error`
/// like the `--key` sign door, not 78.
///
/// Key mode (`key`) short-circuits both: a `--key` reference names the one
/// public key that may have signed, so no keyless matcher is consulted. The
/// parameter is inert here — every caller passes `None`, and loop D supplies
/// it from [`KeyOpt::reference`](crate::options::key::KeyOpt::reference) in
/// its own command files, so this shared leaf is not edited again.
pub(super) async fn resolve_policies(
    context: &crate::app::Context,
    identifier: &oci::Identifier,
    certificate_identity: Option<&str>,
    certificate_oidc_issuer: Option<&str>,
    key: Option<&KeyRef>,
) -> anyhow::Result<Vec<CompiledPolicy>> {
    let compiled =
        resolve_policies_lenient(context, identifier, certificate_identity, certificate_oidc_issuer, key).await?;
    if compiled.is_empty() {
        return Err(VerifyError::new(identifier.clone(), VerifyErrorKind::NoIdentityProvided).into());
    }
    Ok(compiled)
}

/// [`resolve_policies`] without the empty-set refusal: no matching policy is
/// an empty `Vec`, not an error.
///
/// The split exists because "no identity source" is a *question* for
/// `ocx package sbom`, not a verdict. Its default mode reads the empty set as
/// "nobody asked for verification here, so read permissively", where
/// `ocx package verify` and an explicit `--verify` read the same emptiness as
/// "you demanded verification and named nothing to verify against" (exit 64).
/// One resolution, two readings — the alternative is a second copy of the
/// tiered-precedence walk that drifts on the first fix.
///
/// `key` never widens the empty set the split is about: key mode returns
/// exactly one policy or an error, so both readings stay reachable only
/// through the keyless path they were written for.
pub(super) async fn resolve_policies_lenient(
    context: &crate::app::Context,
    identifier: &oci::Identifier,
    certificate_identity: Option<&str>,
    certificate_oidc_issuer: Option<&str>,
    key: Option<&KeyRef>,
) -> anyhow::Result<Vec<CompiledPolicy>> {
    // Key mode pins a single public key and never consults a keyless matcher,
    // so it decides ahead of the flag pair. The keyless certificate flags are
    // refused by clap (`conflicts_with = "key"`) on each command that carries
    // both groups, so reaching here with a key *and* a flag pair is not a
    // reachable invocation.
    if let Some(key) = key {
        let policy = trust::compile_key_signer(key)
            .map_err(|kind| VerifyError::new(identifier.clone(), VerifyErrorKind::from(kind)))?;
        return Ok(vec![policy]);
    }

    if let (Some(identity), Some(issuer)) = (certificate_identity, certificate_oidc_issuer) {
        return Ok(vec![CompiledPolicy::exact(identity.to_owned(), issuer.to_owned())]);
    }

    let target = format!("{}/{}", identifier.registry(), identifier.repository());
    let project_policies = project_trust_policies(context, identifier).await?;
    // Operator tier (config.toml) is authoritative; the project ocx.toml
    // only adds trust for scopes the operator has not governed.
    trust::resolve_tiered(context.config_trust_policies(), &project_policies, &target)
        .map_err(|kind| VerifyError::new(identifier.clone(), VerifyErrorKind::from(kind)).into())
}

/// The project `ocx.toml` trust policies for the in-effect project (empty
/// when no project file resolves). This is the deliberate OCI-tier carve-out
/// for a security concern — verify reads `[[trust.policy]]` from `ocx.toml`,
/// which OCI-tier commands otherwise never consult (see `adr_trust_policy.md`).
async fn project_trust_policies(
    context: &crate::app::Context,
    identifier: &oci::Identifier,
) -> anyhow::Result<Vec<trust::TrustPolicy>> {
    // A missing/inaccessible CWD is non-fatal: `ProjectConfig::resolve` still
    // honors an explicit `--project` / `OCX_PROJECT`, and with no project file
    // resolved the trust-policy set is simply empty (flag-mode verify works).
    let cwd = std::env::current_dir().ok();
    let ocx_home = context.file_structure().root();
    let resolved = ocx_lib::project::ProjectConfig::resolve(
        cwd.as_deref(),
        context.project_path(),
        Some(ocx_home),
        context.global(),
    )
    .await?;
    match resolved {
        Some((config_path, _lock_path)) => {
            // Lenient trust-only parse: an unrelated malformed section (a bad
            // `[tools]` entry, etc.) must NOT fail verify — only `[trust]`
            // matters here (the OCI-tier carve-out is scoped to trust policy).
            let text = tokio::fs::read_to_string(&config_path)
                .await
                .map_err(|error| ocx_lib::error::file_error(&config_path, error))
                .with_context(|| format!("reading project config `{}` for trust policies", config_path.display()))?;
            // Anchored on the project file's own directory, like every other
            // tier: a relative key path must name the same file whether
            // verify runs from the project root or a subdirectory.
            let config_dir = config_path.parent().unwrap_or_else(|| std::path::Path::new("."));
            // Mapped, never bubbled: `TrustPolicyError` has no rung in the
            // downcast ladder, so a bare `?` here exits 1 `internal` for an
            // operator's malformed `ocx.toml`. The same wrapper every other
            // trust-policy refusal on this path already goes through.
            trust::policies_from_ocx_toml(&text, config_dir)
                .map_err(|kind| VerifyError::new(identifier.clone(), VerifyErrorKind::TrustPolicyInvalid(kind)).into())
        }
        None => Ok(Vec::new()),
    }
}

/// Resolve the trust root in precedence order, offline-aware.
///
/// Layers flag-vs-env override resolution on the shared
/// [`ocx_lib::oci::verify::resolve_trust_root`] ladder (`--sigstore-trusted-root` /
/// `OCX_SIGSTORE_TRUSTED_ROOT` → `[trust.sigstore]` → the
/// `$OCX_HOME/sigstore/trusted-root.json` convention path → trust-root
/// cache → embedded root, with the offline pinned-Rekor-key gate). The flag
/// wins over the env; the shared ladder is the single source of truth for
/// every rung below that (auto-verify reuses it). Any failure is tagged
/// with the target identifier.
pub(super) async fn resolve_trust_root(
    context: &crate::app::Context,
    identifier: &oci::Identifier,
    rekor_cache_key: &str,
    offline: bool,
    trusted_root: Option<&std::path::Path>,
) -> anyhow::Result<TrustRoot> {
    let explicit = trusted_root
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::var_os("OCX_SIGSTORE_TRUSTED_ROOT").map(std::path::PathBuf::from));
    let home_trusted_root = ocx_lib::ConfigLoader::home_sigstore_trusted_root_path();
    ocx_lib::oci::verify::resolve_trust_root(
        explicit.as_deref(),
        context.config_trust_sigstore(),
        home_trusted_root.as_deref(),
        &context.file_structure().state,
        rekor_cache_key,
        offline,
    )
    .await
    .map_err(|kind| VerifyError::new(identifier.clone(), kind).into())
}

/// Convert a verify-path [`PackageError`] into an `anyhow::Error`, unwrapping
/// the inner [`VerifyError`] so the `--format json` error envelope's
/// `context.identifier` is populated on every pipeline-stage failure — matching
/// the pre-check paths (URL validation, identity/trust-root resolution) that
/// already surface a bare `VerifyError`.
///
/// `ocx_lib::Error::Verify` is `#[error(transparent)]`, so its `source()`
/// forwards straight to the inner `VerifyErrorKind`, skipping the `VerifyError`
/// node the envelope's context walk downcasts to. The exit code, `error.kind`,
/// and `error.detail` are unchanged — all three reach the same `VerifyErrorKind`
/// whether or not the `VerifyError` node is preserved.
pub(super) fn verify_error_into_anyhow(err: PackageError) -> anyhow::Error {
    match err.kind {
        PackageErrorKind::Internal(LibError::Verify(verify_error)) => anyhow::Error::new(*verify_error),
        kind => anyhow::Error::new(kind),
    }
}

// ── moved from `package_attest.rs` (ARCH-3) ─────────────────────────────

/// Build the `SignError` for a rejected Sigstore endpoint URL.
///
/// Private: every signing verb reaches it through
/// [`resolve_sigstore_pair`], so all three report the same kind, the same
/// exit code (64) and the same offending flag by construction rather than by
/// three call sites agreeing.
fn invalid_endpoint(
    identifier: &oci::Identifier,
    flag: &'static str,
) -> impl Fn(ocx_lib::oci::endpoint::UrlRejection) -> anyhow::Error {
    let identifier = identifier.clone();
    move |reason| {
        anyhow::Error::from(SignError::new(
            identifier.clone(),
            SignErrorKind::InvalidEndpointUrl {
                endpoint: flag.into(),
                reason,
            },
        ))
    }
}

/// Read the `--predicate` file into memory, bounded and without following a
/// symlink at the named path.
///
/// Shared with `package push --sbom`.
///
/// The bound is enforced *while reading*, never as a `metadata().len()` check
/// followed by an unbounded read: the length on disk is a hint, not a promise
/// about how many bytes arrive, and a `Vec::with_capacity` sized from it is an
/// allocation an attacker chooses (PKG-04, PKG-07).
///
/// # Errors
///
/// [`SignErrorKind::PredicateTooLarge`] (exit 65) past the limit; an I/O error
/// (exit 74) naming the path otherwise, including the symlink refusal.
pub(super) async fn read_predicate(path: &Path, identifier: &oci::Identifier) -> anyhow::Result<Vec<u8>> {
    let file = open_predicate(path).await?;

    // One byte past the ceiling: enough to tell "at the limit" from "over it"
    // without reading a byte more than that.
    let ceiling = u64::try_from(MAX_PREDICATE_FILE_BYTES)
        .expect("MAX_PREDICATE_FILE_BYTES is a compile-time constant well under u64::MAX")
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(ceiling)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| ocx_lib::error::file_error(path, error))?;

    if bytes.len() > MAX_PREDICATE_FILE_BYTES {
        return Err(anyhow::Error::from(SignError::new(
            identifier.clone(),
            SignErrorKind::PredicateTooLarge {
                limit: ceiling.saturating_sub(1),
                // What was counted before the limit tripped, not what is on
                // disk: the read stops at the ceiling, so the file's real size
                // is deliberately never asked for.
                actual: ceiling,
            },
        )));
    }
    Ok(bytes)
}

/// Open the predicate file, refusing a symlink at the named path.
///
/// Unlike `--identity-token-file`, ownership and mode are deliberately NOT
/// checked: a predicate is public data destined for publication, and a 0644
/// SBOM written by an earlier CI step is the ordinary case — a mode gate would
/// reject that while protecting nothing. The symlink refusal is kept because
/// its consequence is not confidentiality-shaped but irreversible: whatever the
/// link resolves to would be embedded, signed with the caller's identity,
/// pushed, and hashed into an append-only public log. Refusing at `open(2)`
/// rather than by a prior `symlink_metadata` check closes the swap window
/// between the decision and the read (CWE-367).
async fn open_predicate(path: &Path) -> anyhow::Result<tokio::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        // `OpenOptions::open` is a blocking syscall; run it on the blocking
        // pool and hand the resulting descriptor to the reactor, the same shape
        // `package_sign_common` uses for the token file.
        let owned = path.to_path_buf();
        let opened = tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&owned)
        })
        .await
        .map_err(|join| ocx_lib::error::file_error(path, std::io::Error::other(join)))?;

        match opened {
            Ok(file) => Ok(tokio::fs::File::from_std(file)),
            // POSIX specifies ELOOP for `O_NOFOLLOW` on a symlink; it is the
            // one open(2) error that means "this path is a link", so it is
            // reported as the refusal it is rather than as a generic I/O fault.
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                Err(anyhow::Error::from(ocx_lib::error::file_error(path, error))
                    .context("refusing to read a predicate through a symlink"))
            }
            Err(error) => Err(ocx_lib::error::file_error(path, error).into()),
        }
    }
    #[cfg(not(unix))]
    {
        // No `O_NOFOLLOW` equivalent is wired for Windows here; the open is
        // plain, and the symlink refusal is a Unix-only guarantee.
        tokio::fs::File::open(path)
            .await
            .map_err(|error| ocx_lib::error::file_error(path, error).into())
    }
}

/// Describe a failed `push --sbom` attestation for the push report.
///
/// The slug is lifted out of the JSON error envelope this error would have
/// rendered, so the value a script reads here is the value it reads there —
/// two spellings of one failure is exactly what CLI-04 exists to stop.
pub(super) fn failed_outcome(err: &anyhow::Error) -> crate::api::data::push::AttestationOutcome {
    crate::api::data::push::AttestationOutcome::Failed {
        kind: error_slug("package attest", err),
        // Registry-sourced names and tags reach an error chain verbatim
        // (CWE-150), and this string is rendered to a terminal.
        message: crate::api::data::sanitize_for_terminal(&format!("{err:#}")),
    }
}

/// The slug the JSON error envelope would give `err`, for a report that has to
/// name a failure it survived.
///
/// `error.detail` is the per-variant slug and `error.kind` the frozen category
/// it rolls up to; `detail` is absent for errors outside the sign/verify
/// taxonomy, and the category is then the most specific thing there is. Lifting
/// it out of the rendered envelope rather than re-deriving it is the point:
/// two spellings of one failure is exactly what CLI-04 exists to stop, and a
/// `--tags` sweep reports per-tag failures the envelope never gets to render.
pub(super) fn error_slug(command: &str, err: &anyhow::Error) -> String {
    crate::error_envelope::render_error_envelope(command, err)
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .as_ref()
        .and_then(|value| {
            value["error"]["detail"]
                .as_str()
                .or_else(|| value["error"]["kind"].as_str())
        })
        .unwrap_or("failure")
        .to_owned()
}

/// The frozen `error.kind` category a leg failure rolls up to, as the wire
/// spells it.
///
/// A failed *leg* never reaches the error envelope — its run returned `Ok` —
/// so there is no rendered `error.detail` to lift. The category is then the
/// most specific thing there is, and it is the same fallback
/// [`error_slug`] takes for errors outside the sign and verify taxonomies, so
/// a sweep's rows carry one vocabulary either way.
pub(super) fn category_slug(code: ocx_lib::cli::ExitCode) -> String {
    serde_json::to_value(ocx_lib::cli::ErrorCategory::from_exit_code(code))
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "failure".to_string())
}

/// The exit code a `--tags` / `--tags-file` sweep returns.
///
/// * No failure -> `Success`.
/// * Every failure the same code -> that code. A twenty-tag sweep that hit one
///   fault (every tag 401, say) is scriptably that one fault, and flattening it
///   to a generic failure would throw away the only thing `case $?` could act
///   on.
/// * A mix -> `Failure`. There is no true single answer, and picking the first
///   or the worst would claim a fault class the run did not have. `Failure` is
///   defined as "use only when no specific code applies", which is exactly the
///   state a mixed sweep is in. The per-tag codes stay readable in the report.
///
/// No new [`ExitCode`](ocx_lib::cli::ExitCode) variant: every answer here is
/// one a script already knows.
pub(super) fn sweep_exit_code(failures: &[ocx_lib::cli::ExitCode]) -> ocx_lib::cli::ExitCode {
    let mut codes = failures.iter();
    let Some(first) = codes.next() else {
        return ocx_lib::cli::ExitCode::Success;
    };
    match codes.all(|code| code == first) {
        true => *first,
        false => ocx_lib::cli::ExitCode::Failure,
    }
}

/// Convert an attest-path [`PackageError`] into an `anyhow::Error`, unwrapping
/// the inner [`SignError`] so the `--format json` envelope's
/// `context.identifier` is populated on every pipeline-stage failure — matching
/// the pre-check paths (offline refusal, URL validation, predicate read) that
/// already surface a bare `SignError`.
///
/// `ocx_lib::Error::Sign` is `#[error(transparent)]`, so its `source()` forwards
/// straight to the inner `SignErrorKind`, skipping the `SignError` node the
/// envelope's context walk downcasts to. The exit code, `error.kind` and
/// `error.detail` reach the same `SignErrorKind` either way; the identifier
/// does not, which is what the unwrap is for.
pub(super) fn attest_error_into_anyhow(err: PackageError) -> anyhow::Error {
    match err.kind {
        PackageErrorKind::Internal(LibError::Sign(sign_error)) => anyhow::Error::new(*sign_error),
        kind => anyhow::Error::new(kind),
    }
}

/// Build the per-reference report from one pipeline result.
///
/// Shared by the single-reference path and the sweep, so a swept tag's row
/// carries the same document a single run prints — the sweep aggregates the
/// existing report rather than modelling a second one.
pub(super) fn signature_report(
    identifier: &oci::Identifier,
    platform: Option<&oci::Platform>,
    result: ocx_lib::oci::sign::SignResult,
) -> SignatureReport {
    let legs = result
        .legs
        .iter()
        .map(|leg| match &leg.outcome {
            Ok(digests) => SignatureLegReport {
                format: leg.format,
                payload_digest: Some(digests.payload_digest.clone()),
                manifest_digest: Some(digests.manifest_digest.clone()),
                error: None,
            },
            Err(error) => SignatureLegReport {
                format: leg.format,
                payload_digest: None,
                manifest_digest: None,
                error: Some(error.to_string()),
            },
        })
        .collect();

    SignatureReport::new(
        identifier.to_string(),
        result.subject_digest,
        legs,
        platform,
        result.certificate_identity,
        result.certificate_oidc_issuer,
    )
    .with_key_model(result.key_backend, result.public_key_hint)
    .with_transparency_log(result.transparency_log_index)
}

/// The exit code one failed leg deserves, walking an `Internal` cause exactly
/// the way [`SignError::classify`](ocx_lib::oci::sign::SignError::classify) does for a whole-run failure.
///
/// `SignErrorKind::exit_code` answers `Failure` (1) for `Internal`, and every
/// registry fault reaches a leg wrapped in `Internal` (`referrers::map_client_error`
/// keeps the `ClientError` intact under it rather than flattening it). Reading
/// the kind directly would exit 1 for a 503 that exits 75 when it fails the run
/// as a whole — the same fault, two codes, decided by how many legs it hit.
pub(super) fn leg_exit_code(kind: &ocx_lib::oci::sign::SignErrorKind) -> ocx_lib::cli::ExitCode {
    match kind {
        ocx_lib::oci::sign::SignErrorKind::Internal(cause) => ocx_lib::cli::classify_error(cause.as_ref()),
        other => ocx_lib::cli::ClassifyErrorKind::exit_code(other),
    }
}

#[cfg(test)]
mod leg_exit_code_tests {
    use super::leg_exit_code;
    use ocx_lib::cli::ExitCode;
    use ocx_lib::oci::client::error::ClientError;
    use ocx_lib::oci::sign::SignErrorKind;

    /// A leg that failed on a registry fault must exit the way the same fault
    /// exits when it fails the whole run.
    ///
    /// `SignErrorKind::exit_code` answers `Failure` (1) for `Internal`, which is
    /// how every registry fault reaches a leg — `referrers::map_client_error`
    /// keeps the `ClientError` intact under it rather than flattening it. Read
    /// directly, a 503 that fails one leg would exit 1 and a 503 that fails both
    /// would exit 75: the same fault, two codes, decided by arithmetic on legs.
    #[test]
    fn a_failed_leg_takes_the_exit_code_of_the_cause_it_wraps() {
        let transient = SignErrorKind::Internal(Box::new(ClientError::RegistryTransient("503".into())));
        assert_eq!(leg_exit_code(&transient), ExitCode::TempFail);
    }

    /// A kind that classifies itself still answers for itself.
    #[test]
    fn a_leg_failing_on_a_sign_side_kind_keeps_that_kinds_code() {
        assert_eq!(
            leg_exit_code(&SignErrorKind::ReferrersUnsupported),
            ExitCode::ReferrersUnsupported
        );
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`resolve_override_token`] (C-S1-4),
    //! [`resolve_endpoint`] (the Sigstore endpoint precedence), and the
    //! attest-path error contract and `--predicate` read that moved here with
    //! their helpers.

    use super::*;
    use crate::error_envelope::render_error_envelope;

    /// A `[trust.sigstore]` block pinning both endpoints at a self-hosted stack.
    fn self_hosted() -> ocx_lib::trust::SigstoreTrust {
        ocx_lib::trust::SigstoreTrust {
            fulcio_url: Some("https://fulcio.corp.example".to_string()),
            rekor_url: Some("https://rekor.corp.example".to_string()),
            ..Default::default()
        }
    }

    /// The full precedence triple, per endpoint: builtin when nothing is set,
    /// config over builtin, flag over config.
    ///
    /// Swapping the `flag.or(from_config)` order in [`resolve_endpoint`] reds
    /// the third row of each table.
    #[test]
    fn an_endpoint_resolves_flag_over_config_over_builtin() {
        let cfg = self_hosted();
        let cases = [
            (
                SigstoreEndpoint::Fulcio,
                DEFAULT_FULCIO_URL,
                "https://fulcio.corp.example",
                "https://fulcio.flag.example",
            ),
            (
                SigstoreEndpoint::Rekor,
                ocx_lib::oci::endpoint::DEFAULT_REKOR_URL,
                "https://rekor.corp.example",
                "https://rekor.flag.example",
            ),
        ];
        for (endpoint, builtin, configured, flag) in cases {
            assert_eq!(resolve_endpoint(None, None, endpoint), builtin, "{endpoint:?}: builtin");
            assert_eq!(
                resolve_endpoint(None, Some(&cfg), endpoint),
                configured,
                "{endpoint:?}: config must beat the builtin default"
            );
            assert_eq!(
                resolve_endpoint(Some(flag), Some(&cfg), endpoint),
                flag,
                "{endpoint:?}: the flag must beat config"
            );
        }
    }

    /// A `[trust.sigstore]` block that sets only one endpoint must not drag the
    /// other off its default — the two fields are independent decisions.
    #[test]
    fn a_half_filled_config_leaves_the_other_endpoint_alone() {
        let cfg = ocx_lib::trust::SigstoreTrust {
            rekor_url: Some("https://rekor.corp.example".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_endpoint(None, Some(&cfg), SigstoreEndpoint::Fulcio),
            DEFAULT_FULCIO_URL
        );
        assert_eq!(
            resolve_endpoint(None, Some(&cfg), SigstoreEndpoint::Rekor),
            "https://rekor.corp.example"
        );
    }

    /// A config-sourced URL is exactly as untrusted as a flag-sourced one: a
    /// `[managed]` payload can carry one, so it must hit the same SSRF guard
    /// and be refused identically (CWE-918).
    ///
    /// The positive half is load-bearing — without it a validator that refused
    /// *everything* would pass the negative half unnoticed.
    #[test]
    fn a_forbidden_config_url_is_refused_exactly_like_a_forbidden_flag_url() {
        use ocx_lib::oci::endpoint::validate_sigstore_url;

        let forbidden = "http://fulcio.corp.example";
        let hostile = ocx_lib::trust::SigstoreTrust {
            fulcio_url: Some(forbidden.to_string()),
            ..Default::default()
        };

        let via_config = resolve_endpoint(None, Some(&hostile), SigstoreEndpoint::Fulcio);
        let via_flag = resolve_endpoint(Some(forbidden), None, SigstoreEndpoint::Fulcio);
        assert_eq!(via_config, via_flag, "both tiers must yield the same URL to validate");
        assert!(
            validate_sigstore_url(&via_config, "--fulcio-url").is_err(),
            "plain-http config endpoint must be refused"
        );
        assert!(validate_sigstore_url(&via_flag, "--fulcio-url").is_err());

        // The guard can say yes: an https config endpoint passes.
        let allowed = resolve_endpoint(None, Some(&self_hosted()), SigstoreEndpoint::Fulcio);
        assert!(
            validate_sigstore_url(&allowed, "--fulcio-url").is_ok(),
            "an https config endpoint must pass the same guard"
        );
    }

    fn test_identifier() -> oci::Identifier {
        oci::Identifier::parse("registry.example/pkg:1.0").expect("static parse")
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
        let id = test_identifier();
        let result = resolve_override_token(Some(path.as_path()), false, &id).await;
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
        let id = test_identifier();
        let token = resolve_override_token(Some(path.as_path()), false, &id)
            .await
            .expect("0600 must succeed");
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
        let id = test_identifier();
        let result = resolve_override_token(Some(link.as_path()), false, &id).await;
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

    /// On a non-Unix target the permission gate cannot run, so the flag is
    /// refused outright rather than accepted unchecked.
    ///
    /// This is the only test that reaches the `#[cfg(not(unix))]` arm — the arm
    /// was previously unreachable from any test, so a regression to "silently
    /// skip the check" would have compiled and shipped.
    #[cfg(not(unix))]
    #[tokio::test]
    async fn token_file_is_refused_where_permissions_cannot_be_checked() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("token");
        std::fs::write(&path, "tok123\n").expect("write");
        let id = test_identifier();
        let err = resolve_override_token(Some(path.as_path()), false, &id)
            .await
            .expect_err("a token file must be refused where its mode cannot be read");
        let sign_err = err.downcast_ref::<SignError>().expect("SignError in chain");
        match &sign_err.kind {
            SignErrorKind::OidcPreCheckFailed { reason } => {
                // Name the escape hatches, or the refusal is a dead end.
                assert!(
                    reason.contains("--identity-token-stdin") && reason.contains("OCX_IDENTITY_TOKEN"),
                    "the refusal must name the supported alternatives, got: {reason}",
                );
            }
            other => panic!("expected OidcPreCheckFailed, got {other:?}"),
        }
    }

    // ── endpoint pair resolution ────────────────────────────────────────

    /// A rejected endpoint names the flag it came from, and the pair validates
    /// Fulcio first so the answer does not depend on which one is worse.
    ///
    /// Both flags are rejectable here, so a helper that reported whichever it
    /// happened to check second would red this.
    #[test]
    fn the_pair_refuses_fulcio_first_and_names_the_flag() {
        let err = resolve_sigstore_pair(
            None,
            &test_identifier(),
            Some("http://fulcio.evil.example"),
            Some("http://rekor.evil.example"),
        )
        .expect_err("plain http on a non-loopback host is an SSRF risk");
        let parsed = envelope(&err);

        assert_eq!(parsed["exit_code"], 64, "a bad endpoint is a usage error, not a fault");
        assert_eq!(parsed["error"]["detail"], "invalid_endpoint_url");
        let message = parsed["error"]["message"].as_str().expect("message");
        assert!(
            message.contains("--fulcio-url"),
            "the refusal must name the flag it read, and Fulcio is checked first: {message}"
        );
    }

    /// The Rekor half of the same pair, so the test above cannot pass by
    /// hardcoding one flag name.
    #[test]
    fn the_pair_refuses_rekor_and_names_that_flag() {
        let err = resolve_sigstore_pair(None, &test_identifier(), None, Some("file:///etc/passwd"))
            .expect_err("a non-http scheme is refused");
        let message = envelope(&err)["error"]["message"].as_str().expect("message").to_owned();
        assert!(message.contains("--rekor-url"), "wrong flag named: {message}");
    }

    /// Both endpoints resolve to the builtin defaults when nothing is set —
    /// without this, a helper that refused everything would pass the two above.
    #[test]
    fn the_pair_resolves_the_builtin_defaults() {
        let (fulcio, rekor) =
            resolve_sigstore_pair(None, &test_identifier(), None, None).expect("the builtin defaults are valid");
        assert_eq!(fulcio.as_str().trim_end_matches('/'), DEFAULT_FULCIO_URL);
        assert_eq!(
            rekor.as_str().trim_end_matches('/'),
            ocx_lib::oci::endpoint::DEFAULT_REKOR_URL
        );
    }

    /// The verify-side single is a different error family at the same exit
    /// code: `sbom` and `verify` must not report a sign-side kind, and a
    /// bad `[trust.sigstore].fulcio_url` must not fail a command that never
    /// contacts Fulcio.
    #[test]
    fn the_rekor_single_refuses_through_the_verify_family() {
        let broken_fulcio = ocx_lib::trust::SigstoreTrust {
            fulcio_url: Some("file:///etc/passwd".to_string()),
            ..Default::default()
        };
        let ok = resolve_rekor_endpoint(Some(&broken_fulcio), &test_identifier(), None)
            .expect("a Fulcio typo cannot fail a command that never calls Fulcio");
        assert_eq!(
            ok.as_str().trim_end_matches('/'),
            ocx_lib::oci::endpoint::DEFAULT_REKOR_URL
        );

        let err = resolve_rekor_endpoint(None, &test_identifier(), Some("http://rekor.evil.example"))
            .expect_err("plain http on a non-loopback host is an SSRF risk");
        assert!(
            err.downcast_ref::<VerifyError>().is_some(),
            "the verify path must refuse with a VerifyError, not a SignError: {err:#}"
        );
        assert_eq!(envelope(&err)["exit_code"], 64);
    }

    // ── moved from `package_verify.rs` (ARCH-3) ─────────────────────────

    /// A pipeline-stage `VerifyError` wrapped in a `PackageError` (the shape the
    /// verify facade and the auto-verify hook both produce) must still surface
    /// `context.identifier` in the `--format json` envelope.
    ///
    /// This is a regression guard for the `verify_error_into_anyhow` unwrap:
    /// `PackageError` omits `#[source]` on its `kind`, and `Error::Verify` is
    /// `#[error(transparent)]`, so a naïve `anyhow::Error::new(package_error)`
    /// would leave the envelope's chain-walk unable to downcast to the
    /// `VerifyError` node — dropping the identifier. The unwrap re-roots the chain
    /// on the bare `VerifyError` so the identifier survives. If that unwrap
    /// regresses, `context.identifier` vanishes and this test fails.
    #[test]
    fn verify_error_wrapped_in_package_error_still_populates_envelope_identifier() {
        let id = oci::Identifier::parse("registry.example/pkg:1.0").expect("parse identifier");
        let package_error = PackageError::new(
            id.clone(),
            PackageErrorKind::Internal(LibError::Verify(Box::new(VerifyError::new(
                id,
                VerifyErrorKind::IdentityMismatch,
            )))),
        );
        let err = verify_error_into_anyhow(package_error);
        let json = render_error_envelope("package verify", &err).expect("render envelope");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");

        assert_eq!(parsed["exit_code"], 77);
        assert_eq!(parsed["error"]["kind"], "permission_denied");
        assert_eq!(parsed["error"]["detail"], "identity_mismatch");
        assert_eq!(
            parsed["error"]["context"]["identifier"], "registry.example/pkg:1.0",
            "identifier must survive the PackageError wrap → verify_error_into_anyhow unwrap",
        );
    }

    // ── moved from `package_attest.rs` (ARCH-3) ─────────────────────────

    fn wrapped(kind: SignErrorKind) -> PackageError {
        let id = test_identifier();
        PackageError::new(
            id.clone(),
            PackageErrorKind::Internal(LibError::Sign(Box::new(SignError::new(id, kind)))),
        )
    }

    fn envelope(err: &anyhow::Error) -> serde_json::Value {
        let json = render_error_envelope("package attest", err).expect("render envelope");
        serde_json::from_str(&json).expect("valid json")
    }

    /// A pipeline-stage `SignError` wrapped in a `PackageError` (the shape
    /// `attest_one` produces) must still surface `context.identifier` in the
    /// `--format json` envelope.
    ///
    /// This is the regression guard for the `attest_error_into_anyhow` unwrap.
    /// `PackageError` omits `#[source]` on its `kind` and `Error::Sign` is
    /// `#[error(transparent)]`, so a plain `anyhow::Error::new(err.kind)` leaves
    /// the envelope's chain walk unable to downcast to the `SignError` node —
    /// which is the node holding the identifier. Only the unwrap re-roots the
    /// chain on it. If the unwrap regresses, `context.identifier` vanishes here.
    #[test]
    fn attest_error_wrapped_in_package_error_still_populates_envelope_identifier() {
        let err = attest_error_into_anyhow(wrapped(SignErrorKind::OidcTokenRejected));
        let parsed = envelope(&err);

        assert_eq!(parsed["exit_code"], 80);
        assert_eq!(parsed["error"]["kind"], "auth_error");
        assert_eq!(
            parsed["error"]["context"]["identifier"], "registry.example/pkg:1.0",
            "identifier must survive the PackageError wrap -> attest_error_into_anyhow unwrap",
        );
    }

    /// Four attest failure kinds must classify to four different exit codes
    /// through the same wrapper.
    ///
    /// One kind proves nothing about a wrapper: a helper that collapsed every
    /// error to a single code would satisfy any single-kind assertion. These
    /// four are the codes a caller's script branches on, and the slugs are the
    /// frozen `error.detail` vocabulary.
    #[test]
    fn attest_error_kinds_keep_distinct_exit_codes_through_the_wrapper() {
        let cases = [
            (
                SignErrorKind::ProvenanceVersionUnsupported {
                    resolved: "https://slsa.dev/provenance/v0.2".into(),
                },
                64,
                "usage_error",
                "provenance_version_unsupported",
            ),
            (SignErrorKind::PredicateNotJson, 65, "data_error", "predicate_not_json"),
            (
                SignErrorKind::OfflineAttestRefused,
                77,
                "permission_denied",
                "offline_attest_refused",
            ),
            (
                SignErrorKind::OidcTokenRejected,
                80,
                "auth_error",
                "oidc_token_rejected",
            ),
        ];

        let mut seen = std::collections::BTreeSet::new();
        for (kind, exit, category, detail) in cases {
            let parsed = envelope(&attest_error_into_anyhow(wrapped(kind)));
            assert_eq!(parsed["exit_code"], exit, "exit code for {detail}");
            assert_eq!(parsed["error"]["kind"], category, "category for {detail}");
            assert_eq!(parsed["error"]["detail"], detail, "detail slug for {detail}");
            assert!(seen.insert(exit), "exit code {exit} appeared twice");
        }
        assert_eq!(seen.len(), 4, "four kinds must not collapse onto fewer codes");
    }

    /// `failed_outcome` reports the same slug the error envelope would have,
    /// so `push --sbom` and `attest` speak one vocabulary.
    #[test]
    fn failed_outcome_carries_the_envelope_slug() {
        let err = attest_error_into_anyhow(wrapped(SignErrorKind::OfflineAttestRefused));
        let outcome = failed_outcome(&err);
        let json = serde_json::to_value(&outcome).expect("serialize outcome");
        assert_eq!(json["status"], "failed");
        assert_eq!(json["kind"], "offline_attest_refused");
        assert!(
            json["message"].as_str().expect("message").contains("offline"),
            "message should name the refusal, got {}",
            json["message"]
        );
    }

    /// Registry-sourced text in a failure message is neutralized before it can
    /// reach a terminal (CWE-150).
    #[test]
    fn failed_outcome_sanitizes_the_message() {
        let hostile = oci::Identifier::parse("registry.example/pkg:1.0").expect("parse");
        let err = anyhow::Error::from(SignError::new(
            hostile,
            SignErrorKind::TargetNotFound {
                platform: "linux/\u{1b}[31mamd64\u{202e}".into(),
            },
        ));
        let outcome = failed_outcome(&err);
        let json = serde_json::to_value(&outcome).expect("serialize outcome");
        let message = json["message"].as_str().expect("message");
        assert!(!message.contains('\u{1b}'), "escape survived: {message:?}");
        assert!(!message.contains('\u{202e}'), "bidi override survived: {message:?}");
    }

    // ── `--predicate` read ───────────────────────────────────────────────────

    async fn read(path: &Path) -> anyhow::Result<Vec<u8>> {
        read_predicate(path, &test_identifier()).await
    }

    /// The bytes reach the pipeline exactly as they sit on disk.
    ///
    /// Whitespace, key order and number spelling are what gets signed and
    /// hashed, so any normalization here would be invisible corruption. This
    /// layer also does not parse: a non-JSON predicate is the pipeline's
    /// refusal (`predicate_not_json`, 65), not the reader's.
    #[tokio::test]
    async fn predicate_bytes_are_returned_verbatim_and_unparsed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("predicate.json");
        let raw = b"{\n  \"b\": 1.50,\n  \"a\":  2\n}\n";
        tokio::fs::write(&path, raw).await.expect("write predicate");
        assert_eq!(read(&path).await.expect("read"), raw);

        let text = dir.path().join("not-json.txt");
        tokio::fs::write(&text, b"not json at all").await.expect("write");
        assert_eq!(read(&text).await.expect("read"), b"not json at all");
    }

    /// The read is bounded at `MAX_PREDICATE_FILE_BYTES`, at the real constant.
    ///
    /// Both sides of the boundary are asserted from one file: exactly at the
    /// limit succeeds, one byte over is refused with `predicate_too_large`
    /// (exit 65). A limit test that only checks the over case cannot tell a
    /// correct bound from an off-by-one that rejects the legal maximum.
    ///
    /// The bound is enforced while reading, never by a `metadata().len()`
    /// check followed by an unbounded read — the length on disk is not a
    /// promise about how many bytes arrive.
    #[tokio::test]
    async fn predicate_is_bounded_at_the_documented_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.json");

        let over = vec![b'x'; MAX_PREDICATE_FILE_BYTES + 1];
        tokio::fs::write(&path, &over).await.expect("write oversize");
        let Err(err) = read(&path).await else {
            panic!("one byte over the limit must be refused");
        };
        let parsed = envelope(&err);
        assert_eq!(parsed["exit_code"], 65, "envelope was {parsed}");
        assert_eq!(parsed["error"]["detail"], "predicate_too_large");
        // The two numbers are the whole value of this error: without them the
        // user cannot tell a 1-byte overshoot from a 500 MB one. Nothing else
        // pins the count this producer reports.
        let message = parsed["error"]["message"].as_str().expect("message");
        assert!(
            message.contains(&(MAX_PREDICATE_FILE_BYTES + 1).to_string()),
            "message must name the count reading stopped at, got: {message}"
        );
        assert!(
            message.contains(&MAX_PREDICATE_FILE_BYTES.to_string()),
            "message must name the limit, got: {message}"
        );

        tokio::fs::write(&path, &over[..MAX_PREDICATE_FILE_BYTES])
            .await
            .expect("write at-limit");
        assert_eq!(
            read(&path).await.expect("exactly at the limit must be accepted").len(),
            MAX_PREDICATE_FILE_BYTES
        );
    }

    /// A symlink at `--predicate` is refused rather than followed.
    ///
    /// Following one would embed whatever it points at, sign it with the
    /// caller's identity and publish it to an append-only log — not undoable,
    /// which is why the refusal is kept even though a predicate is public data.
    /// The open itself refuses (`O_NOFOLLOW`), so there is no window between
    /// deciding and reading (CWE-367).
    #[cfg(unix)]
    #[tokio::test]
    async fn predicate_symlink_is_refused_not_followed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real.json");
        let link = dir.path().join("link.json");
        tokio::fs::write(&real, b"{\"secret\":true}")
            .await
            .expect("write target");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let Err(err) = read(&link).await else {
            panic!("a symlinked predicate must be refused, not followed");
        };
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("symlink"),
            "the refusal must say what was refused, got: {rendered}"
        );
        assert_eq!(envelope(&err)["exit_code"], 74, "a refused open is an I/O failure");
    }

    /// A predicate path that does not exist is an I/O failure (74), not a
    /// silent empty predicate.
    #[tokio::test]
    async fn missing_predicate_is_an_io_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let Err(err) = read(&dir.path().join("absent.json")).await else {
            panic!("a missing predicate must fail");
        };
        let parsed = envelope(&err);
        assert_eq!(parsed["exit_code"], 74, "envelope was {parsed}");
    }
}

#[cfg(test)]
mod sweep_exit_code_tests {
    //! Which code a partially-failed `--tags` / `--tags-file` sweep returns.

    use super::sweep_exit_code;
    use ocx_lib::cli::ExitCode;

    /// Nothing failed, so nothing is reported as failing. The sweep's own
    /// skips (a tag resolving to a bare manifest) never reach here — they are
    /// not failures.
    #[test]
    fn a_sweep_with_no_failures_succeeds() {
        assert_eq!(sweep_exit_code(&[]), ExitCode::Success);
    }

    /// One fault across twenty tags stays scriptable as that one fault.
    /// Flattening it to `Failure` would throw away the only thing `case $?`
    /// could act on.
    #[test]
    fn failures_that_agree_keep_their_own_code() {
        assert_eq!(sweep_exit_code(&[ExitCode::AuthError]), ExitCode::AuthError);
        assert_eq!(
            sweep_exit_code(&[ExitCode::NotFound, ExitCode::NotFound, ExitCode::NotFound]),
            ExitCode::NotFound,
        );
    }

    /// A mix has no true single answer. Picking the first, or the worst, would
    /// claim a fault class the run did not have; `Failure` is defined as "use
    /// only when no specific code applies", which is exactly this state.
    #[test]
    fn failures_that_disagree_fall_back_to_the_generic_failure() {
        assert_eq!(
            sweep_exit_code(&[ExitCode::NotFound, ExitCode::AuthError]),
            ExitCode::Failure,
        );
        // Order does not decide it: neither the first nor the last wins.
        assert_eq!(
            sweep_exit_code(&[ExitCode::AuthError, ExitCode::NotFound]),
            ExitCode::Failure,
        );
        // Nor does a majority.
        assert_eq!(
            sweep_exit_code(&[ExitCode::NotFound, ExitCode::NotFound, ExitCode::TempFail]),
            ExitCode::Failure,
        );
    }

    /// The slug a leg failure carries is the wire spelling of its category, so
    /// a sweep's rows read in the same vocabulary as an error envelope.
    #[test]
    fn a_leg_failures_slug_is_the_wire_spelling_of_its_category() {
        assert_eq!(super::category_slug(ExitCode::AuthError), "auth_error");
        assert_eq!(
            super::category_slug(ExitCode::ReferrersUnsupported),
            "referrers_unsupported"
        );
        // `Failure` has no category of its own; it rolls up to `internal`,
        // which is what the envelope would print for it too.
        assert_eq!(super::category_slug(ExitCode::Failure), "internal");
    }
}
