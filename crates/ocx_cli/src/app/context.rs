// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use ocx_lib::{
    ConfigInputs, ConfigLoader,
    cli::{ColorModeConfig, Printer},
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
        // chain: explicit flag > env > CWD walk > home fallback.
        let project_path = options.project.clone();

        let cwd = env::current_dir()?;
        let config = ConfigLoader::load(ConfigInputs {
            explicit_path: options.config.as_deref(),
            explicit_project_path: options.project.as_deref(),
            cwd: Some(&cwd),
        })
        .await?;

        let api = api::Api::new(options.format, Printer::new(color_config.stdout), options.quiet);

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
