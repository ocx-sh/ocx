// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use crate::{api, conventions::*, options};
use clap::Parser;
use ocx_lib::shell::Shell;

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
    platform: options::PlatformOption,

    #[clap(flatten)]
    content_path: options::ContentPath,

    /// Package identifiers to resolve the environment for.
    #[clap(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,

    /// Target shell for eval-safe export lines.
    ///
    /// Must be supplied with `=` (`--shell=bash`).  Bare `--shell` (no `=`)
    /// triggers autodetection from `$SHELL`/parent process; exit 64 if
    /// undetectable.
    ///
    /// When absent, output uses the context-level format (root `--format` flag;
    /// default plain). Use `ocx --format json package env` for JSON.
    /// `--shell=sh` is an alias for `--shell=dash` (POSIX strict).
    #[arg(
        long,
        value_enum,
        value_name = "SHELL",
        num_args = 0..=1,
        require_equals = true
    )]
    shell: Option<Option<Shell>>,

    /// Write the composed environment into a CI system's persistence channel.
    ///
    /// `--ci=github` appends tool dirs and vars to `$GITHUB_PATH` /
    /// `$GITHUB_ENV`; `--ci=gitlab` writes JSON-lines to `--export-file` (or
    /// stdout). Bare `--ci` autodetects the provider from CI environment
    /// variables; exit 64 if none is detected. Must be supplied with `=`
    /// (`--ci=github`).
    ///
    /// Unlike `--shell` (which affects only the current step), the CI channel
    /// makes the environment available to later pipeline steps. Conflicts with
    /// `--shell`.
    #[arg(long, value_enum, value_name = "PROVIDER", num_args = 0..=1, require_equals = true, conflicts_with = "shell")]
    ci: Option<Option<ocx_lib::ci::CiFlavor>>,

    /// Write the GitLab export to this file instead of stdout.
    ///
    /// Requires `--ci`. Rejected for `--ci=github`, which infers its sink from
    /// `$GITHUB_PATH` / `$GITHUB_ENV`. Point this at GitLab's export file.
    #[arg(long, value_name = "PATH", requires = "ci")]
    export_file: Option<std::path::PathBuf>,

    /// Annotate each entry with its origin package or companion identifier.
    ///
    /// When `[patches]` is configured, companion overlay entries are appended
    /// after the package's own entries.  `--show-patches` adds a Source column
    /// to the plain table (or a `"source"` field in JSON) so the origin of each
    /// entry is visible.
    ///
    /// Has no effect when `[patches]` is not configured.  Cannot be combined
    /// with `--shell` or `--ci`; use the plain or JSON structured report instead.
    #[arg(long, default_value_t = false, conflicts_with = "shell", conflicts_with = "ci")]
    show_patches: bool,
}

impl Env {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Resolve `--ci` early so a bare-`--ci` autodetect failure surfaces as a
        // usage error before the (potentially slow) find-or-install resolution.
        // `--ci` is mutually exclusive with `--shell` (clap `conflicts_with`).
        let ci = resolve_ci_arg(self.ci)?;

        let platform = platform_or_default(self.platform.platform.clone());
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        let manager = context.manager();

        let info = if let Some(kind) = self.content_path.symlink_kind() {
            manager.find_symlink_all(identifiers, kind).await?
        } else {
            manager
                .find_or_install_all(identifiers, platform, context.concurrency())
                .await?
        };

        let info: Vec<std::sync::Arc<ocx_lib::package::install_info::InstallInfo>> =
            info.into_iter().map(std::sync::Arc::new).collect();
        // `resolve_env_with_attribution` additionally surfaces the admitted-set
        // `binaries`/`entrypoints` claim attribution for the structured report's
        // `binaries`/`entrypoints` arrays; the patch boundary is used the same
        // way `resolve_env_with_patch_boundary` used it (annotate `--show-patches`
        // entries). For `--ci`/`--shell` output the extra data is simply unused.
        //
        // OCI-tier (`ocx package env`): no `ocx.toml`, so no per-package opt-out.
        let (entries, patch_start, provenance, attribution) = manager
            .resolve_env_with_attribution(
                &info,
                self.self_view,
                ocx_lib::package_manager::PatchScope::NoProjectContext,
            )
            .await?;
        // `--ci=<provider>` → CI sink path (persists env for later pipeline
        // steps). Branch BEFORE consuming `entries` via `into_iter()`.
        if let Some(provider) = ci {
            export_ci(provider, self.export_file.clone(), &entries)?;
            return Ok(ExitCode::SUCCESS);
        }
        // `--shell[=NAME]` → eval-safe emit path (handshake §3, C5).
        // Shared bare-shell autodetect + identical UsageError (conventions).
        // Branch BEFORE consuming `entries` via `into_iter()`.
        if let Some(shell) = resolve_shell_arg(self.shell)? {
            emit_lines(shell, &entries);
            return Ok(ExitCode::SUCCESS);
        }

        let all_entries: Vec<api::data::env::EnvEntry> = entries
            .into_iter()
            .enumerate()
            .map(|(i, e)| {
                // Annotate origin when `--show-patches` is enabled. Entries at index
                // `>= patch_start` came from companion overlay projections; the aligned
                // `provenance` vec names the rule glob + companion for each.
                let source = if self.show_patches && i >= patch_start {
                    let prov = &provenance[i - patch_start];
                    Some(api::data::env::EntrySource::Patch {
                        rule: prov.rule_match.clone(),
                        companion: prov.companion.to_string(),
                    })
                } else {
                    None
                };
                api::data::env::EnvEntry {
                    key: e.key,
                    value: e.value,
                    kind: e.kind,
                    source,
                }
            })
            .collect();

        // No synthetic PATHEXT entry: the Windows launcher is now a native
        // `<name>.exe` shim, and `.EXE` is unconditionally in the default
        // Windows PATHEXT — nothing to inject for bare-name resolution.

        if !context.api().is_json() {
            ocx_lib::log::warn!("default output is not eval-safe; use --shell=bash to activate");
        }

        let binaries = api::data::env::BinaryAttribution::from_pairs(&attribution.binaries);
        let entrypoints = api::data::env::BinaryAttribution::from_pairs(&attribution.entrypoints);

        // Structured report. Format is a context-level concern (root
        // `--format`); this command does not override it.
        context
            .api()
            .report(&api::data::env::EnvVars::new(all_entries, binaries, entrypoints))?;

        Ok(ExitCode::SUCCESS)
    }
}
