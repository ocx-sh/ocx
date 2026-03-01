use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{oci, package::metadata::env::exporter::Exporter};

use crate::{api, conventions::*, options};

/// Print the resolved environment variables for one or more installed packages.
///
/// Plain format: aligned table with Key, Value, and Type columns where Type is `constant` or `path`.
/// JSON format:  `{"entries": [{"key": "...", "value": "...", "type": "constant"|"path"}, ...]}`.
///
/// This allows external tools (Python scripts, Bazel rules, CI steps) to correctly
/// configure child process environments without going through `ocx exec`.
///
/// By default, env values are rooted in the content-addressed object store and
/// may change when a package is updated.  Use `--candidate` or `--current` to
/// root them in a stable symlink path instead — suitable for embedding in editor
/// or IDE configuration files that should not change on every package update.
/// See the path resolution modes documentation for details.
#[derive(Parser)]
pub struct Env {
    /// Target platforms to consider when resolving packages.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM", num_args = 0..)]
    platforms: Vec<oci::Platform>,

    #[clap(flatten)]
    content_path: options::ContentPath,

    /// Package identifiers to resolve the environment for.
    #[clap(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Env {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let platforms = platforms_or_default(&self.platforms);
        let identifiers =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;

        let manager = context.manager();

        let info = if let Some(kind) = self.content_path.symlink_kind() {
            manager.find_symlink_all(identifiers, kind).await?
        } else {
            manager.find_or_install_all(identifiers, platforms).await?
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
