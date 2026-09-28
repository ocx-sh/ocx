// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Generated shim directories: a deferred tool on `PATH` without its content materialized.
//!
//! ```text
//! bin/            — the generated launchers, one per claimed name
//! digest          — full digest string for recovery
//! refs/blobs/     — forward-refs keeping the closure's config blobs reachable
//! ```

use std::path::{Path, PathBuf};

use ocx_util::fs::{DirWalker, WalkDecision};

type Result<T> = std::result::Result<T, ocx_util::error::FileError>;

/// One shim directory: `bin/`, `digest` and `refs/blobs/`.
#[derive(Debug, Clone)]
pub struct ShimDir {
    pub dir: PathBuf,
}

impl ShimDir {
    /// Its existence alone means complete: the tree is published by atomic rename.
    pub fn root(&self) -> &Path {
        &self.dir
    }

    /// The directory pushed onto `PATH`, not [`root`](Self::root).
    pub fn bin(&self) -> PathBuf {
        self.dir.join(SHIM_BIN_DIRNAME)
    }

    pub fn digest_file(&self) -> PathBuf {
        self.dir.join(super::cas_path::DIGEST_FILENAME)
    }

    /// These links alone keep the closure's config blobs, which carry the env, from GC.
    pub fn refs_blobs_dir(&self) -> PathBuf {
        self.dir.join("refs").join("blobs")
    }
}

// Launchers nest here, never at the root: a legal claim of `digest` or `refs` would overwrite the CAS marker and GC would collect live blobs.
// Also the walk's shim-dir marker, so producer and walker key on one fact.
const SHIM_BIN_DIRNAME: &str = "bin";

/// Shim directories at `{root}/{registry_slug}/{repository}/{cas_shard_path}/`.
#[derive(Debug, Clone)]
pub struct ShimStore {
    root: PathBuf,
}

impl ShimStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Keyed by repository too: a launcher names its repository, so two repositories on one digest must not share a tree.
    pub fn path(&self, identifier: &ocx_oci::PinnedPackageRef) -> PathBuf {
        self.root
            .join(super::slugify(identifier.registry()))
            .join(super::repository_path(identifier.repository()))
            .join(super::cas_path::cas_shard_path(&identifier.digest()))
    }

    pub fn shim_dir(&self, identifier: &ocx_oci::PinnedPackageRef) -> ShimDir {
        ShimDir {
            dir: self.path(identifier),
        }
    }

    /// Lists every shim directory; empty if the root does not exist.
    ///
    /// # Errors
    ///
    /// The store tree cannot be read.
    pub async fn list_all(&self) -> Result<Vec<ShimDir>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        // No `max_depth`: repositories vary in depth, and GC deletes any live shim this fails to report.
        DirWalker::new(self.root.clone(), classify_shim_dir).walk().await
    }
}

fn classify_shim_dir(dir: &Path, _depth: usize) -> WalkDecision<ShimDir> {
    // Never prune by name: `org/bin` is a legal repository, and a pruned subtree's live shims are collected.
    if !dir.join(SHIM_BIN_DIRNAME).is_dir() {
        return WalkDecision::descend();
    }
    if super::cas_path::is_valid_cas_path(dir) {
        return WalkDecision::leaf(ShimDir { dir: dir.to_path_buf() });
    }
    log::debug!("Not a shim dir despite a bin/ child, descending: {}", dir.display());
    WalkDecision::descend()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An arbitrary valid SHA-256 hex — the same one the sibling stores' layout
    /// goldens use.
    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
    /// The two shard segments `cas_shard_path` derives from [`SHA256_HEX`],
    /// spelled out by hand: a golden that calls the sharding function to build
    /// its own expected value asserts nothing about it.
    const SHARD_PREFIX: &str = "43";
    const SHARD_SUFFIX: &str = "567c07f1a6b07b5e8dc052108c9d4c";

    fn digest() -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256(SHA256_HEX.to_string())
    }

    fn pinned(registry: &str, repository: &str) -> ocx_oci::PinnedPackageRef {
        let identifier = ocx_oci::PackageRef::new_registry(repository, registry).clone_with_digest(digest());
        ocx_oci::PinnedPackageRef::try_from(identifier).expect("a digest-bearing identifier is pinned")
    }

    /// The C-003 layout written out by hand:
    /// `<root>/<registry-slug>/<repo segments…>/<algo>/<2hex>/<30hex>`.
    ///
    /// Every expected value and every on-disk fixture below is built from this
    /// rather than from [`ShimStore::path`], so a wrong `path()` cannot agree
    /// with itself.
    fn layout(root: &Path, registry_slug: &str, repository_segments: &[&str]) -> PathBuf {
        let mut dir = root.join(registry_slug);
        for segment in repository_segments {
            dir = dir.join(segment);
        }
        dir.join("sha256").join(SHARD_PREFIX).join(SHARD_SUFFIX)
    }

    /// Materializes a published shim directory — `bin/`, `digest` and
    /// `refs/blobs/` (C-003) — at the layout above, and returns its root.
    fn publish(root: &Path, registry_slug: &str, repository_segments: &[&str]) -> PathBuf {
        let dir = layout(root, registry_slug, repository_segments);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("refs").join("blobs")).unwrap();
        std::fs::write(dir.join("digest"), format!("sha256:{SHA256_HEX}")).unwrap();
        dir
    }

    /// Sorted roots of a `list_all` result, read from the public `dir` field so
    /// these assertions do not also depend on [`ShimDir::root`].
    fn roots_of(shims: &[ShimDir]) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = shims.iter().map(|shim| shim.dir.clone()).collect();
        found.sort();
        found
    }

    // ── C-003: the layout `path()` produces ───────────────────────────────
    //
    // This is the store's addressing contract: the generation task, the
    // composer's PATH entry and the GC walk all name a shim tree by it.

    #[test]
    fn path_is_registry_slug_then_repository_then_cas_shard() {
        let store = ShimStore::new("/ocx/shims");

        assert_eq!(
            store.path(&pinned("example.com", "cmake")),
            layout(Path::new("/ocx/shims"), "example.com", &["cmake"]),
            "C-003: `path()` is `shims/<registry-slug>/<repo-path>/<algo>/<2hex>/<30hex>/`"
        );
    }

    #[test]
    fn path_slugifies_a_port_bearing_registry() {
        let store = ShimStore::new("/ocx/shims");

        assert_eq!(
            store.path(&pinned("localhost:5000", "cmake")),
            layout(Path::new("/ocx/shims"), "localhost_5000", &["cmake"]),
            "C-003: the registry component is slugified, as in every sibling store"
        );
    }

    #[test]
    fn path_includes_the_repository_unlike_package_store() {
        let store = ShimStore::new("/ocx/shims");
        let cmake = pinned("example.com", "cmake");
        let ninja = pinned("example.com", "ninja");

        assert_eq!(
            super::super::PackageStore::new("/ocx/packages").path(&cmake),
            super::super::PackageStore::new("/ocx/packages").path(&ninja),
            "precondition: the two identifiers differ ONLY in repository — \
             `PackageStore` keys on registry + digest and collapses them"
        );
        assert_ne!(
            store.path(&cmake),
            store.path(&ninja),
            "C-003: unlike `PackageStore`, the repository IS in the shim path, \
             so two repositories resolving to one digest do not share a tree"
        );
    }

    /// C-003: the repo component is built with `repository_path()`, never a
    /// literal `/` join.
    ///
    /// The equality below pins the *layout* — a `path()` that drops, reorders
    /// or collapses repository segments fails it on every host. It does not
    /// pin the *construction*: on Unix `join("org/project")` and
    /// `join("org").join("project")` produce equal paths, and `components()`
    /// splits on `/` on Windows too, so the only observable difference is the
    /// rendered separator. The `cfg(windows)` assertion is where that clause
    /// actually has teeth.
    #[test]
    fn path_splits_a_nested_repository_into_separate_components() {
        let root = Path::new("/ocx/shims");
        let store = ShimStore::new(root);
        let path = store.path(&pinned("example.com", "org/project/sub/tool"));

        assert_eq!(
            path,
            layout(root, "example.com", &["org", "project", "sub", "tool"]),
            "C-003: every repository segment is its own path component, in order"
        );

        // Only the part `path()` BUILDS may be inspected for separators: the
        // POSIX-shaped store root is this test's own fixture, and on Windows it
        // renders its own `/` no matter how the segments below it were joined —
        // asserting over the whole path would fail on the root and say nothing
        // about the repository.
        #[cfg(windows)]
        {
            let built = path
                .strip_prefix(root)
                .expect("`path()` is rooted at the store root it was constructed with");
            assert!(
                !built.to_string_lossy().contains('/'),
                "C-003: a literal `/` join leaves mixed separators on Windows: {}",
                built.display()
            );
        }
    }

    #[test]
    fn shim_dir_is_anchored_at_the_identifier_path() {
        let store = ShimStore::new("/ocx/shims");
        let identifier = pinned("example.com", "cmake");

        assert_eq!(
            store.shim_dir(&identifier).dir,
            store.path(&identifier),
            "C-003: `shim_dir()` anchors a `ShimDir` at `path(identifier)` — the \
             grep-able construction, mirroring `PackageStore::package_dir`"
        );
    }

    // ── C-003 / C-004: the `PackageDir` shape inside a shim directory ─────

    #[test]
    fn shim_dir_children_are_bin_digest_and_refs_blobs() {
        let dir = PathBuf::from("/ocx/shims/example.com/cmake/sha256/43/rest");
        let shim = ShimDir { dir: dir.clone() };

        assert_eq!(
            shim.root(),
            dir.as_path(),
            "C-003: `root()` is the shim directory itself"
        );
        assert_eq!(
            shim.bin(),
            dir.join("bin"),
            "C-003: launchers live in a `bin/` subdirectory of the shim root"
        );
        assert_eq!(
            shim.digest_file(),
            dir.join("digest"),
            "C-003: `digest` is a sibling of `bin/`, as in `PackageDir`"
        );
        assert_eq!(
            shim.refs_blobs_dir(),
            dir.join("refs").join("blobs"),
            "C-004: `refs_blobs_dir()` is `refs/blobs/`, as in `PackageDir`"
        );
    }

    #[test]
    fn bin_subdirectory_isolates_launchers_from_the_digest_and_refs_siblings() {
        let shim = ShimDir {
            dir: PathBuf::from("/ocx/shims/example.com/cmake/sha256/43/rest"),
        };

        // `binaries = ["digest", "refs"]` passes `BinaryName::try_from`, so
        // these are launcher names a publisher can legally claim. Flat at the
        // shim root they would overwrite the CAS marker and the refs directory;
        // GC then mis-classifies liveness and collects config blobs a live shim
        // needs. One path segment closes every present and future sibling name.
        for claimed in ["digest", "refs"] {
            assert_ne!(
                shim.bin().join(claimed),
                shim.root().join(claimed),
                "C-003: a launcher named `{claimed}` must not land at the shim root"
            );
        }
        assert_ne!(
            shim.bin().join("digest"),
            shim.digest_file(),
            "C-003: a launcher named `digest` must not overwrite the CAS marker"
        );
        assert_ne!(
            shim.bin().join("refs"),
            shim.refs_blobs_dir(),
            "C-003: a launcher named `refs` must not collide with the refs tree"
        );
    }

    #[test]
    fn file_structure_roots_the_store_at_ocx_home_shims() {
        let home = Path::new("/ocx-home");

        assert_eq!(
            super::super::FileStructure::with_root(home.to_path_buf()).shims.root(),
            home.join("shims"),
            "C-003: the store root is `$OCX_HOME/shims`"
        );
    }

    // ── C-004: `list_all()` ───────────────────────────────────────────────
    //
    // GC (C-014) collects precisely what `list_all` fails to report, so an
    // under-report here is deletion of a live shim. The depth cases below are
    // the guard against that whole bug class: repository segment count is
    // variable and unbounded, so any `max_depth` bound silently loses the
    // deeper shims.

    #[tokio::test]
    async fn list_all_returns_empty_when_the_root_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ShimStore::new(tmp.path().join("never-created"));

        assert!(
            store.list_all().await.unwrap().is_empty(),
            "C-004: an absent store root lists nothing and is not an error"
        );
    }

    #[tokio::test]
    async fn list_all_finds_a_shim_at_a_single_segment_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let published = publish(tmp.path(), "example.com", &["cmake"]);
        let store = ShimStore::new(tmp.path());

        let shims = store.list_all().await.unwrap();

        assert_eq!(
            roots_of(&shims),
            vec![published.clone()],
            "C-004: the one shim is reported"
        );
        assert_eq!(
            shims[0].bin(),
            published.join("bin"),
            "C-004: a reported `ShimDir` reaches its own launchers"
        );
        assert_eq!(
            shims[0].refs_blobs_dir(),
            published.join("refs").join("blobs"),
            "C-004: and its own blob forward-refs, which is what GC follows"
        );
    }

    /// C-004 (walker-depth decision 2026-08-10): **no `max_depth` bound**. A
    /// bound copied from `package_store.rs` (`1 + CAS_SHARD_DEPTH`) reaches a
    /// one-segment repository and stops one level short of every deeper one —
    /// and because C-014 collects what `list_all` omits, that is silent
    /// deletion of a live shim, reproducible only for users whose repository
    /// names happen to be deep. This test is that bug class's guard.
    #[tokio::test]
    async fn list_all_finds_a_shim_at_a_deep_repository_path() {
        let tmp = tempfile::tempdir().unwrap();
        let published = publish(tmp.path(), "example.com", &["org", "project", "sub", "tool"]);
        let store = ShimStore::new(tmp.path());

        let shims = store.list_all().await.unwrap();

        assert_eq!(
            roots_of(&shims),
            vec![published.clone()],
            "C-004: a shim under a four-segment repository is reported — no \
             depth bound may hide it from GC"
        );
        assert_eq!(
            shims[0].refs_blobs_dir(),
            published.join("refs").join("blobs"),
            "C-004: the deep shim's forward-refs are reachable from the result"
        );
    }

    #[tokio::test]
    async fn list_all_finds_shims_at_mixed_repository_depths() {
        let tmp = tempfile::tempdir().unwrap();
        let shallow = publish(tmp.path(), "example.com", &["cmake"]);
        let nested = publish(tmp.path(), "example.com", &["org", "ninja"]);
        let deep = publish(tmp.path(), "ghcr.io", &["org", "project", "sub", "tool"]);
        let store = ShimStore::new(tmp.path());

        let mut expected = vec![shallow, nested, deep];
        expected.sort();

        assert_eq!(
            roots_of(&store.list_all().await.unwrap()),
            expected,
            "C-004: one store holds repositories of every depth at once — a \
             single bound cannot be right for all of them, so there is none"
        );
    }

    /// C-004, added at Implement: `refs` is a legal OCI repository segment, and
    /// this store puts the repository in the path. A name-based skip list
    /// copied from `PackageStore::list_all` prunes the segment and hides every
    /// shim below it from GC — the same silent-deletion class as a depth bound,
    /// reproducible only for users who name a repository that way.
    #[tokio::test]
    async fn list_all_finds_a_shim_under_a_repository_segment_named_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let published = publish(tmp.path(), "example.com", &["org", "refs"]);
        let store = ShimStore::new(tmp.path());

        assert_eq!(
            roots_of(&store.list_all().await.unwrap()),
            vec![published],
            "C-004: no child name may be pruned by name — the repository is in \
             the path, so `org/refs` is a repository, not shim internals"
        );
    }

    /// C-004, added at Implement: the sibling of the test above for the other
    /// branch. `org/bin` gives the *intermediate* directory `org` a `bin/`
    /// child, so `org` looks like a shim dir until the CAS check rejects it.
    /// Pruning there discards the whole repository subtree.
    #[tokio::test]
    async fn list_all_finds_a_shim_under_a_repository_segment_named_bin() {
        let tmp = tempfile::tempdir().unwrap();
        let published = publish(tmp.path(), "example.com", &["org", "bin"]);
        let store = ShimStore::new(tmp.path());

        assert_eq!(
            roots_of(&store.list_all().await.unwrap()),
            vec![published],
            "C-004: a `bin/` child with an invalid CAS tail is not a shim dir — \
             the walk descends past it instead of pruning a live shim"
        );
    }

    #[tokio::test]
    async fn list_all_does_not_recurse_into_a_published_shim() {
        let tmp = tempfile::tempdir().unwrap();
        let published = publish(tmp.path(), "example.com", &["cmake"]);
        // A shim's own children are generated content, not more store. Without
        // `WalkDecision::leaf` an unbounded walk keeps descending and reports
        // these decoys as shims of their own.
        std::fs::create_dir_all(published.join("bin").join("decoy").join("bin")).unwrap();
        std::fs::create_dir_all(published.join("refs").join("blobs").join("decoy").join("bin")).unwrap();
        let store = ShimStore::new(tmp.path());

        assert_eq!(
            roots_of(&store.list_all().await.unwrap()),
            vec![published],
            "C-004: recursion stops at each shim dir — the walk must not \
             descend into `bin/` or `refs/`"
        );
    }
}
