// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{BTreeMap, HashSet};
use std::process::ExitCode;

use clap::Parser;
use futures::StreamExt;
use ocx_project::{
    ALL_GROUP, DEFAULT_GROUP, ProjectConfig, ResolveLockOptions, expand_all_keyword, resolve_lock, resolve_lock_touched,
};

use crate::api::data::update::{UpdateReport, VerboseUpdateReport, VersionKey};
use crate::app::CommandError;
use crate::app::project_context::{load_project_for_mutate, materialize_lock, record_activation_consent};
use crate::conventions;
use crate::options;

/// Arguments for `ocx update`; its help text lives on `Command::Update`.
#[derive(Parser, Clone)]
pub struct Update {
    /// Verify the candidate lock would match the predecessor and exit.
    ///
    /// Re-resolves the selected scope (every declared tag, or only the
    /// bindings named by `-g`/positional names), prints the bindings that
    /// would move, and exits 0 (nothing would move) or 65 (`DataError`, a pin
    /// would change). No writes, no commit. When the predecessor lock is
    /// absent, exits 78 (`ConfigError`).
    #[arg(long = "check", default_value_t = false)]
    pub check: bool,

    /// List the bindings that did not move as well as the ones that did.
    ///
    /// Affects the plain rendering only. The structured report
    /// (`ocx --format json update`) carries every field either way.
    #[arg(short, long)]
    pub verbose: bool,

    #[clap(flatten)]
    pub pull: options::Pull,

    #[clap(flatten)]
    pub platform: options::PlatformOption,

    /// Advance every binding in the named group(s); freeze the rest.
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The reserved
    /// name `default` selects the top-level `[tools]` table; `all` expands to
    /// `default` plus every declared `[group.*]`. Combine with binding names
    /// to advance only those bindings within the named groups. Omit both this
    /// flag and any names to re-resolve the whole file.
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    pub groups: Vec<String>,

    /// Binding names to advance; freeze every other pin.
    ///
    /// Each name is the `ocx.toml` binding key and is advanced in every group
    /// it appears in (narrow with `-g`). Advancing moves the declared tag to
    /// today's resolution - it does not change the declaration; edit `ocx.toml`
    /// to pin a new explicit version. Scoped mode needs an existing `ocx.lock`
    /// (exit 78 when absent) and refuses a drifted `ocx.toml` (exit 65). An
    /// unknown name exits 64.
    #[arg(num_args = 0..)]
    pub names: Vec<String>,
}

impl Update {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let guard = load_project_for_mutate(&context).await?;

        let staged = guard.stage(|_cfg| Ok(()))?.lock_only();

        // Cloned up front: `commit_and_render` consumes the guard, and the report needs both locks.
        let previous = guard.previous_lock().cloned();

        // Resolves live and never commits tag pointers to the shared local index (`adr_toolchain_update_family.md`).
        let update_index = context.update_index();
        let resolve_index = &update_index;

        let scoped = !self.groups.is_empty() || !self.names.is_empty();

        // `examined` narrows the report's `unchanged` list, so a carried-forward pin is never reported as checked.
        let (new_lock, examined) = if scoped {
            // Checked before any resolve, or a registry failure masks the exit 78.
            let Some(previous) = previous.as_ref() else {
                return Err(missing_lock(guard.lock_path()).into());
            };

            let touched = select_touched(guard.config(), &self.groups, &self.names)?;

            let lock = resolve_lock_touched(
                staged.config(),
                guard.config(),
                previous,
                resolve_index,
                &touched,
                ResolveLockOptions::default(),
            )
            .await?;
            (lock, Some(touched))
        } else {
            // Before the re-resolve, or a registry failure masks the exit 78.
            if self.check && previous.is_none() {
                return Err(missing_lock(guard.lock_path()).into());
            }

            let lock = resolve_lock(staged.config(), resolve_index, &[], ResolveLockOptions::default()).await?;
            (lock, None)
        };

        // Built while the guard still holds the declaration the tag column reads; the commit consumes it.
        let mut report = UpdateReport::diff(previous.as_ref(), &new_lock, guard.config(), examined.as_deref())?;
        // Offline and frozen runs read a snapshot that may lag the registry, so they report no version.
        if !context.is_offline() && !context.config_view().frozen {
            report.fill_versions(&concrete_versions(resolve_index, report.version_lookups()).await);
        }

        // Reported before the exit-65 diagnostic, so a refusal names what moved.
        if self.check {
            let verdict = report.summary(true);
            self.emit(&context, report)?;
            return Ok(match verdict {
                Some(line) => print_verdict(&line),
                None => ExitCode::SUCCESS,
            });
        }
        let summary = report.summary(false);

        // `-g`/NAME scope only the resolution, never the re-render; `--no-pull` renders from the offline view.
        let eager = self.pull.enabled(true);
        let render_manager = if eager {
            context.manager().clone()
        } else {
            context.manager().offline_view(context.local_index().clone())
        };
        let scope = context.toolchain_render_scope(guard.config_path()).await?;
        let platform = conventions::platform_or_default(self.platform.platform.clone());
        // Cloned before the commit consumes `staged`; the eager pull binds the new lock to it.
        let config = staged.config().clone();
        let commit = render_manager
            .commit_and_render(
                guard,
                staged,
                new_lock.clone(),
                ocx_package_manager::ToolchainRender {
                    scope: &scope,
                    toolchain_root: context.toolchain_root(),
                    platform: &platform,
                },
            )
            .await?
            .commit;

        // After the commit, so the stamp records the new source set.
        record_activation_consent(&commit.config_path, &new_lock, None).await;

        // After the commit, so a failure here never rolls back the lock.
        materialize_lock(&context, &new_lock, &config, eager, platform).await?;

        self.emit(&context, report)?;
        if let Some(line) = summary {
            context.ui().success(line);
        }

        Ok(ExitCode::SUCCESS)
    }

    /// One report, two renderings: `--verbose` appends the unmoved pins to plain output only.
    fn emit(&self, context: &crate::app::Context, report: UpdateReport) -> anyhow::Result<()> {
        if self.verbose {
            context.api().report(&VerboseUpdateReport(report))
        } else {
            context.api().report(&report)
        }
    }
}

/// A `--check` that found drift: `line` on stderr, then exit 65.
///
/// Printed, not raised as an error: drift is the answer `--check` was asked for, so it gets no log line.
// Unconditional, unlike `ui()`: in CI (non-interactive) this line is the only explanation of the 65.
pub(crate) fn print_verdict(line: &str) -> ExitCode {
    ocx_console::Printer::new(false, false).cerr().plain(line).end_line();
    ocx_exit::ExitCode::DataError.into()
}

/// Most releases one advisory tag lookup probes before it reports no version.
const VERSION_PROBES: usize = 8;

/// Most version lookups in flight at once.
const VERSION_LOOKUP_CONCURRENCY: usize = 8;

/// The concrete release behind each lookup ([`UpdateReport::version_lookups`]); a miss or a failed lookup is absent.
pub(crate) async fn concrete_versions(
    index: &ocx_index::Index,
    lookups: BTreeMap<VersionKey, BTreeMap<String, ocx_oci::Digest>>,
) -> BTreeMap<VersionKey, String> {
    futures::stream::iter(lookups)
        .map(|((pull, tag), leaves)| async move {
            let advisory = pull.clone_with_tag(tag.as_str());
            let found =
                ocx_package::concrete_version::resolve_concrete_version(index, &advisory, &leaves, VERSION_PROBES)
                    .await
                    // Best effort: a failed lookup reports no version and never changes the exit code.
                    .inspect_err(|error| tracing::debug!("version lookup for {advisory} failed: {error}"))
                    .ok()
                    .flatten();
            found.map(|version| ((pull, tag), version.to_string()))
        })
        .buffer_unordered(VERSION_LOOKUP_CONCURRENCY)
        .filter_map(std::future::ready)
        .collect()
        .await
}

/// The exit-78 error for a missing predecessor `ocx.lock`.
pub(crate) fn missing_lock(lock_path: &std::path::Path) -> CommandError {
    CommandError::new(
        format!(
            "ocx.lock not found at {}; run `ocx lock` to create it",
            lock_path.display()
        ),
        ocx_exit::ExitCode::ConfigError,
    )
}

/// Resolve the `-g` and name selection into the `(group, binding)` pairs a scoped `ocx update` re-resolves.
///
/// A name in several in-scope groups advances in each, unlike `ocx exec`'s ambiguity error.
///
/// # Errors
///
/// [`ocx_exit::ExitCode::UsageError`] for an unknown group or a name matching no binding in scope.
pub(crate) fn select_touched(
    config: &ProjectConfig,
    groups: &[String],
    names: &[String],
) -> Result<Vec<(String, String)>, CommandError> {
    let usage = |message: String| CommandError::new(message, ocx_exit::ExitCode::UsageError);

    let scope: Option<Vec<String>> = if groups.is_empty() {
        None
    } else {
        for raw in groups {
            if raw.is_empty() {
                return Err(usage(
                    "empty group segment in --group value; check for stray commas".to_string(),
                ));
            }
            if raw != DEFAULT_GROUP && raw != ALL_GROUP && !config.groups.contains_key(raw) {
                return Err(usage(format!("unknown group '{raw}' in --group filter")));
            }
        }
        Some(expand_all_keyword(groups, config))
    };

    let in_scope = |group: &str| scope.as_ref().is_none_or(|s| s.iter().any(|g| g == group));
    let name_filter: Option<HashSet<&str>> = (!names.is_empty()).then(|| names.iter().map(String::as_str).collect());
    let selected = |binding: &str| name_filter.as_ref().is_none_or(|f| f.contains(binding));

    // Each group visited once, so a duplicate `-g` never double-counts a binding.
    let mut touched: Vec<(String, String)> = Vec::new();
    let mut matched: HashSet<String> = HashSet::new();

    if in_scope(DEFAULT_GROUP) {
        for binding in config.tools.keys() {
            if selected(binding) {
                touched.push((DEFAULT_GROUP.to_string(), binding.clone()));
                matched.insert(binding.clone());
            }
        }
    }
    for (group, body) in &config.groups {
        if !in_scope(group) {
            continue;
        }
        for binding in body.tools.keys() {
            if selected(binding) {
                touched.push((group.clone(), binding.clone()));
                matched.insert(binding.clone());
            }
        }
    }

    for name in names {
        if !matched.contains(name) {
            return Err(usage(format!("binding '{name}' not found in the selected groups")));
        }
    }

    Ok(touched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn parse(args: &[&str]) -> Update {
        Update::try_parse_from(args).unwrap()
    }

    // ── cases ─────────────────────────────────────────────────────────────────

    /// `--pull`/`--no-pull` wire through the shared `options::Pull` flatten;
    /// `update` defaults to eager. The full flag matrix is tested on the
    /// flatten struct itself (`options/pull.rs`).
    #[test]
    fn pull_flags_flatten_with_eager_default() {
        assert!(parse(&["update"]).pull.enabled(true), "default must be eager");
        assert!(
            !parse(&["update", "--no-pull"]).pull.enabled(true),
            "--no-pull must defer"
        );
    }

    // ── Scoped surface: `-g` and positional names now parse ─────────────

    /// `ocx update --group ci` / `-g ci,lint` parse into `groups` (the flag
    /// is comma-splittable and repeatable).
    #[test]
    fn parses_group_flag() {
        let long = parse(&["update", "--group", "ci"]);
        assert_eq!(long.groups, vec!["ci".to_string()]);
        let short = parse(&["update", "-g", "ci,lint"]);
        assert_eq!(
            short.groups,
            vec!["ci".to_string(), "lint".to_string()],
            "comma-separated -g must split"
        );
    }

    /// `ocx update <binding>...` parses positional binding names.
    #[test]
    fn parses_positional_names() {
        let update = parse(&["update", "cmake", "ninja"]);
        assert_eq!(update.names, vec!["cmake".to_string(), "ninja".to_string()]);
        assert!(update.groups.is_empty());
    }

    /// `-g` and names combine (advance named bindings within named groups).
    #[test]
    fn parses_group_and_names() {
        let update = parse(&["update", "-g", "ci", "ripgrep"]);
        assert_eq!(update.groups, vec!["ci".to_string()]);
        assert_eq!(update.names, vec!["ripgrep".to_string()]);
    }

    // ── select_touched ──────────────────────────────────────────────────

    /// `ripgrep` in both `[tools]` (default) and `[group.ci]`; `fd` only in
    /// default; `cmake` only in `ci`.
    fn sample_config() -> ProjectConfig {
        use ocx_oci::PackageRef;
        use std::collections::BTreeMap;
        let id = |repo: &str| PackageRef::new_registry(repo, "ocx.sh");
        let tools = BTreeMap::from([("ripgrep".to_string(), id("ripgrep")), ("fd".to_string(), id("fd"))]);
        let ci = BTreeMap::from([
            ("ripgrep".to_string(), id("ripgrep")),
            ("cmake".to_string(), id("cmake")),
        ]);
        let groups = BTreeMap::from([("ci".to_string(), ci)]);
        ProjectConfig::from_parts(tools, groups)
    }

    fn pair(group: &str, name: &str) -> (String, String) {
        (group.to_string(), name.to_string())
    }

    fn exit_code(err: &CommandError) -> Option<ocx_exit::ExitCode> {
        use ocx_exit::ClassifyExitCode as _;
        err.classify()
    }

    /// A bare name advances the binding in every group it appears in.
    #[test]
    fn select_touched_by_name_spans_every_group() {
        let touched = select_touched(&sample_config(), &[], &["ripgrep".to_string()]).expect("known name");
        assert_eq!(touched.len(), 2, "ripgrep is declared in default + ci");
        assert!(touched.contains(&pair("default", "ripgrep")));
        assert!(touched.contains(&pair("ci", "ripgrep")));
        assert!(!touched.iter().any(|(_, n)| n == "fd"), "fd must be frozen");
    }

    /// `-g ci` advances every binding in the `ci` group and nothing else.
    #[test]
    fn select_touched_by_group_selects_whole_group() {
        let touched = select_touched(&sample_config(), &["ci".to_string()], &[]).expect("known group");
        assert_eq!(touched.len(), 2);
        assert!(touched.contains(&pair("ci", "cmake")));
        assert!(touched.contains(&pair("ci", "ripgrep")));
        assert!(
            !touched.iter().any(|(g, _)| g == "default"),
            "default tools must be frozen when scoped to ci"
        );
    }

    /// `-g ci ripgrep` intersects: only `ripgrep` within `ci`, not the
    /// default-group `ripgrep`.
    #[test]
    fn select_touched_name_within_group_intersects() {
        let touched =
            select_touched(&sample_config(), &["ci".to_string()], &["ripgrep".to_string()]).expect("known in group");
        assert_eq!(touched, vec![pair("ci", "ripgrep")]);
    }

    /// An unknown binding name is a usage error (exit 64).
    #[test]
    fn select_touched_unknown_name_errors() {
        let err = select_touched(&sample_config(), &[], &["nope".to_string()]).expect_err("unknown name");
        assert_eq!(exit_code(&err), Some(ocx_exit::ExitCode::UsageError));
    }

    /// A name outside the `-g` scope is unknown (exit 64): `fd` exists in the
    /// default group but not in `ci`.
    #[test]
    fn select_touched_name_outside_scope_errors() {
        let err = select_touched(&sample_config(), &["ci".to_string()], &["fd".to_string()]).expect_err("out of scope");
        assert_eq!(exit_code(&err), Some(ocx_exit::ExitCode::UsageError));
    }

    /// An unknown group is a usage error (exit 64).
    #[test]
    fn select_touched_unknown_group_errors() {
        let err = select_touched(&sample_config(), &["ghost".to_string()], &[]).expect_err("unknown group");
        assert_eq!(exit_code(&err), Some(ocx_exit::ExitCode::UsageError));
    }

    /// `--platform` accepts a single value.
    #[test]
    fn parses_platform_flag() {
        let update = parse(&["update", "--platform", "linux/arm64"]);
        assert_eq!(
            update.platform.platform.map(|p| p.to_string()),
            Some("linux/arm64".to_owned())
        );
    }

    /// A second `--platform` occurrence is a usage error, per the
    /// single-platform-authoring decision in `adr_platform_model_unification.md`.
    #[test]
    fn rejects_repeated_platform_flag() {
        assert!(
            Update::try_parse_from(["update", "--platform", "linux/arm64", "-p", "linux/amd64"]).is_err(),
            "repeated --platform must be rejected"
        );
    }
}
