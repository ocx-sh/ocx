// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The write half of `ocx package cascade repair`.
//!
//! Takes a plan the pure core computed whole and applies it: authenticate for
//! push once, preflight every referenced child manifest, then PUT each alias
//! index concurrently. Batched on purpose — the entire plan exists before the
//! first write, so an auth failure or a missing child costs zero writes rather
//! than leaving the graph half-rewritten.
//!
//! Partial failure is reported, not propagated: one alias failing must not
//! discard the outcome of the ones that succeeded, so writes collect per tag
//! and the run reports every result in tag order rather than completion order.
//!
//! Known limitation: the transport has no conditional-write primitive, so a
//! concurrent publish to the same alias is last-writer-wins. The post-write
//! read-back detects it and warns; do not run a repair against a repository
//! with a publish in flight.
//!
//! Every read here — the preflight probes and the post-write read-back alike —
//! addresses the **canonical** registry ([`ReadAddressing::Canonical`]), the
//! host the writes go to. A preflight answered by a mirror would gate a write
//! on a repository the write never touches.

use std::collections::BTreeSet;

use futures::stream::{self, StreamExt, TryStreamExt};
use serde::Serialize;

use super::graph::{AliasTag, PlannedWrite, SlotStatus, Unrepairable};
use crate::Result;
use crate::oci::client::ReadAddressing;
use crate::{log, oci};

/// How many alias indexes to work on at once.
///
/// Far below the gather side's read fan-out: these are writes against a single
/// repository, and a registry that throttles will throttle them first. The
/// preflight reads share the bound instead of taking a second knob — they are
/// derived from the same plan and nobody would tune the two apart.
const CASCADE_APPLY_CONCURRENCY: usize = 8;

/// What happened to one alias tag in a repair run.
#[derive(Clone, Debug, Serialize)]
pub struct RepairOutcome {
    pub tag: AliasTag,
    pub outcome: WriteOutcome,
}

/// The result of attempting one alias index write.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "kebab-case", tag = "outcome")]
pub enum WriteOutcome {
    /// The index was written. `verified` is false when the post-write read-back
    /// returned a different digest — a concurrent writer, which is a warning
    /// rather than a failure: this run's write did land.
    ///
    /// `dropped` names the child digests that were removed from the planned
    /// index before it went on the wire: dead pointers backing nothing but
    /// orphan slots. Empty for every ordinary write, which is why it is absent
    /// from the JSON rather than an empty array on every row.
    Written {
        digest: oci::Digest,
        verified: bool,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        dropped: Vec<String>,
    },
    /// Refused before any write, because applying it would publish something
    /// broken.
    Refused(Unrepairable),
    /// The alias moved between the gather this plan was computed from and the
    /// write, so nothing was written. Not a fault and not unrepairable: a
    /// publish landed in the middle of the run, and re-running the repair
    /// against the new state is the whole fix.
    Raced {
        expected: Option<oci::Digest>,
        live: Option<oci::Digest>,
    },
    /// The registry rejected the write. Carries the rendered cause: the
    /// structured error is logged once at the boundary, and what a per-tag
    /// report row needs is the sentence, not a match target.
    Failed { message: String },
}

/// Applies a repair plan to `identifier`, one whole index PUT per alias.
///
/// Returns one [`RepairOutcome`] per planned write, in tag order, so a partial
/// failure names exactly which aliases landed and which did not. An empty plan
/// performs no registry call at all.
///
/// # Errors
///
/// Push authentication failure, which fails the whole run before any write.
/// Per-alias failures are reported in the returned outcomes instead.
pub async fn apply(
    client: &oci::Client,
    identifier: &oci::Identifier,
    writes: &[PlannedWrite],
) -> Result<Vec<RepairOutcome>> {
    if writes.is_empty() {
        // A clean graph costs zero registry calls — not even a credential
        // probe, which would make `repair` on a healthy package indistinguishable
        // from one that had work to do.
        return Ok(Vec::new());
    }

    // Once, up front. A credential problem is one broken run, not N broken
    // aliases, and the per-tag failure channel below would report it as the
    // latter — after having already written whichever aliases a partially
    // scoped token did happen to cover.
    client.ensure_auth(identifier, oci::RegistryOperation::Push).await?;

    let verdicts = preflight(client, identifier, writes).await?;
    let planned: Vec<(&PlannedWrite, Preflight)> = writes.iter().zip(verdicts).collect();

    let mut outcomes: Vec<RepairOutcome> = stream::iter(planned)
        .map(|(write, verdict)| async move {
            RepairOutcome {
                tag: write.tag.clone(),
                outcome: write_alias(client, identifier, write, verdict).await,
            }
        })
        .buffer_unordered(CASCADE_APPLY_CONCURRENCY)
        // Not `try_collect`: a single alias the registry rejected must not
        // discard the outcome of the ones that landed. The failure travels in
        // the row, and the run still reports the whole plan.
        .collect()
        .await;

    // `buffer_unordered` yields in completion order, which is a property of the
    // registry's latency rather than of the repair. Two runs over the same
    // damage must produce the same report, so it sorts by the one stable key
    // an outcome has.
    outcomes.sort_by(|left, right| left.tag.cmp(&right.tag));

    Ok(outcomes)
}

/// What preflight decided about one planned alias write.
enum Preflight {
    /// Write it, minus the entries naming `dropped` — dead pointers that back
    /// orphan slots and nothing else. Empty for an untouched write.
    Write { dropped: BTreeSet<String> },
    /// Do not write it: applying it would publish something broken.
    Refuse(Unrepairable),
}

/// What one probe learned about one referenced digest.
enum ChildState {
    Present,
    /// The registry no longer serves it.
    Missing,
    /// It names a digest algorithm this build cannot address, so nothing about
    /// it can be checked.
    Unaddressable,
}

/// HEADs every child manifest the plan references, before any write happens.
///
/// Returns one verdict per planned write, in `writes` order. A child the
/// registry no longer serves would make the planned index a dangling pointer;
/// what that costs depends on what the child backs:
///
/// - Backing **only** [`SlotStatus::Orphan`] slots — a leftover from an earlier
///   cascade whose content is now gone — the entries naming it are dropped and
///   the alias is written. This is the orphan policy's removal half: an orphan
///   is preserved while its child exists and removed once it provably does not.
/// - Backing anything the fold expects, or an entry with no platform slot at
///   all, the whole alias is refused. The repair would otherwise publish a
///   pointer to content nobody can fetch.
///
/// The analysis is per **digest**, not per entry: one manifest can legitimately
/// serve two platform keys (a Rosetta-style alias), and dropping it for the
/// orphan half would silently delete the folded half.
async fn preflight(
    client: &oci::Client,
    identifier: &oci::Identifier,
    writes: &[PlannedWrite],
) -> Result<Vec<Preflight>> {
    let probes = writes.iter().enumerate().flat_map(|(position, write)| {
        write
            .referenced_digests
            .iter()
            .map(move |digest| (position, write, digest))
    });

    let probed: Vec<(usize, &String, ChildState)> = stream::iter(probes)
        .map(|(position, write, digest)| async move {
            // An index may legitimately name a digest algorithm this build does
            // not implement, and one that cannot be addressed cannot be
            // checked. Refused rather than waved through — waving it through
            // publishes a pointer nothing verified.
            let Ok(parsed) = oci::Digest::try_from(digest.as_str()) else {
                log::warn!(
                    "Alias '{}' of {identifier} references digest '{digest}', which cannot be addressed",
                    write.tag
                );
                return Ok((position, digest, ChildState::Unaddressable));
            };
            // `clone_with_digest` keeps the tag, and the child has to be
            // addressed by digest alone — hence stripping the tag first.
            let child = identifier.without_tag().clone_with_digest(parsed);
            match client
                .probe_manifest_digest_addressed(&child, ReadAddressing::Canonical)
                .await
            {
                Ok(Some(_)) => Ok((position, digest, ChildState::Present)),
                Ok(None) => Ok((position, digest, ChildState::Missing)),
                // A read that failed says nothing about whether the child is
                // there, and both guesses are destructive: assume present and
                // the repair publishes a dangling pointer, assume gone and it
                // drops a live platform off a rolling tag.
                Err(source) => Err(crate::Error::from(source)),
            }
        })
        .buffer_unordered(CASCADE_APPLY_CONCURRENCY)
        .try_collect()
        .await?;

    // Probes finish in whatever order the registry answers them; the verdicts
    // must not depend on that. `BTreeSet` restores digest order, which is the
    // order `referenced_digests` already carries, so an alias with two dead
    // children is always reported against the same one.
    let mut missing: Vec<BTreeSet<&String>> = vec![BTreeSet::new(); writes.len()];
    let mut unaddressable: Vec<BTreeSet<&String>> = vec![BTreeSet::new(); writes.len()];
    for (position, digest, state) in probed {
        match state {
            ChildState::Present => {}
            ChildState::Missing => {
                missing[position].insert(digest);
            }
            ChildState::Unaddressable => {
                unaddressable[position].insert(digest);
            }
        }
    }

    Ok(writes
        .iter()
        .enumerate()
        .map(|(position, write)| verdict(write, &missing[position], &unaddressable[position]))
        .collect())
}

/// Turns one alias's dead children into a write-or-refuse decision.
fn verdict(write: &PlannedWrite, missing: &BTreeSet<&String>, unaddressable: &BTreeSet<&String>) -> Preflight {
    if let Some(digest) = unaddressable.first() {
        return Preflight::Refuse(Unrepairable::ChildDigestUnaddressable {
            tag: write.tag.clone(),
            digest: (*digest).clone(),
        });
    }

    let mut dropped = BTreeSet::new();
    for digest in missing {
        if !backs_orphans_only(write, digest) {
            return Preflight::Refuse(Unrepairable::ChildManifestMissing {
                tag: write.tag.clone(),
                digest: (*digest).clone(),
            });
        }
        dropped.insert((*digest).clone());
    }

    // Every entry gone is worse than a stale alias, and the same refusal the
    // pure core raises when a fold has nothing to write. Unreachable for a
    // plan the core produced — an alias only earns a write from a missing or
    // stale slot, and neither is an orphan — but the guard is what makes that
    // an invariant rather than an assumption about the caller.
    if !dropped.is_empty()
        && write
            .index
            .manifests
            .iter()
            .all(|entry| dropped.contains(&entry.digest))
    {
        return Preflight::Refuse(Unrepairable::WouldEmptyIndex { tag: write.tag.clone() });
    }

    Preflight::Write { dropped }
}

/// Whether every entry naming `digest` is a preserved orphan.
///
/// The plan carries its own justification in [`PlannedWrite::reasons`], which
/// is where an orphan slot is named — an entry the fold does not expect, kept
/// only because its child was still there. An entry with no platform slot (an
/// attestation, or the wrap of a bare manifest found at the tag) has no row and
/// is therefore never droppable.
fn backs_orphans_only(write: &PlannedWrite, digest: &str) -> bool {
    write
        .index
        .manifests
        .iter()
        .filter(|entry| entry.digest == digest)
        .all(|entry| {
            entry.platform.as_ref().is_some_and(|platform| {
                write.reasons.iter().any(|row| {
                    row.status == SlotStatus::Orphan
                        && row.platform == *platform
                        && row.observed.as_deref() == Some(digest)
                })
            })
        })
}

/// Writes one alias index, or reports why it was not written.
///
/// Never returns an error: a rejected write is this alias's outcome, and the
/// rest of the plan is unaffected by it.
async fn write_alias(
    client: &oci::Client,
    identifier: &oci::Identifier,
    write: &PlannedWrite,
    verdict: Preflight,
) -> WriteOutcome {
    let dropped = match verdict {
        Preflight::Refuse(refusal) => {
            log::warn!("Refusing to write alias '{}' of {identifier}: {refusal:?}", write.tag);
            return WriteOutcome::Refused(refusal);
        }
        Preflight::Write { dropped } => dropped,
    };

    // Borrowed when nothing is dropped, so the ordinary write puts the planned
    // bytes on the wire verbatim rather than a re-serialized copy of them.
    let index = if dropped.is_empty() {
        std::borrow::Cow::Borrowed(&write.index)
    } else {
        log::info!(
            "Dropping {} dead orphan entr{} from alias '{}' of {identifier}: {}",
            dropped.len(),
            if dropped.len() == 1 { "y" } else { "ies" },
            write.tag,
            dropped.iter().cloned().collect::<Vec<_>>().join(", ")
        );
        let mut index = write.index.clone();
        index.manifests.retain(|entry| !dropped.contains(&entry.digest));
        std::borrow::Cow::Owned(index)
    };

    let target = identifier.clone_with_tag(write.tag.to_string());
    // Read-modify-write over a whole registry pass: the plan was computed from
    // a gather that finished some time ago, and nothing has held the tag since.
    // Without this the repair silently overwrites a publish that landed in
    // between — and the read-back below then confirms the repair's own
    // clobber as verified.
    match client
        .probe_manifest_digest_addressed(&target, ReadAddressing::Canonical)
        .await
    {
        Ok(live) if live != write.observed_digest => {
            log::warn!(
                "Not writing alias '{}' of {identifier}: it moved since it was read",
                write.tag
            );
            return WriteOutcome::Raced {
                expected: write.observed_digest.clone(),
                live,
            };
        }
        Ok(_) => {}
        // Fail closed. The alternative is to write over an alias whose current
        // content nobody established, which is the exact damage the check
        // exists to prevent; a re-run costs one repair.
        Err(source) => {
            log::error!(
                "Not writing alias '{}' of {identifier}: could not read it back first: {source}",
                write.tag
            );
            return WriteOutcome::Failed {
                message: format!("could not confirm the alias before writing: {source}"),
            };
        }
    }

    match client.push_index(&target, &index).await {
        Ok(digest) => WriteOutcome::Written {
            verified: verify_write(client, &target, &digest).await,
            digest,
            dropped: dropped.into_iter().collect(),
        },
        Err(source) => {
            // Logged once, here — the boundary that decided to carry on rather
            // than propagate. The row keeps the sentence for the report.
            log::error!("Failed to write alias '{}' of {identifier}: {source}", write.tag);
            WriteOutcome::Failed {
                message: source.to_string(),
            }
        }
    }
}

/// Re-reads `target` and reports whether it still names what this run wrote.
///
/// A mismatch means a concurrent writer, not a failed write: the PUT landed,
/// someone else's landed after it, and there is no conditional-write primitive
/// in the transport that could have prevented it. A read that fails only means
/// the check could not be made — neither is worth failing a completed write
/// over.
async fn verify_write(client: &oci::Client, target: &oci::Identifier, written: &oci::Digest) -> bool {
    match client
        .probe_manifest_digest_addressed(target, ReadAddressing::Canonical)
        .await
    {
        Ok(Some(observed)) => observed == *written,
        Ok(None) => false,
        Err(source) => {
            log::debug!("Could not read '{target}' back after writing it: {source}");
            false
        }
    }
}

// ── Tests (matrix G) ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::RegistryOperation;
    use crate::oci::client::error::ClientError;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};
    use crate::package::version::Version;
    use crate::{MEDIA_TYPE_OCI_IMAGE_INDEX, MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_PACKAGE_V1};

    const PLATFORMS: [&str; 2] = ["linux/amd64", "linux/arm64"];

    fn test_client(data: &StubTransportData) -> oci::Client {
        oci::Client::with_transport(Box::new(StubTransport::new(data.clone())))
    }

    fn test_identifier() -> oci::Identifier {
        oci::Identifier::new_registry("test/pkg", "example.com")
    }

    /// A well-formed, distinguishable child manifest digest.
    fn child_digest(seed: u8) -> String {
        format!("sha256:{seed:064x}")
    }

    /// The stub's key for a digest-addressed read of this package.
    fn child_key(digest: &str) -> String {
        test_identifier()
            .without_tag()
            .clone_with_digest(oci::Digest::try_from(digest).expect("well-formed digest"))
            .canonical_reference()
            .to_string()
    }

    /// The stub's key for a tag-addressed read or write of this package.
    fn tag_key(tag: &AliasTag) -> String {
        test_identifier()
            .clone_with_tag(tag.to_string())
            .canonical_reference()
            .to_string()
    }

    /// Makes the registry serve each digest, so preflight finds the children.
    fn seed_children(data: &StubTransportData, digests: &[String]) {
        for digest in digests {
            data.write()
                .manifests
                .insert(child_key(digest), (Vec::new(), digest.clone()));
        }
    }

    fn planned_write(tag: AliasTag, children: &[String]) -> PlannedWrite {
        let manifests = children
            .iter()
            .enumerate()
            .map(|(position, digest)| {
                let platform: oci::Platform = PLATFORMS[position % PLATFORMS.len()].parse().expect("valid platform");
                oci::ImageIndexEntry {
                    media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                    digest: digest.clone(),
                    size: 100,
                    platform: Some(platform.into()),
                    artifact_type: None,
                    annotations: None,
                }
            })
            .collect();
        PlannedWrite {
            tag,
            index: oci::ImageIndex {
                schema_version: oci::INDEX_SCHEMA_VERSION,
                media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
                manifests,
                annotations: None,
            },
            observed_digest: None,
            referenced_digests: children.to_vec(),
            reasons: Vec::new(),
        }
    }

    fn latest() -> AliasTag {
        AliasTag::Root { variant: None }
    }

    /// One row of a plan's justification, for the platform `planned_write`
    /// assigned to the child at `position`.
    fn reason(
        tag: &AliasTag,
        position: usize,
        status: SlotStatus,
        observed: &str,
    ) -> crate::package::cascade::graph::SlotRow {
        let platform: oci::Platform = PLATFORMS[position % PLATFORMS.len()].parse().expect("valid platform");
        crate::package::cascade::graph::SlotRow {
            tag: tag.clone(),
            platform: platform.into(),
            status,
            observed: Some(observed.to_string()),
            // An orphan is by definition a slot the fold expects nothing for;
            // every other status has an expectation behind it.
            expected: (status != SlotStatus::Orphan).then(|| observed.to_string()),
            source: None,
            observed_source: None,
        }
    }

    fn count_calls(data: &StubTransportData, method: &str) -> usize {
        data.read().calls.iter().filter(|call| *call == method).count()
    }

    /// G1: every PUT body is byte-identical to the planned index it came from.
    ///
    /// A repair re-points aliases at content that already exists; if the write
    /// path re-serialised, re-ordered or enriched the plan on its way out, the
    /// bytes the pure core reasoned about would not be the bytes published.
    #[tokio::test]
    async fn put_bodies_are_byte_identical_to_the_planned_indexes() {
        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let children = [child_digest(1), child_digest(2)];
        seed_children(&data, &children);

        let writes = vec![
            planned_write(AliasTag::Version(Version::new_minor(3, 28)), &children),
            planned_write(latest(), &children[..1]),
        ];
        let client = test_client(&data);

        let outcomes = apply(&client, &test_identifier(), &writes).await.unwrap();

        assert_eq!(outcomes.len(), 2, "every planned write reports an outcome");
        for outcome in &outcomes {
            match &outcome.outcome {
                WriteOutcome::Written { verified, .. } => assert!(
                    *verified,
                    "read-back of '{}' must confirm the digest this run wrote",
                    outcome.tag
                ),
                other => panic!("expected a written alias, got {other:?}"),
            }
        }
        for write in &writes {
            let captured = data
                .read()
                .manifests
                .get(&tag_key(&write.tag))
                .cloned()
                .expect("alias was pushed");
            assert_eq!(
                captured.0,
                serde_json::to_vec(&write.index).unwrap(),
                "PUT body for '{}' must be the planned index verbatim",
                write.tag
            );
        }
    }

    /// G2: a child manifest the registry no longer serves refuses that one
    /// alias; every other alias in the plan is still written.
    #[tokio::test]
    async fn missing_child_refuses_only_its_own_alias() {
        let data = StubTransportData::new();
        let live = [child_digest(1)];
        let dead = [child_digest(9)];
        // Only the live child is seeded — the dead one 404s, which is exactly
        // the dead-pointer orphan the repair must not republish.
        seed_children(&data, &live);

        let healthy = AliasTag::Version(Version::new_minor(3, 28));
        let writes = vec![planned_write(healthy.clone(), &live), planned_write(latest(), &dead)];
        let client = test_client(&data);

        let outcomes = apply(&client, &test_identifier(), &writes).await.unwrap();

        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].tag, latest(), "outcomes are sorted by tag");
        match &outcomes[0].outcome {
            WriteOutcome::Refused(Unrepairable::ChildManifestMissing { tag, digest }) => {
                assert_eq!(*tag, latest());
                assert_eq!(*digest, dead[0]);
            }
            other => panic!("expected the dead-pointer alias to be refused, got {other:?}"),
        }
        assert!(
            matches!(outcomes[1].outcome, WriteOutcome::Written { .. }),
            "the healthy alias '{healthy}' must still be written"
        );
        assert_eq!(
            count_calls(&data, "push_manifest_raw"),
            1,
            "exactly one alias is written; the refused one costs zero PUTs"
        );
    }

    /// G3: one registry rejection is that tag's outcome, not the run's.
    ///
    /// `push_results` is consumed in completion order, so which tag draws the
    /// failure is scheduling-dependent — the assertion is on the shape of the
    /// report, which is the property that matters: a `try_collect` here would
    /// have thrown away the two writes that landed.
    #[tokio::test]
    async fn one_rejected_write_is_reported_per_tag_not_propagated() {
        let data = StubTransportData::new();
        let children = [child_digest(1)];
        seed_children(&data, &children);
        data.write().push_results = vec![
            Ok(child_digest(0xA1)),
            Err(ClientError::Registry("manifest rejected".to_string().into())),
            Ok(child_digest(0xA3)),
        ];

        let writes = vec![
            planned_write(latest(), &children),
            planned_write(AliasTag::Version(Version::new_major(3)), &children),
            planned_write(AliasTag::Version(Version::new_minor(3, 28)), &children),
        ];
        let client = test_client(&data);

        let outcomes = apply(&client, &test_identifier(), &writes).await.unwrap();

        assert_eq!(outcomes.len(), 3, "a partial failure still reports every tag");
        let failed: Vec<&RepairOutcome> = outcomes
            .iter()
            .filter(|outcome| matches!(outcome.outcome, WriteOutcome::Failed { .. }))
            .collect();
        let written = outcomes
            .iter()
            .filter(|outcome| matches!(outcome.outcome, WriteOutcome::Written { .. }))
            .count();
        assert_eq!(failed.len(), 1, "exactly one write was rejected");
        assert_eq!(written, 2, "the other two landed and are reported as such");
        match &failed[0].outcome {
            WriteOutcome::Failed { message } => assert!(
                message.contains("manifest rejected"),
                "the rejection's cause must survive into the row: {message}"
            ),
            other => unreachable!("filtered to Failed, got {other:?}"),
        }
    }

    /// G4: a read-back that disagrees with what this run wrote is a warning on
    /// a completed write, never an error.
    ///
    /// The paired positive control is G1, where the read-back agrees and
    /// `verified` is true — without it a `verified: false` constant would pass
    /// this test just as happily.
    #[tokio::test]
    async fn post_write_digest_mismatch_warns_instead_of_failing() {
        let data = StubTransportData::new();
        let children = [child_digest(1)];
        seed_children(&data, &children);
        // A blanket HEAD answer: preflight sees the children as present, the
        // pre-write check sees the alias exactly where the plan observed it,
        // and the read-back sees a digest this run did not write — a publish
        // that landed in the window the write itself occupies, which is the one
        // race the pre-write check cannot close.
        data.write().digest = Some(child_digest(0xEE));

        let mut write = planned_write(latest(), &children);
        write.observed_digest = Some(oci::Digest::try_from(child_digest(0xEE).as_str()).expect("well-formed"));
        let writes = vec![write];
        let client = test_client(&data);

        let outcomes = apply(&client, &test_identifier(), &writes).await.unwrap();

        assert_eq!(outcomes.len(), 1);
        match &outcomes[0].outcome {
            WriteOutcome::Written { digest, verified, .. } => {
                assert!(!verified, "a foreign digest at the alias must read as unverified");
                assert_ne!(
                    digest.to_string(),
                    child_digest(0xEE),
                    "the reported digest is what this run wrote, not what it read back"
                );
            }
            other => panic!("a mismatch must still be a completed write, got {other:?}"),
        }
    }

    /// G5: the run authenticates for push before it reads or writes anything.
    ///
    /// The first authentication being `Push` is what pins the ordering: the
    /// preflight reads authenticate for `Pull`, so dropping the up-front call
    /// would put `Pull` first.
    #[tokio::test]
    async fn push_authentication_precedes_every_registry_call() {
        let data = StubTransportData::new();
        let children = [child_digest(1)];
        seed_children(&data, &children);

        let writes = vec![planned_write(latest(), &children)];
        let client = test_client(&data);

        apply(&client, &test_identifier(), &writes).await.unwrap();

        let first = data.read().auth_calls.first().cloned().expect("the run authenticated");
        assert_eq!(
            first,
            ("example.com".to_string(), RegistryOperation::Push),
            "the first authentication of a repair is the push scope"
        );
    }

    /// G6: an authentication failure costs zero PUTs and fails the whole run.
    #[tokio::test]
    async fn authentication_failure_writes_nothing() {
        let data = StubTransportData::new();
        let children = [child_digest(1)];
        seed_children(&data, &children);
        data.write().ensure_auth_error_override = Some("token expired".to_string());

        let writes = vec![
            planned_write(latest(), &children),
            planned_write(AliasTag::Version(Version::new_major(3)), &children),
        ];
        let client = test_client(&data);

        let result = apply(&client, &test_identifier(), &writes).await;

        assert!(result.is_err(), "a credential problem fails the run, not one alias");
        assert_eq!(
            data.read().calls,
            Vec::<String>::new(),
            "no registry call is issued after the auth refusal"
        );
        let auth_calls = data.read().auth_calls.clone();
        assert_eq!(
            auth_calls.len(),
            1,
            "the up-front push auth is the only attempt: {auth_calls:?}"
        );
        assert_eq!(auth_calls[0].1, RegistryOperation::Push);
    }

    /// G7: an empty plan touches the registry not at all — not even to
    /// authenticate.
    #[tokio::test]
    async fn empty_plan_performs_no_registry_call() {
        let data = StubTransportData::new();
        let client = test_client(&data);

        let outcomes = apply(&client, &test_identifier(), &[]).await.unwrap();

        assert!(outcomes.is_empty());
        assert!(data.read().calls.is_empty(), "no registry call for an empty plan");
        assert!(
            data.read().auth_calls.is_empty(),
            "not even a credential probe for an empty plan"
        );
    }

    /// G9: outcomes come back in tag order, whatever order the plan listed
    /// them in.
    ///
    /// The stub answers writes in submission order — no latency knob reaches
    /// this path — so what this pins is that the report is sorted rather than
    /// echoing the plan: the input below is deliberately in a different order
    /// from the output.
    ///
    /// The expected order is the tag graph's own: `latest` before any version,
    /// and a bare `3` *above* `3.28`, because [`Version`]'s ordering puts a
    /// rolling tag above the subtree it rolls.
    #[tokio::test]
    async fn outcomes_are_sorted_by_tag_not_by_plan_order() {
        let data = StubTransportData::new();
        let children = [child_digest(1)];
        seed_children(&data, &children);

        let plan_order = [
            AliasTag::Version(Version::new_minor(3, 28)),
            AliasTag::Version(Version::new_major(3)),
            latest(),
        ];
        let writes: Vec<PlannedWrite> = plan_order
            .iter()
            .map(|tag| planned_write(tag.clone(), &children))
            .collect();
        let client = test_client(&data);

        let outcomes = apply(&client, &test_identifier(), &writes).await.unwrap();

        let reported: Vec<AliasTag> = outcomes.iter().map(|outcome| outcome.tag.clone()).collect();
        assert_eq!(
            reported,
            vec![
                latest(),
                AliasTag::Version(Version::new_minor(3, 28)),
                AliasTag::Version(Version::new_major(3)),
            ],
            "the report is sorted by tag"
        );
        assert_ne!(reported, plan_order.to_vec(), "and that is not the plan's own order");
    }

    /// G10: a plan larger than the concurrency bound completes in full.
    #[tokio::test]
    async fn plan_larger_than_the_concurrency_bound_completes_in_full() {
        let data = StubTransportData::new();
        let children = [child_digest(1)];
        seed_children(&data, &children);

        let count = CASCADE_APPLY_CONCURRENCY + 4;
        let writes: Vec<PlannedWrite> = (0..count)
            .map(|minor| {
                planned_write(
                    AliasTag::Version(Version::new_minor(3, u32::try_from(minor).unwrap())),
                    &children,
                )
            })
            .collect();
        let client = test_client(&data);

        let outcomes = apply(&client, &test_identifier(), &writes).await.unwrap();

        assert_eq!(outcomes.len(), count, "every alias in an over-bound plan reports");
        assert!(
            outcomes
                .iter()
                .all(|outcome| matches!(outcome.outcome, WriteOutcome::Written { .. })),
            "and every one of them was written"
        );
        assert_eq!(count_calls(&data, "push_manifest_raw"), count);
    }

    /// G14: a configured mirror serves neither the preflight nor the read-back.
    ///
    /// Both are reads that decide, or confirm, a write to the canonical host.
    /// Only the canonical references are seeded, so a mirror-aware preflight
    /// would find the child gone and refuse the alias, and a mirror-aware
    /// read-back would report the landed write as unverified.
    #[tokio::test]
    async fn g14_preflight_and_read_back_bypass_a_configured_mirror() {
        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let children = [child_digest(1)];
        seed_children(&data, &children);

        let writes = vec![planned_write(latest(), &children)];
        let mirrored = test_client(&data).with_test_mirror("example.com", "mirror.invalid", "upstream");

        let outcomes = apply(&mirrored, &test_identifier(), &writes).await.unwrap();

        match &outcomes[0].outcome {
            WriteOutcome::Written { verified, .. } => {
                assert!(*verified, "the read-back must address the host the write went to")
            }
            other => panic!("the preflight must address the host the write goes to, got {other:?}"),
        }
        let auth_hosts: Vec<String> = data
            .read()
            .auth_calls
            .iter()
            .map(|(registry, _)| registry.clone())
            .collect();
        assert!(
            auth_hosts.iter().all(|host| host == "example.com"),
            "every call in the transaction must authenticate against the canonical host, got {auth_hosts:?}"
        );
    }

    /// G11: an alias with one stale folded slot and one dead orphan is
    /// written, with the dead orphan dropped from the index.
    ///
    /// The orphan policy in full: an orphan is preserved while its child
    /// exists (G2's healthy sibling, and `e3a` in the pure core) and removed
    /// once the registry provably no longer serves it. Refusing the whole
    /// alias here — what a per-alias reading of "a child is gone" gives — would
    /// leave the *stale* slot stale, so one dead leftover would permanently
    /// block the repair of everything beside it.
    #[tokio::test]
    async fn a_dead_orphan_is_dropped_and_its_alias_is_still_written() {
        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let folded = child_digest(1);
        let dead_orphan = child_digest(9);
        // Only the folded child is served; the orphan's child 404s.
        seed_children(&data, std::slice::from_ref(&folded));

        let tag = AliasTag::Version(Version::new_major(3));
        let mut write = planned_write(tag.clone(), &[folded.clone(), dead_orphan.clone()]);
        write.reasons = vec![
            reason(&tag, 0, SlotStatus::Stale, &folded),
            reason(&tag, 1, SlotStatus::Orphan, &dead_orphan),
        ];
        let client = test_client(&data);

        let outcomes = apply(&client, &test_identifier(), &[write.clone()]).await.unwrap();

        assert_eq!(outcomes.len(), 1);
        match &outcomes[0].outcome {
            WriteOutcome::Written { dropped, verified, .. } => {
                assert_eq!(
                    dropped,
                    &vec![dead_orphan.clone()],
                    "the outcome must name the dead pointer it removed"
                );
                assert!(*verified, "the read-back confirms what this run wrote");
            }
            other => panic!("a droppable orphan must not refuse the alias, got {other:?}"),
        }

        let pushed = data
            .read()
            .manifests
            .get(&tag_key(&tag))
            .cloned()
            .expect("the alias was pushed");
        let mut expected = write.index.clone();
        expected.manifests.retain(|entry| entry.digest != dead_orphan);
        assert_eq!(
            pushed.0,
            serde_json::to_vec(&expected).unwrap(),
            "the published index is the plan minus the dead entry — nothing else changed"
        );
        let written: oci::ImageIndex = serde_json::from_slice(&pushed.0).expect("a published index parses");
        assert_eq!(
            written.manifests.iter().map(|entry| &entry.digest).collect::<Vec<_>>(),
            vec![&folded],
            "the live child survives and the dead one is gone"
        );
    }

    /// G12: a dead child that backs a folded slot still refuses the alias,
    /// even when the same alias also has a droppable orphan.
    ///
    /// The discriminating case for the per-digest analysis: dropping is a
    /// property of what the digest backs, never of "something was missing".
    #[tokio::test]
    async fn a_dead_folded_child_refuses_even_beside_a_droppable_orphan() {
        let data = StubTransportData::new();
        let dead_folded = child_digest(1);
        let dead_orphan = child_digest(9);
        // Neither child is seeded — both are gone.

        let tag = latest();
        let mut write = planned_write(tag.clone(), &[dead_folded.clone(), dead_orphan.clone()]);
        write.reasons = vec![
            reason(&tag, 0, SlotStatus::Missing, &dead_folded),
            reason(&tag, 1, SlotStatus::Orphan, &dead_orphan),
        ];
        let client = test_client(&data);

        let outcomes = apply(&client, &test_identifier(), &[write]).await.unwrap();

        match &outcomes[0].outcome {
            WriteOutcome::Refused(Unrepairable::ChildManifestMissing { digest, .. }) => assert_eq!(
                *digest, dead_folded,
                "the refusal must name the child the fold depends on"
            ),
            other => panic!("a dead folded child must refuse the alias, got {other:?}"),
        }
        assert_eq!(
            count_calls(&data, "push_manifest_raw"),
            0,
            "a refused alias costs zero PUTs"
        );
    }

    /// G15: an alias that moved between the gather and the write is not
    /// written, and the aliases beside it still are.
    ///
    /// The registry has no conditional write, so the plan's observed digest is
    /// the only thing standing between a repair and a publisher's fresh push:
    /// without the check the repair overwrites it, and the post-write read-back
    /// then reports the repair's own clobber as `verified: true`.
    #[tokio::test]
    async fn g15_an_alias_that_moved_since_the_gather_is_not_written() {
        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let children = [child_digest(1)];
        seed_children(&data, &children);

        let raced = AliasTag::Version(Version::new_major(3));
        let mut moved = planned_write(raced.clone(), &children);
        // The plan was computed against a digest the alias no longer carries;
        // the registry now serves a publisher's newer one.
        moved.observed_digest = Some(oci::Digest::try_from(child_digest(0x11).as_str()).expect("well-formed"));
        let live = child_digest(0x22);
        data.write()
            .manifests
            .insert(tag_key(&raced), (Vec::new(), live.clone()));

        let writes = vec![moved, planned_write(latest(), &children)];
        let client = test_client(&data);

        let outcomes = apply(&client, &test_identifier(), &writes).await.unwrap();

        let raced_outcome = outcomes
            .iter()
            .find(|outcome| outcome.tag == raced)
            .expect("the raced alias reports");
        match &raced_outcome.outcome {
            WriteOutcome::Raced {
                expected,
                live: observed,
            } => {
                assert_eq!(expected.as_ref().map(ToString::to_string), Some(child_digest(0x11)));
                assert_eq!(observed.as_ref().map(ToString::to_string), Some(live.clone()));
            }
            other => panic!("a moved alias must not be overwritten, got {other:?}"),
        }
        assert_eq!(
            data.read()
                .manifests
                .get(&tag_key(&raced))
                .map(|(_, digest)| digest.clone()),
            Some(live),
            "the publisher's digest must still be what the tag carries"
        );
        assert!(
            matches!(
                outcomes
                    .iter()
                    .find(|outcome| outcome.tag == latest())
                    .expect("the untouched alias reports")
                    .outcome,
                WriteOutcome::Written { .. }
            ),
            "one raced alias must not stop the rest of the plan"
        );
        assert_eq!(
            count_calls(&data, "push_manifest_raw"),
            1,
            "exactly the unraced alias is written"
        );
    }

    /// G16: an alias that did not exist at gather time and exists now is a
    /// race too — the absent-versus-present direction of the same check.
    #[tokio::test]
    async fn g16_an_alias_created_since_the_gather_is_not_overwritten() {
        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let children = [child_digest(1)];
        seed_children(&data, &children);

        // `observed_digest: None` — the plan expects to create this alias.
        let writes = vec![planned_write(latest(), &children)];
        let live = child_digest(0x33);
        data.write()
            .manifests
            .insert(tag_key(&latest()), (Vec::new(), live.clone()));
        let client = test_client(&data);

        let outcomes = apply(&client, &test_identifier(), &writes).await.unwrap();

        match &outcomes[0].outcome {
            WriteOutcome::Raced {
                expected,
                live: observed,
            } => {
                assert!(expected.is_none(), "the plan expected no alias at all");
                assert_eq!(observed.as_ref().map(ToString::to_string), Some(live));
            }
            other => panic!("an alias created under the run must not be overwritten, got {other:?}"),
        }
        assert_eq!(count_calls(&data, "push_manifest_raw"), 0);
    }

    /// G13: a digest this build cannot address refuses its alias, and says so
    /// as itself rather than as a missing manifest.
    ///
    /// Nothing was observed to be gone: the check could not be made at all,
    /// and reporting that as "child manifest gone" would send a publisher
    /// looking for content that is very likely still there.
    #[tokio::test]
    async fn an_unaddressable_digest_is_refused_as_its_own_reason() {
        let data = StubTransportData::new();
        let unaddressable = "sha512-truncated:not-a-digest".to_string();

        let write = planned_write(latest(), std::slice::from_ref(&unaddressable));
        let client = test_client(&data);

        let outcomes = apply(&client, &test_identifier(), &[write]).await.unwrap();

        match &outcomes[0].outcome {
            WriteOutcome::Refused(Unrepairable::ChildDigestUnaddressable { tag, digest }) => {
                assert_eq!(*tag, latest());
                assert_eq!(*digest, unaddressable);
            }
            other => panic!("an unaddressable digest must be its own refusal, got {other:?}"),
        }
        assert_eq!(
            count_calls(&data, "fetch_manifest_digest"),
            0,
            "a digest that cannot be addressed is never probed for"
        );
        assert_eq!(count_calls(&data, "push_manifest_raw"), 0);
    }
}
