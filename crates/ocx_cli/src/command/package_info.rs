// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{oci, publisher::Publisher};

use crate::api::data::package_description::{Inner, PackageDescription};
use crate::options;

/// Show description metadata (title, description, keywords) for a package.
///
/// Reads the description from the __ocx.desc tag in the registry. Optionally
/// saves the README and/or logo to local files.
#[derive(Parser)]
pub struct PackageInfo {
    /// Save the README to this file or directory.
    #[clap(long)]
    save_readme: Option<PathBuf>,

    /// Save the logo to this file or directory.
    #[clap(long)]
    save_logo: Option<PathBuf>,

    /// The package repository to query.
    identifier: options::Identifier,
}

impl PackageInfo {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        let publisher = Publisher::new(context.remote_client()?.clone());

        let temp_dir = std::env::temp_dir().join(format!("ocx-info-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).map_err(|e| anyhow::anyhow!("failed to create temp dir: {e}"))?;
        let desc = publisher.pull_description(&identifier, &temp_dir).await?;
        let _ = std::fs::remove_dir_all(&temp_dir);

        let inner = match desc {
            Some(ref desc) => {
                if let Some(ref save_path) = self.save_readme {
                    let path = resolve_save_path(save_path, "README.md");
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| anyhow::anyhow!("failed to create directory {}: {e}", parent.display()))?;
                    }
                    std::fs::write(&path, &desc.readme)
                        .map_err(|e| anyhow::anyhow!("failed to write README to {}: {e}", path.display()))?;
                }

                if let Some(ref save_path) = self.save_logo {
                    match &desc.logo {
                        Some(logo) => {
                            let default_name = logo_default_filename(logo.media_type);
                            let path = resolve_save_path(save_path, default_name);
                            if let Some(parent) = path.parent() {
                                std::fs::create_dir_all(parent).map_err(|e| {
                                    anyhow::anyhow!("failed to create directory {}: {e}", parent.display())
                                })?;
                            }
                            std::fs::write(&path, &logo.data)
                                .map_err(|e| anyhow::anyhow!("failed to write logo to {}: {e}", path.display()))?;
                        }
                        None => {
                            return Err(anyhow::anyhow!("no logo found in description for {identifier}"));
                        }
                    }
                }

                Some(Inner {
                    title: desc.annotations.get(oci::annotations::TITLE).cloned(),
                    description: desc.annotations.get(oci::annotations::DESCRIPTION).cloned(),
                    keywords: desc.annotations.get(oci::annotations::KEYWORDS).cloned(),
                })
            }
            None => None,
        };

        let report = PackageDescription::new(inner, identifier.to_string());
        context.api().report_package_description(report)?;

        Ok(ExitCode::SUCCESS)
    }
}

/// If the path is an existing directory, append the default filename. Otherwise use as-is.
fn resolve_save_path(path: &std::path::Path, default_filename: &str) -> PathBuf {
    if path.is_dir() {
        path.join(default_filename)
    } else {
        path.to_path_buf()
    }
}

fn logo_default_filename(media_type: &str) -> &str {
    match media_type {
        "image/svg+xml" => "logo.svg",
        _ => "logo.png",
    }
}
