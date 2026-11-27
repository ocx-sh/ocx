// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! First-invocation materialization of a deferred tool, the library half of `ocx launcher shim`.

use std::collections::BTreeSet;

use crate::concurrency::Concurrency;
use crate::error::{Error, PackageError, PackageErrorKind};
use crate::tasks::find_or_install::{Arrival, FoundPackage};
use ocx_package::metadata::BinaryName;
use ocx_project::lazy::LazyReport;

use super::super::PackageManager;

impl PackageManager {
    /// The interface names a deferred tool's shim directory claims: one `<name>.launch` record per name under `bin/`.
    ///
    /// # Errors
    ///
    /// Propagates an I/O failure reading `bin/`; a non-UTF-8 or non-[`BinaryName`] file name is skipped.
    pub async fn claimed_shim_names(&self, package: &ocx_oci::PinnedPackageRef) -> crate::Result<BTreeSet<BinaryName>> {
        let bin = self.file_structure().shims.shim_dir(package).bin();
        let mut entries = match tokio::fs::read_dir(&bin).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Empty fails closed: every name is refused, so nothing downloads for a collected shim.
                log::debug!(
                    "No shim launchers for '{package}' at {}; the claim set is empty.",
                    bin.display()
                );
                return Ok(BTreeSet::new());
            }
            Err(error) => return Err(crate::error::file_error(&bin, error)),
        };

        // The record, not the launcher, names a claim: a Windows launcher is `<name>.exe`, and a claimed
        // `mytool.exe` has the launcher `mytool.exe.exe`. A claimed name ending `.launch` has no launcher at all.
        let suffix = format!(".{}", ocx_env_apply::launch::RECORD_EXTENSION);
        let mut claimed = BTreeSet::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| crate::error::file_error(&bin, error))?
        {
            let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
                log::debug!("Ignoring non-UTF-8 shim launcher in {}", bin.display());
                continue;
            };
            let Some(stem) = file_name.strip_suffix(&suffix) else {
                continue;
            };
            match BinaryName::try_from(stem) {
                Ok(name) => {
                    claimed.insert(name);
                }
                Err(error) => log::debug!("Ignoring '{file_name}' in {}: {error}", bin.display()),
            }
        }
        Ok(claimed)
    }

    /// Materializes a deferred tool by digest, writing nothing under `index/`.
    ///
    /// # Errors
    ///
    /// Whatever the pull surfaces, including
    /// [`PackageErrorKind::Internal`]`(`[`crate::Error::OfflineMode`]`)` when
    /// `--offline` or `--frozen` refuses the fetch (exit 81).
    pub async fn materialize_deferred(
        &self,
        package: &ocx_oci::PinnedPackageRef,
        platform: ocx_oci::Platform,
        report: LazyReport,
    ) -> Result<FoundPackage, Error> {
        let identifier = package.as_identifier().clone();
        // Read-only, or a `tag@digest` pull persists a dispatch object under `index/`, even under `--frozen`.
        let view = self.read_only_view();
        // Probe first, or every cached re-entry opens the terminal and paints a frame for no download.
        match view.find(&identifier, platform.clone()).await {
            Ok(info) => {
                return Ok(FoundPackage {
                    info,
                    arrival: Arrival::Cached,
                });
            }
            Err(PackageErrorKind::NotFound | PackageErrorKind::NotInstalled(_)) => {}
            Err(kind) => return Err(Error::FindFailed(vec![PackageError::new(identifier, kind)])),
        }

        // Never the ambient manager: a shim's stderr belongs to its invoker, which rendering would corrupt.
        let progress = match report {
            LazyReport::Silent => ocx_console::progress::ProgressManager::disabled(),
            LazyReport::Progress => ocx_console::progress::ProgressManager::controlling_terminal().await,
        };
        let manager = view.with_progress(progress);

        let installed = manager
            .find_or_install_all(std::slice::from_ref(&identifier), platform, Concurrency::cores())
            .await?;
        installed.into_iter().next().ok_or_else(|| {
            // Unreachable; an error, not a panic, so a contract change never aborts a user's tool.
            Error::FindFailed(vec![PackageError::new(identifier, PackageErrorKind::NotFound)])
        })
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use ocx_index::{ChainMode, Index, LocalConfig, LocalIndex};
    use ocx_oci::PackageRef;
    use ocx_store::file_structure::FileStructure;

    use super::*;

    /// A pinned identifier over `top_digest`, tag included — this keeps the
    /// advisory tag, and `tag@digest` is precisely the shape that reaches
    /// `persist_dispatch`.
    fn pinned(top_digest: &ocx_oci::Digest) -> ocx_oci::PinnedPackageRef {
        let identifier = PackageRef::parse(&format!("ocx.sh/tool/cmake:3.28@{top_digest}")).expect("fixture parses");
        ocx_oci::PinnedPackageRef::try_from(identifier).expect("fixture is digest-bearing")
    }

    /// Production's wiring (`context.rs`) minus any source: the blob store is
    /// attached, so a pinned image index resolves from staged content with zero
    /// network — the same seam a `--frozen` first invocation resolves through.
    fn manager_for(file_structure: &FileStructure) -> PackageManager {
        let index = Index::from_chained_with_content_store(
            LocalIndex::new(LocalConfig {
                index_store: ocx_index::IndexStore::machine_local(file_structure),
            }),
            vec![],
            ChainMode::Offline,
            file_structure.blobs.clone(),
        );
        PackageManager::new(file_structure.clone(), index, None, "localhost:5000")
    }

    /// A first-invocation materialization leaves the local
    /// index at **zero bytes** — not merely "moves no tag pointer".
    ///
    /// The sibling of
    /// `patch_discovery::tests::companion_install_writes_nothing_into_the_local_index_home`,
    /// and red for the same reason: routed through the default manager instead
    /// of `read_only_view()`, the `AbsentDispatch` recovery below writes the
    /// image index back as a dispatch object under `index/`.
    ///
    /// The pull is expected to fail — the selected platform leaf is
    /// deliberately absent from this store — and the refusal must name the
    /// child digest, or resolution never reached the point where a dispatch
    /// object would be written and the assertion proves nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn materializing_a_deferred_tool_writes_nothing_into_the_local_index_home() {
        let tmp = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(tmp.path().to_path_buf());
        let manager = manager_for(&file_structure);

        let child_digest = format!("sha256:{}", "b".repeat(64));
        let image_index = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child_digest}","size":2,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#
        );
        let top_digest = ocx_oci::Algorithm::Sha256.hash(image_index.as_bytes());
        let package = pinned(&top_digest);
        file_structure
            .blobs
            .write_blob(package.registry(), &top_digest, image_index.as_bytes())
            .await
            .expect("seed the tool's image index the way a pull stages it");

        let refusal = manager
            .materialize_deferred(
                &package,
                "linux/amd64".parse().expect("valid platform"),
                LazyReport::Silent,
            )
            .await
            .expect_err("the selected leaf is deliberately absent from this store");
        assert!(
            refusal.to_string().contains(&child_digest),
            "the pull must have recovered the image index and selected its linux/amd64 child, \
             or an empty index proves nothing; got: {refusal}"
        );

        assert!(
            !file_structure.root().join("index").exists(),
            "a deferred tool's materialization must leave the local index at zero bytes; found {} — \
             the dispatch object at {} is the usual culprit",
            file_structure.root().join("index").display(),
            ocx_index::IndexStore::machine_local(&file_structure)
                .dispatch_object_path(package.registry(), package.repository(), &top_digest)
                .display()
        );
    }

    /// The claim set is the `bin/` records: one name per `<name>.launch`, whatever the launcher beside it is called.
    ///
    /// Reds on a reader that lists launchers: it reports `cmake.exe` on a Windows tree, and a dotted claim such as
    /// `python3.12` or `mytool.exe` under its launcher's own name.
    #[tokio::test]
    async fn claimed_shim_names_reads_the_records() {
        let tmp = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(tmp.path().to_path_buf());
        let manager = manager_for(&file_structure);
        let package = pinned(&ocx_oci::Algorithm::Sha256.hash(b"content"));

        let bin = file_structure.shims.shim_dir(&package).bin();
        tokio::fs::create_dir_all(&bin).await.unwrap();
        for file in [
            "cmake",
            "cmake.launch",
            "python3.12",
            "python3.12.launch",
            "mytool.exe.exe",
            "mytool.exe.launch",
            "stray",
        ] {
            tokio::fs::write(bin.join(file), b"x").await.unwrap();
        }

        let claimed = manager
            .claimed_shim_names(&package)
            .await
            .expect("the listing is readable");
        let names: Vec<&str> = claimed.iter().map(BinaryName::as_str).collect();
        assert_eq!(names, ["cmake", "mytool.exe", "python3.12"]);
    }

    /// An absent shim directory claims nothing, so every name is refused — the
    /// fail-closed direction. Reds on a reader that treats "cannot enumerate"
    /// as "admit everything".
    #[tokio::test]
    async fn an_absent_shim_directory_claims_no_names() {
        let tmp = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(tmp.path().to_path_buf());
        let manager = manager_for(&file_structure);
        let package = pinned(&ocx_oci::Algorithm::Sha256.hash(b"content"));

        assert!(
            manager
                .claimed_shim_names(&package)
                .await
                .expect("an absent directory is not an error")
                .is_empty(),
            "no shim directory means no claimed names"
        );
    }
}
