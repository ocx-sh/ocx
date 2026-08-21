// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Registry-to-registry transfer of an already-published package.
//!
//! The unit of transfer is a **leaf platform manifest**: its bytes, the blobs it
//! references, and the referrer manifests anchored to it. The leaf is copied
//! verbatim — never rebuilt — because its digest is load-bearing twice over. A
//! Sigstore bundle's subject *is* that digest (`oci/sign/pipeline.rs`), and a
//! V2/V3 `ocx.lock` pins it (`adr_lock_records_physical_address.md`), so a
//! promotion that re-serialised the manifest would orphan every signature and
//! invalidate every downstream pin while looking like it had worked.
//!
//! Index merging, rolling tags and canonical tags are deliberately *not* here:
//! an index is a mutable per-platform set, not content, and merging it is
//! [`Client::merge_platform_into_index`]'s job. This module only ever adds
//! content the target did not have. See `adr_package_copy.md`.

use std::collections::BTreeSet;
use std::path::Path;

use futures::StreamExt as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::client::error::{ClientError, TraversalLimit};
use super::client::hashing_reader::HashingAsyncReader;
use super::client::{ReadAddressing, no_progress};
use super::{Client, Digest, Identifier};
use crate::log;

/// Concurrent blob transfers per leaf.
///
/// Each in-flight transfer holds one spooled file, not the blob's bytes, so the
/// bound is on registry connections and disk, not memory. Matched to the pull
/// path's own layer concurrency rather than tuned independently.
const MAX_CONCURRENT_BLOB_TRANSFERS: usize = 4;

/// How far a referrer chain is followed. A signature over an SBOM is a referrer
/// of a referrer, which is depth 2; beyond a handful of levels the chain is a
/// hostile registry's construction rather than a real attestation graph.
const MAX_REFERRER_DEPTH: usize = 8;

/// Ceiling on referrer manifests copied per leaf, across the whole chain.
const MAX_REFERRERS_PER_LEAF: usize = 256;

/// Distinct blobs one manifest may name — its config plus its layers.
///
/// The OCI spec sets no ceiling, so a source can name hundreds of thousands of
/// descriptors inside the 32 MiB manifest cap and buy one HEAD against the
/// target for each. 512 is two orders above anything ocx publishes; a manifest
/// past it is not a package, so this is not configurable (PKG-06).
const MAX_BLOBS_PER_MANIFEST: usize = 512;

/// Largest blob a copy will spool to disk, taken from its own declared size.
///
/// The real bound is the byte count actually written, which is what catches a
/// source that lies downward. This one refuses a source that lies *upward*
/// before a single byte is fetched, so an absurd declared size costs nothing
/// (PKG-07). 8 GiB is an order above the largest toolchain layer ocx ships.
const MAX_COPIED_BLOB_BYTES: u64 = 8 << 30;

/// One blob to move, with the size its descriptor declared.
///
/// The size travels with the digest rather than being re-derived: it is the
/// ceiling the spooled write is bounded by, and dropping it is what left the
/// spool unbounded.
///
/// `size` is already clamped against [`MAX_COPIED_BLOB_BYTES`]. The only
/// constructor is [`blob_set`], which is where the descriptor is read, so an
/// absurd declaration is refused there — before any blob in the set has been
/// HEADed, let alone fetched — rather than per-blob inside a fan-out whose
/// siblings have already started uploading.
#[derive(Debug, Clone)]
struct BlobRef {
    digest: Digest,
    size: u64,
}

/// What happened to one blob.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BlobTransfers {
    /// Already present at the target — nothing transferred.
    pub present: usize,
    /// Mounted from another repository in the same registry.
    pub mounted: usize,
    /// Downloaded from the source and uploaded to the target.
    pub uploaded: usize,
}

impl BlobTransfers {
    fn record(&mut self, outcome: BlobOutcome) {
        match outcome {
            BlobOutcome::Present => self.present += 1,
            BlobOutcome::Mounted => self.mounted += 1,
            BlobOutcome::Uploaded => self.uploaded += 1,
        }
    }
}

impl std::ops::AddAssign for BlobTransfers {
    fn add_assign(&mut self, other: Self) {
        self.present += other.present;
        self.mounted += other.mounted;
        self.uploaded += other.uploaded;
    }
}

#[derive(Debug, Clone, Copy)]
enum BlobOutcome {
    Present,
    Mounted,
    Uploaded,
}

/// The result of copying one leaf.
#[derive(Debug)]
pub struct LeafCopy {
    /// The leaf's digest — unchanged by construction, returned so the caller can
    /// merge it into the target's index without re-reading it.
    pub digest: Digest,
    /// The leaf manifest's size in bytes, for the index entry's descriptor.
    pub size: i64,
    pub blobs: BlobTransfers,
    /// Referrer manifests copied, across the whole chain.
    pub referrers: usize,
}

/// Copies one leaf platform manifest, its blobs and its referrers.
///
/// `source` and `target` are repository identifiers; any tag they carry is
/// ignored, because a leaf is addressed by digest on both ends.
///
/// # Addressing
///
/// Every source read asks for [`ReadAddressing::Canonical`]. A copy's read
/// *becomes* the bytes written to the target, which is exactly the case
/// `subsystem-oci.md` invariant #5 names: deciding from a mirror and applying to
/// the canonical registry is CWE-345/367. Here it is sharper still — a poisoned
/// mirror would be choosing what lands in production under a promoted tag.
///
/// `scratch_root` is the directory the blob spool is created under, and it is
/// deliberately not optional. `$TMPDIR` is memory-backed on most Linux hosts,
/// so a caller allowed to leave this unset would silently defeat the bound the
/// spool exists to enforce — the cap bounds the file, not the medium it lands
/// in. Tests pass `scratch_dir().path()`.
///
/// # Errors
///
/// [`ClientError::DigestMismatch`] when the source serves bytes that do not hash
/// to the digest asked for, or a manifest filed under a digest other than the
/// one requested; [`ClientError::ManifestNotFound`] when the leaf is absent at
/// the source; [`ClientError::ReferrersUnsupported`] when referrers were
/// requested and either registry lacks the OCI 1.1 Referrers API;
/// [`ClientError::TraversalLimitExceeded`] when the source's blob set or
/// referrer graph is larger than this copy will traverse; and any transport
/// error from the underlying registry calls.
pub async fn copy_leaf(
    client: &Client,
    source: &Identifier,
    target: &Identifier,
    leaf_digest: &Digest,
    include_referrers: bool,
    scratch_root: &Path,
) -> Result<LeafCopy, ClientError> {
    let scratch = tempfile::tempdir_in(scratch_root).map_err(|e| ClientError::Io {
        path: scratch_root.to_path_buf(),
        source: e,
    })?;
    let transfer = Transfer {
        client,
        source,
        target,
        scratch: scratch.path(),
    };

    let source_leaf = source.without_tag().clone_with_digest(leaf_digest.clone());
    let (leaf_bytes, digest, manifest) = client
        .fetch_manifest_raw_bytes_addressed(&source_leaf, ReadAddressing::Canonical)
        .await?
        .ok_or_else(|| ClientError::ManifestNotFound(source_leaf.to_string()))?;
    // The fetch checks the bytes against the digest the registry claims for
    // them, which is self-consistency, not identity. Comparing against the
    // digest actually asked for is what stops a source answering a request for
    // A with a coherent manifest for B: B would be pushed to the target while
    // the caller merges A into the target's index, leaving an index entry
    // naming a manifest nobody uploaded, reported as success (CWE-345).
    //
    // A backstop, not a second independent control: `source_leaf` pins
    // `leaf_digest`, and `fetch_manifest_raw_bytes_addressed` already refuses a
    // served digest that differs from a pinned one, so this branch is unreachable
    // as wired. It is kept against a refactor that stops pinning here, or that
    // relaxes the client-layer check — do not read its always-green state as
    // tested coverage.
    if &digest != leaf_digest {
        return Err(ClientError::DigestMismatch {
            expected: leaf_digest.to_string(),
            actual: digest.to_string(),
        });
    }

    // A promoted artifact is one *platform*, so an index here means the caller
    // handed us a dispatch object rather than content. Refusing is not a
    // convenience: merging an index is a per-platform decision the caller has to
    // make against the target's current entries, and byte-copying one would
    // silently delete every target platform the source lacks.
    let super::Manifest::Image(image) = &manifest else {
        return Err(ClientError::InvalidManifest(format!(
            "{source_leaf} is an image index, not a platform manifest"
        )));
    };

    let blobs = transfer.copy_blobs(&blob_set(image, &source_leaf)?).await?;

    // The manifest PUT is digest-addressed, so a spec-compliant registry stores
    // these exact bytes under this exact digest or rejects the request outright.
    // That is the integrity guarantee — `push_manifest_raw` answers with the
    // pullable manifest URL, not a digest, so there is nothing here to compare.
    let target_leaf = client.transport_write_reference(&target.without_tag().clone_with_digest(digest.clone()));
    client
        .transport()
        .ensure_auth(&target_leaf, super::RegistryOperation::Push)
        .await?;
    client
        .transport()
        .push_manifest_raw(&target_leaf, leaf_bytes.clone(), manifest.content_type())
        .await?;
    log::debug!("Copied leaf manifest {digest} to {target_leaf}");

    let referrers = if include_referrers {
        // Probed here rather than up front: a registry answers the referrers
        // endpoint for a subject it holds, and until the line above the target
        // did not hold this one. The leaf is a pure addition — digest-addressed,
        // untagged, invisible until a tag names it — so refusing at this point
        // still leaves every tag the target publishes exactly as it was.
        //
        // Probed even when the source turns out to carry nothing: a registry
        // without the OCI 1.1 Referrers API accepts a referrer manifest as an
        // ordinary PUT and then never lists it, so the loss is silent, and
        // making the refusal depend on whether this particular package happens
        // to be signed is the least predictable contract available.
        transfer.ensure_target_serves_referrers(&digest).await?;
        let mut seen = BTreeSet::new();
        transfer.copy_referrers(&digest, 0, &mut seen).await?
    } else {
        0
    };

    let size = i64::try_from(leaf_bytes.len())
        .map_err(|_| ClientError::InvalidManifest(format!("manifest size {} exceeds i64::MAX", leaf_bytes.len())))?;

    Ok(LeafCopy {
        digest,
        size,
        blobs,
        referrers,
    })
}

/// The two endpoints and the scratch directory one leaf copy works between.
///
/// Five functions threaded `(client, source, target, …, scratch)` in that order
/// (ARCH-01). `source` and `target` are the same type, so transposing them is a
/// one-token edit with no compiler objection behind it — a copy that reads from
/// the target and writes to the source. Binding them once, in the one function
/// that knows which is which, makes the transposition unrepresentable.
struct Transfer<'a> {
    client: &'a Client,
    source: &'a Identifier,
    target: &'a Identifier,
    scratch: &'a Path,
}

impl Transfer<'_> {
    /// Refuses a target that cannot serve the OCI 1.1 Referrers API.
    async fn ensure_target_serves_referrers(&self, subject: &Digest) -> Result<(), ClientError> {
        use super::referrer::{ReferrersApiCapability, ReferrersSupport};

        let image = self.client.transport_write_reference(self.target);
        // Not cached: the capability cache lives in the state store, which this
        // layer does not hold, and one probe per promotion is not a hot path.
        match ReferrersApiCapability::probe(self.client.transport(), &image, subject)
            .await?
            .supported
        {
            ReferrersSupport::Supported => Ok(()),
            ReferrersSupport::Unsupported => Err(ClientError::ReferrersUnsupported {
                registry: image.resolve_registry().to_string(),
            }),
        }
    }

    /// Transfers every blob that is not already at the target, bounded.
    ///
    /// Fail-fast on the *report*, run-to-completion on the *work* (PKG-23): the
    /// first error is returned and the leaf is never pushed, but the tasks
    /// `buffer_unordered` already admitted finish rather than being dropped
    /// mid-upload. Cancelling a push abandons an open upload session at the
    /// target, and nothing the survivors do is wasted — a blob that landed is
    /// one the caller's next attempt finds already present.
    async fn copy_blobs(&self, blobs: &[BlobRef]) -> Result<BlobTransfers, ClientError> {
        let outcomes: Vec<Result<BlobOutcome, ClientError>> = futures::stream::iter(blobs)
            .map(|blob| self.copy_blob(blob))
            .buffer_unordered(MAX_CONCURRENT_BLOB_TRANSFERS)
            .collect()
            .await;

        let mut transfers = BlobTransfers::default();
        for outcome in outcomes {
            transfers.record(outcome?);
        }
        Ok(transfers)
    }

    async fn copy_blob(&self, blob: &BlobRef) -> Result<BlobOutcome, ClientError> {
        let transport = self.client.transport();
        let target_image = self.client.transport_write_reference(self.target);
        let digest = &blob.digest;

        transport
            .ensure_auth(&target_image, super::RegistryOperation::Push)
            .await?;
        if transport.head_blob(&target_image, digest).await.is_ok() {
            log::debug!("Blob {digest} already present at {target_image}");
            return Ok(BlobOutcome::Present);
        }

        // Cross-repository mount is same-registry only: the registry copies the blob
        // internally and nothing crosses this process at all. Across registries it is
        // not applicable, so do not even ask.
        if self.source.registry() == self.target.registry()
            && matches!(
                transport
                    .mount_blob(&target_image, self.source.repository(), digest)
                    .await?,
                super::client::MountOutcome::Mounted
            )
        {
            log::debug!(
                "Mounted blob {digest} from {} into {target_image}",
                self.source.repository()
            );
            return Ok(BlobOutcome::Mounted);
        }

        let spooled = self.spool(blob).await?;
        transport
            .push_blob_from_path(&target_image, &spooled, digest, no_progress())
            .await?;
        // The spool is scratch, not a cache: the next leaf's copy re-HEADs the target
        // and finds the blob present, so nothing here is worth keeping.
        let _ = tokio::fs::remove_file(&spooled).await; // best-effort; the TempDir sweeps it regardless
        Ok(BlobOutcome::Uploaded)
    }

    /// Streams one blob to `scratch/<hex>`, bounded and hashed in the same pass.
    ///
    /// Spooling through a file rather than a buffer is the point: a toolchain
    /// layer is routinely 100-200 MB and several are in flight, so holding them
    /// in RAM is the unbounded allocation PKG-04 exists to stop.
    ///
    /// The read is bounded by the descriptor's own declared size, which is what
    /// stops a source that under-declares a layer from filling the scratch
    /// filesystem (PKG-05, PKG-07). The hash runs over the same pass rather than
    /// in a second one: re-opening the file to re-hash it checks exactly the
    /// property this pass already established, and doubles the disk I/O of every
    /// promoted layer to do it.
    ///
    /// Verifying here rather than letting the target's own digest check catch it
    /// names the source registry — the party that actually served the wrong
    /// bytes — and does it before the upload rather than after (CWE-345).
    async fn spool(&self, blob: &BlobRef) -> Result<std::path::PathBuf, ClientError> {
        // Already clamped by `blob_set`, which is where the descriptor was read.
        let declared = blob.size;

        let source_image = self.client.read_reference(self.source, ReadAddressing::Canonical);
        let transport = self.client.transport();
        transport
            .ensure_auth(&source_image, super::RegistryOperation::Pull)
            .await?;

        let (_, hex) = blob.digest.parts();
        let path = self.scratch.join(hex);
        let mut file = tokio::fs::File::create(&path).await.map_err(|e| ClientError::Io {
            path: path.clone(),
            source: e,
        })?;

        let stream = transport.pull_blob_streaming(&source_image, &blob.digest).await?;
        // One byte past the declaration: an over-long body then reaches the
        // digest check as a genuine mismatch instead of being silently truncated
        // to the cap and hashed as if it were the whole blob.
        let mut hashing = HashingAsyncReader::new(stream.take(declared.saturating_add(1)), blob.digest.algorithm());
        tokio::io::copy(&mut hashing, &mut file)
            .await
            .map_err(|e| ClientError::Io {
                path: path.clone(),
                source: e,
            })?;
        file.flush().await.map_err(|e| ClientError::Io {
            path: path.clone(),
            source: e,
        })?;
        drop(file);

        let (actual, read) = hashing.finalize();
        // Completeness before content, the ordering `Client::pull_layer`
        // documents: a prefix cannot hash to the whole, so every truncated
        // transfer also fails the digest check and would otherwise be reported
        // as the source serving wrong bytes. An over-long body needs no arm of
        // its own — the extra byte admitted above lands it in the digest check,
        // which attributes it correctly.
        if read < declared {
            return Err(ClientError::ShortBlobRead {
                expected: declared,
                actual: read,
            });
        }
        if actual != blob.digest {
            return Err(ClientError::DigestMismatch {
                expected: blob.digest.to_string(),
                actual: actual.to_string(),
            });
        }
        Ok(path)
    }

    /// Copies every referrer anchored to `subject`, then recurses into each one.
    ///
    /// `seen` spans the whole chain, so a registry answering with a cycle — a
    /// referrer that is its own ancestor — terminates instead of recursing forever.
    ///
    /// Both caps are errors rather than warnings, and so is a referrer the source
    /// lists but cannot serve. A promotion that logged "stopping here" and then
    /// exited zero would leave the target holding an artifact whose signature was
    /// silently dropped: verifiable at the source, unverifiable at the target, and
    /// reported as a success (PKG-11). Nothing on this path may `continue` past a
    /// referrer it failed to copy.
    async fn copy_referrers(
        &self,
        subject: &Digest,
        depth: usize,
        seen: &mut BTreeSet<String>,
    ) -> Result<usize, ClientError> {
        if depth >= MAX_REFERRER_DEPTH {
            return Err(ClientError::TraversalLimitExceeded {
                limit_kind: TraversalLimit::ReferrerDepth,
                limit: MAX_REFERRER_DEPTH,
                actual: depth.saturating_add(1),
                subject: subject.to_string(),
            });
        }
        let transport = self.client.transport();
        let source_image = self.client.read_reference(self.source, ReadAddressing::Canonical);
        let target_image = self.client.transport_write_reference(self.target);

        let descriptors = transport.list_referrers(&source_image, subject, None).await?;
        let mut copied = 0usize;
        for descriptor in descriptors {
            if seen.len() >= MAX_REFERRERS_PER_LEAF {
                return Err(ClientError::TraversalLimitExceeded {
                    limit_kind: TraversalLimit::ReferrersPerLeaf,
                    limit: MAX_REFERRERS_PER_LEAF,
                    actual: seen.len().saturating_add(1),
                    subject: subject.to_string(),
                });
            }
            if !seen.insert(descriptor.digest.clone()) {
                continue;
            }
            let referrer_digest = parse_descriptor_digest(&descriptor.digest)?;
            let referrer_id = self.source.without_tag().clone_with_digest(referrer_digest.clone());
            let Some((bytes, digest, manifest)) = self
                .client
                .fetch_manifest_raw_bytes_addressed(&referrer_id, ReadAddressing::Canonical)
                .await?
            else {
                // Listed but absent: the source's referrers index and its manifest
                // store disagree. Skipping it with a warning is what the caps above
                // exist to rule out — the target would end up holding an artifact
                // whose signature stayed behind, reported as a complete promotion
                // and visible only in a log line `--quiet` suppresses (PKG-11).
                return Err(ClientError::InvalidManifest(format!(
                    "referrer {} is listed for {subject} but the source cannot serve it; \
                     re-run the copy, and if it persists the source registry's referrers \
                     index is stale",
                    descriptor.digest
                )));
            };
            // The same identity check the leaf gets: the fetch proves the bytes
            // hash to the digest the registry filed them under, not that it is
            // the digest the referrers listing named. Backstop only, for the same
            // reason the leaf's check is: `referrer_id` pins `referrer_digest`, so
            // the client layer refuses the mismatch first and this branch is
            // unreachable as wired.
            if digest != referrer_digest {
                return Err(ClientError::DigestMismatch {
                    expected: referrer_digest.to_string(),
                    actual: digest.to_string(),
                });
            }

            match &manifest {
                super::Manifest::Image(image) => {
                    self.copy_blobs(&blob_set(image, &referrer_id)?).await?;
                }
                // An index here names child manifests this copy never walks, so
                // pushing it would attach a referrer that resolves to nothing at
                // the target — a signature or SBOM present in a listing and
                // unfetchable behind it. Nothing in the wild produces one, which
                // is exactly why it would go unnoticed.
                super::Manifest::ImageIndex(_) => {
                    return Err(ClientError::InvalidManifest(format!(
                        "referrer {digest} of {subject} is an image index; its children are not copied"
                    )));
                }
            }

            transport
                .push_referrer_manifest(&target_image, subject, &bytes, manifest.content_type())
                .await?;
            copied = copied.saturating_add(1);
            log::debug!("Copied referrer {digest} of {subject} to {target_image}");

            copied =
                copied.saturating_add(Box::pin(self.copy_referrers(&digest, depth.saturating_add(1), seen)).await?);
        }
        Ok(copied)
    }
}

fn parse_descriptor_digest(digest: &str) -> Result<Digest, ClientError> {
    Digest::try_from(digest).map_err(|e| ClientError::InvalidManifest(format!("{e}")))
}

/// The distinct blobs one image manifest names — its config plus its layers.
///
/// Deduplicated because a manifest may legitimately name one digest twice (an
/// empty config reused as a layer is the common shape), and every entry spools
/// to `scratch/<hex>`: two concurrent tasks for one digest write and delete the
/// same path, so one truncates the file the other is still uploading.
fn blob_set(image: &super::ImageManifest, subject: &Identifier) -> Result<Vec<BlobRef>, ClientError> {
    let declared = image.layers.len().saturating_add(1);
    if declared > MAX_BLOBS_PER_MANIFEST {
        return Err(ClientError::TraversalLimitExceeded {
            limit_kind: TraversalLimit::BlobsPerManifest,
            limit: MAX_BLOBS_PER_MANIFEST,
            actual: declared,
            subject: subject.to_string(),
        });
    }

    // Sized from the already-clamped count, never from the raw declaration (PKG-04).
    let mut blobs = Vec::with_capacity(declared);
    let mut seen = BTreeSet::new();
    for descriptor in std::iter::once(&image.config).chain(image.layers.iter()) {
        let digest = parse_descriptor_digest(&descriptor.digest)?;
        // A declared size is a claim by the source, clamped here — the one place
        // the descriptor is read — so a set containing one absurd declaration is
        // refused whole, before any of its siblings starts transferring (PKG-07).
        let size = u64::try_from(descriptor.size)
            .ok()
            .filter(|size| *size <= MAX_COPIED_BLOB_BYTES)
            .ok_or(ClientError::LayerSizeExceeded {
                declared: descriptor.size,
                maximum: MAX_COPIED_BLOB_BYTES,
            })?;
        if seen.insert(digest.to_string()) {
            blobs.push(BlobRef { digest, size });
        }
    }
    Ok(blobs)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::MEDIA_TYPE_OCI_IMAGE_MANIFEST;
    use crate::oci::Algorithm;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};

    const CONFIG_BLOB: &[u8] = b"{\"package\":\"demo\"}";
    const LAYER_BLOB: &[u8] = b"a tar layer, pretend";
    const EMPTY_CONFIG: &[u8] = b"{}";
    const SBOM_PAYLOAD: &[u8] = b"an SPDX document, pretend";
    const SIGNATURE_PAYLOAD: &[u8] = b"a sigstore bundle, pretend";

    fn client_for(data: &StubTransportData) -> Client {
        Client::with_transport(Box::new(StubTransport::new(data.clone())))
    }

    fn identifier(registry: &str, repository: &str) -> Identifier {
        Identifier::new_registry(repository, registry)
    }

    /// The same seam production reads and writes through. Building the reference
    /// off `Identifier` directly is allow-listed away from this file (T-arch-A1),
    /// and going through the client is also what keeps these keys equal to the
    /// ones the engine builds.
    fn canonical(identifier: &Identifier) -> crate::oci::native::Reference {
        Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())))
            .read_reference(identifier, ReadAddressing::Canonical)
    }

    fn descriptor(media_type: &str, bytes: &[u8]) -> super::super::Descriptor {
        super::super::Descriptor {
            media_type: media_type.to_string(),
            digest: Algorithm::Sha256.hash(bytes).to_string(),
            size: bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        }
    }

    fn leaf_manifest() -> super::super::Manifest {
        super::super::Manifest::Image(super::super::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            config: descriptor("application/vnd.ocx.package.metadata.v1+json", CONFIG_BLOB),
            layers: vec![descriptor(crate::MEDIA_TYPE_TAR_GZ, LAYER_BLOB)],
            ..Default::default()
        })
    }

    /// A referrer manifest in the shape `ocx package sign` pushes: an empty
    /// config and the artifact as the single layer.
    fn referrer_manifest(artifact_type: &str, payload: &'static [u8]) -> super::super::Manifest {
        super::super::Manifest::Image(super::super::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            artifact_type: Some(artifact_type.to_string()),
            config: descriptor("application/vnd.oci.empty.v1+json", EMPTY_CONFIG),
            layers: vec![descriptor(artifact_type, payload)],
            ..Default::default()
        })
    }

    /// Files `manifest` and every blob it names in `repository`, returning its
    /// digest. The blob presence map lists that repository only, so a target
    /// elsewhere genuinely starts without the content — which is the difference
    /// this whole module exists to move.
    fn seed(
        data: &StubTransportData,
        repository: &Identifier,
        manifest: &super::super::Manifest,
        blobs: &[&'static [u8]],
    ) -> Digest {
        seed_raw(
            data,
            repository,
            &serde_json::to_vec(manifest).expect("serialize"),
            blobs,
        )
    }

    /// [`seed`] with the manifest bytes chosen by the caller.
    ///
    /// Exists because a fixture serde produced is exactly the fixture a
    /// re-serialising copy reproduces byte for byte — so seeding through
    /// [`seed`] leaves the verbatim-copy assertion unable to fail.
    fn seed_raw(data: &StubTransportData, repository: &Identifier, bytes: &[u8], blobs: &[&'static [u8]]) -> Digest {
        let digest = Algorithm::Sha256.hash(bytes);
        let key = canonical(&repository.without_tag().clone_with_digest(digest.clone())).to_string();
        let location = crate::oci::client::test_transport::blob_location_key(&canonical(repository));

        let mut inner = data.write();
        inner.capture_pushes = true;
        inner.manifests.insert(key, (bytes.to_vec(), digest.to_string()));
        let present = inner
            .blob_locations
            .get_or_insert_with(HashMap::new)
            .entry(location)
            .or_default();
        let mut seeded = Vec::new();
        for blob in blobs {
            let blob_digest = Algorithm::Sha256.hash(blob).to_string();
            present.insert(blob_digest.clone());
            seeded.push((blob_digest, blob.to_vec()));
        }
        for (blob_digest, bytes) in seeded {
            inner.blobs.insert(blob_digest, bytes);
        }
        digest
    }

    fn seed_source(data: &StubTransportData, source: &Identifier, manifest: &super::super::Manifest) -> Digest {
        seed(data, source, manifest, &[CONFIG_BLOB, LAYER_BLOB])
    }

    fn pushed_manifest(data: &StubTransportData, identifier: &Identifier, digest: &Digest) -> Option<Vec<u8>> {
        let key = canonical(&identifier.without_tag().clone_with_digest(digest.clone())).to_string();
        data.read().manifests.get(&key).map(|(bytes, _)| bytes.clone())
    }

    /// The load-bearing property: the target's manifest is byte-identical to the
    /// source's, so its digest is unchanged and every signature subject and lock
    /// pin naming it still resolves.
    ///
    /// The source is seeded **pretty-printed**, and that is what gives the
    /// assertion something to catch. Seeded through serde's compact writer, the
    /// fixture is exactly what a re-serialising copy would produce, so the
    /// comparison holds however the engine behaves — a green with no reachable
    /// red. Indented bytes are a shape serde's writer never emits, so a copy
    /// that parsed and re-encoded fails here on the first byte of whitespace.
    /// A scratch root for the blob spool, bound by the caller.
    ///
    /// Returned as a `TempDir` and not a bare path on purpose: the guard has to
    /// outlive the `copy_leaf` call, and `scratch_dir().path()` inline would
    /// drop it first (TEST-06).
    fn scratch_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("a scratch root for the blob spool")
    }

    #[tokio::test]
    async fn leaf_manifest_bytes_survive_the_copy_verbatim() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let non_canonical = serde_json::to_vec_pretty(&leaf_manifest()).expect("serialize");
        let leaf = seed_raw(&data, &source, &non_canonical, &[CONFIG_BLOB, LAYER_BLOB]);

        let copied = copy_leaf(&client_for(&data), &source, &target, &leaf, false, scratch.path())
            .await
            .expect("copy");

        assert_eq!(copied.digest, leaf, "the digest must not move");
        let target_bytes = pushed_manifest(&data, &target, &leaf).expect("target manifest");
        assert_eq!(target_bytes, non_canonical, "the bytes must survive verbatim");
        assert_ne!(
            non_canonical,
            serde_json::to_vec(&leaf_manifest()).expect("serialize"),
            "the fixture must differ from what a re-serialising copy would emit, \
             or the assertion above cannot fail"
        );
        assert_eq!(copied.size, non_canonical.len() as i64);
    }

    /// Both blobs have to move, and each has to move exactly once.
    #[tokio::test]
    async fn every_referenced_blob_is_uploaded_once() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let leaf = seed_source(&data, &source, &leaf_manifest());

        let copied = copy_leaf(&client_for(&data), &source, &target, &leaf, false, scratch.path())
            .await
            .expect("copy");

        assert_eq!(copied.blobs.uploaded, 2, "config plus one layer");
        assert_eq!(copied.blobs.present, 0);
        assert_eq!(copied.blobs.mounted, 0);
        let calls = data.read().calls.clone();
        for blob in [CONFIG_BLOB, LAYER_BLOB] {
            let digest = Algorithm::Sha256.hash(blob).to_string();
            assert_eq!(
                calls.iter().filter(|c| *c == &format!("push_blob:{digest}")).count(),
                1,
                "blob {digest} pushed exactly once, calls: {calls:?}"
            );
        }
    }

    /// A second copy must cost nothing: the blobs are already there, and the
    /// engine has to notice rather than re-uploading them.
    #[tokio::test]
    async fn a_second_copy_uploads_nothing() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let leaf = seed_source(&data, &source, &leaf_manifest());
        let client = client_for(&data);

        copy_leaf(&client, &source, &target, &leaf, false, scratch.path())
            .await
            .expect("first");
        data.write().calls.clear();
        let second = copy_leaf(&client, &source, &target, &leaf, false, scratch.path())
            .await
            .expect("second");

        assert_eq!(second.blobs.present, 2);
        assert_eq!(second.blobs.uploaded, 0);
        assert!(
            !data.read().calls.iter().any(|c| c.starts_with("push_blob:")),
            "no blob re-uploaded, calls: {:?}",
            data.read().calls
        );
    }

    /// Cross-repository mount is a same-registry facility. Asking for it across
    /// registries is not merely useless — the mount source names a repository on
    /// the target's host, so a registry that answered would serve the wrong blob.
    #[tokio::test]
    async fn mount_is_attempted_within_a_registry_and_never_across_one() {
        let scratch = scratch_dir();
        for (source_registry, target_registry, expect_mount) in [
            ("registry.example.com", "registry.example.com", true),
            ("dev.example.com", "prod.example.com", false),
        ] {
            let data = StubTransportData::new();
            let source = identifier(source_registry, "team/demo");
            let target = identifier(target_registry, "team/promoted");
            let leaf = seed_source(&data, &source, &leaf_manifest());

            copy_leaf(&client_for(&data), &source, &target, &leaf, false, scratch.path())
                .await
                .expect("copy");

            assert_eq!(
                !data.read().mount_calls.is_empty(),
                expect_mount,
                "{source_registry} -> {target_registry}"
            );
        }
    }

    /// An accepted mount must replace the transfer, not precede it.
    #[tokio::test]
    async fn an_accepted_mount_skips_the_upload() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("registry.example.com", "team/demo");
        let target = identifier("registry.example.com", "team/promoted");
        let leaf = seed_source(&data, &source, &leaf_manifest());
        data.write().mount_results = vec![
            Ok(crate::oci::client::MountOutcome::Mounted),
            Ok(crate::oci::client::MountOutcome::Mounted),
        ];

        let copied = copy_leaf(&client_for(&data), &source, &target, &leaf, false, scratch.path())
            .await
            .expect("copy");

        assert_eq!(copied.blobs.mounted, 2);
        assert_eq!(copied.blobs.uploaded, 0);
        assert!(!data.read().calls.iter().any(|c| c.starts_with("push_blob:")));
    }

    /// An image index is a mutable per-platform set, not content. Byte-copying
    /// one would delete every target platform the source lacks, so the engine
    /// refuses it — and refuses it before writing anything.
    #[tokio::test]
    async fn an_image_index_source_is_refused_before_any_write() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let index = super::super::Manifest::ImageIndex(super::super::ImageIndex {
            schema_version: super::super::INDEX_SCHEMA_VERSION,
            media_type: Some(crate::MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
            manifests: Vec::new(),
            artifact_type: None,
            annotations: None,
        });
        let digest = seed_source(&data, &source, &index);

        let error = copy_leaf(&client_for(&data), &source, &target, &digest, false, scratch.path())
            .await
            .expect_err("an index must be refused");

        assert!(
            matches!(error, ClientError::InvalidManifest(ref message) if message.contains("image index")),
            "unexpected error: {error}"
        );
        let calls = data.read().calls.clone();
        assert!(
            !calls.iter().any(|c| c.starts_with("push_")),
            "nothing may be written, calls: {calls:?}"
        );
    }

    /// A source serving bytes that do not hash to the digest asked for is the
    /// registry lying about content (CWE-345). The copy must stop there rather
    /// than relaying the bytes onward under a digest they do not have.
    #[tokio::test]
    async fn a_source_digest_mismatch_stops_the_copy() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let leaf = seed_source(&data, &source, &leaf_manifest());

        // Same key, different bytes: the registry now serves content that does not
        // match the digest it is filed under.
        let key = canonical(&source.without_tag().clone_with_digest(leaf.clone())).to_string();
        let tampered = serde_json::to_vec(&leaf_manifest()).map(|mut bytes| {
            bytes.extend_from_slice(b"   ");
            bytes
        });
        data.write()
            .manifests
            .insert(key, (tampered.expect("serialize"), leaf.to_string()));

        let error = copy_leaf(&client_for(&data), &source, &target, &leaf, false, scratch.path())
            .await
            .expect_err("a mismatch must fail");
        assert!(
            matches!(error, ClientError::DigestMismatch { .. }),
            "unexpected error: {error}"
        );
        assert!(
            pushed_manifest(&data, &target, &leaf).is_none(),
            "nothing may reach the target"
        );
    }

    /// A source may answer a request for one digest with a manifest that is
    /// perfectly self-consistent and simply *not the one asked for*. The fetch
    /// cannot catch it — checking bytes against the digest the registry filed
    /// them under proves consistency, not identity — so the copy has to.
    ///
    /// Left unchecked this is not a failed copy but a wrong one: the substituted
    /// manifest reaches the target while the caller merges the requested digest
    /// into the target's index, leaving an index entry that names a manifest
    /// nobody uploaded, reported as a success (CWE-345).
    #[tokio::test]
    async fn a_manifest_served_under_the_wrong_digest_is_refused() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let requested = seed_source(&data, &source, &leaf_manifest());

        // A second, entirely valid manifest filed under the digest-addressed key
        // of the first. It names the *same* blobs deliberately: every one is
        // already at the source, so without the identity check this copy runs to
        // completion and reports success — which is the failure being tested. A
        // substitute naming an unseeded blob would trip a different guard
        // downstream and prove nothing about this one.
        let substitute = super::super::Manifest::Image(super::super::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            artifact_type: Some("application/vnd.example.substitute".to_string()),
            config: descriptor("application/vnd.ocx.package.metadata.v1+json", CONFIG_BLOB),
            layers: vec![descriptor(crate::MEDIA_TYPE_TAR_GZ, LAYER_BLOB)],
            ..Default::default()
        });
        let substitute_bytes = serde_json::to_vec(&substitute).expect("serialize");
        let substitute_digest = Algorithm::Sha256.hash(&substitute_bytes);
        assert_ne!(
            substitute_digest, requested,
            "the substitute must be a different manifest"
        );
        let key = canonical(&source.without_tag().clone_with_digest(requested.clone())).to_string();
        data.write()
            .manifests
            .insert(key, (substitute_bytes, substitute_digest.to_string()));

        let error = copy_leaf(&client_for(&data), &source, &target, &requested, false, scratch.path())
            .await
            .expect_err("a substituted manifest must be refused");

        match error {
            ClientError::DigestMismatch { expected, actual } => {
                assert_eq!(expected, requested.to_string(), "the error names what was asked for");
                assert_eq!(actual, substitute_digest.to_string(), "and what arrived instead");
            }
            other => panic!("unexpected error: {other}"),
        }
        let calls = data.read().calls.clone();
        assert!(
            !calls.iter().any(|c| c.starts_with("push_")),
            "nothing may reach the target, calls: {calls:?}"
        );
    }

    /// A blob whose bytes do not hash to the digest its descriptor names must
    /// never reach the target, and the copy must say so *before* the upload.
    ///
    /// The target would reject the mismatch on its own — but only after the whole
    /// blob has crossed the wire, and it would attribute the fault to us rather
    /// than to the source registry that served the wrong bytes (CWE-345). The
    /// content is the same length as the descriptor declares, so the read is
    /// complete: this is a content failure and must not be reported as a
    /// truncated transfer.
    #[tokio::test]
    async fn a_spooled_blob_that_rehashes_wrong_never_reaches_the_target() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let leaf = seed_source(&data, &source, &leaf_manifest());

        let layer_digest = Algorithm::Sha256.hash(LAYER_BLOB).to_string();
        let corrupted: Vec<u8> = LAYER_BLOB.iter().map(|byte| byte ^ 0x20).collect();
        assert_eq!(corrupted.len(), LAYER_BLOB.len(), "same length, different content");
        data.write().blobs.insert(layer_digest.clone(), corrupted);

        let error = copy_leaf(&client_for(&data), &source, &target, &leaf, false, scratch.path())
            .await
            .expect_err("a corrupted blob must be refused");

        match error {
            ClientError::DigestMismatch { expected, .. } => {
                assert_eq!(expected, layer_digest, "the error names the blob that was asked for");
            }
            other => panic!("unexpected error: {other}"),
        }
        let calls = data.read().calls.clone();
        assert!(
            !calls.iter().any(|c| c == &format!("push_blob:{layer_digest}")),
            "the corrupted blob must not be uploaded, calls: {calls:?}"
        );
        assert!(
            pushed_manifest(&data, &target, &leaf).is_none(),
            "and the manifest naming it must not be published"
        );
    }

    /// A descriptor's declared size is a claim by the source, and the spool is
    /// sized from it. An absurd claim is refused before a byte is fetched, so
    /// the ceiling costs one comparison rather than a filled filesystem.
    #[tokio::test]
    async fn a_blob_declaring_an_absurd_size_is_refused_before_it_is_fetched() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let mut oversized = descriptor(crate::MEDIA_TYPE_TAR_GZ, LAYER_BLOB);
        oversized.size = i64::MAX;
        let manifest = super::super::Manifest::Image(super::super::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            config: descriptor("application/vnd.ocx.package.metadata.v1+json", CONFIG_BLOB),
            layers: vec![oversized],
            ..Default::default()
        });
        let leaf = seed_source(&data, &source, &manifest);

        let error = copy_leaf(&client_for(&data), &source, &target, &leaf, false, scratch.path())
            .await
            .expect_err("an over-declared blob must be refused");

        assert!(
            matches!(error, ClientError::LayerSizeExceeded { declared, .. } if declared == i64::MAX),
            "unexpected error: {error}"
        );
        let calls = data.read().calls.clone();
        assert!(
            !calls.iter().any(|c| c.starts_with("pull_blob")),
            "the refusal must precede the fetch, calls: {calls:?}"
        );
    }

    /// A blob set too large to traverse is an error, not a truncated success.
    ///
    /// The distinction is the whole point of the cap being typed: a copy that
    /// stopped at the ceiling and exited zero would leave the target holding a
    /// manifest naming blobs that were never uploaded.
    #[tokio::test]
    async fn a_manifest_naming_more_blobs_than_the_cap_is_refused() {
        let scratch = scratch_dir();
        use crate::cli::ClassifyExitCode as _;

        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        // One past the ceiling once the config is counted.
        let layers = (0..MAX_BLOBS_PER_MANIFEST)
            .map(|n| descriptor(crate::MEDIA_TYPE_TAR_GZ, format!("layer {n}").as_bytes()))
            .collect();
        let manifest = super::super::Manifest::Image(super::super::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            config: descriptor("application/vnd.ocx.package.metadata.v1+json", CONFIG_BLOB),
            layers,
            ..Default::default()
        });
        let leaf = seed_source(&data, &source, &manifest);

        let error = copy_leaf(&client_for(&data), &source, &target, &leaf, false, scratch.path())
            .await
            .expect_err("an over-cap blob set must be refused");

        match &error {
            ClientError::TraversalLimitExceeded {
                limit_kind,
                limit,
                actual,
                ..
            } => {
                assert_eq!(*limit_kind, TraversalLimit::BlobsPerManifest);
                assert_eq!(*limit, MAX_BLOBS_PER_MANIFEST);
                assert_eq!(*actual, MAX_BLOBS_PER_MANIFEST + 1, "config plus every layer");
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(
            error.classify(),
            Some(crate::cli::ExitCode::DataError),
            "a registry-supplied graph this build refuses is a data fault, not a retry"
        );
        let calls = data.read().calls.clone();
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("push_") || c.starts_with("head_blob")),
            "the refusal must precede every transfer, calls: {calls:?}"
        );
    }

    /// A signature is a referrer of the leaf; a signature over an SBOM is a
    /// referrer of a referrer. Both have to travel, or verification at the
    /// target reports an unsigned artifact.
    #[tokio::test]
    async fn referrers_are_copied_recursively_and_only_when_asked() {
        for include in [false, true] {
            let data = StubTransportData::new();
            let scratch = scratch_dir();
            let source = identifier("dev.example.com", "team/demo");
            let target = identifier("prod.example.com", "team/demo");
            let leaf = seed_source(&data, &source, &leaf_manifest());

            // The SBOM refers to the leaf; the signature refers to the SBOM.
            let sbom = referrer_manifest("application/spdx+json", SBOM_PAYLOAD);
            let sbom_digest = seed(&data, &source, &sbom, &[EMPTY_CONFIG, SBOM_PAYLOAD]);
            let signature = referrer_manifest("application/vnd.dev.sigstore.bundle.v0.3+json", SIGNATURE_PAYLOAD);
            let signature_digest = seed(&data, &source, &signature, &[EMPTY_CONFIG, SIGNATURE_PAYLOAD]);
            {
                let repository = source.repository().to_string();
                let mut inner = data.write();
                inner.referrers.insert(
                    format!("{repository}@{leaf}"),
                    vec![descriptor(
                        MEDIA_TYPE_OCI_IMAGE_MANIFEST,
                        &serde_json::to_vec(&sbom).unwrap(),
                    )],
                );
                assert_eq!(
                    inner.referrers.values().flatten().next().map(|d| d.digest.clone()),
                    Some(sbom_digest.to_string()),
                    "the referrer descriptor must name the manifest that was seeded"
                );
                inner.referrers.insert(
                    format!("{repository}@{sbom_digest}"),
                    vec![descriptor(
                        MEDIA_TYPE_OCI_IMAGE_MANIFEST,
                        &serde_json::to_vec(&signature).unwrap(),
                    )],
                );
            }
            let copied = copy_leaf(&client_for(&data), &source, &target, &leaf, include, scratch.path())
                .await
                .expect("copy");

            // Named before counted, and in that order deliberately: a count of
            // two is equally what copying the SBOM twice produces, or the SBOM
            // plus some third manifest, neither of which is the property under
            // test. The signature is the one only a *recursive* walk reaches, so
            // asserting it by digest is what separates depth 2 from depth 1 —
            // and asserting it first is what makes it the assertion that fires.
            for (label, digest) in [("sbom", &sbom_digest), ("signature", &signature_digest)] {
                assert_eq!(
                    pushed_manifest(&data, &target, digest).is_some(),
                    include,
                    "{label} at the target, include_referrers = {include}"
                );
            }
            let expected = if include { 2 } else { 0 };
            assert_eq!(copied.referrers, expected, "include_referrers = {include}");
        }
    }

    /// A registry with no Referrers API cannot be asked to hold a signature, and
    /// the copy has to say so rather than quietly promoting an unsigned artifact.
    #[tokio::test]
    async fn a_registry_without_the_referrers_api_fails_the_copy() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let leaf = seed_source(&data, &source, &leaf_manifest());
        data.write().referrers_unsupported = true;

        let error = copy_leaf(&client_for(&data), &source, &target, &leaf, true, scratch.path())
            .await
            .expect_err("referrers must be required once asked for");
        assert!(
            matches!(error, ClientError::ReferrersUnsupported { .. }),
            "unexpected error: {error}"
        );
    }

    /// A referrer the source lists but cannot serve fails the copy.
    ///
    /// The failure mode this rules out is the quiet one: skip it, exit zero, and
    /// the target holds an artifact whose Sigstore bundle stayed behind, reported
    /// as a complete promotion with the loss visible only in a log line `--quiet`
    /// suppresses (PKG-11).
    ///
    /// The positive control is the same fixture with the referrer actually seeded,
    /// which succeeds and copies it — so the failure below is the absence being
    /// caught, not the fixture being unable to copy a referrer at all.
    #[tokio::test]
    async fn a_referrer_listed_but_not_servable_fails_the_copy() {
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let sbom = referrer_manifest("application/spdx+json", SBOM_PAYLOAD);
        let sbom_bytes = serde_json::to_vec(&sbom).unwrap();

        // Control: the referrer is listed AND seeded, so the copy carries it.
        let control = StubTransportData::new();
        let control_scratch = scratch_dir();
        let control_leaf = seed_source(&control, &source, &leaf_manifest());
        let control_sbom = seed(&control, &source, &sbom, &[EMPTY_CONFIG, SBOM_PAYLOAD]);
        control.write().referrers.insert(
            format!("{}@{control_leaf}", source.repository()),
            vec![descriptor(MEDIA_TYPE_OCI_IMAGE_MANIFEST, &sbom_bytes)],
        );
        let copied = copy_leaf(
            &client_for(&control),
            &source,
            &target,
            &control_leaf,
            true,
            control_scratch.path(),
        )
        .await
        .expect("control: a listed and servable referrer copies");
        assert_eq!(copied.referrers, 1, "control must actually copy the referrer");
        assert!(
            pushed_manifest(&control, &target, &control_sbom).is_some(),
            "control must land the referrer at the target"
        );

        // The real case: listed, never seeded. Same descriptor, same digest.
        let data = StubTransportData::new();
        let scratch = scratch_dir();
        let leaf = seed_source(&data, &source, &leaf_manifest());
        data.write().referrers.insert(
            format!("{}@{leaf}", source.repository()),
            vec![descriptor(MEDIA_TYPE_OCI_IMAGE_MANIFEST, &sbom_bytes)],
        );

        let error = copy_leaf(&client_for(&data), &source, &target, &leaf, true, scratch.path())
            .await
            .expect_err("a referrer the source cannot serve must fail the copy");
        let rendered = error.to_string();
        assert!(
            matches!(error, ClientError::InvalidManifest(_)),
            "unexpected error: {error}"
        );
        assert!(
            rendered.contains("cannot serve it"),
            "the failure must name the disagreement, not just the digest: {rendered}"
        );
    }
}
