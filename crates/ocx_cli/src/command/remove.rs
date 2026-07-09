// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx remove <identifier>...` — drop one or more bindings from
//! `ocx.toml`, rewrite `ocx.lock` for the affected groups, and uninstall
//! the tools.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::project::{ResolveLockOptions, remove_binding_in_memory, resolve_lock, resolve_lock_touched};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::project_context::load_project_for_mutate;

/// Remove one or more tool bindings from `ocx.toml`.
///
/// Searches the implicit default `[tools]` table and all named groups for
/// each binding whose key or identifier matches the given identifiers,
/// removes them, rewrites `ocx.lock` with every surviving entry carried
/// forward unchanged, and uninstalls the tools. If any identifier matches
/// no binding, the whole command fails and `ocx.toml` is left untouched.
///
/// Removing a binding never re-resolves the surviving tools: their pins
/// are preserved exactly. Fails with exit 65 when `ocx.toml` drifted from
/// `ocx.lock` before this remove (run `ocx lock` to reconcile), or exit 78
/// when a survivor's legacy entry can no longer be migrated exactly (run
/// `ocx update`).
///
/// When the same binding name exists in multiple groups, use `--group` to
/// target a specific group. `--group default` targets the implicit
/// `[tools]` table; `--group <name>` targets a named `[group.<name>]` table.
///
/// Fails if no matching binding is found in the targeted group (or any
/// group when `--group` is absent).
#[derive(Parser, Clone)]
pub struct Remove {
    /// Identifiers of the tools to remove (binding name or fully-qualified
    /// identifier, e.g. `cmake` or `ocx.sh/cmake:3.28`).
    #[arg(required = true, num_args = 1.., value_name = "IDENTIFIER")]
    pub identifiers: Vec<String>,

    /// Target a specific group. Use `default` to target the implicit
    /// `[tools]` table, or a named group (e.g. `ci`) to target
    /// `[group.ci]`. Without this flag, all groups are searched; if the
    /// binding appears in more than one group an error is returned.
    #[arg(long = "group", short = 'g', value_name = "NAME")]
    pub group: Option<String>,
}

impl Remove {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Validate the group name early — before flock acquisition — so a
        // typo doesn't lock out other writers waiting on `ocx.toml`.
        if let Some(ref g) = self.group
            && g != "default"
        {
            let valid = !g.is_empty() && g.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_');
            if !valid {
                return Err(anyhow::anyhow!(
                    "invalid group name '{}': must be non-empty and contain only alphanumeric characters, '-', or '_'",
                    g
                ));
            }
        }

        // Acquire flock + load snapshot + predecessor. Errors propagate to
        // the `main.rs` boundary (logged + classified there).
        let guard = load_project_for_mutate(&context).await?;

        // For each identifier, derive the binding key + look up the live
        // identifier from the pre-mutation snapshot so we can uninstall it
        // after the commit. `remove_ids` carries one synthetic identifier per
        // input (the in-memory remover only consults the repo basename);
        // `install_identifiers` collects only the bindings that were live so
        // the post-commit uninstall targets exactly those.
        let mut remove_ids: Vec<ocx_lib::oci::Identifier> = Vec::with_capacity(self.identifiers.len());
        let mut install_identifiers: Vec<ocx_lib::oci::Identifier> = Vec::new();

        for raw in &self.identifiers {
            let binding_key = if raw.contains('/') {
                match ocx_lib::oci::Identifier::parse_with_default_registry(raw, context.default_registry()) {
                    Ok(id) => ocx_lib::project::binding_key(&id),
                    Err(_) => raw.rsplit('/').next().unwrap_or(raw).to_owned(),
                }
            } else {
                raw.split_once(':')
                    .map(|(k, _)| k.to_owned())
                    .unwrap_or_else(|| raw.clone())
            };

            let install_identifier = match self.group.as_deref() {
                Some("default") => guard.config().tools.get(&binding_key).cloned(),
                Some(g) => guard
                    .config()
                    .groups
                    .get(g)
                    .and_then(|grp| grp.get(&binding_key))
                    .cloned(),
                None => guard
                    .config()
                    .tools
                    .get(&binding_key)
                    .or_else(|| guard.config().groups.values().find_map(|g| g.get(&binding_key)))
                    .cloned(),
            };

            // Build the synthetic identifier the in-memory remover requires.
            // remove_binding_in_memory only consults the binding key (repo
            // basename), so either the live identifier or a parse of the
            // user-supplied string suffices.
            let dummy_id = ocx_lib::oci::Identifier::parse_with_default_registry(raw, context.default_registry());
            let remove_id = match &install_identifier {
                Some(id) => id.clone(),
                None => match dummy_id {
                    Ok(id) => id,
                    Err(_) => ocx_lib::oci::Identifier::new_registry(&binding_key, context.default_registry()),
                },
            };

            if let Some(id) = install_identifier {
                install_identifiers.push(id);
            }
            remove_ids.push(remove_id);
        }

        // Stage: in-memory remove of every identifier on a clone of the
        // snapshot. A missing binding surfaces as `BindingNotFound` inside the
        // closure, aborting before any disk write — nothing is removed unless
        // every identifier matches.
        let config_path = guard.config_path().to_path_buf();
        let group = self.group.clone();
        let staged = guard.stage(move |cfg| {
            for remove_id in &remove_ids {
                remove_binding_in_memory(cfg, &config_path, remove_id, group.as_deref())?;
            }
            Ok(())
        })?;

        // Whole-file model (spec §4.4): `remove` re-resolves NOTHING. Every
        // survivor is carried forward verbatim (V2 byte-identical; V1 via
        // exact-only pinned-index transcribe), and the dropped binding falls
        // out of the carry-forward because it is no longer declared in the
        // candidate config. The freshness gate anchors on the pre-mutation
        // snapshot (`guard.config()`, which still contains the removed binding)
        // so a clean lock passes; drift surfaces as `StaleLockOnPartial` (65,
        // run `ocx lock`) and a survivor's gone V1 index as `LockUpgradeRequired`
        // (78, run `ocx update`). Both propagate to the `main.rs` boundary.
        let new_lock = match guard.previous_lock().cloned() {
            Some(prev) => {
                resolve_lock_touched(
                    staged.config(), // candidate — removed binding already absent
                    guard.config(),  // pre-mutation snapshot — freshness anchor
                    &prev,
                    context.default_index(),
                    &[], // EMPTY touched set — resolve nothing
                    ResolveLockOptions::default(),
                )
                .await?
            }
            // No predecessor lock to preserve — resolve the post-mutation
            // config directly (never fails closed).
            None => {
                resolve_lock(
                    staged.config(),
                    context.default_index(),
                    &[],
                    ResolveLockOptions::default(),
                )
                .await?
            }
        };

        let commit = guard.commit(staged, new_lock.clone()).await?;
        let _ = commit;

        // Best-effort uninstall after commit. Tools may not be installed
        // (lock-only workflow); errors here do not roll back. One batched
        // `uninstall_all` call over every live binding.
        if !install_identifiers.is_empty() {
            let _ = context
                .manager()
                .uninstall_all(&install_identifiers, false, false)
                .await;

            // Global-tier symmetry: `ocx --global add` sets the `current`
            // selection so the offline login exporter (`ocx --global env`)
            // sees the tool; `ocx --global remove` must clear it so the
            // exporter stops showing a tool that left the global toolchain.
            // `current` is keyed by registry/repository only — strip the
            // tag/digest (matches `resolve_global_current_env`'s lookup and
            // `deselect`'s tag-less requirement). Project tier never touches
            // `current` (resolves via the lock), so this is `--global`-only.
            if context.global() {
                let current_keys: Vec<_> = install_identifiers
                    .iter()
                    .map(|ident| ident.without_specifiers())
                    .collect();
                let _ = context.manager().deselect_all(&current_keys).await;
            }
        }

        let host = ocx_lib::oci::Platform::current().unwrap_or_else(ocx_lib::oci::Platform::any);
        let entries: Vec<LockEntry> = new_lock.tools.iter().map(|t| LockEntry::from_tool(t, &host)).collect();
        let report = LockReport::new(entries);
        context.api().report(&report)?;

        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single positional still parses (back-compat with the pre-plural form).
    #[test]
    fn parse_single_identifier() {
        let remove = Remove::try_parse_from(["remove", "tool"]).unwrap();
        assert_eq!(remove.identifiers, vec!["tool".to_string()]);
    }

    /// Multiple positionals are all captured, in order.
    #[test]
    fn parse_multiple_identifiers() {
        let remove = Remove::try_parse_from(["remove", "a", "b", "c"]).unwrap();
        assert_eq!(
            remove.identifiers,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    /// `num_args=1..` rejects zero positionals.
    #[test]
    fn parse_zero_identifiers_is_error() {
        assert!(
            Remove::try_parse_from(["remove"]).is_err(),
            "remove with no identifier must fail"
        );
    }
}
