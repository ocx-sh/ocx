// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use crate::{ErrorExt, file_lock, log, prelude::SerdeExt};

#[derive(Clone, Deserialize, Serialize)]
pub struct InstallStatus {
    /// The timestamp of the installation attempt.
    pub timestamp: chrono::DateTime<chrono::Utc>,

    /// Defaults to false, and should be set to true by the installer when the installation is complete and successful.
    pub ok: bool,
}

impl Default for InstallStatus {
    fn default() -> Self {
        Self::new()
    }
}

impl InstallStatus {
    pub fn new() -> Self {
        Self {
            timestamp: chrono::Utc::now(),
            ok: false,
        }
    }

    pub fn ok(self) -> Self {
        Self { ok: true, ..self }
    }
}

/// Checks the installation status by attempting to acquire a shared lock on the lock file and reading the status file.
/// Returns a tuple of (ok, status), where ok is true if the package has been successfully installed already.
pub async fn check_install_status(
    status_path: impl AsRef<std::path::Path>,
    lock_path: impl AsRef<std::path::Path>,
    timeout: std::time::Duration,
) -> (bool, Option<InstallStatus>) {
    let status_path = status_path.as_ref();
    let lock_path = lock_path.as_ref();

    let lock_file_handle = match std::fs::File::create(lock_path) {
        Ok(lock_file) => lock_file,

        Err(error) => {
            log::debug!(
                "Failed to create install status lock file '{}': {}",
                lock_path.display(),
                error
            );
            return (false, None);
        }
    };
    let shared_lock = match file_lock::FileLock::lock_shared_with_timeout(lock_file_handle, timeout)
        .await
        .map_to_undefined_error()
    {
        Ok(shared_lock) => shared_lock,
        Err(error) => {
            log::debug!("Failed to acquire shared lock: {}", error);
            return (false, None);
        }
    };
    let status = match InstallStatus::read_json_from_path(status_path) {
        Ok(status) => status,
        Err(error) => {
            log::debug!(
                "Failed to read install status from file '{}': {}",
                status_path.display(),
                error
            );
            return (false, None);
        }
    };
    drop(shared_lock);
    (status.ok, Some(status))
}
