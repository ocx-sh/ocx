// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared plumbing for `ocx package sign`, `verify`, `attest`, `sbom` and `push --sbom`.
//!
//! Shared here, never imported command-to-command: the token resolver is security-critical and must not fork.

use ocx_package_manager::Error as PmError;
use ocx_package_manager::Round;
use std::path::Path;

use anyhow::Context as _;
use ocx_oci::endpoint::{Url, validate_sigstore_url};
use ocx_package_manager::error::{PackageError, PackageErrorKind};
use ocx_sign::attest::MAX_PREDICATE_FILE_BYTES;
use ocx_sign::sign::{SignError, SignErrorKind};
use ocx_sign::verify::{TrustRoot, VerifyError, VerifyErrorKind};
use ocx_trust::key_ref::KeyRef;
use ocx_trust::{self, CompiledPolicy, SigstoreAuthority};
use ocx_util::fs::path::FileReference;
use tokio::io::AsyncReadExt;
use zeroize::Zeroizing;

use crate::api::data::signature::{SignatureLegReport, SignatureReport};

/// Refuses a sign-side operation offline with the caller's refusal `kind` (exit 77).
///
/// Call before token resolution, so a refused run never touches a credential.
pub(super) fn refuse_when_offline(
    context: &crate::app::Context,
    identifier: &ocx_oci::PackageRef,
    kind: SignErrorKind,
) -> anyhow::Result<()> {
    if context.is_offline() {
        return Err(anyhow::Error::from(SignError::new(identifier.clone(), kind)));
    }
    Ok(())
}

/// The basename of a credential path for error messages: the full path must not leak to
/// stderr, the JSON envelope or a log sink (CWE-209).
#[cfg(unix)]
fn redacted_token_path(path: &std::path::Path) -> std::path::PathBuf {
    path.file_name()
        .map_or_else(|| std::path::PathBuf::from("<redacted>"), std::path::PathBuf::from)
}

/// Resolves the override OIDC token: `--identity-token-file`, then `--identity-token-stdin`, then
/// `OCX_IDENTITY_TOKEN`; `Ok(None)` falls through to ambient detection. File and stdin are
/// trimmed, or `echo`'s trailing newline poisons the JWT.
///
/// # Errors
///
/// On Unix, exit 77 when the token file is a symlink or not owned by the effective user
/// ([`SignErrorKind::OidcPreCheckFailed`]) or has a group/other bit set
/// ([`SignErrorKind::IdentityTokenFilePermissive`]).
pub(super) async fn resolve_override_token(
    identity_token_file: Option<&Path>,
    identity_token_stdin: bool,
    identifier: &ocx_oci::PackageRef,
) -> anyhow::Result<Option<Zeroizing<String>>> {
    // No ACL validation off Unix: refuse the file rather than silently skip the check.
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
        // One `O_NOFOLLOW` handle for every check and the read, or a target swap wins the race
        // (CWE-367). Only the final component is checked; a symlinked parent is still followed.
        {
            // Context strings omit the path (CWE-209); only `IdentityTokenFilePermissive` keeps it.
            let path_owned = path.to_path_buf();
            let identifier_for_blocking = identifier.clone();
            let join_result = tokio::task::spawn_blocking(move || -> anyhow::Result<std::fs::File> {
                use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
                let std_file = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path_owned)
                    .map_err(|e| {
                        // `ELOOP` is `O_NOFOLLOW` meeting a symlink: a pre-check failure (77).
                        if e.raw_os_error() == Some(libc::ELOOP) {
                            anyhow::Error::from(SignError::new(
                                identifier_for_blocking.clone(),
                                SignErrorKind::OidcPreCheckFailed {
                                    reason: "identity-token-file is a symlink; refuse to follow (CWE-367)".into(),
                                },
                            ))
                        } else {
                            // Typed with the basename only, or a typo exits 1 `internal` instead of 74.
                            anyhow::Error::new(ocx_util::error::FileError::new(redacted_token_path(&path_owned), e))
                                .context("failed to open --identity-token-file")
                        }
                    })?;
                let meta = std_file
                    .metadata()
                    .map_err(|e| ocx_util::error::FileError::new(redacted_token_path(&path_owned), e))
                    .context("failed to stat --identity-token-file")?;
                // CWE-732: another uid's file could be swapped for malicious content even at 0600.
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
                // Asked of the handle: `/dev/zero` (length 0, endless) and a FIFO (blocks) pass the
                // uid and mode checks when the operator owns them.
                if !meta.is_file() {
                    return Err(anyhow::Error::new(ocx_util::error::FileError::new(
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
            // The whole buffer is zeroized, not just the trimmed copy returned below.
            let mut raw = Zeroizing::new(String::new());
            // Not `read_bounded`: it reopens by path (dropping `O_NOFOLLOW`, CWE-367) into an
            // unzeroized `Vec`. `take(cap + 1)` stops the read itself and tells at-cap from over.
            (&mut file)
                .take(MAX_IDENTITY_TOKEN_BYTES + 1)
                .read_to_string(&mut raw)
                .await
                .map_err(|e| ocx_util::error::FileError::new(redacted_token_path(path), e))
                .context("failed to read --identity-token-file")?;
            if raw.len() as u64 > MAX_IDENTITY_TOKEN_BYTES {
                return Err(anyhow::Error::new(ocx_util::error::FileError::new(
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
        // The whole buffer is zeroized, not just the trimmed copy returned below.
        let mut buf = Zeroizing::new(String::new());
        tokio::io::stdin()
            .read_to_string(&mut buf)
            .await
            .context("failed to read identity token from stdin")?;
        return Ok(Some(Zeroizing::new(buf.trim().to_string())));
    }
    // Credential exemption: never passed down to a child ocx. See subsystem-cli.md.
    // A `file://` reference reads a file, which must not stall the runtime.
    let from_env = tokio::task::spawn_blocking(|| ocx_env::OCX_IDENTITY_TOKEN.get())
        .await
        .context("identity-token read task panicked")??;
    Ok(from_env.map(|token| Zeroizing::new(token.into_inner())))
}

/// The largest `--identity-token-file` read; an OIDC token is a few KiB, and 64 KiB matches
/// `MAX_KEY_PEM_BYTES`.
#[cfg(unix)]
const MAX_IDENTITY_TOKEN_BYTES: u64 = 64 * 1024;

/// Default public Fulcio CA endpoint; Rekor's is [`ocx_oci::endpoint::DEFAULT_REKOR_URL`].
pub(crate) const DEFAULT_FULCIO_URL: &str = "https://fulcio.sigstore.dev";

/// Resolves the public instance's Rekor URL for the verifying verbs: flag > the public table > built-in default.
///
/// Unvalidated: a config URL is as attacker-reachable as a flag (`[managed]`), so every caller
/// passes it through [`validate_sigstore_url`](ocx_oci::endpoint::validate_sigstore_url) before dialing.
pub(crate) fn resolve_public_rekor(flag: Option<&str>, configured: Option<&ocx_trust::SigstoreInstances>) -> String {
    resolve_instance_rekor(flag, configured, &SigstoreAuthority::public())
}

/// [`resolve_public_rekor`] for the instance `instance`: flag > that instance's table > built-in default.
fn resolve_instance_rekor(
    flag: Option<&str>,
    configured: Option<&ocx_trust::SigstoreInstances>,
    instance: &SigstoreAuthority,
) -> String {
    let from_config = configured.and_then(|instances| match *instance == SigstoreAuthority::public() {
        true => instances.public().rekor_url,
        false => instances.get(instance).and_then(|table| table.rekor_url.clone()),
    });
    flag.map(str::to_owned)
        .or(from_config)
        .unwrap_or_else(|| ocx_oci::endpoint::DEFAULT_REKOR_URL.to_owned())
}

/// The Sigstore instance a signing run selected and the endpoints it resolves to, before any URL is validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SigningSelection {
    /// The instance whose table supplied the defaults; `None` when no table names the flag's authority.
    pub authority: Option<ocx_trust::SigstoreAuthority>,
    pub fulcio_url: String,
    /// `None` when neither the flag nor the table names one.
    pub rekor_url: Option<String>,
    /// The table's preference for uploading a key-mode signature.
    pub rekor_upload: Option<bool>,
}

/// Selects the Sigstore instance a signing run uses and resolves its endpoints.
///
/// Keyless runs select the table keyed by the authority of `--fulcio-url`, the public instance without the flag.
/// Key mode has no Fulcio: it selects the table whose `rekor_url` has the authority of `--rekor-url`, the public
/// table without the flag. A flag always beats the table's URL. An authority no table names gets no table
/// defaults, so a private stack reached by flag alone never borrows the public Rekor.
pub(crate) fn select_signing_instance(
    configured: Option<&ocx_trust::SigstoreInstances>,
    key_mode: bool,
    fulcio_flag: Option<&str>,
    rekor_flag: Option<&str>,
) -> SigningSelection {
    use ocx_trust::{SigstoreAuthority, SigstoreInstance, SigstoreInstances};

    let none = SigstoreInstances::default();
    let instances = configured.unwrap_or(&none);
    let public = || (SigstoreAuthority::public(), instances.public());
    let rekor_authority_of = |table: &SigstoreInstance| {
        table
            .rekor_url
            .as_deref()
            .and_then(|url| SigstoreAuthority::from_url(url).ok())
    };
    let selected = if key_mode {
        match rekor_flag.and_then(|url| SigstoreAuthority::from_url(url).ok()) {
            None => Some(public()),
            Some(wanted) => Some(public())
                .filter(|(_, table)| rekor_authority_of(table).as_ref() == Some(&wanted))
                .or_else(|| {
                    instances
                        .iter()
                        .find(|(_, table)| rekor_authority_of(table).as_ref() == Some(&wanted))
                        .map(|(authority, table)| (authority.clone(), table.clone()))
                }),
        }
    } else {
        match fulcio_flag {
            None => Some(public()),
            Some(flag) => instances.select_by_fulcio_url(flag),
        }
    };

    let (authority, table) = match selected {
        Some((authority, table)) => (Some(authority), table),
        None => (None, SigstoreInstance::default()),
    };
    let fulcio_default = authority.as_ref().map(|authority| {
        table
            .fulcio_url
            .clone()
            .unwrap_or_else(|| format!("https://{authority}"))
    });
    SigningSelection {
        fulcio_url: fulcio_flag
            .map(str::to_owned)
            .or(fulcio_default)
            .unwrap_or_else(|| DEFAULT_FULCIO_URL.to_owned()),
        rekor_url: rekor_flag.map(str::to_owned).or(table.rekor_url.clone()),
        rekor_upload: table.rekor_upload,
        authority,
    }
}

/// A signing run's validated Fulcio endpoint and its still-optional Rekor endpoint.
#[derive(Debug)]
pub(super) struct SigningTarget {
    fulcio_url: Url,
    rekor_url: Option<Url>,
    authority: Option<ocx_trust::SigstoreAuthority>,
    /// The selected table's `rekor_upload`, the fallback when neither upload flag is given.
    pub configured_rekor_upload: Option<bool>,
}

impl SigningTarget {
    /// The endpoint pair for a run that does or does not upload to the transparency log.
    ///
    /// # Errors
    ///
    /// `sigstore_rekor_url_missing` (exit 78) when the run uploads and neither `--rekor-url` nor the table gives a Rekor.
    pub(super) fn endpoints(self, upload: bool) -> anyhow::Result<(Url, Url)> {
        let rekor = match self.rekor_url {
            Some(url) => url,
            None if upload => {
                return Err(anyhow::Error::from(ocx_config::error::Error::SigstoreRekorUrlMissing {
                    authority: self
                        .authority
                        .map_or_else(|| "(none)".to_owned(), |authority| authority.to_string()),
                }));
            }
            // Never dialled: nothing uploads.
            None => validate_sigstore_url(ocx_oci::endpoint::DEFAULT_REKOR_URL, "--rekor-url")
                .map_err(|reason| anyhow::anyhow!("built-in Rekor URL rejected: {reason:?}"))?,
        };
        Ok((self.fulcio_url, rekor))
    }
}

/// Selects the Sigstore instance ([`select_signing_instance`]) and SSRF-validates (CWE-918) its endpoints, so a
/// self-hosted stack never signs against two different Fulcios. Fulcio is validated first.
///
/// # Errors
///
/// [`SignErrorKind::InvalidEndpointUrl`] (exit 64) naming the rejected flag.
pub(super) fn resolve_sigstore_pair(
    configured: Option<&ocx_trust::SigstoreInstances>,
    identifier: &ocx_oci::PackageRef,
    key_mode: bool,
    fulcio_flag: Option<&str>,
    rekor_flag: Option<&str>,
) -> anyhow::Result<SigningTarget> {
    let selection = select_signing_instance(configured, key_mode, fulcio_flag, rekor_flag);
    let fulcio_url = validate_sigstore_url(&selection.fulcio_url, "--fulcio-url")
        .map_err(invalid_endpoint(identifier, "--fulcio-url"))?;
    let rekor_url = selection
        .rekor_url
        .as_deref()
        .map(|rekor| validate_sigstore_url(rekor, "--rekor-url").map_err(invalid_endpoint(identifier, "--rekor-url")))
        .transpose()?;
    Ok(SigningTarget {
        fulcio_url,
        rekor_url,
        authority: selection.authority,
        configured_rekor_upload: selection.rekor_upload,
    })
}

/// Resolves and SSRF-validates the Rekor endpoint for verifying verbs.
///
/// Apart from the pair, or a `the Sigstore instance table.fulcio_url` typo fails `verify` and `sbom`.
///
/// # Errors
///
/// [`VerifyErrorKind::InvalidEndpointUrl`] (exit 64) naming `--rekor-url`.
pub(super) fn resolve_rekor_endpoint(
    configured: Option<&ocx_trust::SigstoreInstances>,
    identifier: &ocx_oci::PackageRef,
    rekor_flag: Option<&str>,
    instance: &SigstoreAuthority,
) -> anyhow::Result<Url> {
    let rekor = resolve_instance_rekor(rekor_flag, configured, instance);
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

/// A Rekor `integratedTime` (UTC epoch seconds) as a wire timestamp; `None` when out of range.
pub(super) fn signed_at(epoch_secs: u64) -> Option<ocx_util::time::Timestamp> {
    let timestamp = i64::try_from(epoch_secs)
        .ok()
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .map(ocx_util::time::Timestamp::from);
    // `None` reads as "no transparency record", so an unrepresentable time must not pass silently.
    if timestamp.is_none() {
        log::warn!("Rekor integratedTime {epoch_secs} is out of range; reporting no signed_at");
    }
    timestamp
}

/// The single policy set a signer flag names: `--key`, else the `--certificate-*` pair.
///
/// # Errors
///
/// 74 for an unreadable signer `key` file, a [`VerifyErrorKind`] for an uncompilable key.
fn flag_policies(
    identifier: &ocx_oci::PackageRef,
    certificate_identity: Option<&str>,
    certificate_oidc_issuer: Option<&str>,
    key: Option<&KeyRef>,
    instance: &SigstoreAuthority,
) -> anyhow::Result<Vec<CompiledPolicy>> {
    // Clap refuses a key beside the certificate flags, so this order never drops a flag pair.
    if let Some(key) = key {
        let policy = ocx_trust::compile_key_signer(key)
            .map_err(|kind| VerifyError::new(identifier.clone(), VerifyErrorKind::from(kind)))?;
        return Ok(vec![policy]);
    }
    let (Some(identity), Some(issuer)) = (certificate_identity, certificate_oidc_issuer) else {
        return Ok(Vec::new());
    };
    Ok(vec![CompiledPolicy::exact_on(
        identity.to_owned(),
        issuer.to_owned(),
        instance.clone(),
    )])
}

/// Every signer round a verifying command must pass, in order; empty when nothing covers the package.
///
/// A signer flag replaces every round. Without one the identifier is resolved first, because the resolve may
/// commit or memo the index root, and only then are the declared publishers read.
///
/// # Errors
///
/// The resolve's own failure, the refusals of `ocx_package_manager::signer_rounds` (77/81/82/65 as classified there), 78 for a malformed
/// matched operator policy, 74 for an unreadable signer `key` file.
pub(super) async fn resolve_rounds(
    context: &crate::app::Context,
    identifier: &ocx_oci::PackageRef,
    certificate_identity: Option<&str>,
    certificate_oidc_issuer: Option<&str>,
    key: Option<&KeyRef>,
    instance: &SigstoreAuthority,
) -> anyhow::Result<Vec<Round>> {
    if key.is_some() || certificate_identity.is_some() {
        let flags = flag_policies(identifier, certificate_identity, certificate_oidc_issuer, key, instance)?;
        return Ok(vec![Round::flags(flags)]);
    }

    // The resolve commits or memoizes the index root the publisher read depends on; a failure here must refuse,
    // since a fresh home would otherwise read "no root" as "no publishers declared" and skip the round.
    context
        .manager()
        .index()
        .fetch_manifest(identifier, ocx_index::IndexOperation::Resolve)
        .await?;

    let project_policies = project_trust_policies(context, identifier).await?;
    let decision = ocx_package_manager::signer_rounds(ocx_package_manager::SignerRoundsInput {
        logical: identifier,
        index: context.manager().index(),
        config: context.config(),
        operator_policies: context.config_trust_policies(),
        project_policies: &project_policies,
    })
    .await?;
    match decision {
        ocx_package_manager::RoundsDecision::Uncovered => Ok(Vec::new()),
        ocx_package_manager::RoundsDecision::Refused(refusal) => Err(refusal.into()),
        ocx_package_manager::RoundsDecision::Covered(rounds) => Ok(rounds.ordered()),
    }
}

/// Whether any round holds a keyless backend, i.e. whether a Sigstore trust root is needed at all.
pub(super) fn rounds_need_trust_root(rounds: &[Round]) -> bool {
    rounds
        .iter()
        .flat_map(|round| &round.policies)
        .flat_map(|policy| &policy.backends)
        .any(|backend| matches!(backend, ocx_trust::PolicyBackend::Keyless(_)))
}

/// The Sigstore instances the keyless entries of `rounds` name, public first; `selected` alone when none does.
///
/// A policy-mode run judges each entry against its own instance's root, one pass per instance.
pub(super) fn round_instances(rounds: &[Round], selected: &SigstoreAuthority) -> Vec<SigstoreAuthority> {
    let named: std::collections::BTreeSet<SigstoreAuthority> = rounds
        .iter()
        .flat_map(|round| &round.policies)
        .flat_map(|policy| &policy.backends)
        .filter_map(|backend| match backend {
            ocx_trust::PolicyBackend::Keyless(keyless) => Some(keyless.instance.clone()),
            ocx_trust::PolicyBackend::Key(_) => None,
        })
        .collect();
    if named.is_empty() {
        vec![selected.clone()]
    } else {
        named.into_iter().collect()
    }
}

/// The in-effect project's `[[trust.policy]]` set, empty without a project file: the one `ocx package`
/// read of `ocx.toml` (`adr_trust_policy.md`).
async fn project_trust_policies(
    context: &crate::app::Context,
    identifier: &ocx_oci::PackageRef,
) -> anyhow::Result<Vec<ocx_trust::TrustPolicy>> {
    // A missing CWD is non-fatal: `--project`/`OCX_PROJECT` still resolve.
    let cwd = std::env::current_dir().ok();
    let ocx_home = context.file_structure().root();
    let resolved =
        ocx_project::ProjectConfig::resolve(cwd.as_deref(), context.project_path(), Some(ocx_home), context.global())
            .await?;
    match resolved {
        Some((config_path, _lock_path)) => {
            // Trust-only parse, or a malformed unrelated section fails verify.
            let text = tokio::fs::read_to_string(&config_path)
                .await
                .map_err(|error| ocx_util::error::FileError::new(&config_path, error))
                .with_context(|| format!("reading project config `{}` for trust policies", config_path.display()))?;
            // Anchored on the project file's directory, or a relative key path depends on the CWD.
            let config_dir = config_path.parent().unwrap_or_else(|| std::path::Path::new("."));
            // Mapped, or a malformed `ocx.toml` exits 1 `internal`: `TrustPolicyError` has no classify rung.
            ocx_trust::policies_from_ocx_toml(&text, config_dir)
                .map_err(|kind| VerifyError::new(identifier.clone(), VerifyErrorKind::TrustPolicyInvalid(kind)).into())
        }
        None => Ok(Vec::new()),
    }
}

/// Reads an explicit trust-root override, a bare path or a `file://` URL.
///
/// [`FileReference::as_written`], not `anchored_at`: the value is relative to the process CWD.
/// A non-UTF-8 path passes through untouched.
pub(crate) fn explicit_trust_root_path(value: std::path::PathBuf) -> std::path::PathBuf {
    match value.to_str() {
        Some(written) => FileReference::parse(written).as_written().to_path_buf(),
        None => value,
    }
}

/// The operator-named trust-root override for the selected instance: `--sigstore-trusted-root`, then
/// `OCX_SIGSTORE_TRUSTED_ROOT`. Only the selected instance's pass calls this; every other pass hands the
/// resolvers `None`, so the override never reaches a private instance's own root.
pub(super) fn selected_trust_root_override(trusted_root: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    trusted_root
        .map(std::path::Path::to_path_buf)
        .or_else(|| {
            ocx_env::OCX_SIGSTORE_TRUSTED_ROOT
                .get_raw()
                .map(std::path::PathBuf::from)
        })
        .map(explicit_trust_root_path)
}

/// Resolves the trust root: the already-resolved `trusted_root` override (see [`selected_trust_root_override`]) on top of
/// `ocx_sign::verify::resolve_trust_root`'s ladder, which auto-verify shares.
pub(super) async fn resolve_trust_root(
    context: &crate::app::Context,
    identifier: &ocx_oci::PackageRef,
    instance: &SigstoreAuthority,
    rekor_cache_key: &str,
    offline: bool,
    trusted_root: Option<&std::path::Path>,
) -> anyhow::Result<TrustRoot> {
    let home_trusted_root = ocx_config::loader::ConfigLoader::home_sigstore_trusted_root_path_for(instance);
    ocx_sign::verify::resolve_instance_trust_root(
        instance,
        trusted_root,
        context.config_trust_sigstore(),
        home_trusted_root.as_deref(),
        &ocx_sign::sign::state::SigningStatePaths::new(context.file_structure().state.root()),
        rekor_cache_key,
        offline,
    )
    .await
    .map_err(|kind| VerifyError::new(identifier.clone(), kind).into())
}

/// The trust root and transparency treatment for `rounds`: the full root where a keyless candidate needs
/// one, else whatever is already local with log evidence checked only where that root can check it.
///
/// Boxed at the call site: inlined, its future widens every command's, and the root `execute` overflows a test
/// thread's stack in a debug build.
pub(super) async fn trust_for_rounds(
    context: &crate::app::Context,
    identifier: &ocx_oci::PackageRef,
    rounds: &[Round],
    instance: &SigstoreAuthority,
    rekor_cache_key: &str,
    offline: bool,
    trusted_root: Option<&std::path::Path>,
) -> anyhow::Result<(TrustRoot, ocx_sign::verify::Transparency)> {
    if rounds_need_trust_root(rounds) {
        let root = resolve_trust_root(context, identifier, instance, rekor_cache_key, offline, trusted_root).await?;
        Ok((root, ocx_sign::verify::Transparency::Verify))
    } else {
        let root =
            resolve_local_trust_root_or_empty(context, identifier, instance, rekor_cache_key, trusted_root).await?;
        Ok((root, ocx_sign::verify::Transparency::VerifyIfRootAvailable))
    }
}

/// Reads an operator-named trust root (flag or `OCX_SIGSTORE_TRUSTED_ROOT`) before the identity gate, or an
/// unreadable path exits 64 `no trusted identity` instead of 74 naming the path. The default root stays lazy.
pub(super) async fn validate_explicit_trust_root(
    context: &crate::app::Context,
    identifier: &ocx_oci::PackageRef,
    instance: &SigstoreAuthority,
    rekor_cache_key: &str,
    trusted_root: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    if trusted_root.is_none() {
        return Ok(());
    }
    resolve_local_trust_root_or_empty(context, identifier, instance, rekor_cache_key, trusted_root)
        .await
        .map(drop)
}

/// The trust root a key-only run uses: one already local with no network, else an empty one, so a key
/// candidate's log entry is checked only where it can be and no Sigstore request is made.
pub(super) async fn resolve_local_trust_root_or_empty(
    context: &crate::app::Context,
    identifier: &ocx_oci::PackageRef,
    instance: &SigstoreAuthority,
    rekor_cache_key: &str,
    trusted_root: Option<&std::path::Path>,
) -> anyhow::Result<TrustRoot> {
    let home_trusted_root = ocx_config::loader::ConfigLoader::home_sigstore_trusted_root_path_for(instance);
    let local = ocx_sign::verify::resolve_local_instance_trust_root(
        instance,
        trusted_root,
        context.config_trust_sigstore(),
        home_trusted_root.as_deref(),
        &ocx_sign::sign::state::SigningStatePaths::new(context.file_structure().state.root()),
        rekor_cache_key,
    )
    .await
    .map_err(|kind| anyhow::Error::from(VerifyError::new(identifier.clone(), kind)))?;
    Ok(local.unwrap_or_default())
}

/// Unwraps the inner [`VerifyError`], or the JSON envelope's `context.identifier` is empty:
/// `Error::Verify` is `#[error(transparent)]`, so its `source()` skips that node.
pub(super) fn verify_error_into_anyhow(err: PackageError) -> anyhow::Error {
    match err.kind {
        PackageErrorKind::Internal(PmError::Verify(verify_error)) => anyhow::Error::new(*verify_error),
        kind => anyhow::Error::new(kind),
    }
}

/// The `SignError` for a rejected Sigstore endpoint URL (exit 64).
fn invalid_endpoint(
    identifier: &ocx_oci::PackageRef,
    flag: &'static str,
) -> impl Fn(ocx_oci::endpoint::UrlRejection) -> anyhow::Error {
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

/// Reads the `--predicate` file, bounded and refusing a symlink at the named path.
///
/// # Errors
///
/// [`SignErrorKind::PredicateTooLarge`] (exit 65) past the limit; an I/O error
/// (exit 74) naming the path otherwise, including the symlink refusal.
pub(super) async fn read_predicate(path: &Path, identifier: &ocx_oci::PackageRef) -> anyhow::Result<Vec<u8>> {
    let file = open_predicate(path).await?;

    // Bounded while reading, never sized from `metadata().len()`, which the attacker chooses;
    // one byte past the ceiling tells at-limit from over.
    let ceiling = u64::try_from(MAX_PREDICATE_FILE_BYTES)
        .expect("MAX_PREDICATE_FILE_BYTES is a compile-time constant well under u64::MAX")
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(ceiling)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| ocx_util::error::FileError::new(path, error))?;

    if bytes.len() > MAX_PREDICATE_FILE_BYTES {
        return Err(anyhow::Error::from(SignError::new(
            identifier.clone(),
            SignErrorKind::PredicateTooLarge {
                limit: ceiling.saturating_sub(1),
                // What was counted, not the on-disk size: the read stops at the ceiling.
                actual: ceiling,
            },
        )));
    }
    Ok(bytes)
}

/// Opens the predicate file, refusing a symlink at `open(2)`, or the link target is signed into an
/// append-only public log; a prior `symlink_metadata` would leave a swap window (CWE-367).
///
/// No ownership or mode gate, unlike `--identity-token-file`: a predicate is public, and a 0644
/// SBOM from an earlier CI step is the ordinary case.
async fn open_predicate(path: &Path) -> anyhow::Result<tokio::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        let owned = path.to_path_buf();
        let opened = tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&owned)
        })
        .await
        .map_err(|join| ocx_util::error::FileError::new(path, std::io::Error::other(join)))?;

        match opened {
            Ok(file) => Ok(tokio::fs::File::from_std(file)),
            // `ELOOP` is `O_NOFOLLOW` meeting a symlink: reported as the refusal, not an I/O fault.
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                Err(anyhow::Error::from(ocx_util::error::FileError::new(path, error))
                    .context("refusing to read a predicate through a symlink"))
            }
            Err(error) => Err(ocx_util::error::FileError::new(path, error).into()),
        }
    }
    #[cfg(not(unix))]
    {
        // No `O_NOFOLLOW` equivalent on Windows: the symlink refusal is Unix-only.
        tokio::fs::File::open(path)
            .await
            .map_err(|error| ocx_util::error::FileError::new(path, error).into())
    }
}

/// The slug the JSON error envelope would give `err`: `error.detail`, else `error.kind`.
///
/// Lifted from the rendered envelope, never re-derived, or a report and the envelope spell one
/// failure two ways.
pub(super) fn error_slug(command: &str, err: &anyhow::Error) -> String {
    crate::error_document::render_error_document(command, err)
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

/// The `error.detail` slug of a failed leg: its run returned `Ok`, so no envelope exists to lift it from.
///
/// The detail, not the category, because the category flattens `referrers_unsupported` (and every other
/// capability gap) to `unsupported`; an `Internal` leg answers with its wrapped cause's slug, as a
/// whole-run failure does.
pub(super) fn leg_slug(kind: &ocx_sign::sign::SignErrorKind) -> String {
    crate::exit::detail_slug(ocx_exit::ClassifyErrorKind::kind_detail(kind)).to_owned()
}

/// A `--tags` sweep's exit code: `Success` with no failure, the shared code when every failure
/// agrees, else `Failure` — never the first or worst, which claims a fault class the run did not have.
pub(super) fn sweep_exit_code(failures: &[ocx_exit::ExitCode]) -> ocx_exit::ExitCode {
    let mut codes = failures.iter();
    let Some(first) = codes.next() else {
        return ocx_exit::ExitCode::Success;
    };
    match codes.all(|code| code == first) {
        true => *first,
        false => ocx_exit::ExitCode::Failure,
    }
}

/// Unwraps the inner [`SignError`], or the JSON envelope's `context.identifier` is empty:
/// `Error::Sign` is `#[error(transparent)]`, so its `source()` skips that node.
pub(super) fn attest_error_into_anyhow(err: PackageError) -> anyhow::Error {
    match err.kind {
        PackageErrorKind::Internal(PmError::Sign(sign_error)) => anyhow::Error::new(*sign_error),
        kind => anyhow::Error::new(kind),
    }
}

/// The per-reference report; a swept tag's row carries the same document a single run prints.
pub(super) fn signature_report(
    identifier: &ocx_oci::PackageRef,
    platform: Option<&ocx_oci::Platform>,
    result: ocx_sign::sign::SignResult,
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
        identifier.clone(),
        result.subject_digest,
        legs,
        platform,
        result.certificate_identity,
        result.certificate_oidc_issuer,
    )
    .with_key_model(result.key_backend, result.public_key_hint)
    .with_transparency_log(result.transparency_log_index)
}

/// The exit code for one failed leg, walking an `Internal` cause like a whole-run failure does,
/// or a 503 exits 1 on one leg and 75 on the whole run.
pub(super) fn leg_exit_code(kind: &ocx_sign::sign::SignErrorKind) -> ocx_exit::ExitCode {
    match kind {
        ocx_sign::sign::SignErrorKind::Internal(cause) => crate::exit::classify_library_error(cause.as_ref()),
        // Every other kind decides its own code; `Failure` is only reachable for a kind that defers.
        other => ocx_exit::ClassifyExitCode::classify(other).unwrap_or(ocx_exit::ExitCode::Failure),
    }
}

#[cfg(test)]
mod leg_exit_code_tests {
    use super::leg_exit_code;
    use ocx_exit::ExitCode;
    use ocx_oci::client::error::ClientError;
    use ocx_sign::sign::SignErrorKind;

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
            ExitCode::Unsupported
        );
    }
}

#[cfg(test)]
mod tests {

    //! Unit tests for [`resolve_override_token`] (C-S1-4),
    //! [`resolve_public_rekor`], and the
    //! attest-path error contract and `--predicate` read that moved here with
    //! their helpers.

    use super::*;
    use crate::error_document::render_error_document;

    /// C-083 — both explicit doors (`--sigstore-trusted-root`,
    /// `OCX_SIGSTORE_TRUSTED_ROOT`) take the two spellings
    /// `the Sigstore instance table trusted_root` reads, and nothing narrows: every bare
    /// path that works today round-trips byte for byte.
    #[test]
    fn explicit_trust_root_takes_a_bare_path_or_a_file_url() {
        for (written, expected) in [
            ("/opt/sigstore/trusted-root.json", "/opt/sigstore/trusted-root.json"),
            ("sigstore/trusted-root.json", "sigstore/trusted-root.json"),
            (
                "file:///opt/sigstore/trusted-root.json",
                "/opt/sigstore/trusted-root.json",
            ),
            // Not anchored, unlike the config field: a value typed at a shell
            // means what it means from the process working directory.
            ("file://sigstore/trusted-root.json", "sigstore/trusted-root.json"),
            // `file:` with a single colon is not a spelling anywhere in OCX.
            (
                "file:/opt/sigstore/trusted-root.json",
                "file:/opt/sigstore/trusted-root.json",
            ),
        ] {
            assert_eq!(
                explicit_trust_root_path(std::path::PathBuf::from(written)),
                std::path::PathBuf::from(expected),
                "{written}"
            );
        }
    }

    /// A the Sigstore instance table block pinning both endpoints at a self-hosted stack.
    fn self_hosted() -> ocx_trust::SigstoreInstances {
        ocx_trust::SigstoreInstances::with_public(ocx_trust::SigstoreInstance {
            fulcio_url: Some("https://fulcio.corp.example".to_string()),
            rekor_url: Some("https://rekor.corp.example".to_string()),
            ..Default::default()
        })
    }

    /// The Rekor precedence: built-in when nothing is set, the public table over the built-in, the flag over both.
    #[test]
    fn the_public_rekor_resolves_flag_over_config_over_builtin() {
        let cfg = self_hosted();
        assert_eq!(resolve_public_rekor(None, None), ocx_oci::endpoint::DEFAULT_REKOR_URL);
        assert_eq!(resolve_public_rekor(None, Some(&cfg)), "https://rekor.corp.example");
        assert_eq!(
            resolve_public_rekor(Some("https://rekor.flag.example"), Some(&cfg)),
            "https://rekor.flag.example"
        );
    }

    /// A named instance reads its own table; the public table never answers for it.
    #[test]
    fn a_named_instance_resolves_its_own_rekor() {
        let corp = SigstoreAuthority::parse("fulcio.corp.example").expect("an authority");
        let tables: ocx_trust::SigstoreInstances = serde_json::from_value(
            serde_json::json!({ "fulcio.corp.example": { "rekor_url": "https://rekor.other.example" } }),
        )
        .expect("a table set");
        assert_eq!(
            resolve_instance_rekor(None, Some(&tables), &corp),
            "https://rekor.other.example"
        );
        assert_eq!(
            resolve_instance_rekor(None, Some(&self_hosted()), &corp),
            ocx_oci::endpoint::DEFAULT_REKOR_URL,
            "the public table is not the named instance's"
        );
    }

    /// The identity flags build an entry on the instance `--sigstore` names, the public one without it.
    #[test]
    fn the_flag_entry_carries_the_named_instance() {
        let corp = SigstoreAuthority::parse("fulcio.corp.example").expect("an authority");
        for (instance, expected) in [
            (&corp, &corp),
            (&SigstoreAuthority::public(), &SigstoreAuthority::public()),
        ] {
            let policies = flag_policies(
                &test_identifier(),
                Some("me"),
                Some("https://issuer.example"),
                None,
                instance,
            )
            .expect("flags compile");
            let [policy] = policies.as_slice() else {
                panic!("one policy");
            };
            let [ocx_trust::PolicyBackend::Keyless(keyless)] = policy.backends.as_slice() else {
                panic!("one keyless backend");
            };
            assert_eq!(&keyless.instance, expected);
        }
    }

    /// A table that sets no Rekor leaves the public one.
    #[test]
    fn a_table_without_a_rekor_leaves_the_builtin_rekor() {
        let cfg = ocx_trust::SigstoreInstances::with_public(ocx_trust::SigstoreInstance {
            fulcio_url: Some("https://fulcio.sigstore.dev".to_string()),
            ..Default::default()
        });
        assert_eq!(
            resolve_public_rekor(None, Some(&cfg)),
            ocx_oci::endpoint::DEFAULT_REKOR_URL
        );
    }

    /// A config-sourced URL is exactly as untrusted as a flag-sourced one: a
    /// `[managed]` payload can carry one, so it must hit the same SSRF guard
    /// and be refused identically (CWE-918).
    ///
    /// The positive half is load-bearing: a validator that refused *everything*
    /// would otherwise pass the negative half unnoticed.
    #[test]
    fn a_forbidden_config_url_is_refused_exactly_like_a_forbidden_flag_url() {
        use ocx_oci::endpoint::validate_sigstore_url;

        let forbidden = "http://rekor.corp.example";
        let hostile = ocx_trust::SigstoreInstances::with_public(ocx_trust::SigstoreInstance {
            rekor_url: Some(forbidden.to_string()),
            ..Default::default()
        });

        let via_config = resolve_public_rekor(None, Some(&hostile));
        let via_flag = resolve_public_rekor(Some(forbidden), None);
        assert_eq!(via_config, via_flag, "both tiers must yield the same URL to validate");
        assert!(validate_sigstore_url(&via_config, "--rekor-url").is_err());
        assert!(validate_sigstore_url(&via_flag, "--rekor-url").is_err());

        let allowed = resolve_public_rekor(None, Some(&self_hosted()));
        assert!(validate_sigstore_url(&allowed, "--rekor-url").is_ok());
    }

    fn test_identifier() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::parse("registry.example/pkg:1.0").expect("static parse")
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

    /// An unresolvable `OCX_IDENTITY_TOKEN` reference fails; it is neither unset nor a
    /// fall-through to the ambient token that is also present.
    #[tokio::test]
    async fn an_unresolvable_identity_token_reference_fails_instead_of_reading_as_unset() {
        let env = ocx_env::overrides::lock();
        let dir = tempfile::tempdir().expect("tmpdir");
        env.set(
            ocx_env::OCX_IDENTITY_TOKEN.declaration(),
            format!("file://{}", dir.path().join("absent").display()),
        );
        env.set(ocx_env::SIGSTORE_ID_TOKEN.declaration(), "ambient-token");
        let id = test_identifier();
        let err = resolve_override_token(None, false, &id)
            .await
            .expect_err("an unresolvable reference must fail, not fall through");
        let reference = err
            .downcast_ref::<ocx_env::ReferenceError>()
            .expect("ReferenceError in chain");
        assert!(reference.to_string().contains("OCX_IDENTITY_TOKEN"), "{reference}");
    }

    /// A readable reference yields the token; unset yields `None`.
    #[tokio::test]
    async fn an_identity_token_file_reference_resolves_and_unset_falls_through() {
        let env = ocx_env::overrides::lock();
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("tok");
        std::fs::write(&path, "tok123\n").expect("write");
        let id = test_identifier();
        env.remove(ocx_env::OCX_IDENTITY_TOKEN.declaration());
        assert!(resolve_override_token(None, false, &id).await.expect("unset").is_none());
        env.set(
            ocx_env::OCX_IDENTITY_TOKEN.declaration(),
            format!("file://{}", path.display()),
        );
        let token = resolve_override_token(None, false, &id)
            .await
            .expect("readable")
            .expect("set");
        assert_eq!(token.as_str(), "tok123");
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
            false,
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
        let err = resolve_sigstore_pair(None, &test_identifier(), false, None, Some("file:///etc/passwd"))
            .expect_err("a non-http scheme is refused");
        let message = envelope(&err)["error"]["message"].as_str().expect("message").to_owned();
        assert!(message.contains("--rekor-url"), "wrong flag named: {message}");
    }

    /// Both endpoints resolve to the builtin defaults when nothing is set —
    /// without this, a helper that refused everything would pass the two above.
    #[test]
    fn the_pair_resolves_the_builtin_defaults() {
        let (fulcio, rekor) = resolve_sigstore_pair(None, &test_identifier(), false, None, None)
            .expect("the builtin defaults are valid")
            .endpoints(true)
            .expect("the public table has a Rekor");
        assert_eq!(fulcio.as_str().trim_end_matches('/'), DEFAULT_FULCIO_URL);
        assert_eq!(
            rekor.as_str().trim_end_matches('/'),
            ocx_oci::endpoint::DEFAULT_REKOR_URL
        );
    }

    /// A private stack's table, keyed by the authority of its Fulcio.
    fn private_stack() -> ocx_trust::SigstoreInstances {
        toml::from_str(
            "[\"localhost:5555\"]\nfulcio_url = \"http://localhost:5555\"\n\
             rekor_url = \"http://localhost:3000\"\nrekor_upload = true\n",
        )
        .expect("a private table")
    }

    fn selected(
        configured: Option<&ocx_trust::SigstoreInstances>,
        key_mode: bool,
        fulcio: Option<&str>,
        rekor: Option<&str>,
    ) -> SigningSelection {
        select_signing_instance(configured, key_mode, fulcio, rekor)
    }

    #[test]
    fn keyless_without_a_fulcio_flag_signs_on_the_public_instance() {
        let pick = selected(None, false, None, None);
        assert_eq!(pick.authority, Some(ocx_trust::SigstoreAuthority::public()));
        assert_eq!(pick.fulcio_url, ocx_trust::instance::PUBLIC_FULCIO_URL);
        assert_eq!(pick.rekor_url.as_deref(), Some(ocx_trust::instance::PUBLIC_REKOR_URL));
        // A private table is never picked without its flag.
        let pick = selected(Some(&private_stack()), false, None, None);
        assert_eq!(pick.authority, Some(ocx_trust::SigstoreAuthority::public()));
    }

    #[test]
    fn keyless_with_a_fulcio_flag_takes_the_table_of_that_authority() {
        let pick = selected(Some(&private_stack()), false, Some("http://localhost:5555"), None);
        assert_eq!(pick.authority.expect("table").as_str(), "localhost:5555");
        assert_eq!(pick.fulcio_url, "http://localhost:5555");
        assert_eq!(pick.rekor_url.as_deref(), Some("http://localhost:3000"));
        assert_eq!(pick.rekor_upload, Some(true));
    }

    #[test]
    fn a_rekor_flag_beats_the_selected_tables_rekor() {
        let pick = selected(
            Some(&private_stack()),
            false,
            Some("http://localhost:5555"),
            Some("http://localhost:4000"),
        );
        assert_eq!(pick.rekor_url.as_deref(), Some("http://localhost:4000"));
    }

    #[test]
    fn keyless_with_a_fulcio_flag_no_table_names_gets_no_rekor_default() {
        let pick = selected(
            Some(&private_stack()),
            false,
            Some("https://fulcio.other.example"),
            None,
        );
        assert_eq!(pick.authority, None);
        assert_eq!(pick.fulcio_url, "https://fulcio.other.example");
        assert_eq!(pick.rekor_url, None, "must not fall back to the public Rekor");
        assert_eq!(pick.rekor_upload, None);
        let pick = selected(
            None,
            false,
            Some("https://fulcio.other.example"),
            Some("https://rekor.other.example"),
        );
        assert_eq!(pick.rekor_url.as_deref(), Some("https://rekor.other.example"));
    }

    #[test]
    fn the_public_fulcio_flag_selects_the_public_table_even_when_none_is_declared() {
        let pick = selected(None, false, Some("https://fulcio.sigstore.dev/"), None);
        assert_eq!(pick.authority, Some(ocx_trust::SigstoreAuthority::public()));
        assert_eq!(pick.rekor_url.as_deref(), Some(ocx_trust::instance::PUBLIC_REKOR_URL));
    }

    #[test]
    fn key_mode_without_a_rekor_flag_uses_the_public_table_and_its_rekor_upload() {
        let mut configured = ocx_trust::SigstoreInstances::with_public(ocx_trust::SigstoreInstance {
            rekor_upload: Some(true),
            ..Default::default()
        });
        configured.merge(private_stack());
        let pick = selected(Some(&configured), true, None, None);
        assert_eq!(pick.authority, Some(ocx_trust::SigstoreAuthority::public()));
        assert_eq!(pick.rekor_upload, Some(true));
        assert_eq!(pick.rekor_url.as_deref(), Some(ocx_trust::instance::PUBLIC_REKOR_URL));
    }

    #[test]
    fn key_mode_selects_the_table_whose_rekor_authority_the_flag_names() {
        let pick = selected(Some(&private_stack()), true, None, Some("http://localhost:3000"));
        assert_eq!(pick.authority.expect("table").as_str(), "localhost:5555");
        assert_eq!(pick.rekor_upload, Some(true));
        // Another authority on the same host is another instance.
        let pick = selected(Some(&private_stack()), true, None, Some("http://localhost:3001"));
        assert_eq!(pick.authority, None);
        assert_eq!(pick.rekor_upload, None);
        assert_eq!(pick.rekor_url.as_deref(), Some("http://localhost:3001"));
    }

    #[test]
    fn key_mode_with_the_public_rekor_flag_selects_the_public_table() {
        let pick = selected(None, true, None, Some("https://rekor.sigstore.dev"));
        assert_eq!(pick.authority, Some(ocx_trust::SigstoreAuthority::public()));
    }

    /// A key-mode fulcio flag is clap's refusal, so the selector never reads it for the choice.
    #[test]
    fn key_mode_ignores_the_fulcio_flag_for_selection() {
        let pick = selected(Some(&private_stack()), true, Some("http://localhost:5555"), None);
        assert_eq!(pick.authority, Some(ocx_trust::SigstoreAuthority::public()));
    }

    #[test]
    fn an_upload_with_no_rekor_url_refuses_with_the_table_named_and_no_upload_does_not() {
        let target = resolve_sigstore_pair(
            None,
            &test_identifier(),
            false,
            Some("https://fulcio.other.example"),
            None,
        )
        .expect("endpoints validate");
        let error = target.endpoints(true).expect_err("an upload has nowhere to go");
        assert!(
            matches!(
                error.downcast_ref::<ocx_config::error::Error>(),
                Some(ocx_config::error::Error::SigstoreRekorUrlMissing { .. })
            ),
            "{error:#}"
        );
        let target = resolve_sigstore_pair(
            None,
            &test_identifier(),
            true,
            None,
            Some("https://rekor.other.example"),
        )
        .expect("endpoints validate");
        assert!(target.endpoints(false).is_ok());
    }

    /// The verify-side single is a different error family at the same exit
    /// code: `sbom` and `verify` must not report a sign-side kind, and a
    /// bad `the Sigstore instance table.fulcio_url` must not fail a command that never
    /// contacts Fulcio.
    #[test]
    fn the_rekor_single_refuses_through_the_verify_family() {
        let broken_fulcio = ocx_trust::SigstoreInstances::with_public(ocx_trust::SigstoreInstance {
            fulcio_url: Some("file:///etc/passwd".to_string()),
            ..Default::default()
        });
        let ok = resolve_rekor_endpoint(
            Some(&broken_fulcio),
            &test_identifier(),
            None,
            &SigstoreAuthority::public(),
        )
        .expect("a Fulcio typo cannot fail a command that never calls Fulcio");
        assert_eq!(ok.as_str().trim_end_matches('/'), ocx_oci::endpoint::DEFAULT_REKOR_URL);

        let err = resolve_rekor_endpoint(
            None,
            &test_identifier(),
            Some("http://rekor.evil.example"),
            &SigstoreAuthority::public(),
        )
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
        let id = ocx_oci::PackageRef::parse("registry.example/pkg:1.0").expect("parse identifier");
        let package_error = PackageError::new(
            id.clone(),
            PackageErrorKind::Internal(PmError::Verify(Box::new(VerifyError::new(
                id,
                VerifyErrorKind::IdentityMismatch,
            )))),
        );
        let err = verify_error_into_anyhow(package_error);
        let json = render_error_document("package verify", &err).expect("render envelope");
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
            PackageErrorKind::Internal(PmError::Sign(Box::new(SignError::new(id, kind)))),
        )
    }

    fn envelope(err: &anyhow::Error) -> serde_json::Value {
        let json = render_error_document("package attest", err).expect("render envelope");
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
                81,
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
    use ocx_exit::ExitCode;

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

    /// A failed leg's slug is its `error.detail`, so the capability gap a category
    /// folds away (`referrers_unsupported` under `unsupported`) survives on the row,
    /// and an `Internal` leg reports its wrapped cause's slug.
    ///
    /// Reds on: emitting the category (`unsupported`) or the kind (`internal`).
    #[test]
    fn a_leg_failures_slug_is_its_detail_slug() {
        use ocx_sign::sign::SignErrorKind;
        assert_eq!(
            super::leg_slug(&SignErrorKind::ReferrersUnsupported),
            "referrers_unsupported"
        );
        let transient = SignErrorKind::Internal(Box::new(ocx_oci::client::error::ClientError::RegistryTransient(
            "503".into(),
        )));
        assert_eq!(super::leg_slug(&transient), "registry_transient");
    }
}

#[cfg(test)]
mod rounds_tests {
    use super::*;

    /// an exact keyless policy needs the trust root, no policy at all does not.
    #[test]
    fn a_keyless_round_needs_the_trust_root_and_an_empty_set_does_not() {
        let keyless = vec![Round::flags(vec![CompiledPolicy::exact(
            "id".into(),
            "https://issuer.example".into(),
        )])];
        assert!(rounds_need_trust_root(&keyless));
        assert!(!rounds_need_trust_root(&[]));
    }
}
