// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-mirror pipeline plan` — compute which versions need work without
//! side-effects. Used by the GHA `discover` job.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use ocx_lib::cli::Printer;
use ocx_lib::oci::{ClientBuilder, Platform};
use ocx_lib::package::version::Version;
use ocx_lib::publisher::Publisher;
use serde::Serialize;

use crate::command::options::OutputFormat;
use crate::command::sync::{extract_platforms, list_upstream_versions};
use crate::error::MirrorError;
use crate::filter;
use crate::normalizer;
use crate::resolver;
use crate::resolver::asset_resolution::AssetResolution;
use crate::spec::{self, MirrorSpec};
use crate::version_platform_map::VersionPlatformMap;

/// `new` | `backfill-partial` — what kind of work is needed for this version.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlanVersionKind {
    /// Version not yet present in the target registry.
    New,
    /// Version present for some platforms but missing for others.
    BackfillPartial,
}

/// A single version entry in the plan output.
#[derive(Debug, Clone, Serialize)]
pub struct PlanVersionEntry {
    /// Upstream version string (e.g. `3.29.0`).
    pub version: String,
    /// Platform slugs that require work (e.g. `["linux/amd64", "darwin/arm64"]`).
    pub platforms: Vec<String>,
    /// Kind of work needed.
    pub kind: PlanVersionKind,
}

/// Structured output of `ocx-mirror pipeline plan`.
///
/// JSON shape (schema_version 1):
/// ```json
/// {
///   "schema_version": 1,
///   "has_new": true,
///   "versions": [...],
///   "target": "ocx.sh/cmake",
///   "ocx_mirror_rev": "abc123..."
/// }
/// ```
#[derive(Debug, Clone, Serialize)]
pub struct PlanReport {
    /// Schema version for forward-compat detection.
    pub schema_version: u32,
    /// `true` when at least one version requires action.
    pub has_new: bool,
    /// Versions requiring action, oldest first.
    pub versions: Vec<PlanVersionEntry>,
    /// Full OCI repository identifier (registry/repo).
    pub target: String,
    /// The git SHA of `ocx-mirror` used when generating this plan.
    pub ocx_mirror_rev: Option<String>,
}

/// `ocx-mirror pipeline plan` subcommand.
///
/// Reads `mirror.yml`, queries source + target registry, and emits a
/// side-effect-free plan document listing versions that need action.
#[derive(clap::Parser)]
pub struct PlanCmd {
    /// Path to the mirror spec file.
    #[arg(long, default_value = "./mirror.yml")]
    pub spec: PathBuf,

    /// Output format.
    #[arg(long)]
    pub format: Option<OutputFormat>,
}

impl PlanCmd {
    pub async fn execute(&self, printer: &Printer) -> Result<(), MirrorError> {
        let spec_path = &self.spec;
        let spec = spec::load_spec(spec_path)
            .await
            .map_err(|e| MirrorError::SourceError(format!("failed to load spec: {e}")))?;
        let spec_dir = spec_path.parent().unwrap_or(std::path::Path::new("."));

        let report = build_plan_report(&spec, spec_dir).await?;

        // Determine output format: explicit flag, or JSON when in GitHub Actions.
        let use_json = match self.format {
            Some(OutputFormat::Json) => true,
            Some(OutputFormat::Plain) => false,
            None => std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true"),
        };

        if use_json {
            printer
                .print_json(&report)
                .map_err(|e| MirrorError::ExecutionFailed(vec![format!("failed to serialize plan: {e}")]))?;
        } else {
            print_plan_plain(&report);
        }

        Ok(())
    }
}

/// Core plan computation: load registry state, fetch upstream, filter, classify.
///
/// Extracted so that integration tests can call it without going through the
/// full CLI surface (file-system spec path, `Printer`, format detection).
async fn build_plan_report(spec: &MirrorSpec, spec_dir: &std::path::Path) -> Result<PlanReport, MirrorError> {
    // Build target identifier for registry queries.
    let publisher = Publisher::new(ClientBuilder::from_env());
    let identifier = ocx_lib::oci::Identifier::new_registry(&spec.target.repository, &spec.target.registry);

    // Fetch existing tags from the target registry to build the platform map.
    // On network failure this returns empty (not an error) — same pattern as sync.rs.
    // A genuine connectivity failure surfaces later via SourceError.
    let all_tags: Vec<String> = publisher.list_tags(identifier.clone()).await.unwrap_or_default();

    // Determine which (version, platform) pairs are already present.
    let source_version_tags: HashSet<String> = {
        // Collect version-string forms we care about (including variant-prefixed).
        let mut tags = HashSet::new();
        for tag in &all_tags {
            if let Some(v) = Version::parse(tag) {
                tags.insert(v.to_string());
            }
        }
        tags
    };

    let tags_needing_platform_check: Vec<&str> = all_tags
        .iter()
        .filter(|t| source_version_tags.contains(t.as_str()))
        .map(String::as_str)
        .collect();

    let mut platform_info: BTreeMap<Version, HashSet<Platform>> = BTreeMap::new();
    for tag in &tags_needing_platform_check {
        let tag_id = identifier.clone_with_tag(tag.to_string());
        if let Ok((_, manifest)) = publisher.client().fetch_manifest(&tag_id).await
            && let Some(v) = Version::parse(tag)
        {
            let platforms = extract_platforms(&manifest);
            if !platforms.is_empty() {
                platform_info.entry(v).or_default().extend(platforms);
            }
        }
    }

    let version_map = VersionPlatformMap::from_tags_and_platforms(&all_tags, platform_info);

    // Fetch upstream versions — propagate failures as Unavailable.
    let upstream_versions = list_upstream_versions(spec, spec_dir)
        .await
        .map_err(|e| MirrorError::SourceError(format!("failed to list upstream versions: {e}")))?;

    // Build timestamp (reuse existing normalizer).
    let build_ts = normalizer::build_timestamp(&spec.build_timestamp);

    // Resolve assets per effective variant — same logic as sync.rs.
    let effective_variants = spec.effective_variants();
    let mut resolved_versions = Vec::new();

    for variant in &effective_variants {
        let patterns = variant
            .assets
            .compiled()
            .map_err(|e| MirrorError::SpecInvalid(vec![e]))?;

        for version_info in &upstream_versions {
            if let AssetResolution::Resolved(platforms) = resolver::resolve_assets(&version_info.assets, &patterns)
                && let Ok(normalized) = normalizer::normalize_version(&version_info.version, &build_ts)
            {
                let tagged = match &variant.name {
                    Some(name) => format!("{name}-{normalized}"),
                    None => normalized,
                };
                resolved_versions.push(filter::ResolvedVersion {
                    version: version_info.version.clone(),
                    normalized_version: tagged,
                    variant: variant.name.clone(),
                    platforms,
                    is_prerelease: version_info.is_prerelease,
                });
            }
        }
    }

    // Apply filter pipeline — no exact-version or latest flags for the plan command.
    let filtered = filter::filter_versions(
        resolved_versions,
        &[], // no exact-version pin
        spec.skip_prereleases,
        spec.versions.as_ref(),
        &version_map,
        false, // latest
    );

    // Classify each filtered version: New or BackfillPartial.
    //
    // After filter_versions, each ResolvedVersion.platforms contains ONLY the
    // platforms that still need work (filter_versions trims already-present tiles).
    // To distinguish New from BackfillPartial we need to know whether the version
    // has ANY tile already on the registry.
    //
    // Declared platform set comes from spec.platforms; if absent, every resolved
    // platform is "all declared" so any filtered version must be New.
    let declared_platform_count = spec.platforms.as_ref().map_or(0, |p| p.len());

    let mut version_entries: Vec<PlanVersionEntry> = Vec::with_capacity(filtered.len());

    for rv in &filtered {
        let missing_platforms: Vec<String> = rv.platforms.iter().map(|pa| pa.platform.to_string()).collect();

        // Determine kind: if the version appears in the registry for at least one
        // platform, it's a backfill-partial; otherwise it's entirely new.
        let version_on_registry = Version::parse(&rv.version)
            .is_some_and(|v| all_tags.iter().any(|t| Version::parse(t).is_some_and(|tv| tv == v)));

        let kind = if version_on_registry && declared_platform_count > missing_platforms.len() {
            PlanVersionKind::BackfillPartial
        } else {
            PlanVersionKind::New
        };

        version_entries.push(PlanVersionEntry {
            version: rv.version.clone(),
            platforms: missing_platforms,
            kind,
        });
    }

    // Output is oldest-first (filter_versions already sorts semver ascending).
    let has_new = !version_entries.is_empty();

    let target = format!("{}/{}", spec.target.registry, spec.target.repository);
    let ocx_mirror_rev = spec.ocx_mirror.as_ref().and_then(|c| c.rev.clone());

    Ok(PlanReport {
        schema_version: 1,
        has_new,
        versions: version_entries,
        target,
        ocx_mirror_rev,
    })
}

/// Plain-text rendering of `PlanReport` — one row per version.
fn print_plan_plain(report: &PlanReport) {
    if !report.has_new {
        println!("nothing to do — target is up to date");
        return;
    }

    println!("target: {}", report.target);
    if let Some(rev) = &report.ocx_mirror_rev {
        println!("ocx_mirror_rev: {rev}");
    }
    println!();

    let versions: Vec<String> = report.versions.iter().map(|v| v.version.clone()).collect();
    let kinds: Vec<String> = report
        .versions
        .iter()
        .map(|v| format!("{:?}", v.kind).to_lowercase())
        .collect();
    let platforms: Vec<String> = report.versions.iter().map(|v| v.platforms.join(", ")).collect();

    // Simple aligned table without pulling in Printer::print_table to avoid
    // mutating the Printer reference across the async boundary.
    let v_w = versions.iter().map(|s| s.len()).max().unwrap_or(7).max(7);
    let k_w = kinds.iter().map(|s| s.len()).max().unwrap_or(4).max(4);

    println!("{:<v_w$}  {:<k_w$}  platforms", "version", "kind", v_w = v_w, k_w = k_w);
    println!("{}", "-".repeat(v_w + k_w + 20));

    for ((v, k), p) in versions.iter().zip(kinds.iter()).zip(platforms.iter()) {
        println!("{:<v_w$}  {:<k_w$}  {}", v, k, p, v_w = v_w, k_w = k_w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── §3.5 S5: ocx-mirror pipeline plan — unit tests ────────────────────
    //
    // These tests verify the JSON output schema of PlanReport and the types
    // involved. The actual plan computation (source/registry queries) is
    // exercised via integration tests once execute() is implemented.

    #[test]
    fn plan_report_serializes_schema_version_1() {
        // §3.5: JSON output format matches design spec §2.2 schema exactly.
        // schema_version must be 1.
        let report = PlanReport {
            schema_version: 1,
            has_new: true,
            versions: vec![PlanVersionEntry {
                version: "3.29.0".to_string(),
                platforms: vec!["linux/amd64".to_string(), "darwin/arm64".to_string()],
                kind: PlanVersionKind::New,
            }],
            target: "ocx.sh/cmake".to_string(),
            ocx_mirror_rev: Some("abc123def456".to_string()),
        };

        let value: serde_json::Value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["schema_version"].as_u64().unwrap(), 1);
        assert!(value["has_new"].as_bool().unwrap());
        assert_eq!(value["target"].as_str().unwrap(), "ocx.sh/cmake");
        assert_eq!(value["ocx_mirror_rev"].as_str().unwrap(), "abc123def456");
    }

    #[test]
    fn plan_report_has_new_false_when_no_versions() {
        // §3.5: Empty source + empty target → has_new: false, versions: []
        let report = PlanReport {
            schema_version: 1,
            has_new: false,
            versions: vec![],
            target: "ocx.sh/cmake".to_string(),
            ocx_mirror_rev: None,
        };

        let value: serde_json::Value = serde_json::to_value(&report).unwrap();
        assert!(!value["has_new"].as_bool().unwrap());
        assert!(value["versions"].as_array().unwrap().is_empty());
        // ocx_mirror_rev: null when None (serde default with Option)
    }

    #[test]
    fn plan_version_kind_new_serializes_as_kebab_case() {
        // §3.5: PlanVersionKind::New → "new" in JSON (kebab-case)
        let entry = PlanVersionEntry {
            version: "3.29.0".to_string(),
            platforms: vec!["linux/amd64".to_string()],
            kind: PlanVersionKind::New,
        };
        let value: serde_json::Value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["kind"].as_str().unwrap(), "new");
    }

    #[test]
    fn plan_version_kind_backfill_partial_serializes_as_kebab_case() {
        // §3.5: PlanVersionKind::BackfillPartial → "backfill-partial" in JSON
        let entry = PlanVersionEntry {
            version: "3.28.5".to_string(),
            platforms: vec!["linux/arm64".to_string()],
            kind: PlanVersionKind::BackfillPartial,
        };
        let value: serde_json::Value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["kind"].as_str().unwrap(), "backfill-partial");
    }

    #[test]
    fn plan_report_mixed_new_and_backfill_versions() {
        // §3.5: Mixed: 2 versions present in target, 1 new → only 1 in versions[]
        // This test verifies the schema shape for the mixed case.
        let report = PlanReport {
            schema_version: 1,
            has_new: true,
            versions: vec![
                PlanVersionEntry {
                    version: "3.29.0".to_string(),
                    platforms: vec!["linux/amd64".to_string(), "linux/arm64".to_string()],
                    kind: PlanVersionKind::New,
                },
                PlanVersionEntry {
                    version: "3.28.5".to_string(),
                    platforms: vec!["linux/arm64".to_string()],
                    kind: PlanVersionKind::BackfillPartial,
                },
            ],
            target: "ocx.sh/cmake".to_string(),
            ocx_mirror_rev: None,
        };

        let value: serde_json::Value = serde_json::to_value(&report).unwrap();
        let versions = value["versions"].as_array().unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0]["kind"].as_str().unwrap(), "new");
        assert_eq!(versions[1]["kind"].as_str().unwrap(), "backfill-partial");
        // Partial backfill: only missing platforms listed
        let partial_platforms = versions[1]["platforms"].as_array().unwrap();
        assert_eq!(partial_platforms.len(), 1);
        assert_eq!(partial_platforms[0].as_str().unwrap(), "linux/arm64");
    }

    #[test]
    fn plan_cmd_execute_returns_ok_or_err_not_panic() {
        // §3.5: After implementation, execute() must not panic — it must return
        // a Result (Ok or Err). The prior stub-verification assertion (is_err on
        // catch_unwind) is now inverted: catch_unwind succeeds (is_ok) because
        // execute() no longer calls unimplemented!().
        //
        // When the spec file is absent, execute() returns Err(MirrorError::SourceError)
        // with exit code Unavailable — no panic.
        use std::panic;

        let cmd = PlanCmd {
            spec: std::path::PathBuf::from("./nonexistent-mirror.yml"),
            format: None,
        };
        let printer = ocx_lib::cli::Printer::new(false);
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let _ = rt.block_on(async { cmd.execute(&printer).await });
        }));
        // The closure must NOT panic — catch_unwind returns Ok.
        assert!(
            result.is_ok(),
            "PlanCmd::execute must not panic after implementation; got panic instead of Result"
        );
    }
}
