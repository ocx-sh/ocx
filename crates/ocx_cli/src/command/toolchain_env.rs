// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx env` — toolchain-tier composed-env command.

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use ocx_oci::Platform;
use ocx_package::metadata::env::apply::reconcile_list_separators;
use ocx_package::metadata::env::entry::Entry;
use ocx_package_manager::{
    AdmittedClaims, PatchProvenance,
    composer::{ComposeRequest, ComposeRoots, Materialization},
};
use ocx_project::{
    ALL_GROUP, DEFAULT_GROUP, Origin, ProjectLock, ResolvedTool, compose_tool_set, expand_all_keyword,
    lazy_mode_for_tool, lock::lock_path_for,
};

use crate::{
    api,
    app::project_context::load_project_with_lock,
    conventions::{emit_lines, export_ci, platform_or_default, resolve_ci_arg, resolve_shell_arg},
    options,
};

/// Emit the composed environment for the in-scope toolchain.
///
/// Reads `ocx.toml` + `ocx.lock` (CWD walk, `--project` or `OCX_PROJECT`), or
/// the global toolchain's offline `current` set under `--global`. The default
/// output is a report in the root `--format` and is not eval-safe;
/// `--shell[=NAME]` is the only sourceable form. Missing tools are installed
/// first unless `--no-pull`. The global tier never installs and is lenient (no
/// lock, a corrupt lock or an unknown group yields an empty env) except for a
/// patch fail-closed failure, which still exits non-zero.
/// Exit codes: <https://ocx.sh/docs/reference/command-line#env-root>
#[derive(Parser)]
pub struct ToolchainEnv {
    #[clap(flatten)]
    pub groups: options::GroupSelection,

    #[clap(flatten)]
    pub env: options::EnvOverride,

    /// Target shell for eval-safe export lines.
    ///
    /// Must be supplied with `=` (`--shell=bash`).  Bare `--shell` (no `=`)
    /// triggers autodetection from `$SHELL`/parent process; exit 64 if
    /// undetectable.
    ///
    /// `--shell=sh` is an alias for `--shell=dash` (POSIX strict).
    #[arg(
        long,
        value_enum,
        value_name = "SHELL",
        num_args = 0..=1,
        require_equals = true
    )]
    shell: Option<Option<ocx_shell::shell::Shell>>,

    /// Write the composed environment into a CI system's persistence channel.
    #[arg(long_help = "\
        Write the composed environment into a CI system's persistence channel.\n\n\
        `--ci=github` appends tool dirs and vars to `$GITHUB_PATH` / `$GITHUB_ENV`; `--ci=gitlab` \
        writes JSON-lines to `--export-file` (or stdout). Bare `--ci` autodetects the provider from \
        CI environment variables; exit 64 if none is detected. Must be supplied with `=` \
        (`--ci=github`).\n\n\
        Unlike `--shell` (which affects only the current step), the CI channel makes the \
        environment available to later pipeline steps. Conflicts with `--shell`.")]
    #[arg(long, value_enum, value_name = "PROVIDER", num_args = 0..=1, require_equals = true, conflicts_with = "shell")]
    ci: Option<Option<ocx_shell::ci::CiFlavor>>,

    /// Write the GitLab export to this file instead of stdout.
    ///
    /// Requires `--ci`. Rejected for `--ci=github`, which infers its sink from
    /// `$GITHUB_PATH` / `$GITHUB_ENV`. Point this at GitLab's export file.
    #[arg(long, value_name = "PATH", requires = "ci")]
    export_file: Option<std::path::PathBuf>,

    #[clap(flatten)]
    platform: options::PlatformOption,

    #[clap(flatten)]
    pull: options::Pull,

    /// Top tier of the `lazy-mode` ladder for every tool this command composes.
    ///
    /// `always` composes a tool as a generated shim: its declared names reach
    /// `PATH` immediately and its content downloads on first use. The shim
    /// directory sits *below* the tool's own `entrypoints/` and `bin/` in the
    /// composed `PATH`, so the same exported environment stops routing through
    /// it once the first invocation has materialized the package. Combined with
    /// `--no-pull`, a tool whose metadata is not already local is reported on
    /// stderr and omitted, exactly as a not-materialised tool is on the eager path.
    #[clap(flatten)]
    lazy_mode: options::LazyMode,

    /// Top tier of the `pinned` ladder for the environment this command
    /// composes.
    #[clap(flatten)]
    pinned: options::Pinned,

    /// Annotate each entry with its origin package or companion identifier.
    ///
    /// When `[patches]` is configured, companion overlay entries are appended
    /// after the toolchain's own entries.  `--show-patches` adds a Source column
    /// to the plain table (or a `"source"` field in JSON) so the origin of each
    /// entry is visible.
    ///
    /// Has no effect when `[patches]` is not configured.  Cannot be combined
    /// with `--shell` or `--ci`; use the plain or JSON structured report instead.
    #[arg(long, default_value_t = false, conflicts_with = "shell", conflicts_with = "ci")]
    show_patches: bool,
}

impl ToolchainEnv {
    /// The materialization policy for a tool whose `lazy-mode` resolved to `never`; the global tier never installs.
    fn materialization(&self) -> Materialization {
        if self.pull.enabled(true) {
            Materialization::Install
        } else {
            Materialization::LocalOnly
        }
    }

    /// Reports the deferred tools' omissions and advisories on stderr, omissions in the eager `--no-pull` shape.
    fn report_deferred(&self, context: &crate::app::Context, composed: &ComposeRoots, tools: &[ResolvedTool]) {
        for omission in &composed.omitted {
            // Named by binding, as the user reads `ocx.toml`.
            let name = tools
                .iter()
                .find(|tool| tool.identifier == omission.identifier)
                .map_or_else(|| omission.identifier.to_string(), |tool| tool.binding.clone());
            context.ui().warn(format!(
                "{name} not installed; run `ocx pull` to fetch or drop --no-pull"
            ));
        }
        for advisory in &composed.advisories {
            context.ui().warn(advisory.to_string());
        }
    }

    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        crate::app::project_context::ensure_group_segments_nonempty(self.groups.names())?;

        // Made absolute here, so an emitted export line means the same wherever it is evaluated.
        let cwd = std::env::current_dir()
            .map_err(|error| anyhow::Error::from(error).context("failed to read the current directory"))?;
        let env_overrides = self.env.entries(&cwd)?;

        let shell = resolve_shell_arg(self.shell)?;
        // Early, so a bare-`--ci` autodetect failure beats the slow entry resolution.
        let ci = resolve_ci_arg(self.ci)?;

        let target = platform_or_default(self.platform.platform.clone());

        // Kept for the structured report: a channel that only reaches a log is not a channel.
        let mut advisories: Vec<ocx_package_manager::LazyAdvisory> = Vec::new();

        // ── Resolve entries: one global path, one project path ───────────────
        // `patch_start` marks where companion-overlay entries begin, for `--show-patches` on both tiers.
        let (mut entries, patch_start, provenance, attribution) = if context.global() {
            match resolve_global_pinned_env(
                &context,
                &target,
                self.groups.names(),
                &env_overrides,
                self.pinned.pinned(),
            )
            .await
            {
                Ok(Some((entries, patch_start, provenance, claims))) => (entries, patch_start, provenance, claims),
                Ok(None) => (Vec::new(), 0, Vec::new(), AdmittedClaims::default()),
                Err(error) => return Err(error),
            }
        } else {
            let ctx = load_project_with_lock(&context).await?;

            crate::app::project_context::ensure_groups_known(self.groups.names(), &ctx.config)?;

            let mut expanded = expand_all_keyword(self.groups.names(), &ctx.config);
            if expanded.is_empty() {
                expanded = vec![DEFAULT_GROUP.to_owned()];
            }

            let composed = compose_tool_set(&ctx.config, Some(&ctx.lock), &expanded, &[], &target)?;

            // `--no-pull` composes through an offline clone, so a miss is warned about and omitted, never fetched.
            let manager = context.manager();
            let materialization = self.materialization();
            let requests: Vec<ComposeRequest> = composed
                .iter()
                .map(|tool| ComposeRequest {
                    identifier: tool.identifier.clone(),
                    mode: lazy_mode_for_tool(
                        &ctx.config,
                        &tool.identifier,
                        group_of(&tool.origin),
                        self.lazy_mode.mode(),
                    ),
                })
                .collect();
            let composing = match materialization {
                Materialization::LocalOnly => manager.offline_view(context.local_index().clone()),
                _ => manager.clone(),
            };
            let roots = composing
                .compose_roots(&requests, &target, materialization, context.concurrency())
                .await?;
            self.report_deferred(&context, &roots, &composed);
            advisories = roots.advisories;
            let infos: Vec<Arc<ocx_package::install_info::InstallInfo>> = roots.roots;
            // Assembled exactly as `ocx exec` assembles them: what this prints is what `ocx exec` applies.
            let mut project_env = ocx_project::project_env_entries(&ctx.config, &ctx.config_path, &expanded);
            project_env.extend(env_overrides);
            // Built as `ocx exec` builds it, or `env` and `exec` diverge in the link lane.
            let toolchain = crate::app::project_context::toolchain_links(
                &context,
                &ctx.config_path,
                &ctx.config,
                &ctx.lock,
                &expanded,
                self.pinned.pinned(),
            )
            .await?;
            let scope = ocx_package_manager::EnvScope::Project {
                no_patches: ctx.config.no_patches_repositories(),
                env: project_env,
                toolchain: Some(Box::new(toolchain)),
            };
            manager
                .resolve_env_with_attribution(&infos, false, scope, &target)
                .await?
        };

        // Settle `list` separators before any emit branch reads `entries`.
        reconcile_list_separators(entries.iter_mut())?;

        // ── Emit ─────────────────────────────────────────────────────────────
        if let Some(provider) = ci {
            // An explicit `--ci=github` outside GitHub Actions fails here: global leniency covers resolution only.
            export_ci(provider, self.export_file.clone(), &entries)?;
            return Ok(ExitCode::SUCCESS);
        }

        if let Some(s) = shell {
            emit_lines(s, &entries);
            return Ok(ExitCode::SUCCESS);
        }

        // The overlay is the middle region, so the bound-checked accessor keeps project entries unlabelled.
        let overlay = ocx_package_manager::PatchOverlay::new(patch_start, &provenance);
        let env_data: Vec<api::data::env::EnvEntry> = entries
            .into_iter()
            .enumerate()
            .map(|(i, e)| {
                let source = self
                    .show_patches
                    .then(|| overlay.provenance_for(i))
                    .flatten()
                    .map(|prov| api::data::env::EntrySource::Patch {
                        rule: prov.rule_match.clone(),
                        companion: prov.companion.to_string(),
                    });
                api::data::env::EnvEntry {
                    key: e.key,
                    value: e.value,
                    kind: e.kind,
                    separator: e.separator,
                    source,
                }
            })
            .collect();

        if let Some(advisory) = crate::conventions::not_eval_safe_advisory(
            context.api().is_json(),
            std::io::IsTerminal::is_terminal(&std::io::stdout()),
        ) {
            log::warn!("{advisory}");
        }

        let binaries = api::data::env::BinaryAttribution::from_pairs(&attribution.binaries);
        let entrypoints = api::data::env::BinaryAttribution::from_pairs(&attribution.entrypoints);
        let integrations = api::data::env::IntegrationAttribution::from_pairs(&attribution.integrations);

        context.api().report(
            &api::data::env::EnvVars::new(env_data, binaries, entrypoints, integrations)
                .with_advisories(api::data::env::LazyAdvisoryReport::from_advisories(&advisories)),
        )?;

        Ok(ExitCode::SUCCESS)
    }
}

/// The group tier of the `lazy-mode` ladder for one selected tool; `None` for a positional package.
fn group_of(origin: &Origin) -> Option<&str> {
    match origin {
        Origin::Group(name) => Some(name.as_str()),
        Origin::Explicit => None,
    }
}

/// Resolve the global toolchain's lock-pinned set into env entries, offline; `current` is not consulted.
///
/// Never contacts the registry and silently skips an unmaterialised tool: the login exporter must never block a shell.
/// `Ok(None)` only when neither the lock nor the global `[env]` contributes (`adr_project_env_declaration.md`).
/// `pinned_cli` is `None` when no flag was given, never `Some(false)`, which would outrank `ocx.toml` and the env.
///
/// # Errors
///
/// A fail-closed patch or composition failure propagates; benign toolchain faults return `Ok(None)`.
pub(crate) async fn resolve_global_pinned_env(
    context: &crate::app::Context,
    target: &Platform,
    groups: &[String],
    env_overrides: &[Entry],
    pinned_cli: Option<bool>,
) -> anyhow::Result<Option<(Vec<Entry>, usize, Vec<PatchProvenance>, AdmittedClaims)>> {
    let home = context.file_structure().root();
    let global_config = ocx_project::ProjectConfig::global_manifest_path(home);
    let global_lock_path = lock_path_for(&global_config);

    // An unreadable global config yields empty env, never a failure of the login exporter.
    let (no_patches, mut project_env, pinned) = match ocx_project::ProjectConfig::from_path(&global_config).await {
        Ok(config) => {
            // Against the config's groups, not the lock's: an env-only group has no lock entry.
            let mut env_groups = expand_all_keyword(groups, &config);
            if env_groups.is_empty() {
                env_groups = vec![DEFAULT_GROUP.to_owned()];
            }
            let env = ocx_project::project_env_entries(&config, &global_config, &env_groups);
            let pinned = ocx_package_manager::pinned_for_project(pinned_cli, &config);
            (config.no_patches_repositories(), env, pinned)
        }
        Err(_) => (
            std::collections::BTreeSet::new(),
            Vec::new(),
            ocx_package_manager::pinned_for_project(pinned_cli, &ocx_project::ProjectConfig::default()),
        ),
    };
    // `--env` applies even when the global file is unparseable: it was typed on this invocation.
    project_env.extend_from_slice(env_overrides);

    // Must not contact the registry, even under `--remote`.
    let manager = context.manager().offline_view(context.local_index().clone());

    // A missing or corrupt global lock is benign here; the commands that rewrite it surface it.
    let lock = match ProjectLock::from_path(&global_lock_path).await {
        Ok(lock) => lock,
        Err(error) => {
            tracing::debug!("global lock unreadable; emitting declared env only: {error:#}");
            None
        }
    };

    let mut infos = Vec::new();
    let mut toolchain = None;
    if let Some(lock) = &lock {
        let selected_groups = selected_groups_global(groups, lock);
        // A tool name that cannot be a path component is filtered, or a corrupt lock fails the login exporter.
        toolchain = context
            .manager()
            .toolchain_home(&ocx_store::file_structure::RenderStampScope::Global, None)
            .ok()
            .map(|home| {
                let mut followable = lock.clone();
                followable
                    .tools
                    .retain(|tool| home.entry(&tool.group, &tool.name).is_ok());
                Box::new(ocx_package_manager::ToolchainLinks {
                    pinned,
                    home,
                    scope: ocx_store::file_structure::RenderStampScope::Global,
                    lock: followable,
                    groups: selected_groups.clone(),
                })
            });
        for tool in &lock.tools {
            if !selected_groups.iter().any(|g| g == &tool.group) {
                continue;
            }
            // An absent or ambiguous host leaf is skipped: the login exporter cannot disambiguate.
            let ocx_oci::Selection::Found((leaf, _key)) = ocx_project::lookup_host_leaf(&tool.platforms, target) else {
                continue;
            };
            let identifier: ocx_oci::PackageRef = tool.repository.clone_with_digest(leaf.clone());
            match manager.find(&identifier, target.clone()).await {
                Ok(info) => infos.push(Arc::new(info)),
                Err(_) => continue,
            }
        }
    }

    if infos.is_empty() && project_env.is_empty() {
        return Ok(None);
    }

    // `offline_view` keeps the patch tier, so companion overlays still apply.
    let scope = ocx_package_manager::EnvScope::Project {
        no_patches,
        env: project_env,
        // `None` only without a global lock, which is the digest lane.
        toolchain,
    };
    Ok(Some(
        manager
            .resolve_env_with_attribution(&infos, false, scope, target)
            .await?,
    ))
}

/// Resolve raw `-g` values to the global tier's tool groups: empty is `default`, `all` adds every lock group.
// ponytail: `all` is enumerated from the lock; a group with no locked tool has no tool to select.
fn selected_groups_global(raw: &[String], lock: &ProjectLock) -> Vec<String> {
    if raw.is_empty() {
        return vec![DEFAULT_GROUP.to_owned()];
    }
    if !raw.iter().any(|g| g == ALL_GROUP) {
        return raw.to_vec();
    }
    let mut named: Vec<String> = lock
        .tools
        .iter()
        .map(|tool| tool.group.clone())
        .filter(|group| group != DEFAULT_GROUP)
        .collect();
    named.sort();
    named.dedup();
    let mut groups = vec![DEFAULT_GROUP.to_owned()];
    groups.extend(named);
    groups
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use clap::Parser;

    fn platform(s: &str) -> Platform {
        s.parse().expect("valid platform")
    }

    /// No `--platform` → the host native platform (or `any` when unsupported).
    #[test]
    fn platform_defaults_to_host() {
        // Never errors; the concrete value depends on the build target.
        platform_or_default(None);
    }

    /// `--platform` parses at the clap layer to exactly one value; clap itself
    /// rejects a second occurrence of the flag (Option<T>, not Vec<T>).
    #[test]
    fn parses_platform_flag() {
        let env = ToolchainEnv::try_parse_from(["env", "--platform", "linux/arm64"]).unwrap();
        assert_eq!(env.platform.platform, Some(platform("linux/arm64")));
    }

    /// `-g` is repeatable and comma-delimited: `-g ci,lint -g release` → 3.
    #[test]
    fn parses_repeatable_comma_group_flag() {
        let env = ToolchainEnv::try_parse_from(["env", "-g", "ci,lint", "-g", "release"]).unwrap();
        assert_eq!(env.groups.names(), ["ci", "lint", "release"]);
    }

    /// `--env` reaches the composition on this command too, so an exporter can
    /// print the environment the equivalent `ocx exec` would execute in. Parsing
    /// is `options::EnvOverride`'s own contract; this pins only the wiring.
    #[test]
    fn parses_repeatable_env_flag() {
        let env = ToolchainEnv::try_parse_from(["env", "--env", "A=1", "--env", "PATH:path=/opt/bin"]).unwrap();
        let entries = env.env.entries(Path::new("/invocation")).expect("entries");
        assert_eq!(entries.len(), 2, "both occurrences must reach the command");
        assert_eq!(entries[0].key, "A");
        assert_eq!(entries[1].key, "PATH");
    }

    /// `ocx env` installs on miss by default; `--no-pull` opts out. Pins the
    /// eager default at the parse/wiring site so a default-flip regresses here
    /// in milliseconds instead of only in the acceptance suite.
    #[test]
    fn pull_flags_flatten_with_eager_default() {
        let default = ToolchainEnv::try_parse_from(["env"]).unwrap();
        assert!(
            default.pull.enabled(true),
            "env default must be eager (install on miss)"
        );

        let opt_out = ToolchainEnv::try_parse_from(["env", "--no-pull"]).unwrap();
        assert!(
            !opt_out.pull.enabled(true),
            "--no-pull must opt out of the install fallback"
        );
    }

    // ── selected_groups_global ────────────────────────────────────────────────

    fn lock_with_groups(groups: &[&str]) -> ProjectLock {
        use ocx_oci::{Digest, PackageRef};
        use ocx_project::{LockMetadata, LockVersion, LockedTool};
        let tools = groups
            .iter()
            .enumerate()
            .map(|(i, group)| {
                let mut platforms = std::collections::BTreeMap::new();
                platforms.insert(
                    "linux/amd64".to_owned(),
                    Digest::Sha256(std::iter::repeat_n('a', 64).collect()),
                );
                LockedTool {
                    name: format!("tool{i}"),
                    group: (*group).to_owned(),
                    repository: PackageRef::new_registry(format!("tool{i}"), "ocx.sh"),
                    platforms,
                }
            })
            .collect();
        ProjectLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V3,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", std::iter::repeat_n('0', 64).collect::<String>()),
                generated_by: "ocx test".into(),
                generated_at: "2026-04-24T00:00:00Z".into(),
            },
            tools,
        }
    }

    /// Empty `-g` → the default group only.
    #[test]
    fn selected_groups_global_empty_is_default() {
        let lock = lock_with_groups(&["default", "lint"]);
        assert_eq!(selected_groups_global(&[], &lock), vec!["default".to_owned()]);
    }

    /// `-g all` → default + every distinct named lock group, sorted.
    #[test]
    fn selected_groups_global_all_expands_from_lock() {
        let lock = lock_with_groups(&["default", "lint", "ci", "lint"]);
        assert_eq!(
            selected_groups_global(&["all".to_owned()], &lock),
            vec!["default".to_owned(), "ci".to_owned(), "lint".to_owned()]
        );
    }

    /// Named groups pass through verbatim (unknown names allowed — lenient tier).
    #[test]
    fn selected_groups_global_passthrough() {
        let lock = lock_with_groups(&["default", "lint"]);
        assert_eq!(
            selected_groups_global(&["lint".to_owned(), "missing".to_owned()], &lock),
            vec!["lint".to_owned(), "missing".to_owned()]
        );
    }

    // ── The `pinned` ladder's CLI tier on the GLOBAL tier ─────────────────────
    //
    // `ocx --global env` resolves through `resolve_global_pinned_env`, which is
    // a different code path from the project tier's `toolchain_links` — so the
    // CLI tier reaching one proves nothing about the other. These read the lane
    // at the COMMAND surface (`Cli::parse_from` → `execute`) rather than at a
    // function signature, which is what lets them compile against a tree where
    // the resolver takes no CLI tier at all and fail there.

    /// A global file that declares only an `[env]` — every `pinned` tier absent.
    const DECLARED_ENV: &str = "[env]\nWP18 = \"composed\"\n";

    /// The same, with the FILE tier of the ladder asking to pin.
    const PINNING_ENV: &str = "pinned = true\n[env]\nWP18 = \"composed\"\n";

    /// A `$OCX_HOME` carrying the two things the global resolver reads: a
    /// declared `[env]`, so it always has a contribution and never
    /// short-circuits to `Ok(None)`, and a one-tool `ocx.lock`, so it has a
    /// toolchain lane to choose between.
    fn global_home(config_body: &str) -> tempfile::TempDir {
        let home = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(home.path().join("ocx.toml"), config_body).expect("write ocx.toml");
        std::fs::write(
            home.path().join("ocx.lock"),
            lock_with_groups(&["default"])
                .to_toml_string()
                .expect("the fixture lock serializes"),
        )
        .expect("write ocx.lock");
        home
    }

    /// Runs the real `ocx --global env <flags>` over a fresh home and answers
    /// whether the **link lane** ran.
    ///
    /// `pinned = true` is answered "before any I/O: no heal, no probe,
    /// no link consulted" ([`ocx_package_manager::ToolchainLinks::pinned`]),
    /// while the following lane heals first — and `heal_links` creates the
    /// toolchain home root before it repairs anything. So the root's existence
    /// after the run IS the lane the resolver chose, and it needs no
    /// materialised package to observe.
    ///
    /// The root's creation is conditional on the heal not being refused — a
    /// refused tree creates nothing and answers `HealOutcome::Refused` (crate-
    /// internal to `ocx_lib`, hence no intra-doc link). The premise holds for
    /// this fixture because [`global_home`] is a fresh
    /// tempdir with no symlink anywhere on the path, so the only reachable
    /// answer is a heal that ran.
    ///
    /// The home path is asked of the manager (the same call
    /// `resolve_global_pinned_env` makes), never joined by hand.
    async fn link_lane_ran(config_body: &str, flags: &[&str]) -> bool {
        use ocx_console::ColorModeConfig;
        use ocx_store::file_structure::RenderStampScope;

        use crate::app::{Cli, Context, ManagedConfigGate};

        let home = global_home(config_body);
        // SAFETY: `OCX_HOME` is read through `ocx_util::env::var`, whose
        // `#[cfg(test)]` override seam is internal to `ocx_lib` and therefore
        // unavailable from this crate; the process variable is the only seam.
        // nextest runs one test per process, so this cannot race a sibling.
        unsafe { std::env::set_var("OCX_HOME", home.path()) };

        let mut argv = vec!["ocx", "--global", "env"];
        argv.extend_from_slice(flags);
        let cli = Cli::parse_from(argv);
        let context = Context::try_init(
            &cli.context,
            ColorModeConfig {
                stdout: false,
                stderr: false,
                relayed: false,
            },
            ManagedConfigGate {
                enforce_required: false,
                onboarding: false,
            },
        )
        .await
        .expect("a context over the fixture home");
        assert_eq!(
            context.file_structure().root(),
            home.path(),
            "the context must resolve the tempdir as its home, or this reads someone else's \
             ocx.toml and proves nothing"
        );

        let root = context
            .manager()
            .toolchain_home(&RenderStampScope::Global, None)
            .expect("the global toolchain home")
            .root()
            .to_path_buf();
        assert!(
            !root.exists(),
            "precondition: nothing may have rendered '{}' before the command ran, or every \
             answer below is `true` for free",
            root.display()
        );

        let Some(crate::command::Command::Env(env)) = cli.command else {
            panic!("`ocx --global env` must parse to the env command");
        };
        env.execute(context)
            .await
            .expect("the global tier never fails on a well-formed home");

        let ran = root.exists();
        // SAFETY: see above.
        unsafe { std::env::remove_var("OCX_HOME") };
        ran
    }

    /// `ocx --global env --pinned` composes the DIGEST lane.
    ///
    /// The defect this pins: `resolve_global_pinned_env` re-derived the lane
    /// from the file and environment tiers alone, so the parsed flag was
    /// accepted and silently discarded — exit 0, nothing on stderr, and the
    /// opposite of what was asked.
    ///
    /// `global_env_with_no_flag_follows_the_links` is this test's positive
    /// control and must be read with it: without that one, a fixture whose lock
    /// failed to parse (no toolchain lane at all, hence no heal, hence no root)
    /// would satisfy this assertion for entirely the wrong reason. They are two
    /// tests rather than two assertions because `Context::try_init` installs the
    /// global tracing dispatcher, which a process may do exactly once.
    #[tokio::test]
    async fn global_env_pinned_selects_the_digest_lane() {
        assert!(
            !link_lane_ran(DECLARED_ENV, &["--pinned"]).await,
            "--pinned must reach the global resolver: the digest lane short-circuits before any \
             I/O, so no heal runs and no root is rendered"
        );
    }

    /// The control for [`global_env_pinned_selects_the_digest_lane`]: the same
    /// fixture with no flag follows the links, so the heal runs and the root
    /// appears. Proves the harness can answer `true` at all.
    #[tokio::test]
    async fn global_env_with_no_flag_follows_the_links() {
        assert!(
            link_lane_ran(DECLARED_ENV, &[]).await,
            "with every tier absent the floor is the following lane, so the heal runs"
        );
    }

    /// `ocx --global env --no-pinned` outranks a global
    /// `ocx.toml` that asked to pin.
    ///
    /// The mirror direction, and the one that proves the CLI tier is a real
    /// LADDER tier rather than an `Option`-collapsed default: the file states
    /// `pinned = true` and the flag has to beat it. Control:
    /// [`global_env_with_a_pinning_file_and_no_flag_pins`].
    #[tokio::test]
    async fn global_env_no_pinned_outranks_a_pinning_global_file() {
        assert!(
            link_lane_ran(PINNING_ENV, &["--no-pinned"]).await,
            "--no-pinned must reach the global resolver and beat the file tier, or the flag \
             cannot override an ocx.toml that asked to pin"
        );
    }

    /// The control for [`global_env_no_pinned_outranks_a_pinning_global_file`]:
    /// the file tier alone already pins, so no heal runs. Proves the fixture's
    /// `pinned = true` is actually read.
    #[tokio::test]
    async fn global_env_with_a_pinning_file_and_no_flag_pins() {
        assert!(
            !link_lane_ran(PINNING_ENV, &[]).await,
            "`pinned = true` in the global ocx.toml selects the digest lane on its own"
        );
    }

    /// `ocx env` and `ocx exec` read the SAME CLI tier.
    ///
    /// A control, not a discriminator: the parse layer already agrees on the
    /// unfixed tree. It is here because the two commands' *wiring* is what
    /// diverged, and this is the assertion a future divergence trips — both
    /// feed this exact value into `pinned_for_project`.
    #[test]
    fn env_and_exec_read_the_same_pinned_cli_tier() {
        use crate::command::toolchain_exec::ToolchainExec;

        for (flags, expected) in [
            (Vec::new(), None),
            (vec!["--pinned"], Some(true)),
            (vec!["--no-pinned"], Some(false)),
        ] {
            let mut env_argv = vec!["env"];
            env_argv.extend_from_slice(&flags);
            let env = ToolchainEnv::try_parse_from(env_argv).expect("env parses");

            let mut exec_argv = vec!["exec"];
            exec_argv.extend_from_slice(&flags);
            exec_argv.extend_from_slice(&["--", "true"]);
            let exec = ToolchainExec::try_parse_from(exec_argv).expect("exec parses");

            assert_eq!(env.pinned.pinned(), expected, "env must read {flags:?} as {expected:?}");
            assert_eq!(
                exec.pinned.pinned(),
                env.pinned.pinned(),
                "the two siblings must never disagree about the CLI tier for {flags:?}"
            );
        }
    }
}
