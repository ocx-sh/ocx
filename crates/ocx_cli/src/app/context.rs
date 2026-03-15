// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::{
    cli::{ColorModeConfig, Printer},
    env, file_structure, log,
    oci::{self, index},
    package_manager,
};

use crate::{api, app::log_settings};

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
}

impl Context {
    pub async fn try_init(options: &ContextOptions, color_config: ColorModeConfig) -> anyhow::Result<Context> {
        log_settings::init_with_indicatif(
            ocx_lib::cli::LogSettings::default()
                .with_console_level(options.log_level)
                .with_stderr_color(color_config.stderr),
        )?;

        log::debug!("Creating context with options: {:?}", options);

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
        let local_index = index::LocalIndex::new(index::LocalConfig {
            root: options
                .index
                .clone()
                .or_else(|| env::var("OCX_INDEX").map(std::path::PathBuf::from))
                .unwrap_or_else(|| file_structure.index.root().clone()),
        });

        let selected_index = if options.remote {
            if let Some(remote_index) = &remote_index {
                index::Index::from_remote(remote_index.clone())
            } else {
                return Err(anyhow::anyhow!("Remote index is not available in offline mode."));
            }
        } else {
            index::Index::from_local(local_index.clone())
        };

        let default_registry = env::string("OCX_DEFAULT_REGISTRY", ocx_lib::oci::DEFAULT_REGISTRY.into());

        let manager = package_manager::PackageManager::new(
            file_structure.clone(),
            selected_index.clone(),
            remote_client.clone(),
            &default_registry,
        );

        Ok(Context {
            remote_client,
            remote_index,
            offline: options.offline,
            file_structure,
            api,
            local_index,
            default_index: selected_index,
            manager,
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

    #[allow(dead_code)]
    pub fn local_index(&self) -> &oci::index::LocalIndex {
        &self.local_index
    }

    pub fn local_index_mut(&mut self) -> &mut oci::index::LocalIndex {
        &mut self.local_index
    }

    pub fn default_index(&self) -> &oci::index::Index {
        &self.default_index
    }

    pub fn default_registry(&self) -> String {
        env::string("OCX_DEFAULT_REGISTRY", ocx_lib::oci::DEFAULT_REGISTRY.into())
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
}

#[allow(dead_code)]
fn default_offline_mode() -> bool {
    ocx_lib::env::flag("OCX_OFFLINE", false)
}
