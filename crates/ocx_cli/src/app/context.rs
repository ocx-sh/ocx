// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use ocx_lib::{
    ConfigInputs, ConfigLoader,
    cli::{ColorModeConfig, Printer, UserInterface},
    env,
    file_structure::{self, BlobStore, TagStore},
    log,
    oci::{self, index},
    package_manager,
};

use crate::api;

use super::ContextOptions;

#[derive(Clone)]
pub struct Context {
    offline: bool,
    project_path: Option<PathBuf>,
    remote_client: Option<oci::Client>,
    remote_index: Option<oci::index::RemoteIndex>,
    local_index: oci::index::LocalIndex,
    file_structure: file_structure::FileStructure,
    api: api::Api,
    ui: UserInterface,
    default_index: oci::index::Index,
    manager: package_manager::PackageManager,
    default_registry: String,
    config_view: env::OcxConfigView,
    concurrency: package_manager::Concurrency,
    progress: ocx_lib::cli::progress::ProgressManager,
}

impl Context {
    pub async fn try_init(options: &ContextOptions, color_config: ColorModeConfig) -> anyhow::Result<Context> {
        // Shared span-free progress manager (ADR adr_progress_architecture).
        // Created before the subscriber so its `MultiProgress` backs the
        // fmt log writer (log lines flush inside `suspend`, never tearing
        // bars). Threaded into the OCI client (transfer bars) and the
        // package manager (task spinners). Disabled when stderr is not a
        // TTY so non-interactive runs pay no cost.
        let progress = if ocx_lib::cli::ProgressMode::detect().stderr {
            ocx_lib::cli::progress::ProgressManager::stderr()
        } else {
            ocx_lib::cli::progress::ProgressManager::disabled()
        };

        ocx_lib::cli::LogSettings::default()
            .with_console_level(options.log_level)
            .with_stderr_color(color_config.stderr)
            .init_with_progress(&progress)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        log::debug!("Creating context with options: {:?}", options);

        if options.offline && options.remote {
            // `--offline --remote` = pinned-only mode. Both flags accepted
            // together because the routing matrix collapses cleanly:
            // `--offline` overrides `--remote` to no-source-contact, and
            // any tag-addressed resolution must succeed locally or error.
            // Documented in user-guide §Routing and command-line.md.
            log::info!(
                "--offline --remote: pinned-only mode - tag and catalog lookups will not contact a source. \
                 Tag-addressed resolution attempts must be satisfied locally or by digest-pinned identifiers."
            );
        }

        // Capture the explicit project path before consuming `options` into other
        // init calls. `lock` and similar commands need it for the precedence
        // chain: `--global`/`OCX_GLOBAL` selector ▸ `--project` ▸ `OCX_PROJECT`
        // ▸ CWD walk ▸ None.
        let project_path = options.project.clone();

        let cwd = env::current_dir()?;
        let config = ConfigLoader::load(ConfigInputs {
            explicit_path: options.config.as_deref(),
            explicit_project_path: options.project.as_deref(),
            cwd: Some(&cwd),
        })
        .await?;

        // Resolve the per-host mirror map once via the lib resolver
        // (`ocx_lib::resolve_mirror_map`): `[mirrors]` config merged with the
        // inherited `OCX_MIRRORS` env (env wins per-host key), every entry parsed
        // and the plain-HTTP gate enforced in one place. The same resolved set
        // feeds both the OCI client (transport rewrite) and the `OcxConfigView`
        // (subprocess forwarding), so parent and forwarded children agree on the
        // mirror map. The lib `thiserror` error is re-wrapped into `anyhow` at
        // this CLI boundary.
        let (mirror_entries, mirror_pairs) =
            ocx_lib::resolve_mirror_map(&config, env::mirrors()?, &env::insecure_registries())
                .map_err(anyhow::Error::new)?;
        let mirror_map = oci::MirrorMap::new(mirror_entries);

        let printer = Printer::new(color_config.stdout, color_config.stderr);
        let ui = UserInterface::new(printer, console::Term::stderr().is_term(), options.quiet);
        // `ContextOptions::build_api` owns the printer + format-default +
        // quiet wiring. Shared with the Context-free static-command bypass
        // (`ocx version`) so both paths honour `--color` and the
        // `None → Plain` format default identically (handshake §3 amended
        // 2026-05-19: format is a context-only concern, no per-command
        // divergence).
        let api = options.build_api(color_config);

        let (remote_client, remote_index) = if options.offline {
            (None, None)
        } else {
            // Explicit builder (not `from_env_with_progress`) so the
            // config-derived `MirrorMap` is threaded in; `OCX_MIRRORS` env
            // precedence is already folded into `mirror_map` by
            // `resolve_mirrors`. A plain-HTTP mirror requires its host in
            // `OCX_INSECURE_REGISTRIES` (the mirror host is what gets contacted)
            // — composition with the existing plain-HTTP set, no implicit
            // scheme-driven opt-out (ADR F2).
            let client = oci::ClientBuilder::new()
                .plain_http_registries(env::insecure_registries())
                .mirrors(mirror_map)
                .progress(progress.clone())
                .build();
            (
                Some(client.clone()),
                Some(index::RemoteIndex::new(index::RemoteConfig { client })),
            )
        };
        let file_structure = file_structure::FileStructure::new();
        let tag_root = options
            .index
            .clone()
            .or_else(|| env::var(env::keys::OCX_INDEX).map(std::path::PathBuf::from))
            .unwrap_or_else(|| file_structure.tags.root().to_path_buf());
        let local_index = index::LocalIndex::new(index::LocalConfig {
            tag_store: TagStore::new(tag_root),
            blob_store: BlobStore::new(file_structure.blobs.root().to_path_buf()),
        });

        // Single `Index::from_chained` entry point. `remote_index` is the
        // authoritative signal: `None` means offline (no network sources);
        // `Some` means online (wrap it as a chain source). `options.remote`
        // then selects between `Default` (cache-first) and `Remote`
        // (mutable lookups bypass cache) for online mode. Deriving mode and
        // sources from the same value prevents the `(offline=false,
        // remote_index=None)` unreachable case the older bool-based match
        // produced.
        let (mode, sources): (index::ChainMode, Vec<index::Index>) = match &remote_index {
            None => (index::ChainMode::Offline, Vec::new()),
            Some(remote) => {
                let mode = if options.remote {
                    index::ChainMode::Remote
                } else {
                    index::ChainMode::Default
                };
                (mode, vec![index::Index::from_remote(remote.clone())])
            }
        };
        let selected_index = index::Index::from_chained(local_index.clone(), sources, mode);

        let default_registry = env::string(
            "OCX_DEFAULT_REGISTRY",
            config
                .resolved_default_registry()
                .map(str::to_owned)
                .unwrap_or_else(|| ocx_lib::oci::DEFAULT_REGISTRY.into()),
        );

        let manager = package_manager::PackageManager::new(
            file_structure.clone(),
            selected_index.clone(),
            remote_client.clone(),
            &default_registry,
        )
        .with_progress(progress.clone());

        // Capture the absolute path of the running ocx so subprocess spawns
        // can pin the inner ocx binary via `OCX_BINARY_PIN` instead of relying
        // on whatever `$PATH` resolves at the launcher site. Falling back to
        // the canonical `ocx` name lets ocx still operate when `current_exe()`
        // fails (e.g. binary deleted under a long-running process); the child
        // launcher's `${OCX_BINARY_PIN:-ocx}` form then degrades to `$PATH`-lookup.
        let self_exe = std::env::current_exe().unwrap_or_else(|e| {
            log::warn!("Could not resolve current exe: {e}");
            std::path::PathBuf::from("ocx")
        });
        let mut config_view = options.as_view(self_exe);
        // Feed the same resolved mirror map into the forwarding view so a child
        // ocx inherits `OCX_MIRRORS` matching the parent's transport rewrite.
        config_view.mirrors = mirror_pairs;
        check_global_project_exclusivity(&config_view)?;
        let concurrency = resolve_concurrency(options.jobs);

        Ok(Context {
            remote_client,
            remote_index,
            offline: options.offline,
            project_path,
            file_structure,
            api,
            ui,
            local_index,
            default_index: selected_index,
            manager,
            default_registry,
            config_view,
            concurrency,
            progress,
        })
    }

    /// Shared span-free progress manager (ADR adr_progress_architecture).
    /// Commands wrap long operations in guards from this manager
    /// (`spinner`/`bytes`) instead of emitting tracing-indicatif spans.
    pub fn progress(&self) -> &ocx_lib::cli::progress::ProgressManager {
        &self.progress
    }

    pub fn is_offline(&self) -> bool {
        self.offline
    }

    /// Returns the explicit `--project` / `OCX_PROJECT` override path, if
    /// one was supplied. Commands that need project-level resolution (e.g. `lock`)
    /// should pass this to `ProjectConfig::resolve` as the explicit override so
    /// the flag is not silently discarded.
    pub fn project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }

    /// Whether the global toolchain (`$OCX_HOME/ocx.toml`) was selected
    /// via the root `--global` flag or `OCX_GLOBAL`. Passed to
    /// `ProjectConfig::resolve` so project-tier prologues select the
    /// global file instead of walking the CWD. Mutually exclusive with an
    /// explicit `--project` / `OCX_PROJECT` (enforced by clap
    /// `conflicts_with` for the flag pair and by
    /// [`check_global_project_exclusivity`] for the env-sourced gaps).
    pub fn global(&self) -> bool {
        self.config_view.global
    }

    pub fn remote_client(&self) -> ocx_lib::Result<&oci::Client> {
        self.remote_client.as_ref().ok_or(ocx_lib::Error::OfflineMode)
    }

    pub fn remote_index(&self) -> ocx_lib::Result<&oci::index::RemoteIndex> {
        self.remote_index.as_ref().ok_or(ocx_lib::Error::OfflineMode)
    }

    pub fn local_index(&self) -> &oci::index::LocalIndex {
        &self.local_index
    }

    pub fn default_index(&self) -> &oci::index::Index {
        &self.default_index
    }

    pub fn default_registry(&self) -> &str {
        &self.default_registry
    }

    pub fn file_structure(&self) -> &file_structure::FileStructure {
        &self.file_structure
    }

    pub fn api(&self) -> &api::Api {
        &self.api
    }

    pub fn ui(&self) -> &UserInterface {
        &self.ui
    }

    pub fn manager(&self) -> &package_manager::PackageManager {
        &self.manager
    }

    /// Resolution-affecting policy snapshot to forward to subprocess spawns
    /// via [`env::Env::apply_ocx_config`]. Built from parsed `ContextOptions`
    /// at init time — beats stale parent-shell `OCX_*` exports.
    pub fn config_view(&self) -> &env::OcxConfigView {
        &self.config_view
    }

    /// Concurrency cap for parallel pulls, derived from `--jobs` (CLI),
    /// `OCX_JOBS` (env), or unbounded by default.
    pub fn concurrency(&self) -> package_manager::Concurrency {
        self.concurrency
    }
}

/// Resolves `--jobs` / `OCX_JOBS` into a `Concurrency` value.
///
/// Precedence: CLI flag > env var > unbounded. `0` (from either source)
/// resolves to logical-core count (GNU Parallel convention). Invalid env
/// values are logged and ignored — the env path is best-effort.
fn resolve_concurrency(jobs: Option<usize>) -> package_manager::Concurrency {
    use std::num::NonZeroUsize;

    let raw = match jobs {
        Some(n) => Some(n),
        None => env::var("OCX_JOBS").and_then(|v| match v.parse::<usize>() {
            Ok(n) => Some(n),
            Err(e) => {
                log::warn!("ignoring invalid OCX_JOBS value {v:?}: {e}");
                None
            }
        }),
    };

    match raw {
        None => package_manager::Concurrency::Unbounded,
        Some(0) => package_manager::Concurrency::cores(),
        Some(n) => package_manager::Concurrency::Limit(NonZeroUsize::new(n).expect("n > 0 covered above")),
    }
}

/// Enforce mutual exclusion of the global toolchain selector and an
/// explicit project selection.
///
/// `--global` / `OCX_GLOBAL` and an explicit project (`--project` flag or
/// `OCX_PROJECT` env) both pick a project file. clap's
/// `conflicts_with = "project"` on [`ContextOptions::global`] already
/// rejects the explicit `--global` + `--project` *flag* pair at parse
/// time. This guard closes the gaps clap cannot see: `OCX_GLOBAL` reaches
/// `view.global` through the arg default (not a CLI-provided value, so
/// clap's conflict does not fire), and `OCX_PROJECT` is not a clap arg at
/// all. A project merely discovered by the CWD walk is *not* explicit —
/// `--global` from inside a project tree is legal and the global tier
/// wins by precedence (adr_global_toolchain_tier.md §Decision 2), so the
/// CWD walk deliberately sets neither `view.project` nor `OCX_PROJECT`.
///
/// # Errors
///
/// Returns [`UsageError`](ocx_lib::cli::UsageError) (exit `64`) when the
/// global selector is set alongside an explicit `--project` / `OCX_PROJECT`
/// selection.
fn check_global_project_exclusivity(view: &env::OcxConfigView) -> Result<(), ocx_lib::cli::UsageError> {
    // `OCX_PROJECT=""` is the loader's escape hatch (treated as unset);
    // mirror that here so an explicitly-cleared env var is not misread as
    // an explicit selection.
    let explicit_project = view.project.is_some() || env::var(env::keys::OCX_PROJECT).is_some_and(|v| !v.is_empty());
    if view.global && explicit_project {
        return Err(ocx_lib::cli::UsageError::new(
            "--global cannot be combined with an explicit --project / OCX_PROJECT selection",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Spec for the `--global` ⟂ explicit-project exclusivity guard.
    //!
    //! `--global` is a single root-level flag (peer of `--project`); the
    //! `--global` + `--project` *flag* pair is rejected by clap
    //! (`conflicts_with`). [`check_global_project_exclusivity`] closes the
    //! env-sourced gaps clap cannot see (`OCX_GLOBAL` via the arg default,
    //! or `OCX_PROJECT` which is not a clap arg). The `OCX_PROJECT` gap is
    //! exercised end-to-end by `test/tests/test_global_toolchain.py`
    //! (`test_env_global_with_env_project_conflict`); it is not unit-tested
    //! here because `ocx_lib::env::var`'s test-override seam is inert when
    //! `ocx_lib` is consumed as a (non-`cfg(test)`) dependency, and real
    //! env mutation is `unsafe` on edition 2024. This test pins the
    //! `--project`-flag path, whose `||` short-circuits before any env read
    //! and is therefore deterministic.

    use super::*;
    use ocx_lib::cli::{ClassifyExitCode, ExitCode};

    #[test]
    fn global_with_explicit_project_flag_is_usage_error() {
        let mut view = ocx_lib::env::OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        view.global = true;
        view.project = Some(std::path::PathBuf::from("/abs/explicit/ocx.toml"));

        let err = check_global_project_exclusivity(&view)
            .expect_err("--global + explicit --project must be rejected (ADR §Decision 2)");
        assert_eq!(
            err.classify(),
            Some(ExitCode::UsageError),
            "the conflict must classify to ExitCode::UsageError (64)"
        );
        assert_eq!(
            ExitCode::UsageError as u8,
            64,
            "UsageError must be sysexits EX_USAGE (64)"
        );
        assert!(
            err.to_string().contains("--global"),
            "conflict message must name --global so users can grep stderr; got: {err}"
        );
    }
}
