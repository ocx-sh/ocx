// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashSet;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli;
use ocx_lib::project::{
    ALL_GROUP, DEFAULT_GROUP, LockedTool, ProjectConfig, ProjectLock, ResolveLockOptions, expand_all_keyword,
    resolutions_content_equal, resolve_lock, resolve_lock_touched,
};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::CommandError;
use crate::app::project_context::{load_project_for_mutate, materialize_lock};
use crate::options;

/// Arguments for `ocx update` (whole-file or scoped lock re-resolve).
///
/// The user-facing command overview renders from the `Command::Update`
/// variant doc in `command.rs` — clap uses the variant doc as the subcommand
/// `about`, so a top-level `///` here would be rustdoc-only. Per-mode detail
/// lives on the `check` / `groups` / `names` argument docs below (those clap
/// *does* render).
#[derive(Parser, Clone)]
pub struct Update {
    /// Verify the candidate lock would match the predecessor and exit.
    ///
    /// Re-resolves the selected scope (every declared tag, or only the
    /// bindings named by `-g`/positional names), compares the candidate to the
    /// predecessor, and exits 0 (matches) or 65 (`DataError`, a pin would
    /// change). No writes, no commit. When the predecessor lock is absent,
    /// exits 78 (`ConfigError`).
    #[arg(long = "check", default_value_t = false)]
    pub check: bool,

    /// Materialize packages into the object store after writing the lock (default).
    ///
    /// `--pull` is the affirmative form of the default behavior; `--no-pull`
    /// opts out. Both flags use POSIX last-wins semantics (`overrides_with`):
    /// `--pull --no-pull` resolves to no-pull; `--no-pull --pull` resolves
    /// to pull. Combining the flags is not an error - git `--[no-]verify`
    /// idiom.
    #[arg(long, overrides_with = "no_pull")]
    pub pull: bool,

    /// Write the lock without downloading. Materialization is deferred to
    /// `ocx pull` or first `ocx run` / direnv hit. Useful for CI flows that
    /// batch lock changes and materialize separately.
    #[arg(long = "no-pull", overrides_with = "pull")]
    pub no_pull: bool,

    #[clap(flatten)]
    pub platforms: options::Platforms,

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
        // Errors propagate to the `main.rs` boundary (logged + classified).
        let guard = load_project_for_mutate(&context).await?;

        // Stage as a lock-only mutation; the candidate config is
        // byte-identical to the snapshot.
        let staged = guard.stage(|_cfg| Ok(()))?.lock_only();

        // Update-family routing: resolve tags live against the registry by
        // default, capped by `--offline`/`--frozen`, and never commit tag
        // pointers into the shared local index — `ocx.lock` is the canonical
        // record (`adr_toolchain_update_family.md`).
        let update_index = context.update_index();
        let resolve_index = &update_index;

        // A `-g/--group` or a positional binding name switches update from
        // the whole-file bump to a scoped bump: only the named bindings
        // re-resolve, everything else is carried forward verbatim.
        let scoped = !self.groups.is_empty() || !self.names.is_empty();

        let new_lock = if scoped {
            // Scoped mode carries untouched pins forward, so it needs a
            // predecessor `ocx.lock` — there is nothing to freeze without one.
            // Checked before any resolve so a missing lock is deterministic
            // (exit 78) and never masked by a registry failure.
            let Some(previous) = guard.previous_lock().cloned() else {
                return Err(missing_lock(guard.lock_path()).into());
            };

            // Validate the `-g` / name selection and resolve it to concrete
            // `(group, binding)` pairs. Unknown group / name -> exit 64.
            let touched = select_touched(guard.config(), &self.groups, &self.names)?;

            // Re-resolve exactly the touched bindings against the live index
            // and carry every other pin forward verbatim (V2 byte-identical /
            // V1 exact-transcribe). Only the explicitly named pairs re-resolve
            // (no laundering); untouched entries never live-re-resolve (no
            // drift). This is a lock-only op, so the candidate config and the
            // pre-mutation snapshot are the same value (`staged.config()` ==
            // `guard.config()`). The freshness gate inside `resolve_lock_touched`
            // refuses a drifted `ocx.toml` (exit 65) before any resolve.
            resolve_lock_touched(
                staged.config(),
                guard.config(),
                &previous,
                resolve_index,
                &touched,
                ResolveLockOptions::default(),
            )
            .await?
        } else {
            // ── --check missing-predecessor gate (spec §4.5) ───────────────
            //
            // For `--check`, verify a predecessor lock exists BEFORE the
            // whole-file re-resolve. With no lock, the re-resolve would still
            // hit the registry — a registry/auth/policy failure would then mask
            // the intended exit 78 (and waste a network round-trip). Checking
            // first makes "no lock to verify against" return ConfigError (78)
            // deterministically, with no network attempted.
            if self.check && guard.previous_lock().is_none() {
                return Err(missing_lock(guard.lock_path()).into());
            }

            // Whole-file bump (spec §4.5): bare `resolve_lock(&[])` re-resolves
            // every declared tag. There is no subset, no carry-forward, nothing
            // untouched — laundering and drift are impossible by construction.
            resolve_lock(staged.config(), resolve_index, &[], ResolveLockOptions::default()).await?
        };

        // ── --check verify-only path (both modes) ──────────────────────
        //
        // `--check` performs the re-resolve above and exits without writing:
        // 0 when the candidate matches the predecessor, 65 when any pinned
        // content would change (an advisory tag moved upstream). The
        // missing-predecessor case (78) is already handled above — the scoped
        // branch requires a predecessor unconditionally, the whole-file branch
        // gates `--check` on it — so a predecessor is guaranteed here.
        if self.check {
            let prev = guard
                .previous_lock()
                .expect("missing-predecessor gate guarantees a predecessor lock for --check");
            if !lock_content_matches(&new_lock, prev) {
                return Err(CommandError::new(
                    "ocx.lock candidate would change pinned content; \
                     re-run `ocx update` (without --check) to refresh the lock",
                    cli::ExitCode::DataError,
                )
                .into());
            }
            return Ok(ExitCode::SUCCESS);
        }

        let commit = guard.commit(staged, new_lock.clone()).await?;
        let _ = commit;

        // Best-effort materialization AFTER the commit lands. A failure here
        // does not roll back the lock — the declaration is committed; only
        // the object-store population is deferred. Matches `add` semantics.
        // The `--check` early-return above ensures this line is never reached
        // on the verify-only path. `--no-pull` opts out.
        let eager = !self.no_pull;
        materialize_lock(&context, &new_lock, eager, self.platforms.as_slice()).await?;

        let report_platform = crate::app::project_context::primary_platform(self.platforms.as_slice());
        let entries: Vec<LockEntry> = new_lock
            .tools
            .iter()
            .map(|t| LockEntry::from_tool(t, &report_platform))
            .collect();
        let report = LockReport::new(entries);
        context.api().report(&report)?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Build the "no predecessor `ocx.lock`" error shared by the scoped gate and
/// the whole-file `--check` gate — both require an existing lock (to carry
/// untouched pins forward from, or to verify the candidate against).
fn missing_lock(lock_path: &std::path::Path) -> CommandError {
    CommandError::new(
        format!(
            "ocx.lock not found at {}; run `ocx lock` to create it",
            lock_path.display()
        ),
        cli::ExitCode::ConfigError,
    )
}

/// Resolve the `-g/--group` + positional-name selection into the concrete
/// `(group, binding)` pairs a scoped `ocx update` must re-resolve.
///
/// Group scope: an empty `groups` means every group (the top-level `[tools]`
/// table plus every `[group.*]`); a non-empty `groups` restricts to exactly
/// those, with `all` expanding to the full set. Within that scope, an empty
/// `names` selects every binding (the `-g GROUP` form); a non-empty `names`
/// selects only the matching bindings. A binding name present in several
/// in-scope groups is advanced in each of them (each against its own declared
/// tag) — deliberate, no ambiguity error, unlike `ocx run`'s compose path.
///
/// The config map keys are the binding names, so no `binding_key` derivation
/// is needed here.
///
/// # Errors
///
/// Returns a [`CommandError`] classified [`cli::ExitCode::UsageError`] (exit
/// 64) when a requested group is unknown (mirroring `ocx run`) or a requested
/// name matches no binding in scope.
fn select_touched(
    config: &ProjectConfig,
    groups: &[String],
    names: &[String],
) -> Result<Vec<(String, String)>, CommandError> {
    let usage = |message: String| CommandError::new(message, cli::ExitCode::UsageError);

    // Validate + expand the group filter. `None` = every group. `default` and
    // `all` are always valid reserved keywords; any other name must be a
    // declared `[group.*]`.
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
    // `None` name filter = every binding in scope; `Some` = only these names.
    let name_filter: Option<HashSet<&str>> = (!names.is_empty()).then(|| names.iter().map(String::as_str).collect());
    let selected = |binding: &str| name_filter.as_ref().is_none_or(|f| f.contains(binding));

    // Iterate the config structure once (each group visited once), so a
    // duplicate group in `scope` never double-counts a binding. The
    // deterministic order (default group first, then named groups
    // alphabetically) matches the resolver's own ordering.
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
    for (group, tools) in &config.groups {
        if !in_scope(group) {
            continue;
        }
        for binding in tools.keys() {
            if selected(binding) {
                touched.push((group.clone(), binding.clone()));
                matched.insert(binding.clone());
            }
        }
    }

    // Every explicitly requested name must have matched at least one in-scope
    // binding — otherwise the user named a binding that does not exist (or is
    // outside the `-g` scope). Mirrors `ocx run`'s unknown-name usage error.
    for name in names {
        if !matched.contains(name) {
            return Err(usage(format!("binding '{name}' not found in the selected groups")));
        }
    }

    Ok(touched)
}

/// Resolved-content equality: two locks match when they share the same
/// `(group, name, pinned content)` tuples and the same load-bearing
/// metadata (declaration_hash, lock_version, declaration_hash_version).
/// Advisory metadata (`generated_at`, `generated_by`) is ignored.
///
/// Used by the `update --check` verify-only path: a candidate that
/// resolves to a different digest for any entry must surface as
/// `DataError` (exit 65) without writing.
fn lock_content_matches(candidate: &ProjectLock, prev: &ProjectLock) -> bool {
    if candidate.metadata.declaration_hash != prev.metadata.declaration_hash
        || candidate.metadata.declaration_hash_version != prev.metadata.declaration_hash_version
        || candidate.metadata.lock_version != prev.metadata.lock_version
    {
        return false;
    }
    if candidate.tools.len() != prev.tools.len() {
        return false;
    }
    let mut a: Vec<&LockedTool> = candidate.tools.iter().collect();
    let mut b: Vec<&LockedTool> = prev.tools.iter().collect();
    a.sort_by(|x, y| (x.group.as_str(), x.name.as_str()).cmp(&(y.group.as_str(), y.name.as_str())));
    b.sort_by(|x, y| (x.group.as_str(), x.name.as_str()).cmp(&(y.group.as_str(), y.name.as_str())));
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| x.name == y.name && x.group == y.group && resolutions_content_equal(&x.resolution, &y.resolution))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn parse(args: &[&str]) -> Update {
        Update::try_parse_from(args).unwrap()
    }

    fn eager(update: &Update) -> bool {
        !update.no_pull
    }

    // ── cases ─────────────────────────────────────────────────────────────────

    /// Neither `--pull` nor `--no-pull` → both fields false; default is eager.
    #[test]
    fn parse_no_flags_defaults_to_eager() {
        let update = parse(&["update"]);
        assert!(!update.pull, "pull must be false when neither flag is given");
        assert!(!update.no_pull, "no_pull must be false when neither flag is given");
        assert!(eager(&update), "default must be eager (eager = !no_pull)");
    }

    /// `--pull` alone → pull=true, no_pull=false; still eager.
    #[test]
    fn parse_only_pull_is_eager() {
        let update = parse(&["update", "--pull"]);
        assert!(update.pull, "pull must be true with --pull");
        assert!(!update.no_pull, "no_pull must be false with --pull only");
        assert!(eager(&update), "eager must be true when --pull is the last flag");
    }

    /// `--no-pull` alone → pull=false, no_pull=true; lazy.
    #[test]
    fn parse_only_no_pull_is_lazy() {
        let update = parse(&["update", "--no-pull"]);
        assert!(!update.pull, "pull must be false with --no-pull only");
        assert!(update.no_pull, "no_pull must be true with --no-pull");
        assert!(!eager(&update), "eager must be false when --no-pull is set");
    }

    /// `--pull --no-pull` → POSIX last-wins: no_pull wins, pull=false; lazy.
    #[test]
    fn parse_pull_then_no_pull_no_pull_wins() {
        let update = parse(&["update", "--pull", "--no-pull"]);
        assert!(update.no_pull, "no_pull must be true when --no-pull follows --pull");
        assert!(!update.pull, "pull must be false when --no-pull overrides it");
        assert!(!eager(&update), "eager must be false when --no-pull wins");
    }

    /// `--no-pull --pull` → POSIX last-wins: pull wins, no_pull=false; eager.
    #[test]
    fn parse_no_pull_then_pull_pull_wins() {
        let update = parse(&["update", "--no-pull", "--pull"]);
        assert!(update.pull, "pull must be true when --pull follows --no-pull");
        assert!(!update.no_pull, "no_pull must be false when --pull overrides it");
        assert!(eager(&update), "eager must be true when --pull wins");
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
        use ocx_lib::oci::Identifier;
        use std::collections::BTreeMap;
        let id = |repo: &str| Identifier::new_registry(repo, "ocx.sh");
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

    fn exit_code(err: &CommandError) -> Option<cli::ExitCode> {
        use ocx_lib::cli::ClassifyExitCode as _;
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
        assert_eq!(exit_code(&err), Some(cli::ExitCode::UsageError));
    }

    /// A name outside the `-g` scope is unknown (exit 64): `fd` exists in the
    /// default group but not in `ci`.
    #[test]
    fn select_touched_name_outside_scope_errors() {
        let err = select_touched(&sample_config(), &["ci".to_string()], &["fd".to_string()]).expect_err("out of scope");
        assert_eq!(exit_code(&err), Some(cli::ExitCode::UsageError));
    }

    /// An unknown group is a usage error (exit 64).
    #[test]
    fn select_touched_unknown_group_errors() {
        let err = select_touched(&sample_config(), &["ghost".to_string()], &[]).expect_err("unknown group");
        assert_eq!(exit_code(&err), Some(cli::ExitCode::UsageError));
    }

    /// `--platform` is repeatable and parses into the flattened `Platforms`.
    #[test]
    fn parses_repeatable_platform_flag() {
        let update = parse(&["update", "--platform", "linux/arm64", "-p", "linux/amd64"]);
        assert_eq!(
            update.platforms.as_slice().len(),
            2,
            "two --platform values must parse into two entries"
        );
    }
}
