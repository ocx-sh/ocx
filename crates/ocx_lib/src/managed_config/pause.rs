// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pause state for the managed-config background tick.
//!
//! `ocx config update --pause <duration>` writes a content-bearing
//! `pause.json` beside the snapshot; the background tick short-circuits while
//! it is in force. Pause affects the **tick only** — the required gate and
//! explicit `ocx config update` are never blocked by it. An expired or
//! corrupt pause file reads as absent (benign-state rule) and is overwritten
//! by the next `--pause` / cleared by the next explicit update.

use crate::file_structure::StateStore;

/// Hard ceiling for `--pause <duration>` (7 days). A pause is a temporary
/// hold, not an opt-out — `refresh = "manual"` is the permanent form.
pub const MAX_PAUSE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(7 * 86_400);

/// On-disk pause state at [`StateStore::managed_config_pause_file`]
/// (`$OCX_HOME/state/managed-config/pause.json`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ManagedConfigPause {
    /// ISO-8601 UTC instant until which the background tick is paused.
    pub paused_until: String,
    /// The version spec pinned alongside the pause
    /// (`ocx config update --pause <d> <VERSION>`), for `--check` reporting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_version: Option<String>,
}

impl ManagedConfigPause {
    /// Builds a pause lasting `duration` from now.
    ///
    /// The domain layer owns the 7-day invariant: `duration` is clamped to
    /// [`MAX_PAUSE_INTERVAL`] before the window is computed, so a caller that
    /// bypasses the clap-side ceiling still cannot write an over-cap pause.
    pub fn for_duration(duration: std::time::Duration, pinned_version: Option<String>) -> Self {
        let capped = duration.min(MAX_PAUSE_INTERVAL);
        let paused_until =
            chrono::Utc::now() + chrono::Duration::from_std(capped).unwrap_or_else(|_| chrono::Duration::days(7));
        Self {
            paused_until: paused_until.to_rfc3339(),
            pinned_version,
        }
    }
}

/// Reads the pause file; an absent, corrupt, unparsable-timestamp, or
/// **expired** pause reads as `None` (debug-logged, never an error).
pub async fn read_pause(state: &StateStore) -> Option<ManagedConfigPause> {
    let bytes = tokio::fs::read(state.managed_config_pause_file()).await.ok()?;
    let pause: ManagedConfigPause = match serde_json::from_slice(&bytes) {
        Ok(pause) => pause,
        Err(error) => {
            crate::log::debug!("managed-config pause file is corrupt, treating as absent: {error}");
            return None;
        }
    };
    let until = match chrono::DateTime::parse_from_rfc3339(&pause.paused_until) {
        Ok(until) => until,
        Err(error) => {
            crate::log::debug!("managed-config pause timestamp is unparsable, treating as absent: {error}");
            return None;
        }
    };
    let now = chrono::Utc::now();
    if until <= now {
        crate::log::debug!(
            "managed-config pause expired at {}, treating as absent",
            pause.paused_until
        );
        return None;
    }
    // A window beyond `now + MAX_PAUSE_INTERVAL` cannot have come from
    // `for_duration` (which clamps): treat the file as tampered/corrupt and
    // ignore it. Reject rather than clamp-and-keep — a rolling read-time clamp
    // would let a far-future stamp perpetually re-satisfy the window and never
    // expire.
    let ceiling = now + chrono::Duration::from_std(MAX_PAUSE_INTERVAL).unwrap_or_else(|_| chrono::Duration::days(7));
    if until > ceiling {
        crate::log::debug!(
            "managed-config pause window {} exceeds the {}-day maximum, treating as corrupt",
            pause.paused_until,
            MAX_PAUSE_INTERVAL.as_secs() / 86_400
        );
        return None;
    }
    Some(pause)
}

/// Writes `pause` atomically (temp+rename, `spawn_blocking`) to
/// [`StateStore::managed_config_pause_file`].
///
/// # Errors
///
/// Any I/O failure while creating the directory or persisting the file.
pub async fn write_pause(state: &StateStore, pause: &ManagedConfigPause) -> std::io::Result<()> {
    use std::io::Write as _;

    let dir = state.managed_config_dir();
    let path = state.managed_config_pause_file();
    let bytes = serde_json::to_vec_pretty(pause)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;

    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
        tmp.write_all(&bytes)?;
        crate::utility::fs::persist_temp_file(tmp, &path)
    })
    .await
    .map_err(|join_error| std::io::Error::other(join_error.to_string()))?
}

/// Removes the pause file. Absent is success (idempotent clears).
///
/// # Errors
///
/// Any I/O failure other than the file not existing.
pub async fn clear_pause(state: &StateStore) -> std::io::Result<()> {
    match tokio::fs::remove_file(state.managed_config_pause_file()).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(dir: &tempfile::TempDir) -> StateStore {
        StateStore::new(dir.path())
    }

    #[tokio::test]
    async fn pause_round_trips_through_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        let pause =
            ManagedConfigPause::for_duration(std::time::Duration::from_secs(3600), Some("user-1.4.2".to_string()));

        write_pause(&state, &pause).await.expect("write must succeed");
        let read = read_pause(&state).await.expect("an in-force pause must read back");
        assert_eq!(read, pause);
    }

    #[tokio::test]
    async fn absent_pause_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_pause(&state(&dir)).await.is_none());
    }

    #[tokio::test]
    async fn expired_pause_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        let expired = ManagedConfigPause {
            paused_until: (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
            pinned_version: None,
        };
        write_pause(&state, &expired).await.unwrap();
        assert!(
            read_pause(&state).await.is_none(),
            "an expired pause must read as absent"
        );
    }

    #[tokio::test]
    async fn corrupt_pause_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        std::fs::create_dir_all(state.managed_config_dir()).unwrap();
        std::fs::write(state.managed_config_pause_file(), b"{not json").unwrap();
        assert!(read_pause(&state).await.is_none(), "corrupt JSON must read as absent");

        std::fs::write(
            state.managed_config_pause_file(),
            b"{\"paused_until\":\"not-a-timestamp\"}",
        )
        .unwrap();
        assert!(
            read_pause(&state).await.is_none(),
            "an unparsable timestamp must read as absent"
        );
    }

    #[test]
    fn for_duration_clamps_over_cap_request_to_max() {
        // A caller requesting more than the 7-day ceiling is clamped by the
        // domain layer, independent of the clap-side `--pause` check.
        let pause = ManagedConfigPause::for_duration(std::time::Duration::from_secs(30 * 86_400), None);
        let until = chrono::DateTime::parse_from_rfc3339(&pause.paused_until).unwrap();
        let ceiling =
            chrono::Utc::now() + chrono::Duration::from_std(MAX_PAUSE_INTERVAL).unwrap() + chrono::Duration::seconds(5);
        assert!(
            until <= ceiling,
            "a 30-day pause request must be clamped to the 7-day maximum, got {}",
            pause.paused_until
        );
    }

    #[tokio::test]
    async fn read_pause_rejects_window_beyond_max_as_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        // A hand-tampered file whose window is far beyond the 7-day ceiling — a
        // value `for_duration` could never have written. Rejected as corrupt,
        // not clamped (a rolling clamp would never let it expire).
        let tampered = ManagedConfigPause {
            paused_until: (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339(),
            pinned_version: None,
        };
        write_pause(&state, &tampered).await.unwrap();
        assert!(
            read_pause(&state).await.is_none(),
            "a pause window beyond the 7-day maximum must read as absent"
        );
    }

    #[tokio::test]
    async fn clear_pause_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        clear_pause(&state).await.expect("clearing an absent pause succeeds");

        let pause = ManagedConfigPause::for_duration(std::time::Duration::from_secs(60), None);
        write_pause(&state, &pause).await.unwrap();
        clear_pause(&state).await.expect("clear must succeed");
        assert!(read_pause(&state).await.is_none());
        clear_pause(&state).await.expect("double clear must succeed");
    }
}
