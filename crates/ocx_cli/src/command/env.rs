// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use crate::{api, conventions::*, options};
use clap::Parser;

/// Print the resolved environment variables for one or more installed packages.
///
/// Plain format: aligned table with Key, Value, and Type columns where Type is `constant` or `path`.
/// JSON format:  `{"entries": [{"key": "...", "value": "...", "type": "constant"|"path"}, ...]}`.
///
/// This allows external tools (Python scripts, Bazel rules, CI steps) to correctly
/// configure child process environments without going through `ocx exec`.
///
/// By default, env values are rooted in the content-addressed object store and
/// may change when a package is updated.  Use `--candidate` or `--current` to
/// root them in a stable symlink path instead — suitable for embedding in editor
/// or IDE configuration files that should not change on every package update.
/// See the path resolution modes documentation for details.
#[derive(Parser)]
pub struct Env {
    /// Expose the package's full env, including private (self-only) entries.
    /// See `ocx exec --help` for full view semantics.
    ///
    /// Generated launchers embed `--self`; avoid passing it directly unless
    /// building a launcher equivalent.
    #[clap(long = "self", default_value_t = false)]
    self_view: bool,

    #[clap(flatten)]
    platforms: options::Platforms,

    #[clap(flatten)]
    content_path: options::ContentPath,

    /// Package identifiers to resolve the environment for.
    #[clap(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Env {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let platforms = platforms_or_default(self.platforms.as_slice());
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        let manager = context.manager();

        let info = if let Some(kind) = self.content_path.symlink_kind() {
            manager.find_symlink_all(identifiers, kind).await?
        } else {
            manager
                .find_or_install_all(identifiers, platforms, context.concurrency())
                .await?
        };

        let info: Vec<std::sync::Arc<ocx_lib::package::install_info::InstallInfo>> =
            info.into_iter().map(std::sync::Arc::new).collect();
        let entries = manager.resolve_env(&info, self.self_view).await?;
        #[cfg_attr(not(target_os = "windows"), allow(unused_mut))]
        let mut all_entries: Vec<api::data::env::EnvEntry> = entries
            .into_iter()
            .map(|e| api::data::env::EnvEntry {
                key: e.key,
                value: e.value,
                kind: e.kind,
            })
            .collect();

        // On Windows, append a synthetic `PATHEXT ⊳ .CMD` entry to the env
        // output when the host shell's PATHEXT lacks `.cmd`. `ocx env` is the
        // shell-eval boundary — what we emit becomes the consumer's effective
        // env after `eval`. The host PATHEXT we read here drives the gate, not
        // whether the resolved env actually contains a launcher PATH entry:
        // false-positive cost (one extra exported segment) is lower than the
        // metadata-inspection cost we would pay to gate on launcher presence,
        // and a benign duplicate is collapsed by `launcher::includes_launcher`
        // on the consumer side.
        #[cfg(target_os = "windows")]
        {
            let current_pathext = std::env::var("PATHEXT").unwrap_or_default();
            if let Some(entry) = synthetic_pathext_entry(&current_pathext) {
                all_entries.push(entry);
            }
        }

        context.api().report(&api::data::env::EnvVars::new(all_entries))?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Returns a synthetic `PATHEXT ⊳ .CMD` env entry when the host PATHEXT does
/// not already list the launcher extension. Returns `None` otherwise so
/// `ocx env` does not emit a redundant entry for shells that already include
/// `.CMD`.
#[cfg(target_os = "windows")]
fn synthetic_pathext_entry(host_pathext: &str) -> Option<api::data::env::EnvEntry> {
    if ocx_lib::package_manager::launcher::includes_launcher(host_pathext) {
        return None;
    }
    Some(api::data::env::EnvEntry {
        key: "PATHEXT".to_string(),
        value: ocx_lib::package_manager::launcher::LAUNCHER_EXT.to_string(),
        kind: ocx_lib::package::metadata::env::modifier::ModifierKind::Path,
    })
}

#[cfg(all(test, target_os = "windows"))]
mod synthetic_pathext_tests {
    use super::synthetic_pathext_entry;

    #[test]
    fn returns_none_when_host_pathext_includes_cmd() {
        assert!(synthetic_pathext_entry(".EXE;.CMD;.BAT").is_none());
    }

    #[test]
    fn returns_none_for_lowercase_cmd() {
        assert!(synthetic_pathext_entry(".exe;.cmd").is_none());
    }

    #[test]
    fn returns_synthetic_entry_when_cmd_absent() {
        let entry = synthetic_pathext_entry(".EXE;.BAT").expect("synthetic entry expected");
        assert_eq!(entry.key, "PATHEXT");
        assert_eq!(entry.value, ".CMD");
    }

    #[test]
    fn returns_synthetic_entry_when_pathext_empty() {
        let entry = synthetic_pathext_entry("").expect("synthetic entry expected");
        assert_eq!(entry.key, "PATHEXT");
        assert_eq!(entry.value, ".CMD");
    }

    #[test]
    fn partial_match_still_emits_synthetic() {
        // ".cmdextra" must not silence the gate.
        assert!(synthetic_pathext_entry(".cmdextra;.EXE").is_some());
    }
}
