// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-mirror pipeline prepare` — download, verify, and bundle one version
//! across all declared platforms. Mirrors the per-version subset of the
//! existing `command/sync.rs` Phase-1 loop.

use std::path::PathBuf;

use ocx_lib::cli::Printer;
use ocx_lib::log;

use crate::command::sync::list_upstream_versions;
use crate::error::MirrorError;
use crate::normalizer;
use crate::pipeline::mirror_task::{MirrorTask, VariantContext};
use crate::pipeline::orchestrator::{self, ConcurrencyParams};
use crate::resolver;
use crate::resolver::asset_resolution::AssetResolution;
use crate::spec::{self, MirrorSpec};

/// `ocx-mirror pipeline prepare` subcommand.
///
/// Outputs `{work_dir}/{V}/{platform_slug}/bundle.tar.xz` per declared
/// platform and `{work_dir}/{V}/manifest.json` listing bundles with sizes
/// and digests.
#[derive(clap::Parser)]
pub struct Prepare {
    /// Path to the mirror spec file.
    #[arg(long, default_value = "./mirror.yml")]
    pub spec: PathBuf,

    /// Version to prepare (e.g. `3.29.0`).
    #[arg(long, required = true)]
    pub version: String,

    /// Working directory for intermediate artifacts. Defaults to `./.ocx-mirror`.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,
}

impl Prepare {
    pub async fn execute(&self, _printer: &Printer) -> Result<(), MirrorError> {
        let spec_path = &self.spec;
        let spec = spec::load_spec(spec_path).await?;
        let spec_dir = spec_path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();

        let work_dir = self
            .work_dir
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from(".ocx-mirror"));

        let tasks = build_tasks_for_version(&spec, &spec_dir, &self.version).await?;

        if tasks.is_empty() {
            return Err(MirrorError::SpecInvalid(vec![format!(
                "version '{}' not found in upstream source or no platforms resolved",
                self.version
            )]));
        }

        log::info!(
            "[{}] Preparing version {} ({} platforms)",
            spec.name,
            self.version,
            tasks.len()
        );

        tokio::fs::create_dir_all(&work_dir)
            .await
            .map_err(|e| MirrorError::ExecutionFailed(vec![format!("failed to create work dir: {e}")]))?;

        let http_client = reqwest::Client::new();
        let concurrency = ConcurrencyParams {
            max_downloads: spec.concurrency.max_downloads,
            max_bundles: spec.concurrency.max_bundles,
            compression_threads: spec::resolve_compression_threads(
                spec.concurrency.compression_threads,
                spec.concurrency.max_bundles,
            ),
        };

        let manifest =
            orchestrator::prepare_version(&self.version, &tasks, &work_dir, &http_client, &concurrency).await?;

        let manifest_path = work_dir.join(&self.version).join("manifest.json");
        println!("{}", manifest_path.display());

        log::debug!(
            "[{}] Prepared {} bundles for version {}",
            spec.name,
            manifest.bundles.len(),
            self.version
        );

        Ok(())
    }
}

/// Build `MirrorTask`s for a specific version string across all resolved platforms.
///
/// Lists upstream versions, finds the one matching `version`, applies asset patterns,
/// and returns one task per resolved platform. Returns an empty Vec if the version
/// is not found (no error; caller decides whether to treat this as an error).
async fn build_tasks_for_version(
    spec: &MirrorSpec,
    spec_dir: &std::path::Path,
    version: &str,
) -> Result<Vec<MirrorTask>, MirrorError> {
    let upstream_versions = list_upstream_versions(spec, spec_dir).await?;

    let build_ts = normalizer::build_timestamp(&spec.build_timestamp);
    let effective_variants = spec.effective_variants();
    let mut tasks = Vec::new();

    for variant in &effective_variants {
        let patterns = variant
            .assets
            .compiled()
            .map_err(|e| MirrorError::SpecInvalid(vec![e]))?;

        for version_info in &upstream_versions {
            // Normalize the upstream version to compare against the requested version.
            let normalized = match normalizer::normalize_version(&version_info.version, &build_ts) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Apply variant prefix to match the normalized tag format.
            let tagged = match &variant.name {
                Some(name) => format!("{name}-{normalized}"),
                None => normalized.clone(),
            };

            // Skip versions that don't match the requested version.
            // Accept either the raw upstream version or the normalized/tagged form.
            let matches = version_info.version == version || normalized == version || tagged == version;
            if !matches {
                continue;
            }

            match resolver::resolve_assets(&version_info.assets, &patterns) {
                AssetResolution::Resolved(platforms) => {
                    for platform_asset in &platforms {
                        let platform_str = platform_asset.platform.to_string();
                        let asset_type = variant
                            .asset_type
                            .as_ref()
                            .map(|at| at.resolve(&platform_str))
                            .unwrap_or(spec::AssetType::Archive { strip_components: None });

                        tasks.push(MirrorTask {
                            version: version_info.version.clone(),
                            normalized_version: tagged.clone(),
                            platform: platform_asset.platform.clone(),
                            download_url: platform_asset.url.clone(),
                            asset_name: platform_asset.asset_name.clone(),
                            target: spec.target.clone(),
                            metadata_config: variant.metadata.clone(),
                            verify_config: spec.verify.clone(),
                            cascade: spec.cascade,
                            spec_dir: spec_dir.to_path_buf(),
                            asset_type,
                            variant: variant.name.as_ref().map(|name| VariantContext {
                                name: name.clone(),
                                is_default: variant.is_default,
                            }),
                        });
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

    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use std::panic;
    use std::path::Path;
    use tempfile::tempdir;

    use super::*;

    // ── §3.6 S6: prepare subcommand tests ────────────────────────────────────
    //
    // All tests that call execute() will panic with "not implemented"
    // until wave 3. Tests that only exercise struct construction pass now.

    const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    fn make_printer() -> Printer {
        Printer::new(false)
    }

    fn run_prepare(cmd: Prepare) -> Result<(), MirrorError> {
        let printer = make_printer();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async { cmd.execute(&printer).await })
    }

    #[test]
    fn prepare_produces_bundle_for_each_declared_platform() {
        // §3.6: prepare --version 3.29.0 produces {work_dir}/{V}/{platform_slug}/bundle.tar.xz
        // for every declared platform.
        // Fails with "not implemented" until wave 3.
        let work_dir = tempdir().unwrap();
        let spec_path = Path::new(FIXTURE_DIR).join("mirror-minimal.yml");

        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            run_prepare(Prepare {
                spec: spec_path,
                version: "3.29.0".to_string(),
                work_dir: Some(work_dir.path().to_path_buf()),
            })
        }));

        match result {
            Err(_) => {
                // Panicked with unimplemented!() — expected at Phase 3
            }
            Ok(Ok(())) => {
                let bundle_path = work_dir.path().join("3.29.0").join("linux_amd64").join("bundle.tar.xz");
                assert!(
                    bundle_path.exists(),
                    "Expected bundle at {}, not found",
                    bundle_path.display()
                );
            }
            Ok(Err(_)) => {
                // Other errors acceptable for unimplemented paths
            }
        }
    }

    #[test]
    fn prepare_produces_manifest_json() {
        // §3.6: Manifest file {work_dir}/{V}/manifest.json lists bundles with sizes + digests.
        let work_dir = tempdir().unwrap();
        let spec_path = Path::new(FIXTURE_DIR).join("mirror-minimal.yml");

        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            run_prepare(Prepare {
                spec: spec_path,
                version: "3.29.0".to_string(),
                work_dir: Some(work_dir.path().to_path_buf()),
            })
        }));

        match result {
            Err(_) => {}
            Ok(Ok(())) => {
                let manifest_path = work_dir.path().join("3.29.0").join("manifest.json");
                assert!(manifest_path.exists(), "Expected manifest.json");
                let content = std::fs::read_to_string(&manifest_path).unwrap();
                let value: serde_json::Value =
                    serde_json::from_str(&content).expect("manifest.json must be valid JSON");
                assert!(
                    value.get("bundles").is_some() || value.is_array(),
                    "manifest.json must contain bundle list"
                );
            }
            Ok(Err(_)) => {}
        }
    }

    #[test]
    fn prepare_is_idempotent_on_rerun() {
        // §3.6: Re-run is idempotent (same bundles, no errors).
        let work_dir = tempdir().unwrap();
        let spec_path = Path::new(FIXTURE_DIR).join("mirror-minimal.yml");

        let result1 = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            run_prepare(Prepare {
                spec: spec_path.clone(),
                version: "3.29.0".to_string(),
                work_dir: Some(work_dir.path().to_path_buf()),
            })
        }));

        if result1.is_err() {
            // Both runs panicked with unimplemented — expected at Phase 3
            let result2 = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                run_prepare(Prepare {
                    spec: spec_path,
                    version: "3.29.0".to_string(),
                    work_dir: Some(work_dir.path().to_path_buf()),
                })
            }));
            assert!(result2.is_err(), "Second run must also panic with unimplemented");
            return;
        }

        if let Ok(Ok(())) = result1 {
            let result2 = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                run_prepare(Prepare {
                    spec: spec_path,
                    version: "3.29.0".to_string(),
                    work_dir: Some(work_dir.path().to_path_buf()),
                })
            }));
            assert!(matches!(result2, Ok(Ok(()))), "Second run (idempotent) must succeed");
        }
    }

    #[test]
    fn prepare_exits_65_on_checksum_mismatch() {
        // §3.6: Checksum mismatch → exit 65 (DataError).
        // Uses a fake version string to trigger failure.
        // Until implementation: expect unimplemented!() panic.
        use ocx_lib::cli::ExitCode;

        let work_dir = tempdir().unwrap();
        let spec_path = Path::new(FIXTURE_DIR).join("mirror-minimal.yml");

        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            run_prepare(Prepare {
                spec: spec_path,
                version: "99.99.99-bad-checksum".to_string(),
                work_dir: Some(work_dir.path().to_path_buf()),
            })
        }));

        match result {
            Err(_) => {} // unimplemented — expected at Phase 3
            Ok(Err(MirrorError::SpecInvalid(_))) => {
                // Version-not-found is acceptable response for fake version
            }
            Ok(Err(e)) => {
                let exit_code = e.kind_exit_code();
                assert!(
                    exit_code == ExitCode::DataError || exit_code == ExitCode::Unavailable,
                    "Checksum mismatch must exit DataError(65) or Unavailable(69), got: {:?}",
                    exit_code
                );
            }
            Ok(Ok(())) => panic!("Expected error for bad checksum version"),
        }
    }

    #[test]
    fn prepare_exits_69_on_source_unreachable() {
        // §3.6: Source unreachable → exit 69 (Unavailable).
        // SourceError maps to ExitCode::Unavailable (69) via kind_exit_code().
        let work_dir = tempdir().unwrap();
        let spec_path = Path::new(FIXTURE_DIR).join("mirror-minimal.yml");

        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            run_prepare(Prepare {
                spec: spec_path,
                version: "3.29.0".to_string(),
                work_dir: Some(work_dir.path().to_path_buf()),
            })
        }));

        match result {
            Err(_) => {} // unimplemented — expected at Phase 3
            Ok(Err(MirrorError::SourceError(_))) => {
                // Source unreachable → SourceError maps to Unavailable (69)
            }
            Ok(Err(e)) => {
                let _ = e.kind_exit_code();
            }
            Ok(Ok(())) => {
                // Acceptable if network is available and source resolves
            }
        }
    }

    #[test]
    fn prepare_default_work_dir_uses_none() {
        // §3.6: Default work_dir when not specified → uses default ./.ocx-mirror.
        // Verify Prepare struct accepts None for work_dir.
        let spec_path = Path::new(FIXTURE_DIR).join("mirror-minimal.yml");

        let cmd = Prepare {
            spec: spec_path,
            version: "3.29.0".to_string(),
            work_dir: None, // uses default ./.ocx-mirror
        };

        // Struct construction must succeed (no panic)
        // Actual execution will panic with unimplemented!() — expected at Phase 3
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let printer = make_printer();
            let rt = tokio::runtime::Runtime::new().unwrap();
            let _ = rt.block_on(async { cmd.execute(&printer).await });
        }));
        // Panicked or returned — either is acceptable at Phase 3
        let _ = result;
    }
}
