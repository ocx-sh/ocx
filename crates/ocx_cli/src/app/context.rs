// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

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
    remote_client: Option<oci::Client>,
    remote_index: Option<oci::index::RemoteIndex>,
    local_index: oci::index::LocalIndex,
    file_structure: file_structure::FileStructure,
    api: api::Api,
    default_index: oci::index::Index,
    manager: package_manager::PackageManager,
    default_registry: String,
    config_view: env::OcxConfigView,
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

        let config = ConfigLoader::load(ConfigInputs {
            explicit_path: options.config.as_deref(),
            cwd: None,
        })
        .await?;

        let api = api::Api::new(options.format, Printer::new(color_config.stdout));

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

        Ok(Context {
            remote_client,
            remote_index,
            offline: options.offline,
            file_structure,
            api,
            local_index,
            default_index: selected_index,
            manager,
            default_registry,
            config_view,
        })
    }

    pub fn is_offline(&self) -> bool {
        self.offline
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
}
