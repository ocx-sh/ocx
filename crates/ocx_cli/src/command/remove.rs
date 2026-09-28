// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx remove <identifier>...` — drop one or more bindings from
//! `ocx.toml`, rewrite `ocx.lock` for the affected groups, and uninstall
//! the tools.

use std::process::ExitCode;

use clap::Parser;
use ocx_project::{ResolveLockOptions, remove_binding_in_memory, resolve_lock, resolve_lock_touched};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::project_context::{load_project_for_mutate, record_activation_consent};

/// Remove one or more package bindings from `ocx.toml`.
///
/// Each argument reduces to a binding name (`ocx.sh/cmake:3.28` matches
/// `cmake`; an explicitly named binding only by its name), searched in
/// `[tools]` and every group, or only in `--group <name>`. Matches are removed,
/// `ocx.lock` keeps every survivor's pin exactly, and their packages are
/// uninstalled. Any argument matching nothing fails the whole command with
/// `ocx.toml` untouched. Exits 65 when `ocx.toml` drifted from `ocx.lock`
/// (run `ocx lock`), 78 when a survivor's legacy entry can no longer be
/// migrated exactly (run `ocx update`).
#[derive(Parser, Clone)]
pub struct Remove {
    /// Bindings to remove (binding name or fully-qualified
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
        let guard = load_project_for_mutate(&context).await?;

        let mut remove_keys: Vec<String> = Vec::with_capacity(self.identifiers.len());
        let mut install_identifiers: Vec<ocx_oci::PackageRef> = Vec::new();

        for raw in &self.identifiers {
            let binding_key = if raw.contains('/') {
                match ocx_oci::PackageRef::parse_with_default_registry(raw, context.default_registry()) {
                    Ok(id) => ocx_project::binding_key(&id),
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
                    .and_then(|grp| grp.tools.get(&binding_key))
                    .cloned(),
                None => guard
                    .config()
                    .tools
                    .get(&binding_key)
                    .or_else(|| guard.config().groups.values().find_map(|g| g.tools.get(&binding_key)))
                    .cloned(),
            };

            if let Some(id) = install_identifier {
                install_identifiers.push(id);
            }
            remove_keys.push(binding_key);
        }

        // Any unmatched key aborts here, before a disk write, so nothing is removed unless all match.
        let config_path = guard.config_path().to_path_buf();
        let group = self.group.clone();
        let staged = guard.stage(move |cfg| {
            for key in &remove_keys {
                remove_binding_in_memory(cfg, &config_path, key, group.as_deref())?;
            }
            Ok(())
        })?;

        // Empty touched set: survivors keep their pins verbatim, and drift fails 65/78 instead of re-resolving.
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

        // A render failure never rolls the commit back, so `ocx.lock` can land with a stale trampoline.
        let scope = context.toolchain_render_scope(guard.config_path()).await?;
        let host = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);
        let commit = context
            .manager()
            .commit_and_render(
                guard,
                staged,
                new_lock.clone(),
                ocx_package_manager::ToolchainRender {
                    scope: &scope,
                    toolchain_root: context.toolchain_root(),
                    platform: &host,
                },
            )
            .await?
            .commit;

        // Stamped after the commit, or consent records the source set being replaced.
        record_activation_consent(&commit.config_path, &new_lock, None).await;

        // Best-effort: a lock-only workflow never installed them, and the commit is not rolled back.
        if !install_identifiers.is_empty() {
            let _ = context
                .manager()
                .uninstall_all(&install_identifiers, false, false)
                .await;

            // Keyed tag-less like `resolve_global_current_env`, or a `package select`-anchored `current`
            // outlives its binding.
            if context.global() {
                let current_keys: Vec<_> = install_identifiers
                    .iter()
                    .map(|ident| ident.without_specifiers())
                    .collect();
                let _ = context.manager().deselect_all(&current_keys).await;
            }
        }

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
