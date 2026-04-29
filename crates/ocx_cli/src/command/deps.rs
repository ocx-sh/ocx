// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    file_structure::FileStructure,
    oci,
    package::{
        install_info::InstallInfo,
        metadata::{Metadata, ValidMetadata, visibility::Visibility},
        resolved_package::ResolvedPackage,
    },
    prelude::SerdeExt,
    utility,
};

use crate::{api, conventions::platforms_or_default, options};

/// Show the dependency tree for one or more packages.
///
/// Default output is a logical tree showing declared dependencies.
/// Use `--flat` for the resolved evaluation order (same order as `ocx exec`).
/// Use `--why <dep>` to trace why a dependency is pulled in.
///
/// Operates on locally-present packages only — no auto-install.
#[derive(Parser)]
pub struct Deps {
    /// Show the flattened evaluation order instead of the tree.
    #[clap(long, conflicts_with = "why")]
    flat: bool,

    /// Explain why a dependency is pulled in (matches by registry and repository; tag is ignored).
    #[clap(long, value_name = "DEP", conflicts_with = "flat")]
    why: Option<options::Identifier>,

    /// Limit tree depth (default: unlimited). Only applies to tree view.
    #[clap(long, value_name = "N", conflicts_with_all = ["flat", "why"])]
    depth: Option<usize>,

    #[clap(flatten)]
    platforms: options::PlatformsFlag,

    /// Package identifiers to inspect.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Deps {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let platforms = platforms_or_default(self.platforms.as_slice());
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        let manager = context.manager();
        let infos = manager.find_all(identifiers, platforms).await?;
        let fs = manager.file_structure();

        if self.flat {
            // Validate no conflicts among visible deps.
            manager.resolve_visible_set(&infos).await?;

            let mut seen = HashSet::new();
            let mut entries = Vec::new();
            for info in &infos {
                // Process resolved deps (accessing .identifier through ResolvedDependency),
                // then the root package itself.
                for dep in &info.resolved.dependencies {
                    if seen.insert(dep.identifier.digest()) {
                        entries.push(api::data::deps::FlatDependency {
                            identifier: oci::Identifier::from(dep.identifier.clone()),
                            visibility: dep.visibility,
                        });
                    }
                }
                // Root packages are always public — they are the roots of env resolution.
                if seen.insert(info.identifier.digest()) {
                    entries.push(api::data::deps::FlatDependency {
                        identifier: oci::Identifier::from(info.identifier.clone()),
                        visibility: ocx_lib::package::metadata::visibility::Visibility::Public,
                    });
                }
            }
            context.api().report(&api::data::deps::FlatDependencies::new(entries))?;
        } else if let Some(ref why_pkg) = self.why {
            // Why view: find all paths from roots to target via resolve.json.
            let why_id = why_pkg.with_domain(context.default_registry())?;
            let mut all_paths = Vec::new();

            for info in &infos {
                let mut current_path = vec![oci::Identifier::from(info.identifier.clone())];
                find_paths_to(fs, info, &why_id, &mut current_path, &mut all_paths).await;
            }

            if all_paths.is_empty() {
                let root_names: Vec<_> = infos.iter().map(|r| r.identifier.to_string()).collect();
                let msg = format!("{} is not a dependency of {}", why_id, root_names.join(", "));
                let data = api::data::deps::DependenciesTrace {
                    paths: vec![],
                    message: Some(msg),
                };
                context.api().report(&data)?;
                return Ok(ExitCode::FAILURE);
            }

            context
                .api()
                .report(&api::data::deps::DependenciesTrace::new(all_paths))?;
        } else {
            // Tree view (default): walk metadata deps, resolve via resolve.json.
            let mut seen = HashSet::new();
            let max_depth = self.depth.unwrap_or(usize::MAX);
            let mut tree_roots = Vec::with_capacity(infos.len());
            for info in &infos {
                tree_roots.push(build_tree_node(fs, info, None, max_depth, 0, &mut seen).await);
            }
            context.api().report(&api::data::deps::Dependencies::new(tree_roots))?;
        }

        Ok(ExitCode::SUCCESS)
    }
}

// ── Tree view helpers ────────────────────────────────────────────────

fn build_tree_node<'a>(
    fs: &'a FileStructure,
    info: &'a InstallInfo,
    visibility: Option<Visibility>,
    max_depth: usize,
    current_depth: usize,
    seen: &'a mut HashSet<String>,
) -> Pin<Box<dyn Future<Output = api::data::deps::Dependency> + 'a>> {
    Box::pin(async move {
        let key = info.identifier.to_string();
        let is_repeated = !seen.insert(key);

        let children = if is_repeated || current_depth >= max_depth {
            Vec::new()
        } else {
            // Resolve declared deps to content paths via resolve.json (not deps/ symlinks).
            // The resolved transitive closure maps (registry, repo) → platform-specific identifier.
            let resolved_map = resolved_dep_map(&info.resolved);
            let mut children = Vec::new();
            for dep in info.metadata.dependencies() {
                if let Some(dep_info) = resolve_dep_via_metadata(fs, &dep.identifier, &resolved_map).await {
                    children.push(
                        build_tree_node(fs, &dep_info, Some(dep.visibility), max_depth, current_depth + 1, seen).await,
                    );
                }
            }
            children
        };

        api::data::deps::Dependency {
            identifier: info.identifier.clone().into(),
            repeated: is_repeated,
            visibility,
            dependencies: children,
        }
    })
}

// ── Why view helpers ─────────────────────────────────────────────────

fn find_paths_to<'a>(
    fs: &'a FileStructure,
    info: &'a InstallInfo,
    target: &'a oci::Identifier,
    current_path: &'a mut Vec<oci::Identifier>,
    all_paths: &'a mut Vec<Vec<oci::Identifier>>,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        // Resolve declared deps via resolve.json (not deps/ symlinks).
        let resolved_map = resolved_dep_map(&info.resolved);

        for dep in info.metadata.dependencies() {
            let dep_id = &dep.identifier;
            current_path.push(dep_id.clone().into());

            if oci::Repository::from(dep_id.as_identifier()) == oci::Repository::from(target) {
                all_paths.push(current_path.clone());
            }

            if let Some(dep_info) = resolve_dep_via_metadata(fs, dep_id, &resolved_map).await {
                find_paths_to(fs, &dep_info, target, current_path, all_paths).await;
            }

            current_path.pop();
        }
    })
}

// ── Shared resolution helpers ────────────────────────────────────────

/// Builds a lookup from `(registry, repo)` to the platform-resolved
/// [`PinnedIdentifier`] using the pre-computed resolve.json data.
///
/// This replaces the previous `read_deps_symlinks` approach — resolve.json
/// is the canonical source written at pull time, and digest-pinned metadata
/// cannot form cycles by construction.
fn resolved_dep_map(resolved: &ResolvedPackage) -> HashMap<oci::Repository, &oci::PinnedIdentifier> {
    resolved
        .dependencies
        .iter()
        .map(|rd| (oci::Repository::from(&*rd.identifier), &rd.identifier))
        .collect()
}

/// Resolves a declared dependency to an [`InstallInfo`] using the parent's
/// resolve.json map (not deps/ symlinks).
async fn resolve_dep_via_metadata(
    fs: &FileStructure,
    id: &oci::PinnedIdentifier,
    resolved_map: &HashMap<oci::Repository, &oci::PinnedIdentifier>,
) -> Option<InstallInfo> {
    // Primary: direct digest lookup (single-platform manifests where
    // the declared digest matches the stored content digest).
    let content = fs.packages.content(id);
    if utility::fs::path_exists_lossy(&content).await {
        return load_install_info(id.clone(), content).await;
    }

    // Fallback: the declared digest is an Image Index digest — look up the
    // platform-resolved identifier from the parent's resolve.json.
    let repo_key = oci::Repository::from(&**id);
    let resolved_id = resolved_map.get(&repo_key)?;
    let content = fs.packages.content(resolved_id);
    load_install_info((*resolved_id).clone(), content).await
}

async fn load_install_info(identifier: oci::PinnedIdentifier, content: std::path::PathBuf) -> Option<InstallInfo> {
    let (metadata, resolved) = tokio::join!(
        Metadata::read_json(content.with_file_name("metadata.json")),
        ResolvedPackage::read_json(content.with_file_name("resolve.json")),
    );
    // Enforce the ValidMetadata typestate: tampered or invalid metadata is silently
    // skipped (returns None) rather than fed to downstream graph traversal.
    let metadata = ValidMetadata::try_from(metadata.ok()?).ok()?.into();
    let resolved = resolved.ok()?;
    Some(InstallInfo {
        identifier,
        metadata,
        resolved,
        content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(n: u8) -> String {
        format!("{:x}", n).repeat(64).chars().take(64).collect()
    }

    /// `load_install_info` must return `None` when `metadata.json` contains metadata
    /// that fails `ValidMetadata` validation (e.g. an env token referencing a dep name
    /// absent from the dependency list).
    #[tokio::test]
    async fn load_install_info_returns_none_for_invalid_metadata() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let content = dir.path().join("content");
        tokio::fs::create_dir_all(&content).await.expect("create content dir");

        // Build metadata that references "ninja" in an env token but declares no such dep.
        // ValidMetadata::try_from rejects this with an UnknownDependencyRef error.
        let h = hex(1);
        let metadata_json = format!(
            r#"{{"type":"bundle","version":1,"dependencies":[{{"identifier":"ocx.sh/cmake:1@sha256:{h}"}}],"env":[{{"key":"PATH","type":"constant","value":"${{deps.ninja.installPath}}/bin"}}]}}"#
        );
        tokio::fs::write(content.with_file_name("metadata.json"), &metadata_json)
            .await
            .expect("write metadata.json");

        // Write a valid resolve.json so the only failure comes from metadata validation.
        // ResolvedPackage uses #[serde(deny_unknown_fields)] — passing a `version` field
        // would short-circuit the test before it reaches the ValidMetadata gate.
        let resolve_json = r#"{"dependencies":[]}"#;
        tokio::fs::write(content.with_file_name("resolve.json"), resolve_json)
            .await
            .expect("write resolve.json");

        let identifier: oci::Identifier = format!("ocx.sh/cmake:1@sha256:{h}").parse().expect("valid identifier");
        let pinned = oci::PinnedIdentifier::try_from(identifier).expect("identifier has digest");

        let result = load_install_info(pinned, content).await;
        assert!(
            result.is_none(),
            "load_install_info must return None for invalid metadata"
        );
    }
}
