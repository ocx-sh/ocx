use std::{collections::HashMap, process::ExitCode};

use clap::Parser;
use ocx_lib::{oci, package::install_info, symlink};

use crate::{api, conventions::platforms_or_default, options, task};

/// Set the current version of one or more packages.
///
/// Resolves each package via the index and verifies that its content is present
/// in the local object store, then updates the `current` symlink to point
/// directly to that content.  No downloading is performed.
#[derive(Parser)]
pub struct Select {
    /// Platforms to consider when resolving the package. Defaults to the current platform.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM")]
    platforms: Vec<oci::Platform>,

    /// Package identifiers to select.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Select {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifiers = options::Identifier::transform_all(
            self.packages.clone().into_iter(),
            context.default_registry(),
        )?;

        let fs = context.file_structure().clone();
        let platforms = platforms_or_default(&self.platforms);

        // Find resolves the identifier via the index and verifies the content
        // exists in the local object store. It does not auto-install.
        let package_infos = task::package::find::Find {
            context: context.clone(),
            file_structure: fs.clone(),
            platforms,
        }
        .find_all(identifiers)
        .await?;

        let mut packages = HashMap::with_capacity(package_infos.len());

        for (raw, info) in self.packages.iter().zip(package_infos) {
            let current_path = fs.installs.current(&info.identifier);
            symlink::update(&info.content, &current_path)?;

            packages.insert(
                raw.raw().to_string(),
                install_info::InstallInfo {
                    identifier: info.identifier,
                    metadata: info.metadata,
                    content: current_path,
                },
            );
        }

        context
            .api()
            .report_installs(api::data::install::Installs::new(packages))?;

        Ok(ExitCode::SUCCESS)
    }
}
