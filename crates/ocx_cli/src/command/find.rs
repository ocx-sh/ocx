// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

use crate::{api, conventions::platforms_or_default, options};

/// Resolve one or more packages and print their content directory paths.
///
/// By default, the content-addressed object-store path is returned.  Use
/// `--candidate` or `--current` to return the stable install symlink path
/// instead — useful when the path is embedded in editor configs, Makefiles,
/// or shell scripts that should not change on every package update.
///
/// No downloading is performed — the package must already be installed.
///
/// Useful for scripting (use `--format json` for machine-readable output):
///
///   cmake_root=$(ocx find --candidate --format json cmake:3.28 | jq -r '.["cmake:3.28"]')
#[derive(Parser)]
pub struct Find {
    #[clap(flatten)]
    platforms: options::PlatformsFlag,

    #[clap(flatten)]
    content_path: options::ContentPath,

    /// Package identifiers to resolve.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Find {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        let manager = context.manager();
        let fs = context.file_structure();

        let entries: Vec<api::data::paths::PathEntry> = if let Some(kind) = self.content_path.symlink_kind() {
            // Validate the symlink resolves and the package is installed,
            // then report the symlink anchor itself (the stable per-repo
            // path that targets the package root). Consumers traverse into
            // `<anchor>/content` or `<anchor>/entrypoints` as needed.
            let _infos = manager.find_symlink_all(identifiers.clone(), kind).await?;
            self.packages
                .iter()
                .zip(identifiers.iter())
                .map(|(raw, id)| api::data::paths::PathEntry {
                    package: raw.raw().to_string(),
                    path: fs.symlinks.symlink(id, kind),
                })
                .collect()
        } else {
            let platforms = platforms_or_default(self.platforms.as_slice());
            let infos = manager.find_all(identifiers, platforms).await?;
            self.packages
                .iter()
                .zip(infos)
                .map(|(raw, info)| api::data::paths::PathEntry {
                    package: raw.raw().to_string(),
                    path: info.content,
                })
                .collect()
        };

        context.api().report(&api::data::paths::Paths::new(entries))?;

        Ok(ExitCode::SUCCESS)
    }
}
