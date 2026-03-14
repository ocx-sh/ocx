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
use crate::pipeline::mirror_task::MirrorTask;
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
    pub async fn execute(&self) -> Result<(), MirrorError> {
        let spec_path = &self.spec;
        if !spec_path.exists() {
            return Err(MirrorError::SpecNotFound(spec_path.display().to_string()));
        }

        let content = tokio::fs::read_to_string(spec_path)
            .await
            .map_err(|e| MirrorError::SpecNotFound(format!("{}: {e}", spec_path.display())))?;
        let spec: MirrorSpec = serde_yaml_ng::from_str(&content)
            .map_err(|e| MirrorError::SpecInvalid(vec![format!("YAML parse error: {e}")]))?;

        let errors = spec.validate(spec_path);
        if !errors.is_empty() {
            return Err(MirrorError::SpecInvalid(errors));
        }

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

        let upstream_versions = list_upstream_versions(&spec).await?;
        log::debug!("[{}] Found {} upstream versions", spec.name, upstream_versions.len());

        // Compile asset patterns
        let patterns = spec.assets.compiled().map_err(|e| MirrorError::SpecInvalid(vec![e]))?;

        // Generate build timestamp
        let build_ts = normalizer::build_timestamp(&spec.build_timestamp);
        log::debug!("[{}] Build timestamp: {:?}", spec.name, build_ts);

        // Resolve assets + normalize versions
        let mut resolved_versions = Vec::new();
        for version_info in &upstream_versions {
            match resolver::resolve_assets(&version_info.assets, &patterns) {
                AssetResolution::Resolved(platforms) => {
                    match normalizer::normalize_version(&version_info.version, &build_ts) {
                        Ok(normalized) => {
                            log::debug!(
                                "[{}] Resolved version {} -> {} ({} platforms)",
                                spec.name,
                                version_info.version,
                                normalized,
                                platforms.len()
                            );
                            resolved_versions.push(filter::ResolvedVersion {
                                version: version_info.version.clone(),
                                normalized_version: normalized,
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

        // Identify source versions that already have tags on the registry.
        // Only these need manifest fetches to check platform completeness.
        let source_version_tags: HashSet<String> = resolved_versions
            .iter()
            .filter_map(|rv| Version::parse(&rv.version).map(|v| v.to_string()))
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

        // Filter (now platform-aware)
        let filtered = filter::filter_versions(
            resolved_versions,
            &self.options.version,
            spec.skip_prereleases,
            spec.versions.as_ref(),
            &version_map,
        );

        if filtered.is_empty() {
            log::info!("[{}] Nothing to mirror", spec.name);
            return Ok(());
        }

        // Build mirror tasks
        let mut tasks = Vec::new();
        for rv in &filtered {
            for platform_asset in &rv.platforms {
                tasks.push(MirrorTask {
                    version: rv.version.clone(),
                    normalized_version: rv.normalized_version.clone(),
                    platform: platform_asset.platform.clone(),
                    download_url: platform_asset.url.clone(),
                    asset_name: platform_asset.asset_name.clone(),
                    target: spec.target.clone(),
                    metadata_config: spec.metadata.clone(),
                    verify_config: spec.verify.clone(),
                    cascade: spec.cascade,
                    spec_dir: spec_dir.to_path_buf(),
                    strip_components: spec.strip_components,
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

        // Prepare work directory (scoped per spec name)
        let work_dir = self.options.work_dir.join(&spec.name);
        tokio::fs::create_dir_all(&work_dir)
            .await
            .map_err(|e| MirrorError::ExecutionFailed(vec![format!("failed to create work dir: {e}")]))?;

        // Execute
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
        let has_failures = options::report_results(&results, self.options.format);

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
async fn list_upstream_versions(spec: &MirrorSpec) -> Result<Vec<source::VersionInfo>, MirrorError> {
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
        spec::Source::UrlIndex { url, versions } => {
            if let Some(versions) = versions {
                log::debug!("Loading {} inline versions", versions.len());
                source::url_index::from_inline(versions)
                    .map_err(|e| MirrorError::SourceError(format!("invalid url_index versions: {e}")))
            } else if let Some(url) = url {
                log::debug!("Fetching remote URL index from {}", url);
                source::url_index::from_remote(url)
                    .await
                    .map_err(|e| MirrorError::SourceError(format!("failed to fetch url_index: {e}")))
            } else {
                Err(MirrorError::SpecInvalid(vec![
                    "source requires url or versions".to_string(),
                ]))
            }
        }
    }
}
