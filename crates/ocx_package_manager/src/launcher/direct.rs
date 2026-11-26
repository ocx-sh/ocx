// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Direct launchers: the vendored directory's entry points, a copy of the launcher image beside a `.direct` sidecar.

use std::path::Path;

use ocx_env_apply::direct::DirectSidecar;

use crate::tasks::vendor::VendorError;

/// Writes the launcher `<name>` (`<name>.exe` on Windows) as a byte copy of `blob`, then `<name>.direct`.
///
/// The copy is never a hardlink: a vendored directory must survive its source being deleted or moved.
///
/// # Errors
///
/// [`VendorError::ValueUnrepresentable`] or [`VendorError::PathUnrepresentable`] when `sidecar` breaks a rule of
/// [`DirectSidecar::validate`], before anything is written; [`VendorError::Package`] for a failed write.
pub async fn write_direct_launcher(
    dir: &Path,
    name: &str,
    blob: &[u8],
    sidecar: &DirectSidecar,
) -> Result<(), VendorError> {
    let _ = (dir, name, blob, sidecar);
    unimplemented!("write_direct_launcher")
}

#[cfg(test)]
mod tests {
    use ocx_env_apply::direct::DirectSidecar;

    use super::write_direct_launcher;

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
}
