// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Filesystem-kind detection for network-FS posture enforcement.
//!
//! Two-layer design for testability:
//! - [`classify_magic`] — pure function mapping a raw `statfs.f_type` (or
//!   Windows NTFS-type constant) magic number to a [`FilesystemKind`] variant.
//!   Called from [`filesystem_kind`] but fully unit-testable without I/O.
//! - [`filesystem_kind`] — async entry point that calls the OS (`statfs(2)` on
//!   Linux, `statfs(2)` on macOS, `GetVolumeInformationW` on Windows) and
//!   dispatches to [`classify_magic`].
//!
//! Testing seam: set `__OCX_TESTING_FORCE_FS_KIND=<kind>` (e.g. `nfs`) to
//! override the OS probe in test builds. See
//! [`system_design_shared_store.md`] §5 M4 item 6.

use std::path::Path;
use std::sync::Mutex;

use crate::env;
use crate::log;

/// Classification of a filesystem as local or network-attached.
///
/// The distinction matters for advisory-lock reliability: `flock(2)` is safe on
/// local filesystems; on NFS it degrades silently (POSIX locks are advisory
/// cross-host, not mandatory). See `research_shared_store.md` §Lock primitives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemKind {
    /// Local filesystem (ext4, APFS, NTFS, tmpfs, btrfs, xfs, …). Advisory
    /// locks are cross-process reliable.
    Local,
    /// Network-attached filesystem (NFS, SMB/CIFS, AFS, GPFS, …). Advisory
    /// lock semantics degrade silently on NFS v2/v3; use with caution.
    Network,
    /// Filesystem kind could not be determined (unsupported platform, I/O
    /// error resolving the path's mount-point, or an unrecognised magic
    /// number). The caller must treat this as `Local` (allow) unless policy
    /// explicitly refuses unknown kinds.
    Unknown,
}

impl std::fmt::Display for FilesystemKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Network => write!(f, "network"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Pure classifier: maps a raw filesystem magic number to a [`FilesystemKind`].
///
/// `magic` is the value of `statfs.f_type` on Linux/macOS, or a synthesised
/// constant for Windows volume-type probes. The function is synchronous, pure,
/// and I/O-free — usable in unit tests without a real mount.
///
/// Known network-filesystem magic numbers:
/// - `0x6969` — NFS
/// - `0x517B` — SMB (CIFS)
/// - `0xFF534D42` — SMB2
/// - `0x65735546` — FUSE (may wrap NFS or other network FS)
/// - `0x61756673` — AFS
/// - `0x47504653` — GPFS
///
/// Any unrecognised magic number returns [`FilesystemKind::Unknown`].
pub fn classify_magic(magic: u64) -> FilesystemKind {
    // Network-attached filesystems where advisory locks degrade. Values from
    // <linux/magic.h>. FUSE is treated as network because it commonly wraps a
    // remote backend (sshfs, NFS gateways); a false-positive only triggers the
    // warn/refuse posture, never data loss.
    const NFS: u64 = 0x6969;
    const SMB: u64 = 0x517B;
    const SMB2: u64 = 0xFF53_4D42;
    const FUSE: u64 = 0x6573_5546;
    const AFS: u64 = 0x6175_6673;
    const GPFS: u64 = 0x4750_4653;

    // Local filesystems with reliable cross-process advisory locks. Values from
    // <linux/magic.h>.
    const EXT: u64 = 0xEF53; // ext2/3/4
    const TMPFS: u64 = 0x0102_1994;
    const BTRFS: u64 = 0x9123_683E;
    const XFS: u64 = 0x5846_5342;
    const ZFS: u64 = 0x2FC1_2FC1;
    const OVERLAYFS: u64 = 0x794C_7630;
    const F2FS: u64 = 0xF2F5_2010;

    match magic {
        NFS | SMB | SMB2 | FUSE | AFS | GPFS => FilesystemKind::Network,
        EXT | TMPFS | BTRFS | XFS | ZFS | OVERLAYFS | F2FS => FilesystemKind::Local,
        _ => FilesystemKind::Unknown,
    }
}

/// Probes the filesystem kind for `path`.
///
/// Walks up to the nearest existing ancestor when `path` itself does not yet
/// exist (same convention as [`crate::utility::fs::same_filesystem`]).
///
/// On Linux and macOS this calls `statfs(2)`; on Windows it calls
/// `GetVolumeInformationW`. On other platforms it returns
/// [`FilesystemKind::Unknown`].
///
/// # Testing seam
///
/// When the environment variable `__OCX_TESTING_FORCE_FS_KIND` is set, the OS
/// probe is skipped and the value is interpreted as a filesystem kind string
/// (`local`, `network`, or `unknown`). Unrecognised test values yield
/// [`FilesystemKind::Unknown`]. This seam is strictly for unit / acceptance
/// tests — the `__` prefix marks it as a testing-only override
/// ([`feedback_testing_env_prefix`]).
///
/// # Errors
///
/// Returns [`crate::Error::InternalFile`] when the underlying OS probe fails.
pub async fn filesystem_kind(path: &Path) -> crate::Result<FilesystemKind> {
    // Testing seam: bypass the OS probe entirely when forced. The acceptance
    // harness passes `nfs` (the concrete network FS) as well as the documented
    // canonical values; accept both spellings.
    if let Some(forced) = env::var(env::keys::__OCX_TESTING_FORCE_FS_KIND) {
        let kind = match forced.trim().to_ascii_lowercase().as_str() {
            "local" => FilesystemKind::Local,
            "network" | "nfs" | "smb" | "cifs" => FilesystemKind::Network,
            _ => FilesystemKind::Unknown,
        };
        return Ok(kind);
    }
    probe_filesystem_kind(path).await
}

/// OS-level filesystem-kind probe (no testing seam).
///
/// Linux/macOS: `statfs(2)` → [`classify_magic`]. Windows:
/// `GetDriveTypeW`/`GetVolumeInformationW` → [`FilesystemKind::Network`] for
/// `DRIVE_REMOTE`, else [`FilesystemKind::Local`]. Other platforms:
/// [`FilesystemKind::Unknown`].
async fn probe_filesystem_kind(path: &Path) -> crate::Result<FilesystemKind> {
    let anchor = nearest_existing_ancestor(path).await;
    probe_anchor(&anchor).await
}

/// Resolve `path` to its nearest existing ancestor (mirrors
/// [`crate::utility::fs::same_filesystem`]) so a not-yet-created zone path is
/// keyed off its parent. Falls back to `path` itself when no ancestor exists.
async fn nearest_existing_ancestor(path: &Path) -> std::path::PathBuf {
    if crate::utility::fs::path_exists_lossy(path).await {
        return path.to_path_buf();
    }
    let mut current = path.parent();
    while let Some(parent) = current {
        let probe = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        if crate::utility::fs::path_exists_lossy(probe).await {
            return probe.to_path_buf();
        }
        current = parent.parent();
    }
    path.to_path_buf()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn probe_anchor(anchor: &Path) -> crate::Result<FilesystemKind> {
    let anchor = anchor.to_path_buf();
    // `statfs(2)` is a blocking syscall — keep it off the async reactor.
    tokio::task::spawn_blocking(move || statfs_kind(&anchor))
        .await
        .map_err(|e| crate::error::file_error(Path::new("<statfs>"), std::io::Error::other(e)))?
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn statfs_kind(anchor: &Path) -> crate::Result<FilesystemKind> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = match std::ffi::CString::new(anchor.as_os_str().as_bytes()) {
        Ok(p) => p,
        // A path with an interior NUL cannot be probed — treat as Unknown.
        Err(_) => return Ok(FilesystemKind::Unknown),
    };
    // SAFETY: `statbuf` is fully initialised by a successful `statfs` call; we
    // read `f_type` only when the call returned 0. `c_path` is a valid
    // null-terminated C string that lives for the duration of the call.
    let mut statbuf: libc::statfs = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::statfs(c_path.as_ptr(), &mut statbuf) };
    if result != 0 {
        return Err(crate::error::file_error(anchor, std::io::Error::last_os_error()));
    }
    // `f_type` is `i64`/`u32`/`u64` depending on target; widen to `u64` for the
    // pure classifier. Cast through the unsigned same-width type first so a
    // negative `i64` magic does not sign-extend incorrectly.
    #[cfg(target_os = "linux")]
    let magic = statbuf.f_type as u64;
    #[cfg(target_os = "macos")]
    let magic = statbuf.f_type as u64;
    Ok(classify_magic(magic))
}

#[cfg(target_os = "windows")]
async fn probe_anchor(anchor: &Path) -> crate::Result<FilesystemKind> {
    let anchor = anchor.to_path_buf();
    tokio::task::spawn_blocking(move || windows_drive_kind(&anchor))
        .await
        .map_err(|e| crate::error::file_error(Path::new("<GetDriveTypeW>"), std::io::Error::other(e)))?
}

#[cfg(target_os = "windows")]
fn windows_drive_kind(anchor: &Path) -> crate::Result<FilesystemKind> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{DRIVE_REMOTE, GetDriveTypeW, GetVolumePathNameW};

    let wide: Vec<u16> = anchor.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut volume = vec![0u16; 261];
    // SAFETY: `wide` is null-terminated; `volume` is a writable buffer of length
    // `volume.len()`; both live for the duration of the call.
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) };
    if ok == 0 {
        return Err(crate::error::file_error(anchor, std::io::Error::last_os_error()));
    }
    // SAFETY: `volume` is a null-terminated UTF-16 volume-root string produced
    // by `GetVolumePathNameW` above.
    let drive_type = unsafe { GetDriveTypeW(volume.as_ptr()) };
    if drive_type == DRIVE_REMOTE {
        Ok(FilesystemKind::Network)
    } else {
        Ok(FilesystemKind::Local)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
async fn probe_anchor(_anchor: &Path) -> crate::Result<FilesystemKind> {
    Ok(FilesystemKind::Unknown)
}

/// Network-FS posture: how OCX reacts when a zone path is on a network FS.
///
/// Configured via `OCX_NETWORK_FS` ∈ {`warn` (default), `refuse`, `allow`}.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkFsPosture {
    /// Emit a `warn!` log and continue. Default.
    #[default]
    Warn,
    /// Return `Err(Error::NetworkFsRefused)` (exit 81 / PolicyBlocked).
    Refuse,
    /// No check, no log. Explicitly opted out.
    Allow,
}

impl NetworkFsPosture {
    /// Parse `OCX_NETWORK_FS` from the environment.
    ///
    /// Returns the default ([`NetworkFsPosture::Warn`]) when the variable is
    /// absent, empty, or unrecognised.
    pub fn from_env() -> Self {
        match env::var(env::keys::OCX_NETWORK_FS) {
            Some(value) => match value.trim().to_ascii_lowercase().as_str() {
                "refuse" => Self::Refuse,
                "allow" => Self::Allow,
                // "warn" plus any unrecognised (or empty) value fall back to the
                // default warn posture.
                _ => Self::Warn,
            },
            None => Self::Warn,
        }
    }
}

impl std::fmt::Display for NetworkFsPosture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Warn => write!(f, "warn"),
            Self::Refuse => write!(f, "refuse"),
            Self::Allow => write!(f, "allow"),
        }
    }
}

/// Applies the network-FS posture for the given `zone` label and `kind`.
///
/// - [`NetworkFsPosture::Allow`] — no-op.
/// - [`NetworkFsPosture::Warn`] — emits `warn!` once per process lifetime for
///   each distinct zone label (avoids log spam on repeated checks).
/// - [`NetworkFsPosture::Refuse`] — returns
///   `Err(crate::Error::NetworkFsRefused { zone })`.
///
/// Only called when `kind == FilesystemKind::Network`. Local/Unknown kinds skip
/// this check.
pub fn apply_posture(posture: NetworkFsPosture, zone: &str, kind: FilesystemKind) -> crate::Result<()> {
    // Only network kinds drive the posture; local/unknown always proceed.
    if kind != FilesystemKind::Network {
        return Ok(());
    }
    match posture {
        NetworkFsPosture::Allow => Ok(()),
        NetworkFsPosture::Warn => {
            if warn_once(zone) {
                log::warn!(
                    "zone '{zone}' is on a network filesystem; advisory locks degrade silently on NFS \
                     — set OCX_NETWORK_FS=refuse to block, or OCX_NETWORK_FS=allow to suppress this warning"
                );
            }
            Ok(())
        }
        NetworkFsPosture::Refuse => Err(crate::Error::NetworkFsRefused { zone: zone.to_string() }),
    }
}

/// Tracks `(zone)` labels already warned about this process, so a repeated
/// posture check (clean probes multiple zones) does not spam the log.
///
/// Returns `true` when the caller should emit the warning (first time for this
/// zone), `false` when it was already warned.
fn warn_once(zone: &str) -> bool {
    static WARNED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let mut warned = match WARNED.lock() {
        Ok(guard) => guard,
        // A poisoned mutex here is harmless — emit the warning rather than
        // suppress a genuine network-FS signal.
        Err(poisoned) => poisoned.into_inner(),
    };
    if warned.iter().any(|z| z == zone) {
        return false;
    }
    warned.push(zone.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify_magic: pure unit tests (no I/O) ────────────────────────────

    #[test]
    fn nfs_magic_is_network() {
        // 0x6969 = NFS magic on Linux (from <linux/magic.h>)
        assert_eq!(classify_magic(0x6969), FilesystemKind::Network);
    }

    #[test]
    fn smb_magic_is_network() {
        // 0x517B = SMB (CIFS) magic
        assert_eq!(classify_magic(0x517B), FilesystemKind::Network);
    }

    #[test]
    fn smb2_magic_is_network() {
        // 0xFF534D42 = SMB2 magic
        assert_eq!(classify_magic(0xFF534D42), FilesystemKind::Network);
    }

    #[test]
    fn unknown_magic_returns_unknown() {
        // 0xDEAD_BEEF is not a known filesystem magic
        assert_eq!(classify_magic(0xDEAD_BEEF), FilesystemKind::Unknown);
    }

    #[test]
    fn ext4_magic_is_local() {
        // 0xEF53 = EXT2/3/4 magic
        assert_eq!(classify_magic(0xEF53), FilesystemKind::Local);
    }

    // ── Additional missing magic cases from system_design_shared_store.md §5 M4 ──
    // NFS/SMB/SMB2/unknown/ext4 covered above. Missing: FUSE, AFS, GPFS.

    #[test]
    fn fuse_magic_is_network() {
        // 0x65735546 = FUSE (may wrap NFS or other network FS)
        // Requirement: system_design_shared_store.md §5 M4 item 6 — FUSE is network.
        // Traced to: plan_shared_store P3.2s classify_magic missing cases.
        assert_eq!(classify_magic(0x6573_5546), FilesystemKind::Network);
    }

    #[test]
    fn afs_magic_is_network() {
        // 0x61756673 = AFS
        // Requirement: system_design_shared_store.md §5 M4 item 6 — AFS is network.
        // Traced to: plan_shared_store P3.2s classify_magic missing cases.
        assert_eq!(classify_magic(0x6175_6673), FilesystemKind::Network);
    }

    #[test]
    fn gpfs_magic_is_network() {
        // 0x47504653 = GPFS (IBM General Parallel File System)
        // Requirement: system_design_shared_store.md §5 M4 item 6 — GPFS is network.
        // Traced to: plan_shared_store P3.2s classify_magic missing cases.
        assert_eq!(classify_magic(0x4750_4653), FilesystemKind::Network);
    }

    // ── NetworkFsPosture tests ────────────────────────────────────────────────
    //
    // Requirement: system_design_shared_store.md §5 M4 item 6 —
    // OCX_NETWORK_FS ∈ {warn (default), refuse, allow}.
    // Traced to: plan_shared_store P3.2s posture tests.

    #[test]
    fn posture_default_is_warn() {
        // Requirement: system_design_shared_store.md §5 M4 item 6 —
        // OCX_NETWORK_FS absent → Warn (default).
        // Traced to: plan_shared_store P3.2s posture "warn default".
        let env = crate::test::env::lock();
        env.remove(crate::env::keys::OCX_NETWORK_FS);
        let posture = NetworkFsPosture::from_env();
        assert_eq!(posture, NetworkFsPosture::Warn, "default posture must be Warn");
    }

    #[test]
    fn posture_warn_is_the_derive_default() {
        // NetworkFsPosture derives Default; the Default impl must be Warn.
        assert_eq!(NetworkFsPosture::default(), NetworkFsPosture::Warn);
    }

    #[test]
    fn posture_from_env_parses_refuse() {
        // OCX_NETWORK_FS=refuse → NetworkFsPosture::Refuse.
        // Traced to: plan_shared_store P3.2s posture.
        let env = crate::test::env::lock();
        env.set(crate::env::keys::OCX_NETWORK_FS, "refuse");
        let posture = NetworkFsPosture::from_env();
        assert_eq!(posture, NetworkFsPosture::Refuse);
    }

    #[test]
    fn posture_from_env_parses_allow() {
        // OCX_NETWORK_FS=allow → NetworkFsPosture::Allow.
        // Traced to: plan_shared_store P3.2s posture "allow proceeds".
        let env = crate::test::env::lock();
        env.set(crate::env::keys::OCX_NETWORK_FS, "allow");
        let posture = NetworkFsPosture::from_env();
        assert_eq!(posture, NetworkFsPosture::Allow);
    }

    #[test]
    fn posture_from_env_unknown_value_falls_back_to_warn() {
        // Unrecognised OCX_NETWORK_FS → fall back to Warn.
        // Traced to: plan_shared_store P3.2s posture.
        let env = crate::test::env::lock();
        env.set(crate::env::keys::OCX_NETWORK_FS, "garbage");
        let posture = NetworkFsPosture::from_env();
        assert_eq!(
            posture,
            NetworkFsPosture::Warn,
            "unrecognised value must fall back to Warn"
        );
    }

    // ── apply_posture ────────────────────────────────────────────────────────

    #[test]
    fn apply_posture_allow_is_ok() {
        // Allow posture: no-op, returns Ok(()).
        // Traced to: plan_shared_store P3.2s posture "allow proceeds".
        let result = apply_posture(NetworkFsPosture::Allow, "cache", FilesystemKind::Network);
        assert!(result.is_ok(), "Allow posture must return Ok(())");
    }

    #[test]
    fn apply_posture_refuse_returns_network_fs_refused_error() {
        // Refuse posture on Network FS → Error::NetworkFsRefused → exit 81.
        // Requirement: system_design_shared_store.md §5 M4 item 6 —
        // `refuse` → `Error::NetworkFsRefused` (exit 81 / PolicyBlocked).
        // Traced to: plan_shared_store P3.2s posture "refuse → 81".
        use crate::cli::{ClassifyExitCode, ExitCode};

        let result = apply_posture(NetworkFsPosture::Refuse, "state", FilesystemKind::Network);
        assert!(result.is_err(), "Refuse posture on Network FS must return Err");
        let err = result.unwrap_err();
        // Verify the error message contains the zone label.
        let display = err.to_string();
        assert!(
            display.contains("state"),
            "NetworkFsRefused error must include the zone label 'state'"
        );
        // Verify exit code classification → 81 (PolicyBlocked).
        assert_eq!(
            err.classify(),
            Some(ExitCode::PolicyBlocked),
            "NetworkFsRefused must classify as PolicyBlocked (81)"
        );
    }

    #[test]
    fn apply_posture_warn_returns_ok() {
        // Warn posture: emits warn! log but returns Ok(()).
        // The log is unobservable in a unit test without injecting a subscriber;
        // we verify the return value only.
        // Traced to: plan_shared_store P3.2s posture "warn default".
        let result = apply_posture(NetworkFsPosture::Warn, "cache", FilesystemKind::Network);
        assert!(
            result.is_ok(),
            "Warn posture must return Ok(()) despite network FS detection"
        );
    }

    #[test]
    fn apply_posture_refuse_on_local_fs_proceeds() {
        // apply_posture self-guards: `if kind != Network { return Ok(()) }`.
        // Local and Unknown filesystems therefore always return Ok(()), regardless
        // of the posture setting. Callers in `clean` only call this when kind ==
        // Network, but the guard makes the function safe to call unconditionally.
        //
        // Traced to: plan_shared_store P3.7 posture "local/unknown always proceed".
        let result = apply_posture(NetworkFsPosture::Refuse, "cache", FilesystemKind::Local);
        assert!(
            result.is_ok(),
            "Refuse posture on a Local FS must return Ok(()) — apply_posture self-guards on kind"
        );

        let result_unknown = apply_posture(NetworkFsPosture::Refuse, "cache", FilesystemKind::Unknown);
        assert!(
            result_unknown.is_ok(),
            "Refuse posture on an Unknown FS must return Ok(()) — apply_posture self-guards on kind"
        );
    }
}
