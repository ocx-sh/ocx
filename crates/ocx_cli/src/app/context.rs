// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use ocx_lib::{
    ConfigInputs, ConfigLoader,
    cli::{ColorModeConfig, DataInterface, Printer, UserInterface},
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
}

impl Context {
    pub async fn try_init(options: &ContextOptions, color_config: ColorModeConfig) -> anyhow::Result<Context> {
        let style =
            ocx_lib::cli::indicatif::ProgressStyle::with_template("{span_child_prefix}{spinner} {span_name}{msg}")
                .expect("valid indicatif template");

        ocx_lib::cli::LogSettings::default()
            .with_console_level(options.log_level)
            .with_stderr_color(color_config.stderr)
            .init_progress(style)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        log::debug!("Creating context with options: {:?}", options);

        if options.offline && options.remote {
            // `--offline --remote` = pinned-only mode. Both flags accepted
            // together because the routing matrix collapses cleanly:
            // `--offline` overrides `--remote` to no-source-contact, and
            // any tag-addressed resolution must succeed locally or error.
            // Documented in user-guide §Routing and command-line.md.
            log::info!(
                "--offline --remote: pinned-only mode — tag and catalog lookups will not contact a source. \
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

        let printer = Printer::new(color_config.stdout, color_config.stderr);
        let data = DataInterface::new(printer);
        let ui = UserInterface::new(printer, console::Term::stderr().is_term(), options.quiet);
        let api = api::Api::new(options.format, data, options.quiet);

        let (remote_client, remote_index) = if options.offline {
            (None, None)
        } else {
            let client = oci::ClientBuilder::from_env();
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
        );

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
        let config_view = options.as_view(self_exe);
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
        })
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

    /// Whether the global toolchain (`$OCX_HOME/ocx.toml`) was explicitly
    /// selected via `--global` / `OCX_GLOBAL`. Passed to
    /// `ProjectConfig::resolve` so project-tier prologues select the
    /// global file instead of walking the CWD. Mutually exclusive with an
    /// explicit `--project` (enforced by clap `conflicts_with`).
    pub fn global(&self) -> bool {
        self.config_view.global
    }

    /// Fold a subcommand-level `--global` flag into this context's
    /// resolution view, enforcing mutual exclusion with an explicit
    /// project selection.
    ///
    /// `--global` is exposed both at the top level (flattened
    /// `ContextOptions`, before the subcommand) and per project-tier
    /// command (after the subcommand, before positionals — project CLI
    /// convention). Both surfaces denote the *same* logical selector;
    /// this is the single reconciliation seam so the two never diverge
    /// into a parallel pipeline (feedback_extend_dont_duplicate). The
    /// effective selector is the logical OR of the two surfaces.
    ///
    /// # Exclusivity enforcement
    ///
    /// The top-level `--global` + top-level `--project` pair is rejected
    /// by clap (`conflicts_with = "project"` on
    /// [`ContextOptions::global`]) and that path is left untouched. clap
    /// cannot relate a *per-subcommand* `--global` bool to the flattened
    /// top-level `project` arg, so the per-command surface is reconciled
    /// here instead of via a duplicated `conflicts_with` on every command
    /// (extend-don't-duplicate; one enforcement seam).
    ///
    /// When the **per-command** `--global` is set and an **explicit**
    /// project selection is in effect — the `--project` flag or the
    /// `OCX_PROJECT` env var (NOT a project merely discovered via the CWD
    /// walk; `--global` while standing inside a project directory is
    /// legal and global wins by precedence per
    /// adr_global_toolchain_tier.md §Decision 2) — this returns a
    /// [`UsageError`] that classifies to [`ExitCode::UsageError`] (`64`),
    /// mirroring what clap's top-level `conflicts_with` already does for
    /// the top-level pair.
    ///
    /// # Errors
    ///
    /// Returns [`UsageError`] (exit `64`) when `command_global` is set
    /// alongside an explicit `--project` / `OCX_PROJECT` selection.
    pub fn with_command_global(self, command_global: bool) -> Result<Self, ocx_lib::cli::UsageError> {
        if command_global && self.has_explicit_project_selection() {
            return Err(ocx_lib::cli::UsageError::new(
                "--global cannot be combined with an explicit --project / OCX_PROJECT selection",
            ));
        }
        // Reconcile `command_global` into `config_view.global` (logical
        // OR): the top-level and per-command surfaces denote the *same*
        // logical selector, so the effective selector is true when either
        // is set. Folding it here (not a parallel pipeline) keeps the two
        // surfaces from diverging (feedback_extend_dont_duplicate). Both
        // `context.global()` (drives `ProjectConfig::resolve`) and the
        // forwarded `OCX_GLOBAL` read from `config_view.global`, so this
        // single mutation propagates to resolution and subprocess spawn.
        let mut ctx = self;
        ctx.config_view.global = ctx.config_view.global || command_global;
        Ok(ctx)
    }

    /// Whether an *explicit* project file was selected — via the
    /// `--project` flag or the `OCX_PROJECT` env var. A project merely
    /// discovered through the CWD walk is NOT explicit: `--global` from
    /// inside a project tree is legal and the global tier wins by
    /// precedence (adr_global_toolchain_tier.md §Decision 2). This is the
    /// detection half of the exclusivity guard in
    /// [`Self::with_command_global`].
    fn has_explicit_project_selection(&self) -> bool {
        // The discriminator between an *explicit* project selection and a
        // CWD-walk-discovered one is which surface carried the path. The
        // `--project` flag is captured into `config_view.project` (via
        // `ContextOptions::as_view`); `OCX_PROJECT` is the env-var peer.
        // A project found by the CWD walk sets *neither* — so standing
        // inside a project tree and passing `--global` reads as
        // non-explicit and global wins by precedence
        // (adr_global_toolchain_tier.md §Decision 2).
        if self.config_view.project.is_some() {
            return true;
        }
        // `OCX_PROJECT=""` is the loader's escape hatch (treated as
        // unset); mirror that here so an explicitly-cleared env var is
        // not misread as an explicit selection.
        env::var(env::keys::OCX_PROJECT).is_some_and(|v| !v.is_empty())
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

#[cfg(test)]
impl Context {
    /// Assemble a [`Context`] from cheap, no-I/O subsystems for unit tests.
    ///
    /// Only the per-command `--global` reconciliation path
    /// ([`Self::with_command_global`] → [`Self::has_explicit_project_selection`])
    /// is exercised through this constructor; that path reads only
    /// `config_view` and `project_path` and never touches the index,
    /// package manager, client, or filesystem. The heavy fields are
    /// constructed with their cheapest stubs (no network, no disk writes)
    /// purely to satisfy the struct shape.
    fn for_seam_test(project_path: Option<PathBuf>, config_view: env::OcxConfigView) -> Self {
        let fs = file_structure::FileStructure::default();
        let local_index = oci::index::LocalIndex::new(oci::index::LocalConfig {
            tag_store: TagStore::new(fs.tags.root().to_path_buf()),
            blob_store: BlobStore::new(fs.blobs.root().to_path_buf()),
        });
        let default_index = oci::index::Index::from_chained(
            oci::index::LocalIndex::new(oci::index::LocalConfig {
                tag_store: TagStore::new(fs.tags.root().to_path_buf()),
                blob_store: BlobStore::new(fs.blobs.root().to_path_buf()),
            }),
            Vec::new(),
            oci::index::ChainMode::Offline,
        );
        let manager = package_manager::PackageManager::new(
            file_structure::FileStructure::default(),
            oci::index::Index::from_chained(
                oci::index::LocalIndex::new(oci::index::LocalConfig {
                    tag_store: TagStore::new(fs.tags.root().to_path_buf()),
                    blob_store: BlobStore::new(fs.blobs.root().to_path_buf()),
                }),
                Vec::new(),
                oci::index::ChainMode::Offline,
            ),
            None,
            "ocx.sh",
        );
        let printer = Printer::new(false, false);
        let api = api::Api::new(crate::options::Format::default(), DataInterface::new(printer), true);
        let ui = UserInterface::new(printer, false, true);
        Self {
            offline: true,
            project_path,
            remote_client: None,
            remote_index: None,
            local_index,
            file_structure: fs,
            api,
            ui,
            default_index,
            manager,
            default_registry: "ocx.sh".to_string(),
            config_view,
            concurrency: package_manager::Concurrency::Unbounded,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Contract-first spec for the per-command `--global` reconciliation seam.
    //!
    //! Encodes plan_project_toolchain_hardening.md W2-P3 + C2.5 +
    //! adr_global_toolchain_tier.md §Decision 2: a per-subcommand `--global`
    //! combined with an explicit `--project` / `OCX_PROJECT` selection is a
    //! hard usage error (exit 64), mirroring clap's top-level
    //! `conflicts_with` on the flattened pair.
    //!
    //! The spec binds directly to the fallible seam
    //! [`Context::with_command_global`], whose inner
    //! `has_explicit_project_selection` is fully implemented: an explicit
    //! `--project` (carried on `config_view.project`) or a non-empty
    //! `OCX_PROJECT` env var counts as an explicit selection, while a
    //! CWD-walk-discovered project does not. The test pins the contract
    //! that combining a per-command `--global` with such an explicit
    //! selection surfaces as a [`ocx_lib::cli::UsageError`] classifying to
    //! [`ocx_lib::cli::ExitCode::UsageError`] (`64`) and naming `--global`.

    use super::*;
    use ocx_lib::cli::{ClassifyExitCode, ExitCode};

    #[test]
    fn global_conflicts_with_project_is_usage_error() {
        // An explicit `--project` selection is in effect (carried on the
        // context's `project_path`, the seam's explicit-selection signal).
        let mut view = ocx_lib::env::OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        view.project = Some(std::path::PathBuf::from("/abs/explicit/ocx.toml"));
        let ctx = Context::for_seam_test(Some(std::path::PathBuf::from("/abs/explicit/ocx.toml")), view);

        // Per-command `--global = true` while an explicit project selection
        // is in effect → the seam must reject with a UsageError (exit 64).
        // `Context` is not `Debug`, so match instead of `expect_err`.
        let err = match ctx.with_command_global(true) {
            Ok(_) => panic!(
                "--global + explicit --project must be rejected by with_command_global \
                 (adr_global_toolchain_tier.md §Decision 2)"
            ),
            Err(e) => e,
        };
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
