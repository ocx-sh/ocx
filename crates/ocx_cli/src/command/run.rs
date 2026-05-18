// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project-tier `ocx run` command.
//!
//! `ocx run` is the project-tier counterpart to the OCI-tier `ocx exec`.
//! Symbols are binding names from `ocx.toml`, not OCI identifiers. The
//! command resolves the requested bindings through `ocx.lock` (digest-pinned),
//! composes the child environment from the resolved packages, and execs the
//! given `ARGV` in that environment — mirroring `ocx exec`'s child-spawn
//! mechanics but driven entirely by the project toolchain declaration.
//!
//! # NOTE: clap floor
//!
//! The `value_terminator = "--"` on `names` combined with `last = true` on
//! `argv` requires clap ≥ 4.5.57. clap 4.5.55 introduced a regression in
//! this combination; 4.5.57 fixed it. The floor is set in `Cargo.toml`.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::env;
use ocx_lib::project::{ALL_GROUP, DEFAULT_GROUP, Origin, ResolvedTool, compose_tool_set, expand_all_keyword};
use ocx_lib::utility::child_process;

use crate::app::project_context::load_project_with_lock;
use crate::conventions::platforms_or_default;

/// Run a command with the composed environment from the project toolchain.
///
/// Loads the nearest `ocx.toml` together with its sibling `ocx.lock`, selects
/// the tool bindings in the requested groups, composes their environment, and
/// execs `ARGV` with that environment.
///
/// `--` is mandatory: everything before `--` is a binding name filter; everything
/// after is the command and arguments forwarded to the child process unchanged.
///
/// # Composition order
///
/// Group-selection order (the order of `-g` flags after `all` expansion,
/// deduplicated); then alphabetical by binding name within each group
/// (lock-file order).
#[derive(Parser, Clone)]
pub struct Run {
    /// Restrict the env composition to the named group(s).
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The
    /// reserved name `default` selects the top-level `[tools]` table.
    /// The reserved name `all` expands to `default` + every declared
    /// `[group.*]`. When omitted, scope is exactly `[tools]`
    /// (matches `ocx pull` precedent: omitted `-g` does NOT mean
    /// "everything"; it means "the default group").
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    pub groups: Vec<String>,

    /// Start with a clean environment containing only the package
    /// variables, instead of inheriting the current shell environment.
    #[arg(long = "clean", default_value_t = false)]
    pub clean: bool,

    /// Expose each package's full env, including its private (self-only)
    /// entries. See `ocx exec --self` for the cross-cutting flag contract.
    #[arg(long = "self", default_value_t = false)]
    pub self_view: bool,

    /// Binding names to compose into the child env. Each name must
    /// resolve unambiguously inside the selected scope. Empty list means
    /// "every binding in scope."
    ///
    /// `value_terminator = "--"` so clap stops collecting names at the
    /// mandatory `--` separator without trying to interpret subsequent
    /// hyphen-prefixed argv as more names.
    #[arg(num_args = 0.., value_terminator = "--")]
    pub names: Vec<String>,

    /// Command to execute, with arguments. The command runs with the
    /// composed package env. `--` is mandatory and at least one argv
    /// token is required (`required = true` + `num_args = 1..`).
    ///
    /// `allow_hyphen_values = true` so flag-prefixed argv like
    /// `--format json` is forwarded to the child unchanged. `last = true`
    /// makes clap parse everything before the first `--` into `names`
    /// and everything after into `argv`. `required = true` ensures
    /// clap rejects `ocx run` / `ocx run NAME` / `ocx run NAME --` with
    /// a usage error (exit 2) instead of letting an empty argv slip
    /// through to a runtime panic on `split_first`.
    #[arg(allow_hyphen_values = true, last = true, num_args = 1.., required = true)]
    pub argv: Vec<String>,
}

impl Run {
    /// Execute the `ocx run` command.
    ///
    /// # Behavior
    ///
    /// Resolves the project context (ocx.toml + ocx.lock), expands `-g all`
    /// to the full group union, calls `compose_tool_set` with the expanded
    /// group list, filters the composed set to the requested `names`, and
    /// execs `argv` with the resulting package environment. Exit code is
    /// forwarded byte-for-byte from the child process on success.
    ///
    /// Composition order: group-selection order (the order of `-g` flags
    /// after `all` expansion, deduplicated), then alphabetical by binding
    /// name within each group (lock-file order).
    ///
    /// # Errors
    ///
    /// - Exit 64 (`UsageError`): no `ocx.toml` found, unknown group, unknown
    ///   or ambiguous binding name, empty `-g` segment.
    /// - Exit 78 (`ConfigError`): `ocx.lock` absent.
    /// - Exit 65 (`DataError`): `ocx.lock` stale (hash mismatch).
    /// - Other exit codes from package-manager / registry errors forwarded
    ///   via the existing `ClassifyExitCode` chain.
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        use ocx_lib::cli;

        // ── Phase A: parse-time validation ───────────────────────────────

        // Reject empty comma segments (`-g ci,,lint`) BEFORE any filesystem
        // or network work. `clap`'s `value_delimiter = ','` splits the value
        // into `["ci", "", "lint"]`; an empty string is a user-typing error.
        for raw in &self.groups {
            if raw.is_empty() {
                return Err(cli::UsageError::new("empty group segment in --group value").into());
            }
        }

        // ── Phase B: project context ──────────────────────────────────────
        // Errors propagate to the `main.rs` boundary: logged once and
        // classified by `app::classify_error` from `ProjectContextError`'s
        // `ClassifyExitCode` impl (NoProject→64, LockMissing→78, StaleLock→65).
        let ctx = load_project_with_lock(&context).await?;

        // Phase B.3: validate `-g` groups against the loaded config.
        // `default` and `all` are always valid (all is expanded later).
        // Anything else must appear in config.groups.
        for raw in &self.groups {
            if raw == DEFAULT_GROUP || raw == ALL_GROUP {
                continue;
            }
            if !ctx.config.groups.contains_key(raw) {
                return Err(cli::UsageError::new(format!("unknown group '{raw}' in --group filter")).into());
            }
        }

        // ── Phase C: `all` expansion + default scope ───────────────────────

        let mut expanded = expand_all_keyword(&self.groups, &ctx.config);
        // Default scope: if groups is empty (no -g flags) or expansion produced
        // an empty list, scope = [DEFAULT_GROUP] — matches pull semantics.
        if expanded.is_empty() {
            expanded = vec![DEFAULT_GROUP.to_owned()];
        }

        // ── Phase D: compose_tool_set ─────────────────────────────────────

        let composed = compose_tool_set(&ctx.config, Some(&ctx.lock), &expanded, &[])?;

        // ── Phase E: NAME filter ──────────────────────────────────────────

        let filtered = match filter_by_names(composed, &self.names) {
            Ok(v) => v,
            Err(RunFilterError::Unknown { name }) => {
                return Err(cli::UsageError::new(format!("binding '{name}' not found in selected groups")).into());
            }
            Err(RunFilterError::Ambiguous { name, groups }) => {
                let groups_str = groups.join(", ");
                return Err(cli::UsageError::new(format!(
                    "binding '{name}' exists in multiple selected groups: [{groups_str}]; pass `-g <group>` to narrow scope"
                ))
                .into());
            }
        };

        // ── Phase F: install + env compose ───────────────────────────────

        let manager = context.manager();
        let platforms = platforms_or_default(&[]);

        let identifiers: Vec<_> = filtered.iter().map(|r| r.identifier.clone()).collect();
        let infos = manager
            .find_or_install_all(identifiers, platforms, context.concurrency())
            .await?;
        let install_infos: Vec<std::sync::Arc<ocx_lib::package::install_info::InstallInfo>> =
            infos.into_iter().map(std::sync::Arc::new).collect();
        let entries = manager.resolve_env(&install_infos, self.self_view).await?;

        // ── Phase G: spawn child ──────────────────────────────────────────

        let mut process_env = if self.clean { env::Env::clean() } else { env::Env::new() };
        process_env.apply_entries(&entries);
        // Forward the running ocx's resolution-affecting config (binary path,
        // offline/remote, config file, index) to any child ocx (e.g. through
        // a generated entrypoint launcher). Runs after `Env::clean()` /
        // `Env::new()` so the outer ocx's parsed state is the sole authority
        // for `OCX_*` keys on the child env — no ambient parent-shell export
        // can override it.
        process_env.apply_ocx_config(context.config_view());
        // No PATHEXT manipulation: the Windows launcher is now a native
        // `<name>.exe` shim and `.EXE` is unconditionally in the default
        // Windows PATHEXT, so the child resolves it via the OS default.

        // clap enforces `last = true, num_args = 1.., required = true` on the
        // `argv` field — `self.argv` is always non-empty at this point.
        let (command, args) = self
            .argv
            .split_first()
            .expect("clap last=true + num_args=1.. + required=true guarantees non-empty argv");

        let resolved = process_env.resolve_command(command);

        // Replace this process with the child on Unix (PID inherited via
        // `execvp(2)`); on Windows spawn+wait then `process::exit`, since
        // `CreateProcess` has no exec equivalent. Either way the helper
        // diverges on success — only start-up failures fall through to
        // the error-wrapping path below.
        let err = child_process::exec(&resolved, args, process_env);
        Err(anyhow::Error::from(err).context(format!("failed to run '{}'", resolved.display())))
    }
}

/// Errors from the CLI-layer NAME filter applied after `compose_tool_set`.
///
/// Both variants correspond to exit 64 (`UsageError`) — the user named a
/// binding that is either absent from or ambiguous within the composed set.
/// Private to this module; the command maps these to `eprintln` + return
/// before surfacing them as `anyhow::Error`.
#[derive(Debug, thiserror::Error)]
enum RunFilterError {
    /// The requested binding name was not found in any tool in the composed set.
    #[error("binding '{name}' not found in selected groups")]
    Unknown { name: String },

    /// The requested binding name matched entries in two or more selected
    /// groups (defense-in-depth — not reachable through normal flow in v1;
    /// see plan §NAME Filter for rationale).
    #[error("binding '{name}' exists in multiple selected groups: {groups:?}; pass `-g <group>` to narrow scope")]
    Ambiguous { name: String, groups: Vec<String> },
}

/// Filter the composed tool set to the explicitly-requested binding names.
///
/// When `names` is empty, the full `composed` set is returned unchanged —
/// every binding in scope participates.
///
/// When `names` is non-empty, user-supplied name order wins: the output
/// preserves the order of `names`, not the order of `composed`. Duplicate
/// names in `names` are silently deduplicated (same binding enumerated twice
/// is not a usage error).
///
/// # Errors
///
/// - [`RunFilterError::Unknown`] — a requested name has no match in `composed`.
/// - [`RunFilterError::Ambiguous`] — a requested name matches multiple entries
///   (defense-in-depth; not reachable through normal v1 flow — see plan §NAME Filter).
fn filter_by_names(composed: Vec<ResolvedTool>, names: &[String]) -> Result<Vec<ResolvedTool>, RunFilterError> {
    if names.is_empty() {
        return Ok(composed);
    }

    // Build a `binding -> Vec<index_into_composed>` lookup once. Replaces the
    // previous O(N·M) `composed.iter().filter` scan with an O(1) probe per
    // user-supplied name. The hits are stored as indices so we can move out
    // of `composed` without reborrowing during the user-order walk below.
    let mut hits_by_binding: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::with_capacity(composed.len());
    for (i, tool) in composed.iter().enumerate() {
        hits_by_binding.entry(tool.binding.as_str()).or_default().push(i);
    }

    // Iterate names in user order. Dedup by tracking seen names.
    let mut out = Vec::with_capacity(names.len());
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for name in names {
        if !seen.insert(name.as_str()) {
            // Duplicate name — silently skip.
            continue;
        }
        let hit_indices = hits_by_binding.get(name.as_str()).map(Vec::as_slice).unwrap_or(&[]);
        match hit_indices {
            [] => return Err(RunFilterError::Unknown { name: name.clone() }),
            [single] => out.push(composed[*single].clone()),
            [_, _, ..] => {
                let groups: Vec<String> = hit_indices
                    .iter()
                    .filter_map(|&i| match &composed[i].origin {
                        Origin::Group(g) => Some(g.clone()),
                        Origin::Explicit => None,
                    })
                    .collect();
                return Err(RunFilterError::Ambiguous {
                    name: name.clone(),
                    groups,
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_lib::oci::{Digest, Identifier, PinnedIdentifier};

    // ── helpers ──────────────────────────────────────────────────────────────

    fn sha(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    fn pin(repo: &str, tag: Option<&str>, c: char) -> PinnedIdentifier {
        let mut id = Identifier::new_registry(repo, "ocx.sh");
        if let Some(t) = tag {
            id = id.clone_with_tag(t);
        }
        let id = id.clone_with_digest(Digest::Sha256(sha(c)));
        PinnedIdentifier::try_from(id).expect("digest present")
    }

    fn tool(binding: &str, c: char, group: &str) -> ResolvedTool {
        ResolvedTool {
            binding: binding.into(),
            identifier: pin(binding, None, c).into(),
            origin: Origin::Group(group.into()),
        }
    }

    // ── filter_by_names ──────────────────────────────────────────────────────

    /// Plan §Phase 3.1: `filter_empty_names_returns_full_set`
    ///
    /// When names is empty every binding in scope participates.
    #[test]
    fn filter_empty_names_returns_full_set() {
        let composed = vec![tool("cmake", 'a', "default"), tool("ninja", 'b', "default")];
        let result = filter_by_names(composed.clone(), &[]).expect("empty names must succeed");
        assert_eq!(result.len(), composed.len());
        assert!(result.iter().any(|r| r.binding == "cmake"));
        assert!(result.iter().any(|r| r.binding == "ninja"));
    }

    /// Plan §Phase 3.1: `filter_unknown_name_errors`
    ///
    /// A name with no match in the composed set → `RunFilterError::Unknown`.
    #[test]
    fn filter_unknown_name_errors() {
        let composed = vec![tool("cmake", 'a', "default")];
        let err = filter_by_names(composed, &["does-not-exist".into()]).expect_err("unknown name must fail");
        assert!(
            matches!(&err, RunFilterError::Unknown { name } if name == "does-not-exist"),
            "expected Unknown {{ name: \"does-not-exist\" }}; got: {err}"
        );
    }

    /// Plan §Phase 3.1: `filter_ambiguous_name_errors_with_groups_listed`
    ///
    /// Synthetic composed set: two entries with same binding but different
    /// group origins (defense-in-depth — not reachable through normal v1 flow).
    #[test]
    fn filter_ambiguous_name_errors_with_groups_listed() {
        let composed = vec![
            ResolvedTool {
                binding: "tool".into(),
                identifier: pin("tool", None, 'a').into(),
                origin: Origin::Group("ci".into()),
            },
            ResolvedTool {
                binding: "tool".into(),
                identifier: pin("tool", None, 'b').into(),
                origin: Origin::Group("release".into()),
            },
        ];
        let err = filter_by_names(composed, &["tool".into()]).expect_err("ambiguous name must fail");
        let RunFilterError::Ambiguous { name, groups } = &err else {
            panic!("expected Ambiguous; got: {err}");
        };
        assert_eq!(name, "tool");
        assert!(
            groups.contains(&"ci".to_string()),
            "groups must contain 'ci'; got: {groups:?}"
        );
        assert!(
            groups.contains(&"release".to_string()),
            "groups must contain 'release'; got: {groups:?}"
        );
    }

    /// Plan §Phase 3.1: `filter_unique_name_picks_single_entry`
    ///
    /// Happy path: exactly one matching entry is returned.
    #[test]
    fn filter_unique_name_picks_single_entry() {
        let composed = vec![tool("cmake", 'a', "default"), tool("ninja", 'b', "default")];
        let result = filter_by_names(composed, &["cmake".into()]).expect("ok");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].binding, "cmake");
    }

    /// Plan §Phase 3.1: `filter_preserves_compose_order` — actually user order wins.
    ///
    /// When names is non-empty the output preserves user-supplied name order,
    /// NOT compose order. Composed `[a, b, c]`, names `[b, a]` → output `[b, a]`.
    #[test]
    fn filter_preserves_user_name_order_not_compose_order() {
        let composed = vec![
            tool("a", 'a', "default"),
            tool("b", 'b', "default"),
            tool("c", 'c', "default"),
        ];
        let names: Vec<String> = vec!["b".into(), "a".into()];
        let result = filter_by_names(composed, &names).expect("ok");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].binding, "b", "user-order: b must come first");
        assert_eq!(result[1].binding, "a", "user-order: a must come second");
    }

    /// Plan §Phase 3.1: `filter_duplicate_names_dedupe`
    ///
    /// Duplicate names in the names list are silently deduplicated.
    /// Composed has one `cmake`; names `[cmake, cmake]` → output `[cmake]` once.
    #[test]
    fn filter_duplicate_names_deduplicated_silently() {
        let composed = vec![tool("cmake", 'a', "default")];
        let names: Vec<String> = vec!["cmake".into(), "cmake".into()];
        let result = filter_by_names(composed, &names).expect("dedup must succeed");
        assert_eq!(result.len(), 1, "duplicate name must be silently deduped to one entry");
        assert_eq!(result[0].binding, "cmake");
    }
}
