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
    /// What became of the leaf's cosign sidecar tags.
    pub sidecars: SidecarCopy,
}

/// What became of the three cosign `<algorithm>-<hex>.{sig,att,sbom}` tags.
///
/// `conflicts` is data rather than an error on purpose (C-098). A destination
/// tag holding a *different* manifest is a conflict local to one sidecar — the
/// source view is perfectly coherent — and failing the whole copy there would
/// block the legitimate case the guard exists to protect: re-promoting onto a
/// destination that merely holds more signatures than the source. So the leaf
/// and every other sidecar still land, and the caller turns a non-empty list
/// into a non-zero exit.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SidecarCopy {
    /// Sidecar manifests written to the target, or already there under the same
    /// digest.
    pub copied: usize,
    /// Tags left exactly as they were, because the target already holds a
    /// different manifest under them. Named so the caller can print them.
    pub conflicts: Vec<String>,
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

    // Before `ensure_target_serves_referrers`, and the position is the contract
    // (C-091). That gate refuses a target without the OCI 1.1 Referrers API,
    // which is backwards for a mechanism that exists *for* registries lacking
    // it: placed after it, this sweep could never run against a `registry:2`
    // destination, and the scenario asserting sidecars still land there would
    // pass only because it never executed. `--no-referrers` skips it (C-096) —
    // one flag governs everything anchored to the leaf.
    let sidecars = if include_referrers {
        transfer.copy_sidecar_tags(&digest).await?
    } else {
        SidecarCopy::default()
    };

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
        sidecars,
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

    /// Copies cosign's `<algorithm>-<hex>.{sig,att,sbom}` sidecar tags, verbatim.
    ///
    /// A cosign sidecar is not a referrer: its manifest declares neither
    /// `artifactType` nor `subject`, so nothing lists it and the tag name is the
    /// only way in. That is also why it is copied under the *same* tag name and
    /// never re-homed as a proper referrer — re-homing means reconstructing the
    /// manifest, and reconstruction is what corrupts signatures
    /// ([cosign#4207](https://github.com/sigstore/cosign/issues/4207)). Same
    /// bytes, same digest, same tag, or nothing.
    ///
    /// All three tags are probed **unconditionally**, with `HEAD`
    /// ([`OciTransport::fetch_manifest_digest`](super::client::transport::OciTransport::fetch_manifest_digest)),
    /// never only when the Referrers API came back empty. A repository
    /// mid-migration carries an OCX referrer *and* a cosign `.sig`; under a
    /// probe-when-empty rule the signature is dropped and the copy exits 0,
    /// which is the silent loss `copy_referrers` refuses by name (PKG-11). Three
    /// round-trips of headers answer the cost question without trading
    /// correctness for it.
    ///
    /// # What fails the whole copy, and what does not
    ///
    /// A tag that HEADs and then cannot be fetched fails the copy, exactly as a
    /// listed-but-unservable referrer does: the *source* view is incoherent, so
    /// nothing this run writes is trustworthy. An index-shaped sidecar fails it
    /// too, before any push — [`blob_set`] takes an `&ImageManifest` and cannot
    /// see an index's children, so pushing one would publish a manifest at the
    /// target naming children that were never transferred.
    ///
    /// A destination tag already holding a *different* manifest is the opposite
    /// case and is returned as data, not an error: see [`SidecarCopy`].
    async fn copy_sidecar_tags(&self, subject: &Digest) -> Result<SidecarCopy, ClientError> {
        let transport = self.client.transport();
        let source_image = self.client.read_reference(self.source, ReadAddressing::Canonical);
        let target_image = self.client.transport_write_reference(self.target);
        let mut outcome = SidecarCopy::default();

        for suffix in crate::package::tag::SIDECAR_SUFFIXES {
            let tag = crate::package::tag::sidecar_tag(subject, suffix);
            let source_tag = super::client::sibling_tag_reference(&source_image, tag.clone());
            let served = match transport.fetch_manifest_digest(&source_tag).await {
                Ok(digest) => digest,
                // No such attachment. The overwhelmingly common answer, and the
                // one thing here that is not a fault.
                Err(ClientError::ManifestNotFound(_)) => continue,
                Err(other) => return Err(other),
            };
            let served = parse_descriptor_digest(&served)?;

            let target_tag = super::client::sibling_tag_reference(&target_image, tag.clone());
            match transport.fetch_manifest_digest(&target_tag).await {
                // Same manifest already there: the copy has nothing to do and
                // counts it, because the target does serve it.
                Ok(existing) if existing == served.to_string() => {
                    outcome.copied = outcome.copied.saturating_add(1);
                    continue;
                }
                // A `.sig`/`.att` manifest accumulates signatures as layers
                // *within itself*, so a verbatim PUT over a different manifest
                // silently destroys every signature the target holds and the
                // source does not. Merging the two layer sets is not the answer
                // either — merging is reconstruction (cosign#4207). Refuse this
                // one tag, name it, carry on.
                Ok(_) => {
                    outcome.conflicts.push(tag);
                    continue;
                }
                Err(ClientError::ManifestNotFound(_)) => {}
                Err(other) => return Err(other),
            }

            // Addressed by the digest the HEAD answered with, never by the tag:
            // the identity check inside the fetch then covers this read too, and
            // a tag that moves between the two calls cannot substitute a
            // manifest for the one whose absence at the target was just checked.
            let sidecar_id = self.source.without_tag().clone_with_digest(served.clone());
            let Some((bytes, digest, manifest)) = self
                .client
                .fetch_manifest_raw_bytes_addressed(&sidecar_id, ReadAddressing::Canonical)
                .await?
            else {
                return Err(ClientError::InvalidManifest(format!(
                    "sidecar tag {tag} of {subject} resolves to {served} but the source cannot serve it; \
                     re-run the copy, and if it persists the source registry is inconsistent"
                )));
            };

            let super::Manifest::Image(image) = &manifest else {
                return Err(ClientError::InvalidManifest(format!(
                    "sidecar {tag} of {subject} is an image index; its children are not copied"
                )));
            };
            // The signed payload is a blob, not an annotation: only the
            // verification material (signature, certificate, chain, Rekor
            // bundle) rides in annotations, and the payload the signature is
            // over is the layer. Pushing the manifest alone would publish a
            // sidecar at the target naming a blob nobody transferred.
            self.copy_blobs(&blob_set(image, &sidecar_id)?).await?;

            transport
                .ensure_auth(&target_tag, super::RegistryOperation::Push)
                .await?;
            transport
                .push_manifest_raw(&target_tag, bytes, manifest.content_type())
                .await?;

            // There is no conditional manifest PUT anywhere in the OCI
            // distribution spec, so the check above is optimistic, not atomic:
            // a second `ocx package copy` can observe the same absent tag and
            // land its own PUT after ours. Reading the tag back and demanding
            // *our* digest is what turns that into a reported conflict instead
            // of the silent accumulation loss this guard exists to prevent —
            // the same read-back `push_referrer_fallback_index` documents, and
            // with the same limit: two writers converge, three need not.
            match transport.fetch_manifest_digest(&target_tag).await {
                Ok(landed) if landed == digest.to_string() => {
                    outcome.copied = outcome.copied.saturating_add(1);
                }
                Ok(_) | Err(ClientError::ManifestNotFound(_)) => outcome.conflicts.push(tag),
                Err(other) => return Err(other),
            }
        }
        Ok(outcome)
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

        // `_with_fallback`, never the verdict-shaped `list_referrers`: this is a
        // READ of the source, and a source that serves no Referrers API is not a
        // failed copy — it is a source whose referrers live on the
        // `<algorithm>-<encoded>` fallback tag, which is exactly what OCX's own
        // sign/attest write there. The verdict belongs to
        // `ensure_target_serves_referrers` above, on the target, which is the
        // side that has to hold what it is handed; raising it here reported 84
        // naming the SOURCE, contradicting the documented split and dropping
        // every fallback-tag signature on the floor.
        let descriptors = transport
            .list_referrers_with_fallback(&source_image, subject, None)
            .await?
            .descriptors;
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
    const SIDECAR_PAYLOAD: &[u8] = b"a simplesigning payload, pretend";
    const OTHER_SIDECAR_PAYLOAD: &[u8] = b"a second simplesigning payload, pretend";

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

    // ── cosign sidecar tags ──────────────────────────────────────────────

    /// A cosign `sha256-<hex>.sig` manifest: empty config, the signed payload as
    /// the single layer, and **no** `subject` or `artifactType`.
    ///
    /// Those two absences are the whole reason this mechanism needs its own
    /// sweep: nothing lists a sidecar, so the tag name is the only way in — and
    /// they are why it cannot be re-homed as a referrer without reconstructing
    /// the manifest, which is what corrupts signatures (cosign#4207).
    fn sidecar_manifest(payload: &'static [u8]) -> super::super::Manifest {
        super::super::Manifest::Image(super::super::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            config: descriptor("application/vnd.oci.empty.v1+json", EMPTY_CONFIG),
            layers: vec![descriptor("application/vnd.dev.cosign.simplesigning.v1+json", payload)],
            ..Default::default()
        })
    }

    fn sidecar_reference(identifier: &Identifier, subject: &Digest, suffix: &str) -> crate::oci::native::Reference {
        crate::oci::client::sibling_tag_reference(
            &canonical(identifier),
            crate::package::tag::sidecar_tag(subject, suffix),
        )
    }

    /// Files `bytes` under BOTH keys a registry answers a tagged manifest on —
    /// the tag and its digest — plus the blobs it names.
    ///
    /// Two keys and not one because the sweep uses both: it HEADs the *tag* and
    /// then fetches by the *digest* that answered. Seeding only the tag is
    /// exactly the listed-but-unservable state, which one test below wants
    /// deliberately.
    fn seed_sidecar_raw(
        data: &StubTransportData,
        repository: &Identifier,
        subject: &Digest,
        suffix: &str,
        bytes: &[u8],
        blobs: &[&'static [u8]],
    ) -> Digest {
        let digest = seed_raw(data, repository, bytes, blobs);
        data.write().manifests.insert(
            sidecar_reference(repository, subject, suffix).to_string(),
            (bytes.to_vec(), digest.to_string()),
        );
        digest
    }

    /// [`seed_sidecar_raw`] with the manifest serialised **pretty-printed**.
    ///
    /// Indented bytes are a shape serde's writer never emits, so a copy that
    /// parsed and re-encoded the sidecar fails the byte comparison on the first
    /// byte of whitespace. Seeded compactly the assertion could not fail — the
    /// same reason `leaf_manifest_bytes_survive_the_copy_verbatim` seeds pretty.
    fn seed_sidecar(
        data: &StubTransportData,
        repository: &Identifier,
        subject: &Digest,
        suffix: &str,
        payload: &'static [u8],
    ) -> (Digest, Vec<u8>) {
        let bytes = serde_json::to_vec_pretty(&sidecar_manifest(payload)).expect("serialize");
        let digest = seed_sidecar_raw(data, repository, subject, suffix, &bytes, &[EMPTY_CONFIG, payload]);
        (digest, bytes)
    }

    fn sidecar_at(
        data: &StubTransportData,
        identifier: &Identifier,
        subject: &Digest,
        suffix: &str,
    ) -> Option<Vec<u8>> {
        data.read()
            .manifests
            .get(&sidecar_reference(identifier, subject, suffix).to_string())
            .map(|(bytes, _)| bytes.clone())
    }

    fn probe_count(data: &StubTransportData) -> usize {
        data.read()
            .calls
            .iter()
            .filter(|c| *c == "fetch_manifest_digest")
            .count()
    }

    /// The sidecar's bytes, its tag name and its payload blob all have to reach
    /// the target — and the bytes have to arrive unchanged, because a
    /// simplesigning signature is over the payload the manifest names and the
    /// verification material rides in the layer's annotations.
    ///
    /// The payload blob is the half an earlier reading of this feature missed:
    /// "sidecar payloads are annotation-embedded" is half true, and pushing the
    /// manifest alone publishes a sidecar at the target naming a blob nobody
    /// transferred.
    #[tokio::test]
    async fn a_cosign_sidecar_tag_and_its_payload_blob_are_carried_verbatim() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let leaf = seed_source(&data, &source, &leaf_manifest());
        let (_, sidecar_bytes) = seed_sidecar(&data, &source, &leaf, ".sig", SIDECAR_PAYLOAD);

        let copied = copy_leaf(&client_for(&data), &source, &target, &leaf, true, scratch.path())
            .await
            .expect("copy");

        assert_eq!(copied.sidecars.copied, 1, "one sidecar tag carried");
        assert!(copied.sidecars.conflicts.is_empty());
        assert_eq!(
            sidecar_at(&data, &target, &leaf, ".sig").as_deref(),
            Some(sidecar_bytes.as_slice()),
            "the sidecar must land under the same tag, byte for byte"
        );
        assert_ne!(
            sidecar_bytes,
            serde_json::to_vec(&sidecar_manifest(SIDECAR_PAYLOAD)).expect("serialize"),
            "the fixture must differ from what a re-serialising copy would emit, \
             or the assertion above cannot fail"
        );
        let payload = Algorithm::Sha256.hash(SIDECAR_PAYLOAD).to_string();
        let target_key = crate::oci::client::test_transport::blob_location_key(&canonical(&target));
        assert!(
            data.read()
                .blob_locations
                .as_ref()
                .and_then(|locations| locations.get(&target_key))
                .is_some_and(|digests| digests.contains(&payload)),
            "the signed payload blob must be at the target, or the sidecar names bytes nobody transferred"
        );
    }

    /// The ordering is the contract (C-091). `ensure_target_serves_referrers`
    /// refuses a target without the OCI 1.1 Referrers API — which is backwards
    /// for a mechanism that exists *for* registries lacking it — and it returns
    /// before `copy_referrers`. A sidecar sweep placed after it could never run
    /// against such a target, and the scenario asserting sidecars still land
    /// there would pass only because it never executed.
    ///
    /// So the assertion is deliberately "the copy fails AND the sidecar is at
    /// the target": the error proves the gate still fires, the tag proves the
    /// sweep ran first.
    #[tokio::test]
    async fn the_sidecar_sweep_runs_before_the_referrers_gate() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let leaf = seed_source(&data, &source, &leaf_manifest());
        let (_, sidecar_bytes) = seed_sidecar(&data, &source, &leaf, ".sig", SIDECAR_PAYLOAD);
        data.write().referrers_unsupported = true;

        let error = copy_leaf(&client_for(&data), &source, &target, &leaf, true, scratch.path())
            .await
            .expect_err("the referrers gate must still refuse the target");

        assert!(
            matches!(error, ClientError::ReferrersUnsupported { .. }),
            "unexpected error: {error}"
        );
        assert_eq!(
            sidecar_at(&data, &target, &leaf, ".sig").as_deref(),
            Some(sidecar_bytes.as_slice()),
            "the sidecar must already have landed when the gate refused, or it runs after it"
        );
    }

    /// All three tags are probed whether or not primary discovery found
    /// anything (C-094).
    ///
    /// The fixture is the mid-migration state that makes this matter: an OCX
    /// referrer the Referrers API *does* list, alongside a cosign sidecar. Under
    /// a probe-only-when-empty rule the referrer satisfies discovery, the probe
    /// never runs, and the signature is dropped at exit 0.
    ///
    /// Three probes, not four and not one: the count is what separates
    /// "unconditional" from "incidental", and `fetch_manifest_digest` is the
    /// HEAD-shaped call — the whole point of using it is that three absent tags
    /// cost three sets of headers rather than three manifest bodies.
    #[tokio::test]
    async fn all_three_sidecar_tags_are_probed_even_when_the_referrers_api_answers() {
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let sbom = referrer_manifest("application/spdx+json", SBOM_PAYLOAD);
        let sbom_bytes = serde_json::to_vec(&sbom).expect("serialize");

        // Run 1 — nothing under any sidecar tag, so every `fetch_manifest_digest`
        // in the log is a source probe and the count is exact.
        let data = StubTransportData::new();
        let scratch = scratch_dir();
        let leaf = seed_source(&data, &source, &leaf_manifest());
        seed(&data, &source, &sbom, &[EMPTY_CONFIG, SBOM_PAYLOAD]);
        data.write().referrers.insert(
            format!("{}@{leaf}", source.repository()),
            vec![descriptor(MEDIA_TYPE_OCI_IMAGE_MANIFEST, &sbom_bytes)],
        );

        let copied = copy_leaf(&client_for(&data), &source, &target, &leaf, true, scratch.path())
            .await
            .expect("copy");

        assert_eq!(
            copied.referrers, 1,
            "primary discovery must be non-empty for this fixture to bite"
        );
        assert_eq!(probe_count(&data), 3, "one HEAD per suffix, unconditionally");
        assert_eq!(copied.sidecars, SidecarCopy::default(), "and nothing was there to copy");

        // Run 2 — the same non-empty discovery, plus a sidecar under `.att`,
        // deliberately not the first suffix: a sweep that stopped at the first
        // 404 would still be green on `.sig`.
        let data = StubTransportData::new();
        let scratch = scratch_dir();
        let leaf = seed_source(&data, &source, &leaf_manifest());
        seed(&data, &source, &sbom, &[EMPTY_CONFIG, SBOM_PAYLOAD]);
        data.write().referrers.insert(
            format!("{}@{leaf}", source.repository()),
            vec![descriptor(MEDIA_TYPE_OCI_IMAGE_MANIFEST, &sbom_bytes)],
        );
        let (_, att_bytes) = seed_sidecar(&data, &source, &leaf, ".att", SIDECAR_PAYLOAD);

        let copied = copy_leaf(&client_for(&data), &source, &target, &leaf, true, scratch.path())
            .await
            .expect("copy");

        assert_eq!(copied.sidecars.copied, 1);
        assert_eq!(
            sidecar_at(&data, &target, &leaf, ".att").as_deref(),
            Some(att_bytes.as_slice()),
            "the sidecar must travel even though the Referrers API answered"
        );
    }

    /// `--no-referrers` governs everything anchored to the leaf, sidecar tags
    /// included (C-096) — and it must cost no probe at all, not merely no write.
    #[tokio::test]
    async fn no_referrers_skips_the_sidecar_tags_entirely() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let leaf = seed_source(&data, &source, &leaf_manifest());
        seed_sidecar(&data, &source, &leaf, ".sig", SIDECAR_PAYLOAD);

        let copied = copy_leaf(&client_for(&data), &source, &target, &leaf, false, scratch.path())
            .await
            .expect("copy");

        assert_eq!(copied.sidecars, SidecarCopy::default(), "nothing swept");
        assert_eq!(probe_count(&data), 0, "not even probed");
        assert!(sidecar_at(&data, &target, &leaf, ".sig").is_none());
    }

    /// A tag that HEADs and then cannot be fetched fails the whole copy (C-095).
    ///
    /// Same rule as a listed-but-unservable referrer, and for the same reason:
    /// the *source* view is incoherent, so nothing this run writes is
    /// trustworthy. Skipping it would leave the target holding an artifact whose
    /// signature stayed behind, reported as a complete promotion.
    ///
    /// The control is the identical fixture with the digest key also seeded,
    /// which copies — so the failure below is the absence being caught, not the
    /// fixture being unable to carry a sidecar at all.
    #[tokio::test]
    async fn a_sidecar_tag_that_heads_but_cannot_be_fetched_fails_the_copy() {
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let bytes = serde_json::to_vec_pretty(&sidecar_manifest(SIDECAR_PAYLOAD)).expect("serialize");

        let control = StubTransportData::new();
        let control_scratch = scratch_dir();
        let control_leaf = seed_source(&control, &source, &leaf_manifest());
        seed_sidecar_raw(
            &control,
            &source,
            &control_leaf,
            ".sig",
            &bytes,
            &[EMPTY_CONFIG, SIDECAR_PAYLOAD],
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
        .expect("control: a servable sidecar copies");
        assert_eq!(copied.sidecars.copied, 1, "control must actually carry the sidecar");

        // The real case: the TAG answers, the digest behind it does not.
        let data = StubTransportData::new();
        let scratch = scratch_dir();
        let leaf = seed_source(&data, &source, &leaf_manifest());
        let orphan = Algorithm::Sha256.hash(&bytes);
        data.write().manifests.insert(
            sidecar_reference(&source, &leaf, ".sig").to_string(),
            (bytes.clone(), orphan.to_string()),
        );

        let error = copy_leaf(&client_for(&data), &source, &target, &leaf, true, scratch.path())
            .await
            .expect_err("a sidecar the source cannot serve must fail the copy");

        let rendered = error.to_string();
        assert!(
            matches!(error, ClientError::InvalidManifest(_)),
            "unexpected error: {error}"
        );
        assert!(
            rendered.contains("cannot serve it") && rendered.contains(&orphan.to_string()),
            "the failure must name the disagreement and the manifest: {rendered}"
        );
        assert!(sidecar_at(&data, &target, &leaf, ".sig").is_none());
    }

    /// An image-index-shaped sidecar is refused before any push (C-097a).
    ///
    /// `blob_set` takes an `&ImageManifest` and cannot see an index's children,
    /// so a hostile source serving an index under `sha256-<hex>.sig` would
    /// otherwise land at the target naming children that were never
    /// transferred — the same defect the referrer path already refuses.
    #[tokio::test]
    async fn an_index_shaped_sidecar_is_refused_before_any_push() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let leaf = seed_source(&data, &source, &leaf_manifest());
        let index = super::super::Manifest::ImageIndex(super::super::ImageIndex {
            schema_version: super::super::INDEX_SCHEMA_VERSION,
            media_type: Some(crate::MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
            manifests: Vec::new(),
            artifact_type: None,
            annotations: None,
        });
        let bytes = serde_json::to_vec(&index).expect("serialize");
        seed_sidecar_raw(&data, &source, &leaf, ".sig", &bytes, &[]);

        let error = copy_leaf(&client_for(&data), &source, &target, &leaf, true, scratch.path())
            .await
            .expect_err("an index-shaped sidecar must be refused");

        assert!(
            matches!(error, ClientError::InvalidManifest(ref message) if message.contains("image index")),
            "unexpected error: {error}"
        );
        assert!(
            sidecar_at(&data, &target, &leaf, ".sig").is_none(),
            "nothing may be pushed"
        );
    }

    /// The three destination states of one sidecar tag (C-098): absent → write,
    /// identical → no-op, different → refuse *that tag* and carry on.
    ///
    /// The refusal is per-sidecar rather than whole-copy on purpose. A
    /// `.sig`/`.att` manifest accumulates signatures as layers within itself, so
    /// a verbatim PUT over a different manifest silently destroys every
    /// signature the target holds and the source does not — but the source view
    /// is perfectly coherent, and failing the whole copy would block the
    /// legitimate case: re-promoting onto a destination that merely holds
    /// *more* signatures than the source. So the leaf lands, `.att` lands, and
    /// `.sig` is named.
    #[tokio::test]
    async fn a_destination_sidecar_tag_holding_a_different_manifest_is_refused_and_named() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let leaf = seed_source(&data, &source, &leaf_manifest());
        let (_, sig_bytes) = seed_sidecar(&data, &source, &leaf, ".sig", SIDECAR_PAYLOAD);
        let (_, att_bytes) = seed_sidecar(&data, &source, &leaf, ".att", SIDECAR_PAYLOAD);

        // The target already holds a DIFFERENT `.sig` — the shape of a
        // destination carrying a signature the source never had.
        let occupier = serde_json::to_vec(&sidecar_manifest(OTHER_SIDECAR_PAYLOAD)).expect("serialize");
        let occupier_digest = Algorithm::Sha256.hash(&occupier);
        data.write().manifests.insert(
            sidecar_reference(&target, &leaf, ".sig").to_string(),
            (occupier.clone(), occupier_digest.to_string()),
        );

        let copied = copy_leaf(&client_for(&data), &source, &target, &leaf, true, scratch.path())
            .await
            .expect("a sidecar conflict must not fail the copy");

        assert_eq!(
            copied.sidecars.conflicts,
            vec![crate::package::tag::sidecar_tag(&leaf, ".sig")],
            "the refusal must name the tag it refused"
        );
        assert_eq!(
            sidecar_at(&data, &target, &leaf, ".sig").as_deref(),
            Some(occupier.as_slice()),
            "the target's own signature must survive untouched"
        );
        assert_ne!(
            occupier, sig_bytes,
            "the fixture must be a genuinely different manifest"
        );
        assert_eq!(copied.sidecars.copied, 1, "the other sidecar still lands");
        assert_eq!(
            sidecar_at(&data, &target, &leaf, ".att").as_deref(),
            Some(att_bytes.as_slice())
        );
        assert!(
            pushed_manifest(&data, &target, &leaf).is_some(),
            "and so does the leaf: the conflict is local to one tag"
        );
    }

    /// A destination already serving the identical sidecar is a no-op, not a
    /// conflict and not a re-push (C-098, "same digest → the copy proceeds").
    #[tokio::test]
    async fn a_destination_sidecar_tag_holding_the_same_manifest_is_a_no_op() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let leaf = seed_source(&data, &source, &leaf_manifest());
        let (sidecar_digest, sidecar_bytes) = seed_sidecar(&data, &source, &leaf, ".sig", SIDECAR_PAYLOAD);
        data.write().manifests.insert(
            sidecar_reference(&target, &leaf, ".sig").to_string(),
            (sidecar_bytes.clone(), sidecar_digest.to_string()),
        );
        data.write().calls.clear();

        let copied = copy_leaf(&client_for(&data), &source, &target, &leaf, true, scratch.path())
            .await
            .expect("copy");

        assert_eq!(copied.sidecars.copied, 1, "counted: the target does serve it");
        assert!(copied.sidecars.conflicts.is_empty());
        assert!(
            !data.read().calls.iter().any(
                |c| c.starts_with("push_blob:") && c.contains(&Algorithm::Sha256.hash(SIDECAR_PAYLOAD).to_string())
            ),
            "an identical sidecar must cost no transfer, calls: {:?}",
            data.read().calls
        );
    }

    /// The read-back after the PUT (`transport.rs`'s
    /// `push_referrer_fallback_index` documents the same pattern and the same
    /// limit): there is **no conditional manifest PUT anywhere in the OCI
    /// distribution spec**, so the pre-push absence check is optimistic, not
    /// atomic. Two concurrent copies can both see the tag absent, and the later
    /// PUT clobbers the earlier one — recreating the accumulation loss C-098
    /// exists to prevent. Reading the tag back and demanding *our own* digest is
    /// what turns that into a reported conflict.
    ///
    /// The fixture models the clobber as "the target does not serve back what it
    /// accepted", which is the other half of the same match arm — the stub
    /// cannot express a third party writing between our PUT and our read. What
    /// is proven here is that the read-back exists and that its failure becomes
    /// a conflict rather than a counted success; what is *not* proven is the
    /// interleaving itself. Two writers converge, three need not: that limit is
    /// inherited from the pattern, not fixed here.
    #[tokio::test]
    async fn a_sidecar_the_target_does_not_serve_back_is_reported_as_a_conflict() {
        let scratch = scratch_dir();
        let data = StubTransportData::new();
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let leaf = seed_source(&data, &source, &leaf_manifest());
        seed_sidecar(&data, &source, &leaf, ".sig", SIDECAR_PAYLOAD);
        // Accepts every PUT and stores none of it.
        data.write().capture_pushes = false;

        let copied = copy_leaf(&client_for(&data), &source, &target, &leaf, true, scratch.path())
            .await
            .expect("the copy continues; the sidecar is reported");

        assert_eq!(
            copied.sidecars.conflicts,
            vec![crate::package::tag::sidecar_tag(&leaf, ".sig")],
            "a PUT whose read-back does not answer our digest is a conflict, not a success"
        );
        assert_eq!(copied.sidecars.copied, 0);
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

    /// A source registry with no OCI 1.1 Referrers API keeps its referrers on
    /// the `<algorithm>-<encoded>` fallback tag — which is exactly where OCX's
    /// own `sign` and `attest` write them. Reading the source through the
    /// verdict-shaped `list_referrers` turned that into exit 84 naming the
    /// SOURCE, dropping every fallback-tag signature on the floor. The verdict
    /// belongs to `ensure_target_serves_referrers`, on the target.
    ///
    /// `Transfer` is built directly rather than driving this through
    /// `copy_leaf`: `StubTransportData::referrers_unsupported` is global, so
    /// `copy_leaf` fails first — correctly — on the TARGET probe, a behaviour
    /// `a_registry_without_the_referrers_api_fails_the_copy` already pins.
    ///
    /// The same global flag also refuses the stub's `push_referrer_manifest`,
    /// so a fallback-listed referrer cannot complete a copy through this double
    /// without editing it. The strongest observable that IS reachable is the
    /// listed-but-unservable refusal: it names the descriptor, and the only
    /// place that descriptor exists is the fallback tag. The control below —
    /// the same fixture served through a working Referrers API — is what stops
    /// that reading from being vacuous, by copying a referrer end to end.
    #[tokio::test]
    async fn a_source_without_the_referrers_api_is_read_through_the_fallback_tag() {
        let source = identifier("dev.example.com", "team/demo");
        let target = identifier("prod.example.com", "team/demo");
        let sbom = referrer_manifest("application/spdx+json", SBOM_PAYLOAD);
        let sbom_bytes = serde_json::to_vec(&sbom).expect("serialize");
        let sbom_descriptor = descriptor(MEDIA_TYPE_OCI_IMAGE_MANIFEST, &sbom_bytes);

        // Control: the Referrers API answers and the referrer is servable, so
        // the copy carries it all the way to the target.
        let control = StubTransportData::new();
        let control_scratch = scratch_dir();
        let control_leaf = seed_source(&control, &source, &leaf_manifest());
        let control_sbom = seed(&control, &source, &sbom, &[EMPTY_CONFIG, SBOM_PAYLOAD]);
        control.write().referrers.insert(
            format!("{}@{control_leaf}", source.repository()),
            vec![sbom_descriptor.clone()],
        );
        let control_client = client_for(&control);
        let control_transfer = Transfer {
            client: &control_client,
            source: &source,
            target: &target,
            scratch: control_scratch.path(),
        };
        assert_eq!(
            control_transfer
                .copy_referrers(&control_leaf, 0, &mut BTreeSet::new())
                .await
                .expect("control: a listed, servable referrer copies"),
            1,
            "control must actually copy the referrer"
        );
        assert!(
            pushed_manifest(&control, &target, &control_sbom).is_some(),
            "control must land the referrer at the target"
        );

        // The real case: no Referrers API, and the referrer listed ONLY on the
        // fallback tag — never in the stub's referrers map.
        let data = StubTransportData::new();
        let scratch = scratch_dir();
        let leaf = seed_source(&data, &source, &leaf_manifest());
        {
            let index = crate::oci::ImageIndex {
                schema_version: crate::oci::INDEX_SCHEMA_VERSION,
                media_type: Some(crate::media_type::MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                manifests: vec![crate::oci::ImageIndexEntry {
                    media_type: sbom_descriptor.media_type.clone(),
                    digest: sbom_descriptor.digest.clone(),
                    size: sbom_descriptor.size,
                    platform: None,
                    annotations: None,
                    artifact_type: sbom_descriptor.artifact_type.clone(),
                }],
                artifact_type: None,
                annotations: None,
            };
            let bytes = serde_json::to_vec(&index).expect("serialize the fallback index");
            let key = crate::oci::client::sibling_tag_reference(
                &canonical(&source),
                crate::package::tag::referrer_fallback_tag(&leaf),
            )
            .to_string();
            let digest = Algorithm::Sha256.hash(&bytes).to_string();
            data.write().manifests.insert(key, (bytes, digest));
        }
        data.write().referrers_unsupported = true;

        let client = client_for(&data);
        let transfer = Transfer {
            client: &client,
            source: &source,
            target: &target,
            scratch: scratch.path(),
        };
        let error = transfer
            .copy_referrers(&leaf, 0, &mut BTreeSet::new())
            .await
            .expect_err("the fallback-listed referrer is deliberately left unseeded");

        assert!(
            !matches!(error, ClientError::ReferrersUnsupported { .. }),
            "reading the SOURCE must never raise the referrers verdict: {error}"
        );
        let rendered = error.to_string();
        assert!(
            rendered.contains(&sbom_descriptor.digest),
            "the refusal must name the descriptor the fallback tag listed, or the tag was never read: {rendered}"
        );
        assert!(
            rendered.contains("cannot serve it"),
            "the walk must have reached the referrer fetch: {rendered}"
        );
    }
}
