// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use ocx_lib::cli::tracing_indicatif::span_ext::IndicatifSpanExt;
use ocx_lib::log;
use ocx_lib::oci::Platform;
use tracing::info_span;

use super::options::{self, SyncOptions};
use crate::error::MirrorError;
use crate::filter;
use crate::normalizer;
use crate::pipeline::mirror_result::MirrorResult;
use crate::pipeline::mirror_task::{MirrorTask, VariantContext};
use crate::pipeline::orchestrator;
use crate::resolver;
use crate::resolver::asset_resolution::AssetResolution;
use crate::source;
use crate::spec::{self, MirrorSpec};
use crate::version_platform_map::VersionPlatformMap;
use ocx_lib::oci::ClientBuilder;
use ocx_lib::package::version::Version;
use ocx_lib::publisher::Publisher;

#[derive(clap::Args)]
pub struct Sync {
    /// Path to the mirror spec YAML file
    pub spec: PathBuf,

    #[clap(flatten)]
    pub options: SyncOptions,
}

impl Sync {
    pub async fn execute(&self, printer: &ocx_lib::cli::Printer) -> Result<(), MirrorError> {
        let spec_path = &self.spec;
        let spec = spec::load_spec(spec_path).await?;
        let spec_dir = spec_path.parent().unwrap_or(std::path::Path::new("."));

        // Authenticate with target registry before starting progress bars.
        // This ensures any credential prompt (e.g., GPG for Docker credential helpers)
        // appears on a clean terminal, not interleaved with indicatif output.
        let publisher = Publisher::new(ClientBuilder::from_env());
        let identifier = ocx_lib::oci::Identifier::new_registry(&spec.target.repository, &spec.target.registry);
        log::debug!("[{}] Fetching existing tags from {}", spec.name, identifier);
        let all_tags: Vec<String> = publisher.list_tags(identifier.clone()).await.unwrap_or_default();
        log::debug!("[{}] Found {} existing tags", spec.name, all_tags.len());

        // List upstream versions
        let prep_span = info_span!("preparing");
        prep_span.pb_set_message(&spec.name);
        let _prep_guard = prep_span.entered();

        let upstream_versions = list_upstream_versions(&spec, spec_dir).await?;
        log::debug!("[{}] Found {} upstream versions", spec.name, upstream_versions.len());

        // Generate build timestamp
        let build_ts = normalizer::build_timestamp(&spec.build_timestamp);
        log::debug!("[{}] Build timestamp: {:?}", spec.name, build_ts);

        // Resolve assets per variant + normalize versions
        let effective_variants = spec.effective_variants();
        let mut resolved_versions = Vec::new();

        for variant in &effective_variants {
            let patterns = variant
                .assets
                .compiled()
                .map_err(|e| MirrorError::SpecInvalid(vec![e]))?;

            for version_info in &upstream_versions {
                match resolver::resolve_assets(&version_info.assets, &patterns) {
                    AssetResolution::Resolved(platforms) => {
                        match normalizer::normalize_version(&version_info.version, &build_ts) {
                            Ok(normalized) => {
                                // Prepend variant prefix to the normalized version tag
                                let tagged = match &variant.name {
                                    Some(name) => format!("{name}-{normalized}"),
                                    None => normalized,
                                };
                                log::debug!(
                                    "[{}] Resolved version {} -> {} ({} platforms)",
                                    spec.name,
                                    version_info.version,
                                    tagged,
                                    platforms.len()
                                );
                                resolved_versions.push(filter::ResolvedVersion {
                                    version: version_info.version.clone(),
                                    normalized_version: tagged,
                                    variant: variant.name.clone(),
                                    platforms,
                                    is_prerelease: version_info.is_prerelease,
                                });
                            }
                            Err(e) => {
                                log::warn!("[{}] Skipping version {}: {e}", spec.name, version_info.version);
                            }
                        }
                    }
                    AssetResolution::Ambiguous(amb) => {
                        for a in &amb {
                            log::warn!(
                                "[{}] Ambiguous asset match for version {} on {}: {:?}",
                                spec.name,
                                version_info.version,
                                a.platform,
                                a.matched_assets
                            );
                        }
                    }
                }
            }
        }

        // Identify source versions that already have tags on the registry.
        // Include variant-prefixed forms so we detect already-mirrored variant tags.
        let source_version_tags: HashSet<String> = resolved_versions
            .iter()
            .filter_map(|rv| {
                let v = Version::parse(&rv.version)?;
                Some(v.to_string())
            })
            .chain(resolved_versions.iter().filter_map(|rv| {
                let name = rv.variant.as_ref()?;
                let v = Version::parse(&format!("{name}-{}", rv.version))?;
                Some(v.to_string())
            }))
            .collect();

        let tags_needing_platform_check: Vec<&str> = all_tags
            .iter()
            .filter(|t| source_version_tags.contains(t.as_str()))
            .map(String::as_str)
            .collect();

        // Build platform-aware version map.
        // Fetch manifests for matching tags to determine which platforms are already pushed.
        let mut platform_info: BTreeMap<Version, HashSet<Platform>> = BTreeMap::new();
        for tag in &tags_needing_platform_check {
            let tag_id = identifier.clone_with_tag(tag.to_string());
            match publisher.client().fetch_manifest(&tag_id).await {
                Ok((_, manifest)) => {
                    if let Some(v) = Version::parse(tag) {
                        let platforms = extract_platforms(&manifest);
                        if !platforms.is_empty() {
                            platform_info.entry(v).or_default().extend(platforms);
                        }
                    }
                }
                Err(e) => {
                    log::warn!("[{}] Could not fetch manifest for tag {tag}: {e}", spec.name);
                }
            }
        }

        let version_map = VersionPlatformMap::from_tags_and_platforms(&all_tags, platform_info);

        // Filter (now platform-aware and variant-aware)
        let filtered = filter::filter_versions(
            resolved_versions,
            &self.options.version,
            spec.skip_prereleases,
            spec.versions.as_ref(),
            &version_map,
            self.options.latest,
        );

        if filtered.is_empty() {
            log::info!("[{}] Nothing to mirror", spec.name);
            return Ok(());
        }

        // Build mirror tasks — find variant context for each resolved version
        let mut tasks = Vec::new();
        for rv in &filtered {
            // Find matching effective variant for metadata/asset_type inheritance
            let eff_variant = effective_variants
                .iter()
                .find(|ev| ev.name == rv.variant)
                .expect("resolved version must have matching variant");

            for platform_asset in &rv.platforms {
                let platform_str = platform_asset.platform.to_string();
                let asset_type = eff_variant
                    .asset_type
                    .as_ref()
                    .map(|at| at.resolve(&platform_str))
                    .unwrap_or(crate::spec::AssetType::Archive { strip_components: None });

                tasks.push(MirrorTask {
                    version: rv.version.clone(),
                    normalized_version: rv.normalized_version.clone(),
                    platform: platform_asset.platform.clone(),
                    download_url: platform_asset.url.clone(),
                    asset_name: platform_asset.asset_name.clone(),
                    target: spec.target.clone(),
                    metadata_config: eff_variant.metadata.clone(),
                    verify_config: spec.verify.clone(),
                    cascade: spec.cascade,
                    spec_dir: spec_dir.to_path_buf(),
                    asset_type,
                    variant: rv.variant.as_ref().map(|name| VariantContext {
                        name: name.clone(),
                        is_default: eff_variant.is_default,
                    }),
                });
            }
        }

        // Drop the prep span before starting orchestration
        drop(_prep_guard);

        log::info!(
            "[{}] Mirroring {} versions ({} tasks)",
            spec.name,
            filtered.len(),
            tasks.len()
        );

        if self.options.dry_run {
            for task in &tasks {
                log::info!(
                    "[{}] Would mirror {} {} ({})",
                    spec.name,
                    task.normalized_version,
                    task.platform,
                    task.asset_name
                );
            }
            return Ok(());
        }

        let work_dir = self.options.work_dir.join(&spec.name);
        tokio::fs::create_dir_all(&work_dir)
            .await
            .map_err(|e| MirrorError::ExecutionFailed(vec![format!("failed to create work dir: {e}")]))?;

        let http_client = reqwest::Client::new();

        let compression_threads = crate::spec::resolve_compression_threads(
            spec.concurrency.compression_threads,
            spec.concurrency.max_bundles,
        );

        let results = orchestrator::execute_mirror(
            tasks,
            &publisher,
            &http_client,
            &work_dir,
            version_map,
            self.options.fail_fast,
            orchestrator::ConcurrencyParams {
                max_downloads: spec.concurrency.max_downloads,
                max_bundles: spec.concurrency.max_bundles,
                compression_threads,
            },
        )
        .await;

        // Report results
        let has_failures = options::report_results(&results, self.options.format, printer);

        if has_failures {
            let errors: Vec<String> = results
                .iter()
                .filter_map(|r| match r {
                    MirrorResult::Failed {
                        version,
                        platform,
                        error,
                    } => Some(format!("{version} {platform}: {error}")),
                    _ => None,
                })
                .collect();
            return Err(MirrorError::ExecutionFailed(errors));
        }

        Ok(())
    }
}

/// Extract platform entries from an OCI manifest.
fn extract_platforms(manifest: &ocx_lib::oci::Manifest) -> Vec<Platform> {
    match manifest {
        ocx_lib::oci::Manifest::ImageIndex(index) => index
            .manifests
            .iter()
            .filter_map(|entry| entry.platform.as_ref().and_then(|p| Platform::try_from(p.clone()).ok()))
            .collect(),
        _ => vec![],
    }
}

/// List upstream versions from the configured source.
async fn list_upstream_versions(
    spec: &MirrorSpec,
    spec_dir: &std::path::Path,
) -> Result<Vec<source::VersionInfo>, MirrorError> {
    match &spec.source {
        spec::Source::GithubRelease {
            owner,
            repo,
            tag_pattern,
        } => {
            let token = ocx_lib::env::var("GITHUB_TOKEN");
            let mut builder = octocrab::Octocrab::builder();
            if let Some(token) = token {
                builder = builder.personal_token(token);
            }
            let octocrab = builder
                .build()
                .map_err(|e| MirrorError::SourceError(format!("failed to create GitHub client: {e}")))?;

            let pattern = regex::Regex::new(tag_pattern)
                .map_err(|e| MirrorError::SpecInvalid(vec![format!("invalid tag_pattern: {e}")]))?;

            log::debug!("Fetching GitHub releases for {}/{}", owner, repo);
            source::github_release::list_versions(&octocrab, owner, repo, &pattern, spec.concurrency.rate_limit_ms)
                .await
                .map_err(|e| MirrorError::SourceError(format!("failed to list GitHub releases: {e}")))
        }
        spec::Source::UrlIndex(url_index_source) => match url_index_source {
            spec::UrlIndexSource::Remote { url } => {
                log::debug!("Fetching remote URL index from {}", url);
                source::url_index::from_remote(url)
                    .await
                    .map_err(|e| MirrorError::SourceError(format!("failed to fetch url_index: {e}")))
            }
            spec::UrlIndexSource::Inline { versions } => {
                log::debug!("Loading {} inline versions", versions.len());
                source::url_index::from_inline(versions)
                    .map_err(|e| MirrorError::SourceError(format!("invalid url_index versions: {e}")))
            }
            spec::UrlIndexSource::Generator { generator } => {
                log::debug!("Running generator: {}", generator.command.join(" "));
                source::url_index::from_generator(generator, spec_dir)
                    .await
                    .map_err(|e| MirrorError::SourceError(format!("generator failed: {e}")))
            }
        },
    }
}
