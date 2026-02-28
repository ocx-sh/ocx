use std::process::ExitCode;

use clap::Parser;
use ocx_lib::oci;

use crate::{api, conventions::platforms_or_default, options, task};

/// Resolve one or more packages and print their content directory paths.
///
/// By default, the content-addressed object-store path is returned.  Use
/// `--candidate` or `--current` to return the stable install symlink path
/// instead — useful when the path is embedded in editor configs, Makefiles,
/// or shell scripts that should not change on every package update.
///
/// No downloading is performed — the package must already be installed.
///
/// Useful for scripting (use `--json` for machine-readable output):
///
///   cmake_root=$(ocx find --candidate --json cmake:3.28 | jq -r '.["cmake:3.28"]')
#[derive(Parser)]
pub struct Find {
    /// Platforms to consider when resolving the package. Defaults to the current platform.
    /// Ignored when `--candidate` or `--current` is used.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM")]
    platforms: Vec<oci::Platform>,

    #[clap(flatten)]
    content_path: options::ContentPath,

    /// Package identifiers to resolve.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Find {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifiers = options::Identifier::transform_all(
            self.packages.clone().into_iter(),
            context.default_registry(),
        )?;

        let fs = context.file_structure().clone();

        let infos = if let Some(kind) = self.content_path.symlink_kind() {
            task::package::find_symlink::FindSymlink {
                context: context.clone(),
                file_structure: fs,
                kind,
            }
            .find_all(identifiers)
            .await?
        } else {
            let platforms = platforms_or_default(&self.platforms);
            task::package::find::Find {
                context: context.clone(),
                file_structure: fs,
                platforms,
            }
            .find_all(identifiers)
            .await?
        };

        let entries = self
            .packages
            .iter()
            .zip(infos)
            .map(|(raw, info)| api::data::paths::PathEntry {
                package: raw.raw().to_string(),
                path: info.content,
            })
            .collect();

        context
            .api()
            .report_paths(api::data::paths::Paths::new(entries))?;

        Ok(ExitCode::SUCCESS)
    }
}
