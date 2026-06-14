// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Delete-objects audit log (`$OCX_STATE_DIR/gc-log.jsonl`).
//!
//! Append-only JSONL file recording every object deleted (or `WouldDelete` in
//! dry-run mode) by `ocx clean`. One JSON record per line; each record carries
//! a `run_id` (UUID for the clean invocation), `instance_id`, `timestamp`,
//! `action`, and object-specific fields.
//!
//! ## Design properties
//!
//! - **Best-effort**: write failures emit `warn!` and are never fatal. A broken
//!   audit log must never abort a clean run.
//! - **Per-instance**: one file per `$OCX_STATE_DIR`. Cross-instance forensics
//!   correlate by `instance_id`.
//! - **Rotation**: at `OCX_GC_LOG_MAX_BYTES` (default 10 MiB) the active log
//!   is atomically renamed to `gc-log.jsonl.1` and a new file is started
//!   (one-generation rotation). The `.1` file is overwritten on the next
//!   rotation; it is a "previous run" snapshot, not an archive.
//! - **Disable**: `OCX_GC_LOG=off` disables all writes.
//!
//! ## Record schema (v1)
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "run_id": "<uuid-v4>",
//!   "instance_id": "<uuid from state/instance-id>",
//!   "timestamp": "<ISO-8601 UTC>",
//!   "action": "Deleted" | "WouldDelete",
//!   "object_kind": "Package" | "Layer" | "Blob" | "Temp",
//!   "path": "<absolute path>",
//!   "digest": "<hex or null>"
//! }
//! ```
//!
//! See `system_design_shared_store.md` §5 M4 item 5.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::env;
use crate::log;

/// Schema version for audit-log records. Increment on breaking schema change.
pub const SCHEMA_VERSION: u32 = 1;

/// Default maximum log file size before rotation (10 MiB).
pub const DEFAULT_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// The action recorded for a deleted object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditAction {
    /// The object was deleted from the store.
    Deleted,
    /// Dry-run mode: the object would have been deleted.
    WouldDelete,
}

/// Kind of object-store entry recorded in the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectKind {
    /// An assembled package directory under `packages/`.
    Package,
    /// An extracted layer directory under `layers/`.
    Layer,
    /// A raw OCI blob under `blobs/`.
    Blob,
    /// A stale temporary staging directory.
    Temp,
}

/// A single audit-log record (one JSONL line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Schema version — always [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// UUID identifying this `ocx clean` invocation (stable across all records
    /// in one run).
    pub run_id: String,
    /// UUID identifying this OCX instance (`$OCX_STATE_DIR/instance-id`).
    pub instance_id: String,
    /// ISO-8601 UTC timestamp of the delete/would-delete action.
    pub timestamp: String,
    /// Whether the object was actually deleted or only flagged (dry-run).
    pub action: AuditAction,
    /// Kind of object-store entry.
    pub object_kind: ObjectKind,
    /// Absolute path of the deleted / would-be-deleted entry directory.
    pub path: PathBuf,
    /// Hex digest of the entry, or `null` for temp entries without a digest.
    pub digest: Option<String>,
}

/// Append-only audit log writer.
///
/// Constructed once per `clean` invocation. Records are appended via
/// [`AuditLog::append`]. The log is flushed and rotated (if over-size) on
/// first open; rotation is checked lazily per append thereafter.
///
/// When `OCX_GC_LOG=off` all operations are no-ops.
pub struct AuditLog {
    /// Absolute path to `$OCX_STATE_DIR/gc-log.jsonl`.
    path: PathBuf,
    /// UUID for this clean invocation — stamped on every record.
    run_id: String,
    /// UUID of the OCX instance — stamped on every record.
    instance_id: String,
    /// Whether logging is enabled (`OCX_GC_LOG != "off"`).
    enabled: bool,
    /// Maximum file size before rotation.
    max_bytes: u64,
}

impl AuditLog {
    /// Async constructor — opens the audit log for `state_dir`, loading or
    /// creating the per-instance UUID from `$OCX_STATE_DIR/instance-id` via
    /// `spawn_blocking` so the Tokio worker is never blocked.
    ///
    /// Reads `OCX_GC_LOG` and `OCX_GC_LOG_MAX_BYTES` from the environment.
    ///
    /// Prefer this constructor in async contexts (e.g. `clean()`). Use
    /// [`Self::new_with_instance_id`] in tests or other contexts where the
    /// `instance_id` string is already available.
    pub async fn open(state_dir: &Path, run_id: String) -> Self {
        let instance_id = crate::utility::instance_id::load_or_create_instance_id(state_dir).await;
        Self::new_with_instance_id(state_dir, run_id, instance_id)
    }

    /// Constructs an [`AuditLog`] with a caller-supplied `instance_id`.
    ///
    /// No I/O is performed. Suitable for tests and for callers that have already
    /// loaded the instance UUID via an async path.
    ///
    /// Reads `OCX_GC_LOG` and `OCX_GC_LOG_MAX_BYTES` from the environment.
    pub fn new_with_instance_id(state_dir: &Path, run_id: String, instance_id: String) -> Self {
        let enabled = !logging_disabled();
        let max_bytes = max_bytes_from_env();
        Self {
            path: audit_log_path(state_dir),
            run_id,
            instance_id,
            enabled,
            max_bytes,
        }
    }

    /// Constructs a disabled [`AuditLog`] (all operations are no-ops).
    ///
    /// Useful when `OCX_GC_LOG=off` is set or as a default for testing.
    pub fn disabled() -> Self {
        Self {
            path: PathBuf::new(),
            run_id: String::new(),
            instance_id: String::new(),
            enabled: false,
            max_bytes: DEFAULT_LOG_MAX_BYTES,
        }
    }

    /// Appends a single [`AuditRecord`] to the log.
    ///
    /// Best-effort: any I/O failure is logged at `warn!` and swallowed — the
    /// audit log must never abort a `clean` run. Performs one-generation
    /// rotation if the log exceeds `max_bytes`.
    pub async fn append(&self, record: AuditRecord) {
        if !self.enabled {
            return;
        }
        if let Err(e) = self.append_inner(record).await {
            // Best-effort: a broken audit log must never abort a clean run.
            log::warn!("GC audit log append to '{}' failed: {e}", self.path.display());
        }
    }

    /// Fallible body of [`Self::append`]. Rotates if over-size, then appends one
    /// JSONL line. Errors are caught and downgraded to a WARN by the caller.
    async fn append_inner(&self, record: AuditRecord) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await?;
        }

        self.rotate_if_oversize().await?;

        let mut line = serde_json::to_string(&record).map_err(std::io::Error::other)?;
        line.push('\n');

        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }

    /// One-generation rotation: when the active log is at or over `max_bytes`,
    /// rename it to `gc-log.jsonl.1` (overwriting any previous rotation) and
    /// start a fresh log.
    async fn rotate_if_oversize(&self) -> std::io::Result<()> {
        let size = match tokio::fs::metadata(&self.path).await {
            Ok(meta) => meta.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if size < self.max_bytes {
            return Ok(());
        }
        let rotated = rotated_path(&self.path);
        tokio::fs::rename(&self.path, &rotated).await
    }

    /// Convenience: constructs and appends an [`AuditRecord`] for `path`.
    ///
    /// Infers `object_kind` from the path's position in the store layout
    /// (`packages/`, `layers/`, `blobs/`, or temp). `digest` may be `None`
    /// for temp entries.
    pub async fn record_delete(&self, action: AuditAction, object_kind: ObjectKind, path: &Path, digest: Option<&str>) {
        if !self.enabled {
            return;
        }
        let record = AuditRecord {
            schema_version: SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            instance_id: self.instance_id.clone(),
            timestamp: now_iso8601(),
            action,
            object_kind,
            path: path.to_path_buf(),
            digest: digest.map(str::to_string),
        };
        self.append(record).await;
    }

    /// Returns the `run_id` UUID for this clean invocation.
    #[allow(dead_code)] // public accessor; correlation API for forensic consumers.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Returns the `instance_id` UUID.
    #[allow(dead_code)] // public accessor; correlation API for forensic consumers.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Returns whether the audit log is enabled.
    #[allow(dead_code)] // public accessor; used by tests and external consumers.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Returns the path to the audit log file for `state_dir`.
///
/// `$OCX_STATE_DIR/gc-log.jsonl`
pub fn audit_log_path(state_dir: &Path) -> PathBuf {
    state_dir.join("gc-log.jsonl")
}

/// Returns the path to the rotated (previous) audit log file.
///
/// `$OCX_STATE_DIR/gc-log.jsonl.1`
#[allow(dead_code)] // public path accessor; rotation impl uses `rotated_path`.
pub fn audit_log_rotated_path(state_dir: &Path) -> PathBuf {
    rotated_path(&audit_log_path(state_dir))
}

/// Returns the rotated-log path for an active log path
/// (`gc-log.jsonl` → `gc-log.jsonl.1`).
fn rotated_path(active: &Path) -> PathBuf {
    let mut os = active.as_os_str().to_os_string();
    os.push(".1");
    PathBuf::from(os)
}

/// Generates a fresh `run_id` for a single `clean` invocation.
///
/// Reuses the shared UUID-v4 generator (`crate::utility::instance_id`) — the
/// single source of truth, also used by the per-instance id and the
/// shared-roots ledger.
pub fn new_run_id() -> String {
    crate::utility::instance_id::new_uuid_v4()
}

/// Returns `true` when `OCX_GC_LOG` is set to `off` (case-insensitive).
fn logging_disabled() -> bool {
    env::var(env::keys::OCX_GC_LOG)
        .map(|value| value.trim().eq_ignore_ascii_case("off"))
        .unwrap_or(false)
}

/// Reads `OCX_GC_LOG_MAX_BYTES`, falling back to [`DEFAULT_LOG_MAX_BYTES`].
fn max_bytes_from_env() -> u64 {
    env::var(env::keys::OCX_GC_LOG_MAX_BYTES)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_LOG_MAX_BYTES)
}

/// Current time as an ISO-8601 UTC timestamp (`2026-01-01T00:00:00Z`).
fn now_iso8601() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::*;

    fn make_record(action: AuditAction, object_kind: ObjectKind, path: PathBuf, digest: Option<&str>) -> AuditRecord {
        AuditRecord {
            schema_version: SCHEMA_VERSION,
            run_id: "run-001".to_string(),
            instance_id: "inst-001".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            action,
            object_kind,
            path,
            digest: digest.map(|s| s.to_string()),
        }
    }

    // ── AuditLog::disabled ───────────────────────────────────────────────────

    #[test]
    fn disabled_audit_log_is_not_enabled() {
        // Requirement: system_design_shared_store.md §5 M4 item 5 —
        // OCX_GC_LOG=off → AuditLog::disabled() is a no-op with is_enabled()==false.
        // Traced to: plan_shared_store P3.2s audit-log "disabled() is a no-op".
        let log = AuditLog::disabled();
        assert!(!log.is_enabled(), "disabled log must have is_enabled() == false");
    }

    // ── AuditLog::new ───────────────────────────────────────────────────────

    #[test]
    fn new_audit_log_is_enabled_by_default() {
        // Requirement: system_design_shared_store.md §5 M4 item 5 —
        // OCX_GC_LOG absent (or not "off") → audit log is enabled.
        // Traced to: plan_shared_store P3.2s audit-log "append writes JSONL record".
        let env = crate::test::env::lock();
        env.remove(crate::env::keys::OCX_GC_LOG);
        let dir = tempdir().unwrap();
        let log = AuditLog::new_with_instance_id(dir.path(), "run-001".to_string(), "inst-test".to_string());
        assert!(
            log.is_enabled(),
            "default audit log must be enabled when OCX_GC_LOG is not set"
        );
    }

    #[test]
    fn new_audit_log_stores_run_id_and_instance_id() {
        // Requirement: system_design_shared_store.md §5 M4 item 5 —
        // each record carries run_id and instance_id.
        // Traced to: plan_shared_store P3.2s audit-log.
        let env = crate::test::env::lock();
        env.remove(crate::env::keys::OCX_GC_LOG);
        let log = AuditLog::new_with_instance_id(
            std::path::Path::new("/tmp/nonexistent"),
            "my-run-id".to_string(),
            "my-instance-id".to_string(),
        );
        assert_eq!(log.run_id(), "my-run-id");
        assert_eq!(
            log.instance_id(),
            "my-instance-id",
            "instance_id must match the value passed in"
        );
    }

    // ── AuditLog::append ────────────────────────────────────────────────────

    #[tokio::test]
    async fn append_writes_jsonl_record_to_log_file() {
        // Requirement: system_design_shared_store.md §5 M4 item 5 —
        // append writes a JSONL record; the log file must exist after append.
        // Traced to: plan_shared_store P3.2s audit-log "append writes JSONL record".
        let env = crate::test::env::lock();
        env.remove(crate::env::keys::OCX_GC_LOG);
        let dir = tempdir().unwrap();
        let log = AuditLog::new_with_instance_id(dir.path(), "run-abc".to_string(), "inst-test".to_string());

        let record = make_record(
            AuditAction::Deleted,
            ObjectKind::Package,
            dir.path().join("packages/sha256/ab/cdef"),
            Some("abcdef1234567890"),
        );
        log.append(record).await;

        let log_path = audit_log_path(dir.path());
        assert!(log_path.exists(), "gc-log.jsonl must exist after append");

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(!content.is_empty(), "log file must contain the written record");
        // Verify it parses as valid JSONL.
        for line in content.lines() {
            let _: AuditRecord = serde_json::from_str(line).expect("each line must parse as a valid AuditRecord");
        }
    }

    #[tokio::test]
    async fn append_to_disabled_log_is_noop() {
        // Requirement: system_design_shared_store.md §5 M4 item 5 —
        // OCX_GC_LOG=off → disabled log; append must not write any file.
        // Traced to: plan_shared_store P3.2s audit-log "disabled() is a no-op".
        let dir = tempdir().unwrap();
        let log = AuditLog::disabled();

        let record = make_record(
            AuditAction::Deleted,
            ObjectKind::Blob,
            dir.path().join("blobs/sha256/00/1122"),
            Some("001122"),
        );
        log.append(record).await;

        let log_path = audit_log_path(dir.path());
        assert!(!log_path.exists(), "disabled log must not write gc-log.jsonl");
    }

    // ── dry-run emits WouldDelete, not Deleted ───────────────────────────────

    #[tokio::test]
    async fn dry_run_record_uses_would_delete_action() {
        // Requirement: system_design_shared_store.md §5 M4 item 5 —
        // dry-run mode logs `WouldDelete` not `Deleted`.
        // Traced to: plan_shared_store P3.2s audit-log "dry-run emits WouldDelete".
        let env = crate::test::env::lock();
        env.remove(crate::env::keys::OCX_GC_LOG);
        let dir = tempdir().unwrap();
        let log = AuditLog::new_with_instance_id(dir.path(), "run-dry".to_string(), "inst-test".to_string());

        log.record_delete(
            AuditAction::WouldDelete,
            ObjectKind::Layer,
            &dir.path().join("layers/sha256/aa/bb"),
            Some("aabb"),
        )
        .await;

        let content = std::fs::read_to_string(audit_log_path(dir.path())).unwrap();
        let record: AuditRecord = serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(
            record.action,
            AuditAction::WouldDelete,
            "dry-run record must use WouldDelete action"
        );
    }

    // ── null-digest (Temp kind) ──────────────────────────────────────────────

    #[tokio::test]
    async fn null_digest_temp_record_serializes_correctly() {
        // Requirement: system_design_shared_store.md §5 M4 item 5 —
        // Temp entries may have `digest: null` in the JSONL record.
        // Traced to: plan_shared_store P3.2s audit-log "null-digest record (Temp kind)".
        let env = crate::test::env::lock();
        env.remove(crate::env::keys::OCX_GC_LOG);
        let dir = tempdir().unwrap();
        let log = AuditLog::new_with_instance_id(dir.path(), "run-temp".to_string(), "inst-test".to_string());

        log.record_delete(
            AuditAction::Deleted,
            ObjectKind::Temp,
            &dir.path().join("temp/__stale_1234_abcd"),
            None, // No digest for temp entries.
        )
        .await;

        let content = std::fs::read_to_string(audit_log_path(dir.path())).unwrap();
        let record: AuditRecord = serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(record.object_kind, ObjectKind::Temp);
        assert!(record.digest.is_none(), "Temp record must have null digest");
    }

    // ── rotation at max_bytes ────────────────────────────────────────────────

    #[tokio::test]
    async fn rotation_creates_dot_1_file_when_log_exceeds_max_bytes() {
        // Requirement: system_design_shared_store.md §5 M4 item 5 —
        // at OCX_GC_LOG_MAX_BYTES the active log is renamed to gc-log.jsonl.1
        // and a new log is started.
        // Traced to: plan_shared_store P3.2s audit-log "rotation at DEFAULT_LOG_MAX_BYTES".
        let env = crate::test::env::lock();
        env.remove(crate::env::keys::OCX_GC_LOG);
        // Set max bytes to 1 byte to force rotation on the second append.
        env.set(crate::env::keys::OCX_GC_LOG_MAX_BYTES, "1");
        let dir = tempdir().unwrap();
        let log = AuditLog::new_with_instance_id(dir.path(), "run-rotate".to_string(), "inst-test".to_string());

        // First append creates the log.
        log.record_delete(
            AuditAction::Deleted,
            ObjectKind::Package,
            &dir.path().join("packages/sha256/aa/bb"),
            Some("aabb"),
        )
        .await;

        // Second append should trigger rotation (log is > 1 byte).
        log.record_delete(
            AuditAction::Deleted,
            ObjectKind::Package,
            &dir.path().join("packages/sha256/cc/dd"),
            Some("ccdd"),
        )
        .await;

        // After rotation, gc-log.jsonl.1 must exist.
        let rotated_path = audit_log_rotated_path(dir.path());
        assert!(
            rotated_path.exists(),
            "gc-log.jsonl.1 must exist after rotation triggered by OCX_GC_LOG_MAX_BYTES=1"
        );
    }

    // ── failure isolation (best-effort) ─────────────────────────────────────

    #[tokio::test]
    async fn append_failure_does_not_panic_or_propagate() {
        // Requirement: system_design_shared_store.md §5 M4 item 5 —
        // "best-effort: any I/O failure is logged at warn! and swallowed —
        //  the audit log must never abort a clean run."
        // Traced to: plan_shared_store P3.2s audit-log "failure isolation".
        //
        // We simulate a path that cannot be written to by constructing the
        // AuditLog with an existing file where a directory must be created.
        // The AuditLog must not panic.
        let env = crate::test::env::lock();
        env.remove(crate::env::keys::OCX_GC_LOG);
        let dir = tempdir().unwrap();

        // Create a file at the path where gc-log.jsonl would be created's parent dir,
        // causing an I/O error on open. We write a file named "gc-log.jsonl" as a
        // directory placeholder (use a non-writable path).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Make the state dir read-only so writes fail.
            let state_dir = dir.path().join("readonly-state");
            std::fs::create_dir(&state_dir).unwrap();
            std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
            let log = AuditLog::new_with_instance_id(&state_dir, "run-fail".to_string(), "inst-test".to_string());
            let record = make_record(
                AuditAction::Deleted,
                ObjectKind::Blob,
                state_dir.join("blobs/sha256/00/11"),
                Some("0011"),
            );
            // Must not panic even though the log file cannot be written.
            log.append(record).await;
            // Restore permissions for cleanup.
            std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        #[cfg(not(unix))]
        {
            // On non-Unix platforms we just verify the append signature compiles
            // and the disabled() path is a no-op (already tested above).
            let _ = AuditLog::disabled();
        }
    }

    // ── audit_log_path / audit_log_rotated_path ──────────────────────────────

    #[test]
    fn audit_log_path_is_gc_log_jsonl() {
        // Traced to: plan_shared_store P3.2s audit-log schema.
        let state_dir = PathBuf::from("/var/ocx/state");
        assert_eq!(audit_log_path(&state_dir), state_dir.join("gc-log.jsonl"));
    }

    #[test]
    fn audit_log_rotated_path_is_gc_log_jsonl_dot_1() {
        // Traced to: plan_shared_store P3.2s audit-log rotation.
        let state_dir = PathBuf::from("/var/ocx/state");
        assert_eq!(audit_log_rotated_path(&state_dir), state_dir.join("gc-log.jsonl.1"));
    }

    // ── load_or_create_instance_id (relocated to crate::utility::instance_id) ─
    //
    // The instance-id load-or-create logic moved to `crate::utility::instance_id`
    // (P3.6) so the shared-roots ledger and the audit log share one source of
    // truth. These tests still pin the contract from the audit-log perspective
    // (P3.2s) — they exercise the relocated function the audit log consumes.
    use crate::utility::instance_id::load_or_create_instance_id;

    #[tokio::test]
    async fn load_or_create_instance_id_creates_file_on_first_call() {
        // Requirement: system_design_shared_store.md §5 M4 item 5 —
        // `$OCX_STATE_DIR/instance-id` is created on first call.
        // Traced to: plan_shared_store P3.2s audit-log (instance_id in records).
        let dir = tempdir().unwrap();
        let id_path = dir.path().join("instance-id");
        assert!(!id_path.exists(), "instance-id must not pre-exist");

        let id = load_or_create_instance_id(dir.path()).await;
        assert!(!id.is_empty(), "returned instance_id must not be empty");
        assert!(id_path.exists(), "instance-id file must be created");
    }

    #[tokio::test]
    async fn load_or_create_instance_id_is_stable_across_calls() {
        // The same UUID must be returned on repeated calls to the same state_dir.
        // Traced to: plan_shared_store P3.2s audit-log.
        let dir = tempdir().unwrap();
        let id_first = load_or_create_instance_id(dir.path()).await;
        let id_second = load_or_create_instance_id(dir.path()).await;
        assert_eq!(id_first, id_second, "instance_id must be stable across calls");
    }

    // ── schema constants ─────────────────────────────────────────────────────

    #[test]
    fn schema_version_is_1() {
        // Traced to: plan_shared_store P3.2s audit-log schema.
        assert_eq!(SCHEMA_VERSION, 1u32, "schema version must be 1");
    }

    #[test]
    fn default_log_max_bytes_is_10_mib() {
        // 10 MiB = 10 * 1024 * 1024.
        // Traced to: plan_shared_store P3.2s audit-log rotation.
        assert_eq!(DEFAULT_LOG_MAX_BYTES, 10 * 1024 * 1024);
    }
}
