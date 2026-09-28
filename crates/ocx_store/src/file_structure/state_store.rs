// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use ocx_util::prelude::StringExt as _;

type Result<T> = std::result::Result<T, ocx_util::error::FileError>;

const CONSENT_STAMP_FILE: &str = "consent.json";

const RENDER_STAMP_FILE: &str = "render_stamp.json";

/// Any other `v` reads as absent, never an error, or a prompt fails over pure derived state.
const RENDER_STAMP_VERSION: u8 = 1;

// Project keys canonicalize the FILE, then take its parent, never the directory, or a symlinked `ocx.toml` is keyed at the link.
// Reuse `register_project_dir_best_effort` (`dunce`, not `tokio::fs::canonicalize`); a second derivation re-opens that hole.

/// One `bin/` entry's identity in a [`RenderStamp`].
///
/// Per entry, never one folded digest, or every prompt re-hashes every entry.
/// No mtime: it survives an in-place overwrite, so it would call exactly the tampered edit unchanged.
/// Residual: an in-place overwrite keeps `(size, file_id)`, so its `content_hash` is believed, not re-checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinEntryStamp {
    pub size: u64,

    /// Inode on Unix, file index on Windows; `None` falls through to the content hash, as a missing key does.
    pub file_id: Option<u64>,

    /// SHA-256 hex of the bytes: the authority the stat pair only indexes.
    pub content_hash: String,
}

impl BinEntryStamp {
    /// The one derivation of `file_id`: producer and gate both call it, or they disagree on Windows.
    ///
    /// `path` must be the path `metadata` came from, or the stamp describes two files.
    #[must_use]
    pub fn from_metadata(path: &Path, metadata: &std::fs::Metadata, content_hash: String) -> Self {
        Self {
            size: metadata.len(),
            file_id: file_id_of(path, metadata),
            content_hash,
        }
    }

    /// The one hashing derivation, called by render and gate alike; blocking.
    ///
    /// `None` on any read failure, including an entry grown past `metadata.len()` since the stat.
    #[must_use]
    pub fn of_file(path: &Path, metadata: &std::fs::Metadata) -> Option<Self> {
        let bytes = ocx_util::fs::read_bounded(path, metadata.len())
            .inspect_err(|e| log::debug!("Could not hash '{}': {e}", path.display()))
            .ok()?;
        let content_hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
        Some(Self::from_metadata(path, metadata, content_hash))
    }
}

#[cfg(unix)]
fn file_id_of(_path: &Path, metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    Some(metadata.ino())
}

/// Opens a handle because `MetadataExt::file_index()` is unstable (rust-lang/rust#63010).
///
/// Never stub this to `None`: every render would then republish every trampoline and `--dry-run` never clears.
#[cfg(windows)]
fn file_id_of(path: &Path, _metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_OPEN_REPARSE_POINT, GetFileInformationByHandle,
    };

    // Open the reparse point itself, or a swapped name hands back its target's identity.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .inspect_err(|e| log::debug!("Could not open '{}' for its file index: {e}", path.display()))
        .ok()?;
    // SAFETY: a plain-old-data struct, so all-zero is valid; it is read only after the call succeeds.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `file` keeps the handle live for the call, and `info` is writable storage of the written size.
    let queried = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
    if queried == 0 {
        log::debug!(
            "GetFileInformationByHandle failed for '{}': {}",
            path.display(),
            std::io::Error::last_os_error()
        );
        return None;
    }
    Some((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
}

#[cfg(not(any(unix, windows)))]
fn file_id_of(_path: &Path, _metadata: &std::fs::Metadata) -> Option<u64> {
    None
}

/// What one render left on disk; written last, so a crash mid-render leaves a detectably incomplete tree.
///
/// No field may be `Option`-shaped: serde reads a dropped `Option` key as `None`, which here would silently mean the global tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderStamp {
    // Private so [`Self::new`] is the only writer: a spellable literal would be a second source of the wire version.
    v: u8,

    pub home: PathBuf,

    /// Identity beside `home`: under `toolchain_dir`, two projects colliding in `name_for_path` share one home and stamp.
    pub scope: RenderStampScope,

    /// Always [`Self::bin_fingerprint`]'s key set, derived by [`Self::new`], or the gate and the heal diverge.
    pub names: BTreeSet<String>,

    pub bin_fingerprint: BTreeMap<String, BinEntryStamp>,

    /// `"<group>/<entry>"` → link target, default group only: the key is not the on-disk path, so layout moves keep the format.
    ///
    /// Never widen past the default group: the prompt heals no other group, so their entries would mismatch forever.
    pub link_fingerprint: BTreeMap<String, String>,
}

impl RenderStamp {
    /// A stamp at the version this binary writes, with `names` derived from `bin_fingerprint`.
    #[must_use]
    pub fn new(
        home: PathBuf,
        scope: RenderStampScope,
        bin_fingerprint: BTreeMap<String, BinEntryStamp>,
        link_fingerprint: BTreeMap<String, String>,
    ) -> Self {
        Self {
            v: RENDER_STAMP_VERSION,
            home,
            scope,
            names: bin_fingerprint.keys().cloned().collect(),
            bin_fingerprint,
            link_fingerprint,
        }
    }

    #[must_use]
    pub fn version(&self) -> u8 {
        self.v
    }
}

/// Which tier a persisted [`RenderStamp`] describes; tagged, never `Option<PathBuf>`, so a dropped key errors instead of meaning global.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderStampScope {
    /// The canonical project directory, as `ConsentStamp::project_dir` records it.
    Project(PathBuf),
    Global,
}

/// Which tier's render stamp a [`StateStore`] call addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStampTarget<'a> {
    /// Keyed like the consent stamp.
    Project(&'a str),
    Global,
}

/// Runtime state under `$OCX_HOME/state/`, one named accessor per subsystem so none keys into another's namespace.
///
/// ```text
/// {root}/
///   update-check/
///     {slug}          — zero-byte file; mtime = time of last registry probe
///   projects/
///     {key}/
///       consent.json  — per-project shell-activation consent stamp
/// ```
#[derive(Debug, Clone)]
pub struct StateStore {
    root: PathBuf,
}

impl StateStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn update_check_dir(&self) -> PathBuf {
        self.root.join("update-check")
    }

    /// Zero-byte throttle file whose mtime is the last registry probe; write it only through [`Self::touch`].
    pub fn update_check_file(&self, identifier: &ocx_oci::PackageRef) -> PathBuf {
        let slug = identifier.to_string().to_slug();
        self.update_check_dir().join(slug)
    }

    pub fn managed_config(&self) -> ocx_config::managed_config::ManagedConfigPaths {
        ocx_config::managed_config::ManagedConfigPaths::new(&self.root)
    }

    /// Deletable at any time, unlike the `$OCX_HOME/projects/` GC ledger that shares its key.
    pub fn project_state_dir(&self, key: &str) -> PathBuf {
        self.project_state_root().join(key)
    }

    /// Replaced, never edited in place, so any lock for it goes through `lock_scoped`, never a sidecar.
    pub fn consent_stamp_file(&self, key: &str) -> PathBuf {
        self.project_state_dir(key).join(CONSENT_STAMP_FILE)
    }

    /// The one part of `state/` that `ocx clean` sweeps.
    pub fn project_state_root(&self) -> PathBuf {
        self.root.join("projects")
    }

    /// Ungated path accessor; read and write through [`Self::render_stamp`] / [`Self::set_render_stamp`].
    pub fn render_stamp_file(&self, key: &str) -> PathBuf {
        self.project_state_dir(key).join(RENDER_STAMP_FILE)
    }

    /// Never under `projects/<key>/`: `ocx clean` removes the `$OCX_HOME` key's directory, so every clean would delete it.
    pub fn global_render_stamp_file(&self) -> PathBuf {
        self.root.join(RENDER_STAMP_FILE)
    }

    /// `target`'s stamp, or `None` on every failure, which callers answer by re-rendering; blocking.
    #[must_use]
    pub fn render_stamp(&self, target: RenderStampTarget<'_>) -> Option<RenderStamp> {
        let path = self.render_stamp_path(target);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                // Debug, not WARN: absence is the ordinary first-prompt state.
                log::debug!("No usable render stamp at '{}': {e}", path.display());
                return None;
            }
        };
        let stamp: RenderStamp = match serde_json::from_slice(&bytes) {
            Ok(stamp) => stamp,
            Err(e) => {
                log::debug!(
                    "Render stamp '{}' did not parse, treating as absent: {e}",
                    path.display()
                );
                return None;
            }
        };
        if stamp.v != RENDER_STAMP_VERSION {
            log::debug!(
                "Render stamp '{}' is version {}, which this ocx does not recognise; treating as absent",
                path.display(),
                stamp.v
            );
            return None;
        }
        Some(stamp)
    }

    /// Atomically replaces `target`'s stamp; blocking.
    ///
    /// # Errors
    ///
    /// Serialization or the write fails.
    pub fn set_render_stamp(&self, target: RenderStampTarget<'_>, stamp: &RenderStamp) -> Result<()> {
        let path = self.render_stamp_path(target);
        let bytes = serde_json::to_vec_pretty(stamp)
            .map_err(|e| ocx_util::error::FileError::new(&path, std::io::Error::other(e)))?;

        // `write_bytes_atomic` does not create the parent.
        let parent = path.parent().ok_or_else(|| {
            ocx_util::error::FileError::new(
                &path,
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "render stamp path has no parent"),
            )
        })?;
        std::fs::create_dir_all(parent).map_err(|e| ocx_util::error::FileError::new(parent, e))?;
        ocx_util::fs::write_bytes_atomic(&path, &bytes).map_err(|e| ocx_util::error::FileError::new(&path, e))
    }

    fn render_stamp_path(&self, target: RenderStampTarget<'_>) -> PathBuf {
        match target {
            RenderStampTarget::Project(key) => self.render_stamp_file(key),
            RenderStampTarget::Global => self.global_render_stamp_file(),
        }
    }

    pub fn host_capabilities_file(&self) -> PathBuf {
        self.root.join("host").join("capabilities.json")
    }

    /// Whether `path` was touched within `interval`; absent, unreadable or a zero `interval` mean not throttled. Blocking.
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
            return true;
        };
        elapsed < interval
    }

    /// Atomically refreshes `path`'s mtime; a failure is non-fatal, since the next invocation just re-probes.
    pub async fn touch(path: PathBuf) {
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(touch_err) = touch_atomic(&path) {
                log::debug!("state store: failed to touch state file: {touch_err}");
            }
        })
        .await;
    }
}

/// Publishes a zero-byte file at `path` by rename, so readers never see it absent.
fn touch_atomic(path: &Path) -> std::io::Result<()> {
    use std::fs;

    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "state path has no parent"))?;

    fs::create_dir_all(parent)?;

    let unique_suffix = {
        use std::sync::atomic::{AtomicU64, Ordering};
        // A counter, not wall-clock nanos: coarse macOS timers collided concurrent writers on one temp name.
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
        let identifier = ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY);
        let path = store.update_check_file(&identifier);
        assert_eq!(path, PathBuf::from("/ocx/state/update-check/ocx_sh_ocx_cli"));
    }

    /// The slug must contain no dots and no forward slashes.
    #[test]
    fn update_check_file_slug_is_dot_free() {
        let store = StateStore::new("/state");
        let identifier = ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY);
        let path = store.update_check_file(&identifier);
        let file_name = path.file_name().unwrap().to_str().unwrap();
        assert!(!file_name.contains('.'), "slug must not contain dots; got: {file_name}");
        assert!(
            !file_name.contains('/'),
            "slug must not contain slashes; got: {file_name}"
        );
    }

    // ── host_capabilities_file ───────────────────────────────────────────────

    /// The host-capability record lands at `host/capabilities.json`, keyed by
    /// nothing.
    ///
    /// Moved here from `ocx_oci::host_capabilities` with ADR 1.14: that module used
    /// to derive the path itself, so the claim was stated beside the derivation.
    /// The path is now injected by the caller, which leaves this accessor as the
    /// only place the layout is decided — and every `read_record` test in
    /// `host_capabilities` hand-builds `state/host/capabilities.json`, so
    /// without this assertion the accessor could be repointed and those tests
    /// would keep proving only that the reader reads whatever path it was
    /// handed.
    #[test]
    fn host_capabilities_file_produces_correct_path() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.host_capabilities_file(),
            PathBuf::from("/ocx/state/host/capabilities.json")
        );
    }

    // ── project-scoped state (C-022) ─────────────────────────────────────────

    /// C-022 — the sweep root `ocx clean` walks is `{root}/projects/`.
    #[test]
    fn project_state_root_is_projects_under_the_state_root() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(store.project_state_root(), PathBuf::from("/ocx/state/projects"));
    }

    /// C-022 — a project's state directory is its key nested under the sweep root.
    #[test]
    fn project_state_dir_nests_the_key_under_the_sweep_root() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.project_state_dir("0123456789abcdef"),
            PathBuf::from("/ocx/state/projects/0123456789abcdef")
        );
        assert_eq!(
            store.project_state_dir("0123456789abcdef").parent(),
            Some(store.project_state_root().as_path()),
            "every project state dir must be a direct child of the sweep root"
        );
    }

    /// C-022 — the consent stamp is `consent.json` inside the project's state dir.
    #[test]
    fn consent_stamp_file_is_consent_json_in_the_project_state_dir() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.consent_stamp_file("0123456789abcdef"),
            PathBuf::from("/ocx/state/projects/0123456789abcdef/consent.json")
        );
    }

    /// C-022 / A-30 — the key is derived by canonicalizing the resolved project
    /// **file**, taking its parent, then `dunce`. A symlinked `ocx.toml` must
    /// therefore land on the real directory's stamp, not on the symlink's own
    /// parent.
    ///
    /// The second assertion is the discriminator: it shows the parent-first
    /// order (`.parent()` of the *un*-canonicalized path, which is what
    /// `resolve_explicit_project_path` returns) produces a different key — so a
    /// green here cannot be mistaken for a test that cannot tell the two orders
    /// apart. That is the `OCX_PROJECT=/w/fake/ocx.toml → /attacker/ocx.toml`
    /// case: parent-first keys the stamp to the victim's granted directory.
    #[cfg(unix)]
    #[test]
    fn consent_stamp_file_follows_a_symlinked_project_file_to_the_real_directory() {
        fn key_file_first(project_file: &Path) -> String {
            let canonical_file = std::fs::canonicalize(project_file).unwrap();
            let dir = dunce::canonicalize(canonical_file.parent().unwrap()).unwrap();
            crate::reference_manager::ReferenceManager::name_for_path(&dir)
        }
        fn key_parent_first(project_file: &Path) -> String {
            crate::reference_manager::ReferenceManager::name_for_path(project_file.parent().unwrap())
        }

        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real");
        let fake_dir = tmp.path().join("fake");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::create_dir_all(&fake_dir).unwrap();
        let real_file = real_dir.join("ocx.toml");
        std::fs::write(&real_file, b"").unwrap();
        let fake_file = fake_dir.join("ocx.toml");
        std::os::unix::fs::symlink(&real_file, &fake_file).unwrap();

        let store = StateStore::new(tmp.path().join("state"));
        assert_eq!(
            store.consent_stamp_file(&key_file_first(&fake_file)),
            store.consent_stamp_file(&key_file_first(&real_file)),
            "a symlinked project file must key onto the real directory's stamp"
        );
        assert_ne!(
            key_parent_first(&fake_file),
            key_file_first(&real_file),
            "parent-first derivation must diverge, or this test cannot tell the two orders apart"
        );
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

    // ── C-003: the render stamp ──────────────────────────────────────────────

    use std::collections::BTreeSet;

    use super::{BinEntryStamp, RENDER_STAMP_VERSION, RenderStamp, RenderStampScope, RenderStampTarget};

    /// The 16-hex project key shape `name_for_path` produces.
    const SAMPLE_PROJECT_KEY: &str = "0123456789abcdef";

    fn sample_bin_entry(size: u64, content_hash: &str) -> BinEntryStamp {
        BinEntryStamp {
            size,
            file_id: Some(424_242),
            content_hash: content_hash.to_string(),
        }
    }

    fn sample_render_stamp() -> RenderStamp {
        RenderStamp {
            v: 1,
            home: PathBuf::from("/w/proj/.ocx/toolchain"),
            scope: RenderStampScope::Project(PathBuf::from("/w/proj")),
            names: ["cmake".to_string(), "ninja".to_string()].into_iter().collect(),
            bin_fingerprint: [
                ("cmake".to_string(), sample_bin_entry(120, "1".repeat(64).as_str())),
                ("ninja".to_string(), sample_bin_entry(131, "2".repeat(64).as_str())),
            ]
            .into_iter()
            .collect(),
            link_fingerprint: [(
                "default/cmake".to_string(),
                "/ocx/packages/sha256/aa/bbccdd".to_string(),
            )]
            .into_iter()
            .collect(),
        }
    }

    fn sample_global_render_stamp() -> RenderStamp {
        RenderStamp {
            v: 1,
            home: PathBuf::from("/ocx/toolchain"),
            scope: RenderStampScope::Global,
            names: ["cmake".to_string()].into_iter().collect(),
            bin_fingerprint: [("cmake".to_string(), sample_bin_entry(120, "3".repeat(64).as_str()))]
                .into_iter()
                .collect(),
            link_fingerprint: [(
                "default/cmake".to_string(),
                "/ocx/packages/sha256/cc/ddeeff".to_string(),
            )]
            .into_iter()
            .collect(),
        }
    }

    /// C-003 — a stamp survives a JSON round trip unchanged, per-entry values
    /// included. The per-entry `size` assertion is separate from the whole-value
    /// equality on purpose: it is the field C-061's cheap stat gate compares,
    /// and it must be the *written* one, not a zero.
    #[test]
    fn a_render_stamp_round_trips_through_json() {
        let stamp = sample_render_stamp();
        let json = serde_json::to_string(&stamp).expect("the stamp must serialize");
        let parsed: RenderStamp = serde_json::from_str(&json).expect("the stamp must deserialize");

        assert_eq!(parsed, stamp, "C-003 — a stamp must survive the round trip unchanged");
        assert_eq!(
            parsed.names,
            parsed.bin_fingerprint.keys().cloned().collect::<BTreeSet<String>>(),
            "C-003 — `names` is the on-disk entry set, so it must equal `bin_fingerprint`'s key set; a stamp \
             where they disagree lets wave 3's gate compare one set while the heal repairs the other"
        );
        assert_eq!(
            parsed.bin_fingerprint["cmake"].size, 120,
            "C-061 — the per-entry size the stat gate compares must survive the write"
        );
        assert_eq!(
            parsed.bin_fingerprint["cmake"].file_id,
            Some(424_242),
            "C-003 — the file identity the Windows hardlink check needs must survive too"
        );
    }

    /// C-003 — `deny_unknown_fields`: a stamp carrying an extra key is a
    /// deserialize error, at both levels of the tree.
    #[test]
    fn a_render_stamp_carrying_an_unknown_key_fails_to_deserialize() {
        let mut value = serde_json::to_value(sample_render_stamp()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_string(), serde_json::json!("x"));
        assert!(
            serde_json::from_value::<RenderStamp>(value).is_err(),
            "C-003 — an unknown key at the stamp's own level must be refused"
        );

        let mut entry = serde_json::to_value(sample_bin_entry(120, "aa")).unwrap();
        entry
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_string(), serde_json::json!("x"));
        assert!(
            serde_json::from_value::<BinEntryStamp>(entry).is_err(),
            "C-003 — an unknown key inside a per-entry stamp must be refused too"
        );
    }

    /// C-003, D-V13 — every field is required, so dropping any one of them is a
    /// deserialize *error* rather than a silent default.
    ///
    /// This is B-C's regression test. `scope` is the field that motivates it:
    /// as an `Option<PathBuf>` its dropped key would deserialize cleanly and
    /// present as the global tier, skipping the project-identity check D-V13
    /// added it for. `deny_unknown_fields` governs *extra* keys only and would
    /// not have caught that.
    #[test]
    fn dropping_any_render_stamp_field_fails_to_deserialize() {
        let value = serde_json::to_value(sample_render_stamp()).unwrap();
        let keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        assert!(
            keys.contains(&"scope".to_string()),
            "the loop must actually reach the tier tag, or this test proves nothing about D-V13"
        );

        for key in &keys {
            let mut reduced = value.clone();
            reduced.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<RenderStamp>(reduced).is_err(),
                "C-003 — dropping {key:?} must fail to deserialize, never resolve to a default"
            );
        }
    }

    /// C-003 — a per-entry stamp requires `size` and `content_hash`; `file_id`
    /// is the tree's one `Option` and its absence is the documented
    /// fall-through-to-the-content-hash case, not a defect.
    #[test]
    fn a_bin_entry_stamp_requires_every_field_except_the_file_id() {
        let value = serde_json::to_value(sample_bin_entry(120, "aa")).unwrap();

        for key in ["size", "content_hash"] {
            let mut reduced = value.clone();
            reduced.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<BinEntryStamp>(reduced).is_err(),
                "C-003 — dropping {key:?} must fail to deserialize"
            );
        }

        let mut without_file_id = value.clone();
        without_file_id.as_object_mut().unwrap().remove("file_id");
        let parsed: BinEntryStamp =
            serde_json::from_value(without_file_id).expect("an absent file_id is the documented fall-through");
        assert_eq!(
            parsed.file_id, None,
            "C-003 — an absent file id must read as `None`, which falls the gate through to the content hash"
        );
    }

    /// C-003 — **no field covers mtime**, at either level.
    ///
    /// Rust cannot assert a field's absence, so the invariant is asserted over
    /// the serialized key set instead: an exact set equality reds both when a
    /// documented key disappears and when an undocumented one — `mtime` being
    /// the one the contract names — appears. Without this form the "neither
    /// fingerprint covers mtime" contract has no reachable red at all.
    ///
    /// mtime is trivially forgeable and is preserved by an in-place overwrite,
    /// so it would report "unchanged" for exactly the edit the stamp exists to
    /// notice.
    #[test]
    fn the_render_stamp_json_carries_exactly_the_documented_keys_and_no_mtime() {
        let value = serde_json::to_value(sample_render_stamp()).unwrap();

        let top_level: BTreeSet<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        let expected_top: BTreeSet<&str> = ["v", "home", "scope", "names", "bin_fingerprint", "link_fingerprint"]
            .into_iter()
            .collect();
        assert_eq!(
            top_level, expected_top,
            "C-003 — the stamp's key set is exactly these six; an `mtime` key must never appear"
        );

        let entry = value
            .get("bin_fingerprint")
            .and_then(|map| map.get("cmake"))
            .expect("the sample must carry a per-entry stamp to inspect");
        let entry_level: BTreeSet<&str> = entry.as_object().unwrap().keys().map(String::as_str).collect();
        let expected_entry: BTreeSet<&str> = ["size", "file_id", "content_hash"].into_iter().collect();
        assert_eq!(
            entry_level, expected_entry,
            "C-003 — a per-entry stamp's key set is exactly these three; an `mtime` key must never appear"
        );
    }

    /// C-003, C-061 — per-entry hashes stay **individually** recoverable by
    /// name across the round trip. A single folded digest gives the prompt gate
    /// nothing to compare per entry and degenerates "hash only what differs"
    /// into the unconditional re-hash the gate exists to avoid.
    #[test]
    fn per_entry_hashes_stay_individually_recoverable_across_the_round_trip() {
        let stamp = sample_render_stamp();
        let json = serde_json::to_string(&stamp).expect("the stamp must serialize");
        let parsed: RenderStamp = serde_json::from_str(&json).expect("the stamp must deserialize");

        let cmake = parsed
            .bin_fingerprint
            .get("cmake")
            .expect("cmake's entry must be recoverable by name");
        let ninja = parsed
            .bin_fingerprint
            .get("ninja")
            .expect("ninja's entry must be recoverable by name");

        assert_ne!(
            cmake.content_hash, ninja.content_hash,
            "two distinct entries must not collapse into one folded digest"
        );
        assert_eq!(cmake.content_hash, stamp.bin_fingerprint["cmake"].content_hash);
        assert_eq!(ninja.content_hash, stamp.bin_fingerprint["ninja"].content_hash);
    }

    /// C-003 — a project's render stamp lives beside its consent stamp, under
    /// the same key, with the same "deletable at any time" lifetime.
    #[test]
    fn a_projects_render_stamp_sits_beside_its_consent_stamp() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.render_stamp_file(SAMPLE_PROJECT_KEY),
            PathBuf::from("/ocx/state/projects/0123456789abcdef/render_stamp.json")
        );
        assert_eq!(
            store.render_stamp_file(SAMPLE_PROJECT_KEY).parent(),
            store.consent_stamp_file(SAMPLE_PROJECT_KEY).parent(),
            "C-003 — the render stamp is a sibling of the consent stamp, no new store"
        );
    }

    /// C-003, D-V13 — the **global** stamp lives directly under `state/`, never
    /// under `projects/`.
    ///
    /// Two shipped mechanisms make the `projects/` path unusable for it, both
    /// silently: `consent::record_in` refuses to stamp `$OCX_HOME` at all, so
    /// that directory is never created; and `ocx clean` classifies a
    /// `state/projects/<key>/` recording `$OCX_HOME` as `SweepReason::OcxHome`
    /// and removes the whole directory, so the stamp would be deleted on every
    /// clean and the global gate would re-render forever.
    #[test]
    fn the_global_render_stamp_is_not_under_the_projects_sweep_root() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.global_render_stamp_file(),
            PathBuf::from("/ocx/state/render_stamp.json")
        );
        assert!(
            !store.global_render_stamp_file().starts_with(store.project_state_root()),
            "C-003 / D-V13 — the global stamp must not sit under the sweep root `ocx clean` walks"
        );
    }

    /// C-003 — a written stamp reads back for the tier it was written for, at
    /// the documented path.
    #[test]
    fn a_written_render_stamp_reads_back_for_the_tier_it_was_written_for() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StateStore::new(tmp.path().join("state"));
        let stamp = sample_render_stamp();

        store
            .set_render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY), &stamp)
            .expect("writing a project stamp must succeed");

        assert_eq!(
            store.render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY)),
            Some(stamp),
            "C-003 — the stamp must read back exactly as written"
        );
        assert!(
            store.render_stamp_file(SAMPLE_PROJECT_KEY).is_file(),
            "C-003 — the bytes must land at the documented project path"
        );
    }

    /// C-003, D-V13 — the two tiers do not read each other's stamps: a written
    /// global stamp is invisible to a project lookup and lands outside
    /// `projects/`.
    #[test]
    fn the_two_tiers_render_stamps_do_not_read_each_other() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StateStore::new(tmp.path().join("state"));

        store
            .set_render_stamp(RenderStampTarget::Global, &sample_global_render_stamp())
            .expect("writing the global stamp must succeed");

        assert_eq!(
            store.render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY)),
            None,
            "C-003 — a project lookup must not find the global stamp"
        );
        assert!(
            store.global_render_stamp_file().is_file(),
            "C-003 — the global stamp must land directly under `state/`"
        );
        assert!(
            !store.render_stamp_file(SAMPLE_PROJECT_KEY).exists(),
            "C-003 — writing the global stamp must not create a project stamp"
        );
    }

    /// C-003 — an unusable stamp is an absent stamp: a corrupt file reads as
    /// `None` so the caller's answer is "re-render", which is always safe.
    ///
    /// The write-valid-first half is the positive control, the pattern the
    /// version sibling below already uses: a bare `None` assertion is
    /// indistinguishable from a test that wrote to the wrong path, or from a
    /// reader that returns `None` unconditionally. Proving the same path reads
    /// `Some` first is what makes the `None` the corruption answering.
    #[test]
    fn an_unusable_render_stamp_reads_as_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StateStore::new(tmp.path().join("state"));
        let path = store.render_stamp_file(SAMPLE_PROJECT_KEY);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        std::fs::write(&path, serde_json::to_vec(&sample_render_stamp()).unwrap()).unwrap();
        assert!(
            store
                .render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY))
                .is_some(),
            "the valid stamp at this exact path must read back, or the `None` below proves nothing"
        );

        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(
            store.render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY)),
            None,
            "C-003 — a corrupt stamp must read as absent, never propagate an error"
        );
    }

    /// C-003 — [`RenderStamp::new`] stamps the version the reader accepts.
    ///
    /// The wire version has one source of truth and it is not a producer's
    /// literal: `RENDER_STAMP_VERSION` is module-private, so a producer in
    /// another module would otherwise hardcode `1`. The assertion is a
    /// *round trip through the reader* rather than `v == 1`, because that is
    /// the property that matters — a constructor stamping a version the reader
    /// rejects would make every render re-render forever, silently.
    #[test]
    fn a_constructed_render_stamp_carries_the_version_the_reader_accepts() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StateStore::new(tmp.path().join("state"));
        let sample = sample_render_stamp();

        let constructed = RenderStamp::new(
            sample.home.clone(),
            sample.scope.clone(),
            sample.bin_fingerprint.clone(),
            sample.link_fingerprint.clone(),
        );
        assert_eq!(
            constructed, sample,
            "C-003 — the constructor must produce exactly the stamp a producer would otherwise write by hand"
        );
        assert_eq!(
            constructed.names,
            constructed
                .bin_fingerprint
                .keys()
                .cloned()
                .collect::<BTreeSet<String>>(),
            "C-003 — `new` derives `names` from the map rather than accepting it, so the pair cannot \
             disagree at the producer; the round-trip test checks the same invariant on the wire"
        );

        store
            .set_render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY), &constructed)
            .expect("writing a constructed stamp must succeed");
        assert_eq!(
            store.render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY)),
            Some(constructed),
            "C-003 — a constructed stamp must read back, so its `v` is the one this binary recognises"
        );
    }

    /// C-003, C-061 — [`BinEntryStamp::from_metadata`] is the one derivation
    /// of the stat pair, so WP-7's producer and WP-9's comparator cannot
    /// disagree about what a "file id" is.
    ///
    /// The `size` assertion is against the bytes actually written, not a
    /// literal, and the Unix half pins `file_id` to the inode — the value the
    /// comparator will stat for itself. The `cfg`-free half below pins what
    /// "file identity" has to *mean* on every supported platform (present, one
    /// value per file, shared by a hardlink), which is the only assertion the
    /// Windows `GetFileInformationByHandle` arm has covering it.
    #[test]
    fn a_bin_entry_stamp_from_metadata_carries_the_platforms_own_file_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = tmp.path().join("cmake");
        let bytes = b"#!/bin/sh\nexec cmake \"$@\"\n";
        std::fs::write(&entry, bytes).unwrap();

        let metadata = std::fs::metadata(&entry).unwrap();
        let stamp = BinEntryStamp::from_metadata(&entry, &metadata, "1".repeat(64));

        assert_eq!(
            stamp.size,
            bytes.len() as u64,
            "C-061 — the stat gate's size must be the entry's real length"
        );
        assert_eq!(
            stamp.content_hash,
            "1".repeat(64),
            "the caller's hash is carried verbatim"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_eq!(
                stamp.file_id,
                Some(metadata.ino()),
                "C-061 — on Unix the file identity is the inode, which is what a comparator stats for itself"
            );
        }

        // The property both consumers actually rest on, asserted without a
        // `cfg`: `file_id` is *the same file*, not merely *some number*. The
        // Windows arm reads it through `GetFileInformationByHandle`, so this is
        // the one place a handle read that answered `None`, a constant, or the
        // wrong field would go red — on the only platform that compiles it.
        assert!(
            stamp.file_id.is_some(),
            "a supported platform must report a file identity, or the stat gate degenerates into an \
             unconditional re-hash"
        );

        let link = tmp.path().join("cmake-hardlink");
        crate::hardlink::create(&entry, &link).expect("a hardlink inside one tempdir is creatable");
        let linked = BinEntryStamp::from_metadata(&link, &std::fs::metadata(&link).unwrap(), "1".repeat(64));
        assert_eq!(
            linked.file_id, stamp.file_id,
            "C-003 — a hardlink IS the same file, and proving a trampoline `.exe` against the ShimBinStore \
             blob is exactly that comparison"
        );

        let other = tmp.path().join("ninja");
        std::fs::write(&other, bytes).unwrap();
        let other_stamp = BinEntryStamp::from_metadata(&other, &std::fs::metadata(&other).unwrap(), "1".repeat(64));
        assert_ne!(
            other_stamp.file_id, stamp.file_id,
            "a different file with identical bytes must not share the identity, or the gate would bless a \
             substituted entry"
        );
    }

    /// C-003 — a stamp at a `v` this binary does not recognise reads as
    /// **absent**, not as an error.
    ///
    /// The contract settled after the rest of this module was written, so the
    /// accepted set now exists and the property is falsifiable. The first half
    /// is the positive control: the very same bytes at the recognised version
    /// DO read back, so the `None` below is the version check answering rather
    /// than a fixture that never parsed. Without it, deleting the version check
    /// entirely would still leave this test green.
    #[test]
    fn a_render_stamp_at_an_unknown_version_reads_as_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StateStore::new(tmp.path().join("state"));
        let path = store.render_stamp_file(SAMPLE_PROJECT_KEY);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let mut value = serde_json::to_value(sample_render_stamp()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("v".to_string(), serde_json::json!(RENDER_STAMP_VERSION));
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            store
                .render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY))
                .is_some(),
            "the recognised version must read back, or the refusal below proves nothing"
        );

        value
            .as_object_mut()
            .unwrap()
            .insert("v".to_string(), serde_json::json!(RENDER_STAMP_VERSION + 1));
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            store.render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY)),
            None,
            "C-003 — an unrecognised `v` must read as absent, never as an error"
        );
    }
}
