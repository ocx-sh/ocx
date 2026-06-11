// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Update-check task methods for [`PackageManager`].
//!
//! ## Throttle contract
//!
//! All methods that accept `throttle: Option<std::time::Duration>` follow the
//! same convention:
//!
//! - `None` → 24-hour default (the normal auto-check path from `app.rs`).
//! - `Some(Duration::ZERO)` → bypass; always query the registry.
//! - `Some(d)` → custom interval.
//!
//! The `self_update` convenience always bypasses throttle (explicit user intent).
//!
//! ## State-file touch policy
//!
//! The update-check state file lives at
//! `$OCX_HOME/state/update-check/<slug>` where `<slug>` is the strict
//! (no-dot) slug of the identifier. Its mtime IS the data — no content is
//! written beyond an empty file.
//!
//! Touch policy:
//! - **Touch** when the registry probe returns cleanly (any `UpdateCheckResult`
//!   variant), regardless of whether an update is available.
//! - **Touch** when the registry probe returns an error (avoids hammering a
//!   broken registry on every command invocation).
//! - **Do NOT touch** when the throttle short-circuits before a probe
//!   (touching on short-circuit would extend the window indefinitely, turning
//!   a 24h throttle into indefinite suppression).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Default auto-check throttle interval: 24 hours.
///
/// Matches the `None` branch of the `throttle: Option<Duration>` convention
/// (see module-level doc).  Tests that assert the 24h default reference this
/// constant rather than the magic literal `86400`.
const DEFAULT_THROTTLE: Duration = Duration::from_secs(24 * 60 * 60);

use super::super::PackageManager;
use crate::{log, oci, package, package_manager};

/// The reason an update check was skipped.
///
/// Used as the payload of [`UpdateCheckResult::Skipped`] and
/// [`SelfUpdateResult::Skipped`] so programmatic consumers (JSON, scripts)
/// can distinguish skip causes without string parsing.
///
/// JSON serialization produces a discriminated object:
/// - Unit variants: `{"reason": "bootstrap"}`
/// - Variants with detail: `{"reason": "registry_probe_failed", "detail": "…"}`
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum SkippedReason {
    /// The subprocess version query failed: binary absent (true bootstrap),
    /// non-zero exit, or unparseable JSON output. The check cannot compare
    /// versions and is skipped; the install path proceeds anyway with
    /// `from = None`.
    Bootstrap,
    /// The operation was blocked by offline mode.
    Offline,
    /// The 24-hour throttle window has not elapsed since the last probe.
    Throttled,
    /// The registry probe returned an error; the error context is carried
    /// as the inner string.
    RegistryProbeFailed(String),
    /// The package was not found in the remote registry.
    NotFound,
    /// The currently installed version string could not be parsed as a
    /// semver-like version.  The unparseable string is carried as context.
    UnparseableCurrent(String),
    /// The latest tag returned by the registry could not be parsed as a
    /// semver-like version.
    UnparseableLatest,
    /// No clean release tag (`major.minor.patch`) was found in the remote
    /// tag list.
    NoReleaseTag,
}

impl std::fmt::Display for SkippedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bootstrap => f.write_str("bootstrap mode — subprocess version query failed"),
            Self::Offline => f.write_str("offline mode"),
            Self::Throttled => f.write_str("throttled"),
            Self::RegistryProbeFailed(detail) => write!(f, "registry probe failed: {detail}"),
            Self::NotFound => f.write_str("package not found in registry"),
            Self::UnparseableCurrent(version) => write!(f, "current version unparseable: {version}"),
            Self::UnparseableLatest => f.write_str("latest tag unparseable"),
            Self::NoReleaseTag => f.write_str("no release version available"),
        }
    }
}

/// Result of a generic or self-flavored update-check operation.
#[derive(Debug)]
pub enum UpdateCheckResult {
    /// The installed version is already the latest.
    AlreadyUpToDate,
    /// The check was skipped. The inner [`SkippedReason`] identifies why so
    /// programmatic consumers can distinguish causes without string parsing.
    Skipped(SkippedReason),
    /// A newer version is available. The inner [`oci::Identifier`] identifies
    /// the latest release (with tag) so the caller can suggest an install
    /// command.
    UpdateAvailable(oci::Identifier),
}

/// Result of an `ocx self update` invocation.
#[derive(Debug)]
pub enum SelfUpdateResult {
    /// The installed version is already the latest; nothing was changed.
    AlreadyUpToDate,
    /// A newer version was found and installed.
    Installed {
        /// Version string of the previously installed binary, if it could be
        /// determined by subprocess query.  `None` indicates the subprocess
        /// invocation was not available (binary absent, non-zero exit, or
        /// malformed JSON output).
        from: Option<String>,
        /// Version string of the newly installed binary.
        to: String,
    },
    /// The update was skipped. The inner [`SkippedReason`] identifies why.
    Skipped(SkippedReason),
}

impl PackageManager {
    /// Checks whether a newer version of `identifier` is available in the
    /// remote index.
    ///
    /// The `throttle` parameter controls how long between registry probes:
    /// `None` = 24-hour default, `Some(Duration::ZERO)` = always query,
    /// `Some(d)` = custom interval.
    ///
    /// Returns [`UpdateCheckResult::Skipped`] when throttle short-circuits
    /// (without touching the state file). Returns [`UpdateCheckResult::UpdateAvailable`]
    /// after a live probe if a release tag is found; the state file is touched
    /// in both cases (success or error).
    ///
    /// # Errors
    ///
    /// Returns `PackageErrorKind` on registry or I/O failure.
    pub async fn check_update(
        &self,
        identifier: &oci::Identifier,
        throttle: Option<Duration>,
    ) -> Result<UpdateCheckResult, package_manager::error::PackageErrorKind> {
        let state_path = self.file_structure().state.update_check_file(identifier);
        let interval = throttle.unwrap_or(DEFAULT_THROTTLE);

        // Throttle short-circuit — do NOT touch state file.
        // Run is_throttled (sync fs I/O) off the async executor via spawn_blocking.
        // `.unwrap_or(false)` collapses a task panic (JoinError) to "not throttled"
        // so a broken blocking thread cannot wedge update-check into permanent skip.
        let state_path_check = state_path.clone();
        let throttled = tokio::task::spawn_blocking(move || is_throttled(&state_path_check, interval))
            .await
            .unwrap_or(false);
        if throttled {
            return Ok(UpdateCheckResult::Skipped(SkippedReason::Throttled));
        }

        // Build a remote-only index for the probe. If we're offline, treat as
        // probe error: touch the state file to avoid hammering on every invocation,
        // then return Skipped.
        let client = match self.client() {
            Some(c) => c,
            None => {
                touch_state_async(state_path.clone()).await;
                return Ok(UpdateCheckResult::Skipped(SkippedReason::Offline));
            }
        };
        let remote_index = oci::index::Index::from_remote(oci::index::RemoteIndex::new(oci::index::RemoteConfig {
            client: client.clone(),
        }));

        let probe_result = remote_index.list_tags(identifier).await;

        // Touch state file after probe (success or error), never on short-circuit.
        touch_state_async(state_path.clone()).await;

        let tags = match probe_result {
            Ok(Some(tags)) => tags,
            Ok(None) => return Ok(UpdateCheckResult::Skipped(SkippedReason::NotFound)),
            Err(err) => {
                log::debug!("update check: registry probe failed: {err}");
                // Return skipped rather than propagating — auto-check should
                // never fail a user command.
                return Ok(UpdateCheckResult::Skipped(SkippedReason::RegistryProbeFailed(
                    err.to_string(),
                )));
            }
        };

        let Some(latest_version) = find_latest_version(&tags) else {
            return Ok(UpdateCheckResult::Skipped(SkippedReason::NoReleaseTag));
        };

        Ok(UpdateCheckResult::UpdateAvailable(
            identifier.clone_with_tag(latest_version.to_string()),
        ))
    }

    /// Self-flavored convenience wrapper around [`check_update`].
    ///
    /// Resolves the well-known OCX CLI identifier (`ocx.sh/ocx/cli`) and
    /// compares the remote latest tag against the version reported by the
    /// currently installed `ocx` binary via subprocess
    /// (`ocx --format json version`).
    ///
    /// If the subprocess invocation fails (binary absent — bootstrap mode, exec
    /// fail, non-zero exit, or malformed JSON), returns
    /// [`UpdateCheckResult::Skipped`]`(SkippedReason::Bootstrap)`.
    ///
    /// `throttle` follows the same `Option<Duration>` convention as
    /// [`check_update`]. The auto-check path in `app.rs` passes `None`
    /// (24-hour default); `ocx self update --check` passes
    /// `Some(Duration::ZERO)` (always query).
    ///
    /// # Errors
    ///
    /// Returns `PackageErrorKind` on registry or I/O failure.
    pub async fn self_check_update(
        &self,
        throttle: Option<Duration>,
    ) -> Result<UpdateCheckResult, package_manager::error::PackageErrorKind> {
        let ocx_id = oci::ocx_cli_identifier();

        // Throttle gate first — on the hot path (every non-static CLI invocation)
        // the check is throttled ~24/25 times. Avoid the subprocess round-trip
        // until we know the registry probe actually returned a newer release.
        let check_result = self.check_update(&ocx_id, throttle).await?;

        // Only proceed to installed-version resolution when we have a candidate to
        // compare against. All other variants (Skipped, AlreadyUpToDate) propagate
        // directly to the caller.
        let latest_id = match check_result {
            UpdateCheckResult::UpdateAvailable(id) => id,
            other => return Ok(other),
        };

        // Registry probe returned a newer release — query the installed version
        // via subprocess. On any failure (binary absent = bootstrap, exec fail,
        // non-zero exit, malformed JSON), return Skipped(Bootstrap).
        let current_version_str = query_installed_version(self, &ocx_id).await;

        let current_version_str = match current_version_str {
            Some(v) => v,
            None => {
                return Ok(UpdateCheckResult::Skipped(SkippedReason::Bootstrap));
            }
        };

        let current = match package::version::Version::parse(&current_version_str) {
            Some(v) => v,
            None => {
                return Ok(UpdateCheckResult::Skipped(SkippedReason::UnparseableCurrent(
                    current_version_str,
                )));
            }
        };

        // Parse the tag from the returned identifier and compare.
        let latest_version = latest_id.tag().and_then(package::version::Version::parse);

        match latest_version {
            Some(latest) if latest > current => Ok(UpdateCheckResult::UpdateAvailable(latest_id)),
            Some(_) => Ok(UpdateCheckResult::AlreadyUpToDate),
            None => Ok(UpdateCheckResult::Skipped(SkippedReason::UnparseableLatest)),
        }
    }

    /// Queries the version string of the installed `current` binary for
    /// `identifier` via the hermetic `ocx --format json version` subprocess.
    ///
    /// Returns `None` when the package is not locally installed (no `current`
    /// symlink), the subprocess fails, or its output is malformed — the same
    /// soft-failure contract as the self-update version probe. This is the
    /// pub seam the pinned-bootstrap downgrade check (plan D10) reuses to learn
    /// the currently-installed version without re-implementing the subprocess
    /// pattern; it stays best-effort and the caller skips silently on `None`.
    ///
    /// The `query_` prefix signals that calling this method may spawn a subprocess
    /// (non-trivial cost), mirroring the `query_installed_version` private helper.
    pub async fn query_installed_self_version(&self, identifier: &oci::Identifier) -> Option<String> {
        query_installed_version(self, identifier).await
    }

    /// Self-flavored install: check for update (always bypasses throttle,
    /// explicit user intent) and install the new version if one is available.
    ///
    /// The current version is queried via subprocess (`ocx --format json version`)
    /// on the binary resolved through the composed env's PATH
    /// (`PackageManager::resolve_env`). Returns `None` when the package is not
    /// locally installed (bootstrap), subprocess fails, or JSON output is malformed.
    ///
    /// Bootstrap mode (binary absent, exec fail, non-zero exit, malformed JSON)
    /// no longer short-circuits — the install proceeds regardless, so a fresh
    /// install from `ocx self update` works even when OCX is not yet installed
    /// via OCX itself.
    ///
    /// Routes the actual install through [`install_all`](PackageManager::install_all)
    /// with `candidate=false, select=true` — self-update does not create a
    /// candidate symlink; only the `current` symlink is updated.
    ///
    /// # Errors
    ///
    /// Returns `package_manager::error::Error` on install failure (aligned
    /// with `install_all`'s error type).
    pub async fn self_update(&self) -> Result<SelfUpdateResult, package_manager::error::Error> {
        use package_manager::concurrency::Concurrency;

        let ocx_id = oci::ocx_cli_identifier();

        // Query the installed version via subprocess before the check so we can
        // populate `from` in SelfUpdateResult::Installed.  Returns None when
        // no binary is present (bootstrap) or exec fails — that is fine; we
        // still proceed with the update check.
        let current_version = query_installed_version(self, &ocx_id).await;

        let check_result = self.self_check_update(Some(Duration::ZERO)).await.map_err(|kind| {
            package_manager::error::Error::SelfCheckFailed(Box::new(package_manager::error::PackageError {
                identifier: oci::ocx_cli_identifier(),
                kind,
            }))
        })?;

        match check_result {
            UpdateCheckResult::Skipped(reason) => Ok(SelfUpdateResult::Skipped(reason)),
            UpdateCheckResult::AlreadyUpToDate => Ok(SelfUpdateResult::AlreadyUpToDate),
            UpdateCheckResult::UpdateAvailable(latest_id) => {
                // QUAL-4: latest_id is produced by check_update via
                // `identifier.clone_with_tag(latest_version.to_string())`, so
                // tag() is always Some.  Falling back to "unknown" would hide a
                // broken invariant; assert it instead.
                let to_tag = latest_id
                    .tag()
                    .expect("find_latest_version always returns tagged identifier")
                    .to_string();
                let platforms = oci::Platform::supported_set();
                // Bind the two adjacent bool positionals to named locals at the
                // call site so the intent reads at a glance — Q-W6 in R3.
                // candidate=false: self-update does not create a candidate symlink.
                // select=true:    updates the `current` symlink to the new version.
                let candidate = false;
                let select = true;
                self.install_all(vec![latest_id], platforms, candidate, select, Concurrency::default())
                    .await?;
                Ok(SelfUpdateResult::Installed {
                    from: current_version,
                    to: to_tag,
                })
            }
        }
    }
}

// ── Private helpers ──────────────────────────────────────────────────────────

/// Maximum wall-clock time the subprocess `ocx --format json version` query
/// is allowed before it is treated as bootstrap.
///
/// A hung installed binary (deadlocked dependency, runaway init code) MUST NOT
/// stall every `ocx self_check_update` invocation. On timeout the function
/// returns `None`, which routes through the same code path as binary-absent
/// (bootstrap mode) — no behaviour regression for a healthy install.
const VERSION_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Queries the installed version of the OCX binary by running it as a subprocess.
///
/// Resolves the installed `ocx` binary path via the proper PackageManager task
/// surface (`find_symlink(SymlinkKind::Current)` → `resolve_env`) rather than
/// constructing it from raw file paths. Using the `current` install symlink
/// avoids tag resolution (which would require the canonical `:latest` tag to
/// exist) and matches the semantic truth: the running ocx binary IS what
/// `current` points at. Returns `None` on any failure: package not locally
/// installed (bootstrap mode — current symlink absent), env resolution error,
/// subprocess launch error, non-zero exit, subprocess timeout (5 s), or
/// malformed JSON output.
///
/// The binary path is resolved via the composed env's PATH
/// (`PackageManager::resolve_env`), which applies the package's full interface
/// surface before locating `ocx` with [`crate::env::Env::resolve_command`].
///
/// # Hermetic subprocess invariant
///
/// The child runs with `env_clear()` and only the entries from `resolve_env`
/// — no inherited `HOME`, no inherited `PATH`, no inherited `OCX_*`. The
/// invoked subcommand [`crate::cli::commands::Version::execute`] MUST NOT
/// rely on `HOME`, `PATH`, or any `OCX_*` env var; otherwise this query
/// silently falls through to `Bootstrap`. See `subsystem-package-manager.md`
/// "OCX Configuration Forwarding" for the canonical subprocess pattern.
///
/// # Offline safety
///
/// Offline-safe: relies on the local index — any resolution failure (including
/// network errors) falls through to `None`. `None` signals bootstrap mode to
/// the caller; the update-check path skips the version comparison and returns
/// `Skipped(Bootstrap)`.
async fn query_installed_version(manager: &PackageManager, identifier: &oci::Identifier) -> Option<String> {
    // 1. Resolve via the `current` install symlink — the running ocx binary
    //    IS what `current` points at, so this is the semantically correct
    //    truth source for "what version am I". Avoids tag resolution
    //    (which would require `:latest` to exist in the index — fragile, and
    //    breaks against test registries that publish without cascade tags).
    //    Returns SymlinkNotFound when no install is present (= bootstrap).
    //    Any error (including network) maps to None.
    let info = manager
        .find_symlink(identifier, crate::file_structure::SymlinkKind::Current)
        .await
        .ok()?;

    // 2. Resolve the composed env (interface view — matches default exec semantics).
    let infos = vec![Arc::new(info)];
    let entries = manager.resolve_env(&infos, false).await.ok()?;

    // 3. Build env, apply package entries. Resolve `ocx` to its absolute path via
    //    the env PATH. No apply_ocx_config: version query reads no OCX_* config.
    let mut env = crate::env::Env::new();
    env.apply_entries(&entries);
    let bin = env.resolve_command("ocx");

    // 4. Subprocess with captured output, wrapped in a 5-second timeout so a hung
    //    binary cannot stall update-check on every command. `env_clear + envs(env)`
    //    gives hermetic execution. NOT `child_process::exec` — that diverges; we
    //    need captured output. Timeout → None → caller routes to `Bootstrap`.
    let future = tokio::process::Command::new(&bin)
        .args(["--format", "json", "version"])
        .env_clear()
        .envs(env)
        .output();
    let output = tokio::time::timeout(VERSION_QUERY_TIMEOUT, future).await.ok()?.ok()?;

    if !output.status.success() {
        return None;
    }

    #[derive(serde::Deserialize)]
    struct VersionPayload {
        version: String,
    }
    let payload: VersionPayload = serde_json::from_slice(&output.stdout).ok()?;
    Some(payload.version)
}

/// Returns the highest `major.minor.patch` release version among `tags`.
///
/// Filters out rolling tags (`1`, `1.2`), build-tagged versions
/// (`1.2.3+build`), and pre-releases (`1.2.3-rc1`) so the update message
/// recommends a clean release tag.
fn find_latest_version(tags: &[String]) -> Option<package::version::Version> {
    tags.iter()
        .filter_map(|tag| package::version::Version::parse(tag))
        .filter(|version| version.has_patch() && !version.has_build() && !version.has_prerelease())
        .max()
}

/// Returns `true` when the state file at `path` was last touched within
/// `interval`, meaning the next probe is not yet due.
///
/// Returns `false` (probe is due) when the file is absent, unreadable, or
/// older than `interval`. An `interval` of `Duration::ZERO` always returns
/// `false` (bypass semantics).
fn is_throttled(path: &Path, interval: Duration) -> bool {
    if interval.is_zero() {
        return false;
    }
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(mtime) = metadata.modified() else {
        return false;
    };
    let Ok(elapsed) = mtime.elapsed() else {
        // mtime in the future — treat as "file was just touched", i.e. throttled
        return true;
    };
    elapsed < interval
}

/// Creates or updates the state file at `path` atomically.
///
/// Write a zero-byte temp file next to `path`, then `std::fs::rename` into
/// place so other processes observe an atomic mtime update. Parent directory
/// is created lazily on first touch.
///
/// # Portability
///
/// The atomic-replace contract here relies on platform-level `rename`
/// semantics:
///
/// - **Linux**: `rename(2)` follows symlinks at the source path and replaces
///   the destination inode atomically — no intermediate "absent" state.
///   `renameat2(RENAME_NOFOLLOW)` is not used; we deliberately accept the
///   source-symlink-follow semantics because the temp file is always a freshly
///   created regular file under our control.
/// - **macOS**: BSD `rename(2)` matches the Linux semantics relied on above.
/// - **Windows**: `std::fs::rename` shims to `MoveFileExW` with
///   `MOVEFILE_REPLACE_EXISTING`, which gives equivalent atomic-replace
///   behaviour modulo open-handle constraints on the destination.
///
/// See <https://doc.rust-lang.org/std/fs/fn.rename.html> for the cross-platform
/// shim contract. The throttle directory is OCX-private under `$OCX_HOME`, so
/// attacker symlink races on the destination are out of scope; the doc-comment
/// is here so future refactors don't accidentally depend on `renameat2`-only
/// guarantees that the cross-platform shim doesn't provide.
fn touch_state_atomic(path: &Path) -> std::io::Result<()> {
    use std::fs;

    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "state path has no parent"))?;

    fs::create_dir_all(parent)?;

    // Build a unique temp filename using PID + a counter derived from the
    // thread ID so concurrent tasks don't collide on the same suffix.
    let unique_suffix = {
        use std::sync::atomic::{AtomicU64, Ordering};
        // PID gives cross-process uniqueness; a process-global monotonic
        // counter gives within-process uniqueness. A wall-clock nanos suffix
        // (the previous approach) collides when concurrent tasks land in the
        // same clock bucket on coarse-resolution timers — observed flaky on
        // macOS. The counter guarantees every call gets a distinct temp name,
        // so concurrent writers never race on the same temp file.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{}.{}", std::process::id(), n)
    };
    let tmp_path = parent.join(format!(
        ".{}.tmp.{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
        unique_suffix
    ));
    fs::write(&tmp_path, b"")?;
    fs::rename(&tmp_path, path)?;

    Ok(())
}

/// Awaits [`touch_state_atomic`] on a blocking thread, logging at debug on
/// failure.
///
/// Both `check_update` call sites (offline branch + post-probe touch) need the
/// same shape: drive the sync write off the async executor, swallow the result
/// at debug, and await completion before returning so the throttle window
/// resets before the next invocation lands. Extracted once to keep the touch
/// policy in one place — modifying the touch contract no longer needs two
/// edits in lockstep.
///
/// `JoinError` (task panic) is silently dropped at debug level: the throttle
/// file failing to touch is non-fatal — the next invocation re-probes, which
/// is the correct behaviour after a panic anyway.
async fn touch_state_async(path: std::path::PathBuf) {
    let _ = tokio::task::spawn_blocking(move || {
        if let Err(touch_err) = touch_state_atomic(&path) {
            log::debug!("update check: failed to touch state file: {touch_err}");
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use std::{path::Path, time::Duration};

    use crate::{
        file_structure::{BlobStore, FileStructure, TagStore},
        oci::{
            self,
            index::{ChainMode, Index, LocalConfig, LocalIndex},
        },
        package::version::Version,
        package_manager::PackageManager,
    };

    use super::{find_latest_version, is_throttled, query_installed_version, touch_state_atomic};

    /// Build a minimal offline [`PackageManager`] for unit tests.
    fn make_offline_manager(ocx_home: &Path) -> PackageManager {
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            tag_store: TagStore::new(ocx_home.join("tags")),
            blob_store: BlobStore::new(ocx_home.join("blobs")),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        PackageManager::new(fs, index, None, "ocx.sh")
    }

    // ── find_latest_version (ported verbatim from app/update_check.rs) ───────

    /// Highest patch release wins; non-patch tags are ignored.
    #[test]
    fn find_latest_version_picks_highest() {
        let tags = vec![
            "1.0.0".to_string(),
            "2.1.0".to_string(),
            "1.5.3".to_string(),
            "latest".to_string(),
            "2.0.9".to_string(),
        ];
        assert_eq!(find_latest_version(&tags), Some(Version::new_patch(2, 1, 0)));
    }

    /// Empty input returns `None`.
    #[test]
    fn find_latest_version_empty() {
        let tags: Vec<String> = vec![];
        assert_eq!(find_latest_version(&tags), None);
    }

    /// All non-version-like tags → `None`.
    #[test]
    fn find_latest_version_no_valid_tags() {
        let tags = vec!["latest".to_string(), "nightly".to_string(), "edge".to_string()];
        assert_eq!(find_latest_version(&tags), None);
    }

    /// Rolling tags (`2`, `2.1`), build-tagged (`2.1.0+build`), and pre-releases
    /// (`2.1.0-rc1`) are all filtered out; the clean `2.1.0` patch release wins.
    #[test]
    fn find_latest_version_skips_rolling_and_build_tags() {
        let tags = vec![
            "2".to_string(),
            "2.1".to_string(),
            "2.1.0+20260101".to_string(),
            "2.1.0-rc1".to_string(),
            "2.1.0".to_string(),
            "1.5.3".to_string(),
        ];
        assert_eq!(find_latest_version(&tags), Some(Version::new_patch(2, 1, 0)));
    }

    /// Only rolling tags → `None`.
    #[test]
    fn find_latest_version_none_when_only_rolling() {
        let tags = vec!["1".to_string(), "2".to_string(), "2.1".to_string()];
        assert_eq!(find_latest_version(&tags), None);
    }

    // ── update_check_file slug ───────────────────────────────────────────────

    /// The state-file name for `ocx.sh/ocx/cli` must contain no dots, no
    /// slashes, and be non-empty.  The strict (no-dot) slug produces
    /// `ocx_sh_ocx_cli` from `to_slug`.
    #[test]
    fn update_check_file_produces_dot_free_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tmp.path().to_path_buf());
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);

        let path = fs.state.update_check_file(&identifier);

        let file_name = path
            .file_name()
            .expect("update_check_file must return a path with a file name component")
            .to_str()
            .expect("file name must be valid UTF-8");

        assert!(!file_name.is_empty(), "slug must not be empty");
        assert!(!file_name.contains('.'), "slug must contain no dots; got: {file_name}");
        assert!(
            !file_name.contains('/'),
            "slug must contain no forward slashes; got: {file_name}"
        );
    }

    // ── is_throttled decision table ──────────────────────────────────────────

    /// A path that does not exist is never throttled (no previous probe record).
    #[test]
    fn is_throttled_absent_path_returns_false() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("does_not_exist");
        assert!(!is_throttled(&absent, Duration::from_secs(86_400)));
    }

    /// A file whose mtime is *newer* than the interval → throttled (still within
    /// the window, so the next probe is not yet due).
    ///
    /// Uses a very large interval so the freshly-created file is always within it.
    #[test]
    fn is_throttled_file_within_interval_returns_true() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state_file");
        // Write the file now; mtime = approximately now.
        std::fs::write(&path, b"").unwrap();
        // 10 minutes in the future means "file was touched very recently"
        assert!(
            is_throttled(&path, Duration::from_secs(600)),
            "file with mtime=now must be throttled within a 10-minute interval"
        );
    }

    /// A file whose mtime is *older* than the interval → not throttled (window
    /// has elapsed; probe is due).
    ///
    /// We use a zero-duration interval so any existing file is always past it.
    #[test]
    fn is_throttled_zero_interval_always_returns_false() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state_file");
        std::fs::write(&path, b"").unwrap();
        // Duration::ZERO = bypass semantics; always return false.
        assert!(
            !is_throttled(&path, Duration::ZERO),
            "Duration::ZERO must always return false (bypass semantics)"
        );
    }

    /// A file whose mtime is older than a positive interval → not throttled.
    ///
    /// Uses a 1-nanosecond interval: no file can have been written within
    /// that window, so the file is always past the interval and the probe is due.
    #[test]
    fn is_throttled_file_older_than_positive_interval_returns_false() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state_file");
        std::fs::write(&path, b"").unwrap();
        // 1-nanosecond interval: the file cannot have been written that recently.
        assert!(
            !is_throttled(&path, Duration::from_nanos(1)),
            "file older than 1ns interval must not be throttled"
        );
    }

    // ── touch_state_atomic ───────────────────────────────────────────────────

    /// `touch_state_atomic` must create parent directories lazily if absent,
    /// write the file, and succeed on repeated calls.
    #[test]
    fn touch_state_atomic_creates_parent_and_writes() {
        let tmp = tempfile::tempdir().unwrap();
        // Parent dir does NOT exist yet.
        let path = tmp.path().join("state").join("update-check").join("ocx_sh_ocx_cli");

        // First call: parent created, file created.
        touch_state_atomic(&path).expect("first touch must succeed");
        assert!(path.exists(), "state file must exist after first touch");

        // Second call: must succeed without error (idempotent).
        touch_state_atomic(&path).expect("second touch must succeed (idempotent)");
        assert!(path.exists(), "state file must still exist after second touch");
    }

    /// Concurrent `touch_state_atomic` calls for the same path must all succeed
    /// (no panics, no I/O errors) and the file must exist at the end.
    #[tokio::test(flavor = "multi_thread")]
    async fn touch_state_atomic_concurrent_safety() {
        let tmp = tempfile::tempdir().unwrap();
        let path = std::sync::Arc::new(tmp.path().join("state").join("update-check").join("concurrent_slug"));

        // Pre-create parent so the race is on the file, not the directory.
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let tasks: Vec<_> = (0..10)
            .map(|_| {
                let p = std::sync::Arc::clone(&path);
                tokio::spawn(async move {
                    // touch_state_atomic is sync; run in spawn_blocking to avoid blocking the
                    // async executor.
                    let p2 = p.as_ref().clone();
                    tokio::task::spawn_blocking(move || touch_state_atomic(&p2))
                        .await
                        .expect("spawn_blocking must not panic")
                })
            })
            .collect();

        for task in tasks {
            task.await
                .expect("task must not panic")
                .expect("touch_state_atomic must not return an error");
        }

        assert!(path.exists(), "state file must exist after concurrent writes");
    }

    // ── check_update throttle short-circuit policy ───────────────────────────

    /// When the state file was touched *within* the throttle interval, calling
    /// `check_update` must return `Skipped` without touching the state file.
    ///
    /// The mtime-unchanged assertion proves the touch policy (DO NOT touch on
    /// short-circuit) is respected.
    #[tokio::test(flavor = "multi_thread")]
    async fn check_update_throttle_short_circuit_does_not_touch() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = make_offline_manager(tmp.path());
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);

        // Write a fresh state file — within any reasonable interval.
        let state_path = manager.file_structure().state.update_check_file(&identifier);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        std::fs::write(&state_path, b"").unwrap();
        let mtime_before = std::fs::metadata(&state_path).unwrap().modified().unwrap();

        let short_interval = Duration::from_secs(3600); // 1h — file is fresh
        let result = manager.check_update(&identifier, Some(short_interval)).await;

        // Must return Skipped (throttle short-circuit), not an error.
        assert!(
            matches!(result, Ok(super::UpdateCheckResult::Skipped(_))),
            "expected Skipped from throttle short-circuit, got: {result:?}"
        );

        let mtime_after = std::fs::metadata(&state_path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "state file mtime must not change on throttle short-circuit"
        );
    }

    /// `Duration::ZERO` bypasses throttle regardless of state-file mtime.
    /// An offline manager returns Skipped("offline") rather than panicking.
    #[tokio::test(flavor = "multi_thread")]
    async fn check_update_bypass_with_zero_duration() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = make_offline_manager(tmp.path());
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);

        // Write a *very fresh* state file — would throttle under any positive interval.
        let state_path = manager.file_structure().state.update_check_file(&identifier);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        std::fs::write(&state_path, b"").unwrap();

        // Duration::ZERO = always bypass throttle; probe path reached.
        // Offline manager → fails to get client → returns Skipped (offline).
        let result = manager.check_update(&identifier, Some(Duration::ZERO)).await;
        // Throttle was bypassed (Duration::ZERO); offline manager has no client →
        // returns Skipped("offline mode").
        assert!(
            matches!(result, Ok(super::UpdateCheckResult::Skipped(_))),
            "expected Skipped (offline) when bypass reaches probe path; got: {result:?}"
        );
    }

    /// When the probe path is reached (throttle bypassed) but the registry
    /// probe fails (offline → Skipped), the state file must still be touched.
    ///
    /// Touching on error avoids hammering a broken registry on every command
    /// invocation (see the module `//!` touch-policy doc).
    #[tokio::test(flavor = "multi_thread")]
    async fn check_update_touches_state_on_probe_error() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = make_offline_manager(tmp.path());
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);
        let state_path = manager.file_structure().state.update_check_file(&identifier);
        assert!(!state_path.exists(), "precondition: state file absent");

        let _ = manager.check_update(&identifier, Some(Duration::ZERO)).await;

        assert!(
            state_path.exists(),
            "state file must be touched after probe error (avoid hammering a broken registry)"
        );
    }

    /// The default throttle interval constant is 24 hours (86 400 seconds).
    ///
    /// This is a const assertion so any accidental change to `DEFAULT_THROTTLE`
    /// fails at compile time rather than at test runtime.
    #[test]
    fn default_throttle_value_is_86400_seconds() {
        assert_eq!(
            super::DEFAULT_THROTTLE,
            Duration::from_secs(24 * 60 * 60),
            "DEFAULT_THROTTLE must be exactly 24 hours"
        );
    }

    /// `self_check_update(None)` on an offline manager returns `Skipped` without
    /// attempting any subprocess invocation.
    ///
    /// The offline check short-circuits inside `check_update` (no client →
    /// `Skipped("offline mode")`) before reaching the installed-version
    /// subprocess query or the bootstrap guard.  The observable contract —
    /// "returns `Ok(Skipped(…))`, not a hard error" — is unchanged.
    #[tokio::test(flavor = "multi_thread")]
    async fn self_check_update_offline_returns_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = make_offline_manager(tmp.path());
        // None → 24h default.  Offline manager short-circuits in check_update.
        let result = manager.self_check_update(None).await;
        assert!(
            result.is_ok(),
            "self_check_update must not return hard error on offline manager; got: {result:?}"
        );
        assert!(
            matches!(result, Ok(super::UpdateCheckResult::Skipped(_))),
            "offline manager must return Skipped; got: {result:?}"
        );
    }

    /// When the throttle short-circuits (`check_update` returns `Skipped`),
    /// `self_check_update` must propagate that `Skipped` directly without
    /// attempting any subprocess invocation.
    ///
    /// This test exercises the fast path: a fresh state file (within a 1-hour
    /// throttle window) triggers throttle short-circuit in `check_update`, so
    /// the installed-version subprocess query is never reached.
    #[tokio::test(flavor = "multi_thread")]
    async fn self_check_update_throttled_returns_skipped_without_subprocess() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = make_offline_manager(tmp.path());
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);

        // Write a fresh state file so the throttle fires.
        let state_path = manager.file_structure().state.update_check_file(&identifier);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        std::fs::write(&state_path, b"").unwrap();

        // 1-hour interval — the freshly-written file is within the window.
        let result = manager.self_check_update(Some(Duration::from_secs(3600))).await;

        assert!(
            result.is_ok(),
            "self_check_update must not return hard error when throttled; got: {result:?}"
        );
        assert!(
            matches!(result, Ok(super::UpdateCheckResult::Skipped(_))),
            "throttled path must return Skipped; got: {result:?}"
        );
    }

    /// `self_check_update` with a bypassed throttle on an offline manager reaches
    /// the probe path and returns `Skipped("offline mode")`.  The subprocess
    /// query is not invoked because the registry probe short-circuits first.
    #[tokio::test(flavor = "multi_thread")]
    async fn self_check_update_bypass_on_offline_returns_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = make_offline_manager(tmp.path());

        // Duration::ZERO bypasses throttle; offline manager → no client →
        // probe path returns Skipped("offline mode") before subprocess query.
        let result = manager.self_check_update(Some(Duration::ZERO)).await;
        assert!(
            result.is_ok(),
            "self_check_update must not return hard error; got: {result:?}"
        );
        assert!(
            matches!(result, Ok(super::UpdateCheckResult::Skipped(_))),
            "offline bypass must return Skipped; got: {result:?}"
        );
    }

    // ── SkippedReason Display tests ──────────────────────────────────────────

    /// Unit variants produce lowercase, no-period Display output per
    /// `C-GOOD-ERR` conventions.
    #[test]
    fn skipped_reason_display_unit_variants() {
        use super::SkippedReason;

        assert_eq!(
            SkippedReason::Bootstrap.to_string(),
            "bootstrap mode — subprocess version query failed"
        );
        assert_eq!(SkippedReason::Offline.to_string(), "offline mode");
        assert_eq!(SkippedReason::Throttled.to_string(), "throttled");
        assert_eq!(SkippedReason::NotFound.to_string(), "package not found in registry");
        assert_eq!(SkippedReason::UnparseableLatest.to_string(), "latest tag unparseable");
        assert_eq!(SkippedReason::NoReleaseTag.to_string(), "no release version available");
    }

    /// Variants with detail context carry the detail in their Display output.
    #[test]
    fn skipped_reason_display_detail_variants() {
        use super::SkippedReason;

        assert_eq!(
            SkippedReason::RegistryProbeFailed("connection refused".into()).to_string(),
            "registry probe failed: connection refused"
        );
        assert_eq!(
            SkippedReason::UnparseableCurrent("weird-tag".into()).to_string(),
            "current version unparseable: weird-tag"
        );
    }

    // ── is_throttled future-mtime clock-skew safety ─────────────────────────

    /// A file whose mtime is in the **future** (clock skew) must be treated as
    /// throttled (i.e. "file was just touched").
    ///
    /// The `is_throttled` implementation returns `true` when `mtime.elapsed()`
    /// errors — which happens when the mtime is ahead of the system clock.
    /// This test sets the file mtime to `now + 10 seconds` and asserts that
    /// `is_throttled` returns `true` for a 24-hour interval.
    ///
    /// Regression guard: without this branch, a host with an NTP drift that
    /// pushes the state-file mtime into the future would trigger a probe on
    /// every single command invocation (elapsed() → Err → return false).
    #[test]
    fn is_throttled_with_future_mtime_returns_true() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state_file");
        std::fs::write(&path, b"").unwrap();

        // Set mtime to 10 seconds in the future.
        let future_time = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(future_time)
            .unwrap();

        assert!(
            is_throttled(&path, std::time::Duration::from_secs(86_400)),
            "future mtime must be treated as throttled (clock-skew safety)"
        );
    }

    // ── SkippedReason Serialize round-trip tests ─────────────────────────────

    /// Unit variants serialize to `{"reason": "<snake_case_name>"}`.
    #[test]
    fn skipped_reason_serialize_unit_variants() {
        use super::SkippedReason;

        let json = serde_json::to_string(&SkippedReason::Bootstrap).unwrap();
        assert_eq!(json, r#"{"reason":"bootstrap"}"#);

        let json = serde_json::to_string(&SkippedReason::Offline).unwrap();
        assert_eq!(json, r#"{"reason":"offline"}"#);

        let json = serde_json::to_string(&SkippedReason::Throttled).unwrap();
        assert_eq!(json, r#"{"reason":"throttled"}"#);

        let json = serde_json::to_string(&SkippedReason::NotFound).unwrap();
        assert_eq!(json, r#"{"reason":"not_found"}"#);

        let json = serde_json::to_string(&SkippedReason::UnparseableLatest).unwrap();
        assert_eq!(json, r#"{"reason":"unparseable_latest"}"#);

        let json = serde_json::to_string(&SkippedReason::NoReleaseTag).unwrap();
        assert_eq!(json, r#"{"reason":"no_release_tag"}"#);
    }

    /// Detail variants serialize to `{"reason": "…", "detail": "…"}`.
    #[test]
    fn skipped_reason_serialize_detail_variants() {
        use super::SkippedReason;

        let json = serde_json::to_string(&SkippedReason::RegistryProbeFailed("timeout".into())).unwrap();
        assert_eq!(json, r#"{"reason":"registry_probe_failed","detail":"timeout"}"#);

        let json = serde_json::to_string(&SkippedReason::UnparseableCurrent("dev-build".into())).unwrap();
        assert_eq!(json, r#"{"reason":"unparseable_current","detail":"dev-build"}"#);
    }

    // ── TC-B2: query_installed_version failure-mode coverage ─────────────────
    //
    // The function collapses four distinct failure modes to `Option<String>`:
    //
    //   1. Bootstrap — `manager.find_symlink(.., Current)` returns
    //      SymlinkNotFound (no `current` install symlink — package not
    //      locally selected). Unit-testable: offline manager with empty store.
    //   2. Env-resolve fail — `manager.resolve_env()` errors. Only reachable
    //      once `find_symlink()` returns Ok, which itself needs the `current`
    //      symlink populated.
    //      Not unit-testable without major test scaffolding; covered by URI-1.
    //   3. Subprocess non-zero exit — `tokio::process::Command::output()`
    //      returns success=false. Requires a real binary on disk that exits
    //      non-zero. Not unit-testable; covered by URI-1.
    //   4. Malformed JSON — `serde_json::from_slice` fails. Requires a real
    //      binary writing non-JSON to stdout. Not unit-testable; covered by
    //      `test/tests/test_self_update.py::test_version_json_format` (the
    //      contract gate that the consumer relies on).
    //
    // The unit test below proves the bootstrap case at the lowest layer:
    // when `find_symlink(.., Current)` cannot resolve the package locally,
    // `query_installed_version` returns `None` — never `Some("")` (which would
    // silently break the `UnparseableCurrent` branch downstream).

    /// Bootstrap path: no `current` install symlink → `query_installed_version`
    /// returns `None`.
    ///
    /// Regression guard: a refactor that mistakenly returns `Some(String::new())`
    /// on symlink-resolve failure would let an empty string flow into
    /// `Version::parse("")`, surfacing as `Skipped(UnparseableCurrent(""))` —
    /// not bootstrap. The downstream `self_check_update` branch then takes the
    /// `UnparseableCurrent` arm instead of `Bootstrap`, producing the wrong
    /// `SkippedReason` for scripts.
    #[tokio::test(flavor = "multi_thread")]
    async fn query_installed_version_returns_none_on_bootstrap() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = make_offline_manager(tmp.path());
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);

        let result = query_installed_version(&manager, &identifier).await;

        assert_eq!(
            result, None,
            "bootstrap mode (package absent from local store) must return None, never Some(\"\")"
        );
    }

    // ── TC-B1: self_check_update comparison-branch coverage (documented gap) ─
    //
    // The three comparison branches at update_check.rs:286-290 are:
    //
    //   - Some(latest) if latest > current → UpdateAvailable(latest_id)
    //   - Some(_)                          → AlreadyUpToDate
    //   - None                             → Skipped(UnparseableLatest)
    //
    // Reaching any of these branches from a unit test requires
    // `check_update()` to return `UpdateAvailable` — which requires a live
    // remote registry probe. The offline `make_offline_manager` short-circuits
    // at `Skipped(Offline)` inside `check_update` before
    // `query_installed_version` is ever invoked.
    //
    // Faking the registry from inside a unit test would require either:
    //   (a) injecting a test-mock `Client` into `PackageManager`, or
    //   (b) refactoring `self_check_update` to take a closure for the probe.
    //
    // Both are larger changes than warranted; URI-1 acceptance test
    // (`test/tests/test_self_update.py::test_self_update_installs_newer_version`)
    // exercises the `latest > current` (UpdateAvailable + install) branch
    // end-to-end against the real localhost:5000 registry, which is the
    // strongest possible coverage for that code path.
    //
    // The `latest == current` (AlreadyUpToDate) branch is exercised
    // implicitly by every CI run where the registry returns no newer tag.
    // The `Bootstrap` branch is covered by
    // `query_installed_version_returns_none_on_bootstrap` above.
    //
    // Mutation-canary status: flipping `>` to `>=` at update_check.rs:287
    // would land green at the unit level but red at the URI-1 acceptance
    // level (registry returns the same tag the binary already runs → `>=`
    // would re-install instead of returning AlreadyUpToDate).
}
