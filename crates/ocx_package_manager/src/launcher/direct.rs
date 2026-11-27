// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Direct launchers: the vendored directory's entry points, a copy of the launcher image beside a `.launch` sidecar.

use std::path::Path;

use ocx_env_apply::launch::{DirectRecord, LaunchRecord, RECORD_EXTENSION, RecordError};
use ocx_store::file_structure::normalise_written_file;

use crate::exec_env::unforwarded_keys;
use crate::tasks::vendor::VendorError;

/// Writes the launcher `<name>` (`<name>.exe` for a Windows target) as a byte copy of `blob`, then `<name>.launch`.
///
/// `windows` is the target's, not the host's: a Linux host vendoring for Windows writes `<name>.exe` and applies
/// the Windows path rules, as the manifest records.
///
/// The copy is never a hardlink: a vendored directory must survive its source being deleted or moved.
///
/// # Errors
///
/// [`VendorError::PathUnrepresentable`] when `sidecar` names a path outside the vendor directory, and
/// [`crate::Error::LauncherRecordRefused`] (as [`VendorError::Package`]) for any other rule of
/// [`DirectRecord::validate`], before anything is written; [`VendorError::Package`] for a failed write.
pub async fn write_direct_launcher_for(
    dir: &Path,
    name: &str,
    blob: &[u8],
    sidecar: &DirectRecord,
    windows: bool,
) -> Result<(), VendorError> {
    let mut sidecar = sidecar.clone();
    // The launcher's environment is never the caller's: every credential and every plumbing key is removed.
    sidecar.unset = unset_keys();
    sidecar.validate(windows).map_err(|error| match error {
        RecordError::PathNotInRoot(path) => VendorError::PathUnrepresentable { path },
        source => VendorError::Package(crate::Error::LauncherRecordRefused {
            path: dir.join(format!("{name}.{RECORD_EXTENSION}")),
            source,
        }),
    })?;
    let body = LaunchRecord::Direct(sidecar).encode();

    let launcher = dir.join(if windows {
        format!("{name}.exe")
    } else {
        name.to_owned()
    });
    // Launcher first, sidecar second: a fault must not leave a sidecar without its launcher.
    tokio::fs::write(&launcher, blob)
        .await
        .map_err(|e| crate::error::file_error(&launcher, e))?;
    let sidecar_path = dir.join(format!("{name}.{RECORD_EXTENSION}"));
    tokio::fs::write(&sidecar_path, body)
        .await
        .map_err(|e| crate::error::file_error(&sidecar_path, e))?;
    // The host umask and clock must not reach the vendored bytes.
    let normalised = launcher.clone();
    tokio::task::spawn_blocking(move || {
        normalise_written_file(&launcher, 0o755)?;
        normalise_written_file(&sidecar_path, 0o644)
    })
    .await
    .map_err(|e| join_failed(&normalised, e))?
    .map_err(|e| crate::error::file_error(&e.path, e.cause))?;
    Ok(())
}

fn join_failed(path: &Path, error: tokio::task::JoinError) -> VendorError {
    VendorError::Package(crate::Error::InternalFile(
        path.to_path_buf(),
        std::io::Error::other(error),
    ))
}

/// Keys removed from the child's environment, sorted for a reproducible sidecar.
fn unset_keys() -> Vec<String> {
    let mut keys: Vec<String> = unforwarded_keys()
        .chain(["OCX_BINARY_PIN"])
        .map(str::to_owned)
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use ocx_env_apply::launch::{DirectRecord, EnvRecord};

    use ocx_exit::{ClassifyExitCode, ExitCode};

    use super::{join_failed, write_direct_launcher_for};
    use crate::tasks::vendor::VendorError;

    fn sidecar(name: &str) -> DirectRecord {
        serde_json::from_value(serde_json::json!({
            "kind": "direct",
            "schema_version": 1,
            "vendor_id": "sha256:0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a",
            "self": name,
            "program": "@VENDOR_DIR@/packages/p/content/bin/tool",
            "args": [],
            "unset": [],
            "env": [],
        }))
        .expect("fixture decodes")
    }

    /// The launcher is the blob's bytes in a file of its own, not a link to the blob's source.
    #[tokio::test]
    async fn launcher_is_a_byte_copy_not_a_hardlink() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("blob");
        let bytes = b"launcher image bytes".to_vec();
        std::fs::write(&source, &bytes).expect("write blob");
        let dir = tmp.path().join("bin");
        std::fs::create_dir_all(&dir).expect("create dir");

        write_direct_launcher_for(&dir, "tool", &bytes, &sidecar("bin/tool"), cfg!(windows))
            .await
            .expect("writer succeeds");

        let launcher = dir.join(if cfg!(windows) { "tool.exe" } else { "tool" });
        assert_eq!(std::fs::read(&launcher).expect("launcher"), bytes);
        assert!(dir.join("tool.launch").is_file(), "sidecar sits beside the launcher");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let source_ino = std::fs::metadata(&source).expect("source").ino();
            let launcher_ino = std::fs::metadata(&launcher).expect("launcher").ino();
            assert_ne!(source_ino, launcher_ino, "a copy has its own inode");
        }
    }

    async fn write(sidecar: &DirectRecord) -> (tempfile::TempDir, Result<(), VendorError>) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = write_direct_launcher_for(tmp.path(), "tool", b"blob", sidecar, cfg!(windows)).await;
        (tmp, result)
    }

    /// A `program` outside the root is refused with nothing written; a well-formed one is written.
    #[tokio::test]
    async fn program_outside_the_root_is_refused() {
        let mut bad = sidecar("bin/tool");
        bad.program = "@VENDOR_DIR@/../escape".into();
        let (tmp, result) = write(&bad).await;
        assert!(matches!(result, Err(VendorError::PathUnrepresentable { .. })));
        assert_eq!(std::fs::read_dir(tmp.path()).expect("dir").count(), 0);

        let (_tmp, result) = write(&sidecar("bin/tool")).await;
        assert!(result.is_ok());
    }

    /// A reserved `env` key is refused; an ordinary key is written, with `unset` filled in.
    #[tokio::test]
    async fn reserved_env_key_is_refused() {
        for key in ["OCX_BINARY_PIN", "__OCX_ENV_STATE", "OCX_IDENTITY_TOKEN"] {
            let mut bad = sidecar("bin/tool");
            bad.env = vec![EnvRecord {
                key: key.into(),
                value: "v".into(),
                kind: ocx_env_apply::ApplyKind::Constant,
                separator: None,
            }];
            let (_tmp, result) = write(&bad).await;
            assert!(
                matches!(
                    result,
                    Err(VendorError::Package(crate::Error::LauncherRecordRefused { .. }))
                ),
                "{key}"
            );
        }

        let mut ok = sidecar("bin/tool");
        ok.env = vec![EnvRecord {
            key: "JAVA_HOME".into(),
            value: "v".into(),
            kind: ocx_env_apply::ApplyKind::Constant,
            separator: None,
        }];
        let (tmp, result) = write(&ok).await;
        assert!(result.is_ok());
        let written: DirectRecord =
            serde_json::from_slice(&std::fs::read(tmp.path().join("tool.launch")).expect("sidecar")).expect("json");
        assert!(written.unset.contains(&"OCX_BINARY_PIN".to_owned()));
        assert!(written.unset.contains(&"OCX_IDENTITY_TOKEN".to_owned()));
    }

    /// The launcher's name follows the target: `.exe` for Windows on any host, none for another target.
    #[tokio::test]
    async fn the_launcher_name_follows_the_target_not_the_host() {
        for (windows, launcher) in [(true, "tool.exe"), (false, "tool")] {
            let tmp = tempfile::tempdir().expect("tempdir");
            let mut target = sidecar("bin/tool");
            target.program = "@VENDOR_DIR@/packages/p/content/bin/tool".into();

            write_direct_launcher_for(tmp.path(), "tool", b"blob", &target, windows)
                .await
                .expect("writer succeeds");

            assert!(tmp.path().join(launcher).is_file(), "windows={windows}: {launcher}");
            let other = if windows { "tool" } else { "tool.exe" };
            assert!(!tmp.path().join(other).exists(), "windows={windows}: no {other}");
            assert!(tmp.path().join("tool.launch").is_file());
        }
    }

    /// The target's rules apply, not the host's: a Windows device name is refused for a Windows target only.
    #[tokio::test]
    async fn the_windows_path_rules_follow_the_target() {
        let mut device = sidecar("bin/tool");
        device.program = "@VENDOR_DIR@/packages/p/content/bin/nul".into();

        let tmp = tempfile::tempdir().expect("tempdir");
        let refused = write_direct_launcher_for(tmp.path(), "tool", b"blob", &device, true).await;
        assert!(
            matches!(refused, Err(VendorError::PathUnrepresentable { .. })),
            "{refused:?}"
        );

        let tmp = tempfile::tempdir().expect("tempdir");
        write_direct_launcher_for(tmp.path(), "tool", b"blob", &device, false)
            .await
            .expect("control: another target allows the name");
    }

    /// Overwriting keeps a file's old mode, so pre-created restrictive files stand in for a host umask.
    #[cfg(unix)]
    #[tokio::test]
    async fn written_files_carry_fixed_modes_and_the_normalised_mtime() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().expect("tempdir");
        for file in ["tool", "tool.launch"] {
            let path = tmp.path().join(file);
            std::fs::write(&path, "old").expect("pre-create");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        }
        write_direct_launcher_for(tmp.path(), "tool", b"blob", &sidecar("bin/tool"), false)
            .await
            .expect("writer succeeds");

        let epoch = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(315_532_800);
        for (file, mode) in [("tool", 0o755), ("tool.launch", 0o644)] {
            let metadata = std::fs::metadata(tmp.path().join(file)).expect("metadata");
            assert_eq!(metadata.permissions().mode() & 0o777, mode, "{file} mode");
            assert_eq!(metadata.modified().expect("mtime"), epoch, "{file} mtime");
        }
    }

    /// A panicked blocking task is an internal I/O failure naming the launcher, not a refused value.
    #[tokio::test]
    async fn join_failure_is_an_internal_file_error() {
        let error = tokio::task::spawn_blocking(|| panic!("boom"))
            .await
            .expect_err("task panicked");
        let failure = join_failed(std::path::Path::new("bin/tool"), error);
        assert_eq!(failure.classify(), Some(ExitCode::IoError));
        let text = failure.to_string();
        assert!(text.contains("bin/tool") && text.contains("panic"), "{text}");
    }
}
