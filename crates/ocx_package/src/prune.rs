// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Tag deletion behind `ocx package prune`: select tags, check each against the index, delete.
//!
//! Registry only. The index is read, never written; every registry call is canonical.

use std::time::Duration;

use ocx_index::{IndexRoot, OcxIndex};
use ocx_oci::client::error::ClientError;
use ocx_oci::client::{DeleteOutcome, ManifestPresence, ReadAddressing};
use ocx_oci::{Digest, OciIdentifier};
use serde::Serialize;

use crate::version::Version;

/// Confirmation GETs after each DELETE, for registries whose tag index is eventually consistent.
pub const CONFIRM_TRIES: usize = 3;
/// Pause between two confirmation GETs.
pub const CONFIRM_INTERVAL: Duration = Duration::from_millis(250);

/// Which tags a run considers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum PruneSelection {
    /// Exactly these tags, deduplicated, in input order.
    Tags { tags: Vec<String> },
    /// Every build of one pre-release, plus its rolling tag.
    Prerelease {
        /// A pre-release without a build, e.g. `0.5.0-canary`.
        prerelease: Version,
        /// Keep the newest this many builds and the rolling tag; at least 1.
        #[schemars(range(min = 1))]
        keep_builds: Option<u32>,
    },
}

/// What a run does with its selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneRequest {
    pub selection: PruneSelection,
    /// Delete what the index would refuse, and run with no index at all.
    pub force: bool,
    /// Run the safeguard and report; delete nothing.
    pub dry_run: bool,
}

/// The served root a run judged, with where it came from.
#[derive(Debug, Clone)]
pub struct IndexedRoot {
    pub url: String,
    /// sha256 of the root bytes as served.
    pub root_sha256: Digest,
    pub root: IndexRoot,
}

/// Where a run deletes: the package, the repository its tags live in, and the root that says so.
#[derive(Debug, Clone)]
pub struct PruneTarget {
    /// The package as named, without tag or digest.
    pub package: OciIdentifier,
    /// The root's `repository` pointer after the host guard, or `package` when there is no index.
    pub repository: OciIdentifier,
    /// `None` for a namespace with no configured index.
    pub index: Option<IndexedRoot>,
}

/// The final state of one selected tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PruneAction {
    /// This run deleted it.
    Deleted,
    /// Already gone from the registry.
    Absent,
    /// A dry run would delete it.
    WouldDelete,
    /// Selected, then kept by `--keep-builds`.
    Kept,
    /// The safeguard refused it.
    Refused,
    /// Never reached: an earlier refusal or error stopped the run.
    NotAttempted,
}

/// Why a tag was kept or refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PruneReason {
    /// One of the newest `--keep-builds` builds.
    Newest,
    /// The pre-release's own tag, kept under `--keep-builds`.
    Rolling,
    /// The index lists it without the ephemeral marker.
    Durable,
    /// The registry has it but the index does not list it.
    NotInIndex,
}

impl std::fmt::Display for PruneAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Deleted => "deleted",
            Self::Absent => "absent",
            Self::WouldDelete => "would_delete",
            Self::Kept => "kept",
            Self::Refused => "refused",
            Self::NotAttempted => "not_attempted",
        })
    }
}

impl std::fmt::Display for PruneReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Newest => "newest",
            Self::Rolling => "rolling",
            Self::Durable => "durable",
            Self::NotInIndex => "not_in_index",
        })
    }
}

/// One row of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct PruneTag {
    pub tag: String,
    /// The root row's content, else the digest the registry served, else `null`.
    pub digest: Option<Digest>,
    pub action: PruneAction,
    pub reason: Option<PruneReason>,
    /// Whether the served root lists this tag; a tag it does not list never reaches the tags file.
    #[serde(skip)]
    pub in_root: bool,
}

/// The index a run read, as reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct PruneIndexReport {
    pub url: String,
    pub root_sha256: Digest,
}

/// What a run did, in processing order; printed whether or not the run failed.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct PruneOutcome {
    pub package: OciIdentifier,
    pub repository: OciIdentifier,
    pub selection: PruneSelection,
    pub force: bool,
    pub dry_run: bool,
    pub index: Option<PruneIndexReport>,
    pub tags: Vec<PruneTag>,
    /// Selection finished, so a real run owes the tags file even when nothing was deleted.
    #[serde(skip)]
    pub selected: bool,
}

impl PruneOutcome {
    /// The tags `--tags-file` receives, in row order, or `None` when this run writes no file:
    /// a dry run, or a run that stopped before its selection was known. A tag the served root
    /// does not list has no index entry, so announcing it would fail and is left out.
    pub fn tags_file_entries(&self) -> Option<Vec<String>> {
        if self.dry_run || !self.selected {
            return None;
        }
        Some(
            self.tags
                .iter()
                .filter(|row| row.in_root && matches!(row.action, PruneAction::Deleted | PruneAction::Absent))
                .map(|row| row.tag.clone())
                .collect(),
        )
    }
}

/// A run's report and, when it failed, why.
#[derive(Debug)]
pub struct PruneRun {
    pub outcome: PruneOutcome,
    pub error: Option<PruneError>,
}

/// Why a prune stopped.
#[derive(Debug, thiserror::Error)]
pub enum PruneError {
    /// `--prerelease` names no pre-release, or one with a build.
    #[error("--prerelease {value:?} must be a pre-release without a build, e.g. 0.5.0-canary")]
    NotAPrereleaseFamily { value: String },

    /// A TAG names a digest; deleting by digest removes every tag sharing it.
    #[error("{value:?} is a digest, not a tag; prune deletes tags only")]
    DigestTag { value: String },

    /// A TAG is outside the OCI tag grammar, or names an internal keep tag.
    #[error("{value:?} is not a tag prune may delete: {reason}")]
    InvalidTag { value: String, reason: &'static str },

    /// The package carries a tag or digest; tags are selected separately.
    #[error("package {package} names a tag or digest; pass the package alone and the tags after it")]
    PackageNotBare { package: String },

    /// The served root could not be read, so the registry location is unknown.
    #[error("reading the index root of {package} from {url}{}", transport_hint(.source))]
    RootUnreadable {
        package: String,
        url: String,
        #[source]
        source: ocx_index::error::Error,
    },

    /// The index has no root for the package.
    #[error("the index at {url} has no package {package}")]
    NotInIndex { package: String, url: String },

    /// The root's `repository` pointer failed to parse or names a forbidden host.
    #[error("the index root of {package} names a registry repository prune may not use")]
    RepositoryPointer {
        package: String,
        #[source]
        source: ocx_index::error::Error,
    },

    /// No index to ask and no `--force`.
    #[error(
        "refusing to delete tags of {package}: no index is configured for its namespace to mark them \
         ephemeral; nothing was deleted; pass --force to delete them anyway"
    )]
    NoIndex { package: String },

    /// The safeguard refused tags; nothing was deleted. A durable refusal wins over a pending one.
    #[error("{}", refusal_message(.package, .url, .durable, .not_in_index))]
    Refused {
        package: String,
        url: String,
        /// Listed without the ephemeral marker.
        durable: Vec<String>,
        /// In the registry, not yet in the index.
        not_in_index: Vec<String>,
    },

    /// The registry refused this run's credential on the first DELETE.
    #[error("deleting {tag} from {repository}: the credential lacks delete rights, or the tag is protected")]
    DeleteDenied {
        repository: String,
        tag: String,
        #[source]
        source: ClientError,
    },

    /// A deleted tag was still served after every confirmation GET.
    #[error("{tag} is still present in {repository} after its delete; retry")]
    StillPresent { repository: String, tag: String },

    /// Any other registry failure, including a registry that cannot delete tags.
    #[error(transparent)]
    Registry(#[from] ClientError),
}

// Only a transport failure is worth a retry; a malformed root stays malformed.
fn transport_hint(source: &ocx_index::error::Error) -> &'static str {
    match ocx_index::error::coalesced_cause(source) {
        ocx_index::error::Error::IndexHttpFailed { .. } => ": the index locates the registry; retry",
        _ => "",
    }
}

// One line: the error boundary strips newlines, so each hint rides inline after `hint:`.
fn refusal_message(package: &str, url: &str, durable: &[String], not_in_index: &[String]) -> String {
    let listed = |tags: &[String]| format!("{} {}", tags.join(", "), if tags.len() == 1 { "is" } else { "are" });
    let mut reasons = Vec::new();
    if !durable.is_empty() {
        reasons.push(format!("{} durable in the index at {url}", listed(durable)));
    }
    if !not_in_index.is_empty() {
        reasons.push(format!("{} not in the index at {url} yet", listed(not_in_index)));
    }
    let mut message = format!(
        "refusing to delete {} tag(s) of {package}: {}; nothing was deleted",
        durable.len() + not_in_index.len(),
        reasons.join("; ")
    );
    if !durable.is_empty() {
        message.push_str(if durable.len() == 1 {
            "; hint: pass --force to delete it anyway; the index row stays until a reviewed announce removes it"
        } else {
            "; hint: pass --force to delete them anyway; the index rows stay until a reviewed announce removes them"
        });
    }
    if !not_in_index.is_empty() {
        message.push_str(if not_in_index.len() == 1 {
            "; hint: retry once its announce has merged, or announce it with \
             `ocx package announce --ephemeral --tags-file PATH`"
        } else {
            "; hint: retry once their announce has merged, or announce them with \
             `ocx package announce --ephemeral --tags-file PATH`"
        });
    }
    message
}

/// Parses `--prerelease`: a [`Version`] with a pre-release and no build.
///
/// # Errors
///
/// [`PruneError::NotAPrereleaseFamily`] for anything else.
#[expect(
    clippy::result_large_err,
    reason = "these errors surface as clap argument errors, so boxing the large variants is not worth it"
)]
pub fn parse_prerelease_family(value: &str) -> Result<Version, PruneError> {
    match Version::parse(value) {
        Some(version) if version.has_prerelease() && !version.has_build() => Ok(version),
        _ => Err(PruneError::NotAPrereleaseFamily {
            value: value.to_string(),
        }),
    }
}

/// Parses an explicit TAG: the OCI tag grammar, never a digest or a keep tag.
///
/// # Errors
///
/// [`PruneError::DigestTag`] for a digest or a `@`-pinned reference, [`PruneError::InvalidTag`]
/// for anything else outside `[A-Za-z0-9_][A-Za-z0-9._-]{0,127}` or a `__ocx.keep.` tag.
#[expect(
    clippy::result_large_err,
    reason = "these errors surface as clap argument errors, so boxing the large variants is not worth it"
)]
pub fn parse_tag(value: &str) -> Result<String, PruneError> {
    // Every digest holds a `:`, so it gets the digest-specific message first.
    if value.contains(':') || value.contains('@') {
        return Err(PruneError::DigestTag {
            value: value.to_string(),
        });
    }
    // The tag is spliced into the DELETE URL; a `/`, `%` or `?` would address another manifest.
    if !ocx_oci::client::is_valid_oci_tag(value) {
        return Err(PruneError::InvalidTag {
            value: value.to_string(),
            reason: "a tag is 1 to 128 of A-Z a-z 0-9 _ . - and starts with a letter, digit or _",
        });
    }
    // A keep tag guards its manifest against registry GC; `--force` must not lift that.
    if value.starts_with(ocx_oci::tag::InternalTag::KEEP_TAG_PREFIX) {
        return Err(PruneError::InvalidTag {
            value: value.to_string(),
            reason: "keep tags protect their manifest and are never pruned",
        });
    }
    Ok(value.to_string())
}

/// Locates the repository to delete from: the served root's pointer, or `package` with no index.
///
/// Reads the root once, uncached, whether or not the run is forced. `index` must be built without
/// `[mirrors]` index entries, since the served root decides what may go; only its root read and
/// pointer guard are used, never the registry client it carries. The report and every error name
/// its base URL with userinfo redacted.
///
/// # Errors
///
/// [`PruneError::PackageNotBare`], [`PruneError::RootUnreadable`], [`PruneError::NotInIndex`],
/// [`PruneError::RepositoryPointer`].
pub async fn locate(package: &OciIdentifier, index: Option<&OcxIndex>) -> Result<PruneTarget, PruneError> {
    if package.tag().is_some() || package.digest().is_some() {
        return Err(PruneError::PackageNotBare {
            package: package.to_string(),
        });
    }
    let Some(source) = index else {
        return Ok(PruneTarget {
            package: package.clone(),
            repository: package.clone(),
            index: None,
        });
    };
    let url = source.redacted_base_url();
    let fetched = source
        .fetch_root_uncached(package.repository())
        .await
        .map_err(|error| PruneError::RootUnreadable {
            package: package.to_string(),
            url: url.clone(),
            source: error,
        })?;
    let Some((root_sha256, root)) = fetched else {
        return Err(PruneError::NotInIndex {
            package: package.to_string(),
            url,
        });
    };
    let repository = source
        .guard_repository_pointer(&root)
        .await
        .map_err(|error| PruneError::RepositoryPointer {
            package: package.to_string(),
            source: error,
        })?;
    Ok(PruneTarget {
        package: package.clone(),
        repository: repository.without_specifiers(),
        index: Some(IndexedRoot { url, root_sha256, root }),
    })
}

/// Selects, runs the safeguard over the whole selection, then deletes: builds oldest first, the
/// rolling tag last, each confirmed gone by a canonical GET.
///
/// `client` must be pinned to `target.repository`'s host; every call it makes is canonical.
pub async fn prune(client: &ocx_oci::Client, target: &PruneTarget, request: &PruneRequest) -> PruneRun {
    let mut outcome = PruneOutcome {
        package: target.package.clone(),
        repository: target.repository.clone(),
        selection: request.selection.clone(),
        force: request.force,
        dry_run: request.dry_run,
        index: target.index.as_ref().map(|indexed| PruneIndexReport {
            url: indexed.url.clone(),
            root_sha256: indexed.root_sha256.clone(),
        }),
        tags: Vec::new(),
        selected: false,
    };
    let error = run(client, target, request, &mut outcome).await.err();
    PruneRun { outcome, error }
}

/// A selected tag's verdict before any DELETE.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Keep(PruneReason),
    Delete,
    Absent,
    Refuse(PruneReason),
}

async fn run(
    client: &ocx_oci::Client,
    target: &PruneTarget,
    request: &PruneRequest,
    outcome: &mut PruneOutcome,
) -> Result<(), PruneError> {
    if target.index.is_none() && !request.force {
        return Err(PruneError::NoIndex {
            package: target.package.to_string(),
        });
    }

    let selected = select(client, target, &request.selection).await?;
    outcome.selected = true;

    let mut verdicts = Vec::with_capacity(selected.len());
    for (tag, kept) in selected {
        let (verdict, digest, in_root) = judge(client, target, &tag, kept, request.force).await?;
        verdicts.push(verdict);
        let (action, reason) = match verdict {
            Verdict::Keep(reason) => (PruneAction::Kept, Some(reason)),
            Verdict::Refuse(reason) => (PruneAction::Refused, Some(reason)),
            Verdict::Absent => (PruneAction::Absent, None),
            Verdict::Delete if request.dry_run => (PruneAction::WouldDelete, None),
            Verdict::Delete => (PruneAction::NotAttempted, None),
        };
        outcome.tags.push(PruneTag {
            tag,
            digest,
            action,
            reason,
            in_root,
        });
    }

    let refused = |reason: PruneReason| -> Vec<String> {
        outcome
            .tags
            .iter()
            .filter(|row| row.action == PruneAction::Refused && row.reason == Some(reason))
            .map(|row| row.tag.clone())
            .collect()
    };
    let durable = refused(PruneReason::Durable);
    let not_in_index = refused(PruneReason::NotInIndex);
    if !durable.is_empty() || !not_in_index.is_empty() {
        for row in &mut outcome.tags {
            if matches!(row.action, PruneAction::WouldDelete | PruneAction::Absent) {
                row.action = PruneAction::NotAttempted;
            }
        }
        return Err(PruneError::Refused {
            package: target.package.to_string(),
            url: target
                .index
                .as_ref()
                .map(|indexed| indexed.url.clone())
                .unwrap_or_default(),
            durable,
            not_in_index,
        });
    }
    if request.dry_run {
        return Ok(());
    }

    for (position, verdict) in verdicts.into_iter().enumerate() {
        if verdict != Verdict::Delete {
            continue;
        }
        let row = &mut outcome.tags[position];
        row.action = delete_and_confirm(client, &target.repository, &row.tag).await?;
    }
    Ok(())
}

/// The selected tags in processing order, each with its keep reason when `--keep-builds` spares it.
async fn select(
    client: &ocx_oci::Client,
    target: &PruneTarget,
    selection: &PruneSelection,
) -> Result<Vec<(String, Option<PruneReason>)>, PruneError> {
    match selection {
        PruneSelection::Tags { tags } => {
            let mut seen = std::collections::HashSet::new();
            Ok(tags
                .iter()
                .filter(|tag| seen.insert(tag.as_str()))
                .map(|tag| (tag.clone(), None))
                .collect())
        }
        PruneSelection::Prerelease {
            prerelease,
            keep_builds,
        } => {
            // A repository with no tags at all holds no family: nothing to select, not a failure.
            let listing = client
                .list_tags_or_empty_addressed(target.repository.without_specifiers(), ReadAddressing::Canonical)
                .await?;
            let mut builds: Vec<(Version, String)> = Vec::new();
            let mut rolling = None;
            for tag in listing {
                let Some(version) = Version::parse(&tag) else {
                    continue;
                };
                if version == *prerelease {
                    rolling = Some(tag);
                } else if version.has_build() && version.parent().as_ref() == Some(prerelease) {
                    builds.push((version, tag));
                }
            }
            builds.sort_by(|left, right| left.0.cmp(&right.0));

            let keep = keep_builds.map_or(0, |count| count as usize);
            let first_kept = builds.len().saturating_sub(keep);
            let mut selected: Vec<(String, Option<PruneReason>)> = builds
                .into_iter()
                .enumerate()
                .map(|(position, (_, tag))| (tag, (position >= first_kept).then_some(PruneReason::Newest)))
                .collect();
            if let Some(tag) = rolling {
                selected.push((tag, keep_builds.map(|_| PruneReason::Rolling)));
            }
            Ok(selected)
        }
    }
}

/// The safeguard's verdict on one tag, with the digest to report and whether the root lists it.
async fn judge(
    client: &ocx_oci::Client,
    target: &PruneTarget,
    tag: &str,
    kept: Option<PruneReason>,
    force: bool,
) -> Result<(Verdict, Option<Digest>, bool), PruneError> {
    let row = target.index.as_ref().and_then(|indexed| indexed.root.tags.get(tag));
    let in_root = row.is_some();
    let root_digest = row.map(|row| row.content.clone());
    if let Some(reason) = kept {
        return Ok((Verdict::Keep(reason), root_digest, in_root));
    }
    if let Some(row) = row {
        let verdict = if row.ephemeral || force {
            Verdict::Delete
        } else {
            Verdict::Refuse(PruneReason::Durable)
        };
        return Ok((verdict, root_digest, true));
    }
    // Not in the root: only the registry can say whether it still exists, forced or not.
    match client
        .probe_manifest_canonical(&target.repository.clone_with_tag(tag))
        .await?
    {
        ManifestPresence::Present(digest) => {
            let verdict = if force {
                Verdict::Delete
            } else {
                Verdict::Refuse(PruneReason::NotInIndex)
            };
            Ok((verdict, Some(digest), false))
        }
        ManifestPresence::Absent(_) => Ok((Verdict::Absent, None, false)),
    }
}

/// DELETEs `tag`, then GETs it until it is gone: [`PruneAction::Deleted`] after this run's own
/// delete, [`PruneAction::Absent`] when the registry already had no such tag.
async fn delete_and_confirm(
    client: &ocx_oci::Client,
    repository: &OciIdentifier,
    tag: &str,
) -> Result<PruneAction, PruneError> {
    let reference = repository.clone_with_tag(tag);
    let deleted = match client.delete_tag(&reference).await {
        Ok(outcome) => outcome,
        Err(ClientError::Authentication(message)) => {
            return Err(PruneError::DeleteDenied {
                repository: repository.to_string(),
                tag: tag.to_string(),
                source: ClientError::Authentication(message),
            });
        }
        Err(error) => return Err(PruneError::Registry(error)),
    };
    // Checked after `AlreadyAbsent` too: a registry hiding a repository from this credential answers 404.
    for attempt in 0..CONFIRM_TRIES {
        if attempt > 0 {
            tokio::time::sleep(CONFIRM_INTERVAL).await;
        }
        if let ManifestPresence::Absent(_) = client.probe_manifest_canonical(&reference).await? {
            return Ok(match deleted {
                DeleteOutcome::Deleted => PruneAction::Deleted,
                DeleteOutcome::AlreadyAbsent => PruneAction::Absent,
            });
        }
    }
    Err(PruneError::StillPresent {
        repository: repository.to_string(),
        tag: tag.to_string(),
    })
}

#[cfg(test)]
mod tests;
