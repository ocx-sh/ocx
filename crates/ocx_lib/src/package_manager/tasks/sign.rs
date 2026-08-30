// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `sign_one` — package-manager task that signs a single target manifest.
//!
//! Wraps [`crate::oci::sign::SignPipeline`] (C-S1-3 pipeline with injection
//! seams) in the three-layer error model: the client / index come from the
//! [`PackageManager`] facade, the pipeline's [`SignResult`] becomes a
//! [`SignReport`], and any failure is wrapped in a [`PackageError`] tagged with
//! the target identifier.
//!
//! Per [`subsystem-package-manager.md`](../../../../../.claude/rules/subsystem-package-manager.md)
//! and Spec A10 — tasks live in `package_manager/tasks/`; the aggregator is
//! `package_manager/tasks.rs` (not `tasks/mod.rs`).

use url::Url;
use zeroize::Zeroizing;

use std::sync::Arc;

use crate::oci::index::IndexOperation;
use crate::oci::sign::key_backend::PemKeyBackend;
use crate::oci::sign::{
    DispatchingTokenProvider, KeySigner, KeylessSigner, SignContext, SignError, SignPipeline, Signer,
};
use crate::oci::{self, sign::pipeline::SignResult};
use crate::package_manager::error::{PackageError, PackageErrorKind};

use super::super::PackageManager;

/// Options forwarded from the CLI to [`PackageManager::sign_one`].
///
/// `fulcio_url` / `rekor_url` are the validated Sigstore endpoints (C-S1-3
/// injection seams — default to the public Fulcio/Rekor URLs, overridden by
/// tests). The CLI performs SSRF validation at its boundary and hands over the
/// parsed [`Url`]s.
///
/// `identity_token` is the precedence-resolved override token from the CLI
/// layer (`--identity-token-file` > `--identity-token-stdin` > env), held under
/// [`Zeroizing`] so the cleartext is scrubbed on drop. When `None`, the
/// dispatching token provider falls back to ambient detection (GHA, GitLab,
/// CircleCI, …) then optionally to a browser OAuth flow when `no_tty` is
/// `false`. See C-S1-4.
///
/// `Clone` because a `--tags` / `--tags-file` sweep signs N references from one
/// parsed option set, and every field is plain data — the token stays under
/// [`Zeroizing`], so a clone is scrubbed on drop exactly like the original.
#[derive(Clone)]
pub struct SignOptions {
    /// Fulcio CA endpoint (validated by the CLI). Default: `https://fulcio.sigstore.dev`.
    pub fulcio_url: Url,
    /// Rekor transparency log endpoint (validated by the CLI). Default: `https://rekor.sigstore.dev`.
    pub rekor_url: Url,
    /// OIDC override token (file / stdin / env, resolved by the CLI layer).
    pub identity_token: Option<Zeroizing<String>>,
    /// Bypass the referrers-capability cache for this invocation.
    pub no_cache: bool,
    /// When true, suppress the browser OAuth fallback (CI / headless).
    pub no_tty: bool,
    /// Selects key mode. `None` is keyless — the default and the differentiator
    /// (spec D10); key mode is added, never substituted.
    pub key: Option<oci::sign::KeyRef>,
    /// Which wire shape(s) to write. `Bundle` by default (spec D8); `Both`
    /// emits each, at the cost of a second Fulcio certificate and a second
    /// Rekor entry.
    pub format: crate::oci::sign::SignatureFormat,
    /// Whether a transparency-log entry is uploaded.
    ///
    /// Resolved by the CLI through `RekorUploadOpt::enabled`, which encodes the
    /// asymmetry: keyless always uploads and `--no-rekor-upload` is an error
    /// there; key mode is off unless opted in.
    pub rekor_upload: bool,
}

/// Success payload returned by [`PackageManager::sign_one`].
///
/// Thin wrapper over [`SignResult`] so the package-manager layer owns the
/// report type and the CLI `Printable` impl lives in `ocx_cli::api::data`.
pub struct SignReport {
    /// Raw pipeline result (subject digest, referrer descriptor, cert identity).
    pub result: SignResult,
}

impl PackageManager {
    /// Sign what `package` resolves to, publishing a Sigstore bundle v0.3
    /// referrer manifest to the registry.
    ///
    /// `platform` is a **narrowing modifier**, not a selector: `None` signs the
    /// resolved object as-is (an image index is then the subject itself),
    /// `Some` narrows into an index and signs that child. It is an error when
    /// the resolution is a bare manifest.
    ///
    /// The pipeline is:
    /// resolve subject digest → pre-check OIDC (keyless only) → obtain signing
    /// material → sign → optional Rekor upload → bundle build → push bundle
    /// blob → push referrer manifest, naming it in the OCI tag-schema fallback
    /// index when the registry serves no Referrers API. Under
    /// `--signature-format simplesigning|both` a second, independent signature
    /// is written to the cosign `sha256-<hex>.sig` sidecar. Emits a
    /// [`SignReport`] on success — one leg per shape, best-effort per leg.
    ///
    /// The registry client comes from the facade ([`require_client`][Self::require_client]);
    /// signing requires network access, so an offline manager fails with
    /// `OfflineMode` (exit 81). (`ocx package sign` refuses `--offline` earlier
    /// with a dedicated policy error.)
    ///
    /// Returns [`PackageError`] tagged with `package` on any failure —
    /// exit-code classification routes via
    /// [`crate::oci::sign::SignErrorKind`].
    pub async fn sign_one(
        &self,
        package: &oci::Identifier,
        platform: Option<&oci::Platform>,
        opts: SignOptions,
    ) -> Result<SignReport, PackageError> {
        let client = self
            .require_client()
            .map_err(|e| PackageError::new(package.clone(), PackageErrorKind::Internal(e)))?;

        let signer = build_signer(opts.key.as_ref(), opts.rekor_upload, &opts.rekor_url)
            .map_err(|kind| map_sign_error(package.clone(), SignError::new(package.clone(), kind)))?;
        let trusted_hosts = self.index().trusted_hosts_for(package.registry()).to_vec();
        let token_provider = DispatchingTokenProvider::new(opts.identity_token, opts.no_tty, trusted_hosts);
        let context = SignContext {
            identifier: package,
            platform,
            signer: signer.as_ref(),
            token_provider: &token_provider,
            no_cache: opts.no_cache,
            index: self.index(),
            fulcio_url: &opts.fulcio_url,
            rekor_url: &opts.rekor_url,
            state: &self.file_structure().state,
            format: opts.format,
        };
        let result = SignPipeline::run(client, context)
            .await
            .map_err(|err| map_sign_error(package.clone(), err))?;
        Ok(SignReport { result })
    }
}

/// What a `--tags` / `--tags-file` sweep did to one tag.
///
/// Generic over the per-reference report so `sign` and `attest` sweep through
/// one type rather than two identical ones — the outcome vocabulary is the
/// spec's, and it is the same vocabulary for both verbs.
#[derive(Debug)]
pub enum SweptOutcome<R> {
    /// The tag's index was signed (or attested); this is that run's report.
    Done(R),
    /// The tag resolved to a bare manifest, so the sweep left it alone.
    ///
    /// Not a failure: `push` already signed each platform manifest inline, and
    /// a tag list mixing single-platform and multi-platform packages is the
    /// normal case for a repository that publishes both.
    SkippedBareManifest,
    /// This tag's own failure. The sweep records it and carries on to the rest.
    ///
    /// Boxed because `PackageError` dwarfs every other variant, and a sweep
    /// holds one of these per tag.
    Failed(Box<PackageError>),
}

/// One swept tag, paired with what the sweep did to it.
#[derive(Debug)]
pub struct SweptTag<R> {
    /// The tag as the caller spelled it, so a report names what was asked for.
    pub tag: String,
    /// What happened to it.
    pub outcome: SweptOutcome<R>,
}

impl PackageManager {
    /// Sign the index each of `tags` resolves to, in the repository `package`
    /// names.
    ///
    /// This is the index sweep the spec's division of labour asks for: `push`
    /// signed each platform manifest inline, and the enclosing index is only
    /// final once the last platform has landed, so a later sweep signs the
    /// index each recorded tag now points at. The manifests underneath are
    /// already signed and are **not** revisited — nothing here narrows into an
    /// index, which is why `--platform` is refused alongside `--tags`.
    ///
    /// Never returns `Err`: a sweep's whole purpose is to survive a per-tag
    /// failure, so every outcome — signed, skipped, failed — is a row in the
    /// returned vector, in the order the tags were given. Aborting at the first
    /// failure of twenty would leave the operator with no idea which of the
    /// remaining nineteen succeeded. The caller decides the exit code from each
    /// row's [`SweptOutcome`].
    pub async fn sign_tags(
        &self,
        package: &oci::Identifier,
        tags: &[String],
        opts: &SignOptions,
    ) -> Vec<SweptTag<SignReport>> {
        let mut swept = Vec::with_capacity(tags.len());
        for tag in tags {
            let identifier = package.clone_with_tag(tag.clone());
            let outcome = match self.resolves_to_index(&identifier).await {
                Err(error) => SweptOutcome::Failed(Box::new(error)),
                Ok(false) => {
                    crate::log::warn!(
                        "Skipping '{identifier}': it resolves to a single manifest, which push already signed."
                    );
                    SweptOutcome::SkippedBareManifest
                }
                // `None` for the platform, always: a sweep acts on the index
                // itself, and clap refuses `--platform` alongside `--tags`.
                Ok(true) => match self.sign_one(&identifier, None, opts.clone()).await {
                    Ok(report) => SweptOutcome::Done(report),
                    Err(error) => SweptOutcome::Failed(Box::new(error)),
                },
            };
            swept.push(SweptTag {
                tag: tag.clone(),
                outcome,
            });
        }
        swept
    }

    /// Whether `identifier` resolves to an image index — the only thing a
    /// sweep acts on.
    ///
    /// Resolution goes through the index chain, the same route
    /// [`resolve_platform_target`](crate::oci::sign::pipeline::resolve_platform_target)
    /// takes, so `--offline` and the mirror map answer here exactly as they do
    /// one call later. The branch is on what resolution **returned**, never on
    /// the reference's form: OCX supports bare-manifest tags, so a tag implies
    /// nothing about the shape underneath it.
    ///
    /// # Errors
    ///
    /// The chain's own failure, and [`SignErrorKind::TargetNotFound`] when the
    /// tag resolves to nothing — a tag naming no object is this tag's failure,
    /// not a reason to skip it silently.
    ///
    /// [`SignErrorKind::TargetNotFound`]: crate::oci::sign::SignErrorKind::TargetNotFound
    pub(super) async fn resolves_to_index(&self, identifier: &oci::Identifier) -> Result<bool, PackageError> {
        let resolved = self
            .index()
            .fetch_manifest(identifier, IndexOperation::Resolve)
            .await
            .map_err(|e| PackageError::new(identifier.clone(), PackageErrorKind::Internal(e)))?;
        match resolved {
            Some((_, oci::Manifest::ImageIndex(_))) => Ok(true),
            Some((_, oci::Manifest::Image(_))) => Ok(false),
            None => Err(map_sign_error(
                identifier.clone(),
                SignError::new(
                    identifier.clone(),
                    crate::oci::sign::SignErrorKind::TargetNotFound {
                        platform: "any".to_string(),
                    },
                ),
            )),
        }
    }
}

impl PackageManager {
    /// Sign each platform manifest a push landed on, by digest.
    ///
    /// This is the other half of the spec's division of labour: `push` signs
    /// the platform manifests inline because their digests are final the
    /// moment they are pushed, while the enclosing index is only final once the
    /// last platform has landed and is swept later by
    /// [`sign_tags`](Self::sign_tags). The index is never signed here.
    ///
    /// `platforms` is a push outcome's `platform_digests` verbatim. Each
    /// reference is **pinned to the recorded digest with the tag dropped**, so
    /// the signature binds the immutable object the push wrote rather than
    /// whatever the tag resolves to now — `push_manifest_and_merge_tags`
    /// rewrites the tag's index on every platform merge, and a tagged
    /// reference would re-resolve to it. `--platform` narrowing is `None` for
    /// the same reason: the pinned reference already *is* the child, so there
    /// is nothing left to narrow into.
    ///
    /// Never returns `Err`: like the tag sweep, every platform gets a row so a
    /// caller learns which ones landed. The caller decides the exit code.
    pub async fn sign_platforms(
        &self,
        package: &oci::Identifier,
        platforms: &[(oci::Platform, oci::Digest)],
        opts: &SignOptions,
    ) -> Vec<(oci::Platform, Result<SignReport, PackageError>)> {
        let mut signed = Vec::with_capacity(platforms.len());
        for (platform, digest) in platforms {
            let pinned = package.clone_with_digest(digest.clone()).without_tag();
            let outcome = self.sign_one(&pinned, None, opts.clone()).await;
            signed.push((platform.clone(), outcome));
        }
        signed
    }
}

/// Build the signer the options select: keyless by default, key mode under
/// `--key`.
///
/// Shared with `attest_one`, which faces the identical choice — a second copy
/// would be a second place for the Rekor-upload asymmetry to drift.
///
/// # Errors
///
/// The key backend's own error class when the key cannot be read or decrypted
/// (see [`crate::oci::sign::KeyBackendError`]), and
/// [`SignErrorKind::UnsupportedKeyBackend`](crate::oci::sign::SignErrorKind)
/// for a recognised scheme with no implementation.
pub(super) fn build_signer(
    key: Option<&oci::sign::KeyRef>,
    rekor_upload: bool,
    rekor_url: &Url,
) -> Result<Box<dyn Signer>, crate::oci::sign::SignErrorKind> {
    let Some(key) = key else {
        return Ok(Box::new(KeylessSigner::new()));
    };
    // One accessor per implemented scheme, and both are `Some` for exactly
    // one: a reference that answers neither is a backend OCX has not built,
    // and it is refused by name here rather than as "no such file or
    // directory" — or, worse for `env://`, as a file named after a variable.
    let backend = if let Some(path) = key.as_path() {
        PemKeyBackend::open(path)?
    } else if let Some(variable) = key.as_env_var() {
        PemKeyBackend::open_env(variable)?
    } else {
        return Err(crate::oci::sign::KeyBackendError::Unsupported { scheme: key.scheme() }.into());
    };
    // The URL travels only when it will be dialled, so the signer's own
    // `uploads_to_transparency_log` cannot disagree with what the pipeline
    // guards.
    let upload_to = rekor_upload.then(|| rekor_url.clone());
    Ok(Box::new(KeySigner::new(Arc::new(backend), upload_to)))
}

/// Wrap a [`SignError`] in a [`PackageError`] tagged with `identifier`,
/// preserving the sign exit code through `PackageErrorKind::Internal`.
fn map_sign_error(identifier: oci::Identifier, err: SignError) -> PackageError {
    PackageError::new(
        identifier,
        PackageErrorKind::Internal(crate::Error::Sign(Box::new(err))),
    )
}
