// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Update-check task methods for [`PackageManager`].
//!
//! `throttle`: `None` is the 24-hour default, `Some(Duration::ZERO)` bypasses, `Some(d)` is a custom interval.
//! The state file at `$OCX_HOME/state/update-check/<slug>` carries no content; only its mtime matters.

use std::sync::Arc;
use std::time::Duration;

/// Default auto-check throttle interval, the shared refresh default.
fn default_throttle() -> Duration {
    ocx_config::refresh::parse_interval(ocx_config::refresh::DEFAULT_INTERVAL).unwrap_or(Duration::from_secs(86_400))
}

use super::super::PackageManager;

/// The reason an update check was skipped.
///
/// Programmatic consumers (JSON, scripts) can distinguish skip causes without
/// string parsing. JSON serialization produces a discriminated object:
/// - Unit variants: `{"reason": "bootstrap"}`
/// - Variants with detail: `{"reason": "registry_probe_failed", "detail": "…"}`
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum SkippedReason {
    /// The subprocess version query failed: binary absent (true bootstrap),
    /// non-zero exit, or unparseable JSON output. The check cannot compare
    /// versions and is skipped; the install proceeds anyway, without a `from`
    /// version.
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
    /// The check was skipped, and why.
    Skipped(SkippedReason),
    /// A newer version is available; the identifier carries the latest release tag.
    UpdateAvailable(ocx_oci::PackageRef),
}

/// Why the hand-off to the newly pulled binary's own `ocx self setup` did not
/// complete cleanly.
///
/// Does not by itself mean the update was not installed: a child that failed
/// late may already have swapped the binary.
///
/// Serialized as a discriminated object: `{"reason": "exited", "detail": 82}`.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
// Never the verdict: the `current` symlink decides installed vs pulled; this value only picks the advice.
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum HandoffFailure {
    /// The new binary could not be started at all — it could not be resolved
    /// out of the package just pulled, or the spawn itself failed. The inner
    /// string carries the underlying error context.
    SpawnFailed(String),
    /// The child ran and exited non-zero, carrying its exit code. `82`
    /// (`DirtyRcBlock`) is the expected one: the setup completed the swap but
    /// left a user-edited shell profile alone.
    Exited(i32),
    /// The child was killed by a signal (Unix only), carrying the signal
    /// number.
    Signalled(i32),
}

impl std::fmt::Display for HandoffFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpawnFailed(detail) => write!(f, "could not run the new binary: {detail}"),
            Self::Exited(code) => write!(f, "setup exited {code}"),
            Self::Signalled(signal) => write!(f, "setup was killed by signal {signal}"),
        }
    }
}

/// Result of an `ocx self update` invocation.
#[derive(Debug)]
pub enum SelfUpdateResult {
    /// The installed version is already the latest; nothing was changed.
    AlreadyUpToDate,
    /// A newer version was pulled and `current` now names it: the update succeeded.
    Installed {
        /// Version of the previously installed binary; `None` when the subprocess query failed.
        from: Option<String>,
        /// Version string of the newly installed binary.
        to: String,
        /// `Some` when the new binary's setup failed after swapping `current`; the update stands and the
        /// caller advises re-running `ocx self setup`.
        handoff: Option<HandoffFailure>,
    },
    /// A newer version was pulled but `current` still names the old install; nothing was activated.
    Pulled {
        /// Version of the still-installed binary; `None` when the subprocess query failed.
        from: Option<String>,
        /// Version string of the release that was pulled but not activated.
        to: String,
        /// How the hand-off ended; `None` means the child reported success yet `current` did not move.
        handoff: Option<HandoffFailure>,
    },
    /// The update was skipped, and why.
    Skipped(SkippedReason),
}

/// Where the update-check probe lists candidate tags from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagProbe {
    /// Resolve through the configured index and `ChainMode`, honouring `--offline`/`--frozen`/`--remote`.
    Index,
    /// A live listing through the configured chain ([`Index::remote_view`](ocx_index::Index::remote_view)),
    /// never a bare tags-API call: `ocx.sh/ocx/cli` is a logical name a bare probe resolves to the wrong
    /// repository, capping the visible releases. Offline short-circuits to [`SkippedReason::Offline`].
    Remote,
}

impl PackageManager {
    /// Checks whether a newer version of `identifier` is available from `probe`'s tag source.
    ///
    /// A throttle short-circuit leaves the state file untouched; any probe, failed or not, touches it.
    ///
    /// # Errors
    ///
    /// Returns `PackageErrorKind` on registry or I/O failure.
    pub async fn check_update(
        &self,
        identifier: &ocx_oci::PackageRef,
        throttle: Option<Duration>,
        probe: TagProbe,
    ) -> Result<UpdateCheckResult, crate::error::PackageErrorKind> {
        let state_path = self.file_structure().state.update_check_file(identifier);
        let interval = throttle.unwrap_or_else(default_throttle);

        // A panicked blocking task reads as not throttled, or update-check wedges into permanent skip.
        let state_path_check = state_path.clone();
        let throttled = tokio::task::spawn_blocking(move || {
            ocx_store::file_structure::StateStore::is_throttled(&state_path_check, interval)
        })
        .await
        .unwrap_or(false);
        if throttled {
            // No touch here, or each short-circuit extends the window into indefinite suppression.
            return Ok(UpdateCheckResult::Skipped(SkippedReason::Throttled));
        }

        let probe_result = match probe {
            TagProbe::Remote => {
                // Touched even offline, or every invocation re-probes.
                if self.client().is_none() {
                    ocx_store::file_structure::StateStore::touch(state_path.clone()).await;
                    return Ok(UpdateCheckResult::Skipped(SkippedReason::Offline));
                }
                self.index().remote_view().list_tags(identifier).await
            }
            TagProbe::Index => self.index().list_tags(identifier).await,
        };

        ocx_store::file_structure::StateStore::touch(state_path.clone()).await;

        let tags = match probe_result {
            Ok(Some(tags)) => tags,
            Ok(None) => return Ok(UpdateCheckResult::Skipped(SkippedReason::NotFound)),
            Err(err) => {
                log::debug!("update check: registry probe failed: {err}");
                // Skipped, not propagated: the auto-check must never fail a user command.
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

    /// [`check_update`] for `ocx.sh/ocx/cli`, comparing the latest tag against the installed binary's
    /// `ocx --format json version`; a failed query is `Skipped(SkippedReason::Bootstrap)`.
    ///
    /// # Errors
    ///
    /// Returns `PackageErrorKind` on registry or I/O failure.
    pub async fn self_check_update(
        &self,
        throttle: Option<Duration>,
        probe: TagProbe,
    ) -> Result<UpdateCheckResult, crate::error::PackageErrorKind> {
        let ocx_id = ocx_oci::ocx_cli_identifier();

        // Check first: the hot path is throttled ~24/25 times and must not pay for the subprocess.
        let check_result = self.check_update(&ocx_id, throttle, probe).await?;

        let latest_id = match check_result {
            UpdateCheckResult::UpdateAvailable(id) => id,
            other => return Ok(other),
        };

        let current_version_str = query_installed_version(self, &ocx_id).await;

        let current_version_str = match current_version_str {
            Some(v) => v,
            None => {
                return Ok(UpdateCheckResult::Skipped(SkippedReason::Bootstrap));
            }
        };

        let current = match ocx_package::version::Version::parse(&current_version_str) {
            Some(v) => v,
            None => {
                return Ok(UpdateCheckResult::Skipped(SkippedReason::UnparseableCurrent(
                    current_version_str,
                )));
            }
        };

        let latest_version = latest_id.tag().and_then(ocx_package::version::Version::parse);

        match latest_version {
            Some(latest) if latest > current => Ok(UpdateCheckResult::UpdateAvailable(latest_id)),
            Some(_) => Ok(UpdateCheckResult::AlreadyUpToDate),
            None => Ok(UpdateCheckResult::Skipped(SkippedReason::UnparseableLatest)),
        }
    }

    /// Version of the installed `current` binary for `identifier`, via a hermetic `ocx --format json version`
    /// subprocess; `None` when not installed, the subprocess fails, or its output is malformed.
    pub async fn query_installed_self_version(&self, identifier: &ocx_oci::PackageRef) -> Option<String> {
        query_installed_version(self, identifier).await
    }

    /// Pulls the release [`TagProbe::Remote`] finds, bypassing the throttle, then re-executes it as its
    /// own `ocx self setup --handoff`; the verdict is the `current` symlink, not the child's exit status.
    ///
    /// # Errors
    ///
    /// `crate::error::Error` on install failure; a failed hand-off is reported as
    /// [`SelfUpdateResult::Pulled`] or an [`Installed`](SelfUpdateResult::Installed) carrying a [`HandoffFailure`].
    pub async fn self_update(&self) -> Result<SelfUpdateResult, crate::error::Error> {
        use crate::concurrency::Concurrency;

        let ocx_id = ocx_oci::ocx_cli_identifier();

        let current_version = query_installed_version(self, &ocx_id).await;

        let check_result = self
            .self_check_update(Some(Duration::ZERO), TagProbe::Remote)
            .await
            .map_err(|kind| {
                crate::error::Error::SelfCheckFailed(Box::new(crate::error::PackageError {
                    identifier: ocx_oci::ocx_cli_identifier(),
                    kind,
                }))
            })?;

        match check_result {
            UpdateCheckResult::Skipped(reason) => Ok(SelfUpdateResult::Skipped(reason)),
            UpdateCheckResult::AlreadyUpToDate => Ok(SelfUpdateResult::AlreadyUpToDate),
            UpdateCheckResult::UpdateAvailable(latest_id) => {
                let to_tag = latest_id
                    .tag()
                    .expect("find_latest_version always returns tagged identifier")
                    .to_string();
                let platform = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);
                let candidate = false;
                // The new binary's setup selects, or `current` swaps before any setup surface is written
                // and a failed setup strands a half-migrated machine.
                let select = false;
                // Patch discovery for ocx itself could abort the update with a spurious required-companion error.
                let skip_discovery = true;
                let infos = self
                    .install_all(
                        vec![latest_id],
                        platform,
                        candidate,
                        select,
                        Concurrency::default(),
                        skip_discovery,
                    )
                    .await?;
                let info = infos
                    .into_iter()
                    .next()
                    .expect("install_all returns one InstallInfo per requested package");

                let handoff = hand_off_setup(self, &info, &to_tag).await;
                let moved = current_names(self.file_structure(), &ocx_id, info.dir().root()).await;

                Ok(self_update_verdict(moved, handoff, current_version, to_tag))
            }
        }
    }
}

// ── Private helpers ──────────────────────────────────────────────────────────

/// Wall-clock limit on the version query, so a hung installed binary cannot stall every update check.
const VERSION_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Queries the installed version of the OCX binary by running it; `None` on any failure.
///
/// Runs with `env_clear()` plus `resolve_env`'s entries only, so `version::Version::execute` must not read
/// `HOME`/`PATH`/`OCX_*` or this falls through to `Bootstrap`. See `subsystem-package-manager.md`
/// "OCX Configuration Forwarding".
async fn query_installed_version(manager: &PackageManager, identifier: &ocx_oci::PackageRef) -> Option<String> {
    // Via `current`, not tag resolution, which needs `:latest` and breaks on registries with no cascade tags.
    let info = manager
        .find_symlink(identifier, &crate::composer::LinkSource::Current)
        .await
        .ok()?;

    let env = compose_env_for(manager, info).await.ok()?;
    let bin = match env.resolve_command("ocx") {
        Ok(bin) => bin,
        // `NotFound` only: a package may claim `ocx`, and folding its refusal into the literal re-arms the
        // self-reference hazard the refusal stops.
        Err(ocx_config::env::CommandResolutionError::NotFound { .. }) => std::path::PathBuf::from("ocx"),
        Err(error) => {
            log::debug!("Skipping the installed-version probe: {error}");
            return None;
        }
    };

    // Not `child_process::exec`: it diverges, and the output must be captured.
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

/// Composes the interface-surface environment of one installed package.
async fn compose_env_for(
    manager: &PackageManager,
    info: ocx_package::InstallInfo,
) -> crate::Result<ocx_config::env::Env> {
    let infos = vec![Arc::new(info)];
    let entries = manager
        .resolve_env(
            &infos,
            false,
            crate::EnvScope::package_tier(),
            &ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any),
        )
        .await?;

    let mut env = ocx_config::env::Env::new();
    use ocx_package::metadata::env::apply::EnvEntriesExt;

    env.apply_entries(&entries);
    Ok(env)
}

/// The argv the hand-off spawns: `tag@digest`, so the child refuses (exit 65) if the tag no longer names
/// the artifact this pull staged.
fn handoff_argv(tag: &str, digest: &ocx_oci::Digest) -> [String; 4] {
    [
        "self".to_owned(),
        "setup".to_owned(),
        format!("{tag}@{digest}"),
        // Heals profiles, introduces none, writes no config: an update must not adopt an opted-out surface.
        "--handoff".to_owned(),
    ]
}

/// Re-executes the freshly pulled binary as its own `ocx self setup`, returning `None` on a clean exit.
///
/// Never falls back to a bare `ocx` on `PATH`: that resolves to the binary being replaced and runs the
/// old setup contract.
async fn hand_off_setup(
    manager: &PackageManager,
    info: &ocx_package::InstallInfo,
    tag: &str,
) -> Option<HandoffFailure> {
    let env = match compose_env_for(manager, info.clone()).await {
        Ok(env) => env,
        Err(error) => return Some(HandoffFailure::SpawnFailed(error.to_string())),
    };
    let binary = match env.resolve_command("ocx") {
        Ok(binary) => binary,
        Err(error) => return Some(HandoffFailure::SpawnFailed(error.to_string())),
    };
    run_handoff(&binary, tag, &info.identifier().digest()).await
}

/// Spawns `binary` with the hand-off argv and classifies how it ended.
///
/// No deadline: a timeout could only kill a setup mid-write, leaving a half-migrated machine.
async fn run_handoff(binary: &std::path::Path, tag: &str, digest: &ocx_oci::Digest) -> Option<HandoffFailure> {
    let argv = handoff_argv(tag, digest);
    log::debug!("Handing setup to '{}' as {:?}.", binary.display(), argv);

    let mut command = tokio::process::Command::new(binary);
    command.args(argv);

    // Child stdout goes to our stderr, or `ocx --format json self update | jq` parses the child's table.
    match stderr_as_stdio() {
        Ok(stdio) => {
            command.stdout(stdio);
        }
        // Discarded, never inherited: an inherited stdout corrupts the parent's payload.
        Err(error) => {
            log::debug!("Cannot redirect the hand-off's stdout ({error}); discarding it.");
            command.stdout(std::process::Stdio::null());
        }
    }

    match command.status().await {
        Ok(status) => classify_handoff_status(status),
        Err(error) => Some(HandoffFailure::SpawnFailed(error.to_string())),
    }
}

/// A `Stdio` writing to this process's stderr, for a child whose human-readable
/// output must stay off the parent's data stream.
fn stderr_as_stdio() -> std::io::Result<std::process::Stdio> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        Ok(std::process::Stdio::from(
            std::io::stderr().as_fd().try_clone_to_owned()?,
        ))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        Ok(std::process::Stdio::from(
            std::io::stderr().as_handle().try_clone_to_owned()?,
        ))
    }
}

/// Maps a finished child's status onto `None` (clean) or a [`HandoffFailure`].
fn classify_handoff_status(status: std::process::ExitStatus) -> Option<HandoffFailure> {
    if status.success() {
        return None;
    }

    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal: Option<i32> = None;

    match (status.code(), signal) {
        (Some(code), _) => Some(HandoffFailure::Exited(code)),
        (None, Some(signal)) => Some(HandoffFailure::Signalled(signal)),
        (None, None) => Some(HandoffFailure::SpawnFailed(
            "the child ended with neither an exit code nor a signal".to_owned(),
        )),
    }
}

/// Whether `identifier`'s `current` symlink now names `root`, both sides canonicalized.
///
/// An unreadable path is `false`, which advises a setup re-run rather than claiming an update.
async fn current_names(
    file_structure: &ocx_store::file_structure::FileStructure,
    identifier: &ocx_oci::PackageRef,
    root: &std::path::Path,
) -> bool {
    let current = file_structure.symlinks.current(identifier);
    let root = root.to_path_buf();

    tokio::task::spawn_blocking(
        move || match (dunce::canonicalize(&current), dunce::canonicalize(&root)) {
            (Ok(current), Ok(root)) => current == root,
            _ => false,
        },
    )
    .await
    .unwrap_or_else(|error| {
        tracing::debug!(%error, "observing `current` failed; reporting the update as not activated");
        false
    })
}

/// Turns the observed state plus the hand-off outcome into the reported result.
///
/// Keyed on `current`, never the exit status: a child failing after its select (a dirty RC block exits 82)
/// already completed the update.
fn self_update_verdict(
    current_moved: bool,
    handoff: Option<HandoffFailure>,
    from: Option<String>,
    to: String,
) -> SelfUpdateResult {
    if current_moved {
        SelfUpdateResult::Installed { from, to, handoff }
    } else {
        SelfUpdateResult::Pulled { from, to, handoff }
    }
}

/// Returns the highest clean `major.minor.patch` release among `tags`, skipping rolling, build and pre-release tags.
fn find_latest_version(tags: &[String]) -> Option<ocx_package::version::Version> {
    tags.iter()
        .filter_map(|tag| ocx_package::version::Version::parse(tag))
        .filter(|version| version.has_patch() && !version.has_build() && !version.has_prerelease())
        .max()
}

#[cfg(test)]
mod tests {
    use std::{path::Path, time::Duration};

    use crate::PackageManager;
    use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
    use ocx_package::version::Version;
    use ocx_store::file_structure::FileStructure;

    use super::{find_latest_version, query_installed_version};

    /// Build a minimal offline [`PackageManager`] for unit tests.
    fn make_offline_manager(ocx_home: &Path) -> PackageManager {
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            index_store: IndexStore::new(ocx_home.join("index")),
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
        let identifier = ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY);

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
        let identifier = ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY);

        // Write a fresh state file — within any reasonable interval.
        let state_path = manager.file_structure().state.update_check_file(&identifier);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        std::fs::write(&state_path, b"").unwrap();
        let mtime_before = std::fs::metadata(&state_path).unwrap().modified().unwrap();

        let short_interval = Duration::from_secs(3600); // 1h — file is fresh
        let result = manager
            .check_update(&identifier, Some(short_interval), super::TagProbe::Remote)
            .await;

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
        let identifier = ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY);

        // Write a *very fresh* state file — would throttle under any positive interval.
        let state_path = manager.file_structure().state.update_check_file(&identifier);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        std::fs::write(&state_path, b"").unwrap();

        // Duration::ZERO = always bypass throttle; probe path reached.
        // Offline manager → fails to get client → returns Skipped (offline).
        let result = manager
            .check_update(&identifier, Some(Duration::ZERO), super::TagProbe::Remote)
            .await;
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
        let identifier = ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY);
        let state_path = manager.file_structure().state.update_check_file(&identifier);
        assert!(!state_path.exists(), "precondition: state file absent");

        let _ = manager
            .check_update(&identifier, Some(Duration::ZERO), super::TagProbe::Remote)
            .await;

        assert!(
            state_path.exists(),
            "state file must be touched after probe error (avoid hammering a broken registry)"
        );
    }

    /// The default throttle, derived from the shared refresh default, is 24 hours.
    #[test]
    fn default_throttle_value_is_86400_seconds() {
        assert_eq!(
            super::default_throttle(),
            Duration::from_secs(24 * 60 * 60),
            "default throttle must be exactly 24 hours"
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
        let result = manager.self_check_update(None, super::TagProbe::Remote).await;
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
        let identifier = ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY);

        // Write a fresh state file so the throttle fires.
        let state_path = manager.file_structure().state.update_check_file(&identifier);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        std::fs::write(&state_path, b"").unwrap();

        // 1-hour interval — the freshly-written file is within the window.
        let result = manager
            .self_check_update(Some(Duration::from_secs(3600)), super::TagProbe::Remote)
            .await;

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
        let result = manager
            .self_check_update(Some(Duration::ZERO), super::TagProbe::Remote)
            .await;
        assert!(
            result.is_ok(),
            "self_check_update must not return hard error; got: {result:?}"
        );
        assert!(
            matches!(result, Ok(super::UpdateCheckResult::Skipped(_))),
            "offline bypass must return Skipped; got: {result:?}"
        );
    }

    // ── TagProbe routing ─────────────────────────────────────────────────────

    /// `TagProbe` selects the tag source. On an offline manager (no client):
    /// `Remote` short-circuits to `Skipped(Offline)` (it needs a client),
    /// whereas `Index` resolves through the manager's index — an empty local
    /// index surfaces as `Skipped(NotFound)`, proving the local index (not the
    /// absent client) was consulted.
    ///
    /// Regression guard for the OCX_INDEX-ignored bug: explicit `ocx self
    /// update` must route version discovery through the configured index, not a
    /// throwaway remote-only probe.
    #[tokio::test(flavor = "multi_thread")]
    async fn tag_probe_index_consults_local_index_not_client() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = make_offline_manager(tmp.path());
        let identifier = ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY);

        // Remote probe: no client → Skipped(Offline) before touching the index.
        let remote = manager
            .check_update(&identifier, Some(Duration::ZERO), super::TagProbe::Remote)
            .await;
        assert!(
            matches!(
                remote,
                Ok(super::UpdateCheckResult::Skipped(super::SkippedReason::Offline))
            ),
            "Remote probe on an offline manager must short-circuit to Skipped(Offline); got: {remote:?}"
        );

        // Index probe: routes through the (empty) local index → NotFound, never
        // Offline — the client is never consulted.
        let index = manager
            .check_update(&identifier, Some(Duration::ZERO), super::TagProbe::Index)
            .await;
        assert!(
            matches!(
                index,
                Ok(super::UpdateCheckResult::Skipped(super::SkippedReason::NotFound))
            ),
            "Index probe must consult the empty local index (→ NotFound), not the absent client; got: {index:?}"
        );
    }

    /// Regression guard for the v0.5.0 self-update blind spot: the `Remote`
    /// probe must list tags through the **configured index chain**, never
    /// through a throwaway index over the bare client.
    ///
    /// `ocx.sh/ocx/cli` is a LOGICAL name; the published index routes it to a
    /// different physical repository. A raw registry tags-API probe answers from
    /// the stale pre-indirection repository and caps the visible releases, so
    /// every newer release is invisible and `ocx self update` reports
    /// up-to-date forever.
    ///
    /// The two stubs disagree on purpose: the chain source knows `9.9.9`, the
    /// manager's own client knows only `0.0.1`. The ambient ChainMode is
    /// `Default` — a probe that honoured it would read the (empty) local index
    /// instead — so the assertion also pins that the view forces a live source
    /// listing.
    #[tokio::test(flavor = "multi_thread")]
    async fn remote_probe_lists_through_the_configured_chain_not_the_bare_client() {
        use ocx_index::{OciIndex, OciIndexConfig};
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let tmp = tempfile::tempdir().unwrap();

        let source_data = StubTransportData::new();
        source_data.write().tags = vec![vec!["9.9.9".to_string()]];
        let source = Index::from_remote(OciIndex::new(OciIndexConfig {
            client: ocx_oci::Client::with_transport(Box::new(StubTransport::new(source_data))),
        }));

        let client_data = StubTransportData::new();
        client_data.write().tags = vec![vec!["0.0.1".to_string()]];
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(client_data)));

        let fs = FileStructure::with_root(tmp.path().to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            index_store: IndexStore::new(tmp.path().join("index")),
        });
        let index = Index::from_chained(local_index, vec![source], ChainMode::Default);
        let manager = PackageManager::new(fs, index, Some(client), "ocx.sh");

        let identifier = ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY);
        let result = manager
            .check_update(&identifier, Some(Duration::ZERO), super::TagProbe::Remote)
            .await;

        match result {
            Ok(super::UpdateCheckResult::UpdateAvailable(latest)) => assert_eq!(
                latest.tag(),
                Some("9.9.9"),
                "the Remote probe must list through the configured chain (9.9.9), not the bare client (0.0.1)"
            ),
            other => panic!("expected UpdateAvailable from the chain source; got: {other:?}"),
        }
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
        let identifier = ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY);

        let result = query_installed_version(&manager, &identifier).await;

        assert_eq!(
            result, None,
            "bootstrap mode (package absent from local store) must return None, never Some(\"\")"
        );
    }

    // ── The hand-off to the new binary's own setup ───────────────────────────
    //
    // `self_update` stops enumerating setup phases: it pulls WITHOUT selecting,
    // then re-executes the pulled binary as `ocx self setup <tag>@<digest>
    // --handoff`. Three properties carry that design, and each is tested where
    // it can actually be observed:
    //
    //   1. the argv reaches the child verbatim, carrying the resolved digest;
    //   2. the verdict comes from the `current` symlink, never the exit status;
    //   3. `current_names` answers about the filesystem, not about intent.

    /// A digest fixture with a distinctive hex so an assertion failure names it.
    fn fixture_digest() -> ocx_oci::Digest {
        ocx_oci::Digest::try_from(format!("sha256:{}", "ab".repeat(32)).as_str()).expect("a well-formed digest")
    }

    /// The hand-off spec is `<tag>@sha256:<hex>`, never a bare tag.
    ///
    /// A bare tag would let the child re-resolve to a different artifact than
    /// the one this pull just staged; the `tag@digest` form asserts
    /// immutability instead (the child exits 65 on a mismatch) and needs no
    /// second index round-trip. `--handoff` is last, and the whole vector is
    /// asserted rather than a `contains`, so an extra argument fails too.
    #[test]
    fn handoff_argv_carries_tag_at_digest() {
        let digest = fixture_digest();
        assert_eq!(
            super::handoff_argv("0.6.1", &digest),
            [
                "self".to_owned(),
                "setup".to_owned(),
                format!("0.6.1@sha256:{}", "ab".repeat(32)),
                "--handoff".to_owned(),
            ]
        );
    }

    /// …and that argv is what actually reaches the child.
    ///
    /// The sibling above pins the vector `handoff_argv` builds; on its own that
    /// proves nothing about the spawn, which could pass anything. This runs a
    /// recorder in the binary's place and reads back the arguments it was
    /// handed — and, from the same spawn, that a non-zero exit is classified by
    /// its code.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn handoff_spawns_the_argv_it_builds() {
        let tmp = tempfile::tempdir().unwrap();
        let recorded = tmp.path().join("argv");
        // 82 = DirtyRcBlock, the failure the design specifically must not read
        // as "the update failed".
        let recorder = write_argv_recorder(tmp.path(), &recorded, 82);

        let outcome = super::run_handoff(&recorder, "0.6.1", &fixture_digest()).await;

        let argv: Vec<String> = std::fs::read_to_string(&recorded)
            .expect("the recorder must have run")
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            argv,
            vec![
                "self".to_owned(),
                "setup".to_owned(),
                format!("0.6.1@sha256:{}", "ab".repeat(32)),
                "--handoff".to_owned(),
            ],
            "the child must receive exactly the hand-off argv"
        );
        assert!(
            matches!(outcome, Some(super::HandoffFailure::Exited(82))),
            "a non-zero child must be classified by its exit code; got: {outcome:?}"
        );
    }

    /// A child that exits 0 is no failure at all.
    ///
    /// The green half of the classification: without it, a `run_handoff` that
    /// reported a failure unconditionally would still satisfy the test above.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn handoff_reports_a_clean_child_as_no_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let recorded = tmp.path().join("argv");
        let recorder = write_argv_recorder(tmp.path(), &recorded, 0);

        let outcome = super::run_handoff(&recorder, "0.6.1", &fixture_digest()).await;

        assert!(outcome.is_none(), "a clean child carries no failure; got: {outcome:?}");
    }

    /// A binary that cannot be spawned is `SpawnFailed`, never a silent pass.
    #[tokio::test(flavor = "multi_thread")]
    async fn handoff_reports_a_missing_binary_as_spawn_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("no-such-binary");

        let outcome = super::run_handoff(&absent, "0.6.1", &fixture_digest()).await;

        assert!(
            matches!(outcome, Some(super::HandoffFailure::SpawnFailed(_))),
            "an unspawnable binary must surface as SpawnFailed; got: {outcome:?}"
        );
    }

    /// Writes a `sh` script that records its arguments one per line and exits
    /// with `exit_code`.
    #[cfg(unix)]
    fn write_argv_recorder(dir: &Path, recorded: &Path, exit_code: i32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let script = dir.join("fake-ocx");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nfor arg in \"$@\"; do printf '%s\\n' \"$arg\"; done > '{}'\nexit {exit_code}\n",
                recorded.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    // ── The verdict is the symlink, not the exit status ──────────────────────

    /// The select is the child's FIRST phase, so a child that failed in a later
    /// one — exit 82, a shell profile with local edits — has already repointed
    /// `current`. That update succeeded.
    ///
    /// Keying the verdict on the exit status would report a completed update as
    /// a failure, and send the user chasing an update they already have.
    #[test]
    fn installed_when_current_moved_even_though_child_failed() {
        let result = super::self_update_verdict(
            true,
            Some(super::HandoffFailure::Exited(82)),
            Some("0.6.0".to_owned()),
            "0.6.1".to_owned(),
        );

        match result {
            super::SelfUpdateResult::Installed { from, to, handoff } => {
                assert_eq!(from.as_deref(), Some("0.6.0"));
                assert_eq!(to, "0.6.1");
                assert!(
                    matches!(handoff, Some(super::HandoffFailure::Exited(82))),
                    "the failure must survive onto the result so the caller can advise"
                );
            }
            other => panic!("a moved `current` is an Installed update; got: {other:?}"),
        }
    }

    /// A clean child that moved `current` is a plain `Installed` with nothing
    /// to advise.
    #[test]
    fn installed_carries_no_advisory_when_the_child_was_clean() {
        let result = super::self_update_verdict(true, None, None, "0.6.1".to_owned());
        assert!(
            matches!(result, super::SelfUpdateResult::Installed { handoff: None, .. }),
            "got: {result:?}"
        );
    }

    /// `current` unmoved means nothing was activated: the machine is
    /// byte-identical to before the update, so the outcome is `Pulled` — the
    /// caller's exit 75 and "run `ocx self setup`" advice.
    #[test]
    fn pulled_when_current_did_not_move() {
        let result = super::self_update_verdict(
            false,
            Some(super::HandoffFailure::SpawnFailed("permission denied".to_owned())),
            Some("0.6.0".to_owned()),
            "0.6.1".to_owned(),
        );

        match result {
            super::SelfUpdateResult::Pulled { from, to, handoff } => {
                assert_eq!(from.as_deref(), Some("0.6.0"));
                assert_eq!(to, "0.6.1");
                assert!(matches!(handoff, Some(super::HandoffFailure::SpawnFailed(_))));
            }
            other => panic!("an unmoved `current` is a Pulled, not an Installed; got: {other:?}"),
        }
    }

    // ── current_names: the observation itself ────────────────────────────────

    /// `current_names` answers about the filesystem — true only when the
    /// symlink resolves to the very package root the pull produced.
    ///
    /// Both arms in one test on purpose: the false arm alone is satisfied by a
    /// function that always returns false, which would turn every successful
    /// update into a `Pulled`.
    #[tokio::test(flavor = "multi_thread")]
    async fn current_names_answers_from_the_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tmp.path().to_path_buf());
        let identifier = ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY);

        let new_root = tmp.path().join("packages/new");
        let old_root = tmp.path().join("packages/old");
        std::fs::create_dir_all(&new_root).unwrap();
        std::fs::create_dir_all(&old_root).unwrap();

        // Absent `current` — nothing was activated.
        assert!(
            !super::current_names(&fs, &identifier, &new_root).await,
            "an absent `current` cannot name the new root"
        );

        // Still pointing at the old install — the hand-off never selected.
        let current = fs.symlinks.current(&identifier);
        ocx_util::fs::symlink::create(&old_root, &current).unwrap();
        assert!(
            !super::current_names(&fs, &identifier, &new_root).await,
            "a `current` naming the old root is not a completed update"
        );

        // Repointed at the new install — the child's select landed.
        ocx_util::fs::symlink::update(&new_root, &current).unwrap();
        assert!(
            super::current_names(&fs, &identifier, &new_root).await,
            "a `current` naming the new root is a completed update"
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
