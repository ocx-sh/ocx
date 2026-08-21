// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci, oci::client::error::ClientError, package, package::tag::InternalTag, publisher::Publisher};

use crate::options;

/// Push or update description metadata for a package repository.
///
/// Pushes a README, optional logo, and catalog annotations to the __ocx.desc tag.
/// When updating an existing description, only the provided fields are changed —
/// omitted fields are preserved from the current description.
#[derive(Parser)]
pub struct PackageDescribe {
    /// Copy the whole description from another package repository.
    ///
    /// Promotes README, logo and catalog annotations verbatim, so a staging
    /// repository's catalog page can be published to production without
    /// re-authoring it. Mutually exclusive with the field flags: this is a copy,
    /// not a merge, and mixing the two would silently pick a winner.
    #[clap(long = "from", value_name = "SOURCE", conflicts_with_all = ["readme", "logo", "title", "description", "keywords"])]
    from: Option<options::Identifier>,

    /// Path to the README markdown file.
    #[clap(long)]
    readme: Option<std::path::PathBuf>,

    /// Path to an optional logo image (PNG or SVG).
    ///
    /// The file's bytes must be the format its extension names. A file that is not
    /// a real PNG or SVG fails the command without touching the published
    /// description, so a broken checkout cannot blank a catalog logo.
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

        if let Some(source) = &self.from {
            return self.copy_from(context, source, &identifier).await;
        }

        let has_updates = self.readme.is_some()
            || self.logo.is_some()
            || self.title.is_some()
            || self.description.is_some()
            || self.keywords.is_some();

        if !has_updates {
            return Err(anyhow::anyhow!(
                "nothing to update - provide at least one of --readme, --logo, --title, --description, or --keywords, or --from to copy a description from another repository"
            ));
        }

        let publisher = Publisher::new(context.remote_client()?.clone());
        publisher.ensure_auth(&identifier).await?;

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
                        "no existing description found - --readme is required for the first push"
                    ));
                }
            },
        };

        let logo = match &self.logo {
            Some(path) => Some(package::description::load_logo(path)?),
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

    /// Copies `source`'s published description onto `target` unchanged.
    ///
    /// Deliberately not a merge: the target's own description is replaced
    /// wholesale, which is what "promote the catalog page I reviewed in staging"
    /// means. A merge would leave the target carrying a mixture nobody wrote.
    async fn copy_from(
        &self,
        context: crate::app::Context,
        source: &options::Identifier,
        target: &oci::Identifier,
    ) -> anyhow::Result<ExitCode> {
        let source = source.with_domain(context.default_registry())?;
        let publisher = Publisher::new(context.remote_client()?.clone());
        publisher.ensure_auth(target).await?;

        let temp_dir = tempfile::tempdir()?;
        // An undescribed source is an expected, actionable outcome, not an
        // unclassified failure: a bare `anyhow!` fell through the chain walk to
        // exit 1, which the pinned table reserves for "no classification fits"
        // (EXIT-04). The typed cause is the one `pull_description` swallowed
        // into `Ok(None)` on the way here, and it classifies to 79.
        //
        // 79 covers both readings — repository absent, and repository present
        // but never described. Telling them apart costs a second round trip
        // (a tag listing) to change nothing the user would do differently, so
        // the message names the tag that was missing and stops there.
        let description = publisher
            .pull_description(&source, temp_dir.path())
            .await?
            .ok_or_else(|| no_description_to_copy(&source))?;
        publisher.push_description(target, &description).await?;

        log::info!("Copied description from {source} to {target}");
        Ok(ExitCode::SUCCESS)
    }

    fn set_annotation(annotations: &mut BTreeMap<String, String>, key: &str, value: &Option<String>) {
        if let Some(v) = value {
            annotations.insert(key.to_string(), v.clone());
        }
    }
}

/// The refusal when `--from` names a repository that publishes no description.
///
/// Carries the cause [`Publisher::pull_description`] swallowed into `Ok(None)`
/// on the way here, so the chain walk reaches a classifiable error and the
/// process exits 79 rather than the unclassified 1.
fn no_description_to_copy(source: &oci::Identifier) -> anyhow::Error {
    anyhow::Error::new(ClientError::ManifestNotFound(format!(
        "{source}:{}",
        InternalTag::DESCRIPTION_TAG
    )))
    .context(format!("{source} has no description to copy"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "The source has no description" is an expected, actionable outcome, and
    /// the pinned table reserves 1 for a classification fall-through with no
    /// application meaning (EXIT-04). 79 is what a script branches on.
    ///
    /// Positive control below: a bare `anyhow!` carrying the same sentence is
    /// the shape this replaced, and it still classifies to 1 — so a green here
    /// cannot be the walker classifying every error as 79.
    #[test]
    fn an_undescribed_source_exits_not_found() {
        let source: oci::Identifier = "dev.example.com/acme/tool:1.4.2".parse().expect("identifier");
        let error = no_description_to_copy(&source);

        assert_eq!(
            crate::app::classify_error(error.as_ref()),
            ocx_lib::cli::ExitCode::NotFound
        );
        assert_eq!(ocx_lib::cli::ExitCode::NotFound as u8, 79);

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("has no description to copy"),
            "the user-facing sentence must survive the typed cause: {rendered}"
        );
        assert!(
            rendered.contains(InternalTag::DESCRIPTION_TAG),
            "the message must name the tag that was missing: {rendered}"
        );

        let untyped = anyhow::anyhow!("{source} has no description to copy");
        assert_eq!(
            crate::app::classify_error(untyped.as_ref()),
            ocx_lib::cli::ExitCode::Failure,
            "control: the prose alone classifies to 1, so the 79 above comes from the cause"
        );
    }
}
