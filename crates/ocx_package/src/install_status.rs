// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use ocx_util::fs::LockedJsonFile;

#[derive(Clone, Deserialize, Serialize)]
pub struct InstallStatus {
    pub timestamp: chrono::DateTime<chrono::Utc>,

    /// Set only once the installation completed successfully.
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

/// Whether `status_path` records a successful install; an absent, unparseable
/// or unlockable file yields `false`.
///
/// Reads under a shared lock on the status file itself, so a writer's partial
/// write is never observed.
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
