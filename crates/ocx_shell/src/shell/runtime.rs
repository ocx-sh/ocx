// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Which POSIX runtime a native Windows `ocx.exe` talks to: MSYS2 (Git Bash, MSYS2) or Cygwin.
//!
//! The two spell a drive differently, and the answer is read from `PATH`: the first directory that
//! holds a runtime DLL decides. No environment variable or flag takes part.
//!
//! Limits: `PATH` is read once per process, so an entry exported under one runtime is not removed by a
//! process that detects the other. The first runtime directory on `PATH` is taken as the calling
//! shell's own runtime, which a `PATH` that lists both can get wrong.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::Shell;
use super::msys::{CYGWIN_DRIVE_PREFIX, MSYS_DRIVE_PREFIX};

const CYGWIN_DLL: &str = "cygwin1.dll";
const MSYS_DLL: &str = "msys-2.0.dll";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PosixRuntime {
    Msys,
    Cygwin,
}

impl PosixRuntime {
    fn drive_prefix(self) -> &'static str {
        match self {
            Self::Msys => MSYS_DRIVE_PREFIX,
            Self::Cygwin => CYGWIN_DRIVE_PREFIX,
        }
    }
}

/// The runtime named by the first directory in `dirs` that holds one of the two DLLs; MSYS2 when none does.
pub(super) fn detect_runtime(dirs: &[PathBuf], exists: impl Fn(&Path) -> bool) -> PosixRuntime {
    for dir in dirs {
        if exists(&dir.join(CYGWIN_DLL)) {
            return PosixRuntime::Cygwin;
        }
        if exists(&dir.join(MSYS_DLL)) {
            return PosixRuntime::Msys;
        }
    }
    PosixRuntime::Msys
}

static HOST_RUNTIME: OnceLock<PosixRuntime> = OnceLock::new();

/// The drive prefix of the resolved host runtime; MSYS2 until [`resolve_host_dialect`] has run.
pub(super) fn host_drive_prefix() -> &'static str {
    HOST_RUNTIME.get().copied().unwrap_or(PosixRuntime::Msys).drive_prefix()
}

/// Find the POSIX runtime on `PATH` once per process, before any shell code is emitted for `shell`.
///
/// Does nothing off Windows, where the host dialect is native, nor for a shell
/// that keeps native paths: a PowerShell prompt must not pay the probe.
pub async fn resolve_host_dialect(shell: Shell) {
    if !cfg!(windows) {
        return;
    }
    let dirs: Vec<PathBuf> = ocx_env::PATH
        .get_raw()
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    resolve_into(&HOST_RUNTIME, shell, &dirs).await;
}

/// [`resolve_host_dialect`] over an explicit cell and `PATH`.
async fn resolve_into(cell: &OnceLock<PosixRuntime>, shell: Shell, dirs: &[PathBuf]) {
    if !shell.converts_paths() || cell.get().is_some() {
        return;
    }
    let found = probe_runtime_dlls(dirs).await;
    let _ = cell.set(detect_runtime(dirs, |path| found.contains(path)));
}

/// The runtime DLLs in the first absolute directory of `dirs` that holds one; later directories are never touched.
///
/// A relative or empty entry means the current directory, which a repository could fill with a DLL.
async fn probe_runtime_dlls(dirs: &[PathBuf]) -> HashSet<PathBuf> {
    let mut found = HashSet::new();
    for dir in dirs.iter().filter(|dir| dir.is_absolute()) {
        for dll in [CYGWIN_DLL, MSYS_DLL] {
            let candidate = dir.join(dll);
            if ocx_util::fs::path_exists_lossy(&candidate).await {
                found.insert(candidate);
            }
        }
        if !found.is_empty() {
            break;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        for file in files {
            std::fs::write(dir.path().join(file), b"").expect("write dll stand-in");
        }
        dir
    }

    fn detect(dirs: &[&tempfile::TempDir]) -> PosixRuntime {
        let paths: Vec<PathBuf> = dirs.iter().map(|dir| dir.path().to_path_buf()).collect();
        detect_runtime(&paths, Path::is_file)
    }

    #[test]
    fn cygwin_dll_alone_means_cygwin() {
        assert_eq!(detect(&[&dir_with(&[CYGWIN_DLL])]), PosixRuntime::Cygwin);
    }

    #[test]
    fn msys_dll_alone_means_msys() {
        assert_eq!(detect(&[&dir_with(&[MSYS_DLL])]), PosixRuntime::Msys);
    }

    #[test]
    fn no_dll_means_msys() {
        assert_eq!(detect(&[&dir_with(&["other.dll"]), &dir_with(&[])]), PosixRuntime::Msys);
        assert_eq!(detect(&[]), PosixRuntime::Msys);
    }

    #[test]
    fn the_earlier_directory_wins() {
        let cygwin = dir_with(&[CYGWIN_DLL]);
        let msys = dir_with(&[MSYS_DLL]);
        assert_eq!(detect(&[&cygwin, &msys]), PosixRuntime::Cygwin);
        assert_eq!(detect(&[&msys, &cygwin]), PosixRuntime::Msys);
    }

    #[test]
    fn a_directory_without_a_dll_is_skipped() {
        assert_eq!(
            detect(&[&dir_with(&[]), &dir_with(&[CYGWIN_DLL])]),
            PosixRuntime::Cygwin
        );
    }

    #[tokio::test]
    async fn a_native_arm_never_probes_and_a_posix_arm_does() {
        let cygwin = dir_with(&[CYGWIN_DLL]);
        let dirs = vec![cygwin.path().to_path_buf()];
        for shell in [Shell::PowerShell, Shell::Nushell, Shell::Elvish, Shell::Batch] {
            let cell = OnceLock::new();
            resolve_into(&cell, shell, &dirs).await;
            assert!(cell.get().is_none(), "{shell:?} must not probe");
        }
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::Dash] {
            let cell = OnceLock::new();
            resolve_into(&cell, shell, &dirs).await;
            assert_eq!(cell.get(), Some(&PosixRuntime::Cygwin), "{shell:?} must probe");
            assert_eq!(PosixRuntime::Cygwin.drive_prefix(), CYGWIN_DRIVE_PREFIX);
        }
    }

    /// The DLL really sits where a relative entry resolves (the current directory), so only the
    /// `is_absolute` filter keeps it from being found. An empty entry resolves to the bare current
    /// directory, which would need a DLL written into the crate directory itself.
    #[tokio::test]
    async fn a_relative_entry_is_not_probed() {
        let inside_cwd = tempfile::tempdir_in(".").expect("temp dir in the current directory");
        std::fs::write(inside_cwd.path().join(CYGWIN_DLL), b"").expect("write dll stand-in");
        let name = inside_cwd.path().file_name().expect("directory name");
        let relative = PathBuf::from(name);
        assert!(!relative.is_absolute());
        assert!(
            Path::new(&relative).join(CYGWIN_DLL).is_file(),
            "the relative entry must reach the DLL"
        );

        assert!(probe_runtime_dlls(&[relative]).await.is_empty());
        let absolute = vec![std::path::absolute(inside_cwd.path()).expect("absolute path")];
        assert_eq!(
            probe_runtime_dlls(&absolute).await.len(),
            1,
            "the same directory, spelled absolute, is probed"
        );
    }

    #[tokio::test]
    async fn probing_stops_at_the_first_directory_holding_a_dll() {
        let first = dir_with(&["other.dll"]);
        let second = dir_with(&[MSYS_DLL]);
        let third = dir_with(&[CYGWIN_DLL]);
        let dirs: Vec<PathBuf> = [&first, &second, &third]
            .iter()
            .map(|dir| dir.path().to_path_buf())
            .collect();

        let found = probe_runtime_dlls(&dirs).await;
        assert_eq!(found, HashSet::from([second.path().join(MSYS_DLL)]));
        let runtime = detect_runtime(&dirs, |path| found.contains(path));
        assert_eq!(runtime, PosixRuntime::Msys);
        assert_eq!(runtime.drive_prefix(), MSYS_DRIVE_PREFIX);
        assert!(probe_runtime_dlls(&[]).await.is_empty());
    }
}
