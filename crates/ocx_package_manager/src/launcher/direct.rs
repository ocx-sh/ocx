// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Direct launchers: the vendored directory's entry points, a copy of the launcher image beside a `.direct` sidecar.

use std::path::Path;

use ocx_env::Propagation;
use ocx_env_apply::direct::{DirectSidecar, SidecarError};

use super::generate::write_launcher_file;
use crate::tasks::vendor::VendorError;

/// [`write_direct_launcher_for`] for the platform this process runs on.
pub async fn write_direct_launcher(
    dir: &Path,
    name: &str,
    blob: &[u8],
    sidecar: &DirectSidecar,
) -> Result<(), VendorError> {
    write_direct_launcher_for(dir, name, blob, sidecar, cfg!(windows)).await
}

/// Writes the launcher `<name>` (`<name>.exe` for a Windows target) as a byte copy of `blob`, then `<name>.direct`.
///
/// `windows` is the target's, not the host's: a Linux host vendoring for Windows writes `<name>.exe` and applies
/// the Windows path rules, as the manifest records.
///
/// The copy is never a hardlink: a vendored directory must survive its source being deleted or moved.
///
/// # Errors
///
/// [`VendorError::ValueUnrepresentable`] or [`VendorError::PathUnrepresentable`] when `sidecar` breaks a rule of
/// [`DirectSidecar::validate`], before anything is written; [`VendorError::Package`] for a failed write.
pub async fn write_direct_launcher_for(
    dir: &Path,
    name: &str,
    blob: &[u8],
    sidecar: &DirectSidecar,
    windows: bool,
) -> Result<(), VendorError> {
    let mut sidecar = sidecar.clone();
    // The launcher's environment is never the caller's: every credential and every plumbing key is removed.
    sidecar.unset = unset_keys();
    sidecar.validate(windows).map_err(|e| match e {
        SidecarError::PathNotInRoot(path) => VendorError::PathUnrepresentable { path },
        other => VendorError::ValueUnrepresentable {
            reason: other.to_string(),
        },
    })?;
    let body = serde_json::to_string_pretty(&sidecar)
        .map_err(|e| VendorError::ValueUnrepresentable { reason: e.to_string() })?;

    let launcher = dir.join(if windows {
        format!("{name}.exe")
    } else {
        name.to_owned()
    });
    // Launcher first, sidecar second: a fault must not leave a sidecar without its launcher.
    tokio::fs::write(&launcher, blob)
        .await
        .map_err(|e| crate::error::file_error(&launcher, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(|e| crate::error::file_error(&launcher, e))?;
    }
    write_launcher_file(dir.join(format!("{name}.direct")), body).await?;
    Ok(())
}

/// Keys removed from the child's environment, sorted for a reproducible sidecar.
fn unset_keys() -> Vec<String> {
    let mut keys: Vec<String> = [Propagation::Credential, Propagation::HookLocal, Propagation::State]
        .into_iter()
        .flat_map(ocx_env::keys_with)
        .chain(["OCX_BINARY_PIN"])
        .map(str::to_owned)
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use ocx_env_apply::direct::{DirectSidecar, EnvRecord};

    use super::{write_direct_launcher, write_direct_launcher_for};
    use crate::tasks::vendor::VendorError;

    fn sidecar(name: &str) -> DirectSidecar {
        serde_json::from_value(serde_json::json!({
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

        write_direct_launcher(&dir, "tool", &bytes, &sidecar("bin/tool"))
            .await
            .expect("writer succeeds");

        let launcher = dir.join(if cfg!(windows) { "tool.exe" } else { "tool" });
        assert_eq!(std::fs::read(&launcher).expect("launcher"), bytes);
        assert!(dir.join("tool.direct").is_file(), "sidecar sits beside the launcher");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let source_ino = std::fs::metadata(&source).expect("source").ino();
            let launcher_ino = std::fs::metadata(&launcher).expect("launcher").ino();
            assert_ne!(source_ino, launcher_ino, "a copy has its own inode");
        }
    }

    async fn write(sidecar: &DirectSidecar) -> (tempfile::TempDir, Result<(), VendorError>) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = write_direct_launcher(tmp.path(), "tool", b"blob", sidecar).await;
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
            assert!(matches!(result, Err(VendorError::ValueUnrepresentable { .. })), "{key}");
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
        let written: DirectSidecar =
            serde_json::from_slice(&std::fs::read(tmp.path().join("tool.direct")).expect("sidecar")).expect("json");
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
            assert!(tmp.path().join("tool.direct").is_file());
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
}
