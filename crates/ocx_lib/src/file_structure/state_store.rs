// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::log;
use crate::oci;
use crate::prelude::StringExt as _;

/// Filename of the managed-config snapshot metadata (`source`/`tag`/`digest`/
/// `fetched_at`) — a small JSON file. The payload it describes lives beside it
/// in [`MANAGED_CONFIG_PAYLOAD_FILE`].
const MANAGED_CONFIG_SNAPSHOT_FILE: &str = "snapshot.json";

/// Filename of the managed-config payload — the raw `config.toml` bytes the
/// metadata snapshot describes, kept as a human-readable sibling of
/// [`MANAGED_CONFIG_SNAPSHOT_FILE`] (readable, greppable, diffable) rather than
/// embedded as an escaped string inside the JSON.
const MANAGED_CONFIG_PAYLOAD_FILE: &str = "config.toml";

/// Manages persistent runtime state files under `$OCX_HOME/state/`.
///
/// `StateStore` is the typed home for state files whose existence or mtime IS
/// the data.  Unlike `cache/` (regenerable bulk), files here are persistent
/// across sessions.
///
/// Layout:
/// ```text
/// {root}/
///   update-check/
///     {slug}          — zero-byte file; mtime = time of last registry probe
/// ```
#[derive(Debug, Clone)]
pub struct StateStore {
    root: PathBuf,
}

impl StateStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the state store (e.g., `$OCX_HOME/state`).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the directory for update-check throttle state files.
    ///
    /// Path: `{root}/update-check/`
    ///
    /// Each file under this directory has the form `{slug}` where `slug` is
    /// the strict (no-dot) slug of the identifier. The mtime of the file is
    /// the only datum — content is always zero bytes.
    pub fn update_check_dir(&self) -> PathBuf {
        self.root.join("update-check")
    }

    /// Returns the throttle state file path for the given identifier.
    ///
    /// Path: `{root}/update-check/{slug}` where `{slug}` is the strict
    /// (no-dot) slug of `identifier.to_string()` — all non-alphanumeric
    /// characters replaced with `_`. For `ocx.sh/ocx/cli` this produces
    /// `ocx_sh_ocx_cli`.
    ///
    /// The file is zero-byte; its mtime is the only datum (time of last
    /// registry probe). The parent directory is created lazily on first touch.
    ///
    /// # Atomic-touch portability
    ///
    /// The throttle write path ([`StateStore::touch`]) relies on
    /// `std::fs::rename` to publish a new mtime atomically without races on
    /// the destination. Platform semantics:
    ///
    /// - **Linux**: `rename(2)` follows symlinks at source, replaces the
    ///   destination inode atomically without intermediate "absent" state.
    /// - **macOS**: BSD `rename(2)` provides the same atomic-replace
    ///   semantics as Linux.
    /// - **Windows**: `std::fs::rename` shims to `MoveFileExW` with
    ///   `MOVEFILE_REPLACE_EXISTING`, which gives equivalent atomic-replace
    ///   semantics (modulo open-handle constraints on the destination).
    ///
    /// In all three cases the atomic-touch contract holds. See
    /// <https://doc.rust-lang.org/std/fs/fn.rename.html> for the cross-platform
    /// shim's documented invariants.
    pub fn update_check_file(&self, identifier: &oci::Identifier) -> PathBuf {
        let slug = identifier.to_string().to_slug();
        self.update_check_dir().join(slug)
    }

    /// Returns the directory holding the managed-config tier's persistent
    /// state.
    ///
    /// Path: `{root}/managed-config/`
    pub fn managed_config_dir(&self) -> PathBuf {
        self.root.join("managed-config")
    }

    /// Returns the path of the managed-config snapshot metadata file
    /// (`ManagedConfigSnapshot`, written atomically by `persist_managed_config`).
    /// The payload it describes lives in the sibling
    /// [`Self::managed_config_toml_file`].
    ///
    /// Path: `{root}/managed-config/snapshot.json`
    pub fn managed_config_snapshot_file(&self) -> PathBuf {
        self.managed_config_dir().join(MANAGED_CONFIG_SNAPSHOT_FILE)
    }

    /// Returns the path of the managed-config payload file — the raw
    /// `config.toml` bytes the metadata snapshot describes, written as a
    /// readable sibling of `snapshot.json` by `persist_managed_config`.
    ///
    /// Path: `{root}/managed-config/config.toml`
    pub fn managed_config_toml_file(&self) -> PathBuf {
        self.managed_config_dir().join(MANAGED_CONFIG_PAYLOAD_FILE)
    }

    /// Returns the zero-byte freshness marker touched by the background
    /// refresh tick (separate from the snapshot file itself so a throttled
    /// probe never has to touch — and risk racing — the content file).
    ///
    /// Path: `{root}/managed-config/.last-refresh-check`
    pub fn managed_config_refresh_marker(&self) -> PathBuf {
        self.managed_config_dir().join(".last-refresh-check")
    }

    /// Content-bearing pause file for the managed-config background tick
    /// (`ocx config update --pause` — see `managed_config::pause`).
    pub fn managed_config_pause_file(&self) -> PathBuf {
        self.managed_config_dir().join("pause.json")
    }

    /// Returns the OCI Referrers-API capability-cache file for `registry`.
    ///
    /// Path: `{root}/referrers/{registry_slug}.json` where `{registry_slug}` is
    /// the relaxed slug of `registry` (dots preserved, `/`/`:` neutralised) so a
    /// hostile registry string cannot escape the store root. Owned by
    /// [`crate::oci::referrer::capability::ReferrersApiCapability`].
    pub fn referrers_capability_file(&self, registry: &str) -> PathBuf {
        self.root
            .join("referrers")
            .join(format!("{}.json", registry.to_relaxed_slug()))
    }

    /// Returns the offline-verify trust-root-cache file for `rekor_authority`.
    ///
    /// Path: `{root}/trust_root/{rekor_authority_slug}.json` where the slug is
    /// the relaxed slug of the Rekor URL authority (`host[:port]`), so public and
    /// private Sigstore instances never collide and a hostile authority string
    /// cannot escape the store root. Owned by
    /// [`crate::oci::verify::trust_cache::TrustRootCache`].
    pub fn trust_root_file(&self, rekor_authority: &str) -> PathBuf {
        self.root
            .join("trust_root")
            .join(format!("{}.json", rekor_authority.to_relaxed_slug()))
    }

    /// Pure associated fn deriving the managed-config snapshot path from
    /// `ocx_home` (the `$OCX_HOME` root, NOT this store's `state/` root)
    /// without constructing a [`StateStore`].
    ///
    /// Shared by [`crate::config::loader::ConfigLoader`] (which must not
    /// depend on [`crate::file_structure::FileStructure`] or reconstruct a
    /// store) and this store's own [`Self::managed_config_snapshot_file`]
    /// accessor — both must agree on one path so the loader's discovery
    /// candidate and the writer's persist target never drift apart.
    pub fn managed_config_snapshot_path(ocx_home: &Path) -> PathBuf {
        ocx_home
            .join("state")
            .join("managed-config")
            .join(MANAGED_CONFIG_SNAPSHOT_FILE)
    }

    /// Derives the managed-config payload path (`config.toml`) sitting beside the
    /// metadata snapshot at `snapshot_path`.
    ///
    /// Pure sibling derivation for
    /// [`read_managed_config_snapshot_at`](crate::managed_config::read_managed_config_snapshot_at),
    /// which holds only the snapshot path (the config loader never constructs a
    /// [`StateStore`]). Both this and the writer's
    /// [`Self::managed_config_toml_file`] resolve to one path, so reader and
    /// writer can never drift.
    pub fn managed_config_toml_path_for_snapshot(snapshot_path: &Path) -> PathBuf {
        snapshot_path.with_file_name(MANAGED_CONFIG_PAYLOAD_FILE)
    }

    /// Returns `true` when the state file at `path` was last touched within
    /// `interval`, meaning the next probe is not yet due.
    ///
    /// Returns `false` (probe is due) when the file is absent, unreadable, or
    /// older than `interval`. An `interval` of `Duration::ZERO` always returns
    /// `false` (bypass semantics).
    ///
    /// Synchronous (blocking) I/O — callers on an async runtime should wrap
    /// in `tokio::task::spawn_blocking`.
    pub fn is_throttled(path: &Path, interval: Duration) -> bool {
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

    /// Awaits [`touch_atomic`] on a blocking thread, logging at debug on
    /// failure.
    ///
    /// Drives the sync write off the async executor, swallows the result at
    /// debug, and awaits completion before returning so the throttle window
    /// resets before the next invocation lands.
    ///
    /// `JoinError` (task panic) is silently dropped at debug level: the
    /// throttle file failing to touch is non-fatal — the next invocation
    /// re-probes, which is the correct behaviour after a panic anyway.
    pub async fn touch(path: PathBuf) {
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(touch_err) = touch_atomic(&path) {
                log::debug!("state store: failed to touch state file: {touch_err}");
            }
        })
        .await;
    }
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
fn touch_atomic(path: &Path) -> std::io::Result<()> {
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use std::time::Duration;

    use super::{StateStore, touch_atomic};
    use crate::oci;

    #[test]
    fn root_returns_store_root() {
        let store = StateStore::new("/state");
        assert_eq!(store.root(), Path::new("/state"));
    }

    #[test]
    fn update_check_dir_is_rooted_under_state() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(store.update_check_dir(), PathBuf::from("/ocx/state/update-check"));
    }

    /// The state file for `ocx.sh/ocx/cli` must be rooted under `update-check/`
    /// with the slug `ocx_sh_ocx_cli` (strict no-dot encoding).
    #[test]
    fn update_check_file_produces_correct_path() {
        let store = StateStore::new("/ocx/state");
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);
        let path = store.update_check_file(&identifier);
        assert_eq!(path, PathBuf::from("/ocx/state/update-check/ocx_sh_ocx_cli"));
    }

    /// The slug must contain no dots and no forward slashes.
    #[test]
    fn update_check_file_slug_is_dot_free() {
        let store = StateStore::new("/state");
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);
        let path = store.update_check_file(&identifier);
        let file_name = path.file_name().unwrap().to_str().unwrap();
        assert!(!file_name.contains('.'), "slug must not contain dots; got: {file_name}");
        assert!(
            !file_name.contains('/'),
            "slug must not contain slashes; got: {file_name}"
        );
    }

    // ── referrers_capability_file / trust_root_file ──────────────────────────

    /// The referrers capability cache for `ghcr.io` lands under `referrers/`
    /// with the relaxed slug (dots preserved) plus a `.json` suffix.
    #[test]
    fn referrers_capability_file_produces_correct_path() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.referrers_capability_file("ghcr.io"),
            PathBuf::from("/ocx/state/referrers/ghcr.io.json")
        );
    }

    /// The trust-root cache is keyed by Rekor authority under `trust_root/`;
    /// the `:` in `host:port` is neutralised to `_` by the relaxed slug.
    #[test]
    fn trust_root_file_produces_correct_path() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.trust_root_file("rekor.example:443"),
            PathBuf::from("/ocx/state/trust_root/rekor.example_443.json")
        );
    }

    /// A hostile registry / authority string must never escape the store root:
    /// the relaxed slug replaces `/` and `..`-forming dots so every produced
    /// path stays a direct child of the subsystem directory. (Moved here from
    /// `capability.rs` when the layout moved onto these accessors.)
    #[test]
    fn cache_files_reject_hostile_slug() {
        let store = StateStore::new("/ocx/state");
        for hostile in ["../evil", "/etc/passwd", "..", "../../etc/shadow", "foo/../bar"] {
            for file in [store.referrers_capability_file(hostile), store.trust_root_file(hostile)] {
                let name = file.file_name().unwrap().to_str().unwrap();
                assert!(!name.contains('/'), "slug must not contain slashes; got: {name}");
                // The only path components below the store root are the subsystem
                // dir and the single slugged filename — no `..` traversal.
                let tail: Vec<_> = file.strip_prefix("/ocx/state").unwrap().components().collect();
                assert_eq!(tail.len(), 2, "expected {{subsystem}}/{{slug}}.json, got {file:?}");
            }
        }
    }

    // ── is_throttled decision table ──────────────────────────────────────────

    /// A path that does not exist is never throttled (no previous probe record).
    #[test]
    fn is_throttled_absent_path_returns_false() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("does_not_exist");
        assert!(!StateStore::is_throttled(&absent, Duration::from_secs(86_400)));
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
            StateStore::is_throttled(&path, Duration::from_secs(600)),
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
            !StateStore::is_throttled(&path, Duration::ZERO),
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
            !StateStore::is_throttled(&path, Duration::from_nanos(1)),
            "file older than 1ns interval must not be throttled"
        );
    }

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
            StateStore::is_throttled(&path, Duration::from_secs(86_400)),
            "future mtime must be treated as throttled (clock-skew safety)"
        );
    }

    // ── touch_atomic ─────────────────────────────────────────────────────────

    /// `touch_atomic` must create parent directories lazily if absent,
    /// write the file, and succeed on repeated calls.
    #[test]
    fn touch_atomic_creates_parent_and_writes() {
        let tmp = tempfile::tempdir().unwrap();
        // Parent dir does NOT exist yet.
        let path = tmp.path().join("state").join("update-check").join("ocx_sh_ocx_cli");

        // First call: parent created, file created.
        touch_atomic(&path).expect("first touch must succeed");
        assert!(path.exists(), "state file must exist after first touch");

        // Second call: must succeed without error (idempotent).
        touch_atomic(&path).expect("second touch must succeed (idempotent)");
        assert!(path.exists(), "state file must still exist after second touch");
    }

    /// Concurrent `touch_atomic` calls for the same path must all succeed
    /// (no panics, no I/O errors) and the file must exist at the end.
    #[tokio::test(flavor = "multi_thread")]
    async fn touch_atomic_concurrent_safety() {
        let tmp = tempfile::tempdir().unwrap();
        let path = std::sync::Arc::new(tmp.path().join("state").join("update-check").join("concurrent_slug"));

        // Pre-create parent so the race is on the file, not the directory.
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let tasks: Vec<_> = (0..10)
            .map(|_| {
                let p = std::sync::Arc::clone(&path);
                tokio::spawn(async move {
                    // touch_atomic is sync; run in spawn_blocking to avoid blocking the
                    // async executor.
                    let p2 = p.as_ref().clone();
                    tokio::task::spawn_blocking(move || touch_atomic(&p2))
                        .await
                        .expect("spawn_blocking must not panic")
                })
            })
            .collect();

        for task in tasks {
            task.await
                .expect("task must not panic")
                .expect("touch_atomic must not return an error");
        }

        assert!(path.exists(), "state file must exist after concurrent writes");
    }
}
