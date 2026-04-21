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

use crate::oci::Identifier;
use crate::oci::identifier::error::IdentifierErrorKind;
use crate::project::config::ProjectConfig;
use crate::project::lock::ProjectLock;

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
        None => identifier
            .name()
            .expect("Identifier::name returns Some for any non-empty repository"),
    };

    Ok(PositionalPackage { binding, identifier })
}

/// Compose the final tool set from selected groups and positional packages.
///
/// Pure function — no I/O. Lock load + staleness checks happen at the CLI
/// boundary; this function trusts the caller to have validated those.
///
/// Pipeline (matches plan §4 resolution order):
///
/// 1. Build initial set from `groups` × `lock.tools` (deduplicating group
///    names, preserving first-seen order).
/// 2. Duplicate-across-groups check: same binding in two groups → error.
/// 3. Apply positional overrides: right-most wins; matching binding replaces
///    the entry, non-matching adds a fresh entry. Positionals also dedup
///    among themselves with right-most-wins.
///
/// `config` is currently unused; reserved for the platform-availability
/// filtering hook in plan §4 step 5 (deferred until `ProjectConfig` carries
/// per-tool platform metadata).
pub fn compose_tool_set(
    _config: &ProjectConfig,
    lock: Option<&ProjectLock>,
    groups: &[String],
    positionals: &[PositionalPackage],
) -> Result<Vec<ResolvedTool>, super::Error> {
    let mut resolved: Vec<ResolvedTool> = Vec::new();

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
            // Step 2: duplicate-across-groups check. Identical content
            // (eq_content ignores advisory tag) in another group → collapse
            // silently. Different content → error.
            if let Some(existing) = resolved.iter().find(|r| r.binding == entry.name) {
                let from_other_group = matches!(&existing.origin, Origin::Group(g) if g != raw);
                if from_other_group {
                    let same_content = existing.identifier.digest() == Some(entry.pinned.digest())
                        && existing.identifier.registry() == entry.pinned.registry()
                        && existing.identifier.repository() == entry.pinned.repository();
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
            resolved.push(ResolvedTool {
                binding: entry.name.clone(),
                identifier: entry.pinned.clone().into(),
                origin: Origin::Group(raw.clone()),
            });
        }
    }

    // Step 3: positional overrides (right-most wins, including among
    // positionals themselves).
    for pos in positionals {
        if let Some(existing) = resolved.iter_mut().find(|r| r.binding == pos.binding) {
            existing.identifier = pos.identifier.clone();
            existing.origin = Origin::Explicit;
        } else {
            resolved.push(ResolvedTool {
                binding: pos.binding.clone(),
                identifier: pos.identifier.clone(),
                origin: Origin::Explicit,
            });
        }
    }

    Ok(resolved)
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
    use crate::oci::{Digest, Identifier, PinnedIdentifier};
    use crate::project::lock::{LockMetadata, LockVersion, LockedTool, ProjectLock};
    use std::collections::BTreeMap;

    fn sha(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    fn pin(reg: &str, repo: &str, tag: Option<&str>, c: char) -> PinnedIdentifier {
        let mut id = Identifier::new_registry(repo, reg);
        if let Some(t) = tag {
            id = id.clone_with_tag(t);
        }
        let id = id.clone_with_digest(Digest::Sha256(sha(c)));
        PinnedIdentifier::try_from(id).expect("digest present")
    }

    fn lock_with(tools: Vec<LockedTool>) -> ProjectLock {
        ProjectLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", sha('0')),
                generated_by: "ocx test".into(),
                generated_at: "2026-04-24T00:00:00Z".into(),
            },
            tools,
        }
    }

    fn cfg() -> ProjectConfig {
        ProjectConfig {
            tools: BTreeMap::new(),
            groups: BTreeMap::new(),
        }
    }

    fn locked(name: &str, group: &str, pinned: PinnedIdentifier) -> LockedTool {
        LockedTool {
            name: name.into(),
            group: group.into(),
            pinned,
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
            locked("cmake", "default", pin("ocx.sh", "cmake", Some("3.28"), 'a')),
            locked("ninja", "default", pin("ocx.sh", "ninja", Some("1.11"), 'b')),
        ]);
        let out = compose_tool_set(&cfg(), Some(&lock), &["default".into()], &[]).expect("ok");
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|r| r.binding == "cmake"));
        assert!(out.iter().any(|r| r.binding == "ninja"));
        for r in &out {
            assert_eq!(r.origin, Origin::Group("default".into()));
        }
    }

    #[test]
    fn compose_dedups_repeated_group_names() {
        let lock = lock_with(vec![locked("cmake", "default", pin("ocx.sh", "cmake", None, 'a'))]);
        let out = compose_tool_set(
            &cfg(),
            Some(&lock),
            &["default".into(), "default".into(), "default".into()],
            &[],
        )
        .expect("ok");
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn compose_unions_multiple_groups() {
        let lock = lock_with(vec![
            locked("cmake", "default", pin("ocx.sh", "cmake", None, 'a')),
            locked("shellcheck", "ci", pin("ocx.sh", "shellcheck", None, 'b')),
            locked("shfmt", "ci", pin("ocx.sh", "shfmt", None, 'c')),
        ]);
        let out = compose_tool_set(&cfg(), Some(&lock), &["default".into(), "ci".into()], &[]).expect("ok");
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn compose_errors_on_duplicate_binding_across_groups_with_different_content() {
        let lock = lock_with(vec![
            locked("shellcheck", "ci", pin("ocx.sh", "shellcheck", Some("0.10"), 'a')),
            locked("shellcheck", "lint", pin("ocx.sh", "shellcheck", Some("0.11"), 'b')),
        ]);
        let err = compose_tool_set(&cfg(), Some(&lock), &["ci".into(), "lint".into()], &[]).expect_err("conflict");
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
        // Two groups define the same binding name with the *same* pinned digest
        // — collapse silently to one entry, no error.
        let lock = lock_with(vec![
            locked("shellcheck", "ci", pin("ocx.sh", "shellcheck", None, 'a')),
            locked("shellcheck", "lint", pin("ocx.sh", "shellcheck", None, 'a')),
        ]);
        let out = compose_tool_set(&cfg(), Some(&lock), &["ci".into(), "lint".into()], &[]).expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].binding, "shellcheck");
        // First-seen group wins for origin attribution.
        assert_eq!(out[0].origin, Origin::Group("ci".into()));
    }

    #[test]
    fn compose_lock_missing_when_group_selected_without_lock() {
        let err = compose_tool_set(&cfg(), None, &["default".into()], &[]).expect_err("no lock");
        let crate::project::Error::Project(pe) = err;
        assert!(matches!(pe.kind, ProjectErrorKind::LockMissing));
    }

    // ── compose: positionals ────────────────────────────────────────────

    #[test]
    fn compose_positionals_only_with_no_lock_ok() {
        let pos = parse_positional("ocx.sh/cmake:3.28", "ocx.sh").expect("ok");
        let out = compose_tool_set(&cfg(), None, &[], std::slice::from_ref(&pos)).expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].binding, "cmake");
        assert_eq!(out[0].origin, Origin::Explicit);
        assert_eq!(out[0].identifier.tag(), Some("3.28"));
    }

    #[test]
    fn compose_positional_overrides_group_entry_by_inferred_binding() {
        // default has cmake@a; positional `cmake:3.29` infers binding `cmake`
        // and overrides.
        let lock = lock_with(vec![locked(
            "cmake",
            "default",
            pin("ocx.sh", "cmake", Some("3.28"), 'a'),
        )]);
        let pos = parse_positional("cmake:3.29", "ocx.sh").expect("ok");
        let out = compose_tool_set(&cfg(), Some(&lock), &["default".into()], std::slice::from_ref(&pos)).expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].binding, "cmake");
        assert_eq!(out[0].origin, Origin::Explicit);
        assert_eq!(out[0].identifier.tag(), Some("3.29"));
    }

    #[test]
    fn compose_positional_explicit_binding_wins_over_repo_basename() {
        // default has `dotnet = "ocx.sh/microsoft-dotnet-sdk:9"`; positional
        // `dotnet=ocx.sh/microsoft-dotnet-sdk:10` overrides via name= prefix
        // (binding does NOT match repo basename).
        let lock = lock_with(vec![locked(
            "dotnet",
            "default",
            pin("ocx.sh", "microsoft-dotnet-sdk", Some("9"), 'a'),
        )]);
        let pos = parse_positional("dotnet=ocx.sh/microsoft-dotnet-sdk:10", "ocx.sh").expect("ok");
        let out = compose_tool_set(&cfg(), Some(&lock), &["default".into()], std::slice::from_ref(&pos)).expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].binding, "dotnet");
        assert_eq!(out[0].identifier.tag(), Some("10"));
        assert_eq!(out[0].origin, Origin::Explicit);
    }

    #[test]
    fn compose_positional_with_different_binding_adds_fresh_entry() {
        // default has `terraform = "ocx.sh/opentofu:1.7"`; positional
        // `opentofu=ocx.sh/opentofu:1.7` has different binding name so both
        // entries kept.
        let lock = lock_with(vec![locked(
            "terraform",
            "default",
            pin("ocx.sh", "opentofu", Some("1.7"), 'a'),
        )]);
        let pos = parse_positional("opentofu=ocx.sh/opentofu:1.7", "ocx.sh").expect("ok");
        let out = compose_tool_set(&cfg(), Some(&lock), &["default".into()], std::slice::from_ref(&pos)).expect("ok");
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|r| r.binding == "terraform"));
        assert!(out.iter().any(|r| r.binding == "opentofu"));
    }

    #[test]
    fn compose_positionals_right_most_wins() {
        // Two positionals share inferred binding `cmake`. Right-most must win.
        let p1 = parse_positional("cmake:3.28", "ocx.sh").expect("ok");
        let p2 = parse_positional("cmake:3.29", "ocx.sh").expect("ok");
        let out = compose_tool_set(&cfg(), None, &[], &[p1, p2]).expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].identifier.tag(), Some("3.29"));
    }
}
