// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::{
    collections::{BTreeSet, HashMap},
    process::ExitCode,
};

use clap::Parser;
use ocx_lib::{log, oci, oci::index::IndexOperation, package::version::Version};

use crate::{api, options};

/// List tags available for a package.
///
/// Identifiers carrying a digest (`@sha256:...`) are rejected for tag and
/// variant listing — a digest narrows nothing there; use
/// `ocx package info <pkg>@<digest>` for a single artifact. With `--platforms`
/// a digest is accepted and resolves to that one artifact's platform set.
#[derive(Parser)]
pub struct IndexList {
    /// Shows which platforms are available for each package.
    /// Uses the tag from the identifier, or `latest` if none specified.
    #[arg(long, conflicts_with = "variants")]
    platforms: bool,

    /// Lists unique variant names found in the tags.
    #[arg(long, conflicts_with = "platforms")]
    variants: bool,

    /// Package identifiers to list the available versions for.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

type ResolvedTags = Vec<(String, oci::Identifier, Vec<String>)>;

impl IndexList {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // `index list` enumerates tags, where a digest-bearing identifier narrows
        // nothing — reject it early with a usage error pointing at `package info`.
        // With `--platforms` a digest DOES resolve to that one artifact's platform
        // set (report_platforms handles it directly), so it is accepted there.
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        for (raw, identifier) in self.packages.iter().zip(&identifiers) {
            if identifier.digest().is_some() && !self.platforms {
                anyhow::bail!(
                    "`ocx index list` lists tags and does not accept digest-pinned identifiers. \
                     Use `ocx package info {raw}` for a single artifact, or drop the @digest suffix.",
                    raw = raw.raw(),
                );
            }
        }

        let resolved = self.resolve_tags(&context, identifiers).await?;

        if self.variants {
            Self::report_variants(&context, resolved)?;
        } else if self.platforms {
            Self::report_platforms(&context, resolved).await?;
        } else {
            Self::report_tags(&context, resolved)?;
        }

        Ok(ExitCode::SUCCESS)
    }

    async fn resolve_tags(
        &self,
        context: &crate::app::Context,
        identifiers: Vec<oci::Identifier>,
    ) -> anyhow::Result<ResolvedTags> {
        let futures = self.packages.iter().zip(identifiers).map(|(package, identifier)| {
            let context = context.clone();
            async move {
                // A digest-pinned identifier with `--platforms` resolves straight to
                // that one artifact (`report_platforms` handles it directly and never
                // reads `tags` for this branch) — skip `list_tags` entirely so it
                // never emits an ordinary "not found in the index" warning for a
                // lookup that was never a tag lookup.
                let tags = if identifier.digest().is_some() && self.platforms {
                    Vec::new()
                } else {
                    let mut tags = match context.default_index().list_tags(&identifier).await? {
                        Some(tags) => tags,
                        None => {
                            log::warn!("Package '{}' not found in the index.", identifier);
                            Vec::new()
                        }
                    };
                    if let Some(requested_tag) = identifier.tag() {
                        tags.retain(|t| t == requested_tag);
                    }
                    tags
                };
                Ok((package.raw().to_string(), identifier, tags))
            }
        });

        futures::future::join_all(futures)
            .await
            .into_iter()
            .collect::<anyhow::Result<Vec<_>>>()
    }

    fn report_tags(context: &crate::app::Context, resolved: ResolvedTags) -> anyhow::Result<()> {
        let tags_report = resolved
            .into_iter()
            .map(|(package, _, tags)| (package, tags.into_iter()))
            .collect::<HashMap<_, _>>();
        context.api().report(&api::data::tag::Tags::from_tags(tags_report))
    }

    fn report_variants(context: &crate::app::Context, resolved: ResolvedTags) -> anyhow::Result<()> {
        let variants_report = resolved
            .into_iter()
            .map(|(package, _, tags)| {
                let versions: Vec<Version> = tags.iter().filter_map(|t| Version::parse(t)).collect();
                let has_default = versions.iter().any(|v| v.variant().is_none());
                let mut variant_names: Vec<String> = versions
                    .iter()
                    .filter_map(|v| v.variant().map(|s| s.to_string()))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                if has_default {
                    variant_names.insert(0, String::new());
                }
                (package, variant_names)
            })
            .collect::<HashMap<_, _>>();
        context
            .api()
            .report(&api::data::tag::Tags::from_variants(variants_report))
    }

    /// Fetch platforms for a single tag per package (the requested tag, or `latest`).
    async fn report_platforms(context: &crate::app::Context, resolved: ResolvedTags) -> anyhow::Result<()> {
        let mut platforms_report = HashMap::new();
        for (package, identifier, tags) in resolved {
            // A digest-pinned identifier resolves straight to that one artifact's
            // platform set (the observation object under its digest) — no tag
            // filtering, and no yank check (a digest pin bypasses the tag lane).
            let target = if identifier.digest().is_some() {
                identifier.clone()
            } else {
                let tag = identifier
                    .tag()
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "latest".to_string());
                if !tags.contains(&tag) {
                    log::warn!("Tag '{}' not found for '{}' - skipping.", tag, package);
                    platforms_report.insert(package, Vec::new());
                    continue;
                }
                identifier.clone_with_tag(&tag)
            };

            let platforms = match context
                .default_index()
                .fetch_manifest(&target, IndexOperation::Query)
                .await?
            {
                Some((_, manifest)) => oci::Platform::from_manifest(&manifest)?
                    .into_iter()
                    .map(|p| p.to_string())
                    .collect(),
                None => {
                    log::warn!("Manifest not found for '{}' - skipping.", target);
                    Vec::new()
                }
            };
            platforms_report.insert(package, platforms);
        }
        context
            .api()
            .report(&api::data::tag::Tags::from_platforms(platforms_report))
    }
}
