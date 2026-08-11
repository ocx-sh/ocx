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
use crate::oci::index::{CatalogDocument, CatalogIndex, IndexFormatConfig, serialize_catalog, serialize_config};
use crate::utility::fs::LockedFile;
use crate::utility::result_ext::ResultExt;

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
    /// This is the only boundary: nothing turns a remote catalog key into a
    /// local path any more, so no repository-keyed read or write ever touches a
    /// path outside its source subtree, whatever the caller.
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
    ///
    /// `pub(crate)` because [`crate::oci::index::regenerate_catalog`] must run
    /// it before its own existence pre-flight: that pre-flight builds a path
    /// through [`Self::source_config_path`], a pure builder with no guard, so
    /// the check has to be reachable from outside this module rather than only
    /// firing inside the store methods that come after it.
    pub(crate) fn ensure_source_contained(source: &str) -> Result<()> {
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
//   c/index.json                      — published indices only — the catalog
//   p/{ns}/{pkg}.json                 — root doc
//   p/{ns}/{pkg}/o/{algo}/{hex}.json  — dispatch object CAS (the OCI image
//                                        index a tag resolved to, verbatim;
//                                        never a leaf platform manifest — A3)
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
// entry upsert and a whole-catalog reconcile-merge (F2 sync) alike — goes
// through [`IndexStore::begin_catalog_transaction`],
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

    /// Path to a source's `c/index.json` catalog (● `{"format_version": 1,
    /// "packages": {"<ns>/<pkg>": "sha256:<root-digest>"}}`, A2/F1) —
    /// per-source, unlike
    /// [`Self::catalog_path`]'s single global file.
    pub fn source_catalog_path(&self, source: &str) -> PathBuf {
        self.wire_source_dir(source).join("c").join("index.json")
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
    /// only** (the OCI image index a tag resolved to, verbatim); a leaf platform
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
        transaction.commit().await?;
        Ok((bytes, root, recovered_entry))
    }

    /// Read a root document without its catalog cross-check — the catalog-free
    /// sibling of [`Self::read_root`]: read → parse → `repository_check` only,
    /// no cross-check and no self-heal, returning
    /// [`CatalogEntryStatus::NoCatalog`]. `Ok(None)` when no root exists yet.
    ///
    /// Two callers choose it, for two unrelated reasons.
    /// [`crate::oci::index::LocalIndex`] reads a DERIVED source this way
    /// because such a source has no `c/index.json` at all (A2: its catalog is
    /// the directory enumeration of `p/`), so there is nothing to cross-check;
    /// provenance never enters this store, it is passed at the call site, so
    /// that divergence stays caller-side (`adr_index_indirection.md` A2/H "two
    /// ifs").
    ///
    /// [`crate::oci::index::regenerate_catalog`] reads a PUBLISHED — and
    /// possibly foreign — source this way, and its reason is **lock
    /// re-entrancy, not provenance**: [`Self::read_root`]'s self-heal opens its
    /// own [`Self::begin_catalog_transaction`], which would block against the
    /// transaction `regenerate` holds across its whole run for the full
    /// [`SOURCE_LOCK_TIMEOUT`] and then error — and [`Self::read_root`]
    /// *swallows* that error as a best-effort self-heal, so the symptom is a
    /// silent 60-second stall per straddled root, not a failure. "Correcting"
    /// that caller to [`Self::read_root`] because its source has a catalog
    /// deadlocks it.
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
    /// The on-disk document is the served [`CatalogDocument`] envelope, the same
    /// bytes the hosted site serves — a local copy that reads one shape and
    /// writes another would not be a copy. The version gate runs here, on read,
    /// exactly as it does for a fetched catalog ([`CatalogDocument::into_packages`]).
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
        let document: CatalogDocument = serde_json::from_slice(&bytes)?;
        Ok(Some(document.into_packages()?))
    }

    /// Reads this source's `config.json` — the served format-version pin (A2)
    /// — or `Ok(None)` when the tree carries none.
    ///
    /// Absence is not an error: a tree written before ocx wrote configs, or one
    /// authored by another implementation, is a valid format-version-1 index.
    /// Substituting [`IndexFormatConfig::assumed_v1`] for the absent document
    /// belongs to the gating caller (C-005) — this reader reports what is on
    /// disk and nothing more.
    ///
    /// # Errors
    ///
    /// A present-but-unparseable document is
    /// [`MalformedIndexDocument`](crate::oci::index::error::Error::MalformedIndexDocument);
    /// a present-but-unreadable one (EACCES, EISDIR) propagates its I/O error
    /// and is **never** flattened to `Ok(None)` — reading a permission failure
    /// as absence would promote an unreadable tree to a valid v1 index and
    /// silently disable the version gate (C-003).
    pub async fn read_source_config(&self, source: &str) -> Result<Option<IndexFormatConfig>> {
        Self::ensure_source_contained(source)?;
        let target = self.source_config_path(source);
        let bytes = match tokio::fs::read(&target).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(crate::error::file_error(&target, e)),
        };
        let config = serde_json::from_slice(&bytes).map_err(|cause| {
            crate::oci::index::error::Error::MalformedIndexDocument {
                url: target.display().to_string(),
                source: cause,
            }
        })?;
        Ok(Some(config))
    }

    /// Writes `{"format_version": 1}` to this source's `config.json` when the
    /// tree carries none, so a tree ocx just published declares itself an index
    /// at the version this binary speaks.
    ///
    /// **Write-if-absent, never update.** An existing config is left
    /// byte-identical, including the `name_segments` an operator declared in a
    /// hosted tree — ocx cannot derive that value from a tree and never guesses
    /// one (C-023).
    ///
    /// Published sources only, by construction: the single call site is
    /// [`crate::oci::index::LocalIndex`]'s `commit_published_root`, after the
    /// catalog transaction commits (C-023). A derived source writes through
    /// [`Self::write_root_document`] and never reaches it.
    ///
    /// **Call it only once [`CatalogTransaction::commit`] has consumed the
    /// transaction guard.** Calling it while a transaction for the same source
    /// is alive blocks on that same lock for the full [`SOURCE_LOCK_TIMEOUT`]
    /// and then errors — the inverted order self-deadlocks.
    ///
    /// Takes the source-scoped `"index-catalog"` lock ([`Self::lock_source`])
    /// for the write — re-acquired rather than inherited, because the write
    /// sits outside the catalog transaction's scope. The discriminator is
    /// `"c/index.json"`, the key [`Self::begin_catalog_transaction`] uses, and
    /// **not** the name of the file being written: `lock_source` keys on scope
    /// *and* discriminator, so a `"config.json"` discriminator would be an
    /// independent lock, letting this write land inside a `regenerate` window
    /// that C-008 requires to leave `config.json` byte-identical.
    ///
    /// A caller whose catalog work already committed should absorb a lock
    /// timeout rather than fail — the tree is content-complete and config-less,
    /// which the next update repairs. `commit_published_root` does exactly that.
    pub(crate) async fn ensure_source_config(&self, source: &str) -> Result<()> {
        // `lock_source` runs the containment guard and creates the source
        // directory, so the existence probe below is post-lock: a concurrent
        // writer cannot land a config between the probe and the write.
        let _lock = self
            .lock_source("index-catalog", source, "c/index.json", SOURCE_LOCK_TIMEOUT)
            .await?;
        let target = self.source_config_path(source);
        if crate::utility::fs::path_exists_lossy(&target).await {
            return Ok(());
        }
        let bytes = serialize_config(&IndexFormatConfig {
            format_version: crate::oci::index::SUPPORTED_FORMAT_VERSION,
            name_segments: None,
        });
        Self::write_bytes_atomic(&target, bytes).await
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
    /// # Errors
    ///
    /// A name under `p/` that is not valid UTF-8 is
    /// [`super::error::Error::NonUtf8WireName`] — never a skipped or
    /// U+FFFD-transliterated entry. This enumeration is what
    /// [`crate::oci::index::regenerate_catalog`] replaces the catalog from, and
    /// that replacement is wholesale, so a name dropped here is a package
    /// deleted from `c/index.json` with its root document still on disk.
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
            // Two independent facts identify it, and a dir named "o" is pruned
            // when EITHER holds — neither alone covers every real layout:
            //
            // 1. Its parent is a package dir, i.e. a sibling root-document file
            //    exists at `{package-dir}.json` in that parent
            //    (`root_document_path`) — `p/kitware/cmake/` pairs with
            //    `p/kitware/cmake.json`. A fixed depth cannot anchor this: a
            //    single-segment repository's package dir sits one level below
            //    `p/`, a two-segment one two levels below.
            // 2. Its own contents have the CAS shape `<algo>/<hex>.json`
            //    ([`is_dispatch_object_cas_dir`]). A package can hold dispatch
            //    objects with NO root document beside them: a `tag@digest` pull
            //    persists the dispatch chain but commits no tag, which is
            //    exactly how a patch companion is materialized (its pin is
            //    patch-tier state, never a local-index tag pointer). Premise 1
            //    alone would then walk into the CAS and emit an object filename
            //    as a phantom repository.
            //
            // A namespace or package literally named "o" satisfies neither: its
            // OWN sibling `.json` lives one level further up rather than beside
            // itself, and it holds root documents rather than digest-named
            // objects.
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
                    let is_dispatch_object_dir = path.file_name().and_then(|name| name.to_str()) == Some("o")
                        && (dir_is_a_package_dir || is_dispatch_object_cas_dir(&path).await);
                    if !is_dispatch_object_dir {
                        queue.push_back(path);
                    }
                    continue;
                }
                if !file_type.is_file() || path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = path.file_stem() else {
                    continue;
                };
                let Ok(relative) = dir.strip_prefix(&root) else {
                    continue;
                };
                let mut segments: Vec<String> = Vec::new();
                for component in relative.components() {
                    segments.push(utf8_wire_name(component.as_os_str(), &dir)?.to_string());
                }
                segments.push(utf8_wire_name(stem, &path)?.to_string());
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
    /// `source`, so no writer can bypass the re-read — see
    /// [`CatalogTransaction::catalog`] for the three that go through it. All
    /// network work (fetching a remote root or dispatch object) MUST happen
    /// before this call.
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
            original: catalog.clone(),
            catalog,
        })
    }
}

/// One wire path component as `str`, or [`super::error::Error::NonUtf8WireName`]
/// naming `path` — the enclosing file or directory, which is what an operator
/// needs to find a name their terminal cannot print.
fn utf8_wire_name<'name>(name: &'name std::ffi::OsStr, path: &Path) -> Result<&'name str> {
    name.to_str().ok_or_else(|| {
        super::error::Error::NonUtf8WireName {
            path: path.to_path_buf(),
        }
        .into()
    })
}

/// Whether `dir` holds the dispatch-object CAS — at least one
/// `<algo>/<hex>.json` object of a supported algorithm's digest length (A2/A3
/// `o/<algo>/<hex>.json`).
///
/// The shape test exists because the cheaper "does a sibling root document
/// exist" premise is falsified by a legitimate state: a `tag@digest` pull
/// persists the dispatch chain without committing a tag, so a package
/// directory can hold objects and no root document at all. Content, not
/// position, is then the only thing separating the CAS from a namespace
/// segment that happens to be named `o`.
///
/// **One conforming object is the whole test, not every child conforming.** A
/// stray file anywhere in the CAS — a `README`, a `.DS_Store`, an interrupted
/// rsync's `.partial` — must not un-prune it: the caller would walk in and emit
/// `<ns>/<pkg>/o/<algo>/<hex>` as a repository, and
/// [`crate::oci::index::regenerate_catalog`] would then publish that dispatch
/// object as a package in `c/index.json`. The strict reading rejected nothing
/// extra — a directory of real objects that also holds junk is still the CAS —
/// it only made the detector fail open on the tree shapes people actually have.
///
/// An unreadable directory answers `false` — the caller then walks it, which
/// is the pre-existing, non-destructive behaviour.
async fn is_dispatch_object_cas_dir(dir: &std::path::Path) -> bool {
    async fn entries_of(dir: &std::path::Path) -> Option<Vec<tokio::fs::DirEntry>> {
        let mut reader = tokio::fs::read_dir(dir).await.ok()?;
        let mut collected = Vec::new();
        while let Some(entry) = reader.next_entry().await.ok()? {
            collected.push(entry);
        }
        Some(collected)
    }

    let Some(algorithm_dirs) = entries_of(dir).await else {
        return false;
    };

    for algorithm_dir in algorithm_dirs {
        let name = algorithm_dir.file_name();
        let Some(algorithm) = name
            .to_str()
            .and_then(|name| crate::oci::Algorithm::ALL.iter().find(|a| a.prefix() == name))
        else {
            continue;
        };
        let Some(objects) = entries_of(&algorithm_dir.path()).await else {
            continue;
        };
        if objects.iter().any(|object| {
            let path = object.path();
            path.extension().and_then(|e| e.to_str()) == Some("json")
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|hex| hex.len() == algorithm.hex_len() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        }) {
            return true;
        }
    }
    false
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
    /// The post-lock map as read, before any mutation — so [`Self::commit`] can
    /// tell a real change from a re-derivation of what is already on disk and
    /// skip the write entirely.
    original: CatalogIndex,
    catalog: CatalogIndex,
}

impl CatalogTransaction<'_> {
    /// The freshly re-read (post-lock) catalog map.
    ///
    /// Three production writers reach the map, at three different scales.
    /// [`Self::write_root`] upserts the single entry it derives from the root
    /// bytes it just wrote, reaching the field directly rather than through
    /// this accessor. [`IndexStore::persist_recovered_catalog_entry`] upserts
    /// one re-derived entry on the **read** path ([`IndexStore::read_root`]'s
    /// self-heal), also directly. [`crate::oci::index::regenerate_catalog`]
    /// replaces the whole map through this accessor — the only operation that
    /// can drop a stale entry, since an upsert never removes one.
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

    /// Publishes the (possibly mutated) catalog map atomically, still under the
    /// held lock. Consumes the guard, releasing the lock on return.
    ///
    /// **A commit that changes nothing writes nothing.** An `ocx index update`
    /// against an unchanged remote catalog re-derives exactly the map already on
    /// disk; rewriting it would be byte-identical but would still churn the
    /// file's mtime, and this tree is a distributable artifact people commit to
    /// repos and `rsync` (A2).
    ///
    /// Written as the [`CatalogDocument`] envelope, the one shape this format
    /// has: what a local index writes is what the hosted site serves, so a
    /// derived source's catalog and a mirrored one are the same document and
    /// [`IndexStore::read_source_catalog`] needs no second branch to tell them
    /// apart. Emitted through [`serialize_catalog`], the wire formatter the
    /// hosted renderer's form is pinned against — `serde_json`'s pretty
    /// printer writes the same document without its trailing newline, one byte
    /// that diffs the whole file on every render of a shared tree (C-025).
    pub async fn commit(self) -> Result<()> {
        // Opportunistic cleanup: ocx used to persist an `index.json.etag`
        // conditional-GET validator beside the catalog. Nothing reads or writes
        // one any more, and it is the only per-machine file in a tree that is
        // otherwise pure served wire content — drop it so it stops travelling in
        // every copied and committed index tree. Failure is ignored on purpose:
        // a read-only shipped copy simply keeps the stray file.
        let stale_etag = self
            .store
            .source_catalog_path(&self.source)
            .with_added_extension("etag");
        tokio::fs::remove_file(&stale_etag).await.ignore();

        if self.catalog == self.original {
            return Ok(());
        }
        let catalog_path = self.store.source_catalog_path(&self.source);
        let catalog_bytes = serialize_catalog(&CatalogDocument::new(self.catalog));
        IndexStore::write_bytes_atomic(&catalog_path, catalog_bytes).await?;

        // `self.lock` drops here, releasing the exclusive advisory lock — held
        // across the whole read-mutate-write critical section.
        Ok(())
    }
}

/// Specification tests for the wire-grammar section (`adr_index_indirection.md`
/// Decisions A2/A3/A4/F1/F2, `adr_servable_index_snapshot.md` C-003/C-022/
/// C-023/C-025) — written from the design records' contracts, not from the
/// bodies. Each test states the contract it pins in its own name and message;
/// none of them describes a transient phase of the implementation.
#[cfg(test)]
mod wire_grammar_tests {
    use std::ffi::OsStr;
    use std::num::NonZeroU32;

    use super::*;
    use crate::cli::{ClassifyExitCode, ExitCode};
    use crate::oci::index::IndexRoot;

    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
    const SHA512_HEX: &str = "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e";

    fn store(dir: &Path) -> IndexStore {
        IndexStore::new(dir)
    }

    /// Decodes a persisted `c/index.json` straight off disk into its `packages`
    /// map. Deliberately NOT a call to [`IndexStore::read_source_catalog`] —
    /// these tests assert what the writer actually put on disk, so going
    /// through the reader under test would let a matched pair of read/write
    /// bugs pass.
    async fn catalog_on_disk(path: &Path) -> CatalogIndex {
        let document: CatalogDocument = serde_json::from_slice(&tokio::fs::read(path).await.unwrap()).unwrap();
        document.into_packages().unwrap()
    }

    /// Builds minimal root-document bytes matching `test/src/static_index.py`'s
    /// `write_package()` shape: a `repository` pointer and one tag whose
    /// `content` names `dispatch_digest`.
    fn minimal_root_bytes(repository: &str, dispatch_digest: &Digest) -> Vec<u8> {
        serde_json::json!({
            "repository": repository,
            "tags": {
                "latest": { "content": dispatch_digest.to_string(), "observed": "2026-07-18T09:00:00Z" }
            }
        })
        .to_string()
        .into_bytes()
    }

    /// A real minimal OCI image index — the shape a dispatch object actually
    /// holds (A3): `schemaVersion` + `mediaType` + one child manifest
    /// descriptor. Never the deleted `{"platforms":[...]}` projection.
    fn minimal_dispatch_object_bytes() -> Vec<u8> {
        serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [{
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": "sha256:aa",
                "size": 42,
                "platform": { "architecture": "amd64", "os": "linux" }
            }]
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
    // Containment (CWE-22) at the wire-grammar path builders — the only guard
    // left now that nothing turns a remote catalog key into a local path: a
    // repository reaching a wire-grammar path builder is
    // split on `/` verbatim (`repository_path`), so `..`, an absolute segment,
    // or a Windows/backslash escape would join outside the source subtree. Every
    // repository-keyed read/write refuses such a value as a `DataError` and
    // touches nothing on disk.

    /// The set of traversing repositories a hostile catalog key could smuggle
    /// to the store: leading `..`, bare `..`, an absolute path, and a
    /// Windows-style backslash escape (rejected host-independently). `a/../b`
    /// resolves WITHIN the root, so it is intentionally NOT in this set — the
    /// containment invariant is "stays under the source root", not "contains no
    /// `..` component".
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
        let payload = minimal_dispatch_object_bytes();
        let digest = crate::oci::Algorithm::Sha256.hash(&payload);
        s.write_dispatch_object("ocx.sh", "kitware/cmake", &digest, &payload)
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
        let payload = minimal_dispatch_object_bytes();
        let digest = crate::oci::Algorithm::Sha256.hash(&payload);
        s.write_dispatch_object("localhost:5000", "kitware/cmake", &digest, &payload)
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
        let payload = minimal_dispatch_object_bytes();
        let digest = crate::oci::Algorithm::Sha256.hash(&payload);

        s.write_dispatch_object("ocx.sh", "kitware/cmake", &digest, &payload)
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
        txn.commit().await.unwrap();

        let on_disk_root = tokio::fs::read(s.root_document_path("ocx.sh", "kitware/cmake"))
            .await
            .unwrap();
        assert_eq!(on_disk_root, root_bytes, "root document bytes must land verbatim");

        let catalog = catalog_on_disk(&s.source_catalog_path("ocx.sh")).await;
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
        let dispatch_payload = minimal_dispatch_object_bytes();
        let dispatch_digest = crate::oci::Algorithm::Sha256.hash(&dispatch_payload);

        // Step 1 (F1): dispatch object into o/ first.
        s.write_dispatch_object("ocx.sh", "kitware/cmake", &dispatch_digest, &dispatch_payload)
            .await
            .unwrap();

        let root_bytes = minimal_root_bytes("oci://ghcr.io/kitware/cmake", &dispatch_digest);

        // Steps 2+3 (F1): root, then the catalog entry, via one transaction.
        let mut txn = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        txn.write_root("kitware/cmake", &root_bytes, |_| Ok(())).await.unwrap();
        txn.commit().await.unwrap();

        assert!(
            tokio::fs::try_exists(s.dispatch_object_path("ocx.sh", "kitware/cmake", &dispatch_digest))
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
        let catalog = catalog_on_disk(&s.source_catalog_path("ocx.sh")).await;
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

        let catalog = catalog_on_disk(&s.source_catalog_path("ocx.sh")).await;
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
        tokio::fs::write(
            &catalog_path,
            serde_json::to_vec(&CatalogDocument::new(stale_catalog)).unwrap(),
        )
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
        let catalog = catalog_on_disk(&catalog_path).await;
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

        // Writer: THIS task, taking the transaction lock first and uncontended
        // — nothing else is running yet, so the acquisition order is structural
        // rather than raced. (An earlier shape spawned the writer and gave the
        // reader a 10 ms head start instead; under whole-suite load the spawned
        // writer could reach the lock *after* the reader, whose post-lock
        // re-read then legitimately saw v1 and failed the `result.bytes`
        // assertion below — a false alarm, not a lost update.)
        let mut transaction = s.begin_catalog_transaction("ocx.sh").await.unwrap();

        // Reader: starts with the lock already held, so its recovery must block
        // in `begin_catalog_transaction` until the commit below releases it.
        let s_reader = s.clone();
        let reader = tokio::spawn(async move { s_reader.read_root("ocx.sh", "kitware/cmake", |_| Ok(())).await });

        // Let the reader complete its pre-lock read (sees v1) and reach the
        // lock before the fresher root lands. If it were somehow still short of
        // the lock after this, it would observe the committed v2 + entry_v2 and
        // report `Consistent` — which the `catalog_status` assertion below
        // rejects loudly rather than passing for the wrong reason.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        // Still holding the lock: publish a FRESHER root (v2) and its matching
        // catalog entry, then release.
        tokio::fs::write(&root_path, &root_bytes_v2).await.unwrap();
        transaction
            .catalog()
            .insert("kitware/cmake".to_string(), entry_v2.clone());
        transaction.commit().await.unwrap();

        let result = reader.await.unwrap().unwrap().expect("root document exists on disk");

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

        let catalog = catalog_on_disk(&s.source_catalog_path("ocx.sh")).await;
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
        txn_a.commit().await.unwrap();

        let mut txn_b = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        txn_b
            .catalog()
            .insert("stable/tool".to_string(), "sha256:bbb".to_string());
        txn_b.commit().await.unwrap();

        let catalog = catalog_on_disk(&s.source_catalog_path("ocx.sh")).await;
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
        txn_b.commit().await.unwrap();

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
        txn_a.commit().await.unwrap();

        let catalog = catalog_on_disk(&s.source_catalog_path("ocx.sh")).await;
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
            txn.commit().await.unwrap();
        });
        // Give task_a a head start so it opens the transaction first.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let s_b = s.clone();
        let task_b = tokio::spawn(async move {
            let mut txn = s_b.begin_catalog_transaction("ocx.sh").await.unwrap();
            txn.catalog()
                .insert("stable/tool".to_string(), "sha256:from-b".to_string());
            txn.commit().await.unwrap();
        });

        task_a.await.unwrap();
        task_b.await.unwrap();

        let catalog = catalog_on_disk(&s.source_catalog_path("ocx.sh")).await;
        assert_eq!(
            catalog.get("kitware/cmake"),
            Some(&"sha256:from-a".to_string()),
            "concurrent catalog writers must not lose each other's entries"
        );
        assert_eq!(catalog.get("stable/tool"), Some(&"sha256:from-b".to_string()));
    }

    // ── 6. Commit no-op + stale-sidecar cleanup ───────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_that_changes_nothing_does_not_rewrite_the_catalog() {
        // An `ocx index update` against an unchanged remote catalog re-derives
        // exactly the map already on disk. Rewriting it would be byte-identical
        // but churn the mtime of a tree that gets committed to repos and
        // rsync'd (A2), so the commit must be a genuine no-op.
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        let mut first = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        first
            .catalog()
            .insert("kitware/cmake".to_string(), "sha256:aaa".to_string());
        first.commit().await.unwrap();

        let catalog_path = s.source_catalog_path("ocx.sh");
        let before = tokio::fs::metadata(&catalog_path).await.unwrap().modified().unwrap();

        // Re-insert the identical entry — the reconcile-merge's steady state.
        let mut second = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        second
            .catalog()
            .insert("kitware/cmake".to_string(), "sha256:aaa".to_string());
        second.commit().await.unwrap();

        assert_eq!(
            tokio::fs::metadata(&catalog_path).await.unwrap().modified().unwrap(),
            before,
            "a commit whose merged map equals the on-disk map must not rewrite the file"
        );

        // A real change still lands.
        let mut third = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        third
            .catalog()
            .insert("stable/tool".to_string(), "sha256:bbb".to_string());
        third.commit().await.unwrap();
        let catalog = s.read_source_catalog("ocx.sh").await.unwrap().unwrap();
        assert_eq!(catalog.get("stable/tool"), Some(&"sha256:bbb".to_string()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_removes_a_stale_etag_sidecar_left_by_an_older_ocx() {
        // Older ocx versions persisted an `index.json.etag` conditional-GET
        // validator beside the catalog — the one per-machine file in a tree that
        // is otherwise pure served wire content. It is dropped opportunistically
        // so it stops travelling in copied and committed index trees.
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        let mut first = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        first
            .catalog()
            .insert("kitware/cmake".to_string(), "sha256:aaa".to_string());
        first.commit().await.unwrap();

        let stale = s.source_catalog_path("ocx.sh").with_added_extension("etag");
        tokio::fs::write(&stale, b"\"abc123\"").await.unwrap();

        // Even a no-op commit clears it: an unchanged catalog is the common case.
        let mut second = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        second
            .catalog()
            .insert("kitware/cmake".to_string(), "sha256:aaa".to_string());
        second.commit().await.unwrap();

        assert!(
            !tokio::fs::try_exists(&stale).await.unwrap(),
            "a stale .etag sidecar must be removed by the next catalog commit"
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
        txn_a.commit().await.unwrap();

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
    // a catalog or lock.

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
    async fn list_wire_repositories_prunes_a_dispatch_dir_that_has_no_root_document() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        // A patch companion is pulled `tag@digest`, so the chain persists its
        // dispatch object but deliberately never commits a root document — a
        // companion pin is patch-tier state, never a local-index tag pointer.
        // The package dir therefore has NO sibling `.json`, and a walk that
        // recognises `o/` only by that sibling descends into the CAS and emits
        // its object filename as a repository.
        let dispatch = s.dispatch_object_path("ocx.sh", "ca-bundle", &crate::oci::Algorithm::Sha256.hash(b"payload"));
        tokio::fs::create_dir_all(dispatch.parent().unwrap()).await.unwrap();
        tokio::fs::write(&dispatch, b"{}").await.unwrap();

        let repos = s.list_wire_repositories("ocx.sh").await.unwrap();
        assert!(
            repos.is_empty(),
            "a dispatch-object CAS with no root document beside it holds no repositories, got {repos:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_wire_repositories_prunes_a_dispatch_dir_holding_a_stray_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        // The case above, plus stray entries an index tree collects in the
        // field: a `.DS_Store`, an interrupted rsync's `.partial`, a
        // hand-dropped README. A detector demanding that EVERY child conform
        // fails open on them — the CAS goes unpruned, the walk descends, and the
        // dispatch object's own digest is emitted as a repository, which
        // `regenerate_catalog` then publishes into `c/index.json` as a package
        // at exit 0.
        let dispatch = s.dispatch_object_path("ocx.sh", "ca-bundle", &crate::oci::Algorithm::Sha256.hash(b"payload"));
        let algorithm_dir = dispatch.parent().unwrap().to_path_buf();
        tokio::fs::create_dir_all(&algorithm_dir).await.unwrap();
        tokio::fs::write(&dispatch, b"{}").await.unwrap();
        tokio::fs::write(algorithm_dir.join("README"), b"notes").await.unwrap();
        tokio::fs::write(algorithm_dir.parent().unwrap().join(".DS_Store"), b"junk")
            .await
            .unwrap();

        let repos = s.list_wire_repositories("ocx.sh").await.unwrap();
        assert!(
            repos.is_empty(),
            "a stray file beside a dispatch object must not publish that object as a package, got {repos:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_wire_repositories_does_not_prune_a_namespace_named_o_without_a_root_document() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        // `p/vendor/o/tool.json` — "o" is a NAMESPACE segment here, and
        // `p/vendor` is a namespace too, so neither dir has a sibling root
        // document. Only the CAS shape ("<algo>/<hex>.json" all the way down)
        // separates this from the case above.
        let namespaced = s.root_document_path("ocx.sh", "vendor/o/tool");
        tokio::fs::create_dir_all(namespaced.parent().unwrap()).await.unwrap();
        tokio::fs::write(&namespaced, b"{}").await.unwrap();

        // A namespace whose child directory is named after an algorithm but
        // holds an ordinary root document, not a digest-named object.
        let algorithm_named = s.root_document_path("ocx.sh", "other/o/sha256/tool");
        tokio::fs::create_dir_all(algorithm_named.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&algorithm_named, b"{}").await.unwrap();

        let repos = s.list_wire_repositories("ocx.sh").await.unwrap();
        assert_eq!(
            repos,
            vec!["other/o/sha256/tool".to_string(), "vendor/o/tool".to_string()],
            "a namespace named \"o\" must survive even without a sibling root document"
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

    // ── 10. config.json reader (C-003) ────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn read_source_config_reports_an_absent_document_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        let config = s.read_source_config("ocx.sh").await.unwrap();
        assert!(
            config.is_none(),
            "a tree carrying no config.json is a valid format-version-1 index, not an error — \
             substituting assumed_v1 for it is the gating caller's job (C-003/C-005), so the \
             reader reports absence and nothing more"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_source_config_parses_a_present_document() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let path = s.source_config_path("ocx.sh");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&path, br#"{"format_version": 1, "name_segments": 2}"#)
            .await
            .unwrap();

        let config = s
            .read_source_config("ocx.sh")
            .await
            .unwrap()
            .expect("a present, parseable config.json must read as Some");
        assert_eq!(config.format_version, 1);
        assert_eq!(
            config.name_segments.map(NonZeroU32::get),
            Some(2),
            "the operator's declared name shape must survive the read verbatim — index.ocx.sh \
             serves 2, and ocx never derives a value of its own"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_source_config_rejects_an_unparseable_document() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let path = s.source_config_path("ocx.sh");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&path, b"not valid json {").await.unwrap();

        let err = s
            .read_source_config("ocx.sh")
            .await
            .expect_err("a present-but-unparseable config.json must be an error, never Ok(None)");
        assert!(
            matches!(
                err,
                crate::Error::OciIndex(crate::oci::index::error::Error::MalformedIndexDocument { .. })
            ),
            "expected MalformedIndexDocument, got {err:?}"
        );
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    /// C-003's fourth row, the local-filesystem twin of C-015: a permission
    /// failure read as absence would promote an unreadable tree to a valid v1
    /// index and silently disable the version gate.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn read_source_config_propagates_an_unreadable_document() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let path = s.source_config_path("ocx.sh");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&path, br#"{"format_version": 1}"#).await.unwrap();
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))
            .await
            .unwrap();

        // A process holding CAP_DAC_OVERRIDE (root in a container) reads a
        // mode-000 file anyway. The contract still holds there; it is simply not
        // observable, so skip rather than report a failure the code did not cause.
        if tokio::fs::read(&path).await.is_ok() {
            return;
        }

        let err = s
            .read_source_config("ocx.sh")
            .await
            .expect_err("an unreadable config.json must propagate its I/O error, never flatten to Ok(None)");
        assert!(
            matches!(err, crate::Error::InternalFile(_, _)),
            "expected the file_error I/O wrapper, got {err:?}"
        );
        assert_eq!(err.classify(), Some(ExitCode::IoError));
    }

    // ── 11. config.json writer (C-023, store half) ────────────────────────

    /// The `config.json` document OCX writes, in C-025's form: two-space
    /// indent, `sort_keys=False` declaration order, one trailing newline, and
    /// no `name_segments` — an operator declaration ocx cannot derive from a
    /// tree and declines to guess.
    const EXPECTED_CONFIG_BYTES: &[u8] = b"{\n  \"format_version\": 1\n}\n";

    #[tokio::test(flavor = "multi_thread")]
    async fn ensure_source_config_writes_the_wire_form_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        s.ensure_source_config("ocx.sh").await.unwrap();

        let on_disk = tokio::fs::read(s.source_config_path("ocx.sh")).await.unwrap();
        assert_eq!(
            on_disk,
            crate::oci::index::serialize_config(&IndexFormatConfig {
                format_version: crate::oci::index::SUPPORTED_FORMAT_VERSION,
                name_segments: None,
            }),
            "the write must route through serialize_config — no second formatter (C-025)"
        );
        assert_eq!(
            on_disk,
            EXPECTED_CONFIG_BYTES,
            "and serialize_config's form for this document is exactly {:?}",
            String::from_utf8_lossy(EXPECTED_CONFIG_BYTES)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ensure_source_config_leaves_an_existing_document_byte_identical() {
        // Write-if-absent, never update. A hosted tree's config carries the
        // operator's `name_segments`, and the hosted renderer's own byte form;
        // an `ocx index update` against that tree must not rewrite either.
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let path = s.source_config_path("ocx.sh");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        let seeded: &[u8] = b"{\n  \"format_version\": 1,\n  \"name_segments\": 2\n}\n";
        tokio::fs::write(&path, seeded).await.unwrap();
        let before = tokio::fs::metadata(&path).await.unwrap().modified().unwrap();

        s.ensure_source_config("ocx.sh").await.unwrap();

        assert_eq!(
            tokio::fs::read(&path).await.unwrap(),
            seeded,
            "an existing config must survive byte-identical, including a name_segments ocx never guesses"
        );
        assert_eq!(
            tokio::fs::metadata(&path).await.unwrap().modified().unwrap(),
            before,
            "an index tree is committed to repos and rsync'd (A2), so a no-op must not churn the mtime"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn ensure_source_config_propagates_a_write_failure() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        // The source directory exists, so the source-scoped lock still acquires
        // (it keys on that directory's identity), but it is read-only, so the
        // atomic write cannot create its tempfile there.
        let source_dir = s.source_config_path("ocx.sh").parent().unwrap().to_path_buf();
        tokio::fs::create_dir_all(&source_dir).await.unwrap();
        tokio::fs::set_permissions(&source_dir, std::fs::Permissions::from_mode(0o555))
            .await
            .unwrap();

        // As above: CAP_DAC_OVERRIDE ignores the mode, making the failure
        // unobservable rather than absent.
        let mode_is_enforced = tokio::fs::write(source_dir.join("probe"), b"x").await.is_err();
        let result = if mode_is_enforced {
            Some(s.ensure_source_config("ocx.sh").await)
        } else {
            None
        };
        // Restore before asserting so TempDir's cleanup runs either way.
        tokio::fs::set_permissions(&source_dir, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();

        let Some(result) = result else { return };
        let err = result.expect_err("an I/O failure on the config write must propagate, never be swallowed");
        assert_eq!(err.classify(), Some(ExitCode::IoError));
    }

    // ── 12. c/index.json goes through the wire formatter (C-025) ──────────

    /// Pins **the switch**: [`CatalogTransaction::commit`] must write
    /// `c/index.json` through [`crate::oci::index::serialize_catalog`] rather
    /// than `serde_json::to_vec_pretty`, whose output diverges by the trailing
    /// newline — one byte, but a full-file diff on every render of a tree the
    /// Rust and Python producers share.
    ///
    /// Scope: this pins the writer `commit` actually calls, **not**
    /// cross-language parity. The vendored Python fixtures that pin
    /// `serialize_catalog`'s bytes against the renderer's belong to C-025's
    /// conformance half and are not exercised here, so nothing below is
    /// evidence that the two producers agree. The comparison is against
    /// `serialize_catalog`'s own output — which is what "went through the
    /// formatter" means — plus an explicit literal, so a change to either side
    /// is localized rather than mutually cancelling.
    #[tokio::test(flavor = "multi_thread")]
    async fn commit_writes_the_catalog_through_the_wire_formatter() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        let mut txn = s.begin_catalog_transaction("ocx.sh").await.unwrap();
        txn.catalog()
            .insert("kitware/cmake".to_string(), format!("sha256:{SHA256_HEX}"));
        txn.commit().await.unwrap();

        let on_disk = tokio::fs::read(s.source_catalog_path("ocx.sh")).await.unwrap();

        let mut expected = CatalogIndex::new();
        expected.insert("kitware/cmake".to_string(), format!("sha256:{SHA256_HEX}"));
        assert_eq!(
            on_disk,
            crate::oci::index::serialize_catalog(&CatalogDocument::new(expected)),
            "commit must write the catalog through serialize_catalog, not serde_json::to_vec_pretty"
        );
        assert_eq!(
            String::from_utf8(on_disk).unwrap(),
            format!(
                "{{\n  \"format_version\": 1,\n  \"packages\": {{\n    \
                 \"kitware/cmake\": \"sha256:{SHA256_HEX}\"\n  }}\n}}\n"
            ),
            "…and that form is a two-space indent, declaration order, and one trailing newline"
        );
    }

    // ── 13. Wire-layout containment (C-022) ───────────────────────────────

    /// C-022: nothing but C-023's update-path hook may write `config.json`.
    ///
    /// Two bounds, because there are two ways to become a second writer. The
    /// obvious one is calling `ensure_source_config`: no more than one
    /// production call site in the crate, and any that exists is inside
    /// `commit_published_root`. The other is bypassing it — `source_config_path`
    /// is `pub`, so any function can build the path and hand it to a writer of
    /// its own; that one is bounded by whitelist, since only the reader and the
    /// writer have a reason to name the path at all.
    ///
    /// The call-site half is **exactly one**, in `commit_published_root`. It was
    /// a `<= 1` bound while C-023's call site was still owed by another work
    /// package; now that it exists, the existence half is assertable here rather
    /// than inferred from `dead_code` firing in a different crate module. The
    /// direction C-022 exists for is still the upper bound — it fails the moment
    /// a second writer appears in `commit`, in `regenerate`, or on a read path,
    /// any of which would make a resolve mutate a tree ocx may not own.
    #[test]
    fn config_json_has_at_most_one_production_writer_and_it_is_the_update_path() {
        fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("crate src/ is readable") {
                let path = entry.expect("a readable dir entry").path();
                if path.is_dir() {
                    rs_files(&path, out);
                } else if path.extension() == Some(OsStr::new("rs")) {
                    out.push(path);
                }
            }
        }

        /// The enclosing production `fn`'s name for a match at `offset`, or
        /// `None` when the match is in a comment or doc link, or inside a
        /// specification test. Matching the bare identifier rather than
        /// `.ident(` is deliberate: the UFCS form `IndexStore::ident(self, …)`
        /// carries no receiver dot and slipped through the narrower needle.
        fn production_site(source: &str, offset: usize) -> Option<String> {
            let line_start = source[..offset].rfind('\n').map_or(0, |newline| newline + 1);
            if source[line_start..offset].trim_start().starts_with("//") {
                return None;
            }
            let fn_at = source[..offset].rfind("fn ")?;
            // A specification test driving the writer is not a production call
            // site. Its enclosing signature carries a test attribute within the
            // few lines directly above it.
            let signature_head: String = source[..fn_at].lines().rev().take(4).collect();
            if signature_head.contains("#[test]") || signature_head.contains("#[tokio::test") {
                return None;
            }
            // Read the name out of the whole file, not out of the text before
            // the match: on the declaration line the match IS the name, so a
            // slice ending at `offset` leaves nothing after `fn `.
            Some(
                source[fn_at + "fn ".len()..]
                    .split(['(', '<', ' ', '\n'])
                    .next()
                    .unwrap_or("")
                    .to_string(),
            )
        }

        // Built rather than written literally: a literal needle would match this
        // test's own source, and the enclosing-`fn` walk would then blame the
        // nearest nested helper rather than the test holding it.
        let writer = format!("ensure_source{}config", "_");
        let path_builder = format!("source_config{}path", "_");

        let mut files = Vec::new();
        rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        files.sort();

        let mut call_sites = Vec::new();
        let mut path_users = Vec::new();
        for file in &files {
            let source = std::fs::read_to_string(file).expect("a readable source file");
            for (needle, found) in [(&writer, &mut call_sites), (&path_builder, &mut path_users)] {
                for (offset, _) in source.match_indices(needle.as_str()) {
                    let Some(enclosing) = production_site(&source, offset) else {
                        continue;
                    };
                    // The item's own declaration: the nearest preceding `fn ` is
                    // the one being declared.
                    if &enclosing == needle {
                        continue;
                    }
                    found.push(format!("{}::{enclosing}", file.display()));
                }
            }
        }

        assert_eq!(
            call_sites.len(),
            1,
            "config.json must have exactly one writer (C-022/C-023); found {call_sites:#?}"
        );
        assert!(
            call_sites[0].ends_with("::commit_published_root"),
            "the one config.json writer must be the update path, not a read or regenerate path; found {}",
            call_sites[0]
        );
        // Naming the path is the prerequisite for writing it, so the bound is on
        // who may name it — stricter than sniffing for a write verb, which a
        // two-line bypass (bind the path, write it on the next line) evades.
        // `regenerate_catalog` is here because it derives the source directory
        // from this accessor, `wire_source_dir` being private; it never writes.
        let allowed = [
            format!("::read_source{}config", "_"),
            format!("::{writer}"),
            format!("::regenerate{}catalog", "_"),
        ];
        assert!(
            path_users
                .iter()
                .all(|site| allowed.iter().any(|suffix| site.ends_with(suffix))),
            "only these may name the config.json path, since naming it is how a second writer starts \
             (C-022): {allowed:?}; found {path_users:#?}"
        );
    }

    // ── 14. Non-UTF-8 wire names must not vanish (WP5b) ───────────────────
    //
    // `list_wire_repositories` is `regenerate`'s view of what exists on disk.
    // A `p/` name that is not valid UTF-8 — and C-007 explicitly admits trees
    // written by another implementation — must not be transliterated to U+FFFD
    // (a key naming a path that does not exist) nor skipped: either way
    // `regenerate` deletes the package from `c/index.json` while its root sits
    // on disk, exit 0, reporting it in none of added/corrected/removed.

    /// The exit code a non-UTF-8 name under `p/` classifies to (C-028),
    /// asserted from one place.
    ///
    /// `DataError` (65), carried by a variant of this module's
    /// [`super::super::error::Error`] — the home for "this tree's structure is
    /// malformed", which every variant of that enum classifies to. Not
    /// `file_error` → `InternalFile` → `IoError` (74): 74 is the generic
    /// fallback that enum's variants exist to escape, and a foreign tree's
    /// un-decodable name is malformed input, not an ocx internal fault.
    ///
    /// The tests below assert `classify()` rather than the variant, so renaming
    /// the variant does not break them.
    #[cfg(unix)]
    const NON_UTF8_WALK_EXIT_CODE: ExitCode = ExitCode::DataError;

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn list_wire_repositories_propagates_a_non_utf8_root_document_stem() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        // A well-formed neighbour, so a dropped bad name cannot hide behind an
        // empty listing.
        let good = s.root_document_path("ocx.sh", "kitware/cmake");
        tokio::fs::create_dir_all(good.parent().unwrap()).await.unwrap();
        tokio::fs::write(&good, b"{}").await.unwrap();

        // `p/kitware/<0xff>.json` — a POSIX filename is a byte string, so
        // nothing makes this one valid UTF-8.
        let bad = good.parent().unwrap().join(OsStr::from_bytes(b"\xff.json"));
        if let Err(refused) = tokio::fs::write(&bad, b"{}").await {
            // APFS validates filenames as UTF-8 and rejects these bytes
            // outright, so the state under test cannot exist on a macOS
            // volume. Observed, not assumed via `target_os`: the assertion
            // below still runs on any filesystem that does accept the name,
            // and a refusal for any other reason (a wrong parent path) reds
            // here instead of passing as this carve-out.
            assert!(
                good.parent().is_some_and(std::path::Path::exists),
                "only the non-UTF-8 component may be refused; the parent must exist: {refused}"
            );
            return;
        }

        let err = s
            .list_wire_repositories("ocx.sh")
            .await
            .expect_err("a non-UTF-8 root-document stem must be reported, never silently skipped");
        assert_eq!(err.classify(), Some(NON_UTF8_WALK_EXIT_CODE));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn list_wire_repositories_propagates_a_non_utf8_directory_component() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        // `p/<0xff>/cmake.json` — the failure is in a directory component, not
        // the stem, so it takes the walk's other lossy path.
        let namespace = s.wire_source_dir("ocx.sh").join("p").join(OsStr::from_bytes(b"\xff"));
        if let Err(refused) = tokio::fs::create_dir_all(&namespace).await {
            // Same APFS carve-out as the stem test above — observed rather
            // than gated on `target_os`, and narrowed to the one component
            // the filesystem can legitimately refuse.
            assert!(
                namespace.parent().is_some_and(std::path::Path::exists),
                "only the non-UTF-8 component may be refused; the parent must exist: {refused}"
            );
            return;
        }
        tokio::fs::write(namespace.join("cmake.json"), b"{}").await.unwrap();

        let err = s
            .list_wire_repositories("ocx.sh")
            .await
            .expect_err("a non-UTF-8 directory component must be reported, never transliterated to U+FFFD");
        assert_eq!(err.classify(), Some(NON_UTF8_WALK_EXIT_CODE));
    }
}
