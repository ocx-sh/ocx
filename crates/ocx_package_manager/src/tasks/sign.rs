// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `sign_one` — package-manager task that signs a single target manifest.
//!
//! See [`subsystem-package-manager.md`](../../../../../.claude/rules/subsystem-package-manager.md).

use url::Url;
use zeroize::Zeroizing;

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{PackageError, PackageErrorKind};
use ocx_index::IndexOperation;
use ocx_sign::sign::key_backend::PemKeyBackend;
use ocx_sign::sign::pipeline::SignResult;
use ocx_sign::sign::state::SigningStatePaths;
use ocx_sign::sign::{
    DispatchingTokenProvider, KeySigner, KeylessSigner, SignContext, SignError, SignPipeline, Signer,
};

use super::super::PackageManager;

/// Options forwarded from the CLI to [`PackageManager::sign_one`]; the caller SSRF-validates both URLs.
#[derive(Clone)]
pub struct SignOptions {
    /// Fulcio CA endpoint (validated by the CLI). Default: `https://fulcio.sigstore.dev`.
    pub fulcio_url: Url,
    /// Rekor transparency log endpoint (validated by the CLI). Default: `https://rekor.sigstore.dev`.
    pub rekor_url: Url,
    /// OIDC override token; `None` falls back to ambient CI detection, then a browser OAuth flow unless `no_tty`.
    pub identity_token: Option<Zeroizing<String>>,
    /// Bypass the referrers-capability cache for this invocation.
    pub no_cache: bool,
    /// When true, suppress the browser OAuth fallback (CI / headless).
    pub no_tty: bool,
    /// Selects key mode; `None` is keyless.
    pub key: Option<ocx_trust::key_ref::KeyRef>,
    /// Which wire shape(s) to write; `Both` costs a second Fulcio certificate and a second Rekor entry.
    pub format: ocx_sign::sign::SignatureFormat,
    /// Whether a transparency-log entry is uploaded, resolved by the CLI through `RekorUploadOpt::enabled`.
    pub rekor_upload: bool,
}

/// Success payload returned by [`PackageManager::sign_one`].
pub struct SignReport {
    /// Raw pipeline result (subject digest, referrer descriptor, cert identity).
    pub result: SignResult,
}

impl PackageManager {
    /// Sign what `package` resolves to, publishing a Sigstore referrer to the registry; a `Some` platform
    /// narrows into an index and signs that child.
    ///
    /// # Errors
    ///
    /// [`PackageError`] tagged with `package`, classified via [`ocx_sign::sign::SignErrorKind`]; an offline
    /// manager fails with `OfflineMode` (exit 81).
    pub async fn sign_one(
        &self,
        package: &ocx_oci::PackageRef,
        platform: Option<&ocx_oci::Platform>,
        opts: SignOptions,
        resolved: Option<&(ocx_oci::Digest, ocx_oci::Manifest)>,
    ) -> Result<SignReport, PackageError> {
        let client = self
            .require_client()
            .map_err(|e| PackageError::new(package.clone(), PackageErrorKind::Internal(e)))?;

        let signer = build_signer(opts.key.as_ref(), opts.rekor_upload, &opts.rekor_url)
            .map_err(|kind| map_sign_error(package.clone(), SignError::new(package.clone(), kind)))?;
        let trusted_hosts = self.index().trusted_hosts_for(package.registry()).to_vec();
        let token_provider = DispatchingTokenProvider::new(opts.identity_token, opts.no_tty, trusted_hosts);
        let resolve = super::resolve_subject::sign_resolver(self.index(), resolved);
        let state = SigningStatePaths::new(self.file_structure().state.root());
        let context = SignContext {
            identifier: package,
            platform,
            signer: signer.as_ref(),
            token_provider: &token_provider,
            no_cache: opts.no_cache,
            dial: self.index().dial_policy(package.registry()),
            resolve,
            fulcio_url: &opts.fulcio_url,
            rekor_url: &opts.rekor_url,
            state,
            format: opts.format,
        };
        let result = SignPipeline::run(client, context)
            .await
            .map_err(|err| map_sign_error(package.clone(), err))?;
        Ok(SignReport { result })
    }
}

/// What a `--tags` / `--tags-file` sweep did to one tag, generic over the `sign` or `attest` report.
#[derive(Debug)]
pub enum SweptOutcome<R> {
    /// The tag's index was signed (or attested); this is that run's report.
    Done(R),
    /// The tag resolved to a bare manifest, which `push` already signed; not a failure.
    SkippedBareManifest,
    /// The tag names the index another tag in this sweep already acted on; the payload is that tag.
    ///
    /// Acting per tag would file identical referrers, and past `MAX_SIGNATURE_CANDIDATES` (8) on one digest
    /// `ocx package verify` stops finding them.
    /// Recorded only for a completed run, so a tag after a failed one retries instead of reporting covered.
    CoveredBy(String),
    /// This tag's own failure; the sweep records it and carries on.
    Failed(Box<PackageError>),
}

/// One swept tag, paired with what the sweep did to it.
#[derive(Debug)]
pub struct SweptTag<R> {
    /// The tag as the caller spelled it, so a report names what was asked for.
    pub tag: String,
    pub outcome: SweptOutcome<R>,
}

impl PackageManager {
    /// Sign the index each of `tags` resolves to, in `package`.
    ///
    /// Never fails: every outcome is a row, in tag order, and the caller derives the exit code from the rows.
    pub async fn sign_tags(
        &self,
        package: &ocx_oci::PackageRef,
        tags: &[String],
        opts: &SignOptions,
    ) -> Vec<SweptTag<SignReport>> {
        // Subject digest -> the tag whose run signed it.
        let mut signed: HashMap<ocx_oci::Digest, String> = HashMap::new();
        let mut swept = Vec::with_capacity(tags.len());
        for tag in tags {
            let identifier = package.clone_with_tag(tag.clone());
            let outcome = match self.resolve_swept_index(&identifier).await {
                Err(error) => SweptOutcome::Failed(Box::new(error)),
                Ok(None) => {
                    log::warn!("Skipping '{identifier}': it resolves to a single manifest, which push already signed.");
                    SweptOutcome::SkippedBareManifest
                }
                Ok(Some(resolved)) => match signed.get(&resolved.0).cloned() {
                    Some(first) => {
                        log::warn!(
                            "Skipping '{identifier}': it names the index tag '{first}' was already signed as; \
                             a signature is a referrer of the subject digest, so one covers both."
                        );
                        SweptOutcome::CoveredBy(first)
                    }
                    None => match self.sign_one(&identifier, None, opts.clone(), Some(&resolved)).await {
                        Ok(report) => {
                            signed.insert(resolved.0.clone(), tag.clone());
                            SweptOutcome::Done(report)
                        }
                        Err(error) => SweptOutcome::Failed(Box::new(error)),
                    },
                },
            };
            swept.push(SweptTag {
                tag: tag.clone(),
                outcome,
            });
        }
        swept
    }

    /// The image index `identifier` resolves to, or `None` for a bare manifest.
    ///
    /// Callers hand the answer on to the pipeline, or the tag can move between two resolutions and the
    /// sweep signs something it never inspected.
    ///
    /// # Errors
    ///
    /// The chain's own failure, and [`SignErrorKind::TargetNotFound`] when the tag resolves to nothing.
    ///
    /// [`SignErrorKind::TargetNotFound`]: ocx_sign::sign::SignErrorKind::TargetNotFound
    pub(super) async fn resolve_swept_index(
        &self,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>, PackageError> {
        let resolved = self
            .index()
            .fetch_manifest(identifier, IndexOperation::Resolve)
            .await
            .map_err(|e| PackageError::new(identifier.clone(), PackageErrorKind::Internal(e.into())))?;
        match resolved {
            Some(index @ (_, ocx_oci::Manifest::ImageIndex(_))) => Ok(Some(index)),
            Some((_, ocx_oci::Manifest::Image(_))) => Ok(None),
            None => Err(map_sign_error(
                identifier.clone(),
                SignError::new(
                    identifier.clone(),
                    ocx_sign::sign::SignErrorKind::TargetNotFound {
                        platform: "any".to_string(),
                    },
                ),
            )),
        }
    }
}

impl PackageManager {
    /// Sign each platform manifest a push landed on, by digest; the index is [`sign_tags`](Self::sign_tags)'s job.
    ///
    /// Never fails: every platform gets a row, and the caller derives the exit code from them.
    pub async fn sign_platforms(
        &self,
        package: &ocx_oci::PackageRef,
        platforms: &[(ocx_oci::Platform, ocx_oci::Digest)],
        opts: &SignOptions,
    ) -> Vec<(ocx_oci::Platform, Result<SignReport, PackageError>)> {
        let mut signed = Vec::with_capacity(platforms.len());
        for (platform, digest) in platforms {
            // Tag dropped: a later platform merge rewrites the tag's index, so a tagged reference re-resolves wrong.
            let pinned = package.clone_with_digest(digest.clone()).without_tag();
            let outcome = self.sign_one(&pinned, None, opts.clone(), None).await;
            signed.push((platform.clone(), outcome));
        }
        signed
    }
}

/// Build the signer the options select: keyless by default, key mode under `--key`.
///
/// # Errors
///
/// The key backend's error when the key cannot be read or decrypted, and
/// [`SignErrorKind::UnsupportedKeyBackend`](ocx_sign::sign::SignErrorKind) for a scheme with no implementation.
pub(super) fn build_signer(
    key: Option<&ocx_trust::key_ref::KeyRef>,
    rekor_upload: bool,
    rekor_url: &Url,
) -> Result<Box<dyn Signer>, ocx_sign::sign::SignErrorKind> {
    let Some(key) = key else {
        return Ok(Box::new(KeylessSigner::new()));
    };
    // A scheme neither accessor answers is refused by name, not as "no such file" (for `env://`,
    // a file named after a variable).
    let backend = if let Some(path) = key.as_path() {
        PemKeyBackend::open(path)?
    } else if let Some(variable) = key.as_env_var() {
        PemKeyBackend::open_env(variable)?
    } else {
        return Err(ocx_sign::sign::KeyBackendError::Unsupported { scheme: key.scheme() }.into());
    };
    // The URL travels only when uploading, so the signer's `uploads_to_transparency_log` cannot
    // disagree with the pipeline.
    let upload_to = rekor_upload.then(|| rekor_url.clone());
    Ok(Box::new(KeySigner::new(Arc::new(backend), upload_to)))
}

/// Wrap a [`SignError`] in a [`PackageError`] tagged with `identifier`,
/// preserving the sign exit code through `PackageErrorKind::Internal`.
fn map_sign_error(identifier: ocx_oci::PackageRef, err: SignError) -> PackageError {
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

    use crate::PackageManager;
    use ocx_index::{Index, IndexOperation};
    use ocx_oci::client::Client;
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
    use ocx_oci::{self, Digest, Manifest, PackageRef};
    use ocx_store::file_structure::FileStructure;

    /// The tags every sweep test runs. Three, not one: the defect is a *per
    /// tag* multiplier, and 1 vs 2 is the one length where N and 2N are close
    /// enough that an off-by-one elsewhere could imitate the fix.
    pub(crate) const TAGS: [&str; 3] = ["1.0.0", "1.0.1", "1.1.0"];

    /// The digest every swept tag resolves to.
    pub(crate) fn swept_digest() -> Digest {
        ocx_oci::Algorithm::Sha256.hash(b"swept image index")
    }

    /// The repository a sweep runs against.
    ///
    /// A **public** IP literal, matching the sign pipeline's own fixtures: the
    /// pipeline resolves the physical host before dialling it, and a DNS name
    /// would make this unit test depend on a resolver while a private range
    /// would be refused by the SSRF floor.
    pub(crate) fn sweep_identifier() -> PackageRef {
        PackageRef::parse("8.8.8.8/acme/tool:1.0").expect("sweep identifier")
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
    impl ocx_index::IndexImpl for CountingIndex {
        async fn list_repositories(&self, _: &str) -> ocx_index::error::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn list_tags(&self, _: &PackageRef) -> ocx_index::error::Result<Option<Vec<String>>> {
            Ok(None)
        }

        async fn fetch_manifest(
            &self,
            identifier: &PackageRef,
            _: IndexOperation,
        ) -> ocx_index::error::Result<Option<(Digest, Manifest)>> {
            self.asked.lock().expect("asked lock").push(identifier.to_string());
            Ok(Some((
                swept_digest(),
                Manifest::ImageIndex(ocx_oci::ImageIndex {
                    schema_version: 2,
                    media_type: Some(ocx_oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
                    manifests: Vec::new(),
                    artifact_type: None,
                    annotations: None,
                }),
            )))
        }

        async fn fetch_manifest_digest(
            &self,
            _: &PackageRef,
            _: IndexOperation,
        ) -> ocx_index::error::Result<Option<Digest>> {
            Ok(Some(swept_digest()))
        }

        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> ocx_index::error::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn box_clone(&self) -> Box<dyn ocx_index::IndexImpl> {
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
            format: ocx_sign::sign::SignatureFormat::Bundle,
            rekor_upload: true,
        }
    }

    /// A `--tags` sweep of N tags resolves N times, not 2N.
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
                SweptOutcome::CoveredBy(_) => "covered",
                SweptOutcome::Failed(_) => "failed",
            };
            assert_eq!(
                outcome, "failed",
                "the empty registry fails each tag inside the pipeline; a skip would mean \
                 the sweep never entered it, and the count would then be one per tag for a \
                 reason unrelated to the fix. `covered` would mean worse: every tag here \
                 resolves to one digest, so recording a digest whose run *failed* would \
                 report these siblings as covered by a signature nobody published, and \
                 they must be retried instead (tag '{}')",
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
