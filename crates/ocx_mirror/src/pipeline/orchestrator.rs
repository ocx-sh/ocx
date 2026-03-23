// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use ocx_lib::log;
use ocx_lib::package::metadata::Metadata;
use ocx_lib::package::version::Version;
use ocx_lib::publisher::Publisher;
use tokio::sync::Semaphore;
use tracing::{Instrument, info_span};

use super::download;
use super::mirror_result::MirrorResult;
use super::mirror_task::MirrorTask;
use super::package;
use super::progress;
use super::push;
use super::verify;
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
pub async fn execute_mirror(
    tasks: Vec<MirrorTask>,
    publisher: &Publisher,
    http_client: &reqwest::Client,
    work_dir: &Path,
    mut version_map: VersionPlatformMap,
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

        join_set.spawn(async move {
            let span = info_span!("mirror_task");

            match prepare_task(&task, &task_dir, &client, &span, &dl_sem, &bd_sem, compression_threads)
                .instrument(span.clone())
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
                    let span = info_span!("mirror_task");
                    progress::set_stage(&span, "Pushing", &prep.task.normalized_version, &prep.task.platform);
                    let _guard = span.entered();

                    let cascade_versions = version_map.versions_for_cascade();
                    let push_result = push_task(
                        &prep.task,
                        &prep.bundle_path,
                        &prep.metadata,
                        publisher,
                        &cascade_versions,
                    )
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
fn task_dir(work_dir: &Path, version: &str, platform: &ocx_lib::oci::Platform) -> PathBuf {
    let platform_slug = platform.ascii_segments().join("_");
    work_dir.join(version).join(platform_slug)
}

/// Phase 1: Download, verify, and bundle a single task.
///
/// Acquires `download_sem` for the download+verify phase, then releases it and
/// acquires `bundle_sem` for the CPU-bound bundling phase. This lets downloads
/// and compression run independently.
async fn prepare_task(
    task: &MirrorTask,
    task_dir: &Path,
    http_client: &reqwest::Client,
    span: &tracing::Span,
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

    if bundle_path.exists() {
        // Resume: nothing to do, bundle already exists
        return Ok((bundle_path, metadata));
    }

    // --- Download phase (I/O-bound) ---
    {
        let _permit = download_sem.acquire().await.expect("semaphore closed");

        // Download
        if !archive_path.exists() {
            progress::set_stage(span, "Downloading", &task.normalized_version, &task.platform);
            download::download(http_client, &task.download_url, &archive_path).await?;
        }

        // Verify (only if configured)
        if let Some(verify_config) = &task.verify_config {
            progress::set_stage(span, "Verifying", &task.normalized_version, &task.platform);
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

        progress::set_stage(span, "Bundling", &task.normalized_version, &task.platform);
        let asset_type = task.asset_type.clone();

        let ap = archive_path.clone();
        let cd = content_dir.clone();
        let bp = bundle_path.clone();
        let md = metadata.clone();
        let an = task.asset_name.clone();
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async {
                if cd.exists() {
                    let _ = tokio::fs::remove_dir_all(&cd).await;
                }
                tokio::fs::create_dir_all(&cd).await?;
                package::extract_and_bundle(&ap, &cd, &bp, &md, &asset_type, &an, compression_threads).await?;
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
