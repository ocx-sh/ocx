// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-instance UUID resolution for the state zone (`$OCX_STATE_DIR/instance-id`).
//!
//! The instance-id is advisory correlation metadata, NOT a correctness
//! invariant: it labels which OCX instance produced a GC audit-log record
//! (`audit_log`) and which instance owns a shared-roots ledger directory
//! (`project::shared_roots`). Both consumers must agree on the same file, so the
//! load-or-create logic lives here as the single source of truth — no
//! per-subsystem reimplementation.
//!
//! The file holds one UUID-v4 string, written once on first create
//! (tempfile + same-dir rename) and read on every subsequent invocation. Any
//! I/O error returns a fresh per-run UUID (best-effort; the id is advisory).

use std::path::{Path, PathBuf};

use crate::log;

/// Loads or creates the per-instance UUID from `$OCX_STATE_DIR/instance-id`.
///
/// Async entry point for callers already on the reactor; delegates to the
/// blocking implementation off the runtime so the small read/create never
/// stalls async work. Returns a fresh per-run UUID on any I/O failure
/// (best-effort — the instance-id is advisory metadata, not a correctness
/// invariant).
pub async fn load_or_create_instance_id(state_dir: &Path) -> String {
    let state_dir = state_dir.to_path_buf();
    tokio::task::spawn_blocking(move || load_or_create_instance_id_blocking(&state_dir))
        .await
        .unwrap_or_else(|_| new_uuid_v4())
}

/// Synchronous load-or-create. The sole source of truth shared by the async
/// wrapper above and by non-async constructors (e.g. `AuditLog::new`).
pub fn load_or_create_instance_id_blocking(state_dir: &Path) -> String {
    let id_path = instance_id_path(state_dir);
    if let Ok(bytes) = std::fs::read(&id_path) {
        let existing = String::from_utf8_lossy(&bytes).trim().to_string();
        if !existing.is_empty() {
            return existing;
        }
    }
    let id = new_uuid_v4();
    if let Some(parent) = id_path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        log::debug!("Could not create instance-id parent '{}' ({e}).", parent.display());
        return id;
    }
    if let Err(e) = atomic_write_blocking(&id_path, id.as_bytes()) {
        log::debug!(
            "Could not persist instance-id to '{}' ({e}); using a per-run id.",
            id_path.display()
        );
    }
    id
}

/// Returns the path to the per-instance id file (`$OCX_STATE_DIR/instance-id`).
fn instance_id_path(state_dir: &Path) -> PathBuf {
    state_dir.join("instance-id")
}

/// Atomically writes `bytes` to `path` via a PID-suffixed temp file + rename.
fn atomic_write_blocking(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let temp = match parent {
        Some(dir) => dir.join(format!(".instance-id.{}.tmp", std::process::id())),
        None => PathBuf::from(format!(".instance-id.{}.tmp", std::process::id())),
    };
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_data()?;
    }
    std::fs::rename(&temp, path)
}

/// Generates a random UUID-v4-formatted string (`8-4-4-4-12` lowercase hex).
///
/// The project carries no `uuid`/`rand` dependency; entropy is derived from a
/// SHA-256 over the process id, a high-resolution timestamp, a process-local
/// atomic counter, and the thread id. The version (4) and variant (RFC 4122)
/// nibbles are set so the output is a well-formed v4 string. This is advisory
/// correlation metadata, not a cryptographic identifier.
pub fn new_uuid_v4() -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut hasher = Sha256::new();
    hasher.update(std::process::id().to_le_bytes());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    hasher.update(nanos.to_le_bytes());
    hasher.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    hasher.update(format!("{:?}", std::thread::current().id()).as_bytes());
    let hash = hasher.finalize();

    let mut b = [0u8; 16];
    b.copy_from_slice(&hash[..16]);
    // Set version (4) and RFC 4122 variant.
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uuid_v4_is_well_formed_and_unique() {
        let a = new_uuid_v4();
        let b = new_uuid_v4();
        assert_ne!(a, b, "two generated ids must differ");
        // 8-4-4-4-12 = 36 chars incl. dashes; version nibble must be 4.
        assert_eq!(a.len(), 36, "uuid must be 36 chars: {a}");
        assert_eq!(a.as_bytes()[14], b'4', "version nibble must be 4: {a}");
    }

    #[test]
    fn load_or_create_persists_and_reuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = load_or_create_instance_id_blocking(dir.path());
        let second = load_or_create_instance_id_blocking(dir.path());
        assert_eq!(first, second, "second load must reuse the persisted id");
        assert!(
            dir.path().join("instance-id").is_file(),
            "instance-id file must be persisted"
        );
    }
}
