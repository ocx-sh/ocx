// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use ocx_lib::cli::progress::{ProgressManager, Spinner};
use ocx_lib::log;
use ocx_lib::package::metadata::Metadata;
use ocx_lib::package::version::Version;
use ocx_lib::publisher::Publisher;
use serde::Serialize;
use tokio::sync::Semaphore;

use super::download;
use super::mirror_result::MirrorResult;
use super::mirror_task::MirrorTask;
use super::package;
use super::progress;
use super::push;
use super::verify;
use crate::error::MirrorError;
use crate::version_platform_map::VersionPlatformMap;

/// A task that completed the prepare phase (download + verify + bundle).
struct PreparedTask {
    task: MirrorTask,
    task_dir: PathBuf,
    bundle_path: PathBuf,
    metadata: Metadata,
}

/// Outcome of the prepare phase for a single task.
enum PrepareOutcome {
    Ready(Box<PreparedTask>),
    Failed(MirrorResult),
}

/// Concurrency parameters for the mirror pipeline.
pub struct ConcurrencyParams {
    pub max_downloads: usize,
    pub max_bundles: usize,
    pub compression_threads: u32,
}

/// Per-bundle entry in a version manifest.
#[derive(Debug, Clone, Serialize)]
pub struct BundleEntry {
    /// Platform slug (e.g. `linux_amd64`).
    pub platform_slug: String,
    /// Absolute path to `bundle.tar.xz`.
    pub bundle_path: PathBuf,
    /// File size in bytes.
    pub size_bytes: u64,
    /// SHA-256 hex digest of the bundle file.
    pub sha256: String,
}

/// Output of `prepare_version`: per-version manifest listing all prepared bundles.
///
/// Written to `{work_dir}/{version}/manifest.json` on success.
#[derive(Debug, Clone, Serialize)]
pub struct VersionManifest {
    pub version: String,
    pub bundles: Vec<BundleEntry>,
}

/// Prepare all platforms for a single version: download, verify, and bundle.
///
/// Runs platform tasks concurrently with `max_downloads` and `max_bundles`
/// semaphore slots. On success, writes `{work_dir}/{version}/manifest.json`
/// and returns the populated manifest.
///
/// Call sites:
/// - `execute_mirror` — drives the existing sync pipeline
/// - `command::pipeline::prepare` — standalone `ocx-mirror pipeline prepare` subcommand
pub(crate) async fn prepare_version(
    version: &str,
    tasks: &[MirrorTask],
    work_dir: &Path,
    http_client: &reqwest::Client,
    concurrency: &ConcurrencyParams,
) -> Result<VersionManifest, MirrorError> {
    let download_sem = Arc::new(Semaphore::new(concurrency.max_downloads));
    let bundle_sem = Arc::new(Semaphore::new(concurrency.max_bundles));
    let compression_threads = concurrency.compression_threads;
    let progress = ProgressManager::hidden();

    let mut join_set = tokio::task::JoinSet::<(usize, Result<(PathBuf, Metadata)>)>::new();

    for (i, task) in tasks.iter().enumerate() {
        let task = task.clone();
        let task_dir = task_dir(work_dir, &task.normalized_version, &task.platform);
        let dl_sem = download_sem.clone();
        let bd_sem = bundle_sem.clone();
        let client = http_client.clone();
        let progress = progress.clone();

        join_set.spawn(async move {
            let spinner = progress.spinner(format!("{} {}", task.normalized_version, task.platform));
            let result = spinner
                .scope(prepare_task(
                    &task,
                    &task_dir,
                    &client,
                    &spinner,
                    &dl_sem,
                    &bd_sem,
                    compression_threads,
                ))
                .await;
            (i, result)
        });
    }

    // Collect in completion order, then sort by index for deterministic output.
    let mut outcomes: Vec<(usize, Result<(PathBuf, Metadata)>)> = Vec::with_capacity(tasks.len());
    while let Some(join_result) = join_set.join_next().await {
        match join_result {
            Ok(outcome) => outcomes.push(outcome),
            Err(e) => {
                return Err(MirrorError::ExecutionFailed(vec![format!(
                    "prepare task panicked: {e}"
                )]));
            }
        }
    }
    outcomes.sort_by_key(|(i, _)| *i);

    // Convert outcomes to bundle entries; propagate the first failure.
    let mut bundles = Vec::with_capacity(tasks.len());
    for (i, result) in outcomes {
        let (bundle_path, _metadata) = result.map_err(|e| {
            MirrorError::ExecutionFailed(vec![format!("prepare failed for {}: {e:#}", tasks[i].platform)])
        })?;

        let size_bytes = tokio::fs::metadata(&bundle_path).await.map(|m| m.len()).unwrap_or(0);

        let sha256 = compute_sha256(&bundle_path).await?;
        let platform_slug = tasks[i].platform.ascii_segments().join("_");

        bundles.push(BundleEntry {
            platform_slug,
            bundle_path,
            size_bytes,
            sha256,
        });
    }

    let manifest = VersionManifest {
        version: version.to_owned(),
        bundles,
    };

    // Write manifest.json to {work_dir}/{version}/
    let version_dir = work_dir.join(version);
    tokio::fs::create_dir_all(&version_dir)
        .await
        .map_err(|e| MirrorError::ExecutionFailed(vec![format!("failed to create version dir: {e}")]))?;

    let manifest_path = version_dir.join("manifest.json");
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| MirrorError::ExecutionFailed(vec![format!("failed to serialize manifest: {e}")]))?;
    tokio::fs::write(&manifest_path, json)
        .await
        .map_err(|e| MirrorError::ExecutionFailed(vec![format!("failed to write manifest.json: {e}")]))?;

    log::debug!("Wrote manifest to {}", manifest_path.display());
    Ok(manifest)
}

/// Compute the SHA-256 hex digest of a file.
async fn compute_sha256(path: &Path) -> Result<String, MirrorError> {
    use sha2::{Digest, Sha256};

    let data = tokio::fs::read(path).await.map_err(|e| {
        MirrorError::ExecutionFailed(vec![format!(
            "failed to read bundle for sha256 {}: {e}",
            path.display()
        )])
    })?;

    let mut hasher = Sha256::new();
    hasher.update(&data);
    let hash = hasher.finalize();
    Ok(hex::encode(hash))
}

/// Execute all mirror tasks with concurrent preparation and sequential pushing.
///
/// All artifacts (downloads, bundles) live under `work_dir/{version}/{platform}/`.
/// On successful push the task directory is removed. On failure it persists so the
/// next run can resume from whatever stage completed.
///
/// **Phases:**
/// 1. *Prepare* (concurrent) — Download and bundle all tasks in parallel.
///    Downloads are gated by `concurrency.max_downloads`, bundling by
///    `concurrency.max_bundles`. The two semaphores are independent so slow
///    downloads don't block idle CPU cores and vice versa.
/// 2. *Push* (sequential) — Push tasks in version order (oldest first) for correct
///    cascade tag ordering. Each successful `(version, platform)` push is immediately
///    registered in the version map so subsequent cascade computations see it.
// Pipeline entrypoint: orthogonal services + policy (tasks, registry
// client, HTTP client, work dir, version map, progress, fail-fast,
// concurrency). A params struct would relocate the list without
// improving clarity, so the lint is allowed here.
#[allow(clippy::too_many_arguments)]
pub async fn execute_mirror(
    tasks: Vec<MirrorTask>,
    publisher: &Publisher,
    http_client: &reqwest::Client,
    work_dir: &Path,
    mut version_map: VersionPlatformMap,
    progress: &ProgressManager,
    fail_fast: bool,
    concurrency: ConcurrencyParams,
) -> Vec<MirrorResult> {
    // Group tasks by version
    let mut by_version: HashMap<String, Vec<MirrorTask>> = HashMap::new();
    for task in tasks {
        by_version
            .entry(task.normalized_version.clone())
            .or_default()
            .push(task);
    }

    // Sort versions oldest first (cascade ordering)
    let mut version_keys: Vec<String> = by_version.keys().cloned().collect();
    version_keys.sort_by(|a, b| {
        let va = Version::parse(a);
        let vb = Version::parse(b);
        match (va, vb) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ => a.cmp(b),
        }
    });

    // Build ordered task list with version boundaries
    let mut entries: Vec<(MirrorTask, PathBuf)> = Vec::new();
    let mut version_ranges: Vec<Range<usize>> = Vec::new();

    for version_key in &version_keys {
        let start = entries.len();
        for task in by_version.remove(version_key).expect("key from version_keys") {
            let task_dir = task_dir(work_dir, &task.normalized_version, &task.platform);
            entries.push((task, task_dir));
        }
        version_ranges.push(start..entries.len());
    }

    let n = entries.len();
    log::debug!(
        "Executing {n} tasks across {} versions (downloads: {}, bundles: {}, compression threads: {})",
        version_keys.len(),
        concurrency.max_downloads,
        concurrency.max_bundles,
        concurrency.compression_threads,
    );

    // Phase 1: Prepare all tasks concurrently (download + verify + bundle)
    // Two independent semaphores: downloads are I/O-bound, bundles are CPU-bound.
    // Spans are created on-demand after acquiring the first semaphore, so only
    // actively-worked-on tasks show progress bars.
    let download_sem = Arc::new(Semaphore::new(concurrency.max_downloads));
    let bundle_sem = Arc::new(Semaphore::new(concurrency.max_bundles));
    let compression_threads = concurrency.compression_threads;
    let mut join_set = tokio::task::JoinSet::<(usize, PrepareOutcome)>::new();

    for (i, (task, task_dir)) in entries.into_iter().enumerate() {
        let dl_sem = download_sem.clone();
        let bd_sem = bundle_sem.clone();
        let client = http_client.clone();
        let progress = progress.clone();

        join_set.spawn(async move {
            let spinner = progress.spinner(format!("{} {}", task.normalized_version, task.platform));

            match spinner
                .scope(prepare_task(
                    &task,
                    &task_dir,
                    &client,
                    &spinner,
                    &dl_sem,
                    &bd_sem,
                    compression_threads,
                ))
                .await
            {
                Ok((bundle_path, metadata)) => (
                    i,
                    PrepareOutcome::Ready(Box::new(PreparedTask {
                        task,
                        task_dir,
                        bundle_path,
                        metadata,
                    })),
                ),
                Err(e) => (
                    i,
                    PrepareOutcome::Failed(MirrorResult::Failed {
                        version: task.normalized_version.clone(),
                        platform: task.platform.clone(),
                        error: format!("{e:#}"),
                    }),
                ),
            }
        });
    }

    // Collect prepare results into index-ordered slots
    let mut prepared: Vec<Option<PrepareOutcome>> = (0..n).map(|_| None).collect();
    while let Some(join_result) = join_set.join_next().await {
        match join_result {
            Ok((idx, outcome)) => {
                prepared[idx] = Some(outcome);
            }
            Err(e) => {
                log::error!("Task panicked: {e}");
            }
        }
    }

    // Phase 2: Push sequentially by version (oldest first).
    // Each successful (version, platform) push is immediately registered in the
    // version map so subsequent cascade computations see it as existing.
    let mut results = Vec::new();
    let mut abort = false;

    for (range_idx, range) in version_ranges.iter().enumerate() {
        if abort {
            break;
        }

        for idx in range.clone() {
            let Some(outcome) = prepared[idx].take() else {
                continue;
            };

            match outcome {
                PrepareOutcome::Ready(prep) => {
                    let spinner = progress.spinner(format!("{} {}", prep.task.normalized_version, prep.task.platform));
                    progress::set_stage(&spinner, "Pushing", &prep.task.normalized_version, &prep.task.platform);

                    let cascade_versions = version_map.versions_for_cascade();
                    let push_result = spinner
                        .scope(push_task(
                            &prep.task,
                            &prep.bundle_path,
                            &prep.metadata,
                            publisher,
                            &cascade_versions,
                        ))
                        .await;

                    match push_result {
                        Ok(result) => {
                            if matches!(&result, MirrorResult::Pushed { .. }) {
                                // Register this (version, platform) immediately so
                                // the next platform's cascade sees it.
                                if let Some(v) = Version::parse(&version_keys[range_idx]) {
                                    // Register bare alias for default variants so subsequent
                                    // bare cascades in this run see correct blockers.
                                    if prep.task.variant.as_ref().is_some_and(|ctx| ctx.is_default)
                                        && v.variant().is_some()
                                    {
                                        version_map.add(v.without_variant(), prep.task.platform.clone());
                                    }
                                    version_map.add(v, prep.task.platform.clone());
                                }
                                clean_task_dir(&prep.task_dir).await;
                            }
                            results.push(result);
                        }
                        Err(e) => {
                            results.push(MirrorResult::Failed {
                                version: prep.task.normalized_version.clone(),
                                platform: prep.task.platform.clone(),
                                error: format!("{e:#}"),
                            });
                            if fail_fast {
                                abort = true;
                                break;
                            }
                        }
                    }
                }
                PrepareOutcome::Failed(result) => {
                    results.push(result);
                    if fail_fast {
                        abort = true;
                        break;
                    }
                }
            }
        }
    }

    results
}

/// Build the task directory path: `{work_dir}/{version}/{platform_slug}/`
pub(crate) fn task_dir(work_dir: &Path, version: &str, platform: &ocx_lib::oci::Platform) -> PathBuf {
    let platform_slug = platform.ascii_segments().join("_");
    work_dir.join(version).join(platform_slug)
}

/// Phase 1: Download, verify, and bundle a single task.
///
/// Acquires `download_sem` for the download+verify phase, then releases it and
/// acquires `bundle_sem` for the CPU-bound bundling phase. This lets downloads
/// and compression run independently.
pub(crate) async fn prepare_task(
    task: &MirrorTask,
    task_dir: &Path,
    http_client: &reqwest::Client,
    spinner: &Spinner,
    download_sem: &Semaphore,
    bundle_sem: &Semaphore,
    compression_threads: u32,
) -> Result<(PathBuf, Metadata)> {
    tokio::fs::create_dir_all(task_dir).await?;

    let archive_path = task_dir.join(&task.asset_name);
    let content_dir = task_dir.join("content");
    let bundle_path = task_dir.join("bundle.tar.xz");

    // Resolve metadata once (needed for both bundle and push)
    let platform_str = task.platform.to_string();
    let metadata = match &task.metadata_config {
        Some(config) => package::resolve_metadata(config, &platform_str, &task.spec_dir)?,
        None => anyhow::bail!("no metadata configuration provided in spec"),
    };

    // Write the resolved per-platform metadata alongside the bundle so the
    // generated CI workflow's `cp` step copies the correct per-platform file
    // (not the spec-level default metadata.json from the working directory).
    // Written before the early-exit check so resume runs also refresh the file.
    let metadata_json = serde_json::to_string_pretty(&metadata)
        .map_err(|e| anyhow::anyhow!("failed to serialize metadata for {}: {e}", task.platform))?;
    tokio::fs::write(task_dir.join("metadata.json"), metadata_json).await?;

    if bundle_path.exists() {
        // Resume: bundle already exists, metadata.json already written above.
        return Ok((bundle_path, metadata));
    }

    // --- Download phase (I/O-bound) ---
    {
        let _permit = download_sem.acquire().await.expect("semaphore closed");

        // Download
        if !archive_path.exists() {
            progress::set_stage(spinner, "Downloading", &task.normalized_version, &task.platform);
            download::download(http_client, &task.download_url, &archive_path).await?;
        }

        // Verify (only if configured)
        if let Some(verify_config) = &task.verify_config {
            progress::set_stage(spinner, "Verifying", &task.normalized_version, &task.platform);
            verify::verify(
                verify_config,
                http_client,
                &archive_path,
                &task.asset_name,
                &HashMap::new(),
                &task.download_url,
            )
            .await?;
        }
    } // download permit released

    // --- Bundle phase (CPU-bound) ---
    {
        let _permit = bundle_sem.acquire().await.expect("semaphore closed");

        progress::set_stage(spinner, "Bundling", &task.normalized_version, &task.platform);
        let asset_type = task.asset_type.clone();

        let ap = archive_path.clone();
        let cd = content_dir.clone();
        let bp = bundle_path.clone();
        let an = task.asset_name.clone();
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async {
                if cd.exists() {
                    let _ = tokio::fs::remove_dir_all(&cd).await;
                }
                tokio::fs::create_dir_all(&cd).await?;
                package::extract_and_bundle(&ap, &cd, &bp, &asset_type, &an, compression_threads).await?;
                let _ = tokio::fs::remove_dir_all(&cd).await;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await??;
    } // bundle permit released

    Ok((bundle_path, metadata))
}

/// Phase 2: Push a prepared bundle to the registry with optional cascade.
async fn push_task(
    task: &MirrorTask,
    bundle_path: &Path,
    metadata: &Metadata,
    publisher: &Publisher,
    cascade_versions: &std::collections::BTreeSet<Version>,
) -> Result<MirrorResult> {
    let identifier = ocx_lib::oci::Identifier::new_registry(&task.target.repository, &task.target.registry)
        .clone_with_tag(&task.normalized_version);

    let info = ocx_lib::package::info::Info {
        identifier,
        metadata: metadata.clone(),
        platform: task.platform.clone(),
    };

    push::push_and_cascade(
        publisher,
        info,
        bundle_path,
        task.cascade,
        cascade_versions,
        task.variant.as_ref(),
    )
    .await
}

/// Remove the task directory after successful push.
async fn clean_task_dir(task_dir: &Path) {
    if let Err(e) = tokio::fs::remove_dir_all(task_dir).await {
        log::debug!("Failed to clean task dir {}: {e}", task_dir.display());
    }
}
