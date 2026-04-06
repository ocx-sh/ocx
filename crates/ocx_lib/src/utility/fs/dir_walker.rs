// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::Result;

/// Default maximum number of concurrent directory reads.
const DEFAULT_CONCURRENCY: usize = 50;

/// Instructs the walker how to handle a directory entry.
///
/// Each directory can yield zero or more items AND optionally descend into
/// children. Use the convenience constructors for common patterns.
pub struct WalkDecision<T> {
    /// Items to yield from this directory.
    pub items: Vec<T>,
    /// Whether to recurse into child directories.
    pub descend: bool,
    /// Child directory names to skip when descending.
    pub skip_names: &'static [&'static str],
}

impl<T> WalkDecision<T> {
    /// This directory is a leaf — include one item, do not recurse.
    pub fn leaf(item: T) -> Self {
        Self {
            items: vec![item],
            descend: false,
            skip_names: &[],
        }
    }

    /// Recurse into child directories with no skip list.
    pub fn descend() -> Self {
        Self {
            items: Vec::new(),
            descend: true,
            skip_names: &[],
        }
    }

    /// Recurse into child directories, skipping children whose file name
    /// matches one of the provided names.
    pub fn descend_skip(skip_names: &'static [&'static str]) -> Self {
        Self {
            items: Vec::new(),
            descend: true,
            skip_names,
        }
    }

    /// Skip this directory entirely (no items, no recursion).
    pub fn skip() -> Self {
        Self {
            items: Vec::new(),
            descend: false,
            skip_names: &[],
        }
    }

    /// Yield multiple items from this directory, do not recurse.
    pub fn collect(items: Vec<T>) -> Self {
        Self {
            items,
            descend: false,
            skip_names: &[],
        }
    }

    /// Yield multiple items from this directory AND recurse into children.
    pub fn collect_and_descend(items: Vec<T>) -> Self {
        Self {
            items,
            descend: true,
            skip_names: &[],
        }
    }
}

/// Async BFS directory walker with semaphore-bounded concurrency.
///
/// Walks from `root` through the directory tree.  For each directory,
/// a `classify` function decides whether it is a leaf result, should be
/// skipped, or should be recursed into.
///
/// # Defaults
///
/// - `max_depth`: unlimited (`usize::MAX`)
/// - `concurrency`: 50 parallel directory reads
///
/// # Example
///
/// ```ignore
/// let results = DirWalker::new("/store", classify_fn)
///     .max_depth(10)
///     .walk()
///     .await?;
/// ```
///
/// Results are sorted by path for deterministic output.
///
/// # Classify and blocking I/O
///
/// The `classify` function has a synchronous signature and runs inside
/// spawned tokio tasks.  Callers performing filesystem checks (e.g.,
/// `is_dir()`, `is_file()`) inside `classify` should keep them fast —
/// a single `stat()` is acceptable, but heavy I/O should be avoided.
/// Concurrency is bounded by the semaphore, so at most `concurrency`
/// blocking calls can be in-flight simultaneously.
pub struct DirWalker<T, F>
where
    T: Send + 'static,
    F: Fn(&Path, usize) -> WalkDecision<T> + Send + Sync + 'static,
{
    root: PathBuf,
    classify: F,
    max_depth: usize,
    concurrency: usize,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, F> DirWalker<T, F>
where
    T: Send + 'static,
    F: Fn(&Path, usize) -> WalkDecision<T> + Send + Sync + 'static,
{
    /// Creates a new walker rooted at `root` with the given classification function.
    pub fn new(root: impl Into<PathBuf>, classify: F) -> Self {
        Self {
            root: root.into(),
            classify,
            max_depth: usize::MAX,
            concurrency: DEFAULT_CONCURRENCY,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Sets the maximum recursion depth. Directories deeper than this are
    /// not explored. Default: unlimited.
    pub fn max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }

    /// Sets the maximum number of concurrent directory reads.
    /// Default: 50. Values below 1 are clamped to 1.
    pub fn concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    /// Executes the walk and returns all leaf results sorted by path.
    pub async fn walk(self) -> Result<Vec<T>> {
        let classify = Arc::new(self.classify);
        let sem = Arc::new(Semaphore::new(self.concurrency));
        let max_depth = self.max_depth;
        let mut tasks: JoinSet<Result<ExploreResult<T>>> = JoinSet::new();
        let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
        let mut results: Vec<(PathBuf, T)> = Vec::new();

        queue.push_back((self.root, 0));

        loop {
            while let Some((dir, depth)) = queue.pop_front() {
                let sem = Arc::clone(&sem);
                let classify = Arc::clone(&classify);
                tasks.spawn(async move {
                    let _permit = sem.acquire_owned().await.expect("semaphore closed");
                    explore_dir(dir, depth, max_depth, classify.as_ref()).await
                });
            }
            if tasks.is_empty() {
                break;
            }
            // Drain all pending tasks before spawning the next wave.
            while let Some(result) = tasks.join_next().await {
                let result = result.expect("task panicked")?;
                results.extend(result.items);
                queue.extend(result.children);
            }
        }

        results.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(results.into_iter().map(|(_, item)| item).collect())
    }
}

struct ExploreResult<T> {
    items: Vec<(PathBuf, T)>,
    children: Vec<(PathBuf, usize)>,
}

async fn explore_dir<T, F>(dir: PathBuf, depth: usize, max_depth: usize, classify: &F) -> Result<ExploreResult<T>>
where
    T: Send + 'static,
    F: Fn(&Path, usize) -> WalkDecision<T> + Send + Sync,
{
    let decision = classify(&dir, depth);

    let items: Vec<(PathBuf, T)> = decision.items.into_iter().map(|item| (dir.clone(), item)).collect();

    if !decision.descend {
        return Ok(ExploreResult {
            items,
            children: Vec::new(),
        });
    }

    if depth >= max_depth {
        crate::log::warn!("Directory walk hit max depth at '{}'", dir.display());
        return Ok(ExploreResult {
            items,
            children: Vec::new(),
        });
    }

    let mut entries = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| crate::Error::InternalFile(dir.clone(), e))?;
    let mut children = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| crate::Error::InternalFile(dir.clone(), e))?
    {
        let path = entry.path();
        if !entry
            .file_type()
            .await
            .map_err(|e| crate::Error::InternalFile(path.clone(), e))?
            .is_dir()
        {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && decision.skip_names.contains(&name)
        {
            continue;
        }
        children.push((path, depth + 1));
    }
    Ok(ExploreResult { items, children })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    /// Auto-cleaned temporary directory tree builder for walker tests.
    struct TreeBuilder {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    impl TreeBuilder {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dunce::canonicalize(dir.path()).unwrap();
            Self { _dir: dir, root }
        }

        fn root(&self) -> &Path {
            &self.root
        }

        /// Creates a directory at the given relative path under root.
        fn mkdir(&self, rel: &str) -> PathBuf {
            let p = self.root.join(rel);
            std::fs::create_dir_all(&p).unwrap();
            p
        }

        /// Creates a file at the given relative path under root.
        fn touch(&self, rel: &str) -> PathBuf {
            let p = self.root.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, b"").unwrap();
            p
        }
    }

    type ClassifyFn = fn(&Path, usize) -> WalkDecision<PathBuf>;

    /// Helper: creates a default walker with marker-based classify.
    fn walker(tree: &TreeBuilder) -> DirWalker<PathBuf, ClassifyFn> {
        DirWalker::new(tree.root(), marker_classify)
    }

    /// Classify that treats any directory containing a "marker" file as a leaf.
    fn marker_classify(dir: &Path, _depth: usize) -> WalkDecision<PathBuf> {
        if dir.join("marker").is_file() {
            return WalkDecision::leaf(dir.to_path_buf());
        }
        WalkDecision::descend()
    }

    /// Classify that skips directories named "skip_me".
    fn skip_classify(dir: &Path, _depth: usize) -> WalkDecision<PathBuf> {
        if dir.join("marker").is_file() {
            return WalkDecision::leaf(dir.to_path_buf());
        }
        WalkDecision::descend_skip(&["skip_me"])
    }

    /// Classify that returns every directory as a leaf (no recursion).
    fn leaf_everything(dir: &Path, _depth: usize) -> WalkDecision<PathBuf> {
        WalkDecision::leaf(dir.to_path_buf())
    }

    /// Classify that skips every directory (no results, no recursion).
    fn skip_everything(_dir: &Path, _depth: usize) -> WalkDecision<PathBuf> {
        WalkDecision::skip()
    }

    /// Shared classify: accepts markers only at depth 3, skips markers at other depths.
    fn depth_aware(dir: &Path, depth: usize) -> WalkDecision<PathBuf> {
        if dir.join("marker").is_file() {
            if depth == 3 {
                return WalkDecision::leaf(dir.to_path_buf());
            }
            return WalkDecision::skip();
        }
        WalkDecision::descend()
    }

    // ── empty / nonexistent ─────────────────────────────────────────────

    #[tokio::test]
    async fn empty_root_returns_empty() {
        let tree = TreeBuilder::new();
        let results = walker(&tree).walk().await.unwrap();
        assert!(results.is_empty());
    }

    // ── single leaf ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn single_leaf_at_root() {
        let tree = TreeBuilder::new();
        tree.touch("marker");
        let results = walker(&tree).walk().await.unwrap();
        assert_eq!(results, vec![tree.root().to_path_buf()]);
    }

    #[tokio::test]
    async fn single_leaf_nested() {
        let tree = TreeBuilder::new();
        let leaf = tree.mkdir("a/b/c");
        tree.touch("a/b/c/marker");
        let results = walker(&tree).walk().await.unwrap();
        assert_eq!(results, vec![leaf]);
    }

    // ── multiple leaves ─────────────────────────────────────────────────

    #[tokio::test]
    async fn multiple_leaves_sorted() {
        let tree = TreeBuilder::new();
        let a = tree.mkdir("x/a");
        tree.touch("x/a/marker");
        let b = tree.mkdir("x/b");
        tree.touch("x/b/marker");
        let c = tree.mkdir("y/c");
        tree.touch("y/c/marker");

        let results = walker(&tree).walk().await.unwrap();
        assert_eq!(results, vec![a, b, c]);
    }

    #[tokio::test]
    async fn leaves_at_different_depths() {
        let tree = TreeBuilder::new();
        let shallow = tree.mkdir("a");
        tree.touch("a/marker");
        let deep = tree.mkdir("b/c/d/e");
        tree.touch("b/c/d/e/marker");

        let results = walker(&tree).walk().await.unwrap();
        assert_eq!(results, vec![shallow, deep]);
    }

    // ── skip_names ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn skip_names_excludes_matching_children() {
        let tree = TreeBuilder::new();
        let good = tree.mkdir("a/good");
        tree.touch("a/good/marker");
        // This leaf is inside a skipped directory — should not appear.
        tree.mkdir("a/skip_me/hidden");
        tree.touch("a/skip_me/hidden/marker");

        let results = DirWalker::new(tree.root(), skip_classify).walk().await.unwrap();
        assert_eq!(results, vec![good]);
    }

    #[tokio::test]
    async fn skip_names_does_not_affect_non_matching() {
        let tree = TreeBuilder::new();
        let keep = tree.mkdir("a/keep_me");
        tree.touch("a/keep_me/marker");

        let results = DirWalker::new(tree.root(), skip_classify).walk().await.unwrap();
        assert_eq!(results, vec![keep]);
    }

    // ── max_depth ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn default_max_depth_is_unlimited() {
        let tree = TreeBuilder::new();
        // 15 levels deep — reachable with default (unlimited) max_depth.
        let mut path = String::new();
        for i in 0..15 {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(&format!("d{i}"));
        }
        let leaf = tree.mkdir(&path);
        tree.touch(&format!("{path}/marker"));

        let results = walker(&tree).walk().await.unwrap();
        assert_eq!(results, vec![leaf]);
    }

    #[tokio::test]
    async fn max_depth_zero_only_classifies_root() {
        let tree = TreeBuilder::new();
        tree.touch("marker");
        let results = walker(&tree).max_depth(0).walk().await.unwrap();
        // Root itself is classified — if it has marker, it's a leaf.
        assert_eq!(results, vec![tree.root().to_path_buf()]);
    }

    #[tokio::test]
    async fn max_depth_zero_does_not_descend() {
        let tree = TreeBuilder::new();
        // Root has no marker, so classify returns Descend, but depth 0 >= max_depth 0 → stop.
        tree.mkdir("a");
        tree.touch("a/marker");
        let results = walker(&tree).max_depth(0).walk().await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn max_depth_limits_recursion() {
        let tree = TreeBuilder::new();
        // Leaf at depth 2 — reachable with max_depth >= 3.
        let reachable = tree.mkdir("a/b");
        tree.touch("a/b/marker");
        // Leaf at depth 4 — unreachable with max_depth 3.
        tree.mkdir("c/d/e/f");
        tree.touch("c/d/e/f/marker");

        let results = walker(&tree).max_depth(3).walk().await.unwrap();
        assert_eq!(results, vec![reachable]);
    }

    // ── concurrency ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn custom_concurrency_still_finds_all() {
        let tree = TreeBuilder::new();
        let mut expected = Vec::new();
        for i in 0..20 {
            let dir = tree.mkdir(&format!("dir_{i:02}"));
            tree.touch(&format!("dir_{i:02}/marker"));
            expected.push(dir);
        }
        expected.sort();

        // concurrency=1 forces sequential execution — results should be the same.
        let results = walker(&tree).concurrency(1).walk().await.unwrap();
        assert_eq!(results, expected);
    }

    #[tokio::test]
    async fn concurrency_zero_clamps_to_one() {
        let tree = TreeBuilder::new();
        let leaf = tree.mkdir("a");
        tree.touch("a/marker");

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), walker(&tree).concurrency(0).walk())
            .await
            .expect("walk should not hang with concurrency(0)");
        assert_eq!(result.unwrap(), vec![leaf]);
    }

    // ── leaf stops recursion ────────────────────────────────────────────

    #[tokio::test]
    async fn leaf_does_not_recurse_into_children() {
        let tree = TreeBuilder::new();
        let leaf = tree.mkdir("a");
        tree.touch("a/marker");
        // Nested marker inside leaf — should NOT produce a second result.
        tree.mkdir("a/nested");
        tree.touch("a/nested/marker");

        let results = walker(&tree).walk().await.unwrap();
        assert_eq!(results, vec![leaf]);
    }

    // ── skip stops recursion ────────────────────────────────────────────

    #[tokio::test]
    async fn skip_does_not_recurse_into_children() {
        let tree = TreeBuilder::new();
        // Root is classified as Skip — nothing should be found.
        let results = DirWalker::new(tree.root(), skip_everything).walk().await.unwrap();
        assert!(results.is_empty());
    }

    // ── leaf_everything ─────────────────────────────────────────────────

    #[tokio::test]
    async fn leaf_everything_returns_only_root() {
        let tree = TreeBuilder::new();
        tree.mkdir("a/b");
        tree.mkdir("c");
        // Since root is immediately a leaf, no children are explored.
        let results = DirWalker::new(tree.root(), leaf_everything).walk().await.unwrap();
        assert_eq!(results, vec![tree.root().to_path_buf()]);
    }

    // ── files are ignored ───────────────────────────────────────────────

    #[tokio::test]
    async fn files_are_not_descended_into() {
        let tree = TreeBuilder::new();
        tree.touch("a_file.txt");
        tree.touch("another.bin");
        let leaf = tree.mkdir("real_dir");
        tree.touch("real_dir/marker");

        let results = walker(&tree).walk().await.unwrap();
        assert_eq!(results, vec![leaf]);
    }

    // ── wide tree (concurrency stress) ──────────────────────────────────

    #[tokio::test]
    async fn wide_tree_many_leaves() {
        let tree = TreeBuilder::new();
        let mut expected = Vec::new();
        for i in 0..100 {
            let dir = tree.mkdir(&format!("dir_{i:03}"));
            tree.touch(&format!("dir_{i:03}/marker"));
            expected.push(dir);
        }
        expected.sort();

        let results = walker(&tree).walk().await.unwrap();
        assert_eq!(results, expected);
    }

    // ── deep tree ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn deep_tree_with_generous_depth() {
        let tree = TreeBuilder::new();
        let mut path = String::new();
        for i in 0..8 {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(&format!("d{i}"));
        }
        let leaf = tree.mkdir(&path);
        tree.touch(&format!("{path}/marker"));

        let results = walker(&tree).walk().await.unwrap();
        assert_eq!(results, vec![leaf]);
    }

    // ── classify receives correct depth ─────────────────────────────────

    #[tokio::test]
    async fn classify_receives_correct_depth() {
        let tree = TreeBuilder::new();
        tree.mkdir("a/b/c");
        tree.touch("a/b/c/marker");

        let results = DirWalker::new(tree.root(), depth_aware).walk().await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn classify_depth_mismatch_skips() {
        let tree = TreeBuilder::new();
        // Marker at depth 1 — but our classify only accepts depth 3.
        tree.mkdir("a");
        tree.touch("a/marker");

        let results = DirWalker::new(tree.root(), depth_aware).walk().await.unwrap();
        assert!(results.is_empty());
    }

    // ── mixed structure ─────────────────────────────────────────────────

    #[tokio::test]
    async fn mixed_files_dirs_leaves_and_skips() {
        let tree = TreeBuilder::new();
        tree.touch("root_file.txt");
        let leaf1 = tree.mkdir("a");
        tree.touch("a/marker");
        tree.mkdir("b");
        tree.touch("b/not_a_marker.txt");
        let leaf2 = tree.mkdir("b/nested");
        tree.touch("b/nested/marker");
        tree.mkdir("c/skip_me/hidden");
        tree.touch("c/skip_me/hidden/marker");
        let leaf3 = tree.mkdir("c/visible");
        tree.touch("c/visible/marker");

        let results = DirWalker::new(tree.root(), skip_classify).walk().await.unwrap();
        assert_eq!(results, vec![leaf1, leaf2, leaf3]);
    }
}
