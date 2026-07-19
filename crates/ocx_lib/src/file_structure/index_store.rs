// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Self-contained index store: the local index collection, holding the hosted
//! `index.ocx.sh` served-tree layout verbatim (`adr_index_indirection.md`
//! Decision A2) — `config.json`, the `c/index.json` catalog, per-repository
//! root documents, and the digest-verified dispatch-object CAS. A tag/version
//! resolves through root documents and dispatch objects here; genuine
//! content-addressed blob bytes (config blobs, leaf platform manifests) are
//! never stored in this collection — they live exclusively in the
//! machine-global [`super::BlobStore`] (`$OCX_HOME/blobs`, Decision B2).
//!
//! This module also hosts [`IndexStore::ensure_repository_contained`] (the
//! CWE-22 containment guard every repository-keyed path builder runs through)
//! and the shared verify-and-self-heal write/read bodies
//! ([`IndexStore::write_verified_object`] / [`IndexStore::read_verified_object`])
//! the dispatch-object CAS writers below reuse.
//!
//! The store is outside the GC reachability graph (Decision B1) — no `CasTier`
//! variant, no locking beyond the per-source catalog transaction lock, no
//! migration code (YAGNI, per the ADR's explicit scope cut).
//!
//! See the `// ── Wire-grammar store ──` divider further down this file for
//! the write-order, locking, and durability contracts (`adr_index_indirection.md`
//! Decisions A2/A3/A4/F1).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Result;
use crate::oci::Digest;
use crate::oci::index::CatalogIndex;
use crate::utility::fs::LockedFile;

/// Max time to block waiting for another writer to release a source-scoped
/// index lock (catalog transaction or derived-root read-modify-write). Long
/// enough to survive a concurrent `ocx index update` against a slow registry,
/// short enough that a stuck holder surfaces instead of hanging the CLI.
pub const SOURCE_LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// Self-contained index store rooted at the index home
/// (`--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index` — home resolution is the
/// caller's concern, not this store's).
///
/// `locks_root` is deliberately *not* under `root`: the index home may be a
/// user-committed or read-only shipped copy, so cross-process locks live in the
/// machine-global `$OCX_HOME/locks` directory instead of as sidecars in the
/// index tree. Both catalog-transaction and derived-root locks acquire through
/// [`crate::utility::fs::lock_scoped`] keyed on the per-source directory's file
/// identity. [`Self::new`] defaults `locks_root` to `root/locks`; the two
/// production construction sites ([`crate::file_structure::FileStructure`] and
/// the `--index`/`OCX_INDEX` override in the CLI context) point it at the real
/// machine-global `$OCX_HOME/locks` via [`Self::with_locks_root`].
#[derive(Debug, Clone)]
pub struct IndexStore {
    root: PathBuf,
    locks_root: PathBuf,
}

impl IndexStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let locks_root = root.join("locks");
        Self { root, locks_root }
    }

    /// Redirect the machine-global lock root away from the default `root/locks`.
    ///
    /// Locks must never land inside a redirected (`--index`/`OCX_INDEX`) or
    /// shipped index home, so both real construction sites point this at
    /// `$OCX_HOME/locks`.
    #[must_use]
    pub fn with_locks_root(mut self, locks_root: impl Into<PathBuf>) -> Self {
        self.locks_root = locks_root.into();
        self
    }

    /// The root directory of this index store (the index home).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The machine-global lock root backing this store's cross-process locks
    /// (never inside [`Self::root`]).
    pub fn locks_root(&self) -> &Path {
        &self.locks_root
    }

    /// Guards that `repository` cannot escape the source subtree when joined as
    /// path components (CWE-22 defense-in-depth). The escape is a property of
    /// `repository` alone — the source segment is slugified — so the check runs
    /// against a fixed sentinel root: [`crate::utility::fs::path::join_under_root`]
    /// rejects an absolute segment, a residual `..` escape, a Windows
    /// drive/UNC/verbatim prefix, and a backslash-separated escape
    /// host-independently, exactly the ways [`super::repository_path`]'s
    /// verbatim `/`-split could land a read or write outside the index home.
    ///
    /// The primary boundary is the catalog-key validation in
    /// [`crate::oci::index::LocalIndex::sync_catalog`]; this is the store-level
    /// last line so no repository-keyed read or write ever touches a path
    /// outside its source subtree, whatever the caller.
    fn ensure_repository_contained(repository: &str) -> Result<()> {
        crate::utility::fs::path::join_under_root(Path::new("/ocx-index-home"), Path::new(repository))
            .map(|_| ())
            .map_err(|source| {
                super::error::Error::RepositoryEscapesIndexHome {
                    repository: repository.to_string(),
                    source,
                }
                .into()
            })
    }

    /// Guards that a `source` name cannot resolve to a filesystem escape once
    /// [`Self::wire_source_dir`] joins its slugified form under [`Self::root`]
    /// (CWE-22 defense-in-depth) — the sibling check to
    /// [`Self::ensure_repository_contained`] for the OTHER untrusted path
    /// input every wire-grammar builder takes. [`super::slugify`] replaces
    /// `/` (so a smuggled path separator cannot survive it) but preserves
    /// `.` — a `source` of exactly `".."` slugifies to `".."` verbatim, a
    /// genuine parent-directory escape once joined. Checked against the same
    /// fixed sentinel [`Self::ensure_repository_contained`] uses.
    ///
    /// ponytail: reuses [`super::error::Error::RepositoryEscapesIndexHome`]
    /// rather than adding a source-specific variant, to keep this fix inside
    /// `index_store.rs` — `error.rs` is shared with `temp_store.rs`. Upgrade
    /// to a dedicated `SourceEscapesIndexHome` variant if the "repository"
    /// wording in the message ever needs to read accurately for a source
    /// escape.
    fn ensure_source_contained(source: &str) -> Result<()> {
        let slug = super::slugify(source);
        crate::utility::fs::path::join_under_root(Path::new("/ocx-index-home"), Path::new(&slug))
            .map(|_| ())
            .map_err(|escape| {
                super::error::Error::RepositoryEscapesIndexHome {
                    repository: source.to_string(),
                    source: escape,
                }
                .into()
            })
    }

    /// Shared verify-and-self-heal write body: recompute-and-verify (A3/A4)
    /// against `claimed_digest`, then a tempfile + [`crate::utility::fs::persist_temp_file`]
    /// atomic publish to `target`. Parameterized on the target path so every
    /// wire-grammar dispatch-object CAS writer below
    /// ([`Self::write_dispatch_object`]) shares one verify-write body instead
    /// of copy-pasted logic — see each caller's doc comment for its own
    /// path-construction contract.
    async fn write_verified_object(&self, target: PathBuf, claimed_digest: &Digest, bytes: &[u8]) -> Result<()> {
        let computed = claimed_digest.algorithm().hash(bytes);
        if &computed != claimed_digest {
            return Err(super::error::Error::DigestMismatch {
                claimed: claimed_digest.clone(),
                computed,
            }
            .into());
        }

        // Verify-and-self-heal: re-hash any existing bytes before trusting
        // them. A zero-byte crash artifact or a tampered file both fail this
        // check and fall through to the overwrite below.
        if let Ok(existing) = tokio::fs::read(&target).await
            && claimed_digest.algorithm().hash(&existing) == computed
        {
            return Ok(());
        }
        let parent = target
            .parent()
            .ok_or_else(|| crate::error::file_error(&target, std::io::Error::other("path has no parent")))?
            .to_path_buf();
        tokio::fs::create_dir_all(&parent)
            .await
            .map_err(|e| crate::error::file_error(&parent, e))?;

        let bytes_owned = bytes.to_vec();
        let target_for_blocking = target.clone();
        let claimed = claimed_digest.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let mut tmp = tempfile::NamedTempFile::new_in(&parent)?;
            std::io::Write::write_all(&mut tmp, &bytes_owned)?;
            tmp.as_file().sync_data()?;
            match crate::utility::fs::persist_temp_file(tmp, &target_for_blocking) {
                Ok(()) => Ok(()),
                // A failed persist is success ONLY when the object now on disk
                // genuinely matches `claimed`. We reach this write in two cases:
                // a first write (target absent) or a self-heal (target present
                // but its bytes FAILED the verify above). In the self-heal case
                // the target still holds the KNOWN-CORRUPT bytes after a failed
                // rename, so a bare `exists()` check would report success while
                // leaving corruption in place. Re-read and re-hash: a genuine
                // concurrent CAS writer publishes byte-equivalent content that
                // hashes to `claimed` (Ok); the corrupt bytes still there, or an
                // unreadable target, propagate the original persist error.
                Err(err) => match std::fs::read(&target_for_blocking) {
                    Ok(current) if claimed.algorithm().hash(&current) == claimed => Ok(()),
                    _ => Err(err),
                },
            }
        })
        .await
        .map_err(|join_err| crate::error::file_error(&target, std::io::Error::other(join_err)))?
        .map_err(|io_err| crate::error::file_error(&target, io_err))?;
        Ok(())
    }

    /// Shared verify-on-read body: read `target`, recompute `sha256(bytes)`,
    /// verify against `digest`. Parameterized on the target path so every
    /// wire-grammar dispatch-object CAS reader below
    /// ([`Self::read_dispatch_object`]) shares one verify-read body instead of
    /// copy-pasted logic.
    async fn read_verified_object(&self, target: PathBuf, digest: &Digest) -> Result<Option<Vec<u8>>> {
        let bytes = match tokio::fs::read(&target).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(crate::error::file_error(&target, e)),
        };

        let computed = digest.algorithm().hash(&bytes);
        if &computed != digest {
            return Err(super::error::Error::DigestMismatch {
                claimed: digest.clone(),
                computed,
            }
            .into());
        }
        Ok(Some(bytes))
    }
}

// ── Wire-grammar store (A2) ─────────────────────────────────────────────────
//
// Each index source's subtree under the home is the hosted served tree,
// byte-for-byte grammar (`adr_index_indirection.md` Decision A2):
//
// ```text
// {root}/{slug(source)}/
//   config.json                       — published indices only
//   c/index.json (+ .etag)            — published indices only — catalog and
//                                        conditional-GET validator
//   p/{ns}/{pkg}.json                 — root doc
//   p/{ns}/{pkg}/o/{algo}/{hex}.json  — dispatch object CAS (obs object /
//                                        image index only, never a leaf
//                                        platform manifest — A3)
// ```
//
// Locks are NOT sidecars in this tree: an index home may be a read-only shipped
// copy. Every source-scoped lock (catalog transaction, derived-root
// read-modify-write) lives in the machine-global `$OCX_HOME/locks`, keyed on the
// per-source directory's file identity ([`IndexStore::lock_source`]).
//
// **Write-order contract (F1).** `ocx index update <pkg>` writes in a fixed,
// idempotent order — never one atomic operation:
//
// 1. the dispatch object into `o/` ([`IndexStore::write_dispatch_object`] —
//    CAS write by content hash; an orphan left by an aborted write is
//    harmless, nothing points at it yet);
// 2. the root `p/{ns}/{pkg}.json` ([`CatalogTransaction::write_root`] —
//    tempfile + atomic rename);
// 3. the package's `c/index.json` catalog entry (upserted into the same
//    [`CatalogTransaction`], committed by [`CatalogTransaction::commit`]).
//
// A crash between any two steps is recoverable on the next read or update —
// never rejected. [`IndexStore::read_root`] performs the read-path half of
// that recovery (root/catalog digest disagreement self-heals by
// re-derivation, logged at info/debug — never a hard error).
//
// **Locking contract.** Every catalog mutation for a source — a per-package
// entry upsert, a whole-catalog reconcile-merge (F2 sync), AND the `.etag`
// sidecar publication — goes through [`IndexStore::begin_catalog_transaction`],
// which takes the source-scoped `"index-catalog"` lock (machine-global,
// file-identity-keyed — [`IndexStore::lock_source`]) and re-reads the on-disk
// catalog before handing it to the caller. All network work (fetching
// a remote root, dispatch object, or catalog) MUST happen *before* opening a
// transaction — the guard's lifetime should span only the local
// read-mutate-write critical section. A wholesale replace of the catalog from
// a pre-lock read is forbidden by construction: the map a caller mutates is
// always the freshly-read, post-lock map, never a caller-held stale snapshot.
//
// **Durability scope.** The recovery guarantee above covers process-crash /
// kill. Hard power-loss is accepted-open: [`crate::utility::fs::persist_temp_file`]
// fsyncs file data, not the parent directory; a residual inconsistency after a
// real power-loss event still self-heals by recompute on the next read, it is
// just not guaranteed durable across that specific failure class
// (`adr_index_indirection.md` Decision B1 lifecycle framing).
//
// **Layering.** This store stays grammar-agnostic — it moves bytes and knows
// the wire *shapes* (via [`crate::oci::index::IndexRoot`]) only enough to
// parse a root doc and derive its catalog entry. Anything about what a
// `repository` field *means* (the `oci://` scheme parse, C3) is the caller's
// concern, injected through the `repository_check` hook on [`Self::read_root`]
// and [`CatalogTransaction::write_root`].

impl IndexStore {
    /// Root directory for one index source under the collection home
    /// (`{root}/{slug(source)}/`, A2). The source is a top-level directory
    /// here, never nested under a shared `p/`.
    fn wire_source_dir(&self, source: &str) -> PathBuf {
        self.root.join(super::slugify(source))
    }

    /// Path to a published source's `config.json` (the `{"format_version":
    /// 1}` version pin, A2). Absent for a derived (OCI-registry) source.
    pub fn source_config_path(&self, source: &str) -> PathBuf {
        self.wire_source_dir(source).join("config.json")
    }

    /// Path to a source's `c/index.json` catalog (● `{"<ns>/<pkg>":
    /// "sha256:<root-digest>"}`, A2/F1) — per-source, unlike
    /// [`Self::catalog_path`]'s single global file.
    pub fn source_catalog_path(&self, source: &str) -> PathBuf {
        self.wire_source_dir(source).join("c").join("index.json")
    }

    /// Path to a source catalog's `ETag` conditional-GET validator sidecar (F2).
    pub fn source_catalog_etag_path(&self, source: &str) -> PathBuf {
        self.source_catalog_path(source).with_added_extension("etag")
    }

    /// Acquire an exclusive cross-process lock scoped to one source's subtree,
    /// held for the returned guard's lifetime.
    ///
    /// The lock is keyed on the per-source directory's *file identity* under
    /// the machine-global [`Self::locks_root`] (via
    /// [`crate::utility::fs::lock_scoped`]) — never a sidecar inside the index
    /// home, which may be a read-only shipped copy. `scope` separates lock
    /// purposes that share a source directory (`"index-catalog"` for a
    /// `c/index.json` mutation, `"index-root"` for a derived root's
    /// read-modify-write); `discriminator` distinguishes locks within a scope.
    /// The source directory is created first so it has a stable identity to key
    /// on.
    ///
    /// # Errors
    ///
    /// Fails if the source directory cannot be created, or the lock cannot be
    /// acquired within `timeout`.
    pub async fn lock_source(
        &self,
        scope: &str,
        source: &str,
        discriminator: &str,
        timeout: Duration,
    ) -> Result<LockedFile> {
        Self::ensure_source_contained(source)?;
        let source_dir = self.wire_source_dir(source);
        tokio::fs::create_dir_all(&source_dir)
            .await
            .map_err(|e| crate::error::file_error(&source_dir, e))?;
        crate::utility::fs::lock_scoped(&self.locks_root, scope, &source_dir, discriminator, timeout).await
    }

    /// Path to a repository's root document (`p/{ns}/{pkg}.json`, A2).
    pub fn root_document_path(&self, source: &str, repository: &str) -> PathBuf {
        self.wire_source_dir(source)
            .join("p")
            .join(super::repository_path(repository))
            .with_added_extension("json")
    }

    /// Path to a dispatch object (`p/{ns}/{pkg}/o/{algo}/{hex}.json`, A2/A3).
    ///
    /// Carries a `.json` extension and lives under the repository's own
    /// `p/{ns}/{pkg}/` directory — the object store holds **dispatch objects
    /// only** (an observation object or an image index); a leaf platform
    /// manifest is never written here (A3).
    pub fn dispatch_object_path(&self, source: &str, repository: &str, digest: &Digest) -> PathBuf {
        self.wire_source_dir(source)
            .join("p")
            .join(super::repository_path(repository))
            .join("o")
            .join(digest.algorithm().prefix())
            .join(format!("{}.json", digest.hex()))
    }

    /// Writes `bytes` verbatim to the dispatch-object path for `(source,
    /// repository, claimed_digest)` (A2/A3 — `p/{ns}/{pkg}/o/{algo}/{hex}.json`).
    ///
    /// Verify-and-self-heal write: recomputes `sha256(bytes)` and verifies it
    /// equals `claimed_digest` **before** the write commits — a mismatch is a
    /// hard error ([`super::error::Error::DigestMismatch`], A3/A4 CWE-345
    /// trust-boundary check), never a silent persist. If a file already exists
    /// at the target, its bytes are re-hashed and short-circuit on a match; a
    /// mismatch (zero-byte crash artifact, tampered file) falls through and
    /// overwrites via tempfile + [`crate::utility::fs::persist_temp_file`]
    /// atomic rename. Routed through the private [`Self::write_verified_object`]
    /// helper shared by every dispatch-object CAS writer.
    ///
    /// # Errors
    ///
    /// Returns [`super::error::Error::DigestMismatch`] if the computed digest
    /// disagrees with `claimed_digest`.
    pub async fn write_dispatch_object(
        &self,
        source: &str,
        repository: &str,
        claimed_digest: &Digest,
        bytes: &[u8],
    ) -> Result<()> {
        Self::ensure_source_contained(source)?;
        Self::ensure_repository_contained(repository)?;
        let target = self.dispatch_object_path(source, repository, claimed_digest);
        self.write_verified_object(target, claimed_digest, bytes).await
    }

    /// Reads the verbatim dispatch-object bytes for `(source, repository,
    /// digest)` (A2/A3).
    ///
    /// Returns `Ok(None)` if the object is absent. If present, recomputes the
    /// digest from the bytes on disk and verifies it against `digest` — a
    /// byte-tampered object fails with [`super::error::Error::DigestMismatch`],
    /// never silently loads. Routed through the private
    /// [`Self::read_verified_object`] helper shared by every dispatch-object
    /// CAS reader.
    pub async fn read_dispatch_object(
        &self,
        source: &str,
        repository: &str,
        digest: &Digest,
    ) -> Result<Option<Vec<u8>>> {
        Self::ensure_source_contained(source)?;
        Self::ensure_repository_contained(repository)?;
        let target = self.dispatch_object_path(source, repository, digest);
        self.read_verified_object(target, digest).await
    }

    /// Derives the catalog-entry value for a root document's on-disk bytes:
    /// `sha256:<hex>` of the bytes themselves.
    ///
    /// F1: "the catalog entry is derivable, not independently trusted... it
    /// is exactly `sha256(root bytes)` and nothing more." A read-path
    /// disagreement between a stored root and its catalog entry is therefore
    /// an inconsistency, never evidence of tampering — see [`Self::read_root`].
    pub fn root_catalog_entry(bytes: &[u8]) -> String {
        crate::oci::Algorithm::Sha256.hash(bytes).to_string()
    }

    /// Reads and parses a repository's root document (`p/{ns}/{pkg}.json`),
    /// cross-checking it against its `c/index.json` catalog entry when this
    /// source carries one and self-healing a mismatch by re-deriving the
    /// entry from the on-disk root bytes and rewriting the catalog (F1 —
    /// "re-derive the entry from the root bytes actually on disk and rewrite
    /// the catalog to match... logged at info/debug, never rejected").
    ///
    /// `repository_check` is the C3 cross-check hook: this store stays
    /// grammar-agnostic about what a `repository` pointer means, so the
    /// caller inspects the parsed root's `repository` field against its own
    /// domain rules (e.g. the `oci://` scheme parse) and returns `Err` to
    /// hard-fail a genuinely malformed physical reference.
    ///
    /// Returns `Ok(None)` when no root document exists yet. The only hard
    /// failures (F1) are an unparseable root
    /// ([`super::error::Error::MalformedRootDocument`]) or a
    /// `repository_check` failure — genuine corruption, never a bare
    /// root/catalog digest disagreement, which always recovers instead of
    /// erroring.
    pub async fn read_root(
        &self,
        source: &str,
        repository: &str,
        repository_check: impl Fn(&crate::oci::index::IndexRoot) -> Result<()>,
    ) -> Result<Option<RootReadResult>> {
        let Some((bytes, root)) = self.read_root_inner(source, repository, &repository_check).await? else {
            return Ok(None);
        };

        let derived_entry = Self::root_catalog_entry(&bytes);
        // ponytail: each published resolve re-parses the whole `c/index.json`
        // here to look up one entry — O(catalog) per resolve. Deferred, NOT
        // memoized: a process-lifetime per-source catalog cache cannot be cleanly
        // invalidated (a concurrent `ocx index update` process rewrites this file
        // out-of-band, and `CatalogTransaction::commit` lives on `IndexStore`, not
        // the `LocalIndex` that would hold the cache), and a stale catalog is a
        // correctness bug (spurious recovery / missed staleness). This single read
        // is already required for the F1 cross-check; the recovery re-read below is
        // deliberately post-lock. Revisit only if profiling shows this dominates.
        let on_disk_entry = self
            .read_source_catalog(source)
            .await?
            .and_then(|catalog| catalog.get(repository).cloned());

        if on_disk_entry.as_deref() == Some(derived_entry.as_str()) {
            return Ok(Some(RootReadResult {
                bytes,
                root,
                catalog_status: CatalogEntryStatus::Consistent { entry: derived_entry },
            }));
        }

        // Absent or stale → self-heal under the source's transaction lock, but
        // BEST-EFFORT: the re-derived `derived_entry` is already authoritative
        // for THIS read; persisting it only spares the next read the same
        // recovery. A read-only or otherwise unwritable index home — a shipped
        // `.ocx/` copy or a `--index`/`OCX_INDEX` redirect resolved online —
        // cannot land that write, so a lock/re-read/commit failure is logged at
        // debug and swallowed rather than failing the resolve. A read-only home
        // must still resolve a version choice (`adr_index_indirection.md` B1/B2).
        match self
            .persist_recovered_catalog_entry(source, repository, &repository_check, &bytes, &root)
            .await
        {
            Ok((bytes, root, recovered_entry)) => {
                crate::log::debug!(
                    "recovered catalog entry for source '{source}' repository '{repository}' \
                     (root/catalog straddle, benign)"
                );
                Ok(Some(RootReadResult {
                    bytes,
                    root,
                    catalog_status: CatalogEntryStatus::Recovered { entry: recovered_entry },
                }))
            }
            Err(error) => {
                crate::log::debug!(
                    "catalog-entry recovery for source '{source}' repository '{repository}' \
                     could not persist ({error}); using the re-derived entry for this read"
                );
                Ok(Some(RootReadResult {
                    bytes,
                    root,
                    catalog_status: CatalogEntryStatus::Recovered { entry: derived_entry },
                }))
            }
        }
    }

    /// Persist the re-derived catalog entry for a root/catalog straddle (F1
    /// read-path recovery), under the source transaction lock, returning the
    /// (possibly fresher) bytes, parsed root, and recovered entry.
    ///
    /// A concurrent writer can commit a FRESHER root + catalog entry between the
    /// caller's pre-lock read and this lock acquisition; the root is re-read
    /// AFTER the lock is held and the recovered entry derived from THOSE bytes.
    /// Deriving from the pre-lock read would silently clobber the concurrent
    /// writer's newer entry back to stale (lost-update) — see
    /// `read_root_recovery_never_clobbers_a_fresher_concurrently_committed_entry`.
    ///
    /// The `prelock_*` values are the caller's pre-lock read, used only when the
    /// root vanished between it and the lock acquisition. This is a fallible
    /// write; [`Self::read_root`] treats a failure as best-effort (a read-only
    /// index home) and falls back to its pre-lock entry.
    async fn persist_recovered_catalog_entry(
        &self,
        source: &str,
        repository: &str,
        repository_check: impl Fn(&crate::oci::index::IndexRoot) -> Result<()>,
        prelock_bytes: &[u8],
        prelock_root: &crate::oci::index::IndexRoot,
    ) -> Result<(Vec<u8>, crate::oci::index::IndexRoot, String)> {
        let mut transaction = self.begin_catalog_transaction(source).await?;
        let (bytes, root) = match self.read_root_inner(source, repository, &repository_check).await? {
            Some(fresh) => fresh,
            None => (prelock_bytes.to_vec(), prelock_root.clone()),
        };
        let recovered_entry = Self::root_catalog_entry(&bytes);
        transaction
            .catalog
            .insert(repository.to_string(), recovered_entry.clone());
        transaction.commit(None).await?;
        Ok((bytes, root, recovered_entry))
    }

    /// Read a DERIVED (OCX-authored) source's root document — the catalog-free
    /// sibling of [`Self::read_root`]. A derived index has no `c/index.json`
    /// (A2: its catalog is the directory enumeration of `p/`), so there is
    /// nothing to cross-check and no self-heal to perform: read → parse →
    /// `repository_check` only, returning [`CatalogEntryStatus::NoCatalog`].
    /// `Ok(None)` when no root exists yet.
    ///
    /// The caller ([`crate::oci::index::LocalIndex`]) picks this over
    /// [`Self::read_root`] per source kind — provenance never enters this store,
    /// it is passed at the call site, so the derived/published divergence stays
    /// caller-side (`adr_index_indirection.md` A2/H "two ifs").
    pub async fn read_root_uncatalogued(
        &self,
        source: &str,
        repository: &str,
        repository_check: impl Fn(&crate::oci::index::IndexRoot) -> Result<()>,
    ) -> Result<Option<RootReadResult>> {
        let Some((bytes, root)) = self.read_root_inner(source, repository, repository_check).await? else {
            return Ok(None);
        };
        Ok(Some(RootReadResult {
            bytes,
            root,
            catalog_status: CatalogEntryStatus::NoCatalog,
        }))
    }

    /// Shared read → parse → `repository_check` core for [`Self::read_root`] and
    /// [`Self::read_root_uncatalogued`]: reads the root-document bytes, parses
    /// them into an [`crate::oci::index::IndexRoot`], and runs the caller's C3
    /// `repository` cross-check. Returns `Ok(None)` when no root exists yet.
    ///
    /// The only hard failures are an unparseable root
    /// ([`super::error::Error::MalformedRootDocument`]) or a `repository_check`
    /// failure — genuine corruption. A catalog disagreement is never one of
    /// these; that recovery lives in [`Self::read_root`].
    async fn read_root_inner(
        &self,
        source: &str,
        repository: &str,
        repository_check: impl Fn(&crate::oci::index::IndexRoot) -> Result<()>,
    ) -> Result<Option<(Vec<u8>, crate::oci::index::IndexRoot)>> {
        Self::ensure_source_contained(source)?;
        Self::ensure_repository_contained(repository)?;
        let target = self.root_document_path(source, repository);
        let bytes = match tokio::fs::read(&target).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(crate::error::file_error(&target, e)),
        };
        let root: crate::oci::index::IndexRoot =
            serde_json::from_slice(&bytes).map_err(|cause| super::error::Error::MalformedRootDocument {
                index_source: source.to_string(),
                repository: repository.to_string(),
                cause,
            })?;
        repository_check(&root)?;
        Ok(Some((bytes, root)))
    }

    /// Reads this source's `c/index.json` catalog map (`Ok(None)` when absent
    /// — a fresh source or a derived index that never gets one, A2).
    ///
    /// This is the offline listing source and the diff basis for the next
    /// catalog sync (F2); the caller reads it before the network fetch, and the
    /// reconcile-commit re-reads it under the lock before writing (never a
    /// wholesale replace from this pre-lock read).
    pub async fn read_source_catalog(&self, source: &str) -> Result<Option<CatalogIndex>> {
        Self::ensure_source_contained(source)?;
        let target = self.source_catalog_path(source);
        let bytes = match tokio::fs::read(&target).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(crate::error::file_error(&target, e)),
        };
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    /// Reads this source's persisted catalog `ETag`, if any (`Ok(None)` when
    /// absent). The conditional-GET validator for the next catalog sync (F2).
    pub async fn read_source_catalog_etag(&self, source: &str) -> Result<Option<String>> {
        Self::ensure_source_contained(source)?;
        let target = self.source_catalog_etag_path(source);
        match tokio::fs::read_to_string(&target).await {
            Ok(etag) => Ok(Some(etag.trim().to_string()).filter(|etag| !etag.is_empty())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(crate::error::file_error(&target, e)),
        }
    }

    /// Writes `bytes` verbatim to a repository's root-document path
    /// (`p/{ns}/{pkg}.json`, A2) with **no** catalog upsert — the catalog-free
    /// root writer for a DERIVED (OCX-authored) source, whose catalog is the
    /// directory enumeration of `p/` (A2), so there is no `c/index.json` to keep
    /// in step. The catalog-carrying counterpart is
    /// [`CatalogTransaction::write_root`]; the catalog-free read counterpart is
    /// [`Self::read_root_document_bytes`].
    ///
    /// **Locking contract.** This is a bare atomic publish — it does **not**
    /// serialize its own writes. A derived root is a shared multi-writer file
    /// (concurrent `commit_root_tag` calls for distinct tags of one repository),
    /// so a caller performing a read-modify-write MUST hold an exclusive lock on
    /// `root_document_path(source, repository).with_added_extension("lock")`
    /// across the read + write. Today's only caller,
    /// [`crate::oci::index::LocalIndex::commit_root_tag`], does exactly that.
    pub async fn write_root_document(&self, source: &str, repository: &str, bytes: &[u8]) -> Result<()> {
        Self::ensure_source_contained(source)?;
        Self::ensure_repository_contained(repository)?;
        let target = self.root_document_path(source, repository);
        Self::write_bytes_atomic(&target, bytes.to_vec()).await
    }

    /// Reads a repository's verbatim root-document bytes (`p/{ns}/{pkg}.json`,
    /// A2), or `Ok(None)` when absent. Unlike [`Self::read_root`] /
    /// [`Self::read_root_uncatalogued`] this neither parses nor cross-checks —
    /// the raw bytes let a caller round-trip a derived root through its own
    /// authoring shape ([`crate::oci::index::LocalIndex::commit_root_tag`]).
    pub async fn read_root_document_bytes(&self, source: &str, repository: &str) -> Result<Option<Vec<u8>>> {
        Self::ensure_source_contained(source)?;
        Self::ensure_repository_contained(repository)?;
        let target = self.root_document_path(source, repository);
        match tokio::fs::read(&target).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(crate::error::file_error(&target, e)),
        }
    }

    /// Shared tempfile + atomic-rename publish body for this section's writers
    /// ([`CatalogTransaction::write_root`] / [`CatalogTransaction::commit`] /
    /// [`IndexStore::write_root_document`]).
    async fn write_bytes_atomic(target: &Path, bytes: Vec<u8>) -> Result<()> {
        let parent = target
            .parent()
            .ok_or_else(|| crate::error::file_error(target, std::io::Error::other("path has no parent")))?
            .to_path_buf();
        tokio::fs::create_dir_all(&parent)
            .await
            .map_err(|e| crate::error::file_error(&parent, e))?;

        let target_owned = target.to_path_buf();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let mut tmp = tempfile::NamedTempFile::new_in(&parent)?;
            std::io::Write::write_all(&mut tmp, &bytes)?;
            tmp.as_file().sync_data()?;
            crate::utility::fs::persist_temp_file(tmp, &target_owned)
        })
        .await
        .map_err(|join_err| crate::error::file_error(target, std::io::Error::other(join_err)))?
        .map_err(|io_err| crate::error::file_error(target, io_err))?;
        Ok(())
    }

    /// Lists every repository under `source` via directory enumeration of
    /// `p/` (`adr_index_indirection.md` A2 — a **derived** index's catalog IS
    /// the directory enumeration; there is no `c/index.json` to read). Walks
    /// `{root}/{slug(source)}/p/`, collecting every `*.json` root-document
    /// file (stripped of its extension) as a `<ns>/<pkg>`-shaped repository
    /// path, and never descends into an `o/` dispatch-object CAS directory
    /// (its own `*.json` object files are not root documents). Sorted,
    /// deduplicated. Returns an empty vec when the source directory does not
    /// exist.
    ///
    /// Iterative `tokio::fs::read_dir` walk, never blocking the executor —
    /// each directory is read exactly once (`entry.file_type()` from that one
    /// listing decides both the recursion and the `.json`-item collection),
    /// unlike a `DirWalker` classify hook here would: `DirWalker` already
    /// performs its own `tokio::fs::read_dir` per directory to find children,
    /// so a synchronous listing inside `classify` would read the same
    /// directory twice per visit, once async and once blocking the worker
    /// thread the classify closure runs on.
    pub async fn list_wire_repositories(&self, source: &str) -> Result<Vec<String>> {
        use std::collections::VecDeque;

        use crate::utility::fs::path_exists_lossy;

        Self::ensure_source_contained(source)?;
        let root = self.wire_source_dir(source).join("p");
        if !path_exists_lossy(&root).await {
            return Ok(Vec::new());
        }

        let mut repos = Vec::new();
        let mut queue: VecDeque<PathBuf> = VecDeque::from([root.clone()]);
        while let Some(dir) = queue.pop_front() {
            // The dispatch-object CAS dir is always named "o" and always a DIRECT
            // CHILD of a package dir (`p/<ns>/<pkg>/o/…`, module header layout).
            // A package dir is distinguished from a namespace/package directory
            // that merely happens to be NAMED "o" by one structural fact: a
            // package dir always has a sibling root-document file at
            // `{package-dir}.json` in ITS OWN parent (`root_document_path`) —
            // e.g. `p/kitware/cmake/` pairs with `p/kitware/cmake.json`. A fixed
            // depth cannot anchor this: a single-segment repository's package
            // dir sits one level below `p/`, a two-segment one two levels below.
            // Checking the sibling instead of counting components holds
            // regardless of how many repository-path segments precede it, so a
            // namespace or package literally named "o" (whose OWN sibling
            // `.json` lives one level further up, not beside itself) is never
            // pruned.
            let dir_is_a_package_dir = path_exists_lossy(&dir.with_extension("json")).await;

            let mut entries = tokio::fs::read_dir(&dir)
                .await
                .map_err(|e| crate::error::file_error(&dir, e))?;
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| crate::error::file_error(&dir, e))?
            {
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .await
                    .map_err(|e| crate::error::file_error(&path, e))?;

                if file_type.is_dir() {
                    let is_dispatch_object_dir =
                        dir_is_a_package_dir && path.file_name().and_then(|name| name.to_str()) == Some("o");
                    if !is_dispatch_object_dir {
                        queue.push_back(path);
                    }
                    continue;
                }
                if !file_type.is_file() || path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                let Ok(relative) = dir.strip_prefix(&root) else {
                    continue;
                };
                let mut segments: Vec<String> = relative
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy().into_owned())
                    .collect();
                segments.push(stem.to_string());
                repos.push(segments.join("/"));
            }
        }

        repos.sort();
        repos.dedup();
        Ok(repos)
    }

    /// Takes the source-scoped `"index-catalog"` lock ([`Self::lock_source`],
    /// machine-global + file-identity-keyed) and re-reads the on-disk catalog,
    /// returning a guard over the freshest map (F1's "re-read + reconcile after
    /// acquiring the lock, before committing" contract).
    ///
    /// This is the **single** entry point for every catalog mutation for
    /// `source` — a per-package root+entry upsert
    /// ([`CatalogTransaction::write_root`]), a whole-catalog reconcile-merge
    /// (F2 sync, mutate [`CatalogTransaction::catalog`] directly), or `.etag`
    /// sidecar publication (via [`CatalogTransaction::commit`]) — so no
    /// writer can bypass the re-read. All network work (fetching a remote
    /// root, dispatch object, or catalog) MUST happen before this call.
    pub async fn begin_catalog_transaction(&self, source: &str) -> Result<CatalogTransaction<'_>> {
        let lock = self
            .lock_source("index-catalog", source, "c/index.json", SOURCE_LOCK_TIMEOUT)
            .await?;
        // Re-read AFTER acquiring the lock — the freshest on-disk map, never
        // a caller-held stale pre-lock snapshot (F1's re-read-then-reconcile
        // contract).
        let catalog = self.read_source_catalog(source).await?.unwrap_or_default();
        Ok(CatalogTransaction {
            store: self,
            source: source.to_string(),
            lock,
            catalog,
        })
    }
}

/// A root document read from disk: the verbatim bytes (needed for
/// byte-identical re-serialization / catalog-entry re-derivation, A2) and the
/// parsed wire shape.
#[derive(Debug, Clone)]
pub struct RootReadResult {
    /// The verbatim on-disk bytes.
    pub bytes: Vec<u8>,
    /// The parsed wire shape.
    pub root: crate::oci::index::IndexRoot,
    /// The outcome of cross-checking this root against its catalog entry.
    pub catalog_status: CatalogEntryStatus,
}

/// Outcome of cross-checking a root's derived catalog entry (F1 read-path
/// recovery) against what was actually stored on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogEntryStatus {
    /// This source carries no `c/index.json` catalog (a derived/OCI-authored
    /// index, A2) — nothing to cross-check.
    ///
    /// Constructed by [`IndexStore::read_root_uncatalogued`], the catalog-free
    /// sibling of [`IndexStore::read_root`]. This store stays
    /// grammar-agnostic and has no signal of its own for "derived vs published"
    /// source kind; the caller ([`crate::oci::index::LocalIndex`]) selects the
    /// uncatalogued read per source kind, so the A2 exemption is enforced
    /// caller-side, never here.
    NoCatalog,
    /// The on-disk catalog entry already matched the root's derived entry.
    Consistent { entry: String },
    /// The on-disk catalog entry disagreed with (or was absent for) the
    /// root's derived entry; the catalog was rewritten to `entry`. Logged at
    /// info/debug — an ordinary, benign straddle (F1), never a warn-worthy
    /// event.
    Recovered { entry: String },
}

/// A held catalog-transaction lock for one index source, together with the
/// freshly re-read on-disk catalog map (F1's "re-read + reconcile after
/// acquiring the lock, before committing" contract).
///
/// Obtained via [`IndexStore::begin_catalog_transaction`] — the only way
/// to reach a mutable [`CatalogIndex`] map, so no writer can skip the
/// lock-then-reread step. Network work (fetching a remote root, dispatch
/// object, or catalog) MUST happen before opening a transaction; this guard's
/// lifetime should span only the local read-mutate-write critical section.
///
pub struct CatalogTransaction<'store> {
    store: &'store IndexStore,
    source: String,
    /// Held purely for its `Drop` side effect — releases the exclusive
    /// advisory lock when the transaction is committed or dropped. Never
    /// read directly, so it needs an explicit dead-code allow.
    #[allow(dead_code)]
    lock: LockedFile,
    catalog: CatalogIndex,
}

impl CatalogTransaction<'_> {
    /// The freshly re-read (post-lock) catalog map. Mutate in place for a
    /// whole-catalog reconcile-merge (F2 sync); [`Self::write_root`] mutates
    /// it too, for the per-package upsert case.
    pub fn catalog(&mut self) -> &mut CatalogIndex {
        &mut self.catalog
    }

    /// Writes `bytes` verbatim to the root-document path for `repository`
    /// (`p/{ns}/{pkg}.json`, A2 — atomic tempfile+rename) and upserts its
    /// derived entry ([`IndexStore::root_catalog_entry`]) into this
    /// transaction's catalog map.
    ///
    /// `repository_check` carries the same C3 cross-check contract as
    /// [`IndexStore::read_root`] — this store parses `bytes` only far
    /// enough to run the caller's hook and derive the catalog entry; it does
    /// not itself interpret `repository`.
    pub async fn write_root(
        &mut self,
        repository: &str,
        bytes: &[u8],
        repository_check: impl FnOnce(&crate::oci::index::IndexRoot) -> Result<()>,
    ) -> Result<()> {
        IndexStore::ensure_repository_contained(repository)?;
        let root: crate::oci::index::IndexRoot =
            serde_json::from_slice(bytes).map_err(|cause| super::error::Error::MalformedRootDocument {
                index_source: self.source.clone(),
                repository: repository.to_string(),
                cause,
            })?;
        repository_check(&root)?;

        let target = self.store.root_document_path(&self.source, repository);
        IndexStore::write_bytes_atomic(&target, bytes.to_vec()).await?;

        self.catalog
            .insert(repository.to_string(), IndexStore::root_catalog_entry(bytes));
        Ok(())
    }

    /// Publishes the (possibly mutated) catalog map — and, when `etag` is
    /// `Some`, the `.etag` sidecar — atomically, still under the held lock.
    /// Consumes the guard, releasing the lock on return.
    pub async fn commit(self, etag: Option<&str>) -> Result<()> {
        let catalog_path = self.store.source_catalog_path(&self.source);
        let catalog_bytes = serde_json::to_vec_pretty(&self.catalog)?;
        IndexStore::write_bytes_atomic(&catalog_path, catalog_bytes).await?;

        if let Some(etag) = etag {
            let etag_path = self.store.source_catalog_etag_path(&self.source);
            IndexStore::write_bytes_atomic(&etag_path, etag.as_bytes().to_vec()).await?;
        }

        // `self.lock` drops here, releasing the exclusive advisory lock —
        // held across both the catalog and (when present) the etag write.
        Ok(())
    }
}

/// Specification tests for the wire-grammar section (`adr_index_indirection.md`
/// Decisions A2/A3/A4/F1/F2) — written from the design record's contracts, not
/// the stub bodies. Every test exercising a stubbed method
/// (`write_dispatch_object`, `read_dispatch_object`, `root_catalog_entry`,
/// `read_root`, `begin_catalog_transaction`, `CatalogTransaction::write_root`/
/// `commit`) is EXPECTED TO PANIC on `unimplemented!()` until implementation
/// lands — that panic is a passing signal for this phase. Path-helper tests
/// (§1) exercise already-implemented pure functions and are expected to pass
/// now; they stay here as regression coverage, not stub-failure proof.
#[cfg(test)]
mod wire_grammar_tests {
    use std::ffi::OsStr;

    use super::*;
    use crate::cli::{ClassifyExitCode, ExitCode};
    use crate::oci::index::IndexRoot;

    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
    const SHA512_HEX: &str = "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e";

    fn store(dir: &Path) -> IndexStore {
        IndexStore::new(dir)
    }

    /// Builds minimal root-document bytes matching `test/src/static_index.py`'s
    /// `write_package()` shape: a `repository` pointer and one tag whose
    /// `content` names `obs_digest`.
    fn minimal_root_bytes(repository: &str, obs_digest: &Digest) -> Vec<u8> {
        serde_json::json!({
            "repository": repository,
            "tags": {
                "latest": { "content": obs_digest.to_string(), "observed": "2026-07-18T09:00:00Z" }
            }
        })
        .to_string()
        .into_bytes()
    }

    // ── 1. Path grammar / byte-parity (cross-checked against the layout
    //        test/src/static_index.py writes: `p/<repository>.json` for the
    //        root, `p/<repository>/o/sha256/<hex>.json` for dispatch objects,
    //        `c/index.json` for the catalog) ──────────────────────────────

    #[test]
    fn source_config_path_matches_hosted_layout() {
        let s = IndexStore::new("/index");
        assert_eq!(s.source_config_path("ocx.sh"), Path::new("/index/ocx.sh/config.json"));
    }

    #[test]
    fn source_catalog_path_matches_hosted_layout() {
        let s = IndexStore::new("/index");
        assert_eq!(
            s.source_catalog_path("ocx.sh"),
            Path::new("/index/ocx.sh/c/index.json"),
            "must mirror static_index.py's fixture_root/c/index.json"
        );
    }

    #[test]
    fn source_catalog_etag_sidecar_appends_extension() {
        let s = IndexStore::new("/index");
        let catalog = s.source_catalog_path("ocx.sh");
        assert_eq!(
            s.source_catalog_etag_path("ocx.sh"),
            catalog.with_added_extension("etag")
        );
    }

    #[test]
    fn locks_root_defaults_under_root_but_is_overridable_off_tree() {
        let s = IndexStore::new("/index");
        assert_eq!(s.locks_root(), Path::new("/index/locks"));
        let redirected = IndexStore::new("/shipped/index").with_locks_root("/home/locks");
        assert_eq!(redirected.locks_root(), Path::new("/home/locks"));
    }

    #[test]
    fn root_document_path_matches_static_index_py_layout() {
        let s = IndexStore::new("/index");
        let p = s.root_document_path("ocx.sh", "kitware/cmake");
        let expected = Path::new("/index")
            .join("ocx.sh")
            .join("p")
            .join("kitware")
            .join("cmake.json");
        assert_eq!(
            p, expected,
            "must mirror static_index.py's p/<repository>.json root file"
        );
    }

    #[test]
    fn dispatch_object_path_matches_static_index_py_layout() {
        let s = IndexStore::new("/index");
        let digest = Digest::Sha256(SHA256_HEX.to_string());
        let p = s.dispatch_object_path("ocx.sh", "kitware/cmake", &digest);
        let expected = Path::new("/index")
            .join("ocx.sh")
            .join("p")
            .join("kitware")
            .join("cmake")
            .join("o")
            .join("sha256")
            .join(format!("{SHA256_HEX}.json"));
        assert_eq!(
            p, expected,
            "must mirror static_index.py's p/<repository>/o/sha256/<hex>.json, with .json present"
        );
    }

    #[test]
    fn dispatch_object_path_supports_non_sha256_algorithm() {
        let s = IndexStore::new("/index");
        let digest = Digest::Sha512(SHA512_HEX.to_string());
        let p = s.dispatch_object_path("ocx.sh", "kitware/cmake", &digest);
        let expected = Path::new("/index")
            .join("ocx.sh")
            .join("p")
            .join("kitware")
            .join("cmake")
            .join("o")
            .join("sha512")
            .join(format!("{SHA512_HEX}.json"));
        assert_eq!(
            p, expected,
            "dispatch object path must key off the digest's own algorithm, not hardcode sha256"
        );
    }

    #[test]
    fn root_document_is_sibling_of_dispatch_package_directory() {
        let s = IndexStore::new("/index");
        let digest = Digest::Sha256(SHA256_HEX.to_string());
        let root = s.root_document_path("ocx.sh", "kitware/cmake");
        let dispatch = s.dispatch_object_path("ocx.sh", "kitware/cmake", &digest);

        // dispatch = .../p/kitware/cmake/o/sha256/<hex>.json — strip
        // <hex>.json, sha256/, o/ to land on the package directory.
        let package_dir = dispatch
            .ancestors()
            .nth(3)
            .expect("dispatch path has a p/<ns>/<pkg> ancestor");
        assert_eq!(
            package_dir.file_name(),
            Some(OsStr::new("cmake")),
            "the dispatch package directory must be named for the package"
        );
        assert_eq!(
            package_dir.parent(),
            root.parent(),
            "root document (p/kitware/cmake.json) and the dispatch package directory \
             (p/kitware/cmake/) must be SIBLINGS, never one nested inside the other"
        );
        assert_ne!(
            package_dir, root,
            "the package directory must not collide with the root document's own path"
        );
    }

    #[test]
    fn wire_paths_slugify_the_source_segment() {
        let s = IndexStore::new("/index");
        assert_eq!(
            s.source_config_path("localhost:5000"),
            Path::new("/index/localhost_5000/config.json")
        );
    }

    // ── 1b. Containment: a traversing repository is refused, nothing escapes ──
    //
    // Defense-in-depth (CWE-22) behind `LocalIndex::sync_catalog`'s catalog-key
    // boundary validation: a repository reaching a wire-grammar path builder is
    // split on `/` verbatim (`repository_path`), so `..`, an absolute segment,
    // or a Windows/backslash escape would join outside the source subtree. Every
    // repository-keyed read/write refuses such a value as a `DataError` and
    // touches nothing on disk.

    /// The set of traversing repositories a hostile catalog key could smuggle
    /// to the store: leading `..`, bare `..`, an absolute path, and a
    /// Windows-style backslash escape (rejected host-independently). `a/../b`
    /// resolves WITHIN the root, so it is intentionally NOT in this set — the
    /// containment invariant is "stays under the source root", not "contains no
    /// `..` component"; the sync-catalog boundary (which parses full identifier
    /// grammar) is the layer that rejects an internal `..`.
    const TRAVERSING_REPOSITORIES: &[&str] = &["../../victim", "..", "/tmp/victim", "..\\victim"];

    #[tokio::test(flavor = "multi_thread")]
    async fn wire_writes_refuse_a_traversing_repository_and_write_nothing() {
        let outside = tempfile::tempdir().unwrap();
        // Sentinel a sibling of the store root; a successful escape from the
        // store (rooted at `outside/index`) could reach it, so its survival is
        // proof nothing was written outside the source subtree.
        let sentinel = outside.path().join("victim.json");
        std::fs::write(&sentinel, b"original").unwrap();
        let store_root = outside.path().join("index");
        let s = store(&store_root);
        let digest = crate::oci::Algorithm::Sha256.hash(b"payload");

        for repository in TRAVERSING_REPOSITORIES {
            let write_dispatch = s.write_dispatch_object("ocx.sh", repository, &digest, b"payload").await;
            assert!(
                matches!(
                    write_dispatch,
                    Err(crate::Error::FileStructure(
                        super::super::error::Error::RepositoryEscapesIndexHome { .. }
                    ))
                ),
                "write_dispatch_object({repository:?}) must be refused as RepositoryEscapesIndexHome, got {write_dispatch:?}"
            );
            assert_eq!(
                write_dispatch.unwrap_err().classify(),
                Some(ExitCode::DataError),
                "a traversal refusal must classify as DataError"
            );

            let write_root = s.write_root_document("ocx.sh", repository, b"{}").await;
            assert!(
                matches!(
                    write_root,
                    Err(crate::Error::FileStructure(
                        super::super::error::Error::RepositoryEscapesIndexHome { .. }
                    ))
                ),
                "write_root_document({repository:?}) must be refused, got {write_root:?}"
            );
        }

        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"original",
            "a refused write must never touch a file outside the source subtree"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wire_reads_refuse_a_traversing_repository() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let digest = Digest::Sha256(SHA256_HEX.to_string());
        let no_check = |_root: &IndexRoot| Ok(());

        for repository in TRAVERSING_REPOSITORIES {
            let read_root = s.read_root("ocx.sh", repository, no_check).await;
            assert!(
                matches!(
                    read_root,
                    Err(crate::Error::FileStructure(
                        super::super::error::Error::RepositoryEscapesIndexHome { .. }
                    ))
                ),
                "read_root({repository:?}) must be refused, got {read_root:?}"
            );

            let read_dispatch = s.read_dispatch_object("ocx.sh", repository, &digest).await;
            assert!(
                matches!(
                    read_dispatch,
                    Err(crate::Error::FileStructure(
                        super::super::error::Error::RepositoryEscapesIndexHome { .. }
                    ))
                ),
                "read_dispatch_object({repository:?}) must be refused, got {read_dispatch:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wire_paths_accept_a_normal_nested_repository() {
        // The guard must not over-reject a legitimate `<ns>/<pkg>` repository.
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let payload = b"{\"platforms\":[]}";
        let digest = crate::oci::Algorithm::Sha256.hash(payload);
        s.write_dispatch_object("ocx.sh", "kitware/cmake", &digest, payload)
            .await
            .expect("a normal nested repository must be accepted");
    }

    // ── 1c. Containment: a traversing SOURCE is refused, nothing escapes (S2) ─
    //
    // [`IndexStore::wire_source_dir`] joins [`super::slugify`]'s output as a
    // SINGLE path segment. Slugify neutralizes an embedded `/` (replaced with
    // `_`), but preserves `.` — a bare `".."` source slugifies to `".."`
    // verbatim, which would resolve `wire_source_dir` to the index home's
    // PARENT directory if unguarded.

    #[tokio::test(flavor = "multi_thread")]
    async fn wire_writes_refuse_a_traversing_source_and_write_nothing() {
        let outside = tempfile::tempdir().unwrap();
        // Sentinel a sibling of the store root; a successful escape from the
        // store (rooted at `outside/index`) could reach it, so its survival is
        // proof nothing was written outside the index home.
        let sentinel = outside.path().join("victim.json");
        std::fs::write(&sentinel, b"original").unwrap();
        let store_root = outside.path().join("index");
        let s = store(&store_root);
        let digest = crate::oci::Algorithm::Sha256.hash(b"payload");

        let write_dispatch = s
            .write_dispatch_object("..", "kitware/cmake", &digest, b"payload")
            .await;
        assert!(
            matches!(
                write_dispatch,
                Err(crate::Error::FileStructure(
                    super::super::error::Error::RepositoryEscapesIndexHome { .. }
                ))
            ),
            "write_dispatch_object with a traversing source must be refused, got {write_dispatch:?}"
        );

        let write_root = s.write_root_document("..", "kitware/cmake", b"{}").await;
        assert!(
            matches!(
                write_root,
                Err(crate::Error::FileStructure(
                    super::super::error::Error::RepositoryEscapesIndexHome { .. }
                ))
            ),
            "write_root_document with a traversing source must be refused, got {write_root:?}"
        );

        let lock = s
            .lock_source("index-catalog", "..", "c/index.json", SOURCE_LOCK_TIMEOUT)
            .await;
        assert!(
            matches!(
                lock,
                Err(crate::Error::FileStructure(
                    super::super::error::Error::RepositoryEscapesIndexHome { .. }
                ))
            ),
            "lock_source with a traversing source must be refused, got {lock:?}"
        );

        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"original",
            "a refused write must never touch a file outside the index home"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wire_reads_refuse_a_traversing_source() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let digest = Digest::Sha256(SHA256_HEX.to_string());
        let no_check = |_root: &IndexRoot| Ok(());

        let read_root = s.read_root("..", "kitware/cmake", no_check).await;
        assert!(
            matches!(
                read_root,
                Err(crate::Error::FileStructure(
                    super::super::error::Error::RepositoryEscapesIndexHome { .. }
                ))
            ),
            "read_root with a traversing source must be refused, got {read_root:?}"
        );

        let read_dispatch = s.read_dispatch_object("..", "kitware/cmake", &digest).await;
        assert!(
            matches!(
                read_dispatch,
                Err(crate::Error::FileStructure(
                    super::super::error::Error::RepositoryEscapesIndexHome { .. }
                ))
            ),
            "read_dispatch_object with a traversing source must be refused, got {read_dispatch:?}"
        );

        let list = s.list_wire_repositories("..").await;
        assert!(
            matches!(
                list,
                Err(crate::Error::FileStructure(
                    super::super::error::Error::RepositoryEscapesIndexHome { .. }
                ))
            ),
            "list_wire_repositories with a traversing source must be refused, got {list:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wire_paths_accept_a_normal_source() {
        // The guard must not over-reject a legitimate source name, including
        // one slugify rewrites (a colon-bearing host:port).
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let payload = b"{\"platforms\":[]}";
        let digest = crate::oci::Algorithm::Sha256.hash(payload);
        s.write_dispatch_object("localhost:5000", "kitware/cmake", &digest, payload)
            .await
            .expect("a normal source name must be accepted");
    }

    // ── 2. Dispatch-object verify contract ───────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn write_dispatch_object_rejects_digest_mismatch_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let wrong_digest = Digest::Sha256(SHA256_HEX.to_string());
        let payload = b"these bytes do not hash to SHA256_HEX";

        let result = s
            .write_dispatch_object("ocx.sh", "kitware/cmake", &wrong_digest, payload)
            .await;
        assert!(
            matches!(
                result,
                Err(crate::Error::FileStructure(
                    super::super::error::Error::DigestMismatch { .. }
                ))
            ),
            "expected DigestMismatch, got {result:?}"
        );
        assert!(
            !tokio::fs::try_exists(s.dispatch_object_path("ocx.sh", "kitware/cmake", &wrong_digest))
                .await
                .unwrap(),
            "a rejected write must leave nothing on disk"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_then_read_dispatch_object_round_trips_verbatim_bytes_at_dot_json_path() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let payload =
            b"{\"platforms\":[{\"platform\":{\"os\":\"linux\",\"architecture\":\"amd64\"},\"digest\":\"sha256:aa\"}]}";
        let digest = crate::oci::Algorithm::Sha256.hash(payload);

        s.write_dispatch_object("ocx.sh", "kitware/cmake", &digest, payload)
            .await
            .unwrap();

        let on_disk_path = s.dispatch_object_path("ocx.sh", "kitware/cmake", &digest);
        let on_disk_bytes = tokio::fs::read(&on_disk_path).await.unwrap();
        assert_eq!(
            on_disk_bytes, payload,
            "dispatch object bytes must land exactly at dispatch_object_path, unmodified"
        );
        assert!(
            on_disk_path.to_string_lossy().ends_with(".json"),
            "dispatch object filename must carry the .json extension (A2)"
        );

        let read_back = s
            .read_dispatch_object("ocx.sh", "kitware/cmake", &digest)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            read_back, payload,
            "read_dispatch_object must return byte-identical content"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_dispatch_object_is_idempotent_on_identical_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let payload = b"identical content";
        let digest = crate::oci::Algorithm::Sha256.hash(payload);

        s.write_dispatch_object("ocx.sh", "kitware/cmake", &digest, payload)
            .await
            .unwrap();
        // Second write with the same digest+bytes must succeed without error.
        s.write_dispatch_object("ocx.sh", "kitware/cmake", &digest, payload)
            .await
            .unwrap();

        let read_back = s
            .read_dispatch_object("ocx.sh", "kitware/cmake", &digest)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read_back, payload);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_dispatch_object_self_heals_a_tampered_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let payload = b"correct dispatch bytes";
        let digest = crate::oci::Algorithm::Sha256.hash(payload);

        let path = s.dispatch_object_path("ocx.sh", "kitware/cmake", &digest);
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&path, b"tampered garbage").await.unwrap();

        s.write_dispatch_object("ocx.sh", "kitware/cmake", &digest, payload)
            .await
            .unwrap();

        let read_back = tokio::fs::read(&path).await.unwrap();
        assert_eq!(
            read_back, payload,
            "write_dispatch_object must self-heal a tampered on-disk file"
        );
    }

    /// A self-heal whose atomic publish FAILS must NOT report success while the
    /// known-corrupt object stays on disk (CWE-345). The target is pre-seeded as
    /// a directory: `tokio::fs::read` fails the verify short-circuit (so the
    /// self-heal write path is taken), and renaming the tempfile over an
    /// existing directory fails deterministically — the exact
    /// persist-failure-with-target-present shape the recovery arm guards. The
    /// old `Err(_) if target.exists() => Ok(())` arm reported this as success;
    /// the re-read-and-re-hash arm must surface it as an error.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn write_dispatch_object_propagates_a_failed_self_heal_persist() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let payload = b"correct dispatch bytes for the failed-heal case";
        let digest = crate::oci::Algorithm::Sha256.hash(payload);

        // Pre-seed the exact target path as a NON-EMPTY directory: it "exists"
        // yet holds none of the claimed content, and a file→dir rename fails.
        let path = s.dispatch_object_path("ocx.sh", "kitware/cmake", &digest);
        tokio::fs::create_dir_all(&path).await.unwrap();
        tokio::fs::write(path.join("occupant"), b"blocks the rename")
            .await
            .unwrap();

        let result = s
            .write_dispatch_object("ocx.sh", "kitware/cmake", &digest, payload)
            .await;
        assert!(
            result.is_err(),
            "a failed self-heal persist must propagate an error, not report success; got {result:?}"
        );
        assert!(
            path.is_dir(),
            "the known-corrupt target must be left untouched (never silently 'healed')"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_dispatch_object_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let digest = Digest::Sha256(SHA256_HEX.to_string());
        let result = s
            .read_dispatch_object("ocx.sh", "kitware/cmake", &digest)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_dispatch_object_detects_tampering_as_data_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let payload = b"original dispatch bytes";
        let digest = crate::oci::Algorithm::Sha256.hash(payload);

        s.write_dispatch_object("ocx.sh", "kitware/cmake", &digest, payload)
            .await
            .unwrap();
        let path = s.dispatch_object_path("ocx.sh", "kitware/cmake", &digest);
        tokio::fs::write(&path, b"tampered after write").await.unwrap();

        let result = s.read_dispatch_object("ocx.sh", "kitware/cmake", &digest).await;
        let err = result.expect_err("tampered dispatch object must fail to read, never silently load");
        assert!(
            matches!(
                err,
                crate::Error::FileStructure(super::super::error::Error::DigestMismatch { .. })
            ),
            "expected DigestMismatch, got {err:?}"
        );
        assert_eq!(
            err.classify(),
            Some(ExitCode::DataError),
            "tampering must classify as DataError"
        );
    }

    // ── 3. Root-doc write + derivable catalog entry (F1) ────────────────

    #[test]
    fn root_catalog_entry_is_sha256_of_the_bytes() {
        let bytes = b"{\"repository\":\"oci://ghcr.io/kitware/cmake\"}";
        let expected = crate::oci::Algorithm::Sha256.hash(bytes).to_string();
        assert_eq!(IndexStore::root_catalog_entry(bytes), expected);
    }

    #[test]
    fn root_catalog_entry_changes_with_the_bytes() {
        let a = IndexStore::root_catalog_entry(b"{\"a\":1}");
        let b = IndexStore::root_catalog_entry(b"{\"a\":2}");
        assert_ne!(a, b, "different bytes must derive different catalog entries");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_root_then_commit_persists_verbatim_bytes_and_catalog_entry() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let root_bytes = minimal_root_bytes("oci://ghcr.io/kitware/cmake", &Digest::Sha256(SHA256_HEX.to_string()));

        let mut txn = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        txn.write_root("kitware/cmake", &root_bytes, |root: &IndexRoot| {
            assert_eq!(root.repository, "oci://ghcr.io/kitware/cmake");
            Ok(())
        })
        .await
        .unwrap();
        txn.commit(None).await.unwrap();

        let on_disk_root = tokio::fs::read(s.root_document_path("ocx.sh", "kitware/cmake"))
            .await
            .unwrap();
        assert_eq!(on_disk_root, root_bytes, "root document bytes must land verbatim");

        let catalog: CatalogIndex =
            serde_json::from_slice(&tokio::fs::read(s.source_catalog_path("ocx.sh")).await.unwrap()).unwrap();
        assert_eq!(
            catalog.get("kitware/cmake"),
            Some(&IndexStore::root_catalog_entry(&root_bytes)),
            "catalog entry must be exactly sha256(root bytes), nothing more"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_order_dispatch_then_root_then_catalog_all_cohere_after_commit() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let obs_payload =
            b"{\"platforms\":[{\"platform\":{\"os\":\"linux\",\"architecture\":\"amd64\"},\"digest\":\"sha256:aa\"}]}";
        let obs_digest = crate::oci::Algorithm::Sha256.hash(obs_payload);

        // Step 1 (F1): dispatch object into o/ first.
        s.write_dispatch_object("ocx.sh", "kitware/cmake", &obs_digest, obs_payload)
            .await
            .unwrap();

        let root_bytes = minimal_root_bytes("oci://ghcr.io/kitware/cmake", &obs_digest);

        // Steps 2+3 (F1): root, then the catalog entry, via one transaction.
        let mut txn = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        txn.write_root("kitware/cmake", &root_bytes, |_| Ok(())).await.unwrap();
        txn.commit(None).await.unwrap();

        assert!(
            tokio::fs::try_exists(s.dispatch_object_path("ocx.sh", "kitware/cmake", &obs_digest))
                .await
                .unwrap(),
            "dispatch object must exist after the write-order sequence"
        );
        assert!(
            tokio::fs::try_exists(s.root_document_path("ocx.sh", "kitware/cmake"))
                .await
                .unwrap(),
            "root document must exist after commit"
        );
        let catalog: CatalogIndex =
            serde_json::from_slice(&tokio::fs::read(s.source_catalog_path("ocx.sh")).await.unwrap()).unwrap();
        assert_eq!(
            catalog.get("kitware/cmake"),
            Some(&IndexStore::root_catalog_entry(&root_bytes)),
            "all three writes must cohere: catalog entry derives from the committed root bytes"
        );
    }

    // ── 4. Crash-recovery (F1) ────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn read_root_recovers_when_catalog_entry_absent() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let root_bytes = minimal_root_bytes("oci://ghcr.io/kitware/cmake", &Digest::Sha256(SHA256_HEX.to_string()));

        // Simulate a crash between step 2 (root written) and step 3 (catalog
        // entry upserted): write the root directly, bypassing the transaction.
        let root_path = s.root_document_path("ocx.sh", "kitware/cmake");
        tokio::fs::create_dir_all(root_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&root_path, &root_bytes).await.unwrap();

        let result = s
            .read_root("ocx.sh", "kitware/cmake", |_| Ok(()))
            .await
            .unwrap()
            .expect("root document exists on disk");

        assert_eq!(result.bytes, root_bytes);
        let expected_entry = IndexStore::root_catalog_entry(&root_bytes);
        assert_eq!(
            result.catalog_status,
            CatalogEntryStatus::Recovered {
                entry: expected_entry.clone()
            },
            "an absent catalog entry must self-heal, never hard-fail"
        );

        let catalog: CatalogIndex =
            serde_json::from_slice(&tokio::fs::read(s.source_catalog_path("ocx.sh")).await.unwrap()).unwrap();
        assert_eq!(
            catalog.get("kitware/cmake"),
            Some(&expected_entry),
            "the catalog on disk must now carry the re-derived entry"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_root_recovers_when_catalog_entry_stale() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let root_bytes = minimal_root_bytes("oci://ghcr.io/kitware/cmake", &Digest::Sha256(SHA256_HEX.to_string()));

        let root_path = s.root_document_path("ocx.sh", "kitware/cmake");
        tokio::fs::create_dir_all(root_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&root_path, &root_bytes).await.unwrap();

        // A STALE catalog entry, written directly — simulates a prior
        // version's entry left behind by an interrupted re-snapshot.
        let mut stale_catalog = CatalogIndex::new();
        stale_catalog.insert(
            "kitware/cmake".to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        );
        let catalog_path = s.source_catalog_path("ocx.sh");
        tokio::fs::create_dir_all(catalog_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&catalog_path, serde_json::to_vec(&stale_catalog).unwrap())
            .await
            .unwrap();

        let result = s
            .read_root("ocx.sh", "kitware/cmake", |_| Ok(()))
            .await
            .unwrap()
            .expect("root document exists on disk");

        let expected_entry = IndexStore::root_catalog_entry(&root_bytes);
        assert_eq!(
            result.catalog_status,
            CatalogEntryStatus::Recovered {
                entry: expected_entry.clone()
            },
            "a stale (mismatched) catalog entry must self-heal by re-derivation, never hard-fail"
        );
        let catalog: CatalogIndex = serde_json::from_slice(&tokio::fs::read(&catalog_path).await.unwrap()).unwrap();
        assert_eq!(
            catalog.get("kitware/cmake"),
            Some(&expected_entry),
            "the catalog must be rewritten to the derived entry, replacing the stale one"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_root_hard_fails_on_unparseable_root_document() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let root_path = s.root_document_path("ocx.sh", "kitware/cmake");
        tokio::fs::create_dir_all(root_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&root_path, b"not valid json {").await.unwrap();

        let result = s.read_root("ocx.sh", "kitware/cmake", |_| Ok(())).await;
        let err = result.expect_err("an unparseable root document must hard-fail, never recover");
        assert!(
            matches!(
                err,
                crate::Error::FileStructure(super::super::error::Error::MalformedRootDocument { .. })
            ),
            "expected MalformedRootDocument, got {err:?}"
        );
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_root_hard_fails_on_malformed_tag_content_digest() {
        // `RootTag::content`'s `oci::Digest` deserialize is exact-wire
        // (`adr_index_indirection.md` amendment 2026-07-19) — well-formed
        // JSON carrying an invalid digest value fails the same way as
        // unparseable JSON: the whole root document fails to deserialize,
        // never a partial parse that drops just the bad tag.
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let root_path = s.root_document_path("ocx.sh", "kitware/cmake");
        tokio::fs::create_dir_all(root_path.parent().unwrap()).await.unwrap();
        let root_bytes = serde_json::json!({
            "repository": "oci://ghcr.io/kitware/cmake",
            "tags": {
                "latest": { "content": "not-a-digest", "observed": "2026-07-18T09:00:00Z" }
            }
        })
        .to_string()
        .into_bytes();
        tokio::fs::write(&root_path, &root_bytes).await.unwrap();

        let result = s.read_root("ocx.sh", "kitware/cmake", |_| Ok(())).await;
        let err = result.expect_err("a malformed tag content digest must hard-fail, never recover");
        assert!(
            matches!(
                err,
                crate::Error::FileStructure(super::super::error::Error::MalformedRootDocument { .. })
            ),
            "expected MalformedRootDocument, got {err:?}"
        );
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_root_propagates_repository_check_failure_as_data_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let root_bytes = minimal_root_bytes(
            "not-a-valid-scheme://ghcr.io/kitware/cmake",
            &Digest::Sha256(SHA256_HEX.to_string()),
        );
        let root_path = s.root_document_path("ocx.sh", "kitware/cmake");
        tokio::fs::create_dir_all(root_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&root_path, &root_bytes).await.unwrap();

        // C3 cross-check hook: the caller (not this grammar-agnostic store)
        // owns interpreting `repository` and hard-fails a genuinely malformed
        // physical reference. This test stands in with an existing
        // DataError-classified variant (`DigestMismatch`) since the concrete
        // repository-scheme error type belongs to `oci/index`, not this store.
        let result = s
            .read_root("ocx.sh", "kitware/cmake", |root: &IndexRoot| {
                assert!(root.repository.starts_with("not-a-valid-scheme://"));
                Err(super::super::error::Error::DigestMismatch {
                    claimed: Digest::Sha256(SHA256_HEX.to_string()),
                    computed: Digest::Sha256("0".repeat(64)),
                }
                .into())
            })
            .await;

        let err = result.expect_err("a repository_check rejection must propagate, never be swallowed");
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_root_recovery_never_clobbers_a_fresher_concurrently_committed_entry() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        // v1 on disk, no catalog entry yet — `read_root` will want to self-heal.
        let root_bytes_v1 = minimal_root_bytes("oci://ghcr.io/kitware/cmake", &Digest::Sha256(SHA256_HEX.to_string()));
        let root_path = s.root_document_path("ocx.sh", "kitware/cmake");
        tokio::fs::create_dir_all(root_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&root_path, &root_bytes_v1).await.unwrap();

        let root_bytes_v2 = minimal_root_bytes("oci://ghcr.io/kitware/cmake", &Digest::Sha256("1".repeat(64)));
        let entry_v2 = IndexStore::root_catalog_entry(&root_bytes_v2);

        // Writer: acquires the transaction lock FIRST (uncontended), then —
        // while still holding it — commits a FRESHER root (v2) and its
        // matching catalog entry. Real lock contention (not sleep-timing)
        // forces the reader's `begin_catalog_transaction` below to block
        // until this writer releases, giving a deterministic interleave.
        let s_writer = s.clone();
        let root_path_writer = root_path.clone();
        let root_bytes_v2_writer = root_bytes_v2.clone();
        let entry_v2_writer = entry_v2.clone();
        let writer = tokio::spawn(async move {
            let mut transaction = s_writer.begin_catalog_transaction("ocx.sh").await.unwrap();
            // Give the reader time to perform its pre-lock read (sees v1)
            // and then block on `begin_catalog_transaction` behind this lock.
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            tokio::fs::write(&root_path_writer, &root_bytes_v2_writer)
                .await
                .unwrap();
            transaction
                .catalog()
                .insert("kitware/cmake".to_string(), entry_v2_writer);
            transaction.commit(None).await.unwrap();
        });

        // Reader: head start so it reads v1 pre-lock, before the writer's
        // 60ms sleep elapses and overwrites the root to v2.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let result = s
            .read_root("ocx.sh", "kitware/cmake", |_| Ok(()))
            .await
            .unwrap()
            .expect("root document exists on disk");

        writer.await.unwrap();

        assert_eq!(
            result.bytes, root_bytes_v2,
            "recovery must return the freshest on-disk root bytes, not the pre-lock read"
        );
        assert_eq!(
            result.catalog_status,
            CatalogEntryStatus::Recovered {
                entry: entry_v2.clone()
            },
            "recovery must derive the entry from the freshest root bytes"
        );

        let catalog: CatalogIndex =
            serde_json::from_slice(&tokio::fs::read(s.source_catalog_path("ocx.sh")).await.unwrap()).unwrap();
        assert_eq!(
            catalog.get("kitware/cmake"),
            Some(&entry_v2),
            "the writer's fresher entry must survive — the reader's recovery must never clobber it back to stale"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_root_recovery_write_failure_still_resolves_the_root() {
        // A read-only shipped/`--index` index home (or any unwritable recovery
        // target) must NOT fail an online resolve: the re-derived entry is
        // authoritative for THIS read and the persist is best-effort. A
        // `locks_root` whose ancestor is a regular FILE makes the recovery's
        // `begin_catalog_transaction` fail deterministically for every user
        // (root included) — standing in for any unwritable recovery target.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let s = IndexStore::new(dir.path().join("home")).with_locks_root(blocker.join("locks"));

        // Root present, NO catalog entry → the straddle-recovery branch fires.
        let root_bytes = minimal_root_bytes("oci://ghcr.io/kitware/cmake", &Digest::Sha256(SHA256_HEX.to_string()));
        let root_path = s.root_document_path("ocx.sh", "kitware/cmake");
        tokio::fs::create_dir_all(root_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&root_path, &root_bytes).await.unwrap();

        let result = s
            .read_root("ocx.sh", "kitware/cmake", |_| Ok(()))
            .await
            .expect("a failed recovery write must be swallowed, never propagated")
            .expect("root document exists on disk");

        assert_eq!(
            result.bytes, root_bytes,
            "the read must still resolve the root bytes when recovery cannot persist"
        );
        assert_eq!(
            result.catalog_status,
            CatalogEntryStatus::Recovered {
                entry: IndexStore::root_catalog_entry(&root_bytes)
            },
            "the in-memory re-derived entry is used for this read even when the write is skipped"
        );
        assert!(
            !s.source_catalog_path("ocx.sh").exists(),
            "a best-effort recovery that could not persist must leave no catalog on disk"
        );
    }

    // ── 5. Catalog-transaction concurrency contract ──────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn sequential_transactions_for_different_packages_both_survive() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        let mut txn_a = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        txn_a
            .catalog()
            .insert("kitware/cmake".to_string(), "sha256:aaa".to_string());
        txn_a.commit(None).await.unwrap();

        let mut txn_b = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        txn_b
            .catalog()
            .insert("stable/tool".to_string(), "sha256:bbb".to_string());
        txn_b.commit(None).await.unwrap();

        let catalog: CatalogIndex =
            serde_json::from_slice(&tokio::fs::read(s.source_catalog_path("ocx.sh")).await.unwrap()).unwrap();
        assert_eq!(catalog.get("kitware/cmake"), Some(&"sha256:aaa".to_string()));
        assert_eq!(catalog.get("stable/tool"), Some(&"sha256:bbb".to_string()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn transaction_reread_reconciles_against_freshly_committed_entry() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        // "Txn B" commits an entry first.
        let mut txn_b = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        txn_b
            .catalog()
            .insert("kitware/cmake".to_string(), "sha256:committed-by-b".to_string());
        txn_b.commit(None).await.unwrap();

        // "Txn A" begins AFTER — begin_catalog_transaction's contract is
        // "re-read the on-disk catalog before handing it to the caller", so
        // the map txn_a actually mutates must already reflect B's committed
        // entry, never a caller-held stale pre-lock snapshot.
        let mut txn_a = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        assert_eq!(
            txn_a.catalog().get("kitware/cmake"),
            Some(&"sha256:committed-by-b".to_string()),
            "a freshly begun transaction must see the freshest on-disk catalog"
        );
        txn_a
            .catalog()
            .insert("stable/tool".to_string(), "sha256:added-by-a".to_string());
        txn_a.commit(None).await.unwrap();

        let catalog: CatalogIndex =
            serde_json::from_slice(&tokio::fs::read(s.source_catalog_path("ocx.sh")).await.unwrap()).unwrap();
        assert_eq!(
            catalog.get("kitware/cmake"),
            Some(&"sha256:committed-by-b".to_string()),
            "B's entry must survive A's later commit"
        );
        assert_eq!(catalog.get("stable/tool"), Some(&"sha256:added-by-a".to_string()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_transactions_serialize_through_the_source_lock_and_both_survive() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        let s_a = s.clone();
        let task_a = tokio::spawn(async move {
            let mut txn = s_a.begin_catalog_transaction("ocx.sh").await.unwrap();
            // Hold the lock briefly so task_b's `begin_catalog_transaction`
            // genuinely blocks behind this one, forcing it to observe this
            // commit on its own re-read.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            txn.catalog()
                .insert("kitware/cmake".to_string(), "sha256:from-a".to_string());
            txn.commit(None).await.unwrap();
        });
        // Give task_a a head start so it opens the transaction first.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let s_b = s.clone();
        let task_b = tokio::spawn(async move {
            let mut txn = s_b.begin_catalog_transaction("ocx.sh").await.unwrap();
            txn.catalog()
                .insert("stable/tool".to_string(), "sha256:from-b".to_string());
            txn.commit(None).await.unwrap();
        });

        task_a.await.unwrap();
        task_b.await.unwrap();

        let catalog: CatalogIndex =
            serde_json::from_slice(&tokio::fs::read(s.source_catalog_path("ocx.sh")).await.unwrap()).unwrap();
        assert_eq!(
            catalog.get("kitware/cmake"),
            Some(&"sha256:from-a".to_string()),
            "concurrent catalog writers must not lose each other's entries"
        );
        assert_eq!(catalog.get("stable/tool"), Some(&"sha256:from-b".to_string()));
    }

    // ── 6. Etag coherence ─────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_with_etag_writes_catalog_and_etag_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        let mut txn = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        txn.catalog()
            .insert("kitware/cmake".to_string(), "sha256:aaa".to_string());
        txn.commit(Some("\"abc123\"")).await.unwrap();

        let etag = tokio::fs::read_to_string(s.source_catalog_etag_path("ocx.sh"))
            .await
            .unwrap();
        assert_eq!(etag.trim(), "\"abc123\"");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_without_etag_leaves_etag_absent() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        let mut txn = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        txn.catalog()
            .insert("kitware/cmake".to_string(), "sha256:aaa".to_string());
        txn.commit(None).await.unwrap();

        assert!(
            !tokio::fs::try_exists(s.source_catalog_etag_path("ocx.sh"))
                .await
                .unwrap(),
            "committing without an etag must not create the sidecar"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_without_etag_leaves_a_previously_set_etag_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        let mut first = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        first
            .catalog()
            .insert("kitware/cmake".to_string(), "sha256:aaa".to_string());
        first.commit(Some("\"etag-1\"")).await.unwrap();

        let mut second = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        second
            .catalog()
            .insert("stable/tool".to_string(), "sha256:bbb".to_string());
        second.commit(None).await.unwrap();

        let etag = tokio::fs::read_to_string(s.source_catalog_etag_path("ocx.sh"))
            .await
            .unwrap();
        assert_eq!(
            etag.trim(),
            "\"etag-1\"",
            "a commit without an etag must not clobber a previously published etag"
        );
    }

    // ── 7. Per-source isolation ───────────────────────────────────────────

    #[test]
    fn two_sources_have_disjoint_wire_paths() {
        let s = IndexStore::new("/index");
        assert_ne!(s.source_config_path("ocx.sh"), s.source_config_path("ghcr.io"));
        assert_ne!(s.source_catalog_path("ocx.sh"), s.source_catalog_path("ghcr.io"));
        let digest = Digest::Sha256(SHA256_HEX.to_string());
        assert_ne!(
            s.root_document_path("ocx.sh", "kitware/cmake"),
            s.root_document_path("ghcr.io", "kitware/cmake")
        );
        assert_ne!(
            s.dispatch_object_path("ocx.sh", "kitware/cmake", &digest),
            s.dispatch_object_path("ghcr.io", "kitware/cmake", &digest)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_entries_do_not_leak_across_sources() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        let mut txn_a = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        txn_a
            .catalog()
            .insert("kitware/cmake".to_string(), "sha256:aaa".to_string());
        txn_a.commit(None).await.unwrap();

        let mut txn_b = s.begin_catalog_transaction("ghcr.io").await.unwrap();
        assert!(
            txn_b.catalog().is_empty(),
            "source ghcr.io's catalog transaction must never see source ocx.sh's entries"
        );
        drop(txn_b);

        assert!(
            !tokio::fs::try_exists(s.source_catalog_path("ghcr.io")).await.unwrap(),
            "writing ocx.sh's catalog must not create a ghcr.io catalog file"
        );
    }

    // ── 8. read_root_uncatalogued: derived (catalog-free) root read (A2/H) ─
    //
    // A DERIVED (OCX-authored) source has no c/index.json — its catalog is the
    // directory enumeration of p/ — so a derived root read must carry
    // `NoCatalog`, run the caller's C3 `repository_check`, and NEVER materialize
    // a catalog or lock. read_root_uncatalogued is implemented (WP-A/C1 additive
    // sibling of read_root), so these pass now as regression coverage.

    #[tokio::test(flavor = "multi_thread")]
    async fn read_root_uncatalogued_reads_verbatim_and_never_creates_a_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let root_bytes = minimal_root_bytes("oci://ghcr.io/kitware/cmake", &Digest::Sha256(SHA256_HEX.to_string()));
        let root_path = s.root_document_path("ghcr.io", "kitware/cmake");
        tokio::fs::create_dir_all(root_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&root_path, &root_bytes).await.unwrap();

        let result = s
            .read_root_uncatalogued("ghcr.io", "kitware/cmake", |_| Ok(()))
            .await
            .unwrap()
            .expect("root document exists on disk");

        assert_eq!(
            result.bytes, root_bytes,
            "the derived read returns the verbatim root bytes"
        );
        assert_eq!(result.root.repository, "oci://ghcr.io/kitware/cmake");
        assert_eq!(
            result.catalog_status,
            CatalogEntryStatus::NoCatalog,
            "a derived (OCI-authored) source read carries NoCatalog — there is no catalog to cross-check"
        );
        assert!(
            !tokio::fs::try_exists(s.source_catalog_path("ghcr.io")).await.unwrap(),
            "a derived root read must never materialize c/index.json"
        );
        assert!(
            !tokio::fs::try_exists(s.locks_root()).await.unwrap(),
            "a derived root read must never take (or create) a source lock"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_root_uncatalogued_returns_none_when_root_absent() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let result = s
            .read_root_uncatalogued("ghcr.io", "kitware/cmake", |_| Ok(()))
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "an absent derived root is a clean miss, never an error"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_root_uncatalogued_propagates_repository_check_failure() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let root_bytes = minimal_root_bytes("oci://ghcr.io/kitware/cmake", &Digest::Sha256(SHA256_HEX.to_string()));
        let root_path = s.root_document_path("ghcr.io", "kitware/cmake");
        tokio::fs::create_dir_all(root_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&root_path, &root_bytes).await.unwrap();

        // The C3 cross-check hook is honored even on the catalog-free path; its
        // rejection propagates as a data error, never swallowed.
        let result = s
            .read_root_uncatalogued("ghcr.io", "kitware/cmake", |root: &IndexRoot| {
                assert_eq!(root.repository, "oci://ghcr.io/kitware/cmake");
                Err(super::super::error::Error::DigestMismatch {
                    claimed: Digest::Sha256(SHA256_HEX.to_string()),
                    computed: Digest::Sha256("0".repeat(64)),
                }
                .into())
            })
            .await;
        let err = result.expect_err("a repository_check rejection must propagate on the derived path too");
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_root_uncatalogued_hard_fails_on_unparseable_root() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let root_path = s.root_document_path("ghcr.io", "kitware/cmake");
        tokio::fs::create_dir_all(root_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&root_path, b"not valid json {").await.unwrap();

        let result = s.read_root_uncatalogued("ghcr.io", "kitware/cmake", |_| Ok(())).await;
        let err = result.expect_err("an unparseable derived root must hard-fail, never recover");
        assert!(
            matches!(
                err,
                crate::Error::FileStructure(super::super::error::Error::MalformedRootDocument { .. })
            ),
            "expected MalformedRootDocument, got {err:?}"
        );
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    // ── 9. list_wire_repositories: directory-enumeration catalog (A2) ──────
    //
    // A derived source's catalog IS the directory enumeration of `p/` — these
    // are characterization tests for the single-read `tokio::fs` walk (W5):
    // no prior coverage existed for this method before the DirWalker →
    // hand-rolled walk transformation, so these lock in its behavior.

    #[tokio::test(flavor = "multi_thread")]
    async fn list_wire_repositories_returns_empty_when_source_absent() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let repos = s.list_wire_repositories("ocx.sh").await.unwrap();
        assert!(repos.is_empty(), "an unseeded source must list as empty, never error");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_wire_repositories_collects_nested_root_documents_sorted_and_deduped() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        // Single-segment repository.
        let single = s.root_document_path("ocx.sh", "cmake");
        tokio::fs::create_dir_all(single.parent().unwrap()).await.unwrap();
        tokio::fs::write(&single, b"{}").await.unwrap();

        // Nested `<ns>/<pkg>` repositories, seeded out of order.
        let b = s.root_document_path("ocx.sh", "kitware/cmake");
        tokio::fs::create_dir_all(b.parent().unwrap()).await.unwrap();
        tokio::fs::write(&b, b"{}").await.unwrap();
        let a = s.root_document_path("ocx.sh", "astral/ruff");
        tokio::fs::create_dir_all(a.parent().unwrap()).await.unwrap();
        tokio::fs::write(&a, b"{}").await.unwrap();

        // A dispatch-object CAS directory sibling of `kitware/cmake.json` —
        // its own `*.json` object files must never surface as repositories.
        let dispatch = s.dispatch_object_path(
            "ocx.sh",
            "kitware/cmake",
            &crate::oci::Algorithm::Sha256.hash(b"payload"),
        );
        tokio::fs::create_dir_all(dispatch.parent().unwrap()).await.unwrap();
        tokio::fs::write(&dispatch, b"{}").await.unwrap();

        // A non-`.json` file must be ignored.
        tokio::fs::write(s.root().join("ocx.sh").join("p").join("README"), b"x")
            .await
            .unwrap();

        let repos = s.list_wire_repositories("ocx.sh").await.unwrap();
        assert_eq!(
            repos,
            vec![
                "astral/ruff".to_string(),
                "cmake".to_string(),
                "kitware/cmake".to_string()
            ],
            "results must be sorted, deduped, and never include the o/ dispatch-object CAS"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_wire_repositories_is_isolated_per_source() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let a = s.root_document_path("ocx.sh", "kitware/cmake");
        tokio::fs::create_dir_all(a.parent().unwrap()).await.unwrap();
        tokio::fs::write(&a, b"{}").await.unwrap();

        let repos = s.list_wire_repositories("ghcr.io").await.unwrap();
        assert!(
            repos.is_empty(),
            "a different source's directory enumeration must never leak in"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_wire_repositories_does_not_prune_a_namespace_literally_named_o() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        // A namespace literally named "o" — `p/o/tool.json` — must survive the
        // walk. A depth- or name-only skip of "o" would wrongly treat the
        // namespace dir itself as the dispatch-object CAS dir and drop it.
        let namespaced = s.root_document_path("ocx.sh", "o/tool");
        tokio::fs::create_dir_all(namespaced.parent().unwrap()).await.unwrap();
        tokio::fs::write(&namespaced, b"{}").await.unwrap();

        // A real dispatch-object CAS dir for an unrelated package must still be
        // skipped, so this isn't just a case of the skip never firing.
        let dispatch = s.dispatch_object_path(
            "ocx.sh",
            "kitware/cmake",
            &crate::oci::Algorithm::Sha256.hash(b"payload"),
        );
        tokio::fs::create_dir_all(dispatch.parent().unwrap()).await.unwrap();
        tokio::fs::write(&dispatch, b"{}").await.unwrap();
        let cmake_root = s.root_document_path("ocx.sh", "kitware/cmake");
        tokio::fs::write(&cmake_root, b"{}").await.unwrap();

        let repos = s.list_wire_repositories("ocx.sh").await.unwrap();
        assert_eq!(
            repos,
            vec!["kitware/cmake".to_string(), "o/tool".to_string()],
            "a namespace named \"o\" must be listed; the real o/ dispatch dir must still be pruned"
        );
    }
}
