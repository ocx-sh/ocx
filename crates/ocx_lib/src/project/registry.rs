// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-user registry of projects whose `ocx.lock` files pin packages on this
//! machine.
//!
//! The registry answers the question "which projects have been seen on this
//! machine" so that `ocx clean` can retain packages held by lockfiles in
//! projects other than the currently-active one.
//!
//! On disk: two files under `$OCX_HOME`:
//!
//! - `projects.json` — single JSON document listing registered project paths.
//! - `.projects.lock` — sibling advisory-lock sentinel (never contains data).
//!
//! Write protocol: read → merge → tempfile → atomic rename, all gated behind
//! an exclusive advisory lock on `.projects.lock`. Pattern identical to
//! [`crate::project::ProjectLock::save`].
//!
//! See [`adr_clean_project_backlinks.md`] for full design rationale and
//! on-disk schema specification.

pub mod error;

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use error::{Error, ProjectRegistryError, ProjectRegistryErrorKind};

use crate::file_lock::FileLock;
use crate::log;

// ── On-disk schema ─────────────────────────────────────────────────────────

/// Current schema version — written on every save, rejected on unknown values.
const SCHEMA_VERSION: u8 = 1;

/// On-disk representation of the registry JSON document.
///
/// `schema_version` is checked on read: any value other than `1` surfaces as
/// [`ProjectRegistryErrorKind::UnknownVersion`] (no fallback, no migration).
#[derive(Serialize, Deserialize, Debug)]
struct RegistryDoc {
    schema_version: u8,
    entries: Vec<EntryOnDisk>,
}

/// Serialization form of a single project entry.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct EntryOnDisk {
    ocx_lock_path: PathBuf,
    last_seen: String,
}

// ── Public types ───────────────────────────────────────────────────────────

/// Per-user registry of known project `ocx.lock` paths.
///
/// Populated whenever an `ocx.lock` is saved and consulted by `ocx clean` to
/// avoid collecting packages that are pinned by any registered project.
///
/// Construct via [`ProjectRegistry::new`]; all I/O is async. See the module
/// documentation and [`adr_clean_project_backlinks.md`] for the on-disk schema
/// and locking protocol.
pub struct ProjectRegistry {
    /// Absolute path to `$OCX_HOME/projects.json`.
    path: PathBuf,
    /// Absolute path to `$OCX_HOME/.projects.lock` (advisory-lock sentinel).
    sentinel_path: PathBuf,
}

/// A single entry in the project registry.
///
/// Each entry records an absolute canonicalised path to a project's `ocx.lock`
/// file and the ISO-8601 UTC timestamp of the last registration call. The
/// `last_seen` field is diagnostic-only; it is not load-bearing for GC
/// decisions.
#[derive(Clone, Debug)]
pub struct ProjectEntry {
    /// Absolute, canonicalised path to the project's `ocx.lock` file.
    pub ocx_lock_path: PathBuf,
    /// ISO-8601 UTC timestamp of the most recent [`ProjectRegistry::register`]
    /// call for this path.
    pub last_seen: String,
}

// ── Lock acquisition helpers ───────────────────────────────────────────────

/// Open the sentinel without following symlinks (mirrors `open_sidecar_no_follow`
/// from `lock.rs`).
///
/// Returns a raw `std::fs::File` on success. The caller must then acquire the
/// advisory lock via [`FileLock::try_exclusive`] or
/// [`FileLock::lock_exclusive_with_timeout`].
fn open_sentinel_no_follow(sentinel: &Path) -> Result<std::fs::File, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            // O_NOFOLLOW is a standard POSIX flag; libc exposes it as c_int.
            .custom_flags(libc::O_NOFOLLOW)
            .open(sentinel)
            .map_err(|e| ProjectRegistryError::new(sentinel.to_path_buf(), ProjectRegistryErrorKind::Io(e)).into())
    }
    // On non-Unix platforms O_NOFOLLOW is unavailable; check via symlink_metadata
    // and refuse if a symlink is present (best-effort — no atomic guarantee).
    #[cfg(not(unix))]
    {
        match std::fs::symlink_metadata(sentinel) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(ProjectRegistryError::new(
                    sentinel.to_path_buf(),
                    ProjectRegistryErrorKind::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "sentinel path is a symlink",
                    )),
                )
                .into());
            }
            _ => {}
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(sentinel)
            .map_err(|e| ProjectRegistryError::new(sentinel.to_path_buf(), ProjectRegistryErrorKind::Io(e)).into())
    }
}

/// Acquire the exclusive advisory lock on the registry sentinel.
///
/// Tries once without blocking; if contended, retries for up to 2 seconds
/// (per ADR "Failure Modes"). On timeout, returns `Err(Locked)`. On a real
/// I/O error from the lock primitive, returns `Err(Io)`.
async fn acquire_exclusive(sentinel: &Path) -> Result<FileLock, Error> {
    let sentinel_path = sentinel.to_path_buf();
    // Fast path: open the sentinel and try once without blocking inside
    // spawn_blocking to avoid calling std::fs::OpenOptions::open on the
    // async executor thread (blocking I/O in async = Block-tier violation).
    let sp = sentinel_path.clone();
    let open_result = tokio::task::spawn_blocking(move || open_sentinel_no_follow(&sp))
        .await
        .expect("spawn_blocking panicked");
    let file = open_result?;
    match FileLock::try_exclusive(file) {
        Ok(Some(lock)) => return Ok(lock),
        Ok(None) => {} // contended — fall through to timed retry
        Err(e) => {
            return Err(ProjectRegistryError::new(sentinel_path, ProjectRegistryErrorKind::Io(e)).into());
        }
    }
    // Retry: open a fresh file descriptor (the first one was consumed by
    // `try_exclusive`), then block for up to 2 s.
    let sp2 = sentinel_path.clone();
    let open_result2 = tokio::task::spawn_blocking(move || open_sentinel_no_follow(&sp2))
        .await
        .expect("spawn_blocking panicked");
    let file2 = open_result2?;
    match FileLock::lock_exclusive_with_timeout(file2, std::time::Duration::from_secs(2)).await {
        Ok(lock) => Ok(lock),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            Err(ProjectRegistryError::new(sentinel_path, ProjectRegistryErrorKind::Locked).into())
        }
        Err(e) => Err(ProjectRegistryError::new(sentinel_path, ProjectRegistryErrorKind::Io(e)).into()),
    }
}

// ── Read / write helpers ───────────────────────────────────────────────────

/// Returns the current UTC timestamp as an ISO-8601 string.
fn now_iso8601() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Read and parse the registry file. Returns `Ok(None)` when absent.
fn read_registry(path: &Path) -> Result<Option<RegistryDoc>, Error> {
    let raw = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ProjectRegistryError::new(path.to_path_buf(), ProjectRegistryErrorKind::Io(e)).into()),
    };
    let doc: RegistryDoc = serde_json::from_slice(&raw)
        .map_err(|e| ProjectRegistryError::new(path.to_path_buf(), ProjectRegistryErrorKind::Corrupt(e)))?;
    if doc.schema_version != SCHEMA_VERSION {
        // Refuse to read or overwrite a registry written by a different
        // (likely newer) version of OCX. Surface as UnknownVersion so the
        // caller can propagate as ConfigError (exit 78).
        return Err(ProjectRegistryError::new(
            path.to_path_buf(),
            ProjectRegistryErrorKind::UnknownVersion {
                found: doc.schema_version,
                expected: SCHEMA_VERSION,
            },
        )
        .into());
    }
    Ok(Some(doc))
}

/// Atomically rewrite the registry file via tempfile + rename (same pattern as
/// `ProjectLock::save`). Caller holds the advisory lock; the file descriptor
/// guard (`_lock`) must outlive this call.
fn write_registry(path: &Path, doc: &RegistryDoc, _lock: &FileLock) -> Result<(), Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ProjectRegistryError::new(parent.to_path_buf(), ProjectRegistryErrorKind::Io(e)))?;
    }

    let serialized = serde_json::to_string_pretty(doc)
        .map_err(|e| ProjectRegistryError::new(path.to_path_buf(), ProjectRegistryErrorKind::Corrupt(e)))?;

    // Snapshot the existing file's permissions (if any) so the atomic rename
    // does not change the mode. On first-ever save this lookup fails with
    // NotFound, which is tolerated — tempfile's default mode stands.
    //
    // On Unix, cap the mode at 0o644 (user rw, group/other r) so an
    // accidentally world-writable file is not perpetuated through the atomic
    // rename cycle (mirrors lock.rs:426-441).
    let prior_perms = std::fs::metadata(path).ok().map(|m| {
        let mut perms = m.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = perms.mode() & 0o644;
            perms.set_mode(mode);
        }
        perms
    });

    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| ProjectRegistryError::new(parent.to_path_buf(), ProjectRegistryErrorKind::Io(e)))?;
    tmp.write_all(serialized.as_bytes())
        .map_err(|e| ProjectRegistryError::new(tmp.path().to_path_buf(), ProjectRegistryErrorKind::Io(e)))?;
    tmp.as_file()
        .sync_data()
        .map_err(|e| ProjectRegistryError::new(tmp.path().to_path_buf(), ProjectRegistryErrorKind::Io(e)))?;
    if let Some(perms) = prior_perms {
        tmp.as_file()
            .set_permissions(perms)
            .map_err(|e| ProjectRegistryError::new(tmp.path().to_path_buf(), ProjectRegistryErrorKind::Io(e)))?;
    }
    tmp.persist(path)
        .map_err(|e| ProjectRegistryError::new(path.to_path_buf(), ProjectRegistryErrorKind::Io(e.error)))?;

    // fsync the directory so the rename is durable across a crash.
    #[cfg(unix)]
    if !parent.as_os_str().is_empty() {
        let dir = std::fs::File::open(parent)
            .map_err(|e| ProjectRegistryError::new(parent.to_path_buf(), ProjectRegistryErrorKind::Io(e)))?;
        dir.sync_all()
            .map_err(|e| ProjectRegistryError::new(parent.to_path_buf(), ProjectRegistryErrorKind::Io(e)))?;
    }

    Ok(())
}

// ── Lazy pruning helper ────────────────────────────────────────────────────

/// Returns `true` when an entry still satisfies all three lazy-pruning rules:
///
/// 1. `ocx_lock_path` exists on disk.
/// 2. `ocx_lock_path` is a regular file (not a directory or special file).
/// 3. A sibling `ocx.toml` exists next to the lock file.
fn entry_is_valid(entry: &EntryOnDisk) -> bool {
    let path = &entry.ocx_lock_path;
    // Rule 1 & 2: exists as a regular file.
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.is_file() {
        return false;
    }
    // Rule 3: sibling ocx.toml must exist.
    let toml = match path.parent() {
        Some(p) => p.join("ocx.toml"),
        None => return false,
    };
    toml.exists()
}

// ── impl ───────────────────────────────────────────────────────────────────

impl ProjectRegistry {
    /// Constructs a [`ProjectRegistry`] rooted at `ocx_home`.
    ///
    /// Pure path arithmetic; performs no I/O. The registry file
    /// (`projects.json`) and sentinel (`.projects.lock`) are located at
    /// `ocx_home/projects.json` and `ocx_home/.projects.lock` respectively,
    /// matching the layout decided in [`adr_clean_project_backlinks.md`].
    pub fn new(ocx_home: &Path) -> Self {
        Self {
            path: ocx_home.join("projects.json"),
            sentinel_path: ocx_home.join(".projects.lock"),
        }
    }

    /// Records (or refreshes) the entry for `ocx_lock_path` in the registry.
    ///
    /// `ocx_lock_path` must be absolute and canonicalised. Updates `last_seen`
    /// to the current UTC time. Persists atomically via tempfile + rename under
    /// an exclusive advisory lock on the sibling sentinel, mirroring the
    /// protocol in [`crate::project::ProjectLock::save`].
    ///
    /// Registration failures are intentionally non-fatal at call sites: a
    /// failed registration loses one window but does not abort the `ocx lock`
    /// save that triggered it. Callers log at WARN and swallow the error.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Registry`] wrapping:
    ///
    /// - [`ProjectRegistryErrorKind::Io`] on directory creation, sentinel
    ///   open, lock acquisition, or atomic rename failure.
    /// - [`ProjectRegistryErrorKind::Corrupt`] if the existing registry file
    ///   parses as garbage (refuses to overwrite blindly).
    /// - [`ProjectRegistryErrorKind::Locked`] if the advisory lock cannot be
    ///   acquired within the retry timeout.
    pub async fn register(&self, ocx_lock_path: &Path) -> Result<(), Error> {
        let registry_path = self.path.clone();
        let sentinel_path = self.sentinel_path.clone();
        let ocx_lock_path = ocx_lock_path.to_path_buf();
        let now = now_iso8601();

        // Ensure OCX_HOME exists (idempotent).
        if let Some(parent) = registry_path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ProjectRegistryError::new(parent.to_path_buf(), ProjectRegistryErrorKind::Io(e)))?;
        }

        // Acquire exclusive lock (sentinel open + fs4 exclusive).
        let lock = acquire_exclusive(&sentinel_path).await?;

        // All subsequent I/O is synchronous (no await while holding lock guard).
        tokio::task::spawn_blocking(move || -> Result<(), Error> {
            // Read existing registry (or start empty).
            let mut doc = match read_registry(&registry_path)? {
                Some(d) => d,
                None => RegistryDoc {
                    schema_version: SCHEMA_VERSION,
                    entries: Vec::new(),
                },
            };

            // Update or append entry.
            if let Some(pos) = doc.entries.iter().position(|e| e.ocx_lock_path == ocx_lock_path) {
                doc.entries[pos].last_seen = now;
            } else {
                doc.entries.push(EntryOnDisk {
                    ocx_lock_path,
                    last_seen: now,
                });
            }

            write_registry(&registry_path, &doc, &lock)?;
            Ok(())
        })
        .await
        .expect("spawn_blocking panicked in ProjectRegistry::register")?;

        Ok(())
    }

    /// Loads the registry and lazily prunes entries whose `ocx_lock_path` no
    /// longer satisfies the validity criteria (file exists, is a regular file,
    /// has a sibling `ocx.toml`).
    ///
    /// If pruning drops at least one entry the shrunken registry is persisted
    /// under the same lock-then-rename protocol as [`Self::register`]. If
    /// nothing is pruned, no write occurs.
    ///
    /// First-run behaviour (file absent): returns an empty `Vec` and writes
    /// nothing, matching the migration contract in [`adr_clean_project_backlinks.md`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Registry`] wrapping:
    ///
    /// - [`ProjectRegistryErrorKind::Io`] on read or write failure.
    /// - [`ProjectRegistryErrorKind::Corrupt`] if the file exists but cannot
    ///   be parsed; the error surfaces so the caller can propagate as
    ///   `ConfigError` (exit 78) rather than silently falling back to an empty
    ///   registry (which would re-introduce GC false positives).
    /// - [`ProjectRegistryErrorKind::Locked`] if the advisory lock cannot be
    ///   acquired for the prune rewrite.
    pub async fn load_and_prune(&self) -> Result<Vec<ProjectEntry>, Error> {
        // First-run: if the file is absent return empty without acquiring the lock.
        // Use tokio::fs::try_exists to avoid blocking the async executor with a
        // synchronous stat(2) call.
        if !tokio::fs::try_exists(&self.path)
            .await
            .map_err(|e| ProjectRegistryError::new(self.path.clone(), ProjectRegistryErrorKind::Io(e)))?
        {
            return Ok(Vec::new());
        }

        let registry_path = self.path.clone();
        let sentinel_path = self.sentinel_path.clone();

        // Acquire exclusive lock before reading so the read + conditional rewrite
        // is atomic with respect to concurrent `register` calls.
        let lock = acquire_exclusive(&sentinel_path).await?;

        tokio::task::spawn_blocking(move || -> Result<Vec<ProjectEntry>, Error> {
            // Read existing registry.
            let doc = match read_registry(&registry_path)? {
                Some(d) => d,
                None => {
                    // File disappeared between the pre-check and the lock
                    // acquisition — treat as first-run.
                    return Ok(Vec::new());
                }
            };

            let original_len = doc.entries.len();

            // Apply lazy pruning rules.
            let surviving: Vec<EntryOnDisk> = doc.entries.into_iter().filter(entry_is_valid).collect();

            let pruned_count = original_len - surviving.len();

            // Persist shrunken registry only when something was dropped.
            // Build new_doc unconditionally to own `surviving`; write only when
            // pruned_count > 0, then consume new_doc.entries for the return value.
            let new_doc = RegistryDoc {
                schema_version: SCHEMA_VERSION,
                entries: surviving,
            };
            if pruned_count > 0 {
                log::debug!(
                    "Pruned {} stale project registry entry/entries from {}.",
                    pruned_count,
                    registry_path.display()
                );
                write_registry(&registry_path, &new_doc, &lock)?;
            }

            Ok(new_doc
                .entries
                .into_iter()
                .map(|e| ProjectEntry {
                    ocx_lock_path: e.ocx_lock_path,
                    last_seen: e.last_seen,
                })
                .collect())
        })
        .await
        .expect("spawn_blocking panicked in ProjectRegistry::load_and_prune")
    }

    /// Registers `ocx_lock_path` if it exists on disk; no-ops silently when
    /// the file is absent.
    ///
    /// Used by code paths that resolve a project lock without saving it (e.g.
    /// `ocx pull --project`): the lock may or may not be present depending on
    /// whether the project has been initialised yet. Calling `register` directly
    /// with a non-existent path would create a registry entry that immediately
    /// prunes on the next `load_and_prune`, so this helper guards the call.
    ///
    /// # Errors
    ///
    /// Same as [`Self::register`]; propagated unchanged when the file exists.
    /// Returns `Ok(())` immediately when the file is absent without touching
    /// the registry.
    pub async fn register_if_present(&self, ocx_lock_path: &Path) -> Result<(), Error> {
        match tokio::fs::metadata(ocx_lock_path).await {
            Ok(meta) if meta.is_file() => self.register(ocx_lock_path).await,
            Ok(_) | Err(_) => Ok(()),
        }
    }
}

// ── Unit tests ──────────────────────────────────────────────────────────────
//
// Contract-first specification tests for [`ProjectRegistry`]. Every test is
// written against the `unimplemented!()` stubs and is therefore expected to
// FAIL (panic) until the Unit 6 implementation phase fills the method bodies.
//
// Test inventory (per ADR "Testing Surface"):
//
//  1. `register_creates_file_when_absent`
//  2. `register_updates_last_seen_for_existing_entry`
//  3. `register_idempotent_for_same_path`
//  4. `register_atomic_under_concurrent_callers`
//  5. `load_and_prune_drops_entries_with_missing_lockfile`
//  6. `load_and_prune_no_write_when_nothing_to_prune`
//  7. `corrupt_registry_returns_corrupt_error`
//  8. `register_rejects_symlink_at_sentinel` (Unix-only)
//  9. `load_and_prune_drops_entry_with_missing_sibling_ocx_toml`
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::project::registry::error::ProjectRegistryErrorKind;

    // ── Helper ────────────────────────────────────────────────────────────────

    /// Write a minimal `ocx.toml` into `dir` so the registry prune rules see a
    /// sibling config file (Lazy Pruning Rule 3: "sibling ocx.toml must exist").
    fn write_ocx_toml(dir: &std::path::Path) {
        let toml_path = dir.join("ocx.toml");
        std::fs::write(toml_path, "[tools]\n").expect("write ocx.toml");
    }

    /// Create a real (empty) `ocx.lock` file at `path`, plus its sibling
    /// `ocx.toml`, so the registry sees a valid project directory.
    fn touch_lock_and_toml(lock_path: &std::path::Path) {
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
            write_ocx_toml(parent);
        }
        std::fs::write(lock_path, "").expect("write ocx.lock");
    }

    // ── Test 1 ────────────────────────────────────────────────────────────────

    /// `register` must create `projects.json` and `.projects.lock` when the
    /// registry is absent, and the JSON must contain exactly one entry whose
    /// `ocx_lock_path` matches the registered path.
    ///
    /// Failure signal: panics with `"Unit 6 implementation phase: register"`.
    #[tokio::test]
    async fn register_creates_file_when_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ocx_home = tmp.path().to_path_buf();

        let lock_dir = ocx_home.join("proj");
        let lock_path = lock_dir.join("ocx.lock");
        touch_lock_and_toml(&lock_path);

        let registry = ProjectRegistry::new(&ocx_home);
        registry.register(&lock_path).await.expect("register must succeed");

        // Both files must exist under OCX_HOME.
        let registry_file = ocx_home.join("projects.json");
        let sentinel = ocx_home.join(".projects.lock");
        assert!(
            registry_file.exists(),
            "projects.json must be created after first register"
        );
        assert!(sentinel.exists(), ".projects.lock sentinel must be created by register");

        // Deserialize and assert one entry with the expected path and a
        // parseable last_seen timestamp.
        let raw = std::fs::read(&registry_file).expect("read projects.json");
        let doc: serde_json::Value = serde_json::from_slice(&raw).expect("valid JSON");
        assert_eq!(doc["schema_version"], serde_json::json!(1), "schema_version must be 1");
        let entries = doc["entries"].as_array().expect("entries is an array");
        assert_eq!(entries.len(), 1, "exactly one entry after first register");
        let ocx_lock_path = PathBuf::from(entries[0]["ocx_lock_path"].as_str().expect("ocx_lock_path is a string"));
        assert_eq!(ocx_lock_path, lock_path, "ocx_lock_path must match registered path");

        let last_seen = entries[0]["last_seen"].as_str().expect("last_seen is a string");
        chrono::DateTime::parse_from_rfc3339(last_seen).expect("last_seen must be a parseable ISO-8601 UTC timestamp");
    }

    // ── Test 2 ────────────────────────────────────────────────────────────────

    /// Calling `register` twice for the same path must update `last_seen` to a
    /// later timestamp and leave exactly one entry in the registry.
    ///
    /// Failure signal: panics with `"Unit 6 implementation phase: register"`.
    #[tokio::test]
    async fn register_updates_last_seen_for_existing_entry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ocx_home = tmp.path().to_path_buf();

        let lock_path = ocx_home.join("proj").join("ocx.lock");
        touch_lock_and_toml(&lock_path);

        let registry = ProjectRegistry::new(&ocx_home);

        // First registration.
        registry.register(&lock_path).await.expect("first register");

        // Read first last_seen.
        let raw_before = std::fs::read(ocx_home.join("projects.json")).expect("read");
        let doc_before: serde_json::Value = serde_json::from_slice(&raw_before).expect("parse");
        let first_seen = doc_before["entries"][0]["last_seen"]
            .as_str()
            .expect("last_seen")
            .to_string();

        // Brief sleep to ensure clock advances (1 ms is enough for
        // ISO-8601 second-level granularity once the implementation uses
        // sub-second precision, but we use a longer delay to be safe).
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Second registration.
        registry.register(&lock_path).await.expect("second register");

        let raw_after = std::fs::read(ocx_home.join("projects.json")).expect("read");
        let doc_after: serde_json::Value = serde_json::from_slice(&raw_after).expect("parse");
        let entries = doc_after["entries"].as_array().expect("entries");

        // Still exactly one entry.
        assert_eq!(entries.len(), 1, "register must not create duplicate entries");

        let second_seen = entries[0]["last_seen"].as_str().expect("last_seen");

        // The second timestamp must be >= the first (strictly greater when clock
        // advances; equal is tolerated in case of sub-millisecond resolution).
        let t1 = chrono::DateTime::parse_from_rfc3339(&first_seen).expect("parse t1");
        let t2 = chrono::DateTime::parse_from_rfc3339(second_seen).expect("parse t2");
        assert!(t2 >= t1, "second last_seen ({t2}) must be >= first last_seen ({t1})");
    }

    // ── Test 3 ────────────────────────────────────────────────────────────────

    /// Registering the same path N times must leave exactly one entry.
    ///
    /// Failure signal: panics with `"Unit 6 implementation phase: register"`.
    #[tokio::test]
    async fn register_idempotent_for_same_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ocx_home = tmp.path().to_path_buf();

        let lock_path = ocx_home.join("proj").join("ocx.lock");
        touch_lock_and_toml(&lock_path);

        let registry = ProjectRegistry::new(&ocx_home);

        for _ in 0..5 {
            registry.register(&lock_path).await.expect("register");
        }

        let raw = std::fs::read(ocx_home.join("projects.json")).expect("read");
        let doc: serde_json::Value = serde_json::from_slice(&raw).expect("parse");
        let entries = doc["entries"].as_array().expect("entries");
        assert_eq!(
            entries.len(),
            1,
            "repeated register of same path must yield exactly one entry; got {}",
            entries.len()
        );
    }

    // ── Test 4 ────────────────────────────────────────────────────────────────

    /// Two concurrent `register` calls for *different* lock paths must both
    /// land in the registry. Uses a multi-thread runtime to exercise the
    /// advisory-lock serialisation path.
    ///
    /// Failure signal: panics with `"Unit 6 implementation phase: register"`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn register_atomic_under_concurrent_callers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ocx_home = tmp.path().to_path_buf();

        let lock_a = ocx_home.join("proj-a").join("ocx.lock");
        let lock_b = ocx_home.join("proj-b").join("ocx.lock");
        touch_lock_and_toml(&lock_a);
        touch_lock_and_toml(&lock_b);

        let registry = std::sync::Arc::new(ProjectRegistry::new(&ocx_home));
        let ra = registry.clone();
        let rb = registry.clone();
        let pa = lock_a.clone();
        let pb = lock_b.clone();

        let (res_a, res_b) = tokio::join!(
            async move { ra.register(&pa).await },
            async move { rb.register(&pb).await },
        );
        res_a.expect("register proj-a must succeed");
        res_b.expect("register proj-b must succeed");

        let raw = std::fs::read(ocx_home.join("projects.json")).expect("read");
        let doc: serde_json::Value = serde_json::from_slice(&raw).expect("parse");
        let entries = doc["entries"].as_array().expect("entries");
        assert_eq!(
            entries.len(),
            2,
            "both concurrent registrations must land; got {} entries",
            entries.len()
        );

        let paths: std::collections::HashSet<PathBuf> = entries
            .iter()
            .map(|e| PathBuf::from(e["ocx_lock_path"].as_str().expect("path")))
            .collect();
        assert!(paths.contains(&lock_a), "proj-a must be in registry");
        assert!(paths.contains(&lock_b), "proj-b must be in registry");
    }

    // ── Test 5 ────────────────────────────────────────────────────────────────

    /// `load_and_prune` must drop an entry whose `ocx_lock_path` has been
    /// deleted from disk, persist the shrunken registry, and return only the
    /// surviving entry.
    ///
    /// Failure signal: panics with `"Unit 6 implementation phase: load_and_prune"`.
    #[tokio::test]
    async fn load_and_prune_drops_entries_with_missing_lockfile() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ocx_home = tmp.path().to_path_buf();

        let lock_a = ocx_home.join("proj-a").join("ocx.lock");
        let lock_b = ocx_home.join("proj-b").join("ocx.lock");
        touch_lock_and_toml(&lock_a);
        touch_lock_and_toml(&lock_b);

        let registry = ProjectRegistry::new(&ocx_home);
        registry.register(&lock_a).await.expect("register a");
        registry.register(&lock_b).await.expect("register b");

        // Capture mtime before deletion so we can verify the file was rewritten.
        let mtime_before = std::fs::metadata(ocx_home.join("projects.json"))
            .expect("stat projects.json before delete")
            .modified()
            .expect("mtime supported");

        // Delete proj-a's lock file: it should be pruned on the next load.
        std::fs::remove_file(&lock_a).expect("delete lock_a");

        // Brief sleep so the rewrite lands with a strictly later mtime.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let entries = registry.load_and_prune().await.expect("load_and_prune");

        // Only proj-b survives.
        assert_eq!(entries.len(), 1, "only one entry should survive prune");
        assert_eq!(entries[0].ocx_lock_path, lock_b, "surviving entry must be proj-b");

        // On-disk file must have been rewritten (mtime must advance).
        let mtime_after = std::fs::metadata(ocx_home.join("projects.json"))
            .expect("stat projects.json after prune")
            .modified()
            .expect("mtime after");
        assert!(
            mtime_after >= mtime_before,
            "projects.json must be rewritten after a prune (mtime must advance)"
        );

        // Deserialize and confirm on-disk content matches in-memory return.
        let raw = std::fs::read(ocx_home.join("projects.json")).expect("read after prune");
        let doc: serde_json::Value = serde_json::from_slice(&raw).expect("parse");
        let disk_entries = doc["entries"].as_array().expect("entries array");
        assert_eq!(disk_entries.len(), 1, "on-disk entries must also be 1 after prune");
    }

    // ── Test 6 ────────────────────────────────────────────────────────────────

    /// `load_and_prune` must NOT rewrite `projects.json` when nothing is pruned.
    ///
    /// Failure signal: panics with `"Unit 6 implementation phase: load_and_prune"`.
    #[cfg(unix)]
    #[tokio::test]
    async fn load_and_prune_no_write_when_nothing_to_prune() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ocx_home = tmp.path().to_path_buf();

        let lock_a = ocx_home.join("proj-a").join("ocx.lock");
        let lock_b = ocx_home.join("proj-b").join("ocx.lock");
        touch_lock_and_toml(&lock_a);
        touch_lock_and_toml(&lock_b);

        let registry = ProjectRegistry::new(&ocx_home);
        registry.register(&lock_a).await.expect("register a");
        registry.register(&lock_b).await.expect("register b");

        // Capture mtime after registrations (both files on disk).
        let mtime_before = std::fs::metadata(ocx_home.join("projects.json"))
            .expect("stat")
            .modified()
            .expect("mtime");

        // Sleep briefly to ensure the clock advances — if the implementation
        // incorrectly rewrites, the mtime will change.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let entries = registry.load_and_prune().await.expect("load_and_prune");
        assert_eq!(entries.len(), 2, "no entries dropped when all locks exist");

        // mtime must be UNCHANGED: no rewrite should have occurred.
        let mtime_after = std::fs::metadata(ocx_home.join("projects.json"))
            .expect("stat after")
            .modified()
            .expect("mtime after");
        assert_eq!(
            mtime_before, mtime_after,
            "projects.json must NOT be rewritten when no entries are pruned"
        );
    }

    // ── Test 7 ────────────────────────────────────────────────────────────────

    /// `load_and_prune` must return `ProjectRegistryErrorKind::Corrupt` when
    /// `projects.json` exists but contains invalid JSON.
    ///
    /// Failure signal: panics with `"Unit 6 implementation phase: load_and_prune"`.
    #[tokio::test]
    async fn corrupt_registry_returns_corrupt_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ocx_home = tmp.path().to_path_buf();

        // Write garbage bytes to the registry file.
        std::fs::write(ocx_home.join("projects.json"), b"not valid json { garbage!!!").expect("write garbage");

        let registry = ProjectRegistry::new(&ocx_home);
        let err = registry
            .load_and_prune()
            .await
            .expect_err("load_and_prune must fail on corrupt file");

        // Verify the error kind — must be Corrupt, not Io.
        let Error::Registry(inner) = &err;
        let ProjectRegistryErrorKind::Corrupt(_) = &inner.kind else {
            panic!("expected ProjectRegistryErrorKind::Corrupt, got: {:?}", inner.kind);
        };
    }

    // ── Test 8 ────────────────────────────────────────────────────────────────

    /// On Unix, a symlink planted at `.projects.lock` must cause `register` to
    /// return an `Io` error rather than following the symlink.
    ///
    /// Mirrors `load_exclusive_rejects_symlink_at_sidecar_path` in `lock.rs`.
    ///
    /// Failure signal: panics with `"Unit 6 implementation phase: register"`.
    #[cfg(unix)]
    #[tokio::test]
    async fn register_rejects_symlink_at_sentinel() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let ocx_home = tmp.path().to_path_buf();
        let sentinel = ocx_home.join(".projects.lock");

        // Plant a symlink at the sentinel path pointing to a file outside
        // the OCX_HOME directory.
        let target = ocx_home.join("sensitive_target");
        symlink(&target, &sentinel).expect("create symlink at sentinel path");

        let lock_path = ocx_home.join("proj").join("ocx.lock");
        touch_lock_and_toml(&lock_path);

        let registry = ProjectRegistry::new(&ocx_home);
        let err = registry
            .register(&lock_path)
            .await
            .expect_err("register must fail when sentinel is a symlink");

        // Must be Io, not Locked (mirrors `load_exclusive_rejects_symlink_at_sidecar_path`).
        let Error::Registry(inner) = &err;
        let ProjectRegistryErrorKind::Io(_) = &inner.kind else {
            panic!("expected ProjectRegistryErrorKind::Io, got: {:?}", inner.kind);
        };

        // The symlink target must NOT have been created or touched.
        assert!(
            !target.exists(),
            "symlink target must not be created; found: {target:?}"
        );
    }

    // ── Test 9 ────────────────────────────────────────────────────────────────

    /// `load_and_prune` must drop an entry whose `ocx.lock` file exists but
    /// whose sibling `ocx.toml` does NOT exist (Lazy Pruning Rule 3).
    ///
    /// Failure signal: panics with `"Unit 6 implementation phase: load_and_prune"`.
    #[tokio::test]
    async fn load_and_prune_drops_entry_with_missing_sibling_ocx_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ocx_home = tmp.path().to_path_buf();

        // proj-a: lock exists, ocx.toml present → will be registered normally.
        let lock_a = ocx_home.join("proj-a").join("ocx.lock");
        touch_lock_and_toml(&lock_a);

        // proj-b: lock exists, ocx.toml will be deleted after registration.
        let lock_b = ocx_home.join("proj-b").join("ocx.lock");
        touch_lock_and_toml(&lock_b);

        let registry = ProjectRegistry::new(&ocx_home);
        registry.register(&lock_a).await.expect("register a");
        registry.register(&lock_b).await.expect("register b");

        // Remove proj-b's ocx.toml while keeping ocx.lock intact.
        let toml_b = ocx_home.join("proj-b").join("ocx.toml");
        std::fs::remove_file(&toml_b).expect("remove ocx.toml for proj-b");

        let entries = registry.load_and_prune().await.expect("load_and_prune");

        // Only proj-a survives (it still has ocx.toml).
        assert_eq!(
            entries.len(),
            1,
            "entry with missing ocx.toml must be pruned; got {} entries",
            entries.len()
        );
        assert_eq!(
            entries[0].ocx_lock_path, lock_a,
            "surviving entry must be proj-a (has ocx.toml)"
        );

        // Verify on-disk state matches.
        let raw = std::fs::read(ocx_home.join("projects.json")).expect("read after prune");
        let doc: serde_json::Value = serde_json::from_slice(&raw).expect("parse");
        let disk_entries = doc["entries"].as_array().expect("entries");
        assert_eq!(disk_entries.len(), 1, "on-disk registry must also contain 1 entry");
    }

    // ── Test 10 ───────────────────────────────────────────────────────────────

    /// `load_and_prune` must drop an entry where `ocx_lock_path` is a directory
    /// rather than a regular file (Lazy Pruning Rule 2).
    ///
    /// Defence against a directory or special file occupying the lock path:
    /// `entry_is_valid` checks `meta.is_file()` and returns `false` for
    /// non-regular-file paths. This test exercises that branch.
    #[tokio::test]
    async fn load_and_prune_drops_entry_with_directory_at_lockfile_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ocx_home = tmp.path().to_path_buf();

        // proj-a: valid lock + sibling ocx.toml — survives prune.
        let lock_a = ocx_home.join("proj-a").join("ocx.lock");
        touch_lock_and_toml(&lock_a);

        // proj-b: lock + sibling ocx.toml exist at registration time.
        let lock_b = ocx_home.join("proj-b").join("ocx.lock");
        touch_lock_and_toml(&lock_b);

        let registry = ProjectRegistry::new(&ocx_home);
        registry.register(&lock_a).await.expect("register a");
        registry.register(&lock_b).await.expect("register b");

        // Replace proj-b's lock file with a directory at the same path.
        // Remove the file first, then create a directory in its place.
        std::fs::remove_file(&lock_b).expect("remove lock_b file");
        std::fs::create_dir(&lock_b).expect("create directory at lock_b path");

        let entries = registry.load_and_prune().await.expect("load_and_prune");

        // Only proj-a survives — proj-b's path is a directory, not a regular file.
        assert_eq!(
            entries.len(),
            1,
            "entry with a directory at lockfile path must be pruned; got {} entries",
            entries.len()
        );
        assert_eq!(
            entries[0].ocx_lock_path, lock_a,
            "surviving entry must be proj-a (regular file)"
        );
    }

    // ── Test 11 ───────────────────────────────────────────────────────────────

    /// `load_and_prune` must return `ProjectRegistryErrorKind::UnknownVersion`
    /// when `projects.json` parses as valid JSON but carries a `schema_version`
    /// other than `1`.
    ///
    /// This is distinct from `corrupt_registry_returns_corrupt_error` (Test 7),
    /// which tests genuine JSON parse failures. This test exercises the explicit
    /// schema-version guard and must NOT return `Corrupt`.
    #[tokio::test]
    async fn unknown_schema_version_returns_unknown_version_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ocx_home = tmp.path().to_path_buf();

        // Write a syntactically valid registry with an unrecognised schema version.
        std::fs::write(
            ocx_home.join("projects.json"),
            br#"{"schema_version": 99, "entries": []}"#,
        )
        .expect("write registry with unknown version");

        let registry = ProjectRegistry::new(&ocx_home);
        let err = registry
            .load_and_prune()
            .await
            .expect_err("load_and_prune must fail on unknown schema version");

        // Must be UnknownVersion, not Corrupt and not Io.
        let Error::Registry(inner) = &err;
        let ProjectRegistryErrorKind::UnknownVersion { found, expected } = &inner.kind else {
            panic!(
                "expected ProjectRegistryErrorKind::UnknownVersion, got: {:?}",
                inner.kind
            );
        };
        assert_eq!(*found, 99, "found must be the version in the file");
        assert_eq!(*expected, 1, "expected must be SCHEMA_VERSION (1)");
    }
}
