// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package export` and `ocx export`: copy packages and everything needed to install them offline into a
//! directory holding a restricted index, a blob tree and a marker.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use ocx_index::export::TagRow;
use ocx_oci::blob_lookup::export_marker::{self, ExportMarker, ExportMarkerError};
use ocx_oci::blob_lookup::{BlobLookup, BlobWriter, LookupError, MAX_REFERRERS_PER_SUBJECT, WriteOutcome, markers};
use ocx_oci::client::error::{ClientError, TraversalLimit};
use ocx_oci::copy::MAX_CONCURRENT_BLOB_TRANSFERS;
use ocx_oci::{Digest, OciIdentifier, PackageRef, PinnedOciIdentifier, PinnedPackageRef, Platform};

use super::common::{load_config_metadata, lookup_error, resolve_top_manifest, stage_chain_blobs};
use super::resolve::{PatchRootScope, ResolvedChain, snapshot_companion_roots};
use crate::PackageManager;
use crate::error::{Error, PackageErrorKind, Result};

/// How long a second export into the same directory waits for the first.
const EXPORT_LOCK_TIMEOUT: Duration = Duration::from_secs(120);

/// How deep a referrer chain is followed; a signature over an SBOM is depth 2.
const MAX_REFERRER_DEPTH: usize = 8;

/// Ceiling on a manifest or index document read into memory.
const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;

/// One digest-pinned platform leaf, as a lock file records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockPin {
    /// Registry, repository and the leaf manifest's digest.
    pub identifier: PinnedPackageRef,
    pub platform: Platform,
}

/// What `ocx package export` and `ocx export` were asked to write.
#[derive(Debug, Clone)]
pub struct ExportRequest {
    /// The export directory; created when absent.
    pub output: PathBuf,
    /// Named packages; empty only for the lock form.
    pub packages: Vec<PackageRef>,
    /// The lock form: each pin names one platform leaf. No `index/` is written for these.
    pub lock_pins: Vec<LockPin>,
    /// Platforms to export; empty means every platform the package publishes.
    pub platforms: Vec<Platform>,
    /// Whether signature referrers travel with their subjects.
    pub referrers: bool,
}

/// Whether a platform's blobs were written by this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportPlatformStatus {
    Written,
    /// Already in the directory.
    Present,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedPlatform {
    pub platform: Platform,
    pub digest: Digest,
    pub status: ExportPlatformStatus,
}

#[derive(Debug, Clone)]
pub struct ExportedPackage {
    /// As named; a lock pin reads `registry/repository@sha256:...`.
    pub identifier: PackageRef,
    /// `None` for a digest identifier and a lock pin, which write no index rows.
    pub tags: Option<Vec<TagRow>>,
    pub platforms: Vec<ExportedPlatform>,
    pub referrer_count: usize,
}

#[derive(Debug, Clone)]
pub struct ExportOutcome {
    pub output_dir: PathBuf,
    pub packages: Vec<ExportedPackage>,
    /// Blobs written by this run.
    pub blob_count: usize,
    /// Bytes written by this run.
    pub written_size: u64,
}

impl PackageManager {
    /// Writes `request`'s packages, their dependency closure and (optionally) referrers into the export
    /// directory, then its restricted index and marker.
    ///
    /// # Errors
    ///
    /// [`crate::Error::ExportDirInvalid`] for an unusable directory or registry slug collision;
    /// [`crate::Error::ExportPlatformRequired`] for a digest identifier without exactly one platform;
    /// [`crate::Error::OfflineMode`] when a blob is nowhere local under `--offline`; resolution and I/O failures.
    pub async fn export(&self, request: ExportRequest) -> Result<ExportOutcome> {
        for package in &request.packages {
            if package.digest().is_some() && request.platforms.len() != 1 {
                return Err(Error::ExportPlatformRequired {
                    identifier: package.to_string(),
                    found: request.platforms.len(),
                });
            }
        }

        let output = request.output.clone();
        let invalid = |reason: String| Error::ExportDirInvalid {
            dir: output.clone(),
            reason,
        };
        tokio::fs::create_dir_all(&output)
            .await
            .map_err(|error| invalid(error.to_string()))?;
        let _lock = ocx_util::fs::lock_scoped(
            &self.file_structure().locks,
            "export-dir",
            &output,
            "",
            EXPORT_LOCK_TIMEOUT,
        )
        .await?;

        let existing = {
            let dir = output.clone();
            tokio::task::spawn_blocking(move || export_marker::read_marker(&dir))
                .await
                .map_err(|join| Error::InternalFile(output.clone(), std::io::Error::other(join)))?
                .map_err(|error| marker_error(&output, error))?
        };
        let run = Run::new(self, &request, existing.unwrap_or_default())?;
        // A slug collision refuses the run before any blob is written.
        for registry in request
            .packages
            .iter()
            .map(PackageRef::registry)
            .chain(request.lock_pins.iter().map(|pin| pin.identifier.registry()))
        {
            run.note_registry(registry)?;
        }

        let mut exported = Vec::new();
        for package in &request.packages {
            exported.push(run.export_named(self, package, &request.platforms).await?);
        }
        for pin in &request.lock_pins {
            exported.push(run.export_pin(self, pin).await?);
        }
        if request.packages.is_empty() {
            exported.extend(run.export_snapshot(self, &request.platforms).await?);
        }

        run.write_index(self, &request.packages, &mut exported).await?;
        run.write_marker().await?;

        Ok(ExportOutcome {
            output_dir: output,
            packages: exported,
            blob_count: run.blob_count.load(Ordering::Relaxed),
            written_size: run.written_size.load(Ordering::Relaxed),
        })
    }
}

fn marker_error(dir: &Path, error: ExportMarkerError) -> Error {
    match error {
        ExportMarkerError::Read(error) => lookup_error(dir, error),
        other => Error::ExportDirInvalid {
            dir: dir.to_path_buf(),
            reason: other.to_string(),
        },
    }
}

fn join_error(path: &Path, join: tokio::task::JoinError) -> Error {
    Error::InternalFile(path.to_path_buf(), std::io::Error::other(join))
}

/// Where a blob no local source holds can be fetched from.
enum Fetch {
    /// A blob endpoint read.
    Blob {
        identifier: PinnedOciIdentifier,
        descriptor: ocx_oci::Descriptor,
    },
    /// A manifest endpoint read; the blobs endpoint does not serve manifest digests.
    Manifest { identifier: OciIdentifier },
}

/// One export run's shared state.
struct Run {
    out_blobs: PathBuf,
    /// The export's own blobs first, then the home, then the configured directories and cache.
    lookup: BlobLookup,
    marker: Mutex<ExportMarker>,
    visited: Mutex<HashSet<PinnedPackageRef>>,
    referrers: bool,
    blob_count: AtomicUsize,
    written_size: AtomicU64,
    referrer_count: AtomicUsize,
}

impl Run {
    fn new(manager: &PackageManager, request: &ExportRequest, marker: ExportMarker) -> Result<Self> {
        let out_blobs = request.output.join("blobs");
        std::fs::create_dir_all(&out_blobs).map_err(|error| Error::ExportDirInvalid {
            dir: request.output.clone(),
            reason: error.to_string(),
        })?;
        let lookup = manager
            .blob_lookup()
            .with_leading_dir(manager.file_structure().blobs.root().to_path_buf())
            .with_leading_dir(out_blobs.clone());
        Ok(Self {
            out_blobs,
            lookup,
            marker: Mutex::new(marker),
            visited: Mutex::default(),
            referrers: request.referrers,
            blob_count: AtomicUsize::new(0),
            written_size: AtomicU64::new(0),
            referrer_count: AtomicUsize::new(0),
        })
    }

    /// The export root, the parent of `blobs/`.
    fn root(&self) -> &Path {
        self.out_blobs.parent().unwrap_or(Path::new(""))
    }

    fn lookup_error(&self, error: LookupError) -> Error {
        lookup_error(self.root(), error)
    }

    /// A writer for `digest` under `DIR/blobs`. `root_for` hashes a destination that already holds the digest, so it
    /// runs on the blocking pool: an async worker must not stall on a large layer.
    async fn writer_for(&self, registry: &str, digest: &Digest) -> Result<BlobWriter> {
        let (root, registry, digest) = (self.out_blobs.clone(), registry.to_owned(), digest.clone());
        tokio::task::spawn_blocking(move || BlobWriter::root_for(&root, &registry, &digest))
            .await
            .map_err(|join| join_error(self.root(), join))?
            .map_err(|error| self.lookup_error(error))
    }

    fn note_registry(&self, registry: &str) -> Result<()> {
        let mut marker = self.marker.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        export_marker::merge_registry(&mut marker, registry).map_err(|error| Error::ExportDirInvalid {
            dir: self.out_blobs.parent().map(Path::to_path_buf).unwrap_or_default(),
            reason: error.to_string(),
        })
    }

    fn count(&self, outcome: WriteOutcome, size: u64) -> WriteOutcome {
        if outcome != WriteOutcome::AlreadyPresent {
            self.blob_count.fetch_add(1, Ordering::Relaxed);
            self.written_size.fetch_add(size, Ordering::Relaxed);
        }
        outcome
    }

    async fn export_named(
        &self,
        manager: &PackageManager,
        package: &PackageRef,
        platforms: &[Platform],
    ) -> Result<ExportedPackage> {
        let targets = resolve_targets(manager, package, platforms)
            .await
            .map_err(|kind| resolve_error(manager, package, kind))?;
        self.export_targets(manager, package.clone(), targets, platforms.first())
            .await
    }

    async fn export_pin(&self, manager: &PackageManager, pin: &LockPin) -> Result<ExportedPackage> {
        let package = pin.identifier.as_identifier().clone();
        let resolved = manager
            .resolve(&package, pin.platform.clone())
            .await
            .map_err(|kind| resolve_error(manager, &package, kind))?;
        self.export_targets(manager, package, vec![resolved], Some(&pin.platform))
            .await
    }

    /// The companions and descriptors an active patch snapshot pins, which a machine without the patch
    /// registry needs to compose the same environment.
    async fn export_snapshot(&self, manager: &PackageManager, platforms: &[Platform]) -> Result<Vec<ExportedPackage>> {
        let Some(snapshot) = manager.patch_snapshot() else {
            return Ok(Vec::new());
        };
        // A snapshot-driven read writes nothing under `index/`.
        let read_only = PackageManager {
            index: manager.index.read_only_view(),
            ..manager.clone()
        };
        let mut exported = Vec::new();
        for (identifier, digest) in snapshot_companion_roots(manager, PatchRootScope::RecordedAndSnapshot) {
            let pinned = identifier.clone_with_digest(digest.clone());
            let targets = match resolve_targets(&read_only, &pinned, platforms).await {
                Ok(targets) => targets,
                Err(kind @ (PackageErrorKind::NotFound | PackageErrorKind::OfflineManifestMissing(_)))
                    if identifier.tag().is_some() =>
                {
                    // Some sources serve only the advisory tag; accepted only while it still names the pinned digest.
                    let by_tag = identifier.clone_with_tag(identifier.tag_or_latest());
                    stage_index_by_tag(&read_only, &by_tag, &digest).await?;
                    resolve_targets(&read_only, &pinned, platforms)
                        .await
                        .map_err(|_| resolve_error(manager, &pinned, kind))?
                }
                Err(kind) => return Err(resolve_error(manager, &pinned, kind)),
            };
            exported.push(
                self.export_targets(&read_only, pinned, targets, platforms.first())
                    .await?,
            );
        }
        for (key, digest) in &snapshot.descriptors {
            let Some((registry, repository)) = key.split_once('/') else {
                continue;
            };
            self.export_descriptor(manager, registry, repository, digest).await?;
        }
        Ok(exported)
    }

    async fn export_descriptor(
        &self,
        manager: &PackageManager,
        registry: &str,
        repository: &str,
        digest: &Digest,
    ) -> Result<()> {
        let named = PackageRef::new_registry(repository, registry).clone_with_digest(digest.clone());
        // Dialled as named, never index-routed: a descriptor is not a package and no index serves one.
        let location = OciIdentifier::passthrough(&named);
        let missing = || missing_blob(manager, &named);
        self.copy_blob(
            manager,
            registry,
            digest,
            Ok(Fetch::Manifest {
                identifier: location.clone_with_digest(digest.clone()),
            }),
            missing(),
        )
        .await?;
        let bytes = self
            .read_local(registry, digest)
            .await?
            .ok_or_else(|| missing_blob(manager, &named))?;
        let manifest: ocx_oci::ImageManifest = serde_json::from_slice(&bytes).map_err(Error::SerializationFailure)?;
        for layer in &manifest.layers {
            let layer_digest = Digest::try_from(layer.digest.as_str())?;
            self.copy_blob(
                manager,
                registry,
                &layer_digest,
                Ok(Fetch::Blob {
                    identifier: location.pinned_at(layer_digest.clone()),
                    descriptor: layer.clone(),
                }),
                missing(),
            )
            .await?;
        }
        Ok(())
    }

    async fn export_targets(
        &self,
        manager: &PackageManager,
        identifier: PackageRef,
        targets: Vec<ResolvedChain>,
        fallback_platform: Option<&Platform>,
    ) -> Result<ExportedPackage> {
        let referrers_before = self.referrer_count.load(Ordering::Relaxed);
        let mut platforms = Vec::new();
        for resolved in &targets {
            let written_before = self.blob_count.load(Ordering::Relaxed);
            let platform = fallback_platform.cloned().unwrap_or_else(Platform::any);
            self.export_resolved(manager, resolved, &platform).await?;
            let status = if self.blob_count.load(Ordering::Relaxed) > written_before {
                ExportPlatformStatus::Written
            } else {
                ExportPlatformStatus::Present
            };
            platforms.push(ExportedPlatform {
                platform: resolved.platform.clone(),
                digest: resolved.pinned.digest(),
                status,
            });
        }
        Ok(ExportedPackage {
            identifier,
            tags: None,
            platforms,
            referrer_count: self.referrer_count.load(Ordering::Relaxed) - referrers_before,
        })
    }

    /// One resolved leaf: its chain, layers, referrers and dependency closure. A leaf already exported by this
    /// run (a shared dependency) is skipped.
    async fn export_resolved(
        &self,
        manager: &PackageManager,
        resolved: &ResolvedChain,
        fallback_platform: &Platform,
    ) -> Result<()> {
        let first_visit = self
            .visited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(resolved.pinned.strip_advisory());
        if !first_visit {
            return Ok(());
        }
        let package = resolved.pinned.as_identifier().clone();
        stage_chain_blobs(manager.file_structure(), manager.index(), resolved)
            .await
            .map_err(|kind| Error::package(package.clone(), kind))?;
        stage_dispatch_from_index(manager, resolved).await?;

        for blob in &resolved.chain {
            let registry = blob.identifier.registry();
            self.copy_blob(
                manager,
                registry,
                &blob.identifier.digest(),
                Err(missing_blob(manager, blob.identifier.as_identifier())),
                missing_blob(manager, blob.identifier.as_identifier()),
            )
            .await?;
        }

        let registry = resolved.pinned.registry();
        let package_ref = &package;
        let transfers = resolved.final_manifest.layers.iter().map(|layer| async move {
            let digest = Digest::try_from(layer.digest.as_str())?;
            let fetch = match &resolved.transport_pinned {
                Ok(transport) => Ok(Fetch::Blob {
                    identifier: transport.clone_with_digest(digest.clone()),
                    descriptor: layer.clone(),
                }),
                Err(no_transport) => Err(no_transport.missing_layer(&resolved.pinned, &digest)),
            };
            self.copy_blob(manager, registry, &digest, fetch, missing_blob(manager, package_ref))
                .await
        });
        let results: Vec<Result<WriteOutcome>> = futures::stream::iter(transfers)
            .buffer_unordered(MAX_CONCURRENT_BLOB_TRANSFERS)
            .collect()
            .await;
        for result in results {
            result?;
        }

        if self.referrers {
            let transport = resolved.transport_pinned.as_ref().ok();
            let subjects = resolved.chain.iter().filter(|blob| {
                matches!(
                    blob.role,
                    super::resolve::ChainRole::Index | super::resolve::ChainRole::Manifest
                )
            });
            for subject in subjects {
                let mut seen = BTreeSet::new();
                self.export_referrers(
                    manager,
                    registry,
                    &subject.identifier.digest(),
                    transport.map(PinnedOciIdentifier::as_oci_identifier),
                    0,
                    &mut seen,
                )
                .await?;
            }
        }

        let metadata = load_config_metadata(manager.index(), &resolved.pinned, &resolved.final_manifest)
            .await
            .map_err(|kind| Error::package(package.clone(), kind))?;
        let platform = if resolved.platform == Platform::any() {
            fallback_platform.clone()
        } else {
            resolved.platform.clone()
        };
        for dependency in metadata.dependencies() {
            let dependency_ref = dependency.identifier.as_identifier().clone();
            let target = manager
                .resolve(&dependency_ref, platform.clone())
                .await
                .map_err(|kind| resolve_error(manager, &dependency_ref, kind))?;
            Box::pin(self.export_resolved(manager, &target, &platform)).await?;
        }
        Ok(())
    }

    /// Lists `subject`'s referrers (live, plus any markers the local sources carry), copies each manifest and its
    /// blobs, records a marker per referrer and `.complete` once the listing was read in full.
    async fn export_referrers(
        &self,
        manager: &PackageManager,
        registry: &str,
        subject: &Digest,
        transport: Option<&OciIdentifier>,
        depth: usize,
        seen: &mut BTreeSet<Digest>,
    ) -> Result<()> {
        if depth >= MAX_REFERRER_DEPTH {
            return Err(ClientError::TraversalLimitExceeded {
                limit_kind: TraversalLimit::ReferrerDepth,
                limit: MAX_REFERRER_DEPTH,
                actual: depth.saturating_add(1),
                subject: subject.to_string(),
            }
            .into());
        }
        let local = self
            .lookup
            .referrers(registry, subject)
            .await
            .map_err(|error| self.lookup_error(error))?;
        let mut digests: Vec<Digest> = local.digests.clone();
        let mut complete = local.complete;
        if let (Some(client), Some(transport)) = (manager.client(), transport) {
            let image = client.read_reference(
                &transport.without_specifiers(),
                ocx_oci::client::ReadAddressing::Canonical,
            );
            match client
                .transport()
                .list_referrers_with_fallback(&image, subject, None)
                .await
            {
                Ok(listing) => {
                    complete = true;
                    for descriptor in listing.descriptors {
                        let digest = Digest::try_from(descriptor.digest.as_str())?;
                        if !digests.contains(&digest) {
                            digests.push(digest);
                        }
                    }
                }
                Err(error) if !digests.is_empty() || complete => {
                    log::warn!("referrers of {subject} not listed ({error}); using the ones found locally");
                    complete = false;
                }
                Err(error) => return Err(error.into()),
            }
        } else if manager.is_offline() && !complete {
            // Offline with no complete local listing would export a silent subset; `--no-referrers` opts out.
            log::error!("referrers of {subject} cannot be listed offline; pass --no-referrers to skip them");
            return Err(Error::OfflineMode);
        }

        for referrer in digests {
            if seen.len() >= MAX_REFERRERS_PER_SUBJECT {
                return Err(ClientError::TraversalLimitExceeded {
                    limit_kind: TraversalLimit::ReferrersPerLeaf,
                    limit: MAX_REFERRERS_PER_SUBJECT,
                    actual: seen.len().saturating_add(1),
                    subject: subject.to_string(),
                }
                .into());
            }
            if !seen.insert(referrer.clone()) {
                continue;
            }
            let named = PackageRef::new_registry("", registry).clone_with_digest(referrer.clone());
            let fetch = match transport {
                Some(location) => Ok(Fetch::Manifest {
                    identifier: location.clone_with_digest(referrer.clone()),
                }),
                None => Err(missing_blob(manager, &named)),
            };
            self.copy_blob(manager, registry, &referrer, fetch, missing_blob(manager, &named))
                .await?;
            let bytes = self
                .read_local(registry, &referrer)
                .await?
                .ok_or_else(|| missing_blob(manager, &named))?;
            match serde_json::from_slice::<ocx_oci::Manifest>(&bytes).map_err(Error::SerializationFailure)? {
                ocx_oci::Manifest::Image(image) => {
                    for descriptor in std::iter::once(&image.config).chain(image.layers.iter()) {
                        let digest = Digest::try_from(descriptor.digest.as_str())?;
                        let fetch = match transport {
                            Some(location) => Ok(Fetch::Blob {
                                identifier: location.pinned_at(digest.clone()),
                                descriptor: descriptor.clone(),
                            }),
                            None => Err(missing_blob(manager, &named)),
                        };
                        self.copy_blob(manager, registry, &digest, fetch, missing_blob(manager, &named))
                            .await?;
                    }
                }
                ocx_oci::Manifest::ImageIndex(_) => {
                    log::warn!("referrer {referrer} of {subject} is an image index; its children are not exported");
                    continue;
                }
            }
            self.write_referrer_marker(registry, subject, Some(&referrer)).await?;
            self.referrer_count.fetch_add(1, Ordering::Relaxed);
            Box::pin(self.export_referrers(manager, registry, &referrer, transport, depth + 1, seen)).await?;
        }
        if complete {
            self.write_referrer_marker(registry, subject, None).await?;
        }
        Ok(())
    }

    async fn write_referrer_marker(&self, registry: &str, subject: &Digest, referrer: Option<&Digest>) -> Result<()> {
        let (root, registry, subject, referrer) = (
            self.out_blobs.clone(),
            registry.to_string(),
            subject.clone(),
            referrer.cloned(),
        );
        let path = root.clone();
        tokio::task::spawn_blocking(move || match referrer {
            Some(referrer) => markers::write_marker(&root, &registry, &subject, &referrer),
            None => markers::write_complete(&root, &registry, &subject),
        })
        .await
        .map_err(|join| join_error(&path, join))?
        .map_err(|error| self.lookup_error(error))
    }

    /// A verified metadata blob from the export, the home or a configured directory.
    async fn read_local(&self, registry: &str, digest: &Digest) -> Result<Option<Vec<u8>>> {
        self.lookup
            .read_metadata(registry, digest, MAX_MANIFEST_BYTES)
            .await
            .map_err(|error| self.lookup_error(error))
    }

    /// Puts one blob into `DIR/blobs`: the export itself, the home and the configured directories in order,
    /// then `fetch`. `missing` is the verdict when nothing local holds it and there is nothing to fetch from.
    async fn copy_blob(
        &self,
        manager: &PackageManager,
        registry: &str,
        digest: &Digest,
        fetch: std::result::Result<Fetch, Error>,
        missing: Error,
    ) -> Result<WriteOutcome> {
        self.note_registry(registry)?;
        let mut walk = self.lookup.walk(registry, digest);
        while let Some(stream) = walk.next_source().await.map_err(|error| self.lookup_error(error))? {
            let origin = stream.origin;
            let size = stream.size;
            let writer = self.writer_for(registry, digest).await?;
            match copy_stream(stream.reader, writer).await {
                Ok(outcome) => return Ok(self.count(outcome, size)),
                Err(LookupError::DigestMismatch { .. }) => walk.reject(origin).await,
                Err(error) => return Err(self.lookup_error(error)),
            }
        }
        // A bad directory copy outranks "absent": the verdict is the corruption, not the missing source.
        let local = walk.exhausted().map_err(|error| self.lookup_error(error));
        let (fetch, client) = match (fetch, manager.client()) {
            (Ok(fetch), Some(client)) => (fetch, client),
            (Err(error), _) => {
                local?;
                return Err(error);
            }
            (Ok(_), None) => {
                local?;
                return Err(missing);
            }
        };
        let writer = self.writer_for(registry, digest).await?;
        let fetched = match fetch {
            Fetch::Blob { identifier, descriptor } => {
                let size = u64::try_from(descriptor.size).unwrap_or(0);
                client
                    .pull_blob_into(&identifier, &descriptor, writer)
                    .await
                    .map(|outcome| (outcome, size))
            }
            Fetch::Manifest { identifier } => match client.fetch_manifest_raw_bytes(&identifier).await {
                Ok(Some((bytes, _, _))) => {
                    let size = bytes.len() as u64;
                    write_bytes(writer, bytes)
                        .await
                        .map(|outcome| (outcome, size))
                        .map_err(|error| ClientError::Internal(Box::new(error)))
                }
                Ok(None) => return Err(missing),
                Err(error) => Err(error),
            },
        };
        match fetched {
            Ok((outcome, size)) => Ok(self.count(outcome, size)),
            Err(error) => {
                local?;
                Err(error.into())
            }
        }
    }

    /// The restricted index for every tagged or bare package, one root per repository.
    async fn write_index(
        &self,
        manager: &PackageManager,
        packages: &[PackageRef],
        exported: &mut [ExportedPackage],
    ) -> Result<()> {
        let mut tags: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
        for package in packages.iter().filter(|package| package.digest().is_none()) {
            tags.entry((package.registry().to_string(), package.repository().to_string()))
                .or_default()
                .insert(package.tag_or_latest().to_string());
        }
        if tags.is_empty() {
            return Ok(());
        }
        let home = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: manager.effective_index_store(),
        });
        let out = ocx_index::IndexStore::new(self.out_blobs.with_file_name("index"));
        let mut rows: BTreeMap<(String, String), Vec<TagRow>> = BTreeMap::new();
        for ((registry, repository), names) in tags {
            let identifier = PackageRef::new_registry(&repository, &registry);
            let names: Vec<String> = names.into_iter().collect();
            let written = ocx_index::export::write_export_index(&home, &out, &identifier, &names).await?;
            rows.insert((registry, repository), written);
        }
        for (package, entry) in packages.iter().zip(exported.iter_mut()) {
            if package.digest().is_some() {
                continue;
            }
            let key = (package.registry().to_string(), package.repository().to_string());
            let tag = package.tag_or_latest();
            entry.tags = Some(
                rows.get(&key)
                    .map(|written| written.iter().filter(|row| row.tag == tag).cloned().collect())
                    .unwrap_or_default(),
            );
        }
        Ok(())
    }

    /// The last file a run writes: a directory without it is not an export.
    async fn write_marker(&self) -> Result<()> {
        let marker = self
            .marker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let dir = self.out_blobs.parent().map(Path::to_path_buf).unwrap_or_default();
        let path = dir.clone();
        tokio::task::spawn_blocking(move || export_marker::write_marker(&dir, &marker))
            .await
            .map_err(|join| join_error(&path, join))?
            .map_err(|error| self.lookup_error(error))
    }
}

/// Streams a local `data` file into `writer`, which verifies the digest before publishing.
async fn copy_stream(file: tokio::fs::File, writer: BlobWriter) -> std::result::Result<WriteOutcome, LookupError> {
    if writer.is_present() {
        return writer.finish();
    }
    let file = file.into_std().await;
    let root = writer.root().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let (mut file, mut writer) = (file, writer);
        let io = |source| LookupError::Io {
            path: root.clone(),
            source,
        };
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(io)?;
            if read == 0 {
                break;
            }
            writer.write(&buffer[..read]).map_err(io)?;
        }
        writer.finish()
    })
    .await
    .map_err(|join| LookupError::Io {
        path: PathBuf::new(),
        source: std::io::Error::other(join),
    })?
}

async fn write_bytes(writer: BlobWriter, bytes: Vec<u8>) -> std::result::Result<WriteOutcome, LookupError> {
    tokio::task::spawn_blocking(move || {
        let mut writer = writer;
        let root = writer.root().to_path_buf();
        writer
            .write(&bytes)
            .map_err(|source| LookupError::Io { path: root, source })?;
        writer.finish()
    })
    .await
    .map_err(|join| LookupError::Io {
        path: PathBuf::new(),
        source: std::io::Error::other(join),
    })?
}

/// The verdict for a blob nowhere local: offline is a policy refusal, online it is simply absent.
fn missing_blob(manager: &PackageManager, package: &PackageRef) -> Error {
    if manager.is_offline() {
        Error::OfflineMode
    } else {
        Error::package(package.clone(), PackageErrorKind::NotFound)
    }
}

fn resolve_error(manager: &PackageManager, package: &PackageRef, kind: PackageErrorKind) -> Error {
    if manager.is_offline()
        && matches!(
            kind,
            PackageErrorKind::NotFound | PackageErrorKind::OfflineManifestMissing(_)
        )
    {
        return Error::OfflineMode;
    }
    Error::package(package.clone(), kind)
}

/// Resolves `package` once per platform to export: the named ones, or every platform an image index lists.
async fn resolve_targets(
    manager: &PackageManager,
    package: &PackageRef,
    platforms: &[Platform],
) -> std::result::Result<Vec<ResolvedChain>, PackageErrorKind> {
    let wanted: Vec<Platform> = if platforms.is_empty() {
        let (_, top) = resolve_top_manifest(manager.index(), package, ocx_index::IndexOperation::Resolve).await?;
        match top {
            ocx_oci::Manifest::Image(_) => vec![Platform::any()],
            ocx_oci::Manifest::ImageIndex(index) => index
                .manifests
                .iter()
                .map(|entry| Platform::try_from(entry.platform.clone()).unwrap_or_else(|_| Platform::any()))
                .collect(),
        }
    } else {
        platforms.to_vec()
    };
    let mut targets: Vec<ResolvedChain> = Vec::new();
    for platform in wanted {
        let resolved = manager.resolve(package, platform).await?;
        if !targets
            .iter()
            .any(|known| known.pinned.digest() == resolved.pinned.digest())
        {
            targets.push(resolved);
        }
    }
    Ok(targets)
}

/// Puts the image index a resolve recorded in the local index into the home blobs, which no digest read serves
/// from a source that answers only by tag.
async fn stage_dispatch_from_index(manager: &PackageManager, resolved: &ResolvedChain) -> Result<()> {
    let Some(blob) = resolved
        .chain
        .iter()
        .find(|blob| blob.role == super::resolve::ChainRole::Index)
    else {
        return Ok(());
    };
    let (registry, digest) = (blob.identifier.registry(), blob.identifier.digest());
    let blobs = &manager.file_structure().blobs;
    if blobs.read_blob(registry, &digest).await?.is_some() {
        return Ok(());
    }
    let object = manager
        .effective_index_store()
        .read_dispatch_object(registry, blob.identifier.repository(), &digest)
        .await?;
    if let Some(bytes) = object.filter(|bytes| digest.algorithm().hash(bytes) == digest) {
        blobs.write_blob(registry, &digest, &bytes).await?;
    }
    Ok(())
}

/// Writes the index document a tag names into the home blobs, when it is the digest the snapshot froze.
async fn stage_index_by_tag(manager: &PackageManager, by_tag: &PackageRef, expected: &Digest) -> Result<()> {
    let Some((bytes, digest, _)) = manager.index().fetch_manifest_raw_bytes(by_tag).await? else {
        return Ok(());
    };
    if digest != *expected || digest.algorithm().hash(&bytes) != *expected {
        return Ok(());
    }
    manager
        .file_structure()
        .blobs
        .write_blob(by_tag.registry(), expected, &bytes)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use ocx_exit::{ClassifyExitCode, ExitCode};
    use ocx_oci::PackageRef;
    use tempfile::TempDir;

    use super::*;
    use crate::Error;
    use crate::test_support::export_fixture::{
        Package, REGISTRY, REPO, linux_amd64, offline_manager, online_manager, tagged,
    };

    fn request(output: &std::path::Path, packages: Vec<PackageRef>) -> ExportRequest {
        ExportRequest {
            output: output.to_path_buf(),
            packages,
            lock_pins: Vec::new(),
            platforms: vec![linux_amd64()],
            // The fixture managers carry no client, so a referrer listing could never complete.
            referrers: false,
        }
    }

    fn exit_of(error: &Error) -> Option<ExitCode> {
        error.classify()
    }

    /// A home that pins `TAG` and holds every blob of the layered package, with a source for the tag.
    async fn complete_home(dir: &TempDir, package: &Package) -> PackageManager {
        let manager = online_manager(dir.path(), package.source());
        package.write_blobs(&manager.file_structure().blobs, true).await;
        manager
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn digest_needs_one_platform() {
        let home = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let package = Package::new();
        let manager = complete_home(&home, &package).await;
        let by_digest = PackageRef::new_registry(REPO, REGISTRY).clone_with_digest(package.leaf.clone());
        let mut none = request(out.path(), vec![by_digest.clone()]);
        none.platforms = Vec::new();
        let mut two = request(out.path(), vec![by_digest]);
        two.platforms = vec![linux_amd64(), "linux/arm64".parse().unwrap()];

        for refused in [none, two] {
            let error = manager
                .export(refused)
                .await
                .expect_err("a digest needs exactly one platform");
            assert_eq!(exit_of(&error), Some(ExitCode::UsageError), "{error}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn offline_missing_archive_is_81() {
        let home = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let package = Package::new();
        package.seed_home(home.path()).await;

        let error = offline_manager(home.path())
            .export(request(out.path(), vec![tagged()]))
            .await
            .expect_err("the layer archive is nowhere local");

        assert_eq!(exit_of(&error), Some(ExitCode::PolicyBlocked), "{error}");
        assert!(
            !out.path().join("ocx-export.json").is_file(),
            "a failed run writes no marker"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn offline_unknown_tag_is_81() {
        let home = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();

        let error = offline_manager(home.path())
            .export(request(out.path(), vec![tagged()]))
            .await
            .expect_err("an empty offline home pins no tag");

        assert_eq!(exit_of(&error), Some(ExitCode::PolicyBlocked), "{error}");
        assert!(
            !out.path().join("ocx-export.json").is_file(),
            "a failed run writes no marker"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn first_sight_tag_commits_home_pin() {
        let home = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let package = Package::new();
        let manager = complete_home(&home, &package).await;

        manager.export(request(out.path(), vec![tagged()])).await.unwrap();

        let pinned = manager
            .index()
            .fetch_manifest_digest(&tagged(), ocx_index::IndexOperation::Query)
            .await
            .unwrap();
        assert_eq!(
            pinned,
            Some(package.dispatch),
            "exporting a first-sight tag pins it in the home"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn slug_collision_refused() {
        let home = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let package = Package::new();
        let manager = complete_home(&home, &package).await;
        std::fs::write(
            out.path().join("ocx-export.json"),
            r#"{"format_version":1,"registries":{"Example.com":"Example.com"}}"#,
        )
        .unwrap();

        let error = manager
            .export(request(out.path(), vec![tagged()]))
            .await
            .expect_err("'example.com' folds onto 'Example.com'");

        assert!(matches!(error, Error::ExportDirInvalid { .. }), "{error}");
        assert_eq!(exit_of(&error), Some(ExitCode::DataError));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registry_slugged_locks_refused() {
        let home = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let package = Package::new();
        let manager = complete_home(&home, &package).await;
        let reserved = PackageRef::new_registry(REPO, "locks").clone_with_tag("3.28");

        let error = manager
            .export(request(out.path(), vec![reserved]))
            .await
            .expect_err("import refuses a registry slugged 'locks'");

        assert!(matches!(error, Error::ExportDirInvalid { .. }), "{error}");
        assert!(
            error.to_string().contains("'locks'"),
            "names the reserved slug: {error}"
        );
        assert_eq!(exit_of(&error), Some(ExitCode::DataError));
        assert!(
            !out.path().join("ocx-export.json").is_file(),
            "a refused run writes no marker"
        );
        assert!(
            !out.path().join("blobs").join("locks").exists(),
            "the refusal comes before any blob is written"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn marker_written_last() {
        let home = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let package = Package::new();
        let manager = complete_home(&home, &package).await;

        manager.export(request(out.path(), vec![tagged()])).await.unwrap();

        let marker = std::fs::metadata(out.path().join("ocx-export.json"))
            .expect("marker written")
            .modified()
            .unwrap();
        let catalog =
            std::fs::metadata(ocx_index::IndexStore::new(out.path().join("index")).source_catalog_path(REGISTRY))
                .expect("catalog written")
                .modified()
                .unwrap();
        assert!(marker >= catalog, "the marker is the last file a run writes");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn root_export_writes_no_index() {
        let home = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let package = Package::new();
        let manager = complete_home(&home, &package).await;
        let mut pins = request(out.path(), Vec::new());
        pins.lock_pins = vec![LockPin {
            identifier: package.leaf_id(),
            platform: linux_amd64(),
        }];

        let outcome = manager.export(pins).await.unwrap();

        assert!(!out.path().join("index").exists(), "lock pins write no index");
        assert!(out.path().join("ocx-export.json").is_file());
        assert!(outcome.packages.iter().all(|exported| exported.tags.is_none()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn snapshot_companions_exported() {
        let home = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let package = Package::layerless();
        let snapshot = crate::patch::PatchSnapshot {
            version: crate::patch::SnapshotVersion::CURRENT,
            companions: [(
                crate::patch::snapshot::companion_key(&tagged()),
                package.dispatch.clone(),
            )]
            .into(),
            descriptors: Default::default(),
        };
        let manager = online_manager(home.path(), package.source()).with_patch_snapshot(Some(snapshot));
        package.write_blobs(&manager.file_structure().blobs, false).await;

        manager.export(request(out.path(), Vec::new())).await.unwrap();

        let leaf = ocx_oci::blob_lookup::blob_data_path(&out.path().join("blobs"), REGISTRY, &package.leaf);
        assert!(leaf.is_file(), "a snapshot-pinned companion travels with the export");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn offline_without_complete_referrer_listing_is_81() {
        let home = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let package = Package::new();
        package.seed_home(home.path()).await;
        let manager = offline_manager(home.path());
        package.write_blobs(&manager.file_structure().blobs, true).await;

        let mut with_referrers = request(out.path(), vec![tagged()]);
        with_referrers.referrers = true;
        let error = manager
            .export(with_referrers)
            .await
            .expect_err("no complete local referrer listing offline");

        assert_eq!(exit_of(&error), Some(ExitCode::PolicyBlocked), "{error}");
        assert!(matches!(error, Error::OfflineMode), "{error}");
        manager
            .export(request(out.path(), vec![tagged()]))
            .await
            .expect("--no-referrers opts out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shard_collision_is_export_dir_invalid() {
        let home = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let package = Package::new();
        let manager = complete_home(&home, &package).await;
        let blobs = out.path().join("blobs");
        let shard = ocx_oci::blob_lookup::blob_dir_path(&blobs, REGISTRY, &package.leaf);
        std::fs::create_dir_all(&shard).unwrap();
        std::fs::write(shard.join("data"), b"not the leaf manifest").unwrap();
        std::fs::write(shard.join("digest"), format!("sha256:{}", "0".repeat(64))).unwrap();

        let error = manager
            .export(request(out.path(), vec![tagged()]))
            .await
            .expect_err("another digest owns the shard");

        assert!(matches!(error, Error::ExportDirInvalid { .. }), "{error}");
        assert_eq!(exit_of(&error), Some(ExitCode::DataError));
    }

    #[tokio::test]
    async fn present_destination_is_not_read_again() {
        use sha2::Digest as _;
        let dir = TempDir::new().unwrap();
        let bytes = b"layer bytes";
        let digest = Digest::Sha256(hex::encode(sha2::Sha256::digest(bytes)));
        let mut writer = BlobWriter::root_for(dir.path(), REGISTRY, &digest).unwrap();
        writer.write(bytes).unwrap();
        writer.finish().unwrap();
        // A write-only handle fails on any read, so reaching the source at all turns this red.
        let source = tokio::fs::File::create(dir.path().join("probe")).await.unwrap();

        let writer = BlobWriter::root_for(dir.path(), REGISTRY, &digest).unwrap();
        let outcome = copy_stream(source, writer).await.unwrap();

        assert_eq!(outcome, WriteOutcome::AlreadyPresent);
    }
}
