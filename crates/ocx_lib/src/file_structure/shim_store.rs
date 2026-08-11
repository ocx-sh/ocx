// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Identity-keyed store for generated shim directories — the on-disk form of a
//! **deferred** tool (plan contracts C-003 / C-004,
//! [#302](https://github.com/ocx-sh/ocx/issues/302)).
//!
//! A shim directory exists precisely when a tool is composed onto `PATH`
//! without its content being materialized. It holds one generated launcher per
//! declared interface name; invoking any of them triggers the ordinary pull and
//! execs the real target.
//!
//! Layout: `{root}/<registry-slug>/<repo-path>/<algo>/<2hex>/<30hex>/`, then
//! the [`PackageDir`](super::PackageDir) shape inside it:
//!
//! ```text
//! bin/            — the generated launchers, one per claimed name
//! digest          — full digest string for recovery
//! refs/blobs/     — forward-refs keeping the closure's config blobs reachable
//! ```
//!
//! Two ways this store differs from its neighbours, both load-bearing:
//!
//! - **The repository IS in the path**, unlike [`PackageStore`](super::PackageStore),
//!   which keys on registry + digest alone for cross-repo dedup. A shim body
//!   names a *pinned identifier*, and that identifier carries the repository,
//!   so two repositories resolving to one digest do not share a shim tree.
//!   The repo component is built with [`super::repository_path`], never a
//!   literal `/` join — the latter produces mixed separators on Windows.
//! - **Launchers nest under `bin/`, never at the directory root.** A legal
//!   `binaries` claim of `["digest", "refs"]` passes
//!   [`BinaryName::try_from`](crate::package::metadata::BinaryName), so flat
//!   launchers would overwrite the CAS marker and the refs directory — GC then
//!   mis-classifies liveness and collects config blobs a live shim needs. One
//!   path segment closes every present and future sibling name without a second
//!   validator.
//!
//! GC liveness is rooted **directly in the lock pins**, not reachable from a
//! package: a shim dir exists exactly when the package dir does not, so there is
//! no package to carry an edge to it (plan contract C-014, WP-9).

use std::path::{Path, PathBuf};

use crate::utility::fs::{DirWalker, WalkDecision};
use crate::{Result, log, oci};

/// A single shim directory — the [`PackageDir`](super::PackageDir) shape,
/// minus everything a materialized package has and a deferred tool does not.
///
/// See the module docs for the layout and for why launchers live under `bin/`.
#[derive(Debug, Clone)]
pub struct ShimDir {
    /// The root directory of this shim (parent of `bin/`, `digest`, `refs/`).
    pub dir: PathBuf,
}

impl ShimDir {
    /// Root directory of the shim — parent of `bin/`, `digest` and `refs/`.
    ///
    /// Its mere existence is the completeness signal: the generation task
    /// publishes the whole tree by atomic rename, so a consumer needs no
    /// further probe (plan contract C-020).
    pub fn root(&self) -> &Path {
        &self.dir
    }

    /// Path to the generated launchers directory.
    ///
    /// This — not [`root`](Self::root) — is the directory the composer pushes
    /// onto `PATH` for a deferred tool (plan contract C-012).
    pub fn bin(&self) -> PathBuf {
        self.dir.join(SHIM_BIN_DIRNAME)
    }

    /// Path to the digest marker file, carrying the full digest string the
    /// truncated CAS path cannot recover.
    pub fn digest_file(&self) -> PathBuf {
        self.dir.join(super::cas_path::DIGEST_FILENAME)
    }

    /// Path to the blob forward-reference directory.
    ///
    /// A deferred tool's env carriers are read from the closure's ref-linked
    /// config blobs rather than from a package directory, so these links are
    /// what keep those blobs off the GC's unreachable set.
    pub fn refs_blobs_dir(&self) -> PathBuf {
        self.dir.join("refs").join("blobs")
    }
}

/// Name of the launchers directory inside a shim dir.
///
/// Doubles as the walk's shim-dir marker (see [`classify_shim_dir`]) and as
/// C-022's completeness marker, so the producer and the walker key on one fact.
const SHIM_BIN_DIRNAME: &str = "bin";

/// Manages the identity-keyed shim store on the local filesystem.
///
/// See the module docs for the layout and for the two ways it diverges from
/// [`PackageStore`](super::PackageStore).
#[derive(Debug, Clone)]
pub struct ShimStore {
    root: PathBuf,
}

impl ShimStore {
    /// Creates a `ShimStore` rooted at `root` (conventionally
    /// `$OCX_HOME/shims`, wired by [`super::FileStructure::with_root`]).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the shim store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the shim directory path for the given pinned identifier.
    ///
    /// Unlike [`PackageStore::path`](super::PackageStore::path), the
    /// **repository is part of the path** — see the module docs.
    pub fn path(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.root
            .join(super::slugify(identifier.registry()))
            .join(super::repository_path(identifier.repository()))
            .join(super::cas_path::cas_shard_path(&identifier.digest()))
    }

    /// Returns a [`ShimDir`] anchored at this identifier's shim root.
    ///
    /// Equivalent to `ShimDir { dir: self.path(identifier) }` — prefer this
    /// over hand-rolled construction so call sites stay grep-able.
    pub fn shim_dir(&self, identifier: &oci::PinnedIdentifier) -> ShimDir {
        ShimDir {
            dir: self.path(identifier),
        }
    }

    /// Lists all shim directories currently present in the store.
    ///
    /// A shim directory is identified by the presence of a `bin/` child whose
    /// directory tail is a valid CAS path. Recursion stops there, so the
    /// generated launchers and forward-refs inside a shim are never traversed.
    ///
    /// The walk is **unbounded in depth**: the repository sits in the path and
    /// its segment count is variable, so any fixed bound loses the shims below
    /// it — and since GC collects whatever this fails to report (plan contract
    /// C-014), losing one is deleting a live tool.
    ///
    /// Returns an empty vec if the store root does not exist yet.
    ///
    /// # Errors
    ///
    /// Returns an error if the store tree cannot be read.
    pub async fn list_all(&self) -> Result<Vec<ShimDir>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        DirWalker::new(self.root.clone(), classify_shim_dir).walk().await
    }
}

/// Classifies a directory for the generic walker.
///
/// - `bin/` child and a valid CAS tail → [`WalkDecision::leaf`] with a [`ShimDir`].
/// - anything else → [`WalkDecision::descend`].
///
/// Unlike [`PackageStore::list_all`](super::PackageStore::list_all) this prunes
/// nothing but a recognized shim: `org/bin` and `org/refs` are legal OCI
/// repositories, and the repository is in this store's path, so a name-based
/// skip list would prune a live shim's whole subtree out of GC's view.
/// Descending past an unrecognized directory can at worst over-report, which
/// under C-014 retains a dead shim — the harmless direction.
fn classify_shim_dir(dir: &Path, _depth: usize) -> WalkDecision<ShimDir> {
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

    use crate::oci;

    /// An arbitrary valid SHA-256 hex — the same one the sibling stores' layout
    /// goldens use.
    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
    /// The two shard segments `cas_shard_path` derives from [`SHA256_HEX`],
    /// spelled out by hand: a golden that calls the sharding function to build
    /// its own expected value asserts nothing about it.
    const SHARD_PREFIX: &str = "43";
    const SHARD_SUFFIX: &str = "567c07f1a6b07b5e8dc052108c9d4c";

    fn digest() -> oci::Digest {
        oci::Digest::Sha256(SHA256_HEX.to_string())
    }

    fn pinned(registry: &str, repository: &str) -> oci::PinnedIdentifier {
        let identifier = oci::Identifier::new_registry(repository, registry).clone_with_digest(digest());
        oci::PinnedIdentifier::try_from(identifier).expect("a digest-bearing identifier is pinned")
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
    /// actually has teeth; it cannot be exercised on this suite's hosts.
    #[test]
    fn path_splits_a_nested_repository_into_separate_components() {
        let store = ShimStore::new("/ocx/shims");
        let path = store.path(&pinned("example.com", "org/project/sub/tool"));

        assert_eq!(
            path,
            layout(
                Path::new("/ocx/shims"),
                "example.com",
                &["org", "project", "sub", "tool"]
            ),
            "C-003: every repository segment is its own path component, in order"
        );

        #[cfg(windows)]
        assert!(
            !path.to_string_lossy().contains('/'),
            "C-003: a literal `/` join leaves mixed separators on Windows: {}",
            path.display()
        );
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
