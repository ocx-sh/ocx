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
use super::common;
use super::toolchain_names::{NotEnumerablePolicy, exposed_names};

impl PackageManager {
    /// The interface names the package's metadata declares: the exposed-name algebra [`PackageManager::prepare_lazy`]
    /// writes a tree from, so a launcher any release or generation published is judged by one answer.
    ///
    /// Read offline through a read-only view, from the manifests and config blobs the home already holds; nothing
    /// is fetched, so an unclaimed name never downloads and nothing is written under `index/`, `shims/` or any
    /// launcher directory.
    ///
    /// # Errors
    ///
    /// [`Error::FindFailed`] when the metadata is not readable locally (a blob removed by hand): the refusal
    /// names the pinned digest instead of guessing a claim set. `ocx clean` keeps every blob this read needs
    /// while the deferred tree stands.
    pub async fn claimed_shim_names(
        &self,
        package: &ocx_oci::PinnedPackageRef,
        platform: ocx_oci::Platform,
    ) -> crate::Result<BTreeSet<BinaryName>> {
        let local = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: self.effective_index_store(),
        });
        let view = self.offline_view(local).read_only_view();
        declared_names(&view, package, platform)
            .await
            .map_err(|kind| Error::FindFailed(vec![PackageError::new(package.as_identifier().clone(), kind)]))
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

/// `exposed_names` over the closure `view` can read, the same walk `prepare_lazy` runs.
async fn declared_names(
    view: &PackageManager,
    package: &ocx_oci::PinnedPackageRef,
    platform: ocx_oci::Platform,
) -> Result<BTreeSet<BinaryName>, PackageErrorKind> {
    let resolved = view.resolve(package.as_identifier(), platform.clone()).await?;
    let metadata = common::load_config_metadata(view.index(), &resolved.pinned, &resolved.final_manifest).await?;
    let config_digest = common::config_blob_digest(&resolved.final_manifest)?;
    let nodes = common::walk_closure_nodes(
        view.file_structure(),
        view.index(),
        true,
        &resolved.pinned,
        &metadata,
        config_digest,
        &platform,
    )
    .await?;
    // `Refuse`, as `prepare_lazy` writes: a node that cannot be enumerated claims nothing.
    Ok(exposed_names(&nodes, NotEnumerablePolicy::Refuse)?
        .into_keys()
        .collect())
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

    const HOST: &str = "linux/amd64";

    /// Stages a single-platform package the way a pull leaves it in the blob store: an image index naming one
    /// leaf, the leaf manifest and its metadata config blob. Returns the leaf's pinned identifier.
    async fn stage_package(file_structure: &FileStructure, metadata: &str) -> ocx_oci::PinnedPackageRef {
        let registry = "ocx.sh";
        let config_bytes = metadata.as_bytes();
        let config_digest = ocx_oci::Algorithm::Sha256.hash(config_bytes);
        let leaf = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{}","digest":"{config_digest}","size":{}}},"layers":[]}}"#,
            ocx_oci::media_type::MEDIA_TYPE_PACKAGE_METADATA_V1,
            config_bytes.len(),
        );
        let leaf_digest = ocx_oci::Algorithm::Sha256.hash(leaf.as_bytes());
        let image_index = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{leaf_digest}","size":{},"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#,
            leaf.len(),
        );
        let top_digest = ocx_oci::Algorithm::Sha256.hash(image_index.as_bytes());
        for (digest, bytes) in [
            (&config_digest, config_bytes),
            (&leaf_digest, leaf.as_bytes()),
            (&top_digest, image_index.as_bytes()),
        ] {
            file_structure
                .blobs
                .write_blob(registry, digest, bytes)
                .await
                .expect("stage a blob");
        }
        pinned(&top_digest)
    }

    /// The claim set is what the package metadata declares, whether or not any `deferred/` tree exists. Reds on a
    /// reader that lists a launcher directory: a package whose trees are gone, or were never written by this
    /// release, would claim nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn claims_come_from_the_metadata_when_no_deferred_tree_exists() {
        let tmp = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(tmp.path().to_path_buf());
        let manager = manager_for(&file_structure);
        let package = stage_package(
            &file_structure,
            r#"{"type":"bundle","version":1,"binaries":["cmake","ctest"]}"#,
        )
        .await;

        let claimed = manager
            .claimed_shim_names(&package, HOST.parse().expect("platform"))
            .await
            .expect("the metadata blobs are in the store");
        let names: Vec<&str> = claimed.iter().map(BinaryName::as_str).collect();
        assert_eq!(names, ["cmake", "ctest"]);
        assert!(
            !file_structure.root().join("deferred").exists(),
            "reading writes no tree"
        );
        assert!(
            !file_structure.root().join("index").exists(),
            "reading writes nothing under index/"
        );
    }

    /// Trees of an older release (`shims/`) and of another generation, carrying other names, change nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn launcher_trees_with_other_names_do_not_change_the_claim_set() {
        let tmp = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(tmp.path().to_path_buf());
        let manager = manager_for(&file_structure);
        let package = stage_package(
            &file_structure,
            r#"{"type":"bundle","version":1,"binaries":["cmake","ctest"]}"#,
        )
        .await;

        let older = file_structure.shims.shim_dir(&package).bin();
        let other = ocx_store::launcher_generation::LauncherGeneration::from_hex("0123456789abcdef").expect("hex");
        let other_bin = file_structure.deferred(&other).shim_dir(&package).bin();
        for dir in [&older, &other_bin] {
            std::fs::create_dir_all(dir).unwrap();
            for file in ["ghost", "ghost.exe", "ghost.shimref", "ghost.launch"] {
                std::fs::write(dir.join(file), b"x").unwrap();
            }
        }

        let claimed = manager
            .claimed_shim_names(&package, HOST.parse().expect("platform"))
            .await
            .expect("the metadata blobs are in the store");
        let names: Vec<&str> = claimed.iter().map(BinaryName::as_str).collect();
        assert_eq!(names, ["cmake", "ctest"], "no directory listing decides membership");
    }

    /// Metadata that is not in the store claims nothing and refuses with the manifest digest, never an empty set
    /// that reads as "refuse" by accident nor an admit-everything one.
    #[tokio::test(flavor = "multi_thread")]
    async fn unreadable_metadata_is_an_error_and_downloads_nothing() {
        let tmp = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(tmp.path().to_path_buf());
        let manager = manager_for(&file_structure);
        let package = pinned(&ocx_oci::Algorithm::Sha256.hash(b"content"));

        let error = manager
            .claimed_shim_names(&package, HOST.parse().expect("platform"))
            .await
            .expect_err("nothing is staged for this digest");
        assert!(matches!(error, Error::FindFailed(_)), "{error:?}");
    }
}
