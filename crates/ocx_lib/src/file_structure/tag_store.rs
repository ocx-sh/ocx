// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use crate::utility::fs::{self, DirWalker, WalkDecision};
use crate::{Result, oci};

/// Manages the local tag store on the filesystem.
///
/// The tag store maps tags to digests for each repository, enabling
/// offline resolution and fast tag lookups without querying a remote registry.
///
/// Layout:
/// ```text
/// {root}/
///   {registry_slug}/
///     {repository}.json      — tag→digest map for the repository
/// ```
#[derive(Debug, Clone)]
pub struct TagStore {
    root: PathBuf,
}

impl TagStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the tag store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path to the tag-to-digest map JSON file for the given identifier.
    pub fn tags(&self, identifier: &oci::Identifier) -> PathBuf {
        self.root
            .join(super::slugify(identifier.registry()))
            .join(super::repository_path(identifier.repository()))
            .with_added_extension("json")
    }

    /// Lists all repository names stored for the given registry.
    ///
    /// Scans `{root}/{registry_slug}/` for `.json` files and returns their
    /// stems as repository names, sorted alphabetically.
    ///
    /// Returns an empty vec if the directory does not exist.
    ///
    /// Uses [`DirWalker`] with a classify function that reads each directory
    /// (sync `std::fs::read_dir` inside semaphore-bounded spawned tasks)
    /// and collects `.json` file stems as repository name fragments.
    pub async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        let registry_dir = self.root.join(super::slugify(registry));
        if !fs::path_exists_lossy(&registry_dir).await {
            return Ok(Vec::new());
        }

        let root = registry_dir.clone();
        let mut repos: Vec<String> = DirWalker::new(registry_dir, move |dir: &Path, _depth| {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return WalkDecision::skip();
            };

            let mut repos = Vec::new();
            let mut has_subdirs = false;

            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    has_subdirs = true;
                } else if path.extension().is_some_and(|e| e == "json") {
                    let stem_path = path.with_extension("");
                    if let Ok(relative) = stem_path.strip_prefix(&root) {
                        let repo_name = relative
                            .components()
                            .map(|c| c.as_os_str().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("/");
                        repos.push(repo_name);
                    }
                }
            }

            match (repos.is_empty(), has_subdirs) {
                (true, true) => WalkDecision::descend(),
                (true, false) => WalkDecision::skip(),
                (false, true) => WalkDecision::collect_and_descend(repos),
                (false, false) => WalkDecision::collect(repos),
            }
        })
        .walk()
        .await?;

        repos.sort();
        repos.dedup();
        Ok(repos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;

    fn id() -> oci::Identifier {
        oci::Identifier::new_registry("cmake", "example.com").clone_with_tag("3.28")
    }

    // -- path methods ---------------------------------------------------------

    #[test]
    fn tags_path_structure() {
        let store = TagStore::new("/tags");
        let p = store.tags(&id());
        assert_eq!(p, PathBuf::from("/tags/example.com/cmake.json"));
    }

    #[test]
    fn tags_path_nested_repo() {
        let store = TagStore::new("/tags");
        let id = oci::Identifier::new_registry("org/sub/pkg", "example.com").clone_with_tag("1.0");
        let p = store.tags(&id);
        let expected = Path::new("/tags")
            .join("example.com")
            .join("org")
            .join("sub")
            .join("pkg.json");
        assert_eq!(p, expected);
    }

    // -- list_repositories ----------------------------------------------------

    #[tokio::test]
    async fn list_repositories_returns_empty_when_registry_dir_absent() {
        let store = TagStore::new("/nonexistent/path/that/does/not/exist");
        assert_eq!(
            store.list_repositories("example.com").await.unwrap(),
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn list_repositories_returns_sorted_repository_names() {
        let dir = tempfile::tempdir().unwrap();
        let registry_dir = dir.path().join("example.com");
        tokio::fs::create_dir_all(&registry_dir).await.unwrap();
        tokio::fs::write(registry_dir.join("zlib.json"), b"{}").await.unwrap();
        tokio::fs::write(registry_dir.join("cmake.json"), b"{}").await.unwrap();
        tokio::fs::write(registry_dir.join("clang.json"), b"{}").await.unwrap();

        let store = TagStore::new(dir.path());
        let repos = store.list_repositories("example.com").await.unwrap();
        assert_eq!(repos, vec!["clang", "cmake", "zlib"]);
    }

    #[tokio::test]
    async fn list_repositories_finds_nested_repos() {
        let dir = tempfile::tempdir().unwrap();
        let registry_dir = dir.path().join("example.com");
        tokio::fs::create_dir_all(&registry_dir).await.unwrap();
        tokio::fs::write(registry_dir.join("cmake.json"), b"{}").await.unwrap();
        let nested = registry_dir.join("org").join("sub");
        tokio::fs::create_dir_all(&nested).await.unwrap();
        tokio::fs::write(nested.join("pkg.json"), b"{}").await.unwrap();

        let store = TagStore::new(dir.path());
        let repos = store.list_repositories("example.com").await.unwrap();
        assert_eq!(repos, vec!["cmake", "org/sub/pkg"]);
    }

    #[tokio::test]
    async fn list_repositories_ignores_non_json_files() {
        let dir = tempfile::tempdir().unwrap();
        let registry_dir = dir.path().join("example.com");
        tokio::fs::create_dir_all(&registry_dir).await.unwrap();
        tokio::fs::write(registry_dir.join("cmake.json"), b"{}").await.unwrap();
        tokio::fs::write(registry_dir.join("README.txt"), b"ignore me")
            .await
            .unwrap();
        tokio::fs::write(registry_dir.join("notes"), b"no extension")
            .await
            .unwrap();

        let store = TagStore::new(dir.path());
        let repos = store.list_repositories("example.com").await.unwrap();
        assert_eq!(repos, vec!["cmake"]);
    }
}
