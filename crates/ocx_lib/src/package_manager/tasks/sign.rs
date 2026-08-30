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
        resolved: Option<&(oci::Digest, oci::Manifest)>,
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
            resolved,
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
            let outcome = match self.resolve_swept_index(&identifier).await {
                Err(error) => SweptOutcome::Failed(Box::new(error)),
                Ok(None) => {
                    crate::log::warn!(
                        "Skipping '{identifier}': it resolves to a single manifest, which push already signed."
                    );
                    SweptOutcome::SkippedBareManifest
                }
                // `None` for the platform, always: a sweep acts on the index
                // itself, and clap refuses `--platform` alongside `--tags`.
                //
                // The resolution travels with it: this loop just asked the
                // index chain what the tag names, and the pipeline would
                // otherwise ask the identical question one call later (#373).
                Ok(Some(resolved)) => match self.sign_one(&identifier, None, opts.clone(), Some(&resolved)).await {
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

    /// The image index `identifier` resolves to — the only thing a sweep acts
    /// on — or `None` when it resolves to a bare manifest and the sweep must
    /// skip it.
    ///
    /// Resolution goes through the index chain, the same route
    /// [`resolve_platform_target`](crate::oci::sign::pipeline::resolve_platform_target)
    /// takes, so `--offline` and the mirror map answer here exactly as they do
    /// one call later. The branch is on what resolution **returned**, never on
    /// the reference's form: OCX supports bare-manifest tags, so a tag implies
    /// nothing about the shape underneath it.
    ///
    /// The resolution is **returned, not discarded**, because the pipeline asks
    /// the index the identical question about the identical identifier
    /// immediately afterwards — a sweep that threw this answer away paid two
    /// manifest fetches per tag (#373). Handing it on also closes the window
    /// where a tag moved between the two calls and the sweep signed something
    /// other than what it inspected.
    ///
    /// # Errors
    ///
    /// The chain's own failure, and [`SignErrorKind::TargetNotFound`] when the
    /// tag resolves to nothing — a tag naming no object is this tag's failure,
    /// not a reason to skip it silently.
    ///
    /// [`SignErrorKind::TargetNotFound`]: crate::oci::sign::SignErrorKind::TargetNotFound
    pub(super) async fn resolve_swept_index(
        &self,
        identifier: &oci::Identifier,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>, PackageError> {
        let resolved = self
            .index()
            .fetch_manifest(identifier, IndexOperation::Resolve)
            .await
            .map_err(|e| PackageError::new(identifier.clone(), PackageErrorKind::Internal(e)))?;
        match resolved {
            Some(index @ (_, oci::Manifest::ImageIndex(_))) => Ok(Some(index)),
            Some((_, oci::Manifest::Image(_))) => Ok(None),
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
            // `None`: nothing was pre-resolved here. The reference is already
            // pinned to the digest the push wrote, so the pipeline's own
            // resolution is the only one this path ever performs.
            let outcome = self.sign_one(&pinned, None, opts.clone(), None).await;
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

/// Fixtures for the `--tags` sweep tests here and in [`super::attest`].
///
/// Shared rather than copied because the two sweeps are one mechanism —
/// `resolve_swept_index` followed by a pipeline that resolves the same
/// reference — and a second copy of the counting index would be a second place
/// for "what does a swept tag resolve to" to drift.
#[cfg(test)]
pub(super) mod sweep_test_support {
    use std::sync::{Arc, Mutex};

    use crate::file_structure::FileStructure;
    use crate::oci::client::Client;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};
    use crate::oci::index::{Index, IndexOperation};
    use crate::oci::{self, Digest, Identifier, Manifest};
    use crate::package_manager::PackageManager;

    /// The tags every sweep test runs. Three, not one: the defect is a *per
    /// tag* multiplier, and 1 vs 2 is the one length where N and 2N are close
    /// enough that an off-by-one elsewhere could imitate the fix.
    pub(crate) const TAGS: [&str; 3] = ["1.0.0", "1.0.1", "1.1.0"];

    /// The digest every swept tag resolves to.
    pub(crate) fn swept_digest() -> Digest {
        oci::Algorithm::Sha256.hash(b"swept image index")
    }

    /// The repository a sweep runs against.
    ///
    /// A **public** IP literal, matching the sign pipeline's own fixtures: the
    /// pipeline resolves the physical host before dialling it, and a DNS name
    /// would make this unit test depend on a resolver while a private range
    /// would be refused by the SSRF floor.
    pub(crate) fn sweep_identifier() -> Identifier {
        Identifier::parse("8.8.8.8/acme/tool:1.0").expect("sweep identifier")
    }

    /// An index that answers every reference with the same image index and
    /// **records which reference it was asked about**.
    ///
    /// The references, not a bare tally: a count alone proves how many
    /// resolutions a sweep performed and says nothing about what each one was
    /// for, so a sweep that resolved the wrong tag N times would read as fixed.
    ///
    /// It answers with an image *index*, never a bare manifest: a sweep skips a
    /// bare manifest without entering the pipeline at all, so a fixture of that
    /// shape would record one resolution per tag whether or not the answer is
    /// threaded — green for a reason that has nothing to do with the fix.
    #[derive(Clone)]
    pub(crate) struct CountingIndex {
        asked: Arc<Mutex<Vec<String>>>,
    }

    impl CountingIndex {
        pub(crate) fn new(asked: Arc<Mutex<Vec<String>>>) -> Self {
            Self { asked }
        }
    }

    #[async_trait::async_trait]
    impl crate::oci::index::IndexImpl for CountingIndex {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(None)
        }

        async fn fetch_manifest(
            &self,
            identifier: &Identifier,
            _: IndexOperation,
        ) -> crate::Result<Option<(Digest, Manifest)>> {
            self.asked.lock().expect("asked lock").push(identifier.to_string());
            Ok(Some((
                swept_digest(),
                Manifest::ImageIndex(oci::ImageIndex {
                    schema_version: 2,
                    media_type: Some(oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
                    manifests: Vec::new(),
                    artifact_type: None,
                    annotations: None,
                }),
            )))
        }

        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<Digest>> {
            Ok(Some(swept_digest()))
        }

        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn box_clone(&self) -> Box<dyn crate::oci::index::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// A manager whose index counts resolutions and whose registry holds
    /// nothing.
    ///
    /// The empty registry is deliberate: the pipeline fetches the subject
    /// manifest bytes immediately **after** resolving the target and long
    /// before it acquires an OIDC token, so every swept tag fails there. That
    /// keeps the test off the network while still driving the pipeline past
    /// the resolution the count is about.
    pub(crate) fn sweep_manager(
        asked: Arc<Mutex<Vec<String>>>,
        ocx_home: &std::path::Path,
    ) -> (PackageManager, StubTransportData) {
        let data = StubTransportData::new();
        let client = Client::with_transport(Box::new(StubTransport::new(data.clone())));
        let manager = PackageManager::new(
            FileStructure::with_root(ocx_home.to_path_buf()),
            Index::from_impl(CountingIndex::new(asked)),
            Some(client),
            "8.8.8.8",
        );
        (manager, data)
    }

    /// The tags, spelled as the sweep spells them — one reference resolution is
    /// owed per entry, in order.
    pub(crate) fn expected_resolutions() -> Vec<String> {
        TAGS.iter()
            .map(|tag| sweep_identifier().clone_with_tag((*tag).to_string()).to_string())
            .collect()
    }

    /// How many manifest reads the registry served — the positive control.
    ///
    /// A count of zero would mean the pipeline never got past resolution, and
    /// the fetch count would then be one per tag for a reason unrelated to the
    /// fix. Asserting on it is what keeps the green honest.
    pub(crate) fn manifest_reads(data: &StubTransportData) -> usize {
        data.read()
            .calls
            .iter()
            .filter(|call| call.as_str() == "pull_manifest_raw")
            .count()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::sweep_test_support::{TAGS, expected_resolutions, manifest_reads, sweep_identifier, sweep_manager};
    use super::{SignOptions, SweptOutcome};

    fn sign_options() -> SignOptions {
        SignOptions {
            // Loopback, like the sign pipeline's own fixtures: the dial-time
            // SSRF guard resolves whatever it is handed, and a documentation
            // domain would put DNS in a unit test's path.
            fulcio_url: url::Url::parse("http://127.0.0.1:5555").expect("fulcio url"),
            rekor_url: url::Url::parse("http://127.0.0.1:3000").expect("rekor url"),
            identity_token: None,
            no_cache: true,
            no_tty: true,
            key: None,
            format: crate::oci::sign::SignatureFormat::Bundle,
            rekor_upload: true,
        }
    }

    /// **S-011 / C-040.** A `--tags` sweep of N tags resolves N times, not 2N.
    ///
    /// The sweep asks the index chain what each tag names so it can skip bare
    /// manifests; the pipeline then asked the identical question about the
    /// identical reference, and the second answer was the one that got used
    /// (#373). Counted rather than smoke-tested: a run that fetches twice
    /// produces exactly the same reports as one that fetches once, so nothing
    /// about the outcome can tell the two apart.
    #[tokio::test]
    async fn a_tag_sweep_resolves_each_tag_exactly_once() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let temp = tempfile::TempDir::new().expect("ocx home");
        let (manager, transport) = sweep_manager(Arc::clone(&asked), temp.path());
        let tags: Vec<String> = TAGS.iter().map(|tag| (*tag).to_string()).collect();

        let swept = manager.sign_tags(&sweep_identifier(), &tags, &sign_options()).await;

        assert_eq!(swept.len(), TAGS.len(), "one row per swept tag");
        for row in &swept {
            let outcome = match &row.outcome {
                SweptOutcome::Done(_) => "signed",
                SweptOutcome::SkippedBareManifest => "skipped",
                SweptOutcome::Failed(_) => "failed",
            };
            assert_eq!(
                outcome, "failed",
                "the empty registry fails each tag inside the pipeline; a skip would mean \
                 the sweep never entered it, and the count would then be one per tag for a \
                 reason unrelated to the fix (tag '{}')",
                row.tag,
            );
        }
        assert_eq!(
            manifest_reads(&transport),
            TAGS.len(),
            "positive control: each tag reached the pipeline's subject fetch, which is \
             past the resolution this test counts",
        );
        assert_eq!(
            *asked.lock().expect("asked lock"),
            expected_resolutions(),
            "one manifest resolution per swept tag, each for that tag — the sweep's answer \
             is the pipeline's",
        );
    }
}
