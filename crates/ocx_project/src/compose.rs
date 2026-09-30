// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Tool-set composition for `ocx exec`: `--group` selections from `ocx.lock`
//! plus positional packages. Pure; the CLI loads the lock.

use std::path::Path;

use crate::DEFAULT_GROUP;
use crate::config::ProjectConfig;
use crate::lock::{BoundTool, ProjectLock, locked_tool_content_equal};
use ocx_oci::package_ref::error::IdentifierErrorKind;
use ocx_oci::{PackageRef, Platform};
use ocx_package::metadata::env::entry::Entry;

use super::error::{ProjectError, ProjectErrorKind};

/// The project `[env]`, then the selected groups' `[group.<name>.env]`, as
/// resolved [`Entry`] values applied after package env, so a constant here wins.
/// A repeated group keeps only its last occurrence; relative `path` values
/// resolve against `config_path`'s directory.
pub fn project_env_entries(config: &ProjectConfig, config_path: &Path, groups: &[String]) -> Vec<Entry> {
    // `.` only keeps a hand-built relative path from panicking.
    let project_root = config_path.parent().unwrap_or_else(|| Path::new("."));

    let mut entries = config.env.to_entries(project_root);

    for (index, name) in groups.iter().enumerate() {
        if name == DEFAULT_GROUP || groups[index + 1..].contains(name) {
            continue;
        }
        let Some(group) = config.groups.get(name) else {
            continue;
        };
        entries.extend(group.env.to_entries(project_root));
    }

    entries
}

/// Provenance of a resolved tool entry — drives error messages and logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Came from a `--group <name>` selection via the lock.
    Group(String),
    /// Came from an explicit positional package on the command line.
    Explicit,
}

/// One tool resolved by [`compose_tool_set`]: digest-pinned for group
/// entries, tag-style for positionals.
#[derive(Debug, Clone)]
pub struct ResolvedTool {
    pub binding: String,
    pub identifier: PackageRef,
    pub origin: Origin,
}

/// The unresolved source of a [`SelectedTool`]. Selection stays platform-free
/// so a caller narrows to named tools first, or a sibling with no host leaf aborts the run.
#[derive(Debug, Clone)]
pub enum ToolSource {
    /// A lock entry bound to its declaration; its host leaf resolves only if it
    /// survives name filtering.
    Locked(BoundTool),
    /// A positional `name=identifier`, resolved verbatim.
    Explicit(PackageRef),
}

/// One tool selected by [`select_tool_set`], before host-leaf resolution.
#[derive(Debug, Clone)]
pub struct SelectedTool {
    pub binding: String,
    pub origin: Origin,
    pub source: ToolSource,
}

/// One positional package; `binding` is explicit (`name=`) or the repository basename.
#[derive(Debug, Clone)]
pub struct PositionalPackage {
    pub binding: String,
    pub identifier: PackageRef,
}

/// Parse `[name=]identifier`; without `name=` the binding is the repository
/// basename (`ghcr.io/acme/foo:1` → `foo`). Short forms use `default_registry`.
pub fn parse_positional(input: &str, default_registry: &str) -> Result<PositionalPackage, super::Error> {
    // The first `=` splits unambiguously: identifiers never contain `=`.
    let (explicit_binding, ident_str) = match input.split_once('=') {
        Some((name, rest)) if is_valid_binding(name) => (Some(name.to_string()), rest),
        _ => (None, input),
    };

    let identifier =
        PackageRef::parse_with_default_registry(ident_str, default_registry).map_err(|e| -> super::Error {
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

/// Expand `all` in place to [`DEFAULT_GROUP`] then every named group,
/// alphabetically; duplicates are left for [`compose_tool_set`]. Empty input
/// stays empty: the caller defaults an empty list.
pub fn expand_all_keyword(groups: &[String], config: &ProjectConfig) -> Vec<String> {
    if groups.is_empty() {
        return Vec::new();
    }
    let all_keyword = super::internal::ALL_GROUP;
    let default_group = super::internal::DEFAULT_GROUP;
    let mut out = Vec::with_capacity(groups.len());
    for entry in groups {
        if entry == all_keyword {
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

/// The selection half of [`compose_tool_set`]: group entries stay unresolved so
/// a caller can filter by name before resolving host leaves.
/// Detects but does not report a cross-group duplicate: every caller must run
/// [`check_duplicate_selection`] over the surviving set.
///
/// # Errors
///
/// [`ProjectErrorKind::LockMissing`] when a group is selected without a lock;
/// [`ProjectErrorKind::LockOutOfSync`] when the lock no longer binds to `config`.
pub fn select_tool_set(
    config: &ProjectConfig,
    lock: Option<&ProjectLock>,
    groups: &[String],
    positionals: &[PositionalPackage],
) -> Result<Vec<SelectedTool>, super::Error> {
    let mut selected: Vec<SelectedTool> = Vec::new();
    // Every push to `selected` pairs with an insert here, or the map goes stale.
    let mut binding_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    let bound: Vec<BoundTool> = match lock {
        _ if groups.is_empty() => Vec::new(),
        // The CLI reports `LockMissing` before calling; reaching here is a broken contract.
        None => {
            return Err(super::Error::Project(ProjectError::new(
                std::path::PathBuf::new(),
                ProjectErrorKind::LockMissing,
            )));
        }
        // Callers refuse a stale hash first; this catches a per-entry desync the hash cannot see.
        Some(lock_ref) => lock_ref.bind_current(config)?,
    };

    let mut seen_groups: Vec<&str> = Vec::with_capacity(groups.len());
    for raw in groups {
        if seen_groups.contains(&raw.as_str()) {
            continue;
        }
        seen_groups.push(raw.as_str());

        for bound_tool in &bound {
            let entry = bound_tool.locked();
            if entry.group != *raw {
                continue;
            }
            // Identical content in another selected group collapses; differing
            // content keeps both for `check_duplicate_selection` after the NAME filter.
            let mut conflicting = false;
            if let Some(&idx) = binding_index.get(&entry.name) {
                let existing = &selected[idx];
                let from_other_group = matches!(&existing.origin, Origin::Group(g) if g != raw);
                if from_other_group {
                    let ToolSource::Locked(existing_tool) = &existing.source else {
                        unreachable!("a Group-origin selection always carries a Locked source");
                    };
                    if locked_tool_content_equal(existing_tool.locked(), entry) {
                        continue;
                    }
                    conflicting = true;
                }
            }
            let idx = selected.len();
            selected.push(SelectedTool {
                binding: entry.name.clone(),
                origin: Origin::Group(raw.clone()),
                source: ToolSource::Locked(bound_tool.clone()),
            });
            // The index stays on the first occurrence, or a third group compares
            // against the wrong sibling.
            if !conflicting {
                binding_index.insert(entry.name.clone(), idx);
            }
        }
    }

    // Positionals override right-most-wins, replacing every group entry for the
    // binding (a conflicting twin included) at the first match's position.
    for pos in positionals {
        let mut overridden = false;
        selected.retain_mut(|tool| {
            if tool.binding != pos.binding {
                return true;
            }
            if overridden {
                return false;
            }
            overridden = true;
            tool.origin = Origin::Explicit;
            tool.source = ToolSource::Explicit(pos.identifier.clone());
            true
        });
        if !overridden {
            selected.push(SelectedTool {
                binding: pos.binding.clone(),
                origin: Origin::Explicit,
                source: ToolSource::Explicit(pos.identifier.clone()),
            });
        }
    }

    Ok(selected)
}

/// Report a binding two selected groups resolve differently. Run after any
/// name filter, so `ocx exec go-task` never fails over a binding it did not name.
///
/// # Errors
///
/// [`ProjectErrorKind::DuplicateToolAcrossSelectedGroups`] naming the binding
/// and the first two disagreeing groups.
pub fn check_duplicate_selection(selected: &[SelectedTool]) -> Result<(), super::Error> {
    let mut first_position_by_binding: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::with_capacity(selected.len());

    for (position, tool) in selected.iter().enumerate() {
        let first = *first_position_by_binding
            .entry(tool.binding.as_str())
            .or_insert(position);
        if first == position {
            continue;
        }
        let (Origin::Group(group_a), Origin::Group(group_b)) = (&selected[first].origin, &tool.origin) else {
            continue;
        };
        return Err(super::Error::Project(ProjectError::new(
            std::path::PathBuf::new(),
            ProjectErrorKind::DuplicateToolAcrossSelectedGroups {
                name: tool.binding.clone(),
                group_a: group_a.clone(),
                group_b: group_b.clone(),
            },
        )));
    }

    Ok(())
}

/// Resolve selected tools for `platform`: a locked entry to its host leaf, an
/// explicit one verbatim.
///
/// # Errors
///
/// [`BoundTool::host_leaf_identifier`]'s, for a locked entry.
pub fn resolve_selected_tools(
    selected: &[SelectedTool],
    platform: &Platform,
) -> Result<Vec<ResolvedTool>, super::Error> {
    selected
        .iter()
        .map(|tool| {
            let identifier = match &tool.source {
                ToolSource::Locked(bound) => bound.host_leaf_identifier(platform)?.into(),
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

/// Compose the final tool set: select, check duplicates, resolve. Duplicates
/// compare [`LockedTool`](crate::LockedTool) content, not resolved leaves; the
/// two agree for real locks, where one manifest yields one `platforms` map.
pub fn compose_tool_set(
    config: &ProjectConfig,
    lock: Option<&ProjectLock>,
    groups: &[String],
    positionals: &[PositionalPackage],
    current_platform: &Platform,
) -> Result<Vec<ResolvedTool>, super::Error> {
    let selected = select_tool_set(config, lock, groups, positionals)?;
    check_duplicate_selection(&selected)?;
    resolve_selected_tools(&selected, current_platform)
}

fn is_valid_binding(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{LockMetadata, LockVersion, LockedTool, ProjectLock};
    use ocx_oci::Digest;
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

    const DECLARED_TAG: &str = "1.0";

    /// `tools` locked from an `ocx.toml` declaring each one as
    /// `<repository>:DECLARED_TAG` under its own `(group, name)`, so the lock binds current.
    fn current_lock(tools: Vec<LockedTool>) -> (ProjectConfig, ProjectLock) {
        let mut default_tools = BTreeMap::new();
        let mut groups: BTreeMap<String, BTreeMap<String, PackageRef>> = BTreeMap::new();
        for tool in &tools {
            let declared = PackageRef::new_registry(tool.repository.repository(), tool.repository.registry())
                .clone_with_tag(DECLARED_TAG);
            let group_tools = if tool.group == DEFAULT_GROUP {
                &mut default_tools
            } else {
                groups.entry(tool.group.clone()).or_default()
            };
            group_tools.insert(tool.name.clone(), declared);
        }
        let config = ProjectConfig::from_parts(default_tools, groups);
        let mut lock = lock_with(tools);
        lock.metadata.declaration_hash = config.declaration_hash_cached().to_owned();
        (config, lock)
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
            repository: ocx_oci::Repository::new(reg, repo),
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
            repository: ocx_oci::Repository::new(reg, repo),
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
        let crate::Error::Project(pe) = err else {
            panic!("expected a project-tier error, got {err:?}");
        };
        assert!(matches!(pe.kind, ProjectErrorKind::ToolValueInvalid { .. }));
    }

    // ── compose: groups only ────────────────────────────────────────────

    #[test]
    fn compose_default_group_returns_lock_entries() {
        let (config, lock) = current_lock(vec![
            locked("cmake", "default", "ocx.sh", "cmake", 'a'),
            locked("ninja", "default", "ocx.sh", "ninja", 'b'),
        ]);
        let out = compose_tool_set(&config, Some(&lock), &["default".into()], &[], &host()).expect("ok");
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|r| r.binding == "cmake"));
        assert!(out.iter().any(|r| r.binding == "ninja"));
        for r in &out {
            assert_eq!(r.origin, Origin::Group("default".into()));
            // The host-leaf digest from the lock, with the tag `ocx.toml` declares.
            assert!(
                r.identifier.digest().is_some(),
                "group entry must resolve to the host-leaf digest"
            );
            assert_eq!(
                r.identifier.tag(),
                Some(DECLARED_TAG),
                "the declared tag rides on the lock leaf"
            );
        }
    }

    #[test]
    fn compose_dedups_repeated_group_names() {
        let (config, lock) = current_lock(vec![locked("cmake", "default", "ocx.sh", "cmake", 'a')]);
        let out = compose_tool_set(
            &config,
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
        let (config, lock) = current_lock(vec![
            locked("cmake", "default", "ocx.sh", "cmake", 'a'),
            locked("shellcheck", "ci", "ocx.sh", "shellcheck", 'b'),
            locked("shfmt", "ci", "ocx.sh", "shfmt", 'c'),
        ]);
        let out = compose_tool_set(&config, Some(&lock), &["default".into(), "ci".into()], &[], &host()).expect("ok");
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn compose_errors_on_duplicate_binding_across_groups_with_different_content() {
        // Same binding in two groups with DIFFERENT leaf digests (distinct
        // content) → error.
        let (config, lock) = current_lock(vec![
            locked("shellcheck", "ci", "ocx.sh", "shellcheck", 'a'),
            locked("shellcheck", "lint", "ocx.sh", "shellcheck", 'b'),
        ]);
        let err =
            compose_tool_set(&config, Some(&lock), &["ci".into(), "lint".into()], &[], &host()).expect_err("conflict");
        let crate::Error::Project(pe) = err else {
            panic!("expected a project-tier error, got {err:?}");
        };
        let ProjectErrorKind::DuplicateToolAcrossSelectedGroups { name, group_a, group_b } = &pe.kind else {
            panic!("expected DuplicateToolAcrossSelectedGroups, got {:?}", pe.kind);
        };
        assert_eq!(name, "shellcheck");
        assert_eq!(group_a, "ci");
        assert_eq!(group_b, "lint");
    }

    /// Selection must not report the conflict itself — it keeps both entries so
    /// a caller can filter the colliding binding out before validating. Without
    /// this, `ocx exec cmake` fails over a `shellcheck` it never named.
    #[test]
    fn select_keeps_both_entries_for_a_conflicting_binding() {
        let (config, lock) = current_lock(vec![
            locked("shellcheck", "ci", "ocx.sh", "shellcheck", 'a'),
            locked("shellcheck", "lint", "ocx.sh", "shellcheck", 'b'),
            locked("cmake", "ci", "ocx.sh", "cmake", 'c'),
        ]);
        let selected = select_tool_set(&config, Some(&lock), &["ci".into(), "lint".into()], &[])
            .expect("selection must not error");

        let shellcheck: Vec<_> = selected.iter().filter(|tool| tool.binding == "shellcheck").collect();
        assert_eq!(shellcheck.len(), 2, "both conflicting entries must survive selection");
        assert_eq!(shellcheck[0].origin, Origin::Group("ci".into()));
        assert_eq!(shellcheck[1].origin, Origin::Group("lint".into()));

        // The conflict is real — it only waits for the caller to validate.
        check_duplicate_selection(&selected).expect_err("the unfiltered set still conflicts");

        // Narrowed to a binding that does not collide, the set validates clean.
        let narrowed: Vec<_> = selected.into_iter().filter(|tool| tool.binding == "cmake").collect();
        check_duplicate_selection(&narrowed).expect("a subset without the conflict must pass");
    }

    /// `group_a` names the first-seen group even when a third group also
    /// disagrees, so the reported pair does not drift off the group walk order.
    #[test]
    fn check_duplicate_selection_reports_the_first_seen_group_pair() {
        let (config, lock) = current_lock(vec![
            locked("shellcheck", "ci", "ocx.sh", "shellcheck", 'a'),
            locked("shellcheck", "lint", "ocx.sh", "shellcheck", 'b'),
            locked("shellcheck", "release", "ocx.sh", "shellcheck", 'c'),
        ]);
        let selected = select_tool_set(
            &config,
            Some(&lock),
            &["ci".into(), "lint".into(), "release".into()],
            &[],
        )
        .expect("selection must not error");

        let err = check_duplicate_selection(&selected).expect_err("conflict");
        let crate::Error::Project(pe) = err else {
            panic!("expected a project-tier error, got {err:?}");
        };
        let ProjectErrorKind::DuplicateToolAcrossSelectedGroups { name, group_a, group_b } = &pe.kind else {
            panic!("expected DuplicateToolAcrossSelectedGroups, got {:?}", pe.kind);
        };
        assert_eq!(name, "shellcheck");
        assert_eq!(group_a, "ci");
        assert_eq!(group_b, "lint");
    }

    /// An explicit positional supersedes every group entry for its binding, so
    /// deliberately overriding a conflicting binding resolves the conflict
    /// instead of leaving a stale twin for the check to trip over.
    #[test]
    fn positional_override_collapses_a_conflicting_binding() {
        let (config, lock) = current_lock(vec![
            locked("shellcheck", "ci", "ocx.sh", "shellcheck", 'a'),
            locked("shellcheck", "lint", "ocx.sh", "shellcheck", 'b'),
        ]);
        let positional = parse_positional("shellcheck=ocx.sh/shellcheck:0.11", "ocx.sh").expect("parses");
        let selected = select_tool_set(
            &config,
            Some(&lock),
            &["ci".into(), "lint".into()],
            std::slice::from_ref(&positional),
        )
        .expect("selection must not error");

        assert_eq!(selected.len(), 1, "the override must collapse both group entries");
        assert_eq!(selected[0].origin, Origin::Explicit);
        check_duplicate_selection(&selected).expect("an overridden binding carries no conflict");
    }

    #[test]
    fn compose_collapses_duplicate_binding_with_identical_content() {
        // Two groups define the same binding name with the *same* host leaf
        // digest — collapse silently to one entry, no error.
        let (config, lock) = current_lock(vec![
            locked("shellcheck", "ci", "ocx.sh", "shellcheck", 'a'),
            locked("shellcheck", "lint", "ocx.sh", "shellcheck", 'a'),
        ]);
        let out = compose_tool_set(&config, Some(&lock), &["ci".into(), "lint".into()], &[], &host()).expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].binding, "shellcheck");
        // First-seen group wins for origin attribution.
        assert_eq!(out[0].origin, Origin::Group("ci".into()));
    }

    #[test]
    fn compose_lock_missing_when_group_selected_without_lock() {
        let err = compose_tool_set(&cfg(), None, &["default".into()], &[], &host()).expect_err("no lock");
        let crate::Error::Project(pe) = err else {
            panic!("expected a project-tier error, got {err:?}");
        };
        assert!(matches!(pe.kind, ProjectErrorKind::LockMissing));
    }

    /// ADR Install-time platform resolution: a host whose key is absent (and
    /// no `"any"`) gives a clean pre-network error from compose, not a late
    /// `SelectResult::NotFound`. The lock ships only `linux/amd64`; composing
    /// for `windows/amd64` must error.
    #[test]
    fn compose_errors_when_host_leaf_absent() {
        let (config, lock) = current_lock(vec![locked("cmake", "default", "ocx.sh", "cmake", 'a')]);
        let windows: Platform = "windows/amd64".parse().expect("valid platform");
        let err = compose_tool_set(&config, Some(&lock), &["default".into()], &[], &windows)
            .expect_err("absent host leaf must error before any network call");
        let crate::Error::Project(_) = err else {
            panic!("expected a project-tier error, got {err:?}");
        };
    }

    /// Regression (bugfix `run_named_scope_resolution`): selection is
    /// resolution-free, so an unnamed sibling that ships no host leaf for the
    /// current host does NOT abort selection; resolving only the named subset
    /// then succeeds, while resolving the whole set still trips on the sibling.
    #[test]
    fn named_subset_skips_unnamed_sibling_without_host_leaf() {
        // default group: cmake (linux leaf) + winonly (windows-only leaf).
        let (config, lock) = current_lock(vec![
            locked("cmake", "default", "ocx.sh", "cmake", 'a'),
            locked_windows_only("winonly", "default", "ocx.sh", "winonly", 'b'),
        ]);
        let host = host(); // linux/amd64

        // Selection is resolution-free: BOTH entries returned, no NoHostLeaf.
        let selected = select_tool_set(&config, Some(&lock), &["default".into()], &[])
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
        let (config, lock) = current_lock(vec![locked("cmake", "default", "ocx.sh", "cmake", 'a')]);
        let pos = parse_positional("cmake:3.29", "ocx.sh").expect("ok");
        let out = compose_tool_set(
            &config,
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
        let (config, lock) = current_lock(vec![locked("dotnet", "default", "ocx.sh", "microsoft-dotnet-sdk", 'a')]);
        let pos = parse_positional("dotnet=ocx.sh/microsoft-dotnet-sdk:10", "ocx.sh").expect("ok");
        let out = compose_tool_set(
            &config,
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
        let (config, lock) = current_lock(vec![locked("terraform", "default", "ocx.sh", "opentofu", 'a')]);
        let pos = parse_positional("opentofu=ocx.sh/opentofu:1.7", "ocx.sh").expect("ok");
        let out = compose_tool_set(
            &config,
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
        let (config, lock) = current_lock(vec![
            locked("cmake", "default", "ocx.sh", "cmake", 'a'),
            locked("shellcheck", "ci", "ocx.sh", "shellcheck", 'b'),
        ]);
        let out = compose_tool_set(&config, Some(&lock), &["ci".into(), "default".into()], &[], &host()).expect("ok");
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

    // ── compose: a locked tool carries its declared tag ─────────────────

    /// A config and a lock whose recorded hash is that config's own.
    fn locked_from(config_toml: &str, tools: Vec<LockedTool>) -> (ProjectConfig, ProjectLock) {
        let config = ProjectConfig::from_toml_str(config_toml).expect("parse ocx.toml");
        let mut lock = lock_with(tools);
        lock.metadata.declaration_hash = config.declaration_hash_cached().to_owned();
        (config, lock)
    }

    /// A tag-anchored patch rule matches only an identifier that still carries
    /// the tag, so the lock digest must arrive with the declared tag attached.
    #[test]
    fn c005_compose_carries_the_declared_tag_onto_each_group_s_lock_leaf() {
        let (config, lock) = locked_from(
            "[tools]\nshellcheck = \"ocx.sh/shellcheck:0.10\"\n\n[group.ci.tools]\nshfmt = \"ocx.sh/shfmt:3.8\"\n",
            vec![
                locked("shfmt", "ci", "ocx.sh", "shfmt", 'b'),
                locked("shellcheck", "default", "ocx.sh", "shellcheck", 'a'),
            ],
        );
        let out = compose_tool_set(&config, Some(&lock), &["default".into(), "ci".into()], &[], &host())
            .expect("a current lock composes");
        let identifier_of = |binding: &str| {
            out.iter()
                .find(|tool| tool.binding == binding)
                .unwrap_or_else(|| panic!("{binding} composed"))
                .identifier
                .to_string()
        };
        assert_eq!(
            identifier_of("shellcheck"),
            format!("ocx.sh/shellcheck:0.10@sha256:{}", sha('a'))
        );
        assert_eq!(identifier_of("shfmt"), format!("ocx.sh/shfmt:3.8@sha256:{}", sha('b')));
    }

    /// The narrowed path (`select_tool_set` then `resolve_selected_tools`) that
    /// `ocx exec <name>` takes carries the same tag as the whole-set compose.
    #[test]
    fn c005_resolving_a_selection_carries_the_declared_tag() {
        let (config, lock) = locked_from(
            "[tools]\ncmake = \"ocx.sh/cmake:3.28\"\n",
            vec![locked("cmake", "default", "ocx.sh", "cmake", 'a')],
        );
        let selected = select_tool_set(&config, Some(&lock), &["default".into()], &[]).expect("selection");
        let resolved = resolve_selected_tools(&selected, &host()).expect("host leaf");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].origin, Origin::Group("default".into()));
        assert_eq!(
            resolved[0].identifier.to_string(),
            format!("ocx.sh/cmake:3.28@sha256:{}", sha('a'))
        );
    }

    /// A lock entry locked from another repository than `ocx.toml` declares,
    /// under a fresh hash: the selection refuses rather than pairing the
    /// declared tag with a digest from the wrong repository, and names the binding.
    #[test]
    fn c003_selecting_a_group_refuses_a_per_entry_repository_desync() {
        let (config, lock) = locked_from(
            "[tools]\ncmake = \"ocx.sh/cmake:3.28\"\n",
            vec![locked("cmake", "default", "ghcr.io", "cmake", 'a')],
        );
        let err = select_tool_set(&config, Some(&lock), &["default".into()], &[])
            .expect_err("a desynced entry must not select");
        let crate::Error::Project(pe) = &err else {
            panic!("expected a project-tier error, got {err:?}");
        };
        let ProjectErrorKind::LockOutOfSync { drift } = &pe.kind else {
            panic!("expected LockOutOfSync, got {:?}", pe.kind);
        };
        assert_eq!(
            drift.as_ref(),
            &crate::lock::LockDrift::Entry {
                group: "default".into(),
                name: "cmake".into(),
                locked: ocx_oci::Repository::new("ghcr.io", "cmake"),
                declared: Some(ocx_oci::Repository::new("ocx.sh", "cmake")),
            }
        );
        let rendered = err.to_string();
        for needle in ["'cmake'", "'default'", "ghcr.io/cmake", "ocx.sh/cmake"] {
            assert!(rendered.contains(needle), "{needle} missing from {rendered:?}");
        }
        assert!(!rendered.contains("declaration_hash"), "{rendered:?}");
    }
}
