// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Opt-in shared-roots ledger for cross-instance GC rooting.
//!
//! Enabled by `OCX_SHARED_STORE=true`. Provides a digest-only ledger at
//! `$OCX_PACKAGES_DIR/roots/<instance_id>/<project_hash>` that allows a
//! shared-store `clean` to union all instances' pinned digests as GC roots,
//! preventing one instance from collecting objects still pinned by another.
//!
//! ## On-disk structure
//!
//! ```text
//! $OCX_PACKAGES_DIR/roots/
//!   <instance_id_A>/
//!     <project_hash_1>    ← each file is a versioned JSON ledger document
//!     <project_hash_2>
//!   <instance_id_B>/
//!     <project_hash_3>
//! ```
//!
//! - `instance_id` = UUID from `$OCX_STATE_DIR/instance-id`.
//! - `project_hash` = 16-hex of `SHA-256(canonical_project_dir)` — same
//!   scheme as [`crate::reference_manager::ReferenceManager::name_for_path`].
//! - File contents = a versioned JSON envelope (see [`LedgerFile`]):
//!   `{"v":1,"digests":["sha256:…", …]}`. Digests are the **ImageIndex
//!   manifest digests** lifted verbatim from `ocx.lock` — they are *not*
//!   resolved to platform-child package digests at write time. The reader
//!   ([`collect_shared_root_digests`]) resolves them via the same
//!   `resolve_to_package_digests` path the project ledger uses.
//!
//! ## Versioned format + the unknown-version fail-closed contract
//!
//! The on-disk version is encoded as a [`SharedRootsVersion`] `serde_repr`
//! enum, so `serde_repr` rejects unknown discriminants at deserialization
//! automatically (no manual `CURRENT_VERSION` check). This is the load-bearing
//! one-way-door safety property: the shared ledger is **cross-version by
//! construction** (a peer running a newer ocx may write a `v2` file into a
//! shared volume an older ocx then reads), so the per-instance `projects/`
//! "no version, just a symlink" precedent does NOT transfer.
//!
//! **Unknown-version / unparseable peer ledger ⇒ retain-all (fail-closed).**
//! When [`SharedRoots::union_all_digests`] encounters a peer ledger file it
//! cannot parse — a future version, a truncated/partial write that escaped the
//! atomic-rename guard, or any other deserialize failure — it CANNOT enumerate
//! that peer's pinned digests. Silently skipping the file would let the peer's
//! pins be collected (silent data loss across the one-way door). The reader
//! therefore returns [`SharedRootsUnion::RetainAll`], which makes the calling
//! `clean` conservative: it collects **nothing** this run rather than risk
//! deleting a peer's still-pinned objects. The next `clean` (on an ocx new
//! enough to parse the file, or after the partial write is rewritten) proceeds
//! normally. A WARN is logged so the over-retention is observable, never silent.
//!
//! This is strictly the conservative direction: over-retention wastes disk but
//! never deletes a live peer's objects. `--force` remains the operator override
//! that bypasses the registry (and shared roots) entirely.
//!
//! ## Write semantics
//!
//! Written best-effort (via [`SharedRoots::write_best_effort`]) on every
//! `ocx.lock` save — the same trigger as
//! [`crate::project::registry::register_project_dir_best_effort`]. Any write
//! failure is logged at `warn!` and swallowed; a missed write means the next
//! `clean` on a peer instance may collect objects this instance needs — the GC
//! grace period ([`crate::env::keys::OCX_GC_GRACE_SECONDS`]) provides a
//! time-bounded backstop for recovery.
//!
//! ## Atomic per-file writes (no lock protects the cross-instance read)
//!
//! Each ledger file is written via tempfile + same-directory rename
//! ([`crate::utility::fs::persist_temp_file`], the `BlobStore` precedent). The
//! cross-instance write/read is **not** lock-protected: the GC lock
//! ([`crate::package_manager::tasks::garbage_collection::GcLock`]) is
//! same-`$OCX_HOME` only (its lock file lives in the per-instance state zone),
//! so a peer's `union_all_digests` can read this instance's `roots/` directory
//! concurrently with a write. Atomicity is the ONLY defense against a peer
//! unioning a half-written file: the rename publishes the complete document or
//! nothing — never a truncated prefix. No truncate-then-append.
//!
//! ## Read semantics
//!
//! `clean` reads all instance directories and unions their digest sets via
//! [`SharedRoots::union_all_digests`], called only when
//! `OCX_SHARED_STORE=true` ([`is_shared_store_enabled`]). A single unreadable
//! *file* (transient I/O, NotFound race) is skipped with a WARN — the union of
//! the remaining files is still returned. An unparseable / unknown-version file
//! escalates to [`SharedRootsUnion::RetainAll`] (see the version contract
//! above). A non-`NotFound` error opening the `roots/` directory itself
//! propagates as `Err`.
//!
//! ## Default-mode isolation
//!
//! When `OCX_SHARED_STORE` is not set (the default), `clean` ignores this
//! ledger entirely and writers emit zero bytes. The existing per-instance
//! `projects/` symlink ledger continues to supply GC roots unchanged.
//!
//! ## Residual boundary (documented for P3.8)
//!
//! - **Opt-in only.** Default behavior is byte-identical to pre-P3.6.
//! - **Missed writes are time-bounded.** A dropped best-effort write is
//!   backstopped by the mtime grace window, not by this ledger.
//! - **Stale departed-instance dirs over-retain.** A `roots/<dead_instance>/`
//!   directory that never gets cleaned up keeps its digests rooted forever —
//!   the safe direction (over-retention, never deletion).
//! - **Forensics.** Cross-instance deletions are recorded in the GC audit log
//!   (`gc-log.jsonl`) keyed by `instance_id`.
//!
//! See `system_design_shared_store.md` §5 M4 item 4.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

/// On-disk version discriminant for a shared-roots ledger file.
///
/// Serialized as a bare integer via `serde_repr`. Deserialization rejects
/// unknown discriminants automatically — the reader treats that rejection as
/// the fail-closed "retain-all" signal (see the module doc). When a future
/// format lands, add `V2 = 2` alongside `V1`; existing v1 files keep parsing.
///
/// Intentionally NOT `#[non_exhaustive]` — an internal on-disk discriminant,
/// not a public library enum (project convention: omit `#[non_exhaustive]` on
/// internal non-error enums so matches stay total).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum SharedRootsVersion {
    /// Version 1 of the shared-roots ledger file format.
    V1 = 1,
}

/// A single shared-roots ledger file: a versioned, digest-only document.
///
/// The `digests` are ImageIndex manifest digests copied verbatim from
/// `ocx.lock` (`registry/repo@digest` → the `@digest` part as a full
/// `algorithm:hex` string). They are resolved to package-store paths at read
/// time, never at write time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerFile {
    /// On-disk format version (currently always [`SharedRootsVersion::V1`]).
    #[serde(rename = "v")]
    pub version: SharedRootsVersion,
    /// Pinned content digests as `algorithm:hex` strings, one per tool.
    pub digests: Vec<String>,
}

/// Outcome of unioning every instance's shared-roots ledger.
///
/// `Digests` carries the deduplicated digest set to add as GC roots.
/// `RetainAll` is the fail-closed marker emitted when any peer ledger file is
/// unparseable / of an unknown version (the one-way-door safety property — see
/// the module doc): the caller must retain every object this run rather than
/// collect against a digest set that omits a peer's unreadable pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedRootsUnion {
    /// Deduplicated union of all parseable instances' pinned digests.
    Digests(HashSet<String>),
    /// Fail-closed: a peer ledger was unparseable / unknown-version — retain
    /// every object this run.
    RetainAll,
}

/// Reader/writer for the opt-in shared-roots ledger.
///
/// Construct via [`SharedRoots::new`]; all I/O is async.
pub struct SharedRoots {
    /// `$OCX_PACKAGES_DIR/roots/`
    roots_dir: PathBuf,
    /// UUID of the current OCX instance (`$OCX_STATE_DIR/instance-id`).
    instance_id: String,
}

impl SharedRoots {
    /// Constructs a [`SharedRoots`] handle.
    ///
    /// `packages_dir` — resolved packages zone root (`$OCX_PACKAGES_DIR`).
    /// `instance_id` — UUID from `$OCX_STATE_DIR/instance-id`.
    ///
    /// Pure path arithmetic; no I/O at construction time.
    pub fn new(packages_dir: &Path, instance_id: impl Into<String>) -> Self {
        Self {
            roots_dir: packages_dir.join("roots"),
            instance_id: instance_id.into(),
        }
    }

    /// Writes the pinned digests for `project_hash` under this instance's
    /// ledger directory.
    ///
    /// The file path is `$OCX_PACKAGES_DIR/roots/<instance_id>/<project_hash>`;
    /// it is created atomically (tempfile + rename in the same directory) so
    /// a concurrent [`Self::union_all_digests`] never observes a partial write
    /// (the cross-instance read is not lock-protected — see the module doc).
    ///
    /// `digests` are wrapped in a [`LedgerFile`] versioned [`SharedRootsVersion::V1`]
    /// envelope. They are stored verbatim — not resolved to platform-child
    /// package digests; the reader resolves them.
    ///
    /// # Parameters
    ///
    /// - `project_hash` — 16-hex `SHA-256(canonical_project_dir)`, same as
    ///   [`crate::reference_manager::ReferenceManager::name_for_path`].
    /// - `digests` — content digests (`algorithm:hex`) pinned by this project.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InternalFile`] on I/O failure (directory create,
    /// serialize, tempfile write, or atomic rename).
    pub async fn write(&self, project_hash: &str, digests: &[String]) -> crate::Result<()> {
        let instance_dir = self.instance_dir();
        let file_path = instance_dir.join(project_hash);
        let document = LedgerFile {
            version: SharedRootsVersion::V1,
            digests: digests.to_vec(),
        };
        // Serialize on the async side (pure CPU, no I/O); the blocking
        // filesystem work (mkdir + tempfile + atomic rename) runs on a
        // blocking thread so it never stalls the runtime.
        let bytes = serde_json::to_vec(&document).map_err(|e| {
            crate::Error::InternalFile(
                file_path.clone(),
                std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            )
        })?;

        // Keep a copy for the panic-mapper closure; the move closure consumes
        // `file_path` for the atomic publish.
        let panic_path = file_path.clone();
        tokio::task::spawn_blocking(move || -> crate::Result<()> {
            std::fs::create_dir_all(&instance_dir).map_err(|e| crate::Error::InternalFile(instance_dir.clone(), e))?;
            // Tempfile in the SAME directory as the target so the publish is a
            // same-dir rename (atomic; no truncate-then-append). The atomic
            // rename is the only defense against a peer unioning a half-written
            // file (the cross-instance read holds no lock).
            let mut tmp = tempfile::NamedTempFile::new_in(&instance_dir)
                .map_err(|e| crate::Error::InternalFile(instance_dir.clone(), e))?;
            std::io::Write::write_all(&mut tmp, &bytes)
                .map_err(|e| crate::Error::InternalFile(tmp.path().to_path_buf(), e))?;
            tmp.as_file()
                .sync_data()
                .map_err(|e| crate::Error::InternalFile(tmp.path().to_path_buf(), e))?;
            crate::utility::fs::persist_temp_file(tmp, &file_path)
                .map_err(|e| crate::Error::InternalFile(file_path.clone(), e))?;
            Ok(())
        })
        .await
        .map_err(|e| {
            crate::Error::InternalFile(
                panic_path,
                std::io::Error::other(format!("shared-roots write task panicked: {e}")),
            )
        })?
    }

    /// Best-effort variant of [`Self::write`].
    ///
    /// Logs `warn!` on failure and swallows the error so the caller's
    /// `ocx.lock`-save path is never aborted. A missed write is backstopped by
    /// the GC mtime grace window, not by this ledger (residual best-effort
    /// corruption guard — see the module doc).
    pub async fn write_best_effort(&self, project_hash: &str, digests: &[String]) {
        if let Err(e) = self.write(project_hash, digests).await {
            crate::log::warn!(
                "Shared-roots ledger: write of '{}' failed (non-fatal): {e}",
                self.instance_dir().join(project_hash).display()
            );
        }
    }

    /// Reads and parses the pinned digests for `project_hash` under this
    /// instance.
    ///
    /// Returns an empty `Vec` when the file does not exist (no digests pinned
    /// yet for this project). Returns `Err` on a non-`NotFound` I/O failure or
    /// an unparseable / unknown-version document — surfacing the parse failure
    /// here lets the round-trip tests pin the version contract; the cross-
    /// instance reader [`Self::union_all_digests`] converts the same parse
    /// failure into [`SharedRootsUnion::RetainAll`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InternalFile`] on a non-`NotFound` read error or
    /// a deserialize failure (unknown version, malformed JSON).
    pub async fn read(&self, project_hash: &str) -> crate::Result<Vec<String>> {
        let file_path = self.instance_dir().join(project_hash);
        match tokio::fs::read(&file_path).await {
            Ok(bytes) => {
                let document: LedgerFile = serde_json::from_slice(&bytes).map_err(|e| {
                    crate::Error::InternalFile(
                        file_path.clone(),
                        std::io::Error::new(std::io::ErrorKind::InvalidData, e),
                    )
                })?;
                Ok(document.digests)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(crate::Error::InternalFile(file_path, e)),
        }
    }

    /// Reads all instances' ledger files and returns the union of all pinned
    /// digests, or the fail-closed [`SharedRootsUnion::RetainAll`] marker.
    ///
    /// Called by `clean` when `OCX_SHARED_STORE=true`. Iterates
    /// `$OCX_PACKAGES_DIR/roots/<instance_id>/<project_hash>` for all instance
    /// directories and collects the deduplicated set of digest strings.
    ///
    /// Per-file robustness:
    /// - A **NotFound race** on one file (the file vanished between readdir and
    ///   read — a rename race during a peer's atomic write, or a concurrent
    ///   prune) → debug + skip (the union of the rest is still returned).
    /// - Any **other I/O error** on a committed file (EACCES/EIO/ESTALE) → WARN +
    ///   [`SharedRootsUnion::RetainAll`] (fail closed: the peer's pins cannot be
    ///   enumerated, so retain everything rather than collect against an
    ///   incomplete root set).
    /// - An **unparseable / unknown-version** file → WARN +
    ///   [`SharedRootsUnion::RetainAll`] (the one-way-door fail-closed contract:
    ///   the peer's pins cannot be enumerated, so retain everything — see the
    ///   module doc).
    ///
    /// Directory-level robustness:
    /// - `roots/` absent → [`SharedRootsUnion::Digests`] with an empty set (no
    ///   peers have written yet).
    /// - A non-`NotFound` error opening `roots/` or an instance dir propagates
    ///   as `Err`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InternalFile`] on a non-`NotFound` directory
    /// enumeration failure on `roots/` or any instance subdirectory.
    pub async fn union_all_digests(&self) -> crate::Result<SharedRootsUnion> {
        let roots_dir = self.roots_dir.clone();

        // Synchronous readdir + per-file read/parse, off the runtime — mirrors
        // the `ProjectRegistry::live_projects` convention.
        tokio::task::spawn_blocking(move || -> crate::Result<SharedRootsUnion> {
            let instance_dirs = match std::fs::read_dir(&roots_dir) {
                Ok(rd) => rd,
                // `roots/` absent → no peers have written. Not an error; the
                // directory is NOT created as a side effect.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(SharedRootsUnion::Digests(HashSet::new()));
                }
                Err(e) => return Err(crate::Error::InternalFile(roots_dir.clone(), e)),
            };

            let mut union: HashSet<String> = HashSet::new();

            for instance_entry in instance_dirs {
                let instance_entry = instance_entry.map_err(|e| crate::Error::InternalFile(roots_dir.clone(), e))?;
                let instance_path = instance_entry.path();
                // Skip non-directory siblings (e.g. a stray `.tmp-*` from a
                // crashed write).
                if !instance_path.is_dir() {
                    continue;
                }

                let ledger_files = match std::fs::read_dir(&instance_path) {
                    Ok(rd) => rd,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(crate::Error::InternalFile(instance_path.clone(), e)),
                };

                for ledger_entry in ledger_files {
                    let ledger_entry =
                        ledger_entry.map_err(|e| crate::Error::InternalFile(instance_path.clone(), e))?;
                    let ledger_path = ledger_entry.path();
                    if !ledger_path.is_file() {
                        continue;
                    }
                    // Skip in-flight tempfiles from a concurrent write — they
                    // are not committed ledger files.
                    if ledger_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(".tmp"))
                    {
                        continue;
                    }

                    let bytes = match std::fs::read(&ledger_path) {
                        Ok(bytes) => bytes,
                        // NotFound = the file was removed between readdir and read
                        // (a rename race during a peer's atomic write, or a
                        // concurrent prune). Benign: skip.
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            crate::log::debug!(
                                "Shared-roots ledger: '{}' vanished between readdir and read (race); skipping.",
                                ledger_path.display()
                            );
                            continue;
                        }
                        // Any other I/O error on a committed ledger: we cannot
                        // enumerate this peer's pins, so we must NOT collect
                        // against an incomplete root set.
                        Err(e) => {
                            crate::log::warn!(
                                "Shared-roots ledger: unreadable committed file '{}' ({e}); \
                                 failing closed (RetainAll).",
                                ledger_path.display()
                            );
                            return Ok(SharedRootsUnion::RetainAll);
                        }
                    };

                    match serde_json::from_slice::<LedgerFile>(&bytes) {
                        Ok(document) => union.extend(document.digests),
                        // Unparseable / unknown-version peer ledger: the
                        // one-way-door fail-closed contract. We cannot
                        // enumerate this peer's pins, so retain EVERYTHING this
                        // run rather than risk collecting the peer's still-
                        // pinned objects.
                        Err(e) => {
                            crate::log::warn!(
                                "Shared-roots ledger: unparseable / unknown-version file '{}' \
                                 ({e}); retaining all objects this run (fail-closed).",
                                ledger_path.display()
                            );
                            return Ok(SharedRootsUnion::RetainAll);
                        }
                    }
                }
            }

            Ok(SharedRootsUnion::Digests(union))
        })
        .await
        .map_err(|e| {
            crate::Error::InternalFile(
                self.roots_dir.clone(),
                std::io::Error::other(format!("shared-roots union task panicked: {e}")),
            )
        })?
    }

    /// Returns the root of the ledger directory (`$OCX_PACKAGES_DIR/roots/`).
    pub fn roots_dir(&self) -> &Path {
        &self.roots_dir
    }

    /// Returns the instance-specific ledger directory
    /// (`$OCX_PACKAGES_DIR/roots/<instance_id>/`).
    pub fn instance_dir(&self) -> PathBuf {
        self.roots_dir.join(&self.instance_id)
    }
}

/// Best-effort shared-roots write fired on every `ocx.lock` save when
/// `OCX_SHARED_STORE` is enabled — the sibling of
/// [`crate::project::registry::register_project_dir_best_effort`].
///
/// **No-op (zero bytes written) when the flag is off** ([`is_shared_store_enabled`]).
/// This preserves byte-identical default-mode behavior: a non-shared-store
/// install never touches the `roots/` ledger.
///
/// When enabled, this:
/// 1. Canonicalizes `config_path` and takes its parent (the project dir) to
///    derive the same `project_hash` the project ledger uses
///    ([`crate::reference_manager::ReferenceManager::name_for_path`]).
/// 2. Resolves the packages zone (`$OCX_PACKAGES_DIR`) from the env (where the
///    `roots/` ledger lives, shared across the fleet) and the `instance_id`
///    from `state_zone_root` (per-instance).
/// 3. Persists the ImageIndex digests verbatim (`LockedTool.pinned`,
///    advisory-stripped) — resolve-on-read, not resolve-on-write (Adjustment 2).
///
/// Every failure mode (canonicalize, packages-zone resolution, write) is the
/// silent-data-loss class: logged at `warn!` and swallowed so the `ocx.lock`
/// save is never aborted. A missed write is backstopped by the GC mtime grace
/// window (residual best-effort corruption guard).
///
/// `pinned_digests` are the canonical `registry/repo@digest` strings lifted
/// from the saved lock's tools (the caller maps `LockedTool.pinned`).
pub async fn write_shared_roots_best_effort(config_path: &Path, state_zone_root: &Path, pinned_digests: &[String]) {
    // Default-mode isolation: zero bytes when the flag is off.
    if !is_shared_store_enabled() {
        return;
    }

    // Derive the project dir (canonical) → project_hash, mirroring
    // `register_project_dir_best_effort`'s canonicalize-config-then-parent
    // shape so the two ledgers key on the same project directory.
    let canonical_project_dir = match tokio::fs::canonicalize(config_path).await {
        Ok(canonical_config) => match canonical_config.parent() {
            Some(dir) => dir.to_path_buf(),
            None => {
                crate::log::warn!(
                    "Shared-roots ledger: config path '{}' has no parent directory \
                     (non-fatal, write skipped)",
                    canonical_config.display()
                );
                return;
            }
        },
        Err(e) => {
            crate::log::warn!(
                "Shared-roots ledger: canonicalize of config path '{}' failed \
                 (non-fatal, write skipped): {e}",
                config_path.display()
            );
            return;
        }
    };
    let project_hash = crate::reference_manager::ReferenceManager::name_for_path(&canonical_project_dir);

    // The `roots/` ledger lives in the PACKAGES zone (shared across the fleet),
    // resolved from the env where this process runs.
    let Some(home) = crate::file_structure::default_ocx_root() else {
        crate::log::warn!("Shared-roots ledger: cannot resolve OCX home (non-fatal, write skipped)");
        return;
    };
    let layout = crate::file_structure::StoreLayout::resolve_from_env(home);
    let packages_zone_root = layout.packages_root();

    let instance_id = crate::utility::instance_id::load_or_create_instance_id(state_zone_root).await;
    let shared_roots = SharedRoots::new(packages_zone_root, instance_id);
    shared_roots.write_best_effort(&project_hash, pinned_digests).await;
}

/// Returns `true` when `OCX_SHARED_STORE` is set to a truthy value.
///
/// Controls whether `clean` calls [`SharedRoots::union_all_digests`] and
/// whether project-ledger links absent from the live set are retained rather
/// than pruned (non-pruning under flag, see
/// [`crate::project::registry::ProjectRegistry::live_projects`]).
///
/// Reads the env var through [`crate::env::flag`] (which parses via
/// [`crate::utility::boolean_string::BooleanString`], the same truthy-string
/// vocabulary every other OCX env boolean uses). Absent / empty / unparseable
/// → `false` (default-mode isolation: zero bytes written, ledger ignored).
pub fn is_shared_store_enabled() -> bool {
    crate::env::flag(crate::env::keys::OCX_SHARED_STORE, false)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    // ── SharedRoots path helpers ─────────────────────────────────────────────

    #[test]
    fn roots_dir_is_under_packages_dir() {
        // Requirement: system_design_shared_store.md §5 M4 item 4 —
        // ledger at `$OCX_PACKAGES_DIR/roots/<instance_id>/<project_hash>`.
        // Traced to: plan_shared_store P3.2s shared-roots (write/read/union test).
        let packages_dir = std::path::PathBuf::from("/vol/shared/packages");
        let sr = SharedRoots::new(&packages_dir, "inst-abc");
        assert_eq!(sr.roots_dir(), packages_dir.join("roots").as_path());
    }

    #[test]
    fn instance_dir_is_under_roots_dir() {
        // instance_dir = $OCX_PACKAGES_DIR/roots/<instance_id>/
        // Traced to: plan_shared_store P3.2s shared-roots.
        let packages_dir = std::path::PathBuf::from("/vol/shared");
        let instance_id = "uuid-1234";
        let sr = SharedRoots::new(&packages_dir, instance_id);
        assert_eq!(sr.instance_dir(), packages_dir.join("roots").join(instance_id));
    }

    // ── write + read round-trip ──────────────────────────────────────────────

    #[tokio::test]
    async fn write_then_read_round_trips_digests() {
        // Requirement: system_design_shared_store.md §5 M4 item 4 —
        // write digest list, read it back via `SharedRoots::read`.
        // Traced to: plan_shared_store P3.2s shared-roots "write then read round-trips".
        let dir = tempdir().unwrap();
        let sr = SharedRoots::new(dir.path(), "instance-a");

        let digests = vec!["abc123".to_string(), "def456".to_string(), "ghi789".to_string()];

        sr.write("project_hash_1", &digests).await.unwrap();
        let read_back = sr.read("project_hash_1").await.unwrap();

        // Order may differ; check set equality.
        let written_set: std::collections::HashSet<_> = digests.iter().cloned().collect();
        let read_set: std::collections::HashSet<_> = read_back.iter().cloned().collect();
        assert_eq!(written_set, read_set, "read must return all written digests");
    }

    #[tokio::test]
    async fn read_absent_file_returns_empty_vec() {
        // No pinned digests for a project → read returns empty (not Err).
        // Traced to: plan_shared_store P3.2s shared-roots.
        let dir = tempdir().unwrap();
        let sr = SharedRoots::new(dir.path(), "instance-a");
        let result = sr.read("nonexistent_project_hash").await.unwrap();
        assert!(result.is_empty(), "absent project file must return empty Vec");
    }

    // ── union_all_digests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn union_all_digests_merges_across_instances() {
        // Requirement: system_design_shared_store.md §5 M4 item 4 —
        // `union_all_digests` reads all instance dirs and returns the set union.
        // Traced to: plan_shared_store P3.2s shared-roots "union merges across 2+ instances".
        let dir = tempdir().unwrap();

        // Two instances sharing the same packages dir.
        let sr_a = SharedRoots::new(dir.path(), "instance-a");
        let sr_b = SharedRoots::new(dir.path(), "instance-b");

        let digests_a = vec!["alpha".to_string(), "shared_digest".to_string()];
        let digests_b = vec!["beta".to_string(), "shared_digest".to_string()];

        sr_a.write("proj1", &digests_a).await.unwrap();
        sr_b.write("proj2", &digests_b).await.unwrap();

        // Union from either instance's perspective must see all digests.
        let union = match sr_a.union_all_digests().await.unwrap() {
            SharedRootsUnion::Digests(set) => set,
            SharedRootsUnion::RetainAll => panic!("expected Digests, got RetainAll"),
        };
        assert!(union.contains("alpha"), "union must contain instance-a's alpha");
        assert!(union.contains("beta"), "union must contain instance-b's beta");
        assert!(
            union.contains("shared_digest"),
            "union must contain the shared digest exactly once"
        );
    }

    #[tokio::test]
    async fn union_all_digests_empty_when_no_roots_dir() {
        // Requirement: system_design_shared_store.md §5 M4 item 4 —
        // If `$OCX_PACKAGES_DIR/roots/` doesn't exist, union returns empty.
        // Traced to: plan_shared_store P3.2s shared-roots.
        let dir = tempdir().unwrap();
        // Sub-directory that doesn't have a `roots/` under it.
        let sr = SharedRoots::new(dir.path(), "instance-a");
        // roots/ does not exist; should return empty Digests (not RetainAll).
        let result = sr.union_all_digests().await.unwrap();
        assert_eq!(
            result,
            SharedRootsUnion::Digests(HashSet::new()),
            "absent roots dir must return an empty digest union"
        );
    }

    // ── unknown-version fail-closed (Adjustment 1) ───────────────────────────

    #[tokio::test]
    async fn unknown_version_is_fail_closed() {
        // Adjustment 1 (plan P3.6): a peer ledger file carrying an unknown /
        // future version (or any unparseable content) must make the cross-
        // instance reader fail CLOSED — `union_all_digests` returns
        // `RetainAll` so `clean` collects nothing this run, never silently
        // de-rooting the peer's pins across the one-way door.
        let dir = tempdir().unwrap();
        let sr_peer = SharedRoots::new(dir.path(), "instance-peer");

        // Hand-write a ledger file with version 2 (unknown to this build).
        // serde_repr rejects the discriminant at deserialize time, which the
        // reader converts to the fail-closed RetainAll marker.
        let peer_dir = sr_peer.instance_dir();
        std::fs::create_dir_all(&peer_dir).unwrap();
        std::fs::write(peer_dir.join("proj_future"), br#"{"v":2,"digests":["sha256:dead"]}"#).unwrap();

        let reader = SharedRoots::new(dir.path(), "instance-reader");
        let result = reader.union_all_digests().await.unwrap();
        assert_eq!(
            result,
            SharedRootsUnion::RetainAll,
            "an unknown-version peer ledger must yield RetainAll (fail-closed), \
             never silently drop the peer's pins"
        );
    }

    #[tokio::test]
    async fn malformed_json_is_fail_closed() {
        // Sibling of the version test: a truncated / partial write that
        // escaped the atomic-rename guard is also unparseable, and must
        // likewise fail closed rather than be silently skipped.
        let dir = tempdir().unwrap();
        let peer = SharedRoots::new(dir.path(), "instance-peer");
        let peer_dir = peer.instance_dir();
        std::fs::create_dir_all(&peer_dir).unwrap();
        std::fs::write(peer_dir.join("proj_partial"), b"{\"v\":1,\"dig").unwrap();

        let reader = SharedRoots::new(dir.path(), "instance-reader");
        let result = reader.union_all_digests().await.unwrap();
        assert_eq!(
            result,
            SharedRootsUnion::RetainAll,
            "a truncated/partial ledger file must fail closed (RetainAll)"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_committed_ledger_is_fail_closed() {
        // FIX B: a transient I/O error (EACCES/EIO/ESTALE) on a COMMITTED peer
        // ledger must fail closed (RetainAll), not silently skip the file — a
        // skip would drop that peer's pins and over-delete its packages. Only a
        // NotFound race (file vanished between readdir and read) is benign.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let peer = SharedRoots::new(dir.path(), "instance-peer");
        let peer_dir = peer.instance_dir();
        std::fs::create_dir_all(&peer_dir).unwrap();
        let ledger_path = peer_dir.join("proj_locked");
        std::fs::write(&ledger_path, br#"{"v":1,"digests":["sha256:abc"]}"#).unwrap();

        // Revoke all permissions so `std::fs::read` returns EACCES (a non-
        // NotFound I/O error on a committed, readdir-visible file).
        std::fs::set_permissions(&ledger_path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let reader = SharedRoots::new(dir.path(), "instance-reader");
        let result = reader.union_all_digests().await;

        // Restore permissions before tempdir drop so cleanup can remove it.
        std::fs::set_permissions(&ledger_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(
            result.unwrap(),
            SharedRootsUnion::RetainAll,
            "an unreadable committed peer ledger must fail closed (RetainAll), \
             never silently skip and drop the peer's pins"
        );
    }

    #[tokio::test]
    async fn write_produces_versioned_v1_envelope() {
        // The on-disk format is the versioned envelope `{"v":1,"digests":[…]}`
        // (Adjustment 1: versioned BEFORE first write). Assert the wire shape
        // so a future format change is a conscious, test-visible decision.
        let dir = tempdir().unwrap();
        let sr = SharedRoots::new(dir.path(), "instance-a");
        sr.write("proj1", &["sha256:abc".to_string()]).await.unwrap();

        let on_disk = std::fs::read_to_string(sr.instance_dir().join("proj1")).unwrap();
        let parsed: LedgerFile = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(parsed.version, SharedRootsVersion::V1, "must serialize version 1");
        assert!(
            on_disk.contains("\"v\":1"),
            "envelope must carry the `v` discriminant: {on_disk}"
        );
        assert_eq!(parsed.digests, vec!["sha256:abc".to_string()]);
    }

    // ── is_shared_store_enabled ──────────────────────────────────────────────

    #[test]
    fn is_shared_store_enabled_false_when_var_absent() {
        // Requirement: system_design_shared_store.md §5 M4 item 4 —
        // Default mode: OCX_SHARED_STORE not set → is_shared_store_enabled() = false.
        // Traced to: plan_shared_store P3.2s shared-roots "default-mode ignores shared roots".
        let env = crate::test::env::lock();
        env.remove(crate::env::keys::OCX_SHARED_STORE);
        assert!(
            !is_shared_store_enabled(),
            "OCX_SHARED_STORE absent → must return false (default-mode isolation)"
        );
    }

    #[test]
    fn is_shared_store_enabled_true_when_var_set_truthy() {
        // Requirement: system_design_shared_store.md §5 M4 item 4 —
        // OCX_SHARED_STORE=true → is_shared_store_enabled() = true.
        // Traced to: plan_shared_store P3.2s shared-roots.
        let env = crate::test::env::lock();
        env.set(crate::env::keys::OCX_SHARED_STORE, "true");
        assert!(is_shared_store_enabled(), "OCX_SHARED_STORE=true must return true");
    }

    #[tokio::test]
    async fn write_best_effort_does_not_propagate_failure() {
        // Requirement: system_design_shared_store.md §5 M4 item 4 —
        // best-effort writes must not abort the caller's lock-save path.
        // Traced to: plan_shared_store P3.2s shared-roots (write best-effort).
        //
        // write_best_effort returns () and swallows any error. The happy path
        // here simply confirms it completes without unwinding; the error-
        // swallowing contract is its signature (Future<Output = ()>).
        let dir = tempdir().unwrap();
        let sr = SharedRoots::new(dir.path(), "inst");
        sr.write_best_effort("hash", &[]).await;
    }
}
