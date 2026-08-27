// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use crate::reference_manager::ReferenceManager;
use crate::{Error, Result, log, oci};

/// Represents a single content-addressed package directory within the package store.
///
/// A package directory has a fixed layout:
/// - `content/`       -- the installed package files (directory tree)
/// - `metadata.json`  -- package metadata
/// - `manifest.json`  -- OCI manifest
/// - `resolve.json`   -- resolved dependency graph
/// - `install.json`   -- install status
/// - `digest`         -- full digest string for recovery
/// - `refs/symlinks/` -- back-reference symlinks for install tracking
/// - `refs/deps/`     -- back-reference symlinks for dependency tracking
/// - `refs/layers/`   -- back-reference symlinks for layer tracking
/// - `refs/blobs/`    -- back-reference symlinks for blob tracking
/// - `refs/origins/`  -- one marker file per logical repository this digest was fetched under
#[derive(Debug, Clone)]
pub struct PackageDir {
    /// The root directory of this package (parent of `content/`, `metadata.json`, etc.).
    pub dir: PathBuf,
}

impl PackageDir {
    /// Construct a [`PackageDir`] rooted at an arbitrary `path`.
    ///
    /// Used by `pull_local` and `package test` to anchor the install pipeline
    /// at a caller-supplied destination (e.g., a `tempfile::TempDir`) rather
    /// than the content-addressed object store location.
    pub fn with_root(path: PathBuf) -> Self {
        Self { dir: path }
    }

    /// Root directory of the package — parent of `content/`, `entrypoints/`,
    /// `metadata.json`, `refs/`, and the other per-package files.
    ///
    /// CLI commands surfacing a package's location to users return this path
    /// so consumers can traverse into either `content/` or `entrypoints/`.
    pub fn root(&self) -> &Path {
        &self.dir
    }

    /// Path to the package content directory.
    pub fn content(&self) -> PathBuf {
        self.dir.join("content")
    }

    /// Path to the package metadata file.
    pub fn metadata(&self) -> PathBuf {
        self.dir.join("metadata.json")
    }

    /// Path to the OCI manifest file.
    pub fn manifest(&self) -> PathBuf {
        self.dir.join("manifest.json")
    }

    /// Path to the resolved dependency graph file.
    pub fn resolve(&self) -> PathBuf {
        self.dir.join("resolve.json")
    }

    /// Path to the install status file.
    pub fn install_status(&self) -> PathBuf {
        self.dir.join("install.json")
    }

    /// Path to the digest marker file.
    pub fn digest_file(&self) -> PathBuf {
        self.dir.join(super::cas_path::DIGEST_FILENAME)
    }

    /// Path to the symlink back-reference directory.
    pub fn refs_symlinks_dir(&self) -> PathBuf {
        self.dir.join("refs").join("symlinks")
    }

    /// Path to the dependency back-reference directory.
    pub fn refs_deps_dir(&self) -> PathBuf {
        self.dir.join("refs").join("deps")
    }

    /// Path to the layer back-reference directory.
    pub fn refs_layers_dir(&self) -> PathBuf {
        self.dir.join("refs").join("layers")
    }

    /// Path to the blob back-reference directory.
    pub fn refs_blobs_dir(&self) -> PathBuf {
        self.dir.join("refs").join("blobs")
    }

    /// Path to the pulling-origin marker directory.
    ///
    /// Holds one file per distinct **logical** repository this host resolved
    /// and materialized digest-verified content for — see [`record_origin`]
    /// for the write contract (including why the coordinate is the logical one)
    /// and [`PackageDir::recorded_origins`] for the read side. Unlike its four
    /// `refs/` siblings these are regular files rather than symlinks, and they
    /// are not part of the GC reachability graph: they record provenance, not
    /// liveness.
    pub fn refs_origins_dir(&self) -> PathBuf {
        self.dir.join("refs").join("origins")
    }

    /// The logical repositories this host has recorded resolving and
    /// materializing this package's digest under, as canonical
    /// `<registry>/<repository-path>` strings.
    ///
    /// Empty when nothing was recorded — an absent directory, an unreadable
    /// one, and a package materialized before origins were tracked are all the
    /// same answer, because none of them is evidence of a repository. Callers
    /// that use this as authorization evidence must treat the empty answer as a
    /// refusal, never as "unconstrained".
    ///
    /// A marker whose content does not hash back to its own file name is
    /// discarded: the file name is [`ReferenceManager::name_for_path`] of the
    /// content, so a torn or clobbered marker is detectable without a second
    /// integrity file.
    ///
    /// Sorted and deduplicated, so the answer does not depend on directory
    /// order. Blocking: one `read_dir` plus one small read per entry.
    #[must_use]
    pub fn recorded_origins(&self) -> Vec<String> {
        let dir = self.refs_origins_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                // Absence is the ordinary state for every package pulled before
                // this marker existed, and for every off-store `PackageDir`, so
                // even a genuine read failure stays at debug: the outcome is
                // identical and a WARN here would fire on the common case.
                log::debug!("No recorded pull origins at '{}': {e}", dir.display());
                return Vec::new();
            }
        };
        let mut origins: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(content) = std::fs::read_to_string(&path) else {
                log::debug!("Skipping unreadable origin marker '{}'", path.display());
                continue;
            };
            let origin = content.trim().to_string();
            if origin_marker_name(&origin) != entry.file_name().to_string_lossy() {
                log::debug!(
                    "Skipping origin marker '{}': its content does not hash to its own name",
                    path.display()
                );
                continue;
            }
            origins.push(origin);
        }
        origins.sort();
        origins.dedup();
        origins
    }

    /// Path to the generated launchers directory.
    ///
    /// `entrypoints/` is a sibling of `content/` and `refs/` under the package root.
    /// Launcher files are regular files generated at install time, not content-addressed.
    pub fn entrypoints(&self) -> PathBuf {
        self.dir.join("entrypoints")
    }
}

/// Manages the content-addressed package store on the local filesystem.
///
/// All packages are stored under a single `root` directory, sharded by
/// registry and digest (via [`super::cas_path::cas_shard_path`]).
///
/// **Repository is NOT part of the path.** Only registry + digest determine
/// the filesystem location. This enables content deduplication across
/// repositories.
///
/// Layout:
/// ```text
/// {root}/
///   {registry_slug}/
///     {algorithm}/             e.g. sha256
///       {2hex}/                first 2 hex chars of digest
///         {30hex}/             next 30 hex chars
///           content/
///           metadata.json
///           manifest.json
///           resolve.json
///           install.json
///           digest
///           refs/
///             symlinks/
///             deps/
///             layers/
///             blobs/
/// ```
#[derive(Debug, Clone)]
pub struct PackageStore {
    root: PathBuf,
}

impl PackageStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the package store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the package directory path for the given identifier.
    ///
    /// **Only uses registry + digest from the identifier.** The repository
    /// is intentionally ignored for content deduplication.
    pub fn path(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.root
            .join(super::slugify(identifier.registry()))
            .join(super::cas_path::cas_shard_path(&identifier.digest()))
    }

    /// Returns a [`PackageDir`] anchored at this identifier's package root.
    ///
    /// Equivalent to `PackageDir { dir: self.path(identifier) }` — prefer this
    /// over hand-rolled construction so call sites stay grep-able.
    pub fn package_dir(&self, identifier: &oci::PinnedIdentifier) -> PackageDir {
        PackageDir {
            dir: self.path(identifier),
        }
    }

    /// Returns the `content/` path for the given identifier.
    pub fn content(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("content")
    }

    /// Returns the `metadata.json` path for the given identifier.
    pub fn metadata(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("metadata.json")
    }

    /// Returns the `manifest.json` path for the given identifier.
    pub fn manifest(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("manifest.json")
    }

    /// Returns the `resolve.json` path for the given identifier.
    pub fn resolve(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("resolve.json")
    }

    /// Returns the `install.json` path for the given identifier.
    pub fn install_status(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("install.json")
    }

    /// Returns the `digest` file path for the given identifier.
    pub fn digest_file(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join(super::cas_path::DIGEST_FILENAME)
    }

    /// Returns the `metadata.json` path for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling file.
    pub fn metadata_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("metadata.json"))
    }

    /// Returns the `refs/symlinks/` directory for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling directory.
    pub fn refs_symlinks_dir_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("refs").join("symlinks"))
    }

    /// Returns the `refs/deps/` directory for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling directory.
    pub fn refs_deps_dir_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("refs").join("deps"))
    }

    /// Returns the `refs/layers/` directory for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling directory.
    pub fn refs_layers_dir_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("refs").join("layers"))
    }

    /// Returns the `refs/blobs/` directory for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling directory.
    pub fn refs_blobs_dir_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("refs").join("blobs"))
    }

    /// Returns the `resolve.json` path for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling file.
    pub fn resolve_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("resolve.json"))
    }

    /// Returns the `entrypoints/` path for the given identifier.
    ///
    /// `entrypoints/` is a sibling of `content/` and `refs/` inside the package root.
    pub fn entrypoints(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("entrypoints")
    }

    /// Returns the `digest` file path for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling file.
    pub fn digest_file_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join(super::cas_path::DIGEST_FILENAME))
    }

    /// Lists all package directories currently present in the store.
    ///
    /// A package directory is identified by the presence of a `content/` child
    /// directory. Recursion stops at that point so that package-installed files
    /// (which may themselves contain arbitrary subdirectories) are never traversed.
    ///
    /// Returns an empty vec if the store root does not exist yet.
    pub async fn list_all(&self) -> Result<Vec<PackageDir>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        crate::utility::fs::DirWalker::new(self.root.clone(), classify_package_dir)
            .max_depth(MAX_WALK_DEPTH)
            .walk()
            .await
    }
}

/// Resolves `path` (following any install symlinks) to the package root
/// directory. Accepts either a `content/` child directory or the package root
/// itself, so callers don't need to know which they hold.
///
/// - When `path` resolves to `packages/.../<digest>/content`, returns
///   `packages/.../<digest>` (the parent — the package root).
/// - When `path` resolves to `packages/.../<digest>` (the package root), returns
///   it unchanged. This is the shape produced by the flattened install layout
///   where `symlinks/{registry}/{repo}/current` and
///   `symlinks/{registry}/{repo}/candidates/{tag}` target the package root
///   directly and consumers traverse into `content/` or `entrypoints/` as
///   needed.
fn package_dir_for_content(path: &Path) -> Result<PathBuf> {
    let canonical = dunce::canonicalize(path).map_err(|e| Error::InternalFile(path.to_path_buf(), e))?;
    if canonical.file_name() == Some(std::ffi::OsStr::new("content")) {
        canonical
            .parent()
            .map(|p| p.to_path_buf())
            .ok_or(Error::InternalPathInvalid(canonical))
    } else {
        Ok(canonical)
    }
}

/// The canonical origin string for `identifier`: `<registry>/<repository-path>`,
/// host lowercased, port preserved, repository path verbatim.
///
/// This is the *full* coordinate, not the consent source: truncating to the
/// org is the consent predicate's normalization
/// ([`crate::project::consent::source_of`]), and a store that recorded only the
/// truncated form could never answer a finer question later.
#[must_use]
fn origin_of(identifier: &oci::Identifier) -> String {
    format!(
        "{}/{}",
        identifier.registry().to_ascii_lowercase(),
        identifier.repository()
    )
}

/// The marker file name for `origin`.
///
/// [`ReferenceManager::name_for_path`] is this codebase's one "name a byte
/// string by its hash" helper — the install back-refs and the project ledger
/// both key on it — and `Path` is a zero-cost wrapper over the same bytes, so
/// this hashes the origin string verbatim rather than introducing a second
/// scheme.
///
/// The name is a 64-bit truncated SHA-256, so a collision is conceivable. It
/// cannot forge an origin: the marker's *content* is the truth and is written
/// only by [`record_origin`], so a collision can at worst suppress recording a
/// second repository for a digest the first one already recorded — and those
/// two repositories resolve to byte-identical content by construction.
fn origin_marker_name(origin: &str) -> String {
    ReferenceManager::name_for_path(Path::new(origin))
}

/// Record that this host resolved `identifier`'s **logical** repository and
/// materialized digest-verified content for `pkg` under it.
///
/// # This is authorization evidence — call it only from a materializing pull
///
/// The package store is addressed by **(registry, digest) only**, so this
/// directory is the sole place the store records *which repository* content
/// arrived under. The shell-activation consent predicate quantifies clause 2
/// over exactly this record
/// ([`crate::project::consent::verified_sources`]), so what a marker attests
/// bounds what that clause can grant.
///
/// State the bound at the strength the gate actually enforces. A marker is
/// evidence that **this host** ran a fetching pull — anything but
/// `pull_local` — which materialized digest-verified content and bound it to
/// the logical repository the identifier spelled. It is **not** evidence that
/// a registry vouched for that binding: the call site sits past two store-hit
/// fast paths that are each conditional on install status, so an absent or
/// not-OK package directory falls through into the fetching branch, and from
/// there the layer cache can satisfy every layer with no client and no wire.
/// A pull naming any logical repository, on a registry whose layers for that
/// digest are already cached, therefore mints that repository's marker
/// offline.
///
/// So clause 2's floor is *"some local actor pulled under this name"*, not
/// *"a registry served under this name"*. That is still strictly stronger
/// than the claim-based spelling it replaced — lock text is written by the
/// clone's author, whereas a marker takes an act of pulling on this host —
/// but the stronger wording belongs here only once the write gate observes
/// wire contact. Tracked as
/// <https://github.com/ocx-sh/ocx/issues/348>.
///
/// # Logical, not transport
///
/// `identifier` is the coordinate the caller resolved, **not** the address the
/// bytes travelled over: an operator's `[mirrors]` entry or an index
/// indirection can redirect the fetch to a different endpoint or repository,
/// and the marker still records the logical one. That is deliberate and
/// matches [`crate::project::consent::source_of`] — consent has one identity,
/// and pinning it to routing is the failure `adr_lock_records_physical_address.md`
/// was rejected for. Both redirects are operator-configured (`config.toml`
/// tiers only; a project's `ocx.toml` reaches neither), and the content is
/// digest-verified whichever endpoint serves it, so a redirect cannot
/// substitute different bytes. What the marker consequently does not answer is
/// *who published* them — that is `[[trust.policy]]` plus signature
/// verification, the same residual consent carries generally.
///
/// It therefore MUST NOT be called from a local-store hit
/// (`tasks::common::find_in_store`, `find_or_install`) or from `pull_local`
/// (a local tarball, whose repository is author-supplied text no registry ever
/// vouched for). Composing a namespace-granted project reaches those paths, so
/// a marker written there would let a project author self-authorize by naming
/// a repository and having the name recorded as if it had been served.
///
/// Idempotent and race-tolerant: one file per distinct origin, written only
/// when absent, so concurrent pulls of different repositories at one digest do
/// not contend. Call it against the staging [`PackageDir`] before the atomic
/// temp→store move, so the marker is published by that same rename and a
/// crash mid-write leaves a discarded temp tree rather than a torn marker.
///
/// # Errors
///
/// Propagates the directory-creation or file-write failure.
pub async fn record_origin(pkg: &PackageDir, identifier: &oci::Identifier) -> Result<()> {
    let origin = origin_of(identifier);
    let dir = pkg.refs_origins_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| Error::InternalFile(dir.clone(), e))?;

    let marker = dir.join(origin_marker_name(&origin));
    if tokio::fs::try_exists(&marker)
        .await
        .map_err(|e| Error::InternalFile(marker.clone(), e))?
    {
        return Ok(());
    }
    tokio::fs::write(&marker, origin.as_bytes())
        .await
        .map_err(|e| Error::InternalFile(marker, e))
}

/// Registry directory + CAS shard depth (algorithm/prefix/suffix).
const MAX_WALK_DEPTH: usize = 1 + super::cas_path::CAS_SHARD_DEPTH;

/// Directory names that are part of the package layout and must not be
/// recursed into during the store walk.
const PACKAGE_SKIP_NAMES: &[&str] = &["content", "refs"];

/// Classifies a directory for the generic walker.
///
/// - If a `content/` subdirectory exists and the path is valid CAS →
///   [`WalkDecision::leaf`] with a [`PackageDir`].
/// - If `content/` exists but the path is invalid → [`WalkDecision::skip`].
/// - Otherwise → [`WalkDecision::descend_skip`], skipping `content`, `refs`.
fn classify_package_dir(dir: &Path, _depth: usize) -> crate::utility::fs::WalkDecision<PackageDir> {
    if dir.join("content").is_dir() {
        if super::cas_path::is_valid_cas_path(dir) {
            return crate::utility::fs::WalkDecision::leaf(PackageDir { dir: dir.to_path_buf() });
        }
        log::warn!("Skipping content/ dir not matching CAS layout: {}", dir.display());
        return crate::utility::fs::WalkDecision::skip();
    }
    crate::utility::fs::WalkDecision::descend_skip(PACKAGE_SKIP_NAMES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;

    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn digest() -> oci::Digest {
        oci::Digest::Sha256(SHA256_HEX.to_string())
    }

    fn pinned(registry: &str, repository: &str) -> oci::PinnedIdentifier {
        let id = oci::Identifier::new_registry(repository, registry).clone_with_digest(digest());
        oci::PinnedIdentifier::try_from(id).unwrap()
    }

    // ---- path construction ------------------------------------------------

    #[test]
    fn path_uses_only_registry_and_digest_not_repository() {
        let store = PackageStore::new("/packages");
        let id_a = pinned("example.com", "cmake");
        let id_b = pinned("example.com", "ninja");
        // Different repos, same digest and registry -> same path
        assert_eq!(store.path(&id_a), store.path(&id_b));
    }

    /// The store's dedup is exactly what makes the origin markers necessary:
    /// two repositories serving one digest share **one** package directory, so
    /// nothing in the path records which of them the bytes arrived from. The
    /// `refs/origins/` directory is that record, and it keeps the two apart
    /// inside the shared directory.
    ///
    /// This is the property `project::consent::verified_sources` rests on, and
    /// it sits beside `path_uses_only_registry_and_digest_not_repository`
    /// deliberately: that test pins the deduplication, this one pins the
    /// provenance the deduplication erases.
    ///
    /// Red state: make `origin_marker_name` a constant, or write the marker
    /// with `create_dir_all` + a fixed name — the two origins collapse to one
    /// and the second assertion drops to a single-element vector.
    #[tokio::test]
    async fn origins_distinguish_two_repositories_sharing_one_package_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = PackageStore::new(dir.path());
        let cmake = pinned("example.com", "acme/cmake");
        let ninja = pinned("example.com", "evil/ninja");

        // The precondition, restated locally so this test fails loudly rather
        // than vacuously if the dedup ever changes.
        assert_eq!(
            store.path(&cmake),
            store.path(&ninja),
            "the two identifiers must share one package directory for this test to mean anything"
        );

        let pkg = store.package_dir(&cmake);
        assert!(
            pkg.recorded_origins().is_empty(),
            "a package directory with no markers records no origin - never a permissive default"
        );

        record_origin(&pkg, cmake.as_identifier()).await.unwrap();
        record_origin(&pkg, ninja.as_identifier()).await.unwrap();
        // Idempotent: a re-pull of an already-recorded repository adds nothing.
        record_origin(&pkg, cmake.as_identifier()).await.unwrap();

        assert_eq!(
            store.package_dir(&ninja).recorded_origins(),
            vec![
                "example.com/acme/cmake".to_string(),
                "example.com/evil/ninja".to_string()
            ],
            "both repositories recorded for this digest are kept, sorted and deduplicated, and \
             either identifier reads the same shared record"
        );
    }

    /// A marker whose content does not hash back to its own name is discarded:
    /// the name IS the integrity check, so a clobbered or truncated marker
    /// cannot silently rename an origin.
    #[tokio::test]
    async fn a_marker_whose_content_does_not_match_its_name_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let store = PackageStore::new(dir.path());
        let id = pinned("example.com", "acme/cmake");
        let pkg = store.package_dir(&id);

        record_origin(&pkg, id.as_identifier()).await.unwrap();
        assert_eq!(
            pkg.recorded_origins(),
            vec!["example.com/acme/cmake".to_string()],
            "the positive control: an intact marker reads back"
        );

        let marker = pkg
            .refs_origins_dir()
            .join(origin_marker_name("example.com/acme/cmake"));
        std::fs::write(&marker, "example.com/granted-org/anything").unwrap();
        assert!(
            pkg.recorded_origins().is_empty(),
            "a marker rewritten to name another repository must not be honoured"
        );
    }

    #[test]
    fn origin_of_lowercases_the_host_and_keeps_the_whole_repository_path() {
        let id = oci::Identifier::new_registry("Acme/tools/cmake", "GHCR.IO");
        assert_eq!(origin_of(&id), "ghcr.io/Acme/tools/cmake");
        let ported = oci::Identifier::new_registry("acme/tool", "localhost:5000");
        assert_eq!(origin_of(&ported), "localhost:5000/acme/tool", "the port is preserved");
    }

    #[test]
    fn path_flat_registry() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let expected = Path::new("/packages")
            .join("example.com")
            .join("sha256")
            .join("43")
            .join("567c07f1a6b07b5e8dc052108c9d4c");
        assert_eq!(store.path(&id), expected);
    }

    #[test]
    fn path_port_containing_registry_is_slugified() {
        let store = PackageStore::new("/packages");
        let id = pinned("localhost:5000", "cmake");
        let expected = Path::new("/packages")
            .join("localhost_5000")
            .join("sha256")
            .join("43")
            .join("567c07f1a6b07b5e8dc052108c9d4c");
        assert_eq!(store.path(&id), expected);
    }

    // ---- identifier-based accessors ---------------------------------------

    #[test]
    fn content_is_path_join_content() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let p = store.content(&id);
        assert_eq!(p.file_name().unwrap(), "content");
        assert_eq!(p.parent().unwrap(), store.path(&id));
    }

    #[test]
    fn metadata_is_path_join_metadata_json() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let p = store.metadata(&id);
        assert_eq!(p.file_name().unwrap(), "metadata.json");
        assert_eq!(p.parent().unwrap(), store.path(&id));
    }

    #[test]
    fn manifest_is_path_join_manifest_json() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let p = store.manifest(&id);
        assert_eq!(p.file_name().unwrap(), "manifest.json");
        assert_eq!(p.parent().unwrap(), store.path(&id));
    }

    #[test]
    fn resolve_is_path_join_resolve_json() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let p = store.resolve(&id);
        assert_eq!(p.file_name().unwrap(), "resolve.json");
        assert_eq!(p.parent().unwrap(), store.path(&id));
    }

    #[test]
    fn install_status_is_path_join_install_json() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let p = store.install_status(&id);
        assert_eq!(p.file_name().unwrap(), "install.json");
        assert_eq!(p.parent().unwrap(), store.path(&id));
    }

    #[test]
    fn digest_file_is_path_join_digest() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let p = store.digest_file(&id);
        assert_eq!(p.file_name().unwrap(), "digest");
        assert_eq!(p.parent().unwrap(), store.path(&id));
    }

    // ---- PackageDir accessors ---------------------------------------------

    #[test]
    fn package_dir_accessors() {
        let pkg = PackageDir {
            dir: PathBuf::from("/pkg/reg/sha256/43/rest"),
        };
        assert_eq!(pkg.content(), PathBuf::from("/pkg/reg/sha256/43/rest/content"));
        assert_eq!(pkg.metadata(), PathBuf::from("/pkg/reg/sha256/43/rest/metadata.json"));
        assert_eq!(pkg.manifest(), PathBuf::from("/pkg/reg/sha256/43/rest/manifest.json"));
        assert_eq!(pkg.resolve(), PathBuf::from("/pkg/reg/sha256/43/rest/resolve.json"));
        assert_eq!(
            pkg.install_status(),
            PathBuf::from("/pkg/reg/sha256/43/rest/install.json")
        );
        assert_eq!(pkg.digest_file(), PathBuf::from("/pkg/reg/sha256/43/rest/digest"));
        assert_eq!(
            pkg.refs_symlinks_dir(),
            PathBuf::from("/pkg/reg/sha256/43/rest/refs/symlinks")
        );
        assert_eq!(pkg.refs_deps_dir(), PathBuf::from("/pkg/reg/sha256/43/rest/refs/deps"));
        assert_eq!(
            pkg.refs_layers_dir(),
            PathBuf::from("/pkg/reg/sha256/43/rest/refs/layers")
        );
        assert_eq!(
            pkg.refs_blobs_dir(),
            PathBuf::from("/pkg/reg/sha256/43/rest/refs/blobs")
        );
    }

    // ---- *_for_content methods --------------------------------------------

    #[test]
    fn metadata_for_content_returns_sibling_metadata_json() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.metadata_for_content(&content).unwrap();
        assert_eq!(result, obj.join("metadata.json"));
    }

    #[test]
    fn refs_symlinks_dir_for_content_returns_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.refs_symlinks_dir_for_content(&content).unwrap();
        assert_eq!(result, obj.join("refs").join("symlinks"));
    }

    #[test]
    fn refs_deps_dir_for_content_returns_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.refs_deps_dir_for_content(&content).unwrap();
        assert_eq!(result, obj.join("refs").join("deps"));
    }

    #[test]
    fn refs_layers_dir_for_content_returns_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.refs_layers_dir_for_content(&content).unwrap();
        assert_eq!(result, obj.join("refs").join("layers"));
    }

    #[test]
    fn refs_blobs_dir_for_content_returns_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.refs_blobs_dir_for_content(&content).unwrap();
        assert_eq!(result, obj.join("refs").join("blobs"));
    }

    #[test]
    fn resolve_for_content_returns_sibling_resolve_json() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.resolve_for_content(&content).unwrap();
        assert_eq!(result, obj.join("resolve.json"));
    }

    #[test]
    fn metadata_for_content_accepts_package_root() {
        // After the layout flatten, install symlinks (`current`,
        // `candidates/{tag}`) target the package root rather than the
        // `content/` child. `*_for_content` must therefore accept the package
        // root and return the root's sibling files directly, without
        // climbing one level higher.
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let pkg_root = root.join("obj");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let result = store.metadata_for_content(&pkg_root).unwrap();
        assert_eq!(result, pkg_root.join("metadata.json"));
    }

    #[test]
    fn metadata_for_content_follows_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();

        let link = root.join("link");
        crate::symlink::create(&content, &link).unwrap();
        let result = store.metadata_for_content(&link).unwrap();
        assert_eq!(result, obj.join("metadata.json"));
    }

    // ---- list_all ---------------------------------------------------------

    #[tokio::test]
    async fn list_all_returns_empty_when_root_absent() {
        let store = PackageStore::new("/nonexistent/path/that/does/not/exist");
        assert_eq!(store.list_all().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_all_finds_single_package() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir
            .path()
            .join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c/content");
        std::fs::create_dir_all(&content).unwrap();

        let store = PackageStore::new(dir.path());
        let packages = store.list_all().await.unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].content(), content);
    }

    #[tokio::test]
    async fn list_all_skips_invalid_cas_path() {
        let dir = tempfile::tempdir().unwrap();

        // Valid package
        let valid = dir
            .path()
            .join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c/content");
        std::fs::create_dir_all(&valid).unwrap();

        // Invalid: wrong algorithm
        let invalid = dir
            .path()
            .join("example.com/md5/43/567c07f1a6b07b5e8dc052108c9d4c/content");
        std::fs::create_dir_all(&invalid).unwrap();

        let store = PackageStore::new(dir.path());
        let packages = store.list_all().await.unwrap();
        assert_eq!(packages.len(), 1);
    }

    #[tokio::test]
    async fn list_all_does_not_recurse_into_content_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir
            .path()
            .join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c/content");
        // Nested content/ inside the package should not produce a second result
        std::fs::create_dir_all(content.join("subdir/content")).unwrap();

        let store = PackageStore::new(dir.path());
        let packages = store.list_all().await.unwrap();
        assert_eq!(packages.len(), 1);
    }

    #[tokio::test]
    async fn list_all_does_not_recurse_into_refs_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir.path().join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c");
        std::fs::create_dir_all(pkg_dir.join("content")).unwrap();
        // refs/ directories should be skipped, not descended into
        std::fs::create_dir_all(pkg_dir.join("refs/symlinks")).unwrap();
        std::fs::create_dir_all(pkg_dir.join("refs/deps")).unwrap();

        let store = PackageStore::new(dir.path());
        let packages = store.list_all().await.unwrap();
        assert_eq!(packages.len(), 1);
    }

    // ── with_root_yields_off_tree_pkg_dir ─────────────────────────────────────
    //
    // `PackageDir::with_root(path)` must anchor the package at `path` regardless
    // of whether `path` is under `$OCX_HOME/packages/`. This is the constructor
    // that `pull_local` and `package test` use to steer the install pipeline
    // toward a caller-supplied destination outside the content-addressed store.
    //
    // Requirements:
    //   - `pkg.root()` == `path` (verbatim — no canonicalization applied)
    //   - All derived accessor paths (`content()`, `metadata()`, …) are children
    //     of `path`, NOT of any object-store shard path.
    //
    // This test is pure logic (no I/O) and always passes even before Phase 4.
    #[test]
    fn with_root_yields_off_tree_pkg_dir() {
        let arbitrary_root = PathBuf::from("/tmp/foo/test-pkg-1234");
        let pkg = PackageDir::with_root(arbitrary_root.clone());

        // Root is preserved verbatim.
        assert_eq!(
            pkg.root(),
            arbitrary_root.as_path(),
            "PackageDir::with_root must preserve the supplied path as root"
        );

        // All child accessors are direct children of the root — not under any
        // object-store shard prefix.
        assert_eq!(pkg.content(), arbitrary_root.join("content"));
        assert_eq!(pkg.metadata(), arbitrary_root.join("metadata.json"));
        assert_eq!(pkg.manifest(), arbitrary_root.join("manifest.json"));
        assert_eq!(pkg.resolve(), arbitrary_root.join("resolve.json"));
        assert_eq!(pkg.install_status(), arbitrary_root.join("install.json"));
        assert_eq!(pkg.digest_file(), arbitrary_root.join("digest"));
        assert_eq!(pkg.entrypoints(), arbitrary_root.join("entrypoints"));
        assert_eq!(pkg.refs_symlinks_dir(), arbitrary_root.join("refs").join("symlinks"));

        // Confirm the root is outside any OCX home path — this is the
        // "off-tree" guarantee the test name encodes.
        let root_str = pkg.root().to_str().unwrap_or("");
        assert!(
            !root_str.contains(".ocx/packages"),
            "with_root must not inject an object-store prefix: {}",
            root_str
        );
    }

    /// Renaming the entrypoints directory invalidates every installed launcher
    /// — the synthetic `PATH ⊳ <pkg_root>/entrypoints` entry baked at install
    /// time hard-codes this name. Both `PackageDir::entrypoints()` and
    /// `PackageStore::entrypoints(identifier)` must keep it stable.
    #[test]
    fn entrypoints_dir_name_is_stable_invariant() {
        // PackageDir
        let pkg_dir = PackageDir {
            dir: PathBuf::from("/packages/foo"),
        };
        let from_dir = pkg_dir.entrypoints();
        assert_eq!(
            from_dir.file_name().and_then(|n| n.to_str()),
            Some("entrypoints"),
            "PackageDir::entrypoints() must terminate in `entrypoints` (baked into installed launchers)"
        );

        // PackageStore
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let from_store = store.entrypoints(&id);
        assert_eq!(
            from_store.file_name().and_then(|n| n.to_str()),
            Some("entrypoints"),
            "PackageStore::entrypoints(id) must terminate in `entrypoints` (baked into installed launchers)"
        );
    }
}
