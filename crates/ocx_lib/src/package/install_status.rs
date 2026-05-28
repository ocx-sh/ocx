// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use crate::log;
use crate::utility::fs::LockedJsonFile;

#[derive(Clone, Deserialize, Serialize)]
pub struct InstallStatus {
    /// The timestamp of the installation attempt.
    pub timestamp: chrono::DateTime<chrono::Utc>,

    /// Defaults to false, and should be set to true by the installer when the installation is complete and successful.
    pub ok: bool,
}

impl Default for InstallStatus {
    fn default() -> Self {
        Self {
            timestamp: chrono::Utc::now(),
            ok: false,
        }
    }
}

impl InstallStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ok(self) -> Self {
        Self { ok: true, ..self }
    }
}

/// Probes whether `status_path` records a successful install
/// (`status.ok == true`).
///
/// Coordinates with concurrent writers via a shared advisory lock acquired
/// through [`LockedJsonFile`]. The status file IS the lock target — no
/// sidecar. Three outcomes collapse to `false`:
///
/// - File absent (no install attempt yet) → `false`.
/// - File exists but unparseable (kill-9 mid-write left partial JSON) →
///   `false`; the inline [`LockedJsonFile::read`] kill-9-recovery contract
///   surfaces a `warn` log.
/// - Lock acquisition failed (e.g. permission denied) → `false` + debug log.
///
/// Returns `true` only when the file exists, parses, and `status.ok` is set.
/// A concurrent writer holding the exclusive lock will block the shared
/// acquisition until its `replace_bytes` write completes, so a partial-write
/// window cannot escape this probe.
pub async fn check_install_status(status_path: impl AsRef<std::path::Path>) -> bool {
    let status_path = status_path.as_ref();
    let mut locked = match LockedJsonFile::<InstallStatus>::open_shared(status_path).await {
        Ok(Some(locked)) => locked,
        Ok(None) => return false, // file absent — no install attempt yet
        Err(error) => {
            log::debug!(
                "Failed to acquire shared lock on install status '{}': {}",
                status_path.display(),
                error
            );
            return false;
        }
    };
    match locked.read().await {
        Ok(Some(status)) => status.ok,
        Ok(None) => false, // empty or unparseable — treat as not-installed
        Err(error) => {
            log::debug!(
                "Failed to read install status from '{}': {}",
                status_path.display(),
                error
            );
            false
        }
    }
}
