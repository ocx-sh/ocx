// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use crate::{api, conventions::*, options};
use clap::Parser;
use ocx_package::metadata::env::apply::reconcile_list_separators;
use ocx_package_manager::composer::{ComposeRequest, Materialization};
use ocx_shell::shell::Shell;

/// Print the resolved environment variables for one or more installed packages.
///
/// Plain format: aligned table with Key, Value, and Type columns where Type is `constant` or `path`.
/// JSON format:  `{"entries": [{"key": "...", "value": "...", "type": "constant"|"path"}, ...]}`.
/// External tools (Python scripts, Bazel rules, CI steps) use it to configure
/// child process environments without going through `ocx exec`.
///
/// Values are rooted in the content-addressed object store and may change when
/// a package is updated; `--candidate` or `--current` roots them in a stable
/// symlink path instead, for editor or IDE configuration files.
#[derive(Parser)]
pub struct Env {
    /// Expose the package's full env, including private (self-only) entries.
    /// See `ocx exec --help` for full view semantics.
    #[arg(long_help = "\
        Expose the package's full env, including private (self-only) entries. See `ocx exec --help` \
        for full view semantics.\n\n\
        Generated launchers embed `--self`; avoid passing it directly unless building a launcher \
        equivalent.\n\n\
        Cannot be combined with `--lazy-mode always`: a generated shim is a launcher, launchers are \
        consumer-facing, and a package's private view bypasses them, so those two ask for \
        contradictory things (exit 64). `--lazy-mode never` agrees with this view and is accepted, \
        as is an `always` coming from `OCX_LAZY_MODE`, which composes eagerly.")]
    #[clap(long = "self", default_value_t = false)]
    self_view: bool,

    #[clap(flatten)]
    env: options::EnvOverride,

    #[clap(flatten)]
    platform: options::PlatformOption,

    #[clap(flatten)]
    content_path: options::ContentPath,

    /// Top tier of the `lazy-mode` ladder for every package this command composes.
    ///
    /// `always` composes a package as a generated shim: its declared names
    /// reach `PATH` immediately and its content downloads on first use. The
    /// shim directory sits *below* the package's own `entrypoints/` and `bin/`
    /// in the composed `PATH`, so the same exported environment stops routing
    /// through it once the first invocation has materialized the package.
    ///
    /// Only this flag and `OCX_LAZY_MODE` apply here: the `ocx.toml` tiers
    /// belong to the toolchain commands, and this one reads no project file.
    #[clap(flatten)]
    lazy_mode: options::LazyMode,

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
    #[arg(long_help = "\
        Write the composed environment into a CI system's persistence channel.\n\n\
        `--ci=github` appends package dirs and vars to `$GITHUB_PATH` / `$GITHUB_ENV`; \
        `--ci=gitlab` writes JSON-lines to `--export-file` (or stdout). Bare `--ci` autodetects the \
        provider from CI environment variables; exit 64 if none is detected. Must be supplied with \
        `=` (`--ci=github`).\n\n\
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
    /// Materialization for an eager package: `--candidate`/`--current`/`--link` root composed values in
    /// an install link instead of the object store.
    fn materialization(&self) -> Result<Materialization, crate::error::UsageError> {
        Ok(match self.content_path.link_source(self.packages.len())? {
            Some(source) => Materialization::Symlink(source),
            None => Materialization::Install,
        })
    }

    /// Refuses `--ci github`, which `require_equals` reads as bare `--ci` plus a package `github` whose
    /// resolve failure would blame the registry. A method so a test can drive it on a clap-parsed `Env`.
    fn refuse_spaced_ci(&self) -> Result<(), crate::error::UsageError> {
        // `Some(None)` is the bare flag, the only form that can have lost a value.
        refuse_spaced_enum_value::<ocx_shell::ci::CiFlavor>(
            "--ci",
            matches!(self.ci, Some(None)),
            self.packages.iter().map(ToString::to_string),
        )
    }

    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Before the autodetect: a spaced value never reached the flag, and saying so beats its guess.
        self.refuse_spaced_ci()?;
        // Early, so a bare-`--ci` autodetect failure is a usage error before the slow resolution.
        let ci = resolve_ci_arg(self.ci)?;

        let cwd = std::env::current_dir()
            .map_err(|error| anyhow::Error::from(error).context("failed to read the current directory"))?;
        let env_overrides = self.env.entries(&cwd)?;

        let platform = platform_or_default(self.platform.platform.clone());
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        let manager = context.manager();

        let materialization = self.materialization()?;
        let mode = resolved_lazy_mode(self.lazy_mode.mode(), self.self_view)?;
        // A deferred package has no install symlink to root values in, so the two requests contradict;
        // 64 beats silently honouring either.
        if mode == ocx_project::lazy::LazyMode::Always && matches!(materialization, Materialization::Symlink(_)) {
            return Err(crate::error::UsageError::new(
                "--candidate/--current/--link cannot be combined with a lazy-mode of 'always': a deferred package has no install symlink to root values in",
            )
            .into());
        }
        let requests: Vec<ComposeRequest> = identifiers
            .into_iter()
            .map(|identifier| ComposeRequest { identifier, mode })
            .collect();
        let composed = manager
            .compose_roots(&requests, &platform, materialization, context.concurrency())
            .await?;
        for advisory in &composed.advisories {
            context.ui().warn(advisory.to_string());
        }
        let advisories = composed.advisories;
        let info: Vec<std::sync::Arc<ocx_package::install_info::InstallInfo>> = composed.roots;
        // OCI tier: `--env` is the only per-invocation input and applies last, matching what
        // `ocx package exec --env` executes with.
        let (mut entries, patch_start, provenance, attribution) = manager
            .resolve_env_with_attribution(
                &info,
                self.self_view,
                ocx_package_manager::EnvScope::Package { env: env_overrides },
                &platform,
            )
            .await?;
        let inherited = ocx_util::env::var(ocx_config::env::keys::OCX_LAUNCH_IDENTITIES);
        entries.extend(manager.launch_identity_entry(&info, &std::collections::BTreeSet::new(), inherited.as_deref()));
        // Before any of the three output branches reads `entries`.
        reconcile_list_separators(entries.iter_mut())?;
        if let Some(provider) = ci {
            export_ci(provider, self.export_file.clone(), &entries)?;
            return Ok(ExitCode::SUCCESS);
        }
        if let Some(shell) = resolve_shell_arg(self.shell)? {
            emit_lines(shell, &entries);
            return Ok(ExitCode::SUCCESS);
        }

        // The overlay is the middle region (`--env` composes after it); the bound-checked accessor keeps
        // an override from being labelled a companion's.
        let overlay = ocx_package_manager::PatchOverlay::new(patch_start, &provenance);
        let all_entries: Vec<api::data::env::EnvEntry> = entries
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

        // Only on a non-terminal stdout (the `eval "$(ocx package env)"` case).
        if let Some(advisory) = not_eval_safe_advisory(
            context.api().is_json(),
            std::io::IsTerminal::is_terminal(&std::io::stdout()),
        ) {
            log::warn!("{advisory}");
        }

        let binaries = api::data::env::BinaryAttribution::from_pairs(&attribution.binaries);
        let entrypoints = api::data::env::BinaryAttribution::from_pairs(&attribution.entrypoints);
        let integrations = api::data::env::IntegrationAttribution::from_pairs(&attribution.integrations);

        context.api().report(
            &api::data::env::EnvVars::new(all_entries, binaries, entrypoints, integrations)
                .with_advisories(api::data::env::LazyAdvisoryReport::from_advisories(&advisories)),
        )?;

        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod ci_flag_tests {
    //! The space form of `--ci`, which the grammar cannot take as a value.
    //!
    //! `require_equals` stays: it is a shipped contract on both `--ci`
    //! declarations, so the fix is a refusal rather than a looser grammar.
    //! Every test parses real argv, because the absorption under test is the
    //! parser's doing — this command's positional is `required = true,
    //! num_args = 1..`, so the detached value lands in `packages`.

    use clap::Parser as _;

    use super::Env;

    fn parse(argv: &[&str]) -> Env {
        let mut full = vec!["env"];
        full.extend_from_slice(argv);
        Env::try_parse_from(full).expect("every argv here is grammatical")
    }

    /// The code the process would exit with, through the same authority
    /// `main.rs` uses.
    fn exit_code(error: &anyhow::Error) -> u8 {
        crate::exit::classify_error(error.as_ref()) as u8
    }

    /// The premise the guard rests on: clap leaves the flag bare and hands the
    /// value to the package positional. If a clap upgrade ever attached it,
    /// the guard would become dead code — and this is what would say so.
    #[test]
    fn a_spaced_value_never_reaches_the_flag() {
        let env = parse(&["--ci", "github", "ripgrep"]);

        assert_eq!(env.ci, Some(None), "the flag must be left bare");
        assert_eq!(
            env.packages.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["github", "ripgrep"],
            "the value must have fallen through to the package positional"
        );
    }

    /// Exit 64, naming the token and the `=` requirement — for every spelling
    /// the `=` form would have accepted, aliases included. A mis-cased
    /// spelling cannot reach the guard on this tier at all; see
    /// [`a_mis_cased_provider_is_refused_before_the_guard`].
    #[test]
    fn a_spaced_value_is_refused_with_exit_64() {
        for value in ["github", "gitlab", "github-actions", "gitlab-ci"] {
            let error = anyhow::Error::new(
                parse(&["--ci", value, "ripgrep"])
                    .refuse_spaced_ci()
                    .expect_err("a spaced value must be refused"),
            );

            assert_eq!(exit_code(&error), 64, "a usage fault is exit 64: {error:#}");
            let message = error.to_string();
            // `contains(value)` alone would pass on the advice clause below
            // ("pass --ci=github or ...") without the message ever having
            // echoed the token. Anchor it to the phrase that quotes it.
            assert!(
                message.contains("--ci") && message.contains(&format!("was given `{value}`")),
                "the refusal must name the flag and echo the token: {message}"
            );
            assert!(
                message.contains("--ci=gitlab") && message.contains("attached with `=`"),
                "the refusal must name the `=` requirement: {message}"
            );
        }
    }

    /// Why the guard's case-insensitive match is unreachable here, and must not
    /// be "fixed" to compensate: an OCI repository name is lowercase, so
    /// `--ci GitLab <pkg>` fails in the positional's own value parser before
    /// `execute` runs. The push tier has no such parser — a layer path may be
    /// `GitLab` — which is why the shared guard matches case-insensitively and
    /// why this tier still gets exit 64, just from clap and with clap's message.
    #[test]
    fn a_mis_cased_provider_is_refused_before_the_guard() {
        for value in ["GitLab", "GITHUB"] {
            let Err(error) = Env::try_parse_from(["env", "--ci", value, "ripgrep"]) else {
                panic!("`{value}` is not a legal package reference and must not parse");
            };

            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{value} must fail the package positional's value parser, got: {error}"
            );
        }
    }

    /// The negative controls. The `--ci=github github` row is the one that
    /// pins the bare-flag gate: with the value attached, a package named
    /// `github` is what the caller meant, so the heuristic must not reach it.
    ///
    /// The last two rows are this tier's escape, the counterpart of the layer
    /// path's `./gitlab`: a package genuinely called `github` is reachable by
    /// qualifying the reference, since neither a tag nor a registry prefix
    /// names a `CiFlavor`.
    #[test]
    fn ordinary_invocations_are_not_refused() {
        for argv in [
            vec!["--ci", "ripgrep"],
            vec!["--ci", "acme/github-cli"],
            vec!["--ci=github", "ripgrep"],
            vec!["--ci=github", "github"],
            vec!["ripgrep"],
            vec!["--ci", "github:latest"],
            vec!["--ci", "ocx.sh/github"],
        ] {
            parse(&argv)
                .refuse_spaced_ci()
                .unwrap_or_else(|error| panic!("{argv:?} must not be refused: {error}"));
        }
    }
}
