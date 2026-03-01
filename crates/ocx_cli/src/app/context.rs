use ocx_lib::{
    env, file_structure, log,
    oci::{self, index},
    package_manager,
};

use crate::{api, app::LogSettings};

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
    pub async fn try_init(options: &ContextOptions) -> anyhow::Result<Context> {
        LogSettings::default().with_console_level(options.log_level).init()?;

        log::debug!("Creating context with options: {:?}", options);

        let api = api::Api::new(options.format);

        let (remote_client, remote_index) = if options.offline {
            (None, None)
        } else {
            let client = oci::ClientBuilder::new().build();
            (
                Some(client.clone()),
                Some(index::RemoteIndex::new(index::RemoteConfig { client })),
            )
        };
        let file_structure = file_structure::FileStructure::new();
        let local_index = index::LocalIndex::new(index::LocalConfig {
            root: match &options.index {
                Some(path) => path.clone(),
                None => file_structure.index.root().clone(),
            },
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

fn default_offline_mode() -> bool {
    ocx_lib::env::flag("OCX_OFFLINE", false)
}
