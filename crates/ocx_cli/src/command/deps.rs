// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    file_structure::FileStructure,
    oci,
    package::{install_info::InstallInfo, metadata::Metadata, resolved_package::ResolvedPackage},
    prelude::SerdeExt,
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

    /// Target platforms to consider when resolving packages.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM")]
    platforms: Vec<oci::Platform>,

    /// Package identifiers to inspect.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Deps {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let platforms = platforms_or_default(&self.platforms);
        let identifiers =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;

        let manager = context.manager();
        let infos = manager.find_all(identifiers, platforms).await?;
        let fs = manager.file_structure();

        if self.flat {
            // Validate no conflicts among exported deps.
            manager.resolve_exported_set(&infos).await?;

            let mut seen = HashSet::new();
            let mut entries = Vec::new();
            for info in &infos {
                // Process resolved deps (accessing .identifier through ResolvedDependency),
                // then the root package itself.
                for dep in &info.resolved.dependencies {
                    if seen.insert(dep.identifier.digest()) {
                        entries.push(api::data::deps::FlatDependency {
                            identifier: oci::Identifier::from(dep.identifier.clone()),
                            exported: dep.exported.into(),
                        });
                    }
                }
                // Root packages are always exported — they are the roots of env resolution.
                if seen.insert(info.resolved.identifier.digest()) {
                    entries.push(api::data::deps::FlatDependency {
                        identifier: oci::Identifier::from(info.resolved.identifier.clone()),
                        exported: api::data::deps::ExportStatus::Exported,
                    });
                }
            }
            context.api().report(&api::data::deps::FlatDependencies::new(entries))?;
        } else if let Some(ref why_pkg) = self.why {
            // Why view: find all paths from roots to target via metadata + deps/ symlinks.
            let why_id = why_pkg.with_domain(context.default_registry())?;
            let mut all_paths = Vec::new();

            for info in &infos {
                let mut current_path = vec![oci::Identifier::from(info.identifier.clone())];
                find_paths_to(fs, &info.content, &why_id, &mut current_path, &mut all_paths).await;
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
            // Tree view (default): walk metadata deps + resolve via deps/ symlinks.
            let mut seen = HashSet::new();
            let max_depth = self.depth.unwrap_or(usize::MAX);
            let mut tree_roots = Vec::with_capacity(infos.len());
            for info in &infos {
                tree_roots.push(build_tree_node(fs, info, true, max_depth, 0, &mut seen).await);
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
    exported: bool,
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
            let dep_contents = read_deps_symlinks(fs, &info.content);
            let mut children = Vec::new();
            for dep in info.metadata.dependencies() {
                if let Some(dep_info) = resolve_dep(fs, &dep.identifier, &dep_contents).await {
                    children.push(build_tree_node(fs, &dep_info, dep.export, max_depth, current_depth + 1, seen).await);
                }
            }
            children
        };

        api::data::deps::Dependency {
            identifier: info.identifier.clone().into(),
            repeated: is_repeated,
            exported,
            dependencies: children,
        }
    })
}

// ── Why view helpers ─────────────────────────────────────────────────

fn find_paths_to<'a>(
    fs: &'a FileStructure,
    content_path: &'a Path,
    target: &'a oci::Identifier,
    current_path: &'a mut Vec<oci::Identifier>,
    all_paths: &'a mut Vec<Vec<oci::Identifier>>,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let metadata_path = content_path.with_file_name("metadata.json");
        let Ok(metadata) = Metadata::read_json(&metadata_path).await else {
            return;
        };

        let dep_contents = read_deps_symlinks(fs, content_path);

        for dep in metadata.dependencies() {
            let dep_id = &dep.identifier;
            current_path.push(dep_id.clone().into());

            if dep_id.registry() == target.registry() && dep_id.repository() == target.repository() {
                all_paths.push(current_path.clone());
            }

            if let Some(dep_info) = resolve_dep(fs, dep_id, &dep_contents).await {
                find_paths_to(fs, &dep_info.content, target, current_path, all_paths).await;
            }

            current_path.pop();
        }
    })
}

// ── Shared resolution helpers ────────────────────────────────────────

/// Reads deps/ symlinks for a package and returns their canonicalized content paths.
fn read_deps_symlinks(fs: &FileStructure, content_path: &Path) -> Vec<PathBuf> {
    let deps_dir = match fs.objects.deps_dir_for_content(content_path) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let entries = match std::fs::read_dir(&deps_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    entries
        .flatten()
        .filter_map(|e| {
            // canonicalize follows symlinks and resolves to absolute path.
            let canonical = std::fs::canonicalize(e.path()).ok()?;
            if canonical.is_dir() { Some(canonical) } else { None }
        })
        .collect()
}

/// Resolves a declared dependency to an InstallInfo.
///
/// Tries direct digest lookup first (single-platform manifests), then
/// falls back to matching deps/ symlink targets by repository directory.
async fn resolve_dep(fs: &FileStructure, id: &oci::Identifier, dep_contents: &[PathBuf]) -> Option<InstallInfo> {
    // Primary: direct digest lookup.
    if let Ok(pinned) = oci::PinnedIdentifier::try_from(id.clone()) {
        let content = fs.objects.content(&pinned);
        if content.is_dir() {
            return load_install_info(content).await;
        }
    }

    // Fallback: match by repository directory prefix in deps/ symlinks.
    let repo_dir_raw = fs.objects.repository_dir(id);
    let repo_dir = std::fs::canonicalize(&repo_dir_raw).unwrap_or(repo_dir_raw);
    for content in dep_contents {
        if content.starts_with(&repo_dir) {
            return load_install_info(content.clone()).await;
        }
    }
    None
}

async fn load_install_info(content: PathBuf) -> Option<InstallInfo> {
    let (metadata, resolved) = tokio::join!(
        Metadata::read_json(content.with_file_name("metadata.json")),
        ResolvedPackage::read_json(content.with_file_name("resolve.json")),
    );
    let metadata = metadata.ok()?;
    let resolved = resolved.ok()?;
    Some(InstallInfo {
        identifier: resolved.identifier.clone(),
        metadata,
        resolved,
        content,
    })
}
