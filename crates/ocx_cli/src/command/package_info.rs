// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci, package::description::Description, publisher::Publisher};

use crate::api::data::package_description::{Inner, PackageDescription, PackageDescriptions};
use crate::options;

/// Show description metadata (title, description, keywords) for one or more
/// packages.
///
/// Reads each description from the `__ocx.desc` tag in the registry. With
/// multiple packages, JSON output is an object keyed by the requested
/// identifier (`{"<id>": {...}|null}`); plain output prints a header line per
/// package.
///
/// `--save-readme` / `--save-logo` write a fixed filename and so require
/// exactly one package.
#[derive(Parser)]
pub struct PackageInfo {
    /// Save the README to this file or directory (single package only).
    #[clap(long)]
    save_readme: Option<PathBuf>,

    /// Save the logo to this file or directory (single package only).
    #[clap(long)]
    save_logo: Option<PathBuf>,

    /// Package repositories to query.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl PackageInfo {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        options::Identifier::reject_duplicate_references(&identifiers)?;

        // `--save-readme` / `--save-logo` write a fixed filename; with more than
        // one package they would collide. clap cannot express this cross-arg
        // rule, so it is a runtime usage error.
        if identifiers.len() > 1 && (self.save_readme.is_some() || self.save_logo.is_some()) {
            return Err(
                ocx_lib::cli::UsageError::new("--save-readme and --save-logo require exactly one package").into(),
            );
        }

        let client = context.remote_client()?.clone();

        // Single secure RAII temp root (mode 0700, random name) instead of a
        // predictable world-writable `ocx-info-{pid}` path — closes the
        // symlink/TOCTOU race (CWE-377/379/59). Bound here so it outlives the
        // drain loop below: `Drop` recursively removes every per-index subdir,
        // including on the `resume_unwind` panic path the old manual
        // `remove_dir_all` loop skipped.
        let temp_root = tempfile::TempDir::new().map_err(|e| anyhow::anyhow!("failed to create temp dir: {e}"))?;

        // Fan out the pulls, each tagged with its input index. `pull_description`
        // returns `crate::Result<Option<Description>>` (not a PackageManager op),
        // so `drain_package_tasks` does not fit; the index-tagged fan-out is
        // inlined here (same shape as `index update`).
        let mut join_set: tokio::task::JoinSet<(usize, ocx_lib::Result<Option<Description>>)> =
            tokio::task::JoinSet::new();
        for (index, identifier) in identifiers.iter().enumerate() {
            let client = client.clone();
            let identifier = identifier.clone();
            let temp_dir = temp_root.path().join(index.to_string());
            join_set.spawn(async move {
                let result = async {
                    tokio::fs::create_dir_all(&temp_dir)
                        .await
                        .map_err(|e| ocx_lib::error::file_error(&temp_dir, e))?;
                    let publisher = Publisher::new(client);
                    publisher.pull_description(&identifier, &temp_dir).await
                }
                .await;
                (index, result)
            });
        }

        // Place successes by index; collect failures with their index so the
        // input-order-first error is the one surfaced (deterministic exit code).
        let mut descriptions: Vec<Option<Option<Description>>> = (0..identifiers.len()).map(|_| None).collect();
        let mut failures: Vec<(usize, ocx_lib::Error)> = Vec::new();
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok((index, Ok(desc))) => descriptions[index] = Some(desc),
                Ok((index, Err(e))) => {
                    log::error!("Failed to fetch description for '{}': {e}", identifiers[index]);
                    failures.push((index, e));
                }
                Err(join_err) => {
                    join_set.abort_all();
                    std::panic::resume_unwind(join_err.into_panic());
                }
            }
        }

        if !failures.is_empty() {
            failures.sort_by_key(|(index, _)| *index);
            let (_, error) = failures.into_iter().next().expect("failures is non-empty");
            return Err(error.into());
        }

        // Every slot is `Some` once no failure remains.
        let descriptions: Vec<Option<Description>> = descriptions
            .into_iter()
            .map(|slot| slot.expect("all slots filled on success"))
            .collect();

        // Save flags are reachable only with a single package (rejected above
        // for N>1), so the first (only) description is the target.
        if (self.save_readme.is_some() || self.save_logo.is_some())
            && let Some(Some(desc)) = descriptions.first()
        {
            self.save_files(desc).await?;
        }

        let entries: Vec<(String, PackageDescription)> = self
            .packages
            .iter()
            .zip(identifiers)
            .zip(descriptions)
            .map(|((raw, identifier), desc)| {
                let inner = desc.as_ref().map(|d| Inner {
                    title: d.annotations.get(oci::annotations::TITLE).cloned(),
                    description: d.annotations.get(oci::annotations::DESCRIPTION).cloned(),
                    keywords: d.annotations.get(oci::annotations::KEYWORDS).cloned(),
                });
                (raw.raw().to_string(), PackageDescription::new(inner, identifier))
            })
            .collect();

        context.api().report(&PackageDescriptions::new(entries))?;

        Ok(ExitCode::SUCCESS)
    }

    /// Writes the README and/or logo to the requested paths. Only invoked for
    /// the single-package case (the flags are rejected for N>1 upstream).
    async fn save_files(&self, desc: &Description) -> anyhow::Result<()> {
        if let Some(ref save_path) = self.save_readme {
            let path = resolve_save_path(save_path, "README.md");
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to create directory {}: {e}", parent.display()))?;
            }
            tokio::fs::write(&path, &desc.readme)
                .await
                .map_err(|e| anyhow::anyhow!("failed to write README to {}: {e}", path.display()))?;
        }

        if let (Some(save_path), Some(logo)) = (&self.save_logo, &desc.logo) {
            let default_name = logo_default_filename(logo.media_type);
            let path = resolve_save_path(save_path, default_name);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to create directory {}: {e}", parent.display()))?;
            }
            tokio::fs::write(&path, &logo.data)
                .await
                .map_err(|e| anyhow::anyhow!("failed to write logo to {}: {e}", path.display()))?;
        }

        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_multiple_positionals() {
        let cmd = PackageInfo::try_parse_from(["info", "a", "b"]).unwrap();
        assert_eq!(cmd.packages.len(), 2);
    }

    #[test]
    fn single_positional_still_parses() {
        let cmd = PackageInfo::try_parse_from(["info", "a"]).unwrap();
        assert_eq!(cmd.packages.len(), 1);
    }

    #[test]
    fn zero_positionals_is_rejected() {
        assert!(PackageInfo::try_parse_from(["info"]).is_err());
    }
}
