use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci, package::metadata::env::exporter::Exporter};

use crate::{api, conventions::*, options, task};

/// Print the resolved environment variables for one or more installed packages.
///
/// Plain format: aligned table with Key, Value, and Type columns where Type is `constant` or `path`.
/// JSON format:  `{"entries": [{"key": "...", "value": "...", "type": "constant"|"path"}, ...]}`.
///
/// This allows external tools (Python scripts, Bazel rules, CI steps) to correctly
/// configure child process environments without going through `ocx exec`.
#[derive(Parser)]
pub struct Env {
    /// Target platforms to consider when resolving packages.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM", num_args = 0..)]
    platforms: Vec<oci::Platform>,

    /// Package identifiers to resolve the environment for.
    #[clap(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Env {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let platforms = platforms_or_default(&self.platforms);
        let identifiers =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;

        let info = task::package::find::Find {
            context: context.clone(),
            file_structure: context.file_structure().clone(),
            platforms: platforms.clone(),
        }
        .find_all(identifiers.clone())
        .await;

        let info = match info {
            Ok(info) => info,
            Err(ocx_lib::Error::PackageNotFound(error)) => {
                if context.is_offline() {
                    log::error!("Package not found and offline mode is enabled: {}", error);
                    return Err(anyhow::anyhow!(
                        "Package not found and offline mode is enabled: {}",
                        error
                    ));
                } else {
                    log::info!("Package not found, will attempt to install: {}", error);
                    task::package::install::Install {
                        context: context.clone(),
                        file_structure: context.file_structure().clone(),
                        platforms,
                        candidate: false,
                        select: false,
                    }
                    .install_all(identifiers)
                    .await?
                }
            }
            Err(e) => return Err(anyhow::anyhow!("Error finding package: {:?}", e)),
        };

        let mut all_entries: Vec<api::data::env::EnvEntry> = Vec::new();

        for package_info in info {
            let mut exporter = Exporter::new(&package_info.content);
            if let Some(metadata_env) = package_info.metadata.env() {
                for v in metadata_env {
                    exporter.add(v)?;
                }
            }
            for entry in exporter.take() {
                all_entries.push(api::data::env::EnvEntry {
                    key: entry.key,
                    value: entry.value,
                    kind: entry.kind,
                });
            }
        }

        context.api().report_env(api::data::env::EnvVars::new(all_entries))?;

        Ok(ExitCode::SUCCESS)
    }
}
