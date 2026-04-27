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
//! default to the values specified in ADR Amendment D and plan Phase 3.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use tokio::task::JoinSet;

use super::error::{ProjectError, ProjectErrorKind};
use super::{LockMetadata, LockVersion, LockedTool, ProjectConfig, ProjectLock, declaration_hash};
use crate::oci::client::error::ClientError;
use crate::oci::index::{Index, IndexOperation};
use crate::oci::{Identifier, PinnedIdentifier};
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
    index: &Index,
    options: &ResolveLockOptions,
) -> Result<Vec<LockedTool>, super::Error> {
    if work.is_empty() {
        return Ok(Vec::new());
    }

    let mut set: JoinSet<Result<LockedTool, super::Error>> = JoinSet::new();
    for (group, name, identifier) in work {
        let index = index.clone();
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

/// Re-resolve a subset of tools against `index` and merge the result
/// into `previous`, preserving every entry the caller did not select.
///
/// Selection rules:
/// - When `bindings` is empty AND `groups` is empty, every tool in
///   `config` is re-resolved (equivalent to [`resolve_lock`] with no
///   group filter — but always uses the live [`Index`], not `previous`).
/// - When `groups` is non-empty, the selection narrows to tools whose
///   owning group is in `groups`. The reserved name `"default"`
///   selects the top-level `[tools]` table.
/// - When `bindings` is non-empty, the selection narrows further to
///   tools whose local binding name is in `bindings`. Unknown binding
///   names produce [`ProjectErrorKind::ToolNotInConfig`].
///
/// Untouched entries in `previous` are carried into the returned lock
/// verbatim (same `name`, `group`, `pinned`). The metadata header is
/// rebuilt from the current `config` (always reflects the live
/// `declaration_hash`); the `generated_at` field is the resolver's
/// timestamp at call time and only survives a [`ProjectLock::save`]
/// round-trip when the merged content matches `previous` byte-for-byte
/// via [`PinnedIdentifier::eq_content`].
///
/// Fully transactional: any resolution failure aborts before the merge,
/// returning an error with no `ProjectLock` produced.
///
/// # Errors
/// Returns [`super::Error`] for the same reasons as [`resolve_lock`],
/// plus [`ProjectErrorKind::ToolNotInConfig`] when a binding name
/// passed in `bindings` is not declared in `config`.
pub async fn resolve_lock_partial(
    config: &ProjectConfig,
    previous: &ProjectLock,
    index: &Index,
    bindings: &[String],
    groups: &[String],
    options: ResolveLockOptions,
) -> Result<ProjectLock, super::Error> {
    // Validate the group filter (defense-in-depth; the CLI layer
    // pre-validates with a UsageError exit). `normalize_groups` returns
    // `None` when `groups` is empty, which means "no group filter".
    let selected = normalize_groups(groups, config)?;

    // Validate every binding is declared somewhere in the config.
    // Cross-checks both the top-level `[tools]` table and every
    // `[group.*]` table — bindings are unique across selected groups
    // by construction, so a positive hit anywhere is sufficient.
    for name in bindings {
        let declared = config.tools.contains_key(name) || config.groups.values().any(|tools| tools.contains_key(name));
        if !declared {
            return Err(
                ProjectError::new(PathBuf::new(), ProjectErrorKind::ToolNotInConfig { name: name.clone() }).into(),
            );
        }
    }

    // Build the work set: start from the group-filtered universe, then
    // narrow to the named bindings when supplied.
    let mut work = collect_work(config, &selected);
    if !bindings.is_empty() {
        work.retain(|(_g, name, _id)| bindings.iter().any(|b| b == name));
    }

    let resolved = resolve_work(work, index, &options).await?;

    // Merge: carry forward every entry in `previous` whose `(group,
    // name)` was NOT just resolved, then append the freshly resolved
    // set. The block scope on `new_keys` ensures the borrow into
    // `resolved` ends before we move `resolved` into `tools` below.
    let mut tools: Vec<LockedTool> = {
        let new_keys: HashSet<(&str, &str)> = resolved.iter().map(|t| (t.group.as_str(), t.name.as_str())).collect();
        previous
            .tools
            .iter()
            .filter(|tool| !new_keys.contains(&(tool.group.as_str(), tool.name.as_str())))
            .cloned()
            .collect()
    };
    tools.extend(resolved);

    // Deterministic output: same `(group, name)` order as `resolve_lock`.
    tools.sort_by(|a, b| (a.group.as_str(), a.name.as_str()).cmp(&(b.group.as_str(), b.name.as_str())));

    Ok(build_lock(tools, config))
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

/// Resolve a single `(group, name, identifier)` to a [`LockedTool`],
/// wrapping the retry chain in `tokio::time::timeout`.
async fn resolve_one(
    index: Index,
    group: String,
    name: String,
    identifier: Identifier,
    options: ResolveLockOptions,
) -> Result<LockedTool, super::Error> {
    let timeout = options.per_tool_timeout;
    let retry_chain = retry_fetch(&index, identifier.clone(), options);

    let digest = match tokio::time::timeout(timeout, retry_chain).await {
        Ok(Ok(digest)) => digest,
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

    let pinned_id = identifier.clone_with_digest(digest);
    // `PinnedIdentifierError` cannot fire here — `clone_with_digest` above
    // unconditionally sets the digest — but guard with `.expect()` (not a
    // library-tier `.unwrap()`) so any future regression surfaces loudly
    // at this exact site rather than as a cryptic downstream panic. The
    // message names the invariant being relied upon.
    let pinned = PinnedIdentifier::try_from(pinned_id)
        .expect("clone_with_digest unconditionally sets the digest; PinnedIdentifier cannot fail here");

    Ok(LockedTool { name, group, pinned })
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
        // Transient: registry-side failures and file I/O are both retryable.
        ClientError::Registry(_) | ClientError::Io { .. } => ClientFailure::Transient,
        // Terminal auth failure — no retry.
        ClientError::Authentication(_) => ClientFailure::Auth,
        // 404-equivalent — no retry.
        ClientError::ManifestNotFound(_) | ClientError::BlobNotFound(_) => ClientFailure::NotFound,
        // Data errors and other structural failures — not retryable.
        ClientError::InvalidManifest(_)
        | ClientError::DigestMismatch { .. }
        | ClientError::UnexpectedManifestType
        | ClientError::Serialization(_)
        | ClientError::InvalidEncoding(_)
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
            lock_version: LockVersion::V1,
            declaration_hash_version: DECLARATION_HASH_VERSION,
            declaration_hash: declaration_hash(config),
            generated_by: format!("ocx {}", env!("CARGO_PKG_VERSION")),
            generated_at: now,
        },
        tools,
    }
}

#[cfg(test)]
mod tests {
    //! Specification tests for [`resolve_lock`] (plan Phase 3, lines 627–635).
    //!
    //! Every test below traces to one design-record bullet:
    //!
    //! | # | Spec bullet                                         | Test                                                          |
    //! |---|-----------------------------------------------------|---------------------------------------------------------------|
    //! | 1 | happy path                                          | [`resolve_lock_happy_path_single_tool`]                       |
    //! | 2 | retry-then-success                                  | [`resolve_lock_retry_then_success`]                           |
    //! | 3 | retry-exhausted → Unavailable                       | [`resolve_lock_retry_exhausted_classifies_as_unavailable`]    |
    //! | 4 | auth failure → AuthError without retry              | [`resolve_lock_auth_failure_no_retry_classifies_as_auth_error`] |
    //! | 5 | 404 → NotFound                                      | [`resolve_lock_404_classifies_as_not_found`]                  |
    //! | 6 | timeout → Unavailable                               | [`resolve_lock_timeout_classifies_as_unavailable`]            |
    //! | 7 | `--group` flag parsing (comma-split, empty segment) | [`group_flag_parsing_and_validation`] (tombstone — Python E2E) |
    //! | 8 | `.gitattributes` detection helper (4 sub-cases)     | [`gitattributes_detection_helper`] (tombstone — see note)     |
    //!
    //! All tests are expected to FAIL against the current stub (which
    //! calls `unimplemented!()`). The contract they encode is the Phase 3
    //! implementation target.
    //!
    //! # Finding F3 compliance
    //!
    //! `ProjectErrorKind` does NOT yet have `TagNotFound` / `AuthFailure` /
    //! `RegistryUnreachable` / `ResolveTimeout` variants — those are Phase 5
    //! additions. Error classification is therefore asserted **only** via
    //! one of two stable properties:
    //!
    //! 1. Walk `err.source()` chain and downcast to `ClientError` — present
    //!    today, will remain present post-Phase 5 as the source cause.
    //! 2. Assert `to_string()` pattern — surface representation stable
    //!    across refactors.
    //!
    //! We do NOT assert on specific `ProjectErrorKind` variants so these
    //! tests keep compiling when Phase 5 lands the new variants.

    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::oci::client::error::ClientError;
    use crate::oci::index::IndexImpl;
    use crate::oci::{Digest, Identifier, Manifest};

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
    #[allow(dead_code, reason = "Phase 3 — kept available for multi-tool tests Phase 5 may add")]
    fn test_digest_b() -> Digest {
        Digest::Sha256("b".repeat(64))
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

    /// Plan line 628: "`resolve_lock` happy path with a mock `Index` that
    /// returns a known digest."
    ///
    /// One tool, mock returns `Ok(Some(digest))` on first call → resolves
    /// to a single-tool `ProjectLock` whose entry carries the expected
    /// digest.
    #[tokio::test]
    async fn resolve_lock_happy_path_single_tool() {
        let config = single_tool_config();
        let digest = test_digest();
        let mock = MockIndex::with_script(vec![Ok(Some(digest.clone()))]);
        let (index, counter) = index_from_mock(&mock);

        let lock = resolve_lock(&config, &index, &[], fast_options())
            .await
            .expect("happy-path resolve must succeed");

        assert_eq!(lock.tools.len(), 1, "exactly one tool should be locked");
        let tool = &lock.tools[0];
        assert_eq!(tool.name, TOOL_NAME, "binding name preserved");
        assert_eq!(tool.group, "default", "[tools] maps to the reserved 'default' group");
        assert_eq!(
            tool.pinned.digest(),
            digest,
            "locked digest must equal the one the mock returned"
        );
        assert_eq!(
            *counter.lock().unwrap(),
            1,
            "happy path must fetch exactly once (no retries)"
        );
    }

    // ── 2. Retry then success ───────────────────────────────────────────────

    /// Plan line 629: "retry-then-success: mock returns `Registry` error
    /// once, then succeeds → lock written."
    ///
    /// Mock fails first with a transient `ClientError::Registry`, then
    /// succeeds with a digest. Resolver must retry and succeed. Exactly
    /// two calls recorded.
    #[tokio::test]
    async fn resolve_lock_retry_then_success() {
        let config = single_tool_config();
        let digest = test_digest();
        let script = vec![
            Err(ClientError::Registry(Box::new(std::io::Error::other("transient 503"))).into()),
            Ok(Some(digest.clone())),
        ];
        let mock = MockIndex::with_script(script);
        let (index, counter) = index_from_mock(&mock);

        let lock = resolve_lock(&config, &index, &[], fast_options())
            .await
            .expect("resolver must recover on second attempt");

        assert_eq!(lock.tools.len(), 1);
        assert_eq!(lock.tools[0].pinned.digest(), digest);
        assert_eq!(*counter.lock().unwrap(), 2, "one initial attempt + one retry expected");
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
}
