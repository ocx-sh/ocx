// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Atomic landing of an already-populated temp layer directory into the layer store.

use std::path::Path;

use crate::error::PackageErrorKind;
use ocx_store::file_structure;

/// Renames `temp_path` into `layers/{registry}/{digest}/`; losing a concurrent race discards the temp and is `Ok`.
///
/// `temp_path` must already hold the layer's `content/` tree and a `digest` file.
pub(super) async fn finalize_layer_dir(
    fs: &file_structure::FileStructure,
    registry: &str,
    digest: &ocx_oci::Digest,
    temp_path: &Path,
) -> Result<(), PackageErrorKind> {
    let layer_path = fs.layers.path(registry, digest);
    let layer_content = fs.layers.content(registry, digest);
    // Without this probe a lost race burns the Windows rename retry's whole backoff (same ERROR_ACCESS_DENIED).
    if ocx_util::fs::path_exists_lossy(&layer_content).await {
        log::debug!("Layer {} already exists (race), cleaning up temp.", digest);
        let _ = tokio::fs::remove_dir_all(temp_path).await;
        return Ok(());
    }
    if let Some(parent) = layer_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(parent.to_path_buf(), e)))?;
    }
    match ocx_util::fs::rename_with_windows_retry(temp_path, &layer_path).await {
        Ok(()) => {
            log::debug!("Extracted layer {} to {}", digest, layer_path.display());
            Ok(())
        }
        Err(_) if ocx_util::fs::path_exists_lossy(&layer_content).await => {
            log::debug!("Layer {} already exists (race), cleaning up temp.", digest);
            let _ = tokio::fs::remove_dir_all(temp_path).await;
            Ok(())
        }
        Err(e) => Err(PackageErrorKind::Internal(crate::Error::InternalFile(
            temp_path.to_path_buf(),
            e,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_store::file_structure::FileStructure;
    use tempfile::TempDir;

    fn fixture_digest() -> ocx_oci::Digest {
        ocx_oci::Digest::try_from("sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .expect("valid digest")
    }

    async fn populate_temp(temp: &Path, digest: &ocx_oci::Digest) {
        let content = temp.join("content");
        tokio::fs::create_dir_all(&content).await.unwrap();
        tokio::fs::write(content.join("payload"), b"hello").await.unwrap();
        file_structure::write_digest_file(&temp.join(file_structure::DIGEST_FILENAME), digest)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rename_succeeds_when_destination_absent() {
        let home = TempDir::new().unwrap();
        let fs = FileStructure::with_root(home.path().to_path_buf());
        let digest = fixture_digest();
        let registry = "test.example";

        let layer_path = fs.layers.path(registry, &digest);
        let temp = layer_path.with_extension("_tmp");
        populate_temp(&temp, &digest).await;

        finalize_layer_dir(&fs, registry, &digest, &temp).await.unwrap();

        assert!(fs.layers.content(registry, &digest).join("payload").exists());
        assert!(!temp.exists(), "temp dir should be consumed by rename");
    }

    #[tokio::test]
    async fn race_branch_recovers_when_winner_already_landed() {
        let home = TempDir::new().unwrap();
        let fs = FileStructure::with_root(home.path().to_path_buf());
        let digest = fixture_digest();
        let registry = "test.example";

        // Pre-create the destination as if a concurrent task won the rename race.
        let layer_path = fs.layers.path(registry, &digest);
        let winner_content = fs.layers.content(registry, &digest);
        tokio::fs::create_dir_all(&winner_content).await.unwrap();
        tokio::fs::write(winner_content.join("payload"), b"winner")
            .await
            .unwrap();

        // Our temp dir holds independent content.
        let temp = layer_path.with_extension("_loser_tmp");
        populate_temp(&temp, &digest).await;

        // finalize_layer_dir should detect the race and return Ok(()) after best-effort cleanup.
        finalize_layer_dir(&fs, registry, &digest, &temp).await.unwrap();

        // Winner content survives untouched; loser temp removed.
        assert_eq!(
            tokio::fs::read(winner_content.join("payload")).await.unwrap(),
            b"winner",
        );
        assert!(!temp.exists(), "loser temp should be cleaned up");
    }
}
