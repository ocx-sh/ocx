// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Tool-set composition for `ocx exec`.
//!
//! Composes the set of tools to fetch and use as environment for `ocx exec`
//! from a mix of `--group` selections (resolved via the digest-pinned
//! `ocx.lock`) and explicit positional packages (resolved via the index,
//! tag-style, like the rest of `ocx`).
//!
//! Pure composition — no I/O. Lock loading and staleness checks are done by
//! the CLI layer; this module accepts the loaded data as input.

use crate::oci::identifier::error::IdentifierErrorKind;
use crate::oci::{Identifier, Platform, Selection};
use crate::project::config::ProjectConfig;
use crate::project::lock::{LockedTool, ProjectLock, locked_tool_content_equal};

use super::error::{ProjectError, ProjectErrorKind};

/// Provenance of a resolved tool entry — drives error messages and logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Came from a `--group <name>` selection via the lock.
    Group(String),
    /// Came from an explicit positional package on the command line.
    Explicit,
}

/// One tool resolved by [`compose_tool_set`].
///
/// `binding` is the local name (TOML key from `ocx.toml` for group entries,
/// inferred or explicit for positionals). `identifier` is the OCI identifier
/// to pull (digest-pinned for group entries, tag-style for positionals).
#[derive(Debug, Clone)]
pub struct ResolvedTool {
    pub binding: String,
    pub identifier: Identifier,
    pub origin: Origin,
}

/// The unresolved source backing a [`SelectedTool`].
///
/// Selection ([`select_tool_set`]) records *where* a binding came from without
/// touching the host platform; [`resolve_selected_tools`] later maps each
/// source to a concrete pull [`Identifier`]. Splitting selection from
/// resolution lets a caller narrow the set to a requested NAME subset *before*
/// resolving host leaves, so an unrelated sibling that ships no leaf for the
/// host never aborts a narrowly-named run.
#[derive(Debug, Clone)]
pub enum ToolSource {
    /// A group entry from the lock. The host leaf is resolved later, only if
    /// the entry survives name filtering.
    Locked(LockedTool),
    /// A positional `name=identifier` package — already a concrete tag-style
    /// identifier, resolved verbatim.
    Explicit(Identifier),
}

/// One tool selected by [`select_tool_set`], before host-leaf resolution.
///
/// `binding` is the local name; `origin` records provenance for diagnostics
/// and logging; `source` carries the unresolved backing (a lock entry or an
/// explicit identifier). [`resolve_selected_tools`] turns this into a
/// [`ResolvedTool`].
#[derive(Debug, Clone)]
pub struct SelectedTool {
    pub binding: String,
    pub origin: Origin,
    pub source: ToolSource,
}

/// One positional package parsed from the command line.
///
/// The `binding` is either explicit (`name=identifier` form) or inferred from
/// the identifier's repository basename via [`Identifier::name`].
#[derive(Debug, Clone)]
pub struct PositionalPackage {
    pub binding: String,
    pub identifier: Identifier,
}

/// Parse a positional package argument of the form `[name=]identifier`.
///
/// When the `name=` prefix is present, that name is the explicit binding.
/// Otherwise, the binding is inferred from the identifier's repository
/// basename (`Identifier::name`), e.g. `cmake:3.29` → binding `cmake`,
/// `ghcr.io/acme/foo:1` → binding `foo`.
///
/// Identifier parsing uses [`Identifier::parse_with_default_registry`] so
/// short forms like `cmake:3.28` resolve against the configured default
/// registry, matching the rest of the `ocx` CLI.
pub fn parse_positional(input: &str, default_registry: &str) -> Result<PositionalPackage, super::Error> {
    // Detect a `name=identifier` prefix. The split is on the *first* `=` —
    // identifier values don't contain `=` themselves (digests use `@`, tags
    // use `:`), so this is unambiguous.
    let (explicit_binding, ident_str) = match input.split_once('=') {
        Some((name, rest)) if is_valid_binding(name) => (Some(name.to_string()), rest),
        _ => (None, input),
    };

    let identifier =
        Identifier::parse_with_default_registry(ident_str, default_registry).map_err(|e| -> super::Error {
            let kind = match e.kind {
                IdentifierErrorKind::MissingRegistry => ProjectErrorKind::ToolValueMissingRegistry {
                    name: explicit_binding.clone().unwrap_or_else(|| ident_str.to_string()),
                    value: ident_str.to_string(),
                },
                _ => ProjectErrorKind::ToolValueInvalid {
                    name: explicit_binding.clone().unwrap_or_else(|| ident_str.to_string()),
                    value: ident_str.to_string(),
                    source: e,
                },
            };
            super::Error::Project(ProjectError::new(std::path::PathBuf::new(), kind))
        })?;

    let binding = match explicit_binding {
        Some(b) => b,
        None => identifier.name().to_string(),
    };

    Ok(PositionalPackage { binding, identifier })
}

/// Expand the reserved scope keyword `all` to the union of the default group
/// ([`DEFAULT_GROUP`]) and every named group declared in `config.groups`,
/// preserving the position of `all` in the input. Non-`all` entries pass
/// through unchanged. Pure: no I/O, no policy beyond the literal-string match.
///
/// Currently consumed by the `ocx run` command. The helper is exposed at the
/// lib layer so other project-tier CLIs (`pull`, `lock`, `update`) can adopt
/// the same `-g all` expansion without duplicating the keyword logic, and so
/// programmatic consumers can call it directly before invoking
/// [`compose_tool_set`].
///
/// # Order
///
/// Each occurrence of `all` is replaced *in place* by
/// `[DEFAULT_GROUP, *config.groups.keys()]` where named groups are in
/// alphabetical order (since `BTreeMap::keys` is alphabetical).
/// [`compose_tool_set`] then deduplicates the resulting list at the group
/// iteration step.
///
/// # Empty input
///
/// `expand_all_keyword(&[], _)` returns `vec![]`. The caller (CLI Phase C.2)
/// is responsible for promoting an empty post-expansion list to the default
/// scope (`vec![DEFAULT_GROUP.into()]`); this helper does not inject default
/// scope on empty input.
///
/// # Example
///
/// Input groups `[ci, all, release]` with config declaring groups `{ci, docs,
/// release}` becomes `[ci, default, ci, docs, release, release]`; the
/// `compose_tool_set` dedup step collapses repeats.
pub fn expand_all_keyword(groups: &[String], config: &ProjectConfig) -> Vec<String> {
    if groups.is_empty() {
        return Vec::new();
    }
    let all_keyword = super::internal::ALL_GROUP;
    let default_group = super::internal::DEFAULT_GROUP;
    let mut out = Vec::with_capacity(groups.len());
    for entry in groups {
        if entry == all_keyword {
            // Expand `all` in place: DEFAULT_GROUP first, then every named group
            // in alphabetical order (BTreeMap::keys is alphabetical).
            out.push(default_group.to_owned());
            for named in config.groups.keys() {
                out.push(named.clone());
            }
        } else {
            out.push(entry.clone());
        }
    }
    out
}

/// Select the final tool set from groups and positionals — *without* resolving
/// host leaves.
///
/// Pure function — no I/O, no host-platform lookup. This is the **selection**
/// half of [`compose_tool_set`]: it builds the binding set, performs
/// whole-scope duplicate-across-groups validation, and applies positional
/// overrides, but leaves every group entry as an unresolved
/// [`ToolSource::Locked`]. A caller that needs only a subset (e.g.
/// `ocx run NAME`) can filter the result and then resolve just the survivors
/// via [`resolve_selected_tools`], so a sibling that ships no host leaf for the
/// current platform never aborts the run.
///
/// Pipeline:
///
/// 1. Build the initial set from `groups` × `lock.tools` (deduplicating group
///    names, preserving first-seen order; within a group, lock entry order is
///    preserved).
/// 2. Duplicate-across-groups check, performed at the **lock level**: the same
///    binding in two selected groups with identical [`LockedTool`]
///    content (via [`locked_tool_content_equal`]) collapses to the first-seen
///    entry; differing content errors with
///    [`ProjectErrorKind::DuplicateToolAcrossSelectedGroups`]. This validation
///    is whole-scope and fires regardless of any later name filter. Because it
///    compares resolutions — not resolved host leaves — an unnamed sibling with
///    no host leaf is never resolved here and so cannot error at selection time.
/// 3. Apply positional overrides: right-most wins; a matching binding replaces
///    the entry (origin becomes [`Origin::Explicit`]), a non-matching one adds
///    a fresh entry. Positionals also dedup among themselves with
///    right-most-wins.
///
/// `config` is currently unused beyond signature parity.
pub fn select_tool_set(
    _config: &ProjectConfig,
    lock: Option<&ProjectLock>,
    groups: &[String],
    positionals: &[PositionalPackage],
) -> Result<Vec<SelectedTool>, super::Error> {
    let mut selected: Vec<SelectedTool> = Vec::new();
    // Parallel `binding -> selected[i]` index that mirrors `selected`. Replaces
    // the previous O(G·T·R) `iter().find` chain inside the inner loop with an
    // O(1) `HashMap` probe. Every push to `selected` must be paired with an
    // insert; every override (positional `right-most wins`) keeps the same
    // index so the map never goes stale.
    let mut binding_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    // Step 1+2: build initial set from groups, in user-specified group order
    // (deduplicated). Within a group, lock entry order is preserved.
    let mut seen_groups: Vec<&str> = Vec::with_capacity(groups.len());
    for raw in groups {
        if seen_groups.contains(&raw.as_str()) {
            continue;
        }
        seen_groups.push(raw.as_str());

        let Some(lock_ref) = lock else {
            // No lock available but a group was selected. The CLI layer is
            // responsible for surfacing `LockMissing` (exit 78) before
            // calling compose; reaching here means a broken contract.
            return Err(super::Error::Project(ProjectError::new(
                std::path::PathBuf::new(),
                ProjectErrorKind::LockMissing,
            )));
        };

        for entry in &lock_ref.tools {
            if entry.group != *raw {
                continue;
            }
            // Step 2: duplicate-across-groups check at the lock level. Identical
            // resolution content in another selected group → collapse silently;
            // different content → error. No host-leaf resolution happens here,
            // so an entry with no host leaf for the current platform never
            // errors at selection time.
            if let Some(&idx) = binding_index.get(&entry.name) {
                let existing = &selected[idx];
                let from_other_group = matches!(&existing.origin, Origin::Group(g) if g != raw);
                if from_other_group {
                    let ToolSource::Locked(existing_tool) = &existing.source else {
                        unreachable!("a Group-origin selection always carries a Locked source");
                    };
                    let same_content = locked_tool_content_equal(existing_tool, entry);
                    if !same_content {
                        let other_group = match &existing.origin {
                            Origin::Group(g) => g.clone(),
                            Origin::Explicit => unreachable!("from_other_group implies Group origin"),
                        };
                        return Err(super::Error::Project(ProjectError::new(
                            std::path::PathBuf::new(),
                            ProjectErrorKind::DuplicateToolAcrossSelectedGroups {
                                name: entry.name.clone(),
                                group_a: other_group,
                                group_b: raw.clone(),
                            },
                        )));
                    }
                    // Same content in another selected group — silently keep the first.
                    continue;
                }
            }
            let idx = selected.len();
            selected.push(SelectedTool {
                binding: entry.name.clone(),
                origin: Origin::Group(raw.clone()),
                source: ToolSource::Locked(entry.clone()),
            });
            binding_index.insert(entry.name.clone(), idx);
        }
    }

    // Step 3: positional overrides (right-most wins, including among
    // positionals themselves).
    for pos in positionals {
        if let Some(&idx) = binding_index.get(&pos.binding) {
            let existing = &mut selected[idx];
            existing.origin = Origin::Explicit;
            existing.source = ToolSource::Explicit(pos.identifier.clone());
        } else {
            let idx = selected.len();
            selected.push(SelectedTool {
                binding: pos.binding.clone(),
                origin: Origin::Explicit,
                source: ToolSource::Explicit(pos.identifier.clone()),
            });
            binding_index.insert(pos.binding.clone(), idx);
        }
    }

    Ok(selected)
}

/// Resolve a previously [`select_tool_set`]-selected slice to concrete pull
/// identifiers for `platform`.
///
/// Maps each [`SelectedTool`] to a [`ResolvedTool`]: a [`ToolSource::Locked`]
/// group entry resolves its host leaf via [`host_leaf_identifier`]; a
/// [`ToolSource::Explicit`] positional keeps its identifier verbatim. Because
/// only the entries in `selected` are resolved, [`ProjectErrorKind::NoHostLeaf`]
/// can surface solely for a tool actually in this slice — a caller that
/// filtered the selection to a NAME subset never trips on an unrelated sibling.
///
/// # Errors
///
/// Returns [`ProjectErrorKind::NoHostLeaf`] when a `Locked` V2 entry ships no
/// leaf for `platform` and no `"any"` fallback at the locked version.
pub fn resolve_selected_tools(
    selected: &[SelectedTool],
    platform: &Platform,
) -> Result<Vec<ResolvedTool>, super::Error> {
    selected
        .iter()
        .map(|tool| {
            let identifier = match &tool.source {
                ToolSource::Locked(locked) => host_leaf_identifier(locked, platform)?,
                ToolSource::Explicit(identifier) => identifier.clone(),
            };
            Ok(ResolvedTool {
                binding: tool.binding.clone(),
                identifier,
                origin: tool.origin.clone(),
            })
        })
        .collect()
}

/// Compose the final tool set from selected groups and positional packages.
///
/// Pure function — no I/O. Lock load + staleness checks happen at the CLI
/// boundary; this function trusts the caller to have validated those.
///
/// Thin delegate over [`select_tool_set`] followed by [`resolve_selected_tools`]:
/// selection performs whole-scope duplicate validation (at the lock level) and
/// positional overrides, then resolution maps every surviving entry to its
/// host-platform pull [`Identifier`]. The two concerns are split so a caller
/// that needs only a subset can filter between the steps; `compose_tool_set`
/// keeps the original "resolve everything" contract by resolving the entire
/// selection.
///
/// Note the internal ordering: duplicate validation now fully precedes
/// host-leaf resolution and compares [`LockedTool`] content rather than
/// resolved leaves. This is behaviour-preserving for real locks — a lock is
/// generated from one resolved manifest, so the same repository implies an
/// identical `platforms` map.
///
/// `current_platform` is the host platform: each lock entry resolves to its
/// host-compatible leaf digest for that platform via
/// [`crate::project::resolve::lookup_host_leaf`]. `config` is currently
/// unused beyond signature parity.
pub fn compose_tool_set(
    config: &ProjectConfig,
    lock: Option<&ProjectLock>,
    groups: &[String],
    positionals: &[PositionalPackage],
    current_platform: &Platform,
) -> Result<Vec<ResolvedTool>, super::Error> {
    let selected = select_tool_set(config, lock, groups, positionals)?;
    resolve_selected_tools(&selected, current_platform)
}

/// Resolve a locked tool to its host-platform pull [`Identifier`].
///
/// Looks up the host platform's compatible leaf via
/// [`crate::project::resolve::lookup_host_leaf`] and reconstructs
/// `repository.clone_with_digest(leaf)`. No compatible entry →
/// [`ProjectErrorKind::NoHostLeaf`]; two or more entries tied at the maximum
/// score → [`ProjectErrorKind::AmbiguousHostLeaf`] — the two conditions carry
/// different remedies (re-resolve vs. disambiguate with `--platform`), so
/// they are surfaced as distinct error kinds rather than both collapsing to
/// "no leaf".
///
/// Single source of the host-leaf resolution shared by every lock reader —
/// `compose_tool_set`, `ocx pull`, and lock materialization — so both
/// conditions always carry the same typed error and message. The error is
/// the outer [`super::Error`] enum so CLI callers can convert it with
/// `.map_err(anyhow::Error::from)` and still have the `main.rs` boundary
/// classify it via the enum's [`crate::cli::ClassifyExitCode`] impl.
///
/// # Errors
///
/// Returns [`ProjectErrorKind::NoHostLeaf`] when the entry ships no leaf
/// compatible with the host platform at the locked version, or
/// [`ProjectErrorKind::AmbiguousHostLeaf`] when two or more leaves tie.
pub fn host_leaf_identifier(tool: &LockedTool, current_platform: &Platform) -> Result<Identifier, super::Error> {
    match super::resolve::lookup_host_leaf(&tool.platforms, current_platform) {
        Selection::Found((leaf, _key)) => Ok(tool.repository.clone_with_digest(leaf.clone())),
        Selection::None => Err(super::Error::Project(ProjectError::new(
            std::path::PathBuf::new(),
            ProjectErrorKind::NoHostLeaf {
                name: tool.name.clone(),
                platform: current_platform.to_string(),
            },
        ))),
        Selection::Ambiguous(candidates) => Err(super::Error::Project(ProjectError::new(
            std::path::PathBuf::new(),
            ProjectErrorKind::AmbiguousHostLeaf {
                name: tool.name.clone(),
                platform: current_platform.to_string(),
                candidates: candidates.into_iter().map(|(_, key)| key.to_string()).collect(),
            },
        ))),
    }
}

/// Validates that `name` is a plausible binding identifier — non-empty and
/// composed of characters that won't ever appear inside an OCI identifier
/// before the first `=`. Used by [`parse_positional`] to disambiguate
/// `name=identifier` from a degenerate identifier that happens to contain
/// `=`. OCI identifiers do not contain `=`, so any leading prefix matching
/// `[A-Za-z0-9_.-]+=` is treated as the explicit-binding form.
fn is_valid_binding(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::{Digest, Identifier};
    use crate::project::lock::{LockMetadata, LockVersion, LockedTool, ProjectLock};
    use std::collections::BTreeMap;

    fn sha(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    /// The host platform the compose tests resolve against. Every lock
    /// fixture below ships a `linux/amd64` leaf so the host lookup finds it.
    fn host() -> Platform {
        "linux/amd64".parse().expect("valid host platform")
    }

    fn lock_with(tools: Vec<LockedTool>) -> ProjectLock {
        ProjectLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V3,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", sha('0')),
                generated_by: "ocx test".into(),
                generated_at: "2026-04-24T00:00:00Z".into(),
            },
            tools,
        }
    }

    fn cfg() -> ProjectConfig {
        ProjectConfig::from_parts(BTreeMap::new(), BTreeMap::new())
    }

    /// A [`LockedTool`] with a single `linux/amd64` leaf keyed by the host
    /// platform's canonical grammar key. `c` selects the leaf digest byte so
    /// distinct content can be expressed without tags (the lock carries no
    /// tag).
    fn locked(name: &str, group: &str, reg: &str, repo: &str, c: char) -> LockedTool {
        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64".to_string(), Digest::Sha256(sha(c)));
        LockedTool {
            name: name.into(),
            group: group.into(),
            repository: Identifier::new_registry(repo, reg),
            platforms,
        }
    }

    /// Like [`locked`] but ships ONLY a `windows/amd64` leaf — no
    /// `linux/amd64`, no `"any"`. On the linux test [`host`] the lookup
    /// finds nothing, so [`resolve_selected_tools`] errors with
    /// `NoHostLeaf` for this entry while [`select_tool_set`] (resolution-free)
    /// returns it without error.
    fn locked_windows_only(name: &str, group: &str, reg: &str, repo: &str, c: char) -> LockedTool {
        let mut platforms = BTreeMap::new();
        platforms.insert("windows/amd64".to_string(), Digest::Sha256(sha(c)));
        LockedTool {
            name: name.into(),
            group: group.into(),
            repository: Identifier::new_registry(repo, reg),
            platforms,
        }
    }

    // ── parse_positional ────────────────────────────────────────────────

    #[test]
    fn parse_positional_inferred_binding_from_repo_basename() {
        let pkg = parse_positional("ocx.sh/cmake:3.28", "ocx.sh").expect("ok");
        assert_eq!(pkg.binding, "cmake");
        assert_eq!(pkg.identifier.tag(), Some("3.28"));
        assert_eq!(pkg.identifier.registry(), "ocx.sh");
    }

    #[test]
    fn parse_positional_inferred_binding_from_short_form() {
        let pkg = parse_positional("cmake:3.28", "ocx.sh").expect("ok");
        assert_eq!(pkg.binding, "cmake");
        assert_eq!(pkg.identifier.registry(), "ocx.sh");
    }

    #[test]
    fn parse_positional_inferred_binding_from_nested_repo() {
        let pkg = parse_positional("ghcr.io/acme/foo:1", "ocx.sh").expect("ok");
        assert_eq!(pkg.binding, "foo", "binding is the *last* repo segment");
        assert_eq!(pkg.identifier.registry(), "ghcr.io");
    }

    #[test]
    fn parse_positional_explicit_binding_via_name_prefix() {
        let pkg = parse_positional("dotnet=ocx.sh/microsoft-dotnet-sdk:10", "ocx.sh").expect("ok");
        assert_eq!(pkg.binding, "dotnet");
        assert_eq!(pkg.identifier.repository(), "microsoft-dotnet-sdk");
    }

    #[test]
    fn parse_positional_rejects_invalid_identifier() {
        let err = parse_positional("ocx.sh/CMAKE:3.28", "ocx.sh").expect_err("uppercase repo rejected");
        let crate::project::Error::Project(pe) = err;
        assert!(matches!(pe.kind, ProjectErrorKind::ToolValueInvalid { .. }));
    }

    // ── compose: groups only ────────────────────────────────────────────

    #[test]
    fn compose_default_group_returns_lock_entries() {
        let lock = lock_with(vec![
            locked("cmake", "default", "ocx.sh", "cmake", 'a'),
            locked("ninja", "default", "ocx.sh", "ninja", 'b'),
        ]);
        let out = compose_tool_set(&cfg(), Some(&lock), &["default".into()], &[], &host()).expect("ok");
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|r| r.binding == "cmake"));
        assert!(out.iter().any(|r| r.binding == "ninja"));
        for r in &out {
            assert_eq!(r.origin, Origin::Group("default".into()));
            // V2 entries resolve to a digest-pinned identifier (host leaf),
            // never a tag.
            assert!(
                r.identifier.digest().is_some(),
                "group entry must resolve to the host-leaf digest"
            );
            assert!(r.identifier.tag().is_none(), "V2 host-leaf identifier carries no tag");
        }
    }

    #[test]
    fn compose_dedups_repeated_group_names() {
        let lock = lock_with(vec![locked("cmake", "default", "ocx.sh", "cmake", 'a')]);
        let out = compose_tool_set(
            &cfg(),
            Some(&lock),
            &["default".into(), "default".into(), "default".into()],
            &[],
            &host(),
        )
        .expect("ok");
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn compose_unions_multiple_groups() {
        let lock = lock_with(vec![
            locked("cmake", "default", "ocx.sh", "cmake", 'a'),
            locked("shellcheck", "ci", "ocx.sh", "shellcheck", 'b'),
            locked("shfmt", "ci", "ocx.sh", "shfmt", 'c'),
        ]);
        let out = compose_tool_set(&cfg(), Some(&lock), &["default".into(), "ci".into()], &[], &host()).expect("ok");
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn compose_errors_on_duplicate_binding_across_groups_with_different_content() {
        // Same binding in two groups with DIFFERENT leaf digests (distinct
        // content) → error.
        let lock = lock_with(vec![
            locked("shellcheck", "ci", "ocx.sh", "shellcheck", 'a'),
            locked("shellcheck", "lint", "ocx.sh", "shellcheck", 'b'),
        ]);
        let err =
            compose_tool_set(&cfg(), Some(&lock), &["ci".into(), "lint".into()], &[], &host()).expect_err("conflict");
        let crate::project::Error::Project(pe) = err;
        let ProjectErrorKind::DuplicateToolAcrossSelectedGroups { name, group_a, group_b } = &pe.kind else {
            panic!("expected DuplicateToolAcrossSelectedGroups, got {:?}", pe.kind);
        };
        assert_eq!(name, "shellcheck");
        assert_eq!(group_a, "ci");
        assert_eq!(group_b, "lint");
    }

    #[test]
    fn compose_collapses_duplicate_binding_with_identical_content() {
        // Two groups define the same binding name with the *same* host leaf
        // digest — collapse silently to one entry, no error.
        let lock = lock_with(vec![
            locked("shellcheck", "ci", "ocx.sh", "shellcheck", 'a'),
            locked("shellcheck", "lint", "ocx.sh", "shellcheck", 'a'),
        ]);
        let out = compose_tool_set(&cfg(), Some(&lock), &["ci".into(), "lint".into()], &[], &host()).expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].binding, "shellcheck");
        // First-seen group wins for origin attribution.
        assert_eq!(out[0].origin, Origin::Group("ci".into()));
    }

    #[test]
    fn compose_lock_missing_when_group_selected_without_lock() {
        let err = compose_tool_set(&cfg(), None, &["default".into()], &[], &host()).expect_err("no lock");
        let crate::project::Error::Project(pe) = err;
        assert!(matches!(pe.kind, ProjectErrorKind::LockMissing));
    }

    /// ADR Install-time platform resolution: a host whose key is absent (and
    /// no `"any"`) gives a clean pre-network error from compose, not a late
    /// `SelectResult::NotFound`. The lock ships only `linux/amd64`; composing
    /// for `windows/amd64` must error.
    #[test]
    fn compose_errors_when_host_leaf_absent() {
        let lock = lock_with(vec![locked("cmake", "default", "ocx.sh", "cmake", 'a')]);
        let windows: Platform = "windows/amd64".parse().expect("valid platform");
        let err = compose_tool_set(&cfg(), Some(&lock), &["default".into()], &[], &windows)
            .expect_err("absent host leaf must error before any network call");
        let crate::project::Error::Project(_) = err;
    }

    /// Regression (bugfix `run_named_scope_resolution`): selection is
    /// resolution-free, so an unnamed sibling that ships no host leaf for the
    /// current host does NOT abort selection; resolving only the named subset
    /// then succeeds, while resolving the whole set still trips on the sibling.
    #[test]
    fn named_subset_skips_unnamed_sibling_without_host_leaf() {
        // default group: cmake (linux leaf) + winonly (windows-only leaf).
        let lock = lock_with(vec![
            locked("cmake", "default", "ocx.sh", "cmake", 'a'),
            locked_windows_only("winonly", "default", "ocx.sh", "winonly", 'b'),
        ]);
        let host = host(); // linux/amd64

        // Selection is resolution-free: BOTH entries returned, no NoHostLeaf.
        let selected = select_tool_set(&cfg(), Some(&lock), &["default".into()], &[])
            .expect("select must not resolve host leaves");
        assert_eq!(selected.len(), 2);

        // Resolving ONLY the named subset (cmake) succeeds.
        let cmake: Vec<_> = selected.iter().filter(|s| s.binding == "cmake").cloned().collect();
        assert!(resolve_selected_tools(&cmake, &host).is_ok());

        // Resolving the whole set still errors — proving the sibling is the
        // condition the old eager whole-scope resolve tripped on.
        assert!(resolve_selected_tools(&selected, &host).is_err());
    }

    // ── compose: positionals ────────────────────────────────────────────

    #[test]
    fn compose_positionals_only_with_no_lock_ok() {
        let pos = parse_positional("ocx.sh/cmake:3.28", "ocx.sh").expect("ok");
        let out = compose_tool_set(&cfg(), None, &[], std::slice::from_ref(&pos), &host()).expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].binding, "cmake");
        assert_eq!(out[0].origin, Origin::Explicit);
        // Positionals are NOT lock entries — they keep their tag-style id.
        assert_eq!(out[0].identifier.tag(), Some("3.28"));
    }

    #[test]
    fn compose_positional_overrides_group_entry_by_inferred_binding() {
        // default ships a cmake host leaf; positional `cmake:3.29` infers
        // binding `cmake` and overrides.
        let lock = lock_with(vec![locked("cmake", "default", "ocx.sh", "cmake", 'a')]);
        let pos = parse_positional("cmake:3.29", "ocx.sh").expect("ok");
        let out = compose_tool_set(
            &cfg(),
            Some(&lock),
            &["default".into()],
            std::slice::from_ref(&pos),
            &host(),
        )
        .expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].binding, "cmake");
        assert_eq!(out[0].origin, Origin::Explicit);
        assert_eq!(out[0].identifier.tag(), Some("3.29"));
    }

    #[test]
    fn compose_positional_explicit_binding_wins_over_repo_basename() {
        // default ships a dotnet host leaf; positional
        // `dotnet=ocx.sh/microsoft-dotnet-sdk:10` overrides via name= prefix
        // (binding does NOT match repo basename).
        let lock = lock_with(vec![locked("dotnet", "default", "ocx.sh", "microsoft-dotnet-sdk", 'a')]);
        let pos = parse_positional("dotnet=ocx.sh/microsoft-dotnet-sdk:10", "ocx.sh").expect("ok");
        let out = compose_tool_set(
            &cfg(),
            Some(&lock),
            &["default".into()],
            std::slice::from_ref(&pos),
            &host(),
        )
        .expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].binding, "dotnet");
        assert_eq!(out[0].identifier.tag(), Some("10"));
        assert_eq!(out[0].origin, Origin::Explicit);
    }

    #[test]
    fn compose_positional_with_different_binding_adds_fresh_entry() {
        // default ships a terraform host leaf; positional
        // `opentofu=ocx.sh/opentofu:1.7` has a different binding name so both
        // entries are kept.
        let lock = lock_with(vec![locked("terraform", "default", "ocx.sh", "opentofu", 'a')]);
        let pos = parse_positional("opentofu=ocx.sh/opentofu:1.7", "ocx.sh").expect("ok");
        let out = compose_tool_set(
            &cfg(),
            Some(&lock),
            &["default".into()],
            std::slice::from_ref(&pos),
            &host(),
        )
        .expect("ok");
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|r| r.binding == "terraform"));
        assert!(out.iter().any(|r| r.binding == "opentofu"));
    }

    #[test]
    fn compose_positionals_right_most_wins() {
        // Two positionals share inferred binding `cmake`. Right-most must win.
        let p1 = parse_positional("cmake:3.28", "ocx.sh").expect("ok");
        let p2 = parse_positional("cmake:3.29", "ocx.sh").expect("ok");
        let out = compose_tool_set(&cfg(), None, &[], &[p1, p2], &host()).expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].identifier.tag(), Some("3.29"));
    }

    // ── compose_preserves_group_selection_order ─────────────────────────

    /// Plan §Phase 3.1: `compose_preserves_group_selection_order`
    ///
    /// Groups are iterated in user-specified order, NOT alphabetically.
    /// Here `ci` comes before `default` in the `-g` list, so `shellcheck`
    /// (from ci) must appear before `cmake` (from default) in the output.
    #[test]
    fn compose_preserves_group_selection_order() {
        let lock = lock_with(vec![
            locked("cmake", "default", "ocx.sh", "cmake", 'a'),
            locked("shellcheck", "ci", "ocx.sh", "shellcheck", 'b'),
        ]);
        let cfg = cfg();
        let out = compose_tool_set(&cfg, Some(&lock), &["ci".into(), "default".into()], &[], &host()).expect("ok");
        assert_eq!(out.len(), 2);
        // ci group (shellcheck) must come first because ci was selected first.
        assert_eq!(out[0].binding, "shellcheck", "ci group must be first; got {out:?}");
        assert_eq!(out[1].binding, "cmake", "default group must be second; got {out:?}");
        // Verify origins
        assert_eq!(out[0].origin, Origin::Group("ci".into()));
        assert_eq!(out[1].origin, Origin::Group("default".into()));
    }

    // ── expand_all_keyword ───────────────────────────────────────────────

    fn cfg_with_groups(group_names: &[&str]) -> ProjectConfig {
        let mut groups = BTreeMap::new();
        for &name in group_names {
            groups.insert(name.to_string(), BTreeMap::new());
        }
        ProjectConfig::from_parts(BTreeMap::new(), groups)
    }

    /// Plan §Phase 3.1: `expand_all_empty_input_returns_empty`
    ///
    /// Empty input → empty output. The helper does NOT inject default scope
    /// on empty input — that promotion is the caller's responsibility.
    #[test]
    fn expand_all_empty_input_returns_empty() {
        let cfg = cfg_with_groups(&["ci", "lint", "release"]);
        let result = expand_all_keyword(&[], &cfg);
        assert!(
            result.is_empty(),
            "empty input must return empty output; got {result:?}"
        );
    }

    /// Plan §Phase 3.1: `expand_all_no_keyword_passthrough`
    ///
    /// When no `all` keyword is present, the input passes through unchanged.
    #[test]
    fn expand_all_no_keyword_passthrough() {
        let cfg = cfg_with_groups(&["ci", "lint", "release"]);
        let input: Vec<String> = vec!["ci".into(), "release".into()];
        let result = expand_all_keyword(&input, &cfg);
        assert_eq!(result, input, "no `all` keyword: input must pass through unchanged");
    }

    /// Plan §Phase 3.1: `expand_all_inserts_default_plus_all_named_groups_in_place`
    ///
    /// Config has named groups {ci, lint, release} (BTreeMap → alphabetical).
    /// Input `[ci, all, release]` becomes
    /// `[ci, default, ci, lint, release, release]` (pre-dedup; compose_tool_set
    /// deduplicates at its own step).
    #[test]
    fn expand_all_inserts_default_plus_all_named_groups_in_place() {
        let cfg = cfg_with_groups(&["ci", "lint", "release"]);
        let input: Vec<String> = vec!["ci".into(), "all".into(), "release".into()];
        let result = expand_all_keyword(&input, &cfg);
        // `all` expands in place to [default, ci, lint, release]
        // so full result = [ci, default, ci, lint, release, release]
        assert_eq!(
            result,
            vec!["ci", "default", "ci", "lint", "release", "release"],
            "all must expand in place; got {result:?}"
        );
    }

    /// Plan §Phase 3.1: `expand_all_only_keyword_returns_default_plus_named_groups`
    ///
    /// Input `[all]` expands to `[default, ci, lint, release]`
    /// (alphabetical named groups because BTreeMap).
    #[test]
    fn expand_all_only_keyword_returns_default_plus_named_groups() {
        let cfg = cfg_with_groups(&["ci", "lint", "release"]);
        let input: Vec<String> = vec!["all".into()];
        let result = expand_all_keyword(&input, &cfg);
        assert_eq!(
            result,
            vec!["default", "ci", "lint", "release"],
            "only `all`: must expand to default + alphabetical named groups; got {result:?}"
        );
    }
}
