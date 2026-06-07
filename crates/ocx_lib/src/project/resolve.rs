// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Resolve advisory tags in a [`ProjectConfig`] to pinned digests and
//! assemble a fresh [`ProjectLock`].
//!
//! The resolution is fully transactional: if any tool fails to resolve
//! (tag not found, registry unreachable, auth failure, timeout) the
//! function returns an error and no lock is produced.
//!
//! Tuning knobs (per-tool timeout, retry attempts, initial backoff)
//! default to conservative values tuned for transactional resolution.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;

use super::error::{ProjectError, ProjectErrorKind};
use super::lock::validate_canonical_platform_keys;
use super::{LockMetadata, LockVersion, LockedTool, ProjectConfig, ProjectLock};
use crate::oci::client::error::ClientError;
use crate::oci::index::{Index, IndexOperation};
use crate::oci::{Digest, Identifier, Platform, Selection, select_best};
use crate::project::hash::DECLARATION_HASH_VERSION;

/// Default per-tool timeout wrapping the entire retry chain.
pub const DEFAULT_PER_TOOL_TIMEOUT: Duration = Duration::from_secs(30);

/// Default number of retry attempts for transient registry failures.
pub const DEFAULT_RETRY_ATTEMPTS: u8 = 2;

/// Default initial backoff between retries (doubled each attempt).
pub const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);

/// Tuning knobs for [`resolve_lock`].
///
/// Per-tool timeout wraps the full retry chain. `retry_attempts` counts
/// additional retries after the initial attempt (so `2` means up to 3
/// total attempts). `initial_backoff` is doubled on each retry.
#[derive(Debug, Clone)]
pub struct ResolveLockOptions {
    /// Timeout wrapping the entire retry chain for a single tool.
    pub per_tool_timeout: Duration,
    /// Number of retries after the initial attempt.
    pub retry_attempts: u8,
    /// Backoff applied before the first retry; doubled each retry.
    pub initial_backoff: Duration,
}

impl Default for ResolveLockOptions {
    fn default() -> Self {
        Self {
            per_tool_timeout: DEFAULT_PER_TOOL_TIMEOUT,
            retry_attempts: DEFAULT_RETRY_ATTEMPTS,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
        }
    }
}

/// Resolve every tool in `config` to a pinned identifier and assemble a
/// fresh [`ProjectLock`].
///
/// Fully transactional — either every tool resolves or the function
/// returns an error and no lock is produced.
///
/// Group selection: when `groups` is empty, every `[tools]` and
/// `[group.*]` entry is resolved. Otherwise only the named groups are
/// resolved; the reserved name `"default"` selects the top-level
/// `[tools]` table.
///
/// # Errors
/// Returns [`super::Error`] on configuration validation failures, tag
/// resolution failures, registry I/O errors, authentication failures,
/// and timeouts.
pub async fn resolve_lock(
    config: &ProjectConfig,
    index: &Index,
    groups: &[String],
    options: ResolveLockOptions,
) -> Result<ProjectLock, super::Error> {
    // Validate + normalize the requested group filter. Empty `groups` means
    // "resolve every tool"; otherwise only the named groups are resolved.
    let selected = normalize_groups(groups, config)?;

    // Collect (group, binding, identifier) tuples in a deterministic order.
    // The final sort by (group, name) happens after resolution, but iterating
    // a `BTreeMap` here already yields stable order for testability.
    let work = collect_work(config, &selected);

    // Wrap the index in an `Arc` once at the top so per-task spawns share a
    // refcount bump rather than each cloning the underlying `Box<dyn IndexImpl>`
    // (`box_clone` is non-trivial — see `IndexImpl::box_clone` impls).
    let index = Arc::new(index.clone());
    let mut resolved = resolve_work(work, index, &options).await?;

    // Deterministic output: sort locked tools by (group, name).
    resolved.sort_by(|a, b| (a.group.as_str(), a.name.as_str()).cmp(&(b.group.as_str(), b.name.as_str())));

    Ok(build_lock(resolved, config))
}

/// Run the per-tool resolver for every tuple in `work`, returning the
/// merged set of [`LockedTool`]s in completion order.
///
/// `JoinSet` provides fail-fast semantics: on the first resolver error,
/// remaining tasks are cancelled via `abort_all()` and the error is
/// propagated. Callers are expected to apply their own deterministic
/// sort (by `(group, name)`) before serialization — this helper does
/// not sort, since partial-update callers compose its output with
/// preserved entries before sorting.
async fn resolve_work(
    work: Vec<(String, String, Identifier)>,
    index: Arc<Index>,
    options: &ResolveLockOptions,
) -> Result<Vec<LockedTool>, super::Error> {
    if work.is_empty() {
        return Ok(Vec::new());
    }

    let mut set: JoinSet<Result<LockedTool, super::Error>> = JoinSet::new();
    for (group, name, identifier) in work {
        let index = Arc::clone(&index);
        let options = options.clone();
        set.spawn(async move { resolve_one(index, group, name, identifier, options).await });
    }

    let mut resolved: Vec<LockedTool> = Vec::new();
    while let Some(join) = set.join_next().await {
        // `.expect` at the join boundary documents "a resolver task
        // panicked" — the inner `Result` is always propagated via `?`.
        match join.expect("project::resolve resolver task panicked") {
            Ok(tool) => resolved.push(tool),
            Err(err) => {
                // Fail fast: cancel the remaining tasks before returning.
                set.abort_all();
                return Err(err);
            }
        }
    }

    Ok(resolved)
}

/// Re-resolve exactly the `touched` `(group, name)` bindings and carry every
/// other predecessor entry forward verbatim — the whole-file model's
/// pin-preserving mutator path for `ocx add` / `ocx remove`.
///
/// Two distinct config snapshots are in flight and must never be conflated
/// (spec §3):
///
/// - `candidate` (post-mutation) drives the new-binding resolve and is stamped
///   into the produced lock's `declaration_hash` via [`build_lock`].
/// - `pre_mutation` (the `ocx.toml` snapshot taken *before* this command's
///   staged edit) is the freshness anchor **only**: it is compared against the
///   predecessor's `declaration_hash` so a clean `add` (whose `candidate`
///   differs by the inserted binding) does not spuriously fail closed.
///
/// Contract:
///
/// 1. **Freshness gate (always on).** If `hash(pre_mutation)` does not equal
///    `previous.metadata.declaration_hash`, return
///    [`ProjectErrorKind::StaleLockOnPartial`] **before any resolve** (exit
///    65). `current_hash` is `hash(pre_mutation)`; `previous_hash` is the
///    lock's.
/// 2. **Resolve exactly `touched`.** An empty set resolves nothing (the
///    `remove` case); a single new binding resolves just it (the `add` case).
///    This is the explicit-touched-set contract — it never falls back to
///    "resolve everything" on an empty set.
/// 3. **Carry forward** every predecessor entry whose `(group, name)` is not in
///    `touched` AND is still declared in `candidate` (so a `remove`'s dropped
///    binding falls out) — byte-identical, zero registry contact
///    ([`merge_carry_forward`]).
/// 4. **Stamp `hash(candidate)`** into the produced metadata.
///
/// Fully transactional: any resolution failure aborts before the merge,
/// returning an error with no [`ProjectLock`] produced.
///
/// # Errors
/// Returns [`super::Error`]: [`ProjectErrorKind::StaleLockOnPartial`] on a
/// drifted pre-mutation hash (65), plus the same tag-resolution / registry /
/// auth / policy failures as [`resolve_lock`] for the touched bindings.
pub async fn resolve_lock_touched(
    candidate: &ProjectConfig,
    pre_mutation: &ProjectConfig,
    previous: &ProjectLock,
    index: &Index,
    touched: &[(String, String)],
    options: ResolveLockOptions,
) -> Result<ProjectLock, super::Error> {
    // 1. Freshness gate (Codex H1) — anchored on the PRE-mutation snapshot, not
    //    the candidate. A clean `add` differs from `pre_mutation` only by the
    //    inserted binding, so anchoring on the candidate would always mismatch
    //    and fail every clean add closed. Compare `hash(pre_mutation)` against
    //    the predecessor's stamped hash; on drift, refuse before any resolve so
    //    the carry-forward can never launder a stale lock under a fresh hash.
    let pre_hash = pre_mutation.declaration_hash_cached();
    if previous.metadata.declaration_hash != pre_hash {
        return Err(ProjectError::new(
            PathBuf::new(),
            ProjectErrorKind::StaleLockOnPartial {
                previous_hash: previous.metadata.declaration_hash.clone(),
                current_hash: pre_hash.to_string(),
            },
        )
        .into());
    }

    // 2. Structural coherence gate (Block 2): the freshness gate above only
    //    proves the predecessor described the same CONFIG — `declaration_hash`
    //    is over `ocx.toml`, not the lock's `[[tool]]` entries. A corrupt or
    //    hand-edited lock with the right metadata hash but a missing / extra /
    //    duplicated / wrong-repository entry would otherwise pass, then be
    //    laundered by the carry-forward under a fresh candidate hash. Validate
    //    that every declared binding is covered by exactly one of {a touched
    //    pair} ∪ {a carried predecessor entry}, and that each carried entry's
    //    bare repository matches the candidate's declared identifier. Any
    //    mismatch is `StaleLockOnPartial` (65) — refuse before any resolve.
    validate_predecessor_coherence(candidate, previous, touched)?;

    // 3. Resolve EXACTLY the touched `(group, name)` bindings against the live
    //    index. The empty set resolves nothing (remove); the explicit-touched-
    //    set contract never falls back to "resolve everything". Each binding's
    //    identifier is looked up in `candidate` (the post-mutation config); a
    //    touched pair absent from `candidate` is a `ToolNotInConfig` (79) error
    //    rather than a silently dropped binding (Block 4).
    // Deduplicate touched pairs before building the work list. The coherence gate
    // above already collapses predecessor duplicates; the work vector must too so
    // a duplicate (group, name) in the caller's touched slice resolves at most once.
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    let mut work: Vec<(String, String, Identifier)> = Vec::with_capacity(touched.len());
    for (group, name) in touched {
        if !seen.insert((group.as_str(), name.as_str())) {
            // Already queued this pair; skip before the declared_identifier lookup
            // so a duplicate of a valid binding is dropped (resolved once), while
            // a genuinely undeclared pair still returns ToolNotInConfig on its
            // first occurrence.
            continue;
        }
        let Some(identifier) = declared_identifier(candidate, group, name) else {
            return Err(
                ProjectError::new(PathBuf::new(), ProjectErrorKind::ToolNotInConfig { name: name.clone() }).into(),
            );
        };
        work.push((group.clone(), name.clone(), identifier));
    }

    // See `resolve_lock` for the rationale on wrapping `index` once in an `Arc`.
    let index = Arc::new(index.clone());
    let resolved = resolve_work(work, index, &options).await?;

    // 4. Carry every untouched, still-declared predecessor entry forward
    //    verbatim, then append + sort.
    let tools = merge_carry_forward(previous, resolved, candidate);

    // 5. Stamp `hash(candidate)` into the produced metadata (`build_lock` reads
    //    `candidate.declaration_hash_cached()`), so the commit-time coherence
    //    gate — which compares `staged.candidate` hash vs the lock's metadata
    //    hash — passes.
    Ok(build_lock(tools, candidate))
}

/// Carry forward every predecessor entry the caller did not (re)resolve and is
/// still declared in `candidate`, then append `resolved` and sort.
///
/// Shared by the carry-forward path of [`resolve_lock_touched`]. A predecessor
/// entry is carried iff its `(group, name)` is:
///
/// - **not** among the freshly `resolved` keys (those entries are produced from
///   the live registry, not carried), and
/// - **still declared** in `candidate` (a binding `remove` dropped from
///   `ocx.toml` must not survive in the lock).
///
/// Every carried entry passes through byte-identical — zero registry contact
/// — because [`ProjectLock::load`] rejects any lock version other than V3
/// before `previous` ever reaches this function (D3: no bridged legacy
/// read), so a carried entry is always already in the current shape.
fn merge_carry_forward(
    previous: &ProjectLock,
    resolved: Vec<LockedTool>,
    candidate: &ProjectConfig,
) -> Vec<LockedTool> {
    // Carry forward every predecessor entry whose `(group, name)` was NOT just
    // resolved AND is still declared in `candidate`. The "still declared"
    // filter is what drops a `remove`'s binding (absent from `candidate`) from
    // the new lock. The block scope on `new_keys` ends its borrow into
    // `resolved` before `resolved` is moved into `tools` below.
    let mut tools: Vec<LockedTool> = {
        let new_keys: HashSet<(&str, &str)> = resolved.iter().map(|t| (t.group.as_str(), t.name.as_str())).collect();
        previous
            .tools
            .iter()
            .filter(|tool| !new_keys.contains(&(tool.group.as_str(), tool.name.as_str())))
            .filter(|tool| declared_identifier(candidate, &tool.group, &tool.name).is_some())
            .cloned()
            .collect()
    };
    tools.extend(resolved);

    // Deterministic output: same `(group, name)` order as `resolve_lock`.
    tools.sort_by(|a, b| (a.group.as_str(), a.name.as_str()).cmp(&(b.group.as_str(), b.name.as_str())));

    tools
}

/// Validate that the `previous` lock structurally describes `candidate`'s
/// declared bindings before any carry-forward laundering can occur (Codex
/// Block 2).
///
/// The two-hash freshness gate proves the predecessor was current with the same
/// *config* (`declaration_hash` is over `ocx.toml`, not the lock's `[[tool]]`
/// entries). It cannot detect a corrupt or hand-edited lock that carries the
/// right metadata hash but a missing, extra, duplicated, or wrong-repository
/// entry. Such a lock would pass the freshness gate, then be carried forward and
/// stamped with a fresh candidate hash — laundering the corruption.
///
/// This gate closes that gap. Every declared binding in `candidate` must be
/// covered by **exactly one** of:
///
/// - a `touched` `(group, name)` pair (it will be resolved fresh), or
/// - a single carried predecessor entry (it will be carried forward),
///
/// and each carried entry's bare `repository` must equal the bare repository of
/// `candidate`'s declared identifier for that `(group, name)`. Any uncovered
/// declared binding, duplicate carried entry, or repository divergence returns
/// [`ProjectErrorKind::StaleLockOnPartial`] (exit 65) before any registry
/// contact. Reuses the existing variant — no new error variant (KISS/YAGNI).
///
/// # Errors
/// Returns [`super::Error`] with [`ProjectErrorKind::StaleLockOnPartial`] on any
/// structural incoherence between `previous` and `candidate`.
fn validate_predecessor_coherence(
    candidate: &ProjectConfig,
    previous: &ProjectLock,
    touched: &[(String, String)],
) -> Result<(), super::Error> {
    let stale = || {
        ProjectError::new(
            PathBuf::new(),
            ProjectErrorKind::StaleLockOnPartial {
                previous_hash: previous.metadata.declaration_hash.clone(),
                current_hash: candidate.declaration_hash_cached().to_string(),
            },
        )
        .into()
    };

    let touched_keys: HashSet<(&str, &str)> = touched.iter().map(|(g, n)| (g.as_str(), n.as_str())).collect();

    // Index the predecessor's carried entries by `(group, name)`, rejecting a
    // duplicate key (a corrupt lock with two entries for the same binding).
    let mut carried: BTreeMap<(&str, &str), &LockedTool> = BTreeMap::new();
    for tool in &previous.tools {
        let key = (tool.group.as_str(), tool.name.as_str());
        // A binding that will be resolved fresh need not (and may not coherently)
        // appear among the carried entries; only validate carry-set duplicates.
        if touched_keys.contains(&key) {
            continue;
        }
        if carried.insert(key, tool).is_some() {
            return Err(stale());
        }
    }

    // Every declared binding must be covered by exactly one of {touched} ∪
    // {carried}, and carried entries must match the declared repository.
    for (group, name, declared) in collect_work(candidate, &None) {
        let key = (group.as_str(), name.as_str());
        if touched_keys.contains(&key) {
            continue;
        }
        let Some(tool) = carried.get(&key) else {
            // Declared but neither touched nor carried — a missing entry.
            return Err(stale());
        };
        if tool.repository != declared.without_specifiers() {
            return Err(stale());
        }
    }

    Ok(())
}

/// Validate `groups` argument and return the set of group names to
/// resolve. An empty input means "resolve every group".
///
/// `default` is accepted — it is the reserved name for the top-level
/// `[tools]` table and callers may legitimately request it as a filter.
///
/// # Errors
///
/// - [`ProjectErrorKind::EmptyGroupFilter`] when a group segment is
///   empty (e.g. `-g ci,,lint` after clap's comma-split).
/// - [`ProjectErrorKind::UnknownGroup`] when a segment names a group
///   not declared in `ocx.toml` (and is not the reserved `default`).
///
/// Both cases are pre-validated by the CLI layer with a dedicated
/// UsageError (64) exit path; the library-level variants exist as
/// defense-in-depth for direct library consumers.
fn normalize_groups(groups: &[String], config: &ProjectConfig) -> Result<Option<Vec<String>>, super::Error> {
    if groups.is_empty() {
        return Ok(None);
    }

    let mut out: Vec<String> = Vec::with_capacity(groups.len());
    for raw in groups {
        if raw.is_empty() {
            return Err(ProjectError::new(PathBuf::new(), ProjectErrorKind::EmptyGroupFilter).into());
        }
        if raw == super::internal::DEFAULT_GROUP {
            if !out.iter().any(|g| g == raw) {
                out.push(raw.clone());
            }
            continue;
        }
        if !config.groups.contains_key(raw) {
            return Err(ProjectError::new(PathBuf::new(), ProjectErrorKind::UnknownGroup { name: raw.clone() }).into());
        }
        if !out.iter().any(|g| g == raw) {
            out.push(raw.clone());
        }
    }
    Ok(Some(out))
}

/// Collect the `(group, name, identifier)` tuples that should be
/// resolved given the `selected` filter. `None` means "all groups".
fn collect_work(config: &ProjectConfig, selected: &Option<Vec<String>>) -> Vec<(String, String, Identifier)> {
    let mut work: Vec<(String, String, Identifier)> = Vec::new();

    // Top-level `[tools]` → reserved `default` group.
    let include_default = selected
        .as_ref()
        .is_none_or(|s| s.iter().any(|g| g == super::internal::DEFAULT_GROUP));
    if include_default {
        for (name, id) in &config.tools {
            work.push((super::internal::DEFAULT_GROUP.to_string(), name.clone(), id.clone()));
        }
    }

    // Named groups.
    for (group_name, group_tools) in &config.groups {
        let include = selected.as_ref().is_none_or(|s| s.iter().any(|g| g == group_name));
        if !include {
            continue;
        }
        for (name, id) in group_tools {
            work.push((group_name.clone(), name.clone(), id.clone()));
        }
    }

    work
}

/// Look up the `ocx.toml` declared identifier for a `(group, name)` binding.
///
/// `"default"` maps to the top-level `[tools]` table; any other group maps to
/// the matching `[group.<group>]` table. Returns `None` when the binding is no
/// longer declared (e.g. the entry was carried forward from a lock whose
/// `ocx.toml` declaration has since been removed).
fn declared_identifier(config: &ProjectConfig, group: &str, name: &str) -> Option<Identifier> {
    if group == super::internal::DEFAULT_GROUP {
        config.tools.get(name).cloned()
    } else {
        config.groups.get(group).and_then(|tools| tools.get(name)).cloned()
    }
}

/// Resolve a single `(group, name, identifier)` to a [`LockedTool`], wrapping
/// the fetch in `tokio::time::timeout`.
///
/// Uses `fetch_candidates` (one entry per advertised index child): it stores
/// the bare `repository` (`identifier` with tag and digest stripped) plus one
/// leaf-digest entry per advertised child, keyed by the child's canonical
/// grammar [`Platform`] string. No omitted-platform rows are synthesized and
/// the outer index digest is discarded. A flat `Manifest::Image` package
/// yields a single `"any"` entry. An `Option::None`/empty candidate set on a
/// `Resolve` miss surfaces as [`ProjectErrorKind::PolicyBlocked`] / a
/// not-found error per the active routing policy.
async fn resolve_one(
    index: Arc<Index>,
    group: String,
    name: String,
    identifier: Identifier,
    options: ResolveLockOptions,
) -> Result<LockedTool, super::Error> {
    // Wrap the whole resolve (retry + policy classification + candidate
    // fan-out into the available-only `platforms` map) in the per-tool
    // timeout. `resolve_to_platforms` owns the retry/classification; the
    // timeout here bounds the entire chain so a hung registry surfaces as
    // `ResolveTimeout` rather than blocking the join set.
    let timeout = options.per_tool_timeout;
    let retry_chain = resolve_to_platforms(&index, identifier.clone(), options);
    let platforms = match tokio::time::timeout(timeout, retry_chain).await {
        Ok(Ok(platforms)) => platforms,
        Ok(Err(err)) => return Err(err),
        Err(_elapsed) => {
            return Err(ProjectError::new(
                PathBuf::new(),
                ProjectErrorKind::ResolveTimeout {
                    identifier: Box::new(identifier),
                },
            )
            .into());
        }
    };

    Ok(LockedTool {
        name,
        group,
        // Bare repository coordinates — the outer index digest is
        // discarded; per-platform pull ids are reconstructed from
        // `repository` + each leaf digest.
        repository: identifier.without_specifiers(),
        platforms,
    })
}

/// Resolve `identifier`'s advertised index children into the available-only
/// `platforms` map, applying the retry/policy classification first.
///
/// The retry chain (`retry_fetch`) validates the tag resolves and produces
/// the policy-aware error classification (offline/frozen → PolicyBlocked,
/// auth → AuthError, etc.). Its result (the outer index digest) is discarded.
/// `fetch_candidates(Resolve)` then fans the parent manifest into one leaf per
/// advertised child; a `None`/empty candidate set surfaces as `TagNotFound`.
async fn resolve_to_platforms(
    index: &Index,
    identifier: Identifier,
    options: ResolveLockOptions,
) -> Result<BTreeMap<String, Digest>, super::Error> {
    // Validate the tag resolves + classify policy/auth/transient failures.
    // The returned index digest is discarded — only the per-platform leaves
    // are durable (the index digest is orphaned on the next push).
    retry_fetch(index, identifier.clone(), options).await?;
    fan_out_candidates(index, &identifier, IndexOperation::Resolve).await
}

/// Fetch the advertised index children of `identifier` under `op` and build
/// the available-only `platforms` map. A `None`/empty candidate set on a
/// `Resolve` miss surfaces as [`ProjectErrorKind::TagNotFound`] — never a
/// silent empty map.
async fn fan_out_candidates(
    index: &Index,
    identifier: &Identifier,
    op: IndexOperation,
) -> Result<BTreeMap<String, Digest>, super::Error> {
    let candidates = index
        .fetch_candidates(identifier, op)
        .await
        .map_err(|err| candidate_fetch_error(err, identifier.clone()))?;

    match candidates {
        Some(children) if !children.is_empty() => build_platforms_map(children),
        _ => Err(ProjectError::new(
            PathBuf::new(),
            ProjectErrorKind::TagNotFound {
                identifier: Box::new(identifier.clone()),
            },
        )
        .into()),
    }
}

/// Map a `fetch_candidates` failure onto a project-tier error, preserving the
/// policy-block classification (offline/frozen → PolicyBlocked) so a
/// no-resolve policy refusal during the candidate fan-out is not laundered
/// into a generic registry error.
fn candidate_fetch_error(err: crate::Error, identifier: Identifier) -> super::Error {
    if let Some(policy) = policy_block_label(&err) {
        return ProjectError::new(
            PathBuf::new(),
            ProjectErrorKind::PolicyBlocked {
                identifier: Box::new(identifier),
                policy,
            },
        )
        .into();
    }
    project_err_from_client(err, identifier, |id, src| ProjectErrorKind::RegistryUnreachable {
        identifier: Box::new(id),
        source: src,
    })
}

/// Build the available-only `platforms` map from the advertised candidate
/// children returned by `Index::fetch_candidates`.
///
/// Each entry's leaf digest is keyed by the child's canonical grammar
/// [`Platform`] string (D2). The runtime dup-key guard ([`guard_unique_key`])
/// fires cleanly (never silently overwrites) if two advertised children
/// stringify to the same key — a defensive assertion that should never fire
/// under an injective key. [`validate_canonical_platform_keys`] is run over
/// the finished map as a write-site defense-in-depth check (D3) — every key
/// here is already canonical by construction (it comes straight from
/// `Platform::to_string()`), so this never fires in practice.
fn build_platforms_map(candidates: Vec<(Identifier, Platform)>) -> Result<BTreeMap<String, Digest>, super::Error> {
    // Each child's leaf digest is inserted through `guard_unique_key` keyed
    // by `platform.to_string()`. A candidate without a digest is skipped: the
    // resolver always pins each child to its leaf digest via
    // `clone_with_digest`, so this never drops an advertised leaf in practice.
    let mut map: BTreeMap<String, Digest> = BTreeMap::new();
    for (candidate, platform) in candidates {
        let key = platform.to_string();
        if let Some(digest) = candidate.digest() {
            guard_unique_key(&mut map, key, digest)?;
        }
    }
    validate_canonical_platform_keys(&map).map_err(|kind| ProjectError::new(PathBuf::new(), kind))?;
    Ok(map)
}

/// Insert `key → digest` into `map`, failing cleanly if `key` is already
/// present (defensive dup-key guard — never a silent `BTreeMap` overwrite).
fn guard_unique_key(map: &mut BTreeMap<String, Digest>, key: String, digest: Digest) -> Result<(), super::Error> {
    if map.contains_key(&key) {
        return Err(ProjectError::new(PathBuf::new(), ProjectErrorKind::DuplicatePlatformKey { key }).into());
    }
    map.insert(key, digest);
    Ok(())
}

/// Look up the leaf [`Digest`] compatible with `host` in a lock/pin
/// `platforms` map (D1: `lookup_host_leaf` routes through the same
/// `select_best` helper as fresh-resolve, closing the dual-libc silent-`None`
/// gap the old exact-key tier walk had).
///
/// Parses every map key via [`Platform::from_str`], then scores each parsed
/// candidate against `host` with [`select_best`] — the identical relation and
/// scoring [`crate::oci::Index::select`] uses at fresh-resolve time, so
/// lock-read and fresh-resolve give provably identical answers for the same
/// host and candidate set. A key that fails to parse is skipped (canonical-key
/// validation already rejects it at load/write time — see
/// [`validate_canonical_platform_keys`] — so this is unreachable for a lock
/// that passed that gate, but the lookup itself does not re-validate).
///
/// Returns the [`Selection`] verbatim — `Selection::Found` carries the
/// winning `(digest, original map key)` pair; `Selection::Ambiguous` carries
/// one pair per tied entry, so a caller that needs to name the tied
/// candidates (e.g. an `AmbiguousHostLeaf` diagnostic) reads their original
/// canonical-grammar keys directly, with no re-parse. `Selection::None` means
/// no entry is compatible with `host` at all. Unlike the pre-D1-F2 shape
/// (`Option<&Digest>`), `None` and `Ambiguous` are no longer conflated —
/// `Option` could not distinguish "no compatible entry" (the publisher
/// genuinely does not ship this platform) from "two or more entries tie at
/// the maximum score" (e.g. a dual-libc host against separate single-libc
/// entries with no combined entry), so callers that need to route the two to
/// different remedies previously collapsed both to the same "no `<host>`
/// leaf" error and the wrong one ("re-resolve") for a genuine ambiguity.
pub fn lookup_host_leaf<'a>(
    platforms: &'a BTreeMap<String, Digest>,
    host: &Platform,
) -> Selection<(&'a Digest, &'a str)> {
    let candidates: Vec<((&'a Digest, &'a str), Platform)> = platforms
        .iter()
        .filter_map(|(key, digest)| {
            key.parse::<Platform>()
                .ok()
                .map(|platform| ((digest, key.as_str()), platform))
        })
        .collect();
    select_best(host, &candidates)
}

/// Run the retry chain: up to `retry_attempts` retries on
/// [`ClientError::Registry`]; `Authentication`, `Ok(None)`, and any
/// other terminal classification returns immediately.
async fn retry_fetch(
    index: &Index,
    identifier: Identifier,
    options: ResolveLockOptions,
) -> Result<crate::oci::Digest, super::Error> {
    let mut attempt: u8 = 0;
    let mut backoff = options.initial_backoff;

    loop {
        match index.fetch_manifest_digest(&identifier, IndexOperation::Resolve).await {
            Ok(Some(digest)) => return Ok(digest),
            Ok(None) => {
                return Err(ProjectError::new(
                    PathBuf::new(),
                    ProjectErrorKind::TagNotFound {
                        identifier: Box::new(identifier),
                    },
                )
                .into());
            }
            Err(err) => {
                // A no-resolve policy (offline / frozen) refused this unpinned
                // tag at the index boundary. Terminal — no retry, no backoff —
                // and routed to its own `ProjectErrorKind` so it classifies as
                // PolicyBlocked (81) rather than falling through to
                // `ClientFailure::Other` → RegistryUnreachable (69).
                if let Some(policy) = policy_block_label(&err) {
                    return Err(ProjectError::new(
                        PathBuf::new(),
                        ProjectErrorKind::PolicyBlocked {
                            identifier: Box::new(identifier),
                            policy,
                        },
                    )
                    .into());
                }
                let classification = classify_client_error(&err);
                match classification {
                    ClientFailure::Transient if attempt < options.retry_attempts => {
                        tokio::time::sleep(backoff).await;
                        backoff = backoff.saturating_mul(2);
                        attempt += 1;
                        continue;
                    }
                    ClientFailure::Transient => {
                        // Retry budget exhausted.
                        return Err(project_err_from_client(err, identifier, |id, src| {
                            ProjectErrorKind::RegistryUnreachable {
                                identifier: Box::new(id),
                                source: src,
                            }
                        }));
                    }
                    ClientFailure::Auth => {
                        return Err(project_err_from_client(err, identifier, |id, src| {
                            ProjectErrorKind::AuthFailure {
                                identifier: Box::new(id),
                                source: src,
                            }
                        }));
                    }
                    ClientFailure::NotFound => {
                        return Err(ProjectError::new(
                            PathBuf::new(),
                            ProjectErrorKind::TagNotFound {
                                identifier: Box::new(identifier),
                            },
                        )
                        .into());
                    }
                    ClientFailure::Other => {
                        // No structured `ClientError` classification applied —
                        // treat as a registry-tier failure so the classification
                        // table remains total. Exit code falls into Unavailable
                        // (69), which is the safest default for "registry said
                        // no, but we don't know why."
                        return Err(project_err_from_client(err, identifier, |id, src| {
                            ProjectErrorKind::RegistryUnreachable {
                                identifier: Box::new(id),
                                source: src,
                            }
                        }));
                    }
                }
            }
        }
    }
}

/// Outcome of inspecting a fetch error for retry/classification routing.
enum ClientFailure {
    /// Registry-side transient (network, 5xx) — subject to the retry budget.
    Transient,
    /// Terminal authentication failure — no retry, maps to AuthError (80).
    Auth,
    /// 404-equivalent — no retry, maps to NotFound (79).
    NotFound,
    /// Anything else; propagated as-is.
    Other,
}

/// Walk `err`'s source chain looking for a [`ClientError`] and classify it.
///
/// `crate::Error::OciClient` uses `#[error(transparent)]`, which makes
/// `source()` delegate past the `ClientError` directly to its inner
/// `#[source]` field. A naive `source()` walk therefore never sees the
/// `ClientError` itself. We compensate by special-casing the wrappers
/// that carry a typed `crate::Error` payload — `OciClient` directly, and
/// `OciIndex(SourceWalkFailed(ArcError))` for errors surfaced by the
/// chained-index source walk — before starting the generic `source()`
/// walk for any remaining downcast opportunities (e.g. nested via
/// `ClientError::Internal`).
/// Detect an index-layer no-resolve policy block in `err`.
///
/// Returns the lowercase policy label (`"offline"` / `"frozen"`) when the
/// chained index refused to resolve an unpinned tag under `--offline` /
/// `--frozen`. The error arrives as
/// [`crate::Error::OciIndex`]`(`[`PolicyResolutionBlocked`]`)` straight from
/// `ChainedIndex::walk_chain` — it is raised before the singleflight walk, so
/// it is never wrapped in `SourceWalkFailed`. Terminal: the caller returns
/// immediately without consuming the retry budget.
///
/// [`PolicyResolutionBlocked`]: crate::oci::index::error::Error::PolicyResolutionBlocked
fn policy_block_label(err: &crate::Error) -> Option<&'static str> {
    if let crate::Error::OciIndex(crate::oci::index::error::Error::PolicyResolutionBlocked { policy, .. }) = err {
        return Some(policy);
    }
    None
}

fn classify_client_error(err: &crate::Error) -> ClientFailure {
    // Direct `crate::Error::OciClient` — the simple case.
    if let crate::Error::OciClient(client) = err {
        return classify(client);
    }
    // Chained-index source-walk failures wrap the original typed error
    // in an `ArcError`. Recurse into it so a `ManifestNotFound` surfaced
    // by a source registry classifies as `NotFound` rather than
    // `Transient` (which would trigger retry + exit 69 Unavailable).
    if let crate::Error::OciIndex(crate::oci::index::error::Error::SourceWalkFailed(arc)) = err {
        return classify_client_error(arc.as_error());
    }
    let mut cause: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    while let Some(c) = cause {
        if let Some(client) = c.downcast_ref::<ClientError>() {
            return classify(client);
        }
        cause = c.source();
    }
    ClientFailure::Other
}

fn classify(client: &ClientError) -> ClientFailure {
    match client {
        // Transient: registry-side failures, file I/O, rate limits, and 5xx are retryable.
        ClientError::Registry(_)
        | ClientError::Io { .. }
        | ClientError::RateLimited { .. }
        | ClientError::ServiceUnavailable { .. } => ClientFailure::Transient,
        // Terminal auth/permission failures — no retry.
        ClientError::Authentication(_) | ClientError::Unauthorized { .. } | ClientError::Forbidden { .. } => {
            ClientFailure::Auth
        }
        // 404-equivalent — no retry.
        ClientError::ManifestNotFound(_) | ClientError::BlobNotFound(_) | ClientError::RepositoryNotFound(_) => {
            ClientFailure::NotFound
        }
        // Data errors and other structural failures — not retryable.
        ClientError::InvalidManifest(_)
        | ClientError::DigestMismatch { .. }
        | ClientError::DecompressionCapExceeded { .. }
        | ClientError::UnexpectedManifestType
        | ClientError::UnexpectedArtifactType { .. }
        | ClientError::WrongLayerCount { .. }
        | ClientError::UnexpectedLayerMediaType { .. }
        | ClientError::LayerSizeExceeded { .. }
        | ClientError::Serialization(_)
        | ClientError::InvalidEncoding(_)
        | ClientError::ReferrersUnsupported { .. }
        | ClientError::Internal(_) => ClientFailure::Other,
    }
}

/// Regression tests for [`classify`].
///
/// Each `ClientError` variant must map to the expected [`ClientFailure`]
/// bucket. This test is the enforcement gate: if a new variant is added to
/// `ClientError` without updating `classify`, the exhaustive match above will
/// produce a compile error here.
#[cfg(test)]
mod classify_tests {
    use super::*;
    use crate::oci::client::error::ClientError;

    fn assert_transient(client: ClientError) {
        assert!(
            matches!(classify(&client), ClientFailure::Transient),
            "expected Transient for {client:?}"
        );
    }

    fn assert_auth(client: ClientError) {
        assert!(
            matches!(classify(&client), ClientFailure::Auth),
            "expected Auth for {client:?}"
        );
    }

    fn assert_not_found(client: ClientError) {
        assert!(
            matches!(classify(&client), ClientFailure::NotFound),
            "expected NotFound for {client:?}"
        );
    }

    fn assert_other(client: ClientError) {
        assert!(
            matches!(classify(&client), ClientFailure::Other),
            "expected Other for {client:?}"
        );
    }

    #[test]
    fn registry_is_transient() {
        assert_transient(ClientError::Registry(Box::new(std::io::Error::other("net"))));
    }

    #[test]
    fn io_is_transient() {
        assert_transient(ClientError::Io {
            path: std::path::PathBuf::from("/tmp/test"),
            source: std::io::Error::other("disk"),
        });
    }

    #[test]
    fn authentication_is_auth() {
        assert_auth(ClientError::Authentication(Box::new(std::io::Error::other("401"))));
    }

    #[test]
    fn manifest_not_found_is_not_found() {
        assert_not_found(ClientError::ManifestNotFound("reg/repo:tag".to_string()));
    }

    #[test]
    fn repository_not_found_is_not_found() {
        assert_not_found(ClientError::RepositoryNotFound("reg/repo".to_string()));
    }

    #[test]
    fn blob_not_found_is_not_found() {
        use crate::oci::{Digest, Identifier, PinnedIdentifier};
        let id = Identifier::parse("registry.test/repo:tag").expect("valid");
        let pinned =
            PinnedIdentifier::try_from(id.clone_with_digest(Digest::Sha256("a".repeat(64)))).expect("valid pinned");
        assert_not_found(ClientError::BlobNotFound(pinned));
    }

    #[test]
    fn invalid_manifest_is_other() {
        assert_other(ClientError::InvalidManifest("bad manifest".to_string()));
    }

    #[test]
    fn digest_mismatch_is_other() {
        assert_other(ClientError::DigestMismatch {
            expected: "sha256:aaa".to_string(),
            actual: "sha256:bbb".to_string(),
        });
    }

    #[test]
    fn decompression_cap_exceeded_is_other() {
        assert_other(ClientError::DecompressionCapExceeded { cap: 256 << 20 });
    }

    #[test]
    fn unexpected_manifest_type_is_other() {
        assert_other(ClientError::UnexpectedManifestType);
    }

    #[test]
    fn serialization_is_other() {
        let json_err = serde_json::from_str::<serde_json::Value>("!!!").unwrap_err();
        assert_other(ClientError::Serialization(json_err));
    }

    #[test]
    fn invalid_encoding_is_other() {
        let encoding_err = String::from_utf8(vec![0xFF]).unwrap_err();
        assert_other(ClientError::InvalidEncoding(encoding_err));
    }

    #[test]
    fn internal_is_other() {
        assert_other(ClientError::Internal(Box::new(std::io::Error::other("internal"))));
    }
}

/// Extract the inner [`ClientError`] from `err` (when present) so the
/// new [`ProjectErrorKind`] variant can carry it as a `source` field.
/// Falls back to wrapping the whole `err` when no `ClientError` is in
/// the chain — the chain is preserved either way via `#[source]`.
fn project_err_from_client(
    err: crate::Error,
    identifier: Identifier,
    ctor: impl FnOnce(Identifier, Box<dyn std::error::Error + Send + Sync>) -> ProjectErrorKind,
) -> super::Error {
    // `crate::Error::OciClient(ClientError)` is the common shape. Pull the
    // inner `ClientError` out verbatim so downstream code can `downcast`
    // without walking past a `crate::Error` wrapper.
    let source: Box<dyn std::error::Error + Send + Sync> = match err {
        crate::Error::OciClient(client) => Box::new(client),
        other => Box::new(other),
    };
    ProjectError::new(PathBuf::new(), ctor(identifier, source)).into()
}

/// Assemble a [`ProjectLock`] from resolved tools and the source config.
fn build_lock(tools: Vec<LockedTool>, config: &ProjectConfig) -> ProjectLock {
    // ISO-8601 UTC with second resolution. `ProjectLock::save` will
    // preserve this value when the resolved content equals `previous`.
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    ProjectLock {
        metadata: LockMetadata {
            lock_version: LockVersion::V3,
            declaration_hash_version: DECLARATION_HASH_VERSION,
            declaration_hash: config.declaration_hash_cached().to_string(),
            generated_by: format!("ocx {}", env!("CARGO_PKG_VERSION")),
            generated_at: now,
        },
        tools,
    }
}

#[cfg(test)]
mod tests {
    //! Specification tests for [`resolve_lock`].
    //!
    //! Every test below traces to one design-record bullet:
    //!
    //! | # | Spec bullet                                         | Test                                                          |
    //! |---|-----------------------------------------------------|---------------------------------------------------------------|
    //! | 1 | happy path                                          | [`resolve_lock_happy_path_records_one_leaf_per_child`]        |
    //! | 2 | retry-then-success                                  | [`resolve_lock_retry_then_success`]                           |
    //! | 3 | retry-exhausted → Unavailable                       | [`resolve_lock_retry_exhausted_classifies_as_unavailable`]    |
    //! | 4 | auth failure → AuthError without retry              | [`resolve_lock_auth_failure_no_retry_classifies_as_auth_error`] |
    //! | 5 | 404 → NotFound                                      | [`resolve_lock_404_classifies_as_not_found`]                  |
    //! | 6 | timeout → Unavailable                               | [`resolve_lock_timeout_classifies_as_unavailable`]            |
    //! | 7 | `--group` flag parsing (comma-split, empty segment) | [`group_flag_parsing_and_validation`] (tombstone — Python E2E) |
    //! | 8 | `.gitattributes` detection helper (4 sub-cases)     | [`gitattributes_detection_helper`] (tombstone — see note)     |
    //!
    //! # Error-classification assertion strategy
    //!
    //! Error classification is asserted via two stable properties rather than
    //! by matching a specific `ProjectErrorKind` variant, so the tests stay
    //! robust across error-type refactors:
    //!
    //! 1. Walk the `err.source()` chain and downcast to `ClientError` — the
    //!    source cause is preserved regardless of the outer variant.
    //! 2. Cross-check `ClassifyExitCode` — the exit code is the stable
    //!    surface contract a script depends on.

    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::super::declaration_hash;
    use super::*;
    use crate::oci::client::error::ClientError;
    use crate::oci::index::IndexImpl;
    use crate::oci::{Digest, Identifier, ImageIndex, ImageIndexEntry, ImageManifest, Manifest};

    // ── Test fixtures ───────────────────────────────────────────────────────

    const TEST_REGISTRY: &str = "registry.test";
    const TOOL_NAME: &str = "cmake";
    const TOOL_REPO: &str = "cmake";
    const TOOL_TAG: &str = "3.28";

    /// Canonical `registry/repo:tag` identifier string used across tests.
    #[allow(dead_code, reason = "Phase 3 — kept available for multi-tool tests Phase 5 may add")]
    fn identifier_string() -> String {
        format!("{TEST_REGISTRY}/{TOOL_REPO}:{TOOL_TAG}")
    }

    /// Fully-qualified cmake identifier for the fake registry.
    #[allow(dead_code, reason = "Phase 3 — kept available for multi-tool tests Phase 5 may add")]
    fn tool_identifier() -> Identifier {
        Identifier::parse(&identifier_string()).expect("valid test identifier")
    }

    /// Stable test digest — 64 lowercase hex characters. Not a real SHA-256
    /// of anything; simply a well-formed digest for test equality checks.
    fn test_digest() -> Digest {
        Digest::Sha256("a".repeat(64))
    }

    /// A second stable test digest distinct from [`test_digest`].
    #[allow(dead_code, reason = "kept available for multi-tool tests Phase 5 may add")]
    fn test_digest_b() -> Digest {
        Digest::Sha256("b".repeat(64))
    }

    /// Build a flat single-image manifest. `fetch_candidates` maps this to a
    /// single `"any"` leaf entry (the flat `Manifest::Image` → `"any"` path).
    fn flat_image_manifest() -> Manifest {
        Manifest::Image(ImageManifest::default())
    }

    /// Build a multi-platform image index advertising the given
    /// `(child-digest-string, optional-platform-string)` children. `None`
    /// platform → an `"any"` child.
    fn image_index_manifest(children: &[(String, Option<&str>)]) -> Manifest {
        let manifests = children
            .iter()
            .map(|(digest, plat)| ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: digest.clone(),
                size: 1,
                platform: plat.map(|p| {
                    let ocx: Platform = p.parse().expect("valid platform string");
                    crate::oci::native::Platform::from(&ocx)
                }),
                artifact_type: None,
                annotations: None,
            })
            .collect();
        Manifest::ImageIndex(ImageIndex {
            schema_version: 2,
            media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
            manifests,
            artifact_type: None,
            annotations: None,
        })
    }

    /// Build a minimal single-tool [`ProjectConfig`] whose `[tools]` table
    /// contains exactly one entry (`cmake`) bound to `registry.test/cmake:3.28`.
    fn single_tool_config() -> ProjectConfig {
        let toml = format!(
            r#"[tools]
{TOOL_NAME} = "{registry}/{TOOL_REPO}:{TOOL_TAG}"
"#,
            registry = TEST_REGISTRY
        );
        ProjectConfig::from_toml_str(&toml).expect("valid single-tool config")
    }

    /// One scripted response in a `MockIndex` queue.
    type ScriptedResponse = crate::Result<Option<Digest>>;

    /// Deterministic mock `IndexImpl` with a FIFO script of responses.
    ///
    /// Each call to `fetch_manifest_digest` pops the next scripted
    /// response. When the script is empty, the mock panics — a missing
    /// response indicates a test bug (either the wrong retry count or an
    /// unexpected extra call).
    ///
    /// `call_count` lets tests assert the exact number of retries the
    /// resolver performed. `delay` (optional per response) lets us
    /// simulate a hang for the timeout test.
    struct MockIndex {
        script: Arc<Mutex<Vec<ScriptedResponse>>>,
        call_count: Arc<Mutex<usize>>,
        per_call_delay: Option<Duration>,
    }

    /// Script + counter + optional delay tuple shared between a
    /// [`MockIndex`] and its [`SharedMock`] clones.
    type MockHandles = (Arc<Mutex<Vec<ScriptedResponse>>>, Arc<Mutex<usize>>, Option<Duration>);

    impl MockIndex {
        fn with_script(script: Vec<ScriptedResponse>) -> Self {
            Self {
                // Stored in reverse so `pop()` returns items in insertion order.
                script: Arc::new(Mutex::new(script.into_iter().rev().collect())),
                call_count: Arc::new(Mutex::new(0)),
                per_call_delay: None,
            }
        }

        fn with_delay(delay: Duration, script: Vec<ScriptedResponse>) -> Self {
            let mut me = Self::with_script(script);
            me.per_call_delay = Some(delay);
            me
        }

        #[allow(
            dead_code,
            reason = "reserved for future tests that assert via the mock directly rather than the shared counter handle"
        )]
        fn call_count(&self) -> usize {
            *self.call_count.lock().unwrap()
        }

        fn handles(&self) -> MockHandles {
            (self.script.clone(), self.call_count.clone(), self.per_call_delay)
        }
    }

    /// Factory for fresh `MockIndex` instances that share counters across
    /// `box_clone()` — essential because the production `Index` holds a
    /// `Box<dyn IndexImpl>` and the resolver may box-clone during retries.
    struct SharedMock {
        script: Arc<Mutex<Vec<ScriptedResponse>>>,
        call_count: Arc<Mutex<usize>>,
        per_call_delay: Option<Duration>,
    }

    impl SharedMock {
        fn from(m: &MockIndex) -> Self {
            let (script, call_count, per_call_delay) = m.handles();
            Self {
                script,
                call_count,
                per_call_delay,
            }
        }
    }

    #[async_trait]
    impl IndexImpl for MockIndex {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(None)
        }

        async fn fetch_manifest(
            &self,
            _: &Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }

        async fn fetch_manifest_digest(&self, _: &Identifier, _op: IndexOperation) -> crate::Result<Option<Digest>> {
            if let Some(delay) = self.per_call_delay {
                tokio::time::sleep(delay).await;
            }
            *self.call_count.lock().unwrap() += 1;
            let next = self.script.lock().unwrap().pop();
            match next {
                Some(response) => response,
                None => panic!("MockIndex script exhausted — unexpected extra call to fetch_manifest_digest"),
            }
        }

        async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(MockIndex {
                script: self.script.clone(),
                call_count: self.call_count.clone(),
                per_call_delay: self.per_call_delay,
            })
        }
    }

    #[async_trait]
    impl IndexImpl for SharedMock {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            _: &Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(&self, _: &Identifier, _op: IndexOperation) -> crate::Result<Option<Digest>> {
            if let Some(delay) = self.per_call_delay {
                tokio::time::sleep(delay).await;
            }
            *self.call_count.lock().unwrap() += 1;
            let next = self.script.lock().unwrap().pop();
            match next {
                Some(response) => response,
                None => panic!("SharedMock script exhausted — unexpected extra call"),
            }
        }
        async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(SharedMock {
                script: self.script.clone(),
                call_count: self.call_count.clone(),
                per_call_delay: self.per_call_delay,
            })
        }
    }

    /// Wrap a `MockIndex` inside an `Index` so it can be passed as
    /// `&Index` to `resolve_lock`.
    fn index_from_mock(mock: &MockIndex) -> (Index, Arc<Mutex<usize>>) {
        let shared = SharedMock::from(mock);
        let counter = shared.call_count.clone();
        (Index::from_impl(shared), counter)
    }

    /// A mock that serves BOTH the digest-addressed retry path
    /// (`fetch_manifest_digest`, consumed by `retry_fetch`) and the
    /// candidate fan-out (`fetch_manifest`, consumed by `fetch_candidates`)
    /// from a single scripted manifest. Used by the V2 resolve specs, which
    /// need `resolve_one` to reach `build_platforms_map`.
    ///
    /// `box_clone` shares the counters and the manifest via `Arc` so the
    /// production `Index`'s `Box<dyn IndexImpl>` cloning does not lose state.
    #[derive(Clone)]
    struct ManifestMock {
        /// The head digest returned by `fetch_manifest_digest` (the index
        /// digest the resolver must DISCARD) and as the `fetch_manifest` head.
        head: Digest,
        /// The manifest `fetch_candidates` fans out. `None` → tag not found.
        manifest: Arc<Option<Manifest>>,
        digest_calls: Arc<Mutex<usize>>,
        manifest_calls: Arc<Mutex<usize>>,
        /// Number of leading `fetch_manifest_digest` calls that return a
        /// transient `ClientError::Registry` before succeeding — drives the
        /// retry-chain spec.
        transient_digest_failures: Arc<Mutex<usize>>,
    }

    impl ManifestMock {
        fn new(head: Digest, manifest: Option<Manifest>) -> Self {
            Self {
                head,
                manifest: Arc::new(manifest),
                digest_calls: Arc::new(Mutex::new(0)),
                manifest_calls: Arc::new(Mutex::new(0)),
                transient_digest_failures: Arc::new(Mutex::new(0)),
            }
        }

        /// Configure the first `n` `fetch_manifest_digest` calls to fail with
        /// a transient registry error before succeeding.
        fn with_transient_digest_failures(self, n: usize) -> Self {
            *self.transient_digest_failures.lock().unwrap() = n;
            self
        }

        fn digest_call_count(&self) -> usize {
            *self.digest_calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl IndexImpl for ManifestMock {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            _: &Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<(Digest, Manifest)>> {
            *self.manifest_calls.lock().unwrap() += 1;
            Ok(self.manifest.as_ref().clone().map(|m| (self.head.clone(), m)))
        }
        async fn fetch_manifest_digest(&self, _: &Identifier, _op: IndexOperation) -> crate::Result<Option<Digest>> {
            *self.digest_calls.lock().unwrap() += 1;
            {
                let mut remaining = self.transient_digest_failures.lock().unwrap();
                if *remaining > 0 {
                    *remaining -= 1;
                    return Err(ClientError::Registry(Box::new(std::io::Error::other("transient 503"))).into());
                }
            }
            Ok(self.manifest.as_ref().as_ref().map(|_| self.head.clone()))
        }
        async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// Extract a [`LockedTool`]'s `(repository, platforms)` pair.
    fn resolved_platforms(tool: &LockedTool) -> (&Identifier, &BTreeMap<String, Digest>) {
        (&tool.repository, &tool.platforms)
    }

    /// Default test options: short timeout, 2 retries, minimal backoff.
    ///
    /// Production defaults (30s timeout, 1s backoff) would make retry
    /// tests run for ~3 seconds each. Tests override to keep wall-clock
    /// under a second while still exercising the retry arithmetic.
    fn fast_options() -> ResolveLockOptions {
        ResolveLockOptions {
            per_tool_timeout: Duration::from_secs(2),
            retry_attempts: 2,
            initial_backoff: Duration::from_millis(1),
        }
    }

    // ── 1. Happy path ───────────────────────────────────────────────────────

    /// Plan Testing Strategy (`resolve_one` via `fetch_candidates`): records
    /// one leaf per advertised child, keyed by the child's lossless platform
    /// key; the outer index digest is DISCARDED, not stored; no omitted-
    /// platform rows are synthesized.
    ///
    /// A two-platform index (`linux/amd64`, `darwin/arm64`) → a `PerPlatform`
    /// entry with a bare `repository` and exactly those two leaf keys.
    #[tokio::test]
    async fn resolve_lock_happy_path_records_one_leaf_per_child() {
        let config = single_tool_config();
        // The outer index digest must be DISCARDED — use a byte distinct from
        // every child leaf so the discard assertion below is meaningful.
        let index_digest = Digest::Sha256("f".repeat(64));
        let amd64_leaf = "a".repeat(64);
        let arm64_leaf = "b".repeat(64);
        let manifest = image_index_manifest(&[
            (format!("sha256:{amd64_leaf}"), Some("linux/amd64")),
            (format!("sha256:{arm64_leaf}"), Some("darwin/arm64")),
        ]);
        let mock = ManifestMock::new(index_digest.clone(), Some(manifest));
        let index = Index::from_impl(mock);

        let lock = resolve_lock(&config, &index, &[], fast_options())
            .await
            .expect("happy-path resolve must succeed");

        assert_eq!(lock.tools.len(), 1, "exactly one tool should be locked");
        let tool = &lock.tools[0];
        assert_eq!(tool.name, TOOL_NAME, "binding name preserved");
        assert_eq!(tool.group, "default", "[tools] maps to the reserved 'default' group");

        let (repository, platforms) = resolved_platforms(tool);
        assert_eq!(
            repository.registry(),
            TEST_REGISTRY,
            "bare repository keeps the registry"
        );
        assert_eq!(repository.repository(), TOOL_REPO, "bare repository keeps the repo");
        assert!(repository.tag().is_none(), "repository must be bare (no tag)");
        assert!(repository.digest().is_none(), "repository must be bare (no digest)");

        assert_eq!(platforms.len(), 2, "one leaf per advertised child; got {platforms:?}");
        assert_eq!(
            platforms.get("linux/amd64"),
            Some(&Digest::Sha256(amd64_leaf)),
            "linux/amd64 leaf recorded under its lossless key"
        );
        assert_eq!(
            platforms.get("darwin/arm64"),
            Some(&Digest::Sha256(arm64_leaf)),
            "darwin/arm64 leaf recorded under its lossless key"
        );
        assert!(
            !platforms.values().any(|d| *d == index_digest),
            "the outer index digest must be DISCARDED, never stored as a leaf"
        );
    }

    /// Plan Testing Strategy: a flat `Manifest::Image` (non-index) package
    /// locks a single `"any"` entry — the host→`"any"` fallback path.
    #[tokio::test]
    async fn resolve_lock_flat_image_records_single_any_leaf() {
        let config = single_tool_config();
        let index_digest = test_digest();
        let mock = ManifestMock::new(index_digest, Some(flat_image_manifest()));
        let index = Index::from_impl(mock);

        let lock = resolve_lock(&config, &index, &[], fast_options())
            .await
            .expect("flat-image resolve must succeed");

        assert_eq!(lock.tools.len(), 1);
        let (_repository, platforms) = resolved_platforms(&lock.tools[0]);
        assert_eq!(platforms.len(), 1, "flat image locks exactly one entry");
        assert!(
            platforms.contains_key("any"),
            "flat Manifest::Image must lock a single `any` leaf; got {platforms:?}"
        );
    }

    // ── 2. Retry then success ───────────────────────────────────────────────

    /// Plan line 629: "retry-then-success: mock returns `Registry` error
    /// once, then succeeds → lock written."
    ///
    /// The digest-addressed retry chain fails once transiently then
    /// succeeds; the resolver retries and then fans out the leaf map.
    /// Exactly two `fetch_manifest_digest` calls recorded, and a V2 leaf
    /// is locked.
    #[tokio::test]
    async fn resolve_lock_retry_then_success() {
        let config = single_tool_config();
        let leaf = "a".repeat(64);
        let manifest = image_index_manifest(&[(format!("sha256:{leaf}"), Some("linux/amd64"))]);
        let mock = ManifestMock::new(test_digest(), Some(manifest)).with_transient_digest_failures(1);
        let probe = mock.clone();
        let index = Index::from_impl(mock);

        let lock = resolve_lock(&config, &index, &[], fast_options())
            .await
            .expect("resolver must recover on second attempt");

        assert_eq!(lock.tools.len(), 1);
        let (_repository, platforms) = resolved_platforms(&lock.tools[0]);
        assert_eq!(
            platforms.get("linux/amd64"),
            Some(&Digest::Sha256(leaf)),
            "the leaf must be locked after the retry recovers"
        );
        assert_eq!(
            probe.digest_call_count(),
            2,
            "one initial attempt + one retry expected; got {}",
            probe.digest_call_count()
        );
    }

    // ── 3. Retry exhausted classifies as Unavailable ───────────────────────

    /// Plan line 630: "retry-exhausted: mock always returns `Registry`
    /// error → returns `Unavailable`-classified error."
    ///
    /// Per F3 this asserts via the source chain (`ClientError::Registry`
    /// downcast) and exit-code classification (`ExitCode::Unavailable`
    /// via `ClassifyExitCode`). No assumption is made about eventual
    /// Phase 5 `ProjectErrorKind` variants.
    #[tokio::test]
    async fn resolve_lock_retry_exhausted_classifies_as_unavailable() {
        let config = single_tool_config();
        // Script enough failures for initial attempt + `retry_attempts` retries.
        // 1 (initial) + 2 (retry_attempts) = 3 total attempts.
        let script = (0..3)
            .map(|_| Err(ClientError::Registry(Box::new(std::io::Error::other("always down"))).into()))
            .collect();
        let mock = MockIndex::with_script(script);
        let (index, counter) = index_from_mock(&mock);

        let err = resolve_lock(&config, &index, &[], fast_options())
            .await
            .expect_err("retry exhaustion must surface as an error");

        assert_eq!(
            *counter.lock().unwrap(),
            3,
            "exactly 3 attempts (1 initial + 2 retries) expected; got {}",
            *counter.lock().unwrap()
        );

        // Assert error classification by walking the source chain and
        // confirming a `ClientError::Registry` is present. Avoids
        // depending on Phase 5 `ProjectErrorKind::RegistryUnreachable`.
        assert_chain_contains_client_error_registry(&err);

        // Cross-check via exit-code classification (stable post-Phase 5).
        let code = <super::super::Error as crate::cli::ClassifyExitCode>::classify(&err);
        assert_eq!(
            code,
            Some(crate::cli::ExitCode::Unavailable),
            "retry-exhausted registry error must classify as Unavailable (exit 69); got {:?}",
            code
        );
    }

    // ── 4. Auth failure — no retry ──────────────────────────────────────────

    /// Plan line 631: "auth failure: mock returns `Authentication` →
    /// returns `AuthError`-classified error without retry (auth is not
    /// transient)."
    ///
    /// Call count MUST be 1 (no retries on auth). Classification via
    /// exit-code mapping + source-chain downcast.
    #[tokio::test]
    async fn resolve_lock_auth_failure_no_retry_classifies_as_auth_error() {
        let config = single_tool_config();
        let script = vec![Err(ClientError::Authentication(Box::new(std::io::Error::other(
            "401 unauthorized",
        )))
        .into())];
        let mock = MockIndex::with_script(script);
        let (index, counter) = index_from_mock(&mock);

        let err = resolve_lock(&config, &index, &[], fast_options())
            .await
            .expect_err("auth failure must surface as an error");

        assert_eq!(
            *counter.lock().unwrap(),
            1,
            "auth failures must NOT retry; expected exactly 1 call, got {}",
            *counter.lock().unwrap()
        );

        let code = <super::super::Error as crate::cli::ClassifyExitCode>::classify(&err);
        assert_eq!(
            code,
            Some(crate::cli::ExitCode::AuthError),
            "auth failure must classify as AuthError (exit 80); got {:?}",
            code
        );
    }

    // ── 5. 404 classifies as NotFound ───────────────────────────────────────

    /// Plan line 632: "404: mock returns `Ok(None)` → returns
    /// `NotFound`-classified error."
    ///
    /// `Ok(None)` at the index layer means "tag not found" per the
    /// `IndexImpl` contract (absence is Option::None, not Err). The
    /// resolver must classify this as a hard failure → NotFound (exit 79).
    #[tokio::test]
    async fn resolve_lock_404_classifies_as_not_found() {
        let config = single_tool_config();
        let mock = MockIndex::with_script(vec![Ok(None)]);
        let (index, counter) = index_from_mock(&mock);

        let err = resolve_lock(&config, &index, &[], fast_options())
            .await
            .expect_err("tag-not-found must surface as an error");

        assert_eq!(
            *counter.lock().unwrap(),
            1,
            "not-found must NOT retry; expected exactly 1 call, got {}",
            *counter.lock().unwrap()
        );

        let code = <super::super::Error as crate::cli::ClassifyExitCode>::classify(&err);
        assert_eq!(
            code,
            Some(crate::cli::ExitCode::NotFound),
            "tag-not-found must classify as NotFound (exit 79); got {:?}",
            code
        );
    }

    // ── 6. Timeout classifies as Unavailable ────────────────────────────────

    /// Plan line 633: "timeout: mock hangs past `per_tool_timeout` →
    /// returns `Unavailable`-classified error."
    ///
    /// Timeout path: per-call delay exceeds `per_tool_timeout`. The
    /// `tokio::time::timeout` wrapper around the retry chain must fire
    /// and map to Unavailable classification (exit 69).
    #[tokio::test]
    async fn resolve_lock_timeout_classifies_as_unavailable() {
        let config = single_tool_config();
        // Mock hangs 500ms per call; timeout is 50ms, so the first
        // attempt can never complete.
        // `crate::Error` isn't Clone, so build the script element-by-element
        // rather than using the `vec![expr; N]` Clone-based macro form.
        let script: Vec<ScriptedResponse> = (0..4).map(|_| Ok(Some(test_digest()))).collect();
        let mock = MockIndex::with_delay(Duration::from_millis(500), script);
        let (index, _counter) = index_from_mock(&mock);

        let options = ResolveLockOptions {
            per_tool_timeout: Duration::from_millis(50),
            retry_attempts: 0,
            initial_backoff: Duration::from_millis(1),
        };

        let start = std::time::Instant::now();
        let err = resolve_lock(&config, &index, &[], options)
            .await
            .expect_err("hanging registry must time out");
        let elapsed = start.elapsed();

        // Sanity check: completed due to timeout, not because of the full
        // 500ms delay.
        assert!(
            elapsed < Duration::from_millis(400),
            "timeout did not fire before the mock delay would have completed; elapsed={elapsed:?}"
        );

        let code = <super::super::Error as crate::cli::ClassifyExitCode>::classify(&err);
        assert_eq!(
            code,
            Some(crate::cli::ExitCode::Unavailable),
            "timeout must classify as Unavailable (exit 69); got {:?}",
            code
        );
    }

    // ── 7. `--group` parsing ────────────────────────────────────────────────

    /// Plan line 634: "`--group` parsing: flattens comma lists; rejects
    /// empty segments; dedup duplicates."
    ///
    /// TOMBSTONE — this test is DEFERRED to `ocx_cli`.
    ///
    /// The clap `Parser` derive for `Lock.groups` lives in
    /// `crates/ocx_cli/src/command/lock.rs`. `ocx_lib` is a pure library
    /// crate and does not depend on `clap` directly; adding it just for a
    /// test shim would pull a large derive-macro crate into the library's
    /// test binary and diverge the shim from the real command's
    /// attributes.
    ///
    /// The two observable behaviors this bullet covers are instead
    /// asserted end-to-end through the CLI boundary in the Python
    /// acceptance tests:
    ///
    /// - Flatten + comma split (`-g ci,lint -g release` works): implicit
    ///   in `test_lock_group_filter_only_includes_selected_group`.
    /// - Empty segment rejection (`-g ci,,lint` → exit 64): the dedicated
    ///   `test_lock_empty_group_segment_exits_64`.
    /// - Duplicate dedup: implicit in the filter test (duplicate `-g ci`
    ///   does not double-emit).
    ///
    /// Once Phase 3 lands a runtime-validator free function in
    /// `project::resolve` (e.g. `normalize_groups(&[String]) -> Result<..>`),
    /// add a unit test for the three sub-cases here. Until then, this
    /// tombstone marks the spec bullet.
    #[test]
    #[ignore = "clap derive lives in ocx_cli; parse-layer coverage via Python acceptance tests"]
    fn group_flag_parsing_and_validation() {
        panic!("deferred — see docstring");
    }

    // ── 8. `.gitattributes` detection helper ────────────────────────────────

    /// Plan line 635: "`.gitattributes` detection helper: present, absent,
    /// file missing, irrelevant content."
    ///
    /// TOMBSTONE — this test is DEFERRED to Phase 5 Implement.
    ///
    /// The design record (plan line 592, step 9) describes the behavior
    /// but does not commit to a free-function signature for this helper.
    /// The Phase 3 stubs do not expose one. Writing the test against an
    /// invented signature would drive an accidental API; writing it
    /// against the unexported implementation would require relaxing
    /// visibility purely for test access.
    ///
    /// The **acceptance test**
    /// `test_lock_gitattributes_note_emitted_when_line_missing` (Python)
    /// covers the end-to-end behavior through the CLI boundary, which is
    /// the behavior the user actually sees. Once Phase 5 Implement lands
    /// a stable helper signature, a corresponding unit test for each of
    /// the four sub-cases (present / absent / missing file / irrelevant
    /// content) should be added here.
    ///
    /// Intentionally `#[ignore]` — tests are expected to FAIL against
    /// stubs (the plan requires exactly that), but a deliberate tombstone
    /// must not pollute the fail signal for the other 7 tests.
    #[test]
    #[ignore = "Phase 5 will expose a stable helper signature; acceptance coverage in Python"]
    fn gitattributes_detection_helper() {
        // Placeholder body — the real test will assert all four sub-cases
        // described in plan line 635.
        panic!("deferred — see docstring");
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    /// Walk `err.source()` chain and assert that at least one cause
    /// downcasts to [`ClientError::Registry`]. Used in place of matching
    /// on eventual Phase 5 `ProjectErrorKind::RegistryUnreachable`.
    fn assert_chain_contains_client_error_registry(err: &super::super::Error) {
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(err);
        while let Some(c) = cause {
            if let Some(client) = c.downcast_ref::<ClientError>() {
                assert!(
                    matches!(client, ClientError::Registry(_)),
                    "expected ClientError::Registry, got {:?}",
                    client
                );
                return;
            }
            cause = c.source();
        }
        panic!("error chain does not contain a ClientError::Registry cause; chain: {err:#}");
    }

    // ── #155 frozen / offline policy block is terminal ──────────────────────

    /// A no-resolve policy block from the index layer
    /// (`PolicyResolutionBlocked`) is terminal in `retry_fetch`: it returns
    /// immediately (no retry, no backoff), routes to
    /// `ProjectErrorKind::PolicyBlocked` carrying the policy label, and
    /// classifies as PolicyBlocked (81). Exercised for both `"offline"` and
    /// `"frozen"` so the single check covers both policies.
    #[tokio::test]
    async fn resolve_lock_policy_block_is_terminal_and_classifies_policy_blocked() {
        for policy in ["offline", "frozen"] {
            let config = single_tool_config();
            let index_err = crate::oci::index::error::Error::PolicyResolutionBlocked {
                identifier: format!("{TEST_REGISTRY}/{TOOL_REPO}:{TOOL_TAG}"),
                policy,
            };
            let mock = MockIndex::with_script(vec![Err(index_err.into())]);
            let (index, counter) = index_from_mock(&mock);

            let err = resolve_lock(&config, &index, &[], fast_options())
                .await
                .expect_err("policy block must surface as an error");

            assert_eq!(
                *counter.lock().unwrap(),
                1,
                "policy block must NOT retry (policy={policy}); expected exactly 1 call, got {}",
                *counter.lock().unwrap()
            );

            // Terminal routing → ProjectErrorKind::PolicyBlocked with the label.
            let mut found = false;
            let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&err);
            while let Some(c) = cause {
                if let Some(ProjectErrorKind::PolicyBlocked { policy: got, .. }) = c.downcast_ref::<ProjectErrorKind>()
                {
                    assert_eq!(
                        *got, policy,
                        "policy label must be carried through to the project error"
                    );
                    found = true;
                    break;
                }
                cause = c.source();
            }
            assert!(
                found,
                "error chain must contain ProjectErrorKind::PolicyBlocked (policy={policy}); chain: {err:#}"
            );

            let code = <super::super::Error as crate::cli::ClassifyExitCode>::classify(&err);
            assert_eq!(
                code,
                Some(crate::cli::ExitCode::PolicyBlocked),
                "policy block must classify as PolicyBlocked (exit 81); got {code:?}"
            );
        }
    }

    // ── resolve_lock_touched: whole-file pin-preserving mutator (spec §3.1) ──
    //
    // These specs prove the two-hash contract and zero-registry-contact for
    // untouched entries. The `PanicMock` index fake panics on EVERY method, so
    // any carry-forward path that reaches the registry fails the test loudly —
    // the structural proof of Codex R2 (untouched entries never re-resolve).

    /// An [`IndexImpl`] that panics on every call. Used to prove the
    /// carry-forward path makes **zero** registry contact for untouched V2
    /// entries: if any predecessor entry were re-resolved, a panic surfaces.
    #[derive(Clone)]
    struct PanicMock;

    #[async_trait]
    impl IndexImpl for PanicMock {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            panic!("PanicMock: list_repositories called — untouched carry-forward must not contact the registry");
        }
        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            panic!("PanicMock: list_tags called — untouched carry-forward must not contact the registry");
        }
        async fn fetch_manifest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<(Digest, Manifest)>> {
            panic!("PanicMock: fetch_manifest called — untouched carry-forward must not contact the registry");
        }
        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<Digest>> {
            panic!("PanicMock: fetch_manifest_digest called — untouched carry-forward must not contact the registry");
        }
        async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            panic!("PanicMock: fetch_blob called — untouched carry-forward must not contact the registry");
        }
        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// Build a three-tool `[tools]` config whose bindings resolve to
    /// `registry.test/<name>:1`.
    fn three_tool_config() -> ProjectConfig {
        let toml = format!(
            r#"[tools]
alpha = "{r}/alpha:1"
beta = "{r}/beta:1"
gamma = "{r}/gamma:1"
"#,
            r = TEST_REGISTRY
        );
        ProjectConfig::from_toml_str(&toml).expect("three-tool config parses")
    }

    /// A single `LockedTool` pinning `default/<name>` to one `linux/amd64`
    /// leaf digest, so carried-forward equality is observable per entry.
    fn locked_tool_fixture(name: &str, leaf: &str) -> LockedTool {
        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64".to_string(), Digest::Sha256(leaf.repeat(64)));
        LockedTool {
            name: name.to_string(),
            group: "default".to_string(),
            repository: Identifier::new_registry(name, TEST_REGISTRY),
            platforms,
        }
    }

    /// Build a predecessor lock stamping `config`'s declaration hash and
    /// carrying `tools` verbatim.
    fn predecessor_lock(config: &ProjectConfig, tools: Vec<LockedTool>) -> ProjectLock {
        ProjectLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V3,
                declaration_hash_version: DECLARATION_HASH_VERSION,
                declaration_hash: config.declaration_hash_cached().to_string(),
                generated_by: "ocx test".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            tools,
        }
    }

    /// Look up a locked tool by `(group, name)`.
    fn locked<'a>(lock: &'a ProjectLock, group: &str, name: &str) -> &'a LockedTool {
        lock.tools
            .iter()
            .find(|t| t.group == group && t.name == name)
            .unwrap_or_else(|| panic!("missing locked tool {group}/{name} in {:?}", lock.tools))
    }

    /// Spec §8.1 — touched = {one} of 3 V2 entries: only that entry is
    /// re-resolved; the other two are carried byte-identical; and the stamped
    /// metadata hash is `hash(candidate)`, NOT `hash(pre_mutation)`.
    ///
    /// Here pre_mutation == candidate (the touched binding already existed at
    /// the same value), so the freshness gate passes and we can resolve `beta`
    /// fresh while `alpha`/`gamma` carry through the `PanicMock` untouched.
    #[tokio::test]
    async fn resolve_lock_touched_resolves_only_touched_carries_rest_v2() {
        let config = three_tool_config();
        let pre_mutation = three_tool_config();
        let alpha = locked_tool_fixture("alpha", "a");
        let beta_old = locked_tool_fixture("beta", "b");
        let gamma = locked_tool_fixture("gamma", "g");
        let previous = predecessor_lock(&config, vec![alpha.clone(), beta_old.clone(), gamma.clone()]);

        // Only `beta` is touched; its fresh resolve returns a NEW leaf so the
        // change is observable. `alpha`/`gamma` must reach the PanicMock zero
        // times — any contact panics.
        let beta_new_leaf = "c".repeat(64);
        let manifest = image_index_manifest(&[(format!("sha256:{beta_new_leaf}"), Some("linux/amd64"))]);
        let index = Index::from_impl(ManifestMock::new(test_digest(), Some(manifest)));

        let touched = vec![("default".to_string(), "beta".to_string())];
        let lock = resolve_lock_touched(&config, &pre_mutation, &previous, &index, &touched, fast_options())
            .await
            .expect("touched resolve must succeed when pre-mutation hash matches");

        // alpha + gamma carried BYTE-IDENTICAL (full LockedTool equality).
        assert_eq!(
            locked(&lock, "default", "alpha"),
            &alpha,
            "untouched alpha carried verbatim"
        );
        assert_eq!(
            locked(&lock, "default", "gamma"),
            &gamma,
            "untouched gamma carried verbatim"
        );

        // beta re-resolved to the new leaf — NOT the old "b" pin.
        let beta = locked(&lock, "default", "beta");
        let (_repo, platforms) = resolved_platforms(beta);
        assert_eq!(
            platforms.get("linux/amd64"),
            Some(&Digest::Sha256(beta_new_leaf)),
            "touched beta must carry the freshly resolved leaf"
        );

        // Two-hash contract: the stamped metadata hash is hash(candidate),
        // explicitly NOT the (equal here, but conceptually distinct) value —
        // assert it equals declaration_hash(&candidate).
        assert_eq!(
            lock.metadata.declaration_hash,
            declaration_hash(&config),
            "stamped metadata hash must be hash(candidate)"
        );
    }

    /// Spec §8.1 — remove's case: touched = ∅, every survivor carried verbatim,
    /// the `PanicMock` never fires (zero registry contact proven structurally).
    #[tokio::test]
    async fn resolve_lock_touched_empty_touched_carries_everything() {
        let config = three_tool_config();
        let pre_mutation = three_tool_config();
        let alpha = locked_tool_fixture("alpha", "a");
        let beta = locked_tool_fixture("beta", "b");
        let gamma = locked_tool_fixture("gamma", "g");
        let previous = predecessor_lock(&config, vec![alpha.clone(), beta.clone(), gamma.clone()]);

        // PanicMock: any resolve attempt panics — proves empty-touched resolves
        // nothing and carries every survivor verbatim.
        let index = Index::from_impl(PanicMock);

        let lock = resolve_lock_touched(&config, &pre_mutation, &previous, &index, &[], fast_options())
            .await
            .expect("empty-touched resolve must carry everything with zero registry contact");

        assert_eq!(lock.tools.len(), 3, "all three survivors carried");
        assert_eq!(locked(&lock, "default", "alpha"), &alpha);
        assert_eq!(locked(&lock, "default", "beta"), &beta);
        assert_eq!(locked(&lock, "default", "gamma"), &gamma);
        assert_eq!(
            lock.metadata.declaration_hash,
            declaration_hash(&config),
            "stamped metadata hash must be hash(candidate)"
        );
    }

    /// Spec §8.1 — `add` on a clean lock: candidate has a new binding the
    /// pre-mutation snapshot lacks; pre_mutation hash matches the predecessor;
    /// only the new binding is resolved; the produced metadata hash is
    /// hash(candidate). Proves no universal add-failure and the commit-gate
    /// (which compares against hash(candidate)) will pass.
    #[tokio::test]
    async fn resolve_lock_touched_add_on_clean_lock_succeeds() {
        // pre_mutation: one tool. candidate: pre_mutation + a new `delta`.
        let pre_mutation = single_tool_config();
        let candidate = {
            let toml = format!(
                r#"[tools]
{TOOL_NAME} = "{r}/{TOOL_REPO}:{TOOL_TAG}"
delta = "{r}/delta:1"
"#,
                r = TEST_REGISTRY
            );
            ProjectConfig::from_toml_str(&toml).expect("candidate config parses")
        };

        // Predecessor lock is current with the PRE-mutation config (one tool),
        // carrying a `cmake` entry. The freshness gate anchors on
        // hash(pre_mutation) == previous hash, so a clean add passes.
        //
        // The predecessor's `cmake` repo must match the declared repository so
        // it carries verbatim through the PanicMock; reuse locked_tool_fixture's
        // shape but with the right repo coordinates.
        let cmake = LockedTool {
            repository: Identifier::new_registry(TOOL_REPO, TEST_REGISTRY),
            platforms: {
                let mut p = BTreeMap::new();
                p.insert("linux/amd64".to_string(), Digest::Sha256("a".repeat(64)));
                p
            },
            ..locked_tool_fixture(TOOL_NAME, "a")
        };
        let previous = predecessor_lock(&pre_mutation, vec![cmake.clone()]);

        // Only `delta` is resolved fresh; `cmake` carries through untouched.
        let delta_leaf = "d".repeat(64);
        let manifest = image_index_manifest(&[(format!("sha256:{delta_leaf}"), Some("linux/amd64"))]);
        let index = Index::from_impl(ManifestMock::new(test_digest(), Some(manifest)));

        let touched = vec![("default".to_string(), "delta".to_string())];
        let lock = resolve_lock_touched(&candidate, &pre_mutation, &previous, &index, &touched, fast_options())
            .await
            .expect("a clean add must NOT fail closed (two-hash contract)");

        // cmake carried verbatim; delta resolved fresh.
        assert_eq!(
            locked(&lock, "default", TOOL_NAME),
            &cmake,
            "existing cmake carried verbatim"
        );
        let delta = locked(&lock, "default", "delta");
        let (_repo, platforms) = resolved_platforms(delta);
        assert_eq!(platforms.get("linux/amd64"), Some(&Digest::Sha256(delta_leaf)));

        // Stamped hash is hash(candidate) — the commit-time coherence gate
        // compares against exactly this value, so it must NOT be
        // hash(pre_mutation).
        assert_eq!(
            lock.metadata.declaration_hash,
            declaration_hash(&candidate),
            "stamped metadata hash must be hash(candidate), proving the commit gate passes"
        );
        assert_ne!(
            lock.metadata.declaration_hash,
            declaration_hash(&pre_mutation),
            "stamped hash must NOT be hash(pre_mutation) — the binding was inserted"
        );
    }

    /// Spec §8.1 — a drifted pre-mutation hash fails `StaleLockOnPartial`
    /// BEFORE any resolve (exit 65); the PanicMock proves the gate short-
    /// circuits ahead of registry contact.
    #[tokio::test]
    async fn resolve_lock_touched_rejects_drifted_pre_mutation_hash() {
        let config = three_tool_config();
        // pre_mutation drifted: a DIFFERENT config whose hash will not match
        // the predecessor stamped from `config`.
        let pre_mutation = single_tool_config();
        let previous = predecessor_lock(&config, vec![locked_tool_fixture("alpha", "a")]);

        // PanicMock: if the gate did not short-circuit, the resolve would panic.
        let index = Index::from_impl(PanicMock);

        let touched = vec![("default".to_string(), "beta".to_string())];
        let err = resolve_lock_touched(&config, &pre_mutation, &previous, &index, &touched, fast_options())
            .await
            .expect_err("a drifted pre-mutation hash must be rejected before any resolve");

        let mut found = false;
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&err);
        while let Some(c) = cause {
            if let Some(ProjectErrorKind::StaleLockOnPartial {
                previous_hash,
                current_hash,
            }) = c.downcast_ref::<ProjectErrorKind>()
            {
                assert_eq!(
                    previous_hash,
                    &config.declaration_hash_cached().to_string(),
                    "previous_hash is the predecessor lock's hash"
                );
                assert_eq!(
                    current_hash,
                    &declaration_hash(&pre_mutation),
                    "current_hash is hash(pre_mutation), the freshness anchor"
                );
                found = true;
                break;
            }
            cause = c.source();
        }
        assert!(found, "error chain must contain StaleLockOnPartial; chain: {err:#}");

        let code = <super::super::Error as crate::cli::ClassifyExitCode>::classify(&err);
        assert_eq!(
            code,
            Some(crate::cli::ExitCode::DataError),
            "StaleLockOnPartial must classify as DataError (exit 65); got {code:?}"
        );
    }

    /// Spec §8.1 — `merge_carry_forward` (exercised via `resolve_lock_touched`
    /// with empty touched): a predecessor `(group, name)` absent from
    /// `candidate` is NOT carried (the remove path). `beta` is removed from the
    /// candidate config, so its survivor entry must fall out of the new lock.
    #[tokio::test]
    async fn merge_carry_forward_drops_undeclared_binding() {
        // candidate: alpha + gamma only (beta removed). pre_mutation: all 3.
        let pre_mutation = three_tool_config();
        let candidate = {
            let toml = format!(
                r#"[tools]
alpha = "{r}/alpha:1"
gamma = "{r}/gamma:1"
"#,
                r = TEST_REGISTRY
            );
            ProjectConfig::from_toml_str(&toml).expect("two-tool candidate parses")
        };
        // Predecessor is current with pre_mutation (all 3 declared).
        let previous = predecessor_lock(
            &pre_mutation,
            vec![
                locked_tool_fixture("alpha", "a"),
                locked_tool_fixture("beta", "b"),
                locked_tool_fixture("gamma", "g"),
            ],
        );

        // PanicMock: empty touched ⇒ nothing resolved; survivors carried.
        let index = Index::from_impl(PanicMock);

        let lock = resolve_lock_touched(&candidate, &pre_mutation, &previous, &index, &[], fast_options())
            .await
            .expect("remove path must carry survivors and drop the undeclared binding");

        assert_eq!(lock.tools.len(), 2, "beta must be dropped; only alpha + gamma survive");
        assert!(
            lock.tools.iter().all(|t| t.name != "beta"),
            "the removed binding must NOT be carried forward; got {:?}",
            lock.tools
        );
        assert_eq!(locked(&lock, "default", "alpha"), &locked_tool_fixture("alpha", "a"));
        assert_eq!(locked(&lock, "default", "gamma"), &locked_tool_fixture("gamma", "g"));
        assert_eq!(
            lock.metadata.declaration_hash,
            declaration_hash(&candidate),
            "stamped metadata hash must be hash(candidate) (beta absent)"
        );
    }

    // ── Block 2: structural coherence between predecessor and candidate ──────
    //
    // The two-hash freshness gate proves the predecessor described the same
    // CONFIG (declaration_hash is over `ocx.toml`, not the lock entries). It
    // does NOT prove the lock's `[[tool]]` entries are coherent: a corrupt or
    // hand-edited lock with the right metadata hash but a missing / extra /
    // wrong-repository entry would otherwise pass the gate, then be laundered by
    // the carry-forward under a freshly stamped candidate hash. These specs pin
    // the structural-coherence gate (StaleLockOnPartial/65) and prove it fires
    // BEFORE any registry contact (PanicMock).

    /// Spec — Block 2(a): a predecessor missing a declared (untouched) binding
    /// must fail `StaleLockOnPartial` BEFORE any resolve. The candidate declares
    /// three tools but the predecessor lock only carries two; `gamma` is
    /// uncovered (not touched, not carried), so the carry-forward would silently
    /// drop a declared binding — refuse instead.
    #[tokio::test]
    async fn resolve_lock_touched_rejects_predecessor_missing_declared_binding() {
        let config = three_tool_config();
        let pre_mutation = three_tool_config();
        // Predecessor is missing `gamma` despite carrying the matching config
        // declaration_hash — a corrupt/hand-edited lock.
        let previous = predecessor_lock(
            &config,
            vec![locked_tool_fixture("alpha", "a"), locked_tool_fixture("beta", "b")],
        );

        // PanicMock: the coherence gate must short-circuit before any resolve.
        let index = Index::from_impl(PanicMock);

        // touched = ∅ (e.g. a remove of an unrelated binding) leaves `gamma` in
        // the would-be carry set, but it is absent from the predecessor.
        let err = resolve_lock_touched(&config, &pre_mutation, &previous, &index, &[], fast_options())
            .await
            .expect_err("a predecessor missing a declared binding must be rejected before any resolve");

        assert!(
            chain_contains_stale_lock_on_partial(&err),
            "error chain must contain StaleLockOnPartial; chain: {err:#}"
        );
        let code = <super::super::Error as crate::cli::ClassifyExitCode>::classify(&err);
        assert_eq!(
            code,
            Some(crate::cli::ExitCode::DataError),
            "missing-declared-binding must classify as DataError (exit 65); got {code:?}"
        );
    }

    /// Spec — Block 2(b): a carried entry whose bare `repository` differs from
    /// the candidate's declared identifier for that `(group, name)` must fail
    /// `StaleLockOnPartial` BEFORE any resolve. The lock pins `beta` to a
    /// different repository than `ocx.toml` declares — the lock no longer
    /// describes the config, so carrying it forward would launder a wrong pin.
    #[tokio::test]
    async fn resolve_lock_touched_rejects_carried_repository_divergence() {
        let config = three_tool_config();
        let pre_mutation = three_tool_config();
        // `beta`'s carried entry points at a DIFFERENT repository than the
        // candidate declares (`registry.test/beta`). A corrupt/hand-edited lock.
        let mut beta_wrong = locked_tool_fixture("beta", "b");
        beta_wrong.repository = Identifier::new_registry("wrong-repo", TEST_REGISTRY);
        let previous = predecessor_lock(
            &config,
            vec![
                locked_tool_fixture("alpha", "a"),
                beta_wrong,
                locked_tool_fixture("gamma", "g"),
            ],
        );

        // PanicMock: the coherence gate must short-circuit before any resolve.
        let index = Index::from_impl(PanicMock);

        let err = resolve_lock_touched(&config, &pre_mutation, &previous, &index, &[], fast_options())
            .await
            .expect_err("a carried entry whose repository diverges from the declaration must be rejected");

        assert!(
            chain_contains_stale_lock_on_partial(&err),
            "error chain must contain StaleLockOnPartial; chain: {err:#}"
        );
        let code = <super::super::Error as crate::cli::ClassifyExitCode>::classify(&err);
        assert_eq!(
            code,
            Some(crate::cli::ExitCode::DataError),
            "carried-repository divergence must classify as DataError (exit 65); got {code:?}"
        );
    }

    /// Spec — Block 2: a predecessor carrying a DUPLICATE `(group, name)` entry
    /// must fail `StaleLockOnPartial` BEFORE any resolve. Each declared binding
    /// must be covered by exactly one carried entry; two entries for the same
    /// key is a corrupt lock.
    #[tokio::test]
    async fn resolve_lock_touched_rejects_duplicate_carried_entry() {
        let config = three_tool_config();
        let pre_mutation = three_tool_config();
        // Two `beta` entries — a duplicate the carry-forward must not launder.
        let previous = predecessor_lock(
            &config,
            vec![
                locked_tool_fixture("alpha", "a"),
                locked_tool_fixture("beta", "b"),
                locked_tool_fixture("beta", "c"),
                locked_tool_fixture("gamma", "g"),
            ],
        );

        let index = Index::from_impl(PanicMock);

        let err = resolve_lock_touched(&config, &pre_mutation, &previous, &index, &[], fast_options())
            .await
            .expect_err("a duplicate carried entry must be rejected before any resolve");

        assert!(
            chain_contains_stale_lock_on_partial(&err),
            "error chain must contain StaleLockOnPartial; chain: {err:#}"
        );
    }

    /// Walk `err`'s source chain and report whether it carries a
    /// [`ProjectErrorKind::StaleLockOnPartial`].
    fn chain_contains_stale_lock_on_partial(err: &super::super::Error) -> bool {
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(err);
        while let Some(c) = cause {
            if matches!(
                c.downcast_ref::<ProjectErrorKind>(),
                Some(ProjectErrorKind::StaleLockOnPartial { .. })
            ) {
                return true;
            }
            cause = c.source();
        }
        false
    }

    // ── Block 4: touched binding not declared in candidate ──────────────────

    /// Spec — Block 4: a touched `(group, name)` absent from `candidate` must
    /// fail `ToolNotInConfig` (exit 79) rather than being silently dropped. The
    /// `PanicMock` proves the rejection fires before any registry contact.
    #[tokio::test]
    async fn resolve_lock_touched_rejects_undeclared_touched_binding() {
        let config = three_tool_config();
        let pre_mutation = three_tool_config();
        let previous = predecessor_lock(
            &config,
            vec![
                locked_tool_fixture("alpha", "a"),
                locked_tool_fixture("beta", "b"),
                locked_tool_fixture("gamma", "g"),
            ],
        );

        let index = Index::from_impl(PanicMock);

        // `nonexistent` is not declared in the candidate config.
        let touched = vec![("default".to_string(), "nonexistent".to_string())];
        let err = resolve_lock_touched(&config, &pre_mutation, &previous, &index, &touched, fast_options())
            .await
            .expect_err("a touched binding absent from candidate must be rejected, not dropped");

        let mut found = false;
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&err);
        while let Some(c) = cause {
            if let Some(ProjectErrorKind::ToolNotInConfig { name }) = c.downcast_ref::<ProjectErrorKind>() {
                assert_eq!(name, "nonexistent", "the undeclared tool is named in the error");
                found = true;
                break;
            }
            cause = c.source();
        }
        assert!(found, "error chain must contain ToolNotInConfig; chain: {err:#}");

        let code = <super::super::Error as crate::cli::ClassifyExitCode>::classify(&err);
        assert_eq!(
            code,
            Some(crate::cli::ExitCode::NotFound),
            "ToolNotInConfig must classify as NotFound (exit 79); got {code:?}"
        );
    }

    // ── Block 4b: duplicate touched pair resolves at most once ─────────────

    /// Regression — Block 4b: passing the SAME `(group, name)` twice in
    /// `touched` must produce exactly ONE lock entry for that binding, not a
    /// duplicate. The coherence gate already collapses predecessor duplicates;
    /// the work vector must too (lib-contract gap closed by the seen-set).
    #[tokio::test]
    async fn resolve_lock_touched_duplicate_touched_pair_resolves_exactly_once() {
        let config = three_tool_config();
        let pre_mutation = three_tool_config();
        let alpha = locked_tool_fixture("alpha", "a");
        let beta_old = locked_tool_fixture("beta", "b");
        let gamma = locked_tool_fixture("gamma", "g");
        let previous = predecessor_lock(&config, vec![alpha.clone(), beta_old, gamma.clone()]);

        // `beta` is listed TWICE in touched — the dedup must ensure it is
        // resolved only once, so the final lock carries exactly one `beta` entry.
        let beta_new_leaf = "c".repeat(64);
        let manifest = image_index_manifest(&[(format!("sha256:{beta_new_leaf}"), Some("linux/amd64"))]);
        let index = Index::from_impl(ManifestMock::new(test_digest(), Some(manifest)));

        let touched = vec![
            ("default".to_string(), "beta".to_string()),
            ("default".to_string(), "beta".to_string()),
        ];
        let lock = resolve_lock_touched(&config, &pre_mutation, &previous, &index, &touched, fast_options())
            .await
            .expect("duplicate touched pair must resolve successfully (deduped to one)");

        // The lock must contain exactly three entries (alpha, beta, gamma) with
        // no duplicate for beta.
        assert_eq!(
            lock.tools.len(),
            3,
            "duplicate touched entry must not produce an extra lock entry; got {:?}",
            lock.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
        let beta_entries: Vec<_> = lock.tools.iter().filter(|t| t.name == "beta").collect();
        assert_eq!(
            beta_entries.len(),
            1,
            "beta must appear exactly once in the lock, not duplicated; got {} entries",
            beta_entries.len()
        );
        // Verify the resolved leaf is the freshly-resolved one.
        let (_repo, platforms) = resolved_platforms(beta_entries[0]);
        assert_eq!(
            platforms.get("linux/amd64"),
            Some(&Digest::Sha256(beta_new_leaf)),
            "beta's lock entry must carry the freshly resolved leaf"
        );
    }

    // ── resolve_one: candidate fan-out edge cases ───────────────────────────

    /// Plan Testing Strategy (`resolve_one`): an unpinned-tag `Resolve` miss
    /// (the manifest is not found) maps to a `NotFound`-classified error —
    /// the resolver never produces an empty lock entry.
    ///
    /// Regression guard: this contract is already satisfied by the preserved
    /// `retry_fetch` → `TagNotFound` machinery (a `None` manifest short-
    /// circuits before the candidate fan-out), so this test passes against
    /// the stub. It pins the NotFound exit code so the V2 fan-out work cannot
    /// silently regress the miss path to a different code.
    #[tokio::test]
    async fn resolve_lock_candidate_miss_classifies_as_not_found() {
        let config = single_tool_config();
        // No manifest → fetch_manifest_digest returns Ok(None) → TagNotFound.
        let mock = ManifestMock::new(test_digest(), None);
        let index = Index::from_impl(mock);

        let err = resolve_lock(&config, &index, &[], fast_options())
            .await
            .expect_err("a candidate miss must surface as an error, not an empty lock");

        let code = <super::super::Error as crate::cli::ClassifyExitCode>::classify(&err);
        assert_eq!(
            code,
            Some(crate::cli::ExitCode::NotFound),
            "candidate miss must classify as NotFound (exit 79); got {code:?}"
        );
    }

    // ── build_platforms_map / guard_unique_key ──────────────────────────────

    /// `build_platforms_map` keys each advertised child's leaf digest by its
    /// lossless canonical [`Platform`] `Display` string. Two distinct
    /// platforms → two distinct keys; the index digest is never part of the
    /// map.
    #[test]
    fn build_platforms_map_keys_by_lossless_platform() {
        let amd64: Platform = "linux/amd64".parse().unwrap();
        let arm64: Platform = "darwin/arm64".parse().unwrap();
        let repo = Identifier::new_registry(TOOL_REPO, TEST_REGISTRY);
        let candidates = vec![
            (repo.clone_with_digest(Digest::Sha256("a".repeat(64))), amd64),
            (repo.clone_with_digest(Digest::Sha256("b".repeat(64))), arm64),
        ];

        let map = build_platforms_map(candidates).expect("two distinct platforms must build cleanly");
        assert_eq!(map.len(), 2, "one entry per advertised child");
        assert_eq!(map.get("linux/amd64"), Some(&Digest::Sha256("a".repeat(64))));
        assert_eq!(map.get("darwin/arm64"), Some(&Digest::Sha256("b".repeat(64))));
    }

    /// The runtime dup-key guard fails cleanly (never silently overwrites)
    /// when two advertised children stringify to the same key. A defensive
    /// assertion that should never fire under an injective key — exercised
    /// here by feeding two children with the same platform string.
    #[test]
    fn build_platforms_map_dup_key_fails_cleanly() {
        let amd64: Platform = "linux/amd64".parse().unwrap();
        let repo = Identifier::new_registry(TOOL_REPO, TEST_REGISTRY);
        let candidates = vec![
            (repo.clone_with_digest(Digest::Sha256("a".repeat(64))), amd64.clone()),
            (repo.clone_with_digest(Digest::Sha256("b".repeat(64))), amd64),
        ];

        let result = build_platforms_map(candidates);
        assert!(
            result.is_err(),
            "a duplicate platform key must fail cleanly, never silently overwrite"
        );
    }

    /// `guard_unique_key` inserts a fresh key and rejects a duplicate without
    /// overwriting the first value.
    #[test]
    fn guard_unique_key_rejects_duplicate_without_overwrite() {
        let mut map: BTreeMap<String, Digest> = BTreeMap::new();
        guard_unique_key(&mut map, "linux/amd64".to_string(), Digest::Sha256("a".repeat(64)))
            .expect("first insert succeeds");
        let dup = guard_unique_key(&mut map, "linux/amd64".to_string(), Digest::Sha256("b".repeat(64)));
        assert!(dup.is_err(), "second insert under the same key must fail");
        assert_eq!(
            map.get("linux/amd64"),
            Some(&Digest::Sha256("a".repeat(64))),
            "the original value must survive the rejected duplicate"
        );
    }

    // ── lookup_host_leaf ────────────────────────────────────────────────────

    /// Test helper: unwrap a `Selection::Found` to its digest, panicking with
    /// the actual `Selection` otherwise — keeps the assertions below terse
    /// now that `lookup_host_leaf` returns [`Selection`] (D1 F2) instead of
    /// `Option`.
    fn expect_found_digest(selection: Selection<(&Digest, &str)>) -> Digest {
        match selection {
            Selection::Found((digest, _key)) => digest.clone(),
            other => panic!("expected Selection::Found, got {other:?}"),
        }
    }

    /// Host key present → that leaf is returned directly.
    #[test]
    fn lookup_host_leaf_returns_host_key_when_present() {
        let host: Platform = "linux/amd64".parse().unwrap();
        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64".to_string(), Digest::Sha256("a".repeat(64)));
        platforms.insert("any".to_string(), Digest::Sha256("z".repeat(64)));

        let leaf = expect_found_digest(lookup_host_leaf(&platforms, &host));
        assert_eq!(leaf, Digest::Sha256("a".repeat(64)), "host key wins over `any`");
    }

    /// Host key absent but `"any"` present → fall back to `"any"` (the flat
    /// `Manifest::Image` host→`any` fallback; correctness gate).
    #[test]
    fn lookup_host_leaf_falls_back_to_any() {
        let host: Platform = "windows/amd64".parse().unwrap();
        let mut platforms = BTreeMap::new();
        platforms.insert("any".to_string(), Digest::Sha256("z".repeat(64)));

        let leaf = expect_found_digest(lookup_host_leaf(&platforms, &host));
        assert_eq!(
            leaf,
            Digest::Sha256("z".repeat(64)),
            "absent host key falls back to `any`"
        );
    }

    /// Host key absent AND no `"any"` → `Selection::None` (the caller raises
    /// a clean pre-network "no <host> leaf" error). `windows/amd64` required
    /// against a `linux/amd64` offer is not `is_compatible` (os mismatch) —
    /// no candidate survives.
    #[test]
    fn lookup_host_leaf_none_when_host_and_any_absent() {
        let host: Platform = "windows/amd64".parse().unwrap();
        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64".to_string(), Digest::Sha256("a".repeat(64)));

        assert_eq!(
            lookup_host_leaf(&platforms, &host),
            Selection::None,
            "neither the host key nor `any` is present → Selection::None"
        );
    }

    /// A bare (empty-`os_features`) entry runs on any feature-bearing host:
    /// `offered.os_features ⊆ required.os_features` holds trivially for an
    /// empty offered set, so a glibc host resolves a plain `linux/amd64`
    /// entry even though the entry declares no libc requirement.
    #[test]
    fn lookup_host_leaf_bare_entry_matches_feature_bearing_host() {
        let host: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64".to_string(), Digest::Sha256("a".repeat(64)));

        let leaf = expect_found_digest(lookup_host_leaf(&platforms, &host));
        assert_eq!(
            leaf,
            Digest::Sha256("a".repeat(64)),
            "a glibc host resolves the plain (empty-os_features) entry"
        );
    }

    /// A feature-matching entry outscores a co-present bare entry: both are
    /// `is_compatible`, but `compatibility_score` ranks the feature-count
    /// axis lexicographically, so the glibc-tagged entry wins over the bare
    /// entry.
    #[test]
    fn lookup_host_leaf_feature_specific_entry_outscores_bare_entry() {
        let host: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        let mut platforms = BTreeMap::new();
        platforms.insert(host.to_string(), Digest::Sha256("g".repeat(64)));
        platforms.insert("linux/amd64".to_string(), Digest::Sha256("a".repeat(64)));

        let leaf = expect_found_digest(lookup_host_leaf(&platforms, &host));
        assert_eq!(
            leaf,
            Digest::Sha256("g".repeat(64)),
            "the glibc-matching leaf wins over the plain bare entry"
        );
    }

    /// A dual-libc host against a map that carries only feature-specific keys
    /// (`libc.glibc` and `libc.musl`, no combined key, no plain base) is
    /// genuinely irreducible: both single-libc entries are individually
    /// `is_compatible` (subset of the host's two-feature set) and tie at the
    /// same score, so `lookup_host_leaf` returns `Selection::Ambiguous`
    /// carrying both tied candidates' original map keys — not a silent
    /// `None`, and not an arbitrary pick.
    #[test]
    fn lookup_host_leaf_dual_libc_ambiguous_between_separate_single_libc_entries() {
        let host: Platform = "linux/amd64+libc.glibc,libc.musl".parse().unwrap();
        let glibc: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        let musl: Platform = "linux/amd64+libc.musl".parse().unwrap();
        let mut platforms = BTreeMap::new();
        platforms.insert(glibc.to_string(), Digest::Sha256("g".repeat(64)));
        platforms.insert(musl.to_string(), Digest::Sha256("m".repeat(64)));

        let result = lookup_host_leaf(&platforms, &host);
        let Selection::Ambiguous(candidates) = result else {
            panic!("expected Selection::Ambiguous, got {result:?}");
        };
        let mut keys: Vec<&str> = candidates.iter().map(|(_, key)| *key).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["linux/amd64+libc.glibc", "linux/amd64+libc.musl"],
            "the ambiguous selection must name both tied original map keys"
        );
    }

    /// The same dual-libc host resolves its exact combined key when present.
    #[test]
    fn lookup_host_leaf_dual_libc_matches_exact_combined_key() {
        let host: Platform = "linux/amd64+libc.glibc,libc.musl".parse().unwrap();
        let mut platforms = BTreeMap::new();
        platforms.insert(host.to_string(), Digest::Sha256("d".repeat(64)));

        let leaf = expect_found_digest(lookup_host_leaf(&platforms, &host));
        assert_eq!(leaf, Digest::Sha256("d".repeat(64)), "exact dual-libc key wins");
    }

    /// D1 gap-closed regression: a dual-libc host resolves EITHER a
    /// glibc-only OR a musl-only lock entry when the map ships only ONE of
    /// them (no combined key). The old exact-key tier walk required a byte-
    /// exact match on the host's full multi-feature key and stripped ALL
    /// features for its fallback tier, so neither single-feature entry ever
    /// matched a dual-libc host — a silent `None` despite the entry being
    /// genuinely runnable (`offered.os_features ⊆ required.os_features`).
    /// `select_best`'s subset relation closes this by construction.
    #[test]
    fn lookup_host_leaf_dual_libc_host_resolves_single_libc_entry_gap_closed() {
        let host: Platform = "linux/amd64+libc.glibc,libc.musl".parse().unwrap();

        let mut glibc_only = BTreeMap::new();
        glibc_only.insert("linux/amd64+libc.glibc".to_string(), Digest::Sha256("g".repeat(64)));
        let leaf = expect_found_digest(lookup_host_leaf(&glibc_only, &host));
        assert_eq!(leaf, Digest::Sha256("g".repeat(64)));

        let mut musl_only = BTreeMap::new();
        musl_only.insert("linux/amd64+libc.musl".to_string(), Digest::Sha256("m".repeat(64)));
        let leaf = expect_found_digest(lookup_host_leaf(&musl_only, &host));
        assert_eq!(leaf, Digest::Sha256("m".repeat(64)));
    }
}
