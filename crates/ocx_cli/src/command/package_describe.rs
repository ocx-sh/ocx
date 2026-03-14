// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci, package, publisher::Publisher};

use crate::options;

/// Push or update description metadata for a package repository.
///
/// Pushes a README, optional logo, and catalog annotations to the __ocx.desc tag.
/// When updating an existing description, only the provided fields are changed —
/// omitted fields are preserved from the current description.
#[derive(Parser)]
pub struct PackageDescribe {
    /// Path to the README markdown file.
    #[clap(long)]
    readme: Option<std::path::PathBuf>,

    /// Path to an optional logo image (PNG or SVG).
    #[clap(long)]
    logo: Option<std::path::PathBuf>,

    /// Short title for catalog display (sets org.opencontainers.image.title).
    #[clap(long)]
    title: Option<String>,

    /// One-line summary for catalog display (sets org.opencontainers.image.description).
    #[clap(long)]
    description: Option<String>,

    /// Comma-separated search keywords (sets sh.ocx.keywords).
    #[clap(long)]
    keywords: Option<String>,

    /// The package repository. Tag is ignored; always pushes to __ocx.desc.
    identifier: options::Identifier,
}

impl PackageDescribe {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        let has_updates = self.readme.is_some()
            || self.logo.is_some()
            || self.title.is_some()
            || self.description.is_some()
            || self.keywords.is_some();

        if !has_updates {
            return Err(anyhow::anyhow!(
                "nothing to update — provide at least one of --readme, --logo, --title, --description, or --keywords"
            ));
        }

        let publisher = Publisher::new(context.remote_client()?.clone());

        // Pull existing description for merge.
        let temp_dir = std::env::temp_dir().join(format!("ocx-describe-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).map_err(|e| anyhow::anyhow!("failed to create temp dir: {e}"))?;
        let existing = publisher.pull_description(&identifier, &temp_dir).await?;
        let _ = std::fs::remove_dir_all(&temp_dir);

        // Build the merged description.
        let (readme, frontmatter) = match &self.readme {
            Some(path) => {
                let data = std::fs::read(path)
                    .map_err(|e| anyhow::anyhow!("failed to read README at {}: {e}", path.display()))?;
                let text = std::str::from_utf8(&data).map_err(|e| anyhow::anyhow!("README is not valid UTF-8: {e}"))?;
                let parsed = package::description::parse_readme(text);
                (parsed.body, parsed.frontmatter)
            }
            None => match &existing {
                Some(desc) => (desc.readme.clone(), package::description::Frontmatter::default()),
                None => {
                    return Err(anyhow::anyhow!(
                        "no existing description found — --readme is required for the first push"
                    ));
                }
            },
        };

        let logo = match &self.logo {
            Some(path) => {
                let data = std::fs::read(path)
                    .map_err(|e| anyhow::anyhow!("failed to read logo at {}: {e}", path.display()))?;
                let media_type = package::description::logo_media_type(path)?;
                Some(package::description::Logo { data, media_type })
            }
            None => existing
                .as_ref()
                .and_then(|d| d.logo.as_ref())
                .map(|l| package::description::Logo {
                    data: l.data.clone(),
                    media_type: l.media_type,
                }),
        };

        // Merge annotations: existing → frontmatter → CLI flags.
        let mut annotations = existing.as_ref().map(|d| d.annotations.clone()).unwrap_or_default();
        Self::set_annotation(&mut annotations, oci::annotations::TITLE, &frontmatter.title);
        Self::set_annotation(
            &mut annotations,
            oci::annotations::DESCRIPTION,
            &frontmatter.description,
        );
        Self::set_annotation(
            &mut annotations,
            oci::annotations::KEYWORDS,
            &frontmatter.keywords.map(|k| k.0),
        );
        Self::set_annotation(&mut annotations, oci::annotations::TITLE, &self.title);
        Self::set_annotation(&mut annotations, oci::annotations::DESCRIPTION, &self.description);
        Self::set_annotation(&mut annotations, oci::annotations::KEYWORDS, &self.keywords);

        let desc = package::description::Description {
            readme,
            logo,
            annotations,
        };

        publisher.push_description(&identifier, &desc).await?;

        log::info!("Pushed description for {}", identifier);
        Ok(ExitCode::SUCCESS)
    }

    fn set_annotation(annotations: &mut BTreeMap<String, String>, key: &str, value: &Option<String>) {
        if let Some(v) = value {
            annotations.insert(key.to_string(), v.clone());
        }
    }
}
