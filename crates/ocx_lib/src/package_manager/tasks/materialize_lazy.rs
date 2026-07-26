// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! First-invocation materialization of a **deferred** tool — the library half
//! of `ocx launcher shim` (plan contracts C-011, S-006).
//!
//! A shim directory exists precisely while a tool's content does not. Invoking
//! any of its generated launchers lands here, and the two things that have to
//! happen before the real target can be executed both live in this module
//! rather than in the CLI verb: reading the name set the shim store claims, and
//! pulling the package.
//!
//! **The pull runs on [`PackageManager::read_only_view`], and that is the
//! contract, not an optimization.** A deferred tool is composed from
//! `ocx.lock`, so its materialization is the same index-free resolve the lock
//! already promises — a `tag@digest` pull skips the tag pointer but under
//! [`LocalWritePolicy::Full`](crate::oci::index::LocalWritePolicy) still
//! persists a dispatch object under `index/`, and the blob store's
//! `AbsentDispatch` recovery writes one back the other way. Either would let a
//! lazily composed tool grow the local index where its eager twin does not, and
//! would leave a `--frozen` first invocation writing there at all. Content is
//! unaffected: blobs, layers and the package tree are written exactly as an
//! eager install writes them.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::Path;

use crate::lazy::LazyReport;
use crate::package::metadata::BinaryName;
use crate::package_manager::concurrency::Concurrency;
use crate::package_manager::error::{Error, PackageError, PackageErrorKind};
use crate::package_manager::tasks::find_or_install::FoundPackage;
use crate::{log, oci};

use super::super::PackageManager;

/// Extensions a Windows producer appends to a launcher it already wrote
/// extensionless.
///
/// `write_shim_launchers` writes only the extensionless file on every platform
/// today; a Windows producer adds `<name>.exe` and its `<name>.shimref`
/// sidecar. `.shimref` and not `.shim`: a shim tree's `bin/` is the one
/// directory the deferred-tool grammar is written into (`ocx_shim`'s
/// `SIDECAR_PROBE_ORDER`), and `.shim` names an installed package under
/// `entrypoints/`, which never appears here.
const GENERATED_SIBLING_EXTENSIONS: [&str; 2] = ["exe", "shimref"];

/// Whether `file_name` is a generated sibling of another launcher in the same
/// listing, rather than a claimed name in its own right.
///
/// The **pair** is what makes it a sibling, not the extension: `BinaryName`
/// permits interior dots and imposes no suffix rule, so a publisher may claim
/// `mytool.exe` outright and `prepare_lazy` writes exactly one launcher for it.
/// Skipping on the extension alone dropped that name from the claim set while
/// leaving its launcher on `PATH`, so every invocation was refused as
/// unclaimed — naming the wrong defect, since it *was* claimed. A sibling
/// therefore has to be accompanied by the extensionless file it belongs to.
///
/// Taking a file *stem* unconditionally would be wrong in the other direction:
/// a claimed `python3.12` has `.12` for an extension and would be reported as
/// `python3`.
fn is_generated_sibling(file_name: &str, listing: &BTreeSet<&str>) -> bool {
    let Some(extension) = Path::new(file_name).extension().and_then(OsStr::to_str) else {
        return false;
    };
    if !GENERATED_SIBLING_EXTENSIONS.contains(&extension) {
        return false;
    }
    // `- 1` for the dot; `extension()` guarantees both are present.
    listing.contains(&file_name[..file_name.len() - extension.len() - 1])
}

impl PackageManager {
    /// The interface names a deferred tool's shim directory claims — one
    /// generated launcher per name under `bin/`.
    ///
    /// The launchers **are** the name set, so this reads the directory rather
    /// than re-deriving the union from the closure's ref-linked config blobs:
    /// it costs no closure walk on every first invocation, and it is the set
    /// the invoked shim actually came from. Trusting the store's directory
    /// contents is no weaker than trusting the shim body itself, which C-011's
    /// trust-boundary paragraph already concedes.
    ///
    /// An **absent** shim directory yields an empty set rather than an error.
    /// That is fail-closed: an empty set claims nothing, so every name is
    /// refused by [`PackageErrorKind::ShimNameNotClaimed`] and no download is
    /// triggered by a shim whose store entry is gone.
    ///
    /// # Errors
    ///
    /// Propagates an I/O failure reading the `bin/` directory. A file name that
    /// is not valid UTF-8, or does not satisfy the [`BinaryName`] grammar, is
    /// skipped with a debug log — it cannot be the name any shim was invoked
    /// under, since `argv0` clears the same grammar first.
    pub async fn claimed_shim_names(&self, package: &oci::PinnedIdentifier) -> crate::Result<BTreeSet<BinaryName>> {
        let bin = self.file_structure().shims.shim_dir(package).bin();
        let mut entries = match tokio::fs::read_dir(&bin).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                log::debug!(
                    "No shim launchers for '{package}' at {}; the claim set is empty.",
                    bin.display()
                );
                return Ok(BTreeSet::new());
            }
            Err(error) => return Err(crate::error::file_error(&bin, error)),
        };

        // Read the whole listing before classifying any of it: a `.exe` is a
        // generated sibling only when the extensionless file it belongs to is
        // also present, which is not knowable one entry at a time.
        let mut listing: Vec<String> = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| crate::error::file_error(&bin, error))?
        {
            match entry.file_name().to_str() {
                Some(file_name) => listing.push(file_name.to_owned()),
                None => log::debug!("Ignoring non-UTF-8 shim launcher in {}", bin.display()),
            }
        }

        let names: BTreeSet<&str> = listing.iter().map(String::as_str).collect();
        let mut claimed = BTreeSet::new();
        for file_name in &names {
            if is_generated_sibling(file_name, &names) {
                continue;
            }
            match BinaryName::try_from(*file_name) {
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
    /// The ordinary pull, on a [`read_only_view`](PackageManager::read_only_view)
    /// — see the module doc for why the view is the contract. `report` decides
    /// whether the download renders progress; it is resolved by the caller from
    /// the full `lazy-report` ladder and consumed here, the one place that
    /// knows when the transfer starts and ends.
    ///
    /// Returns the [`FoundPackage`] rather than the bare install, because a
    /// shim's caller has to report `resolution.autoInstalled`: a first
    /// invocation pulls, a second one finds the same package already in the
    /// store, and only the [`Arrival`](super::find_or_install::Arrival) tells
    /// the two apart.
    ///
    /// Neither arm inherits the ambient manager. A shim runs *inside* another
    /// tool's process tree, so its stderr belongs to whatever invoked it —
    /// `make`, a build step, a wrapper script — and is the wrong channel in
    /// both directions: rendering there corrupts the caller's stream, and
    /// suppressing because it is a pipe hides a multi-hundred-megabyte download
    /// behind a silent hang. [`Progress`](LazyReport::Progress) therefore opens
    /// the controlling terminal, degrading to silence when there is none.
    ///
    /// # Errors
    ///
    /// Whatever the pull surfaces — including
    /// [`PackageErrorKind::Internal`]`(`[`crate::Error::OfflineMode`]`)` when
    /// `--offline` or `--frozen` refuses the fetch, which is the exit-81 leg of
    /// C-011.
    pub async fn materialize_deferred(
        &self,
        package: &oci::PinnedIdentifier,
        platform: oci::Platform,
        report: LazyReport,
    ) -> Result<FoundPackage, Error> {
        let progress = match report {
            LazyReport::Silent => crate::cli::progress::ProgressManager::disabled(),
            LazyReport::Progress => crate::cli::progress::ProgressManager::controlling_terminal().await,
        };
        let manager = self.read_only_view().with_progress(progress);

        let identifier = package.as_identifier().clone();
        let installed = manager
            .find_or_install_all(std::slice::from_ref(&identifier), platform, Concurrency::cores())
            .await?;
        installed.into_iter().next().ok_or_else(|| {
            // `find_or_install_all` returns one entry per input identifier, in
            // input order, so a one-element request either failed above or
            // yields exactly one entry. Unreachable — an error rather than a
            // panic so a future change to that contract degrades instead of
            // aborting a user's tool invocation.
            Error::FindFailed(vec![PackageError::new(identifier, PackageErrorKind::NotFound)])
        })
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use crate::{
        file_structure::FileStructure,
        oci::{
            Identifier,
            index::{ChainMode, Index, LocalConfig, LocalIndex},
        },
    };

    use super::*;

    /// A pinned identifier over `top_digest`, tag included — C-011 keeps the
    /// advisory tag, and `tag@digest` is precisely the shape that reaches
    /// `persist_dispatch`.
    fn pinned(top_digest: &oci::Digest) -> oci::PinnedIdentifier {
        let identifier = Identifier::parse(&format!("ocx.sh/tool/cmake:3.28@{top_digest}")).expect("fixture parses");
        oci::PinnedIdentifier::try_from(identifier).expect("fixture is digest-bearing")
    }

    /// Production's wiring (`context.rs`) minus any source: the blob store is
    /// attached, so a pinned image index resolves from staged content with zero
    /// network — the same seam a `--frozen` first invocation resolves through.
    fn manager_for(file_structure: &FileStructure) -> PackageManager {
        let index = Index::from_chained_with_content_store(
            LocalIndex::new(LocalConfig {
                index_store: file_structure.index.clone(),
            }),
            vec![],
            ChainMode::Offline,
            file_structure.blobs.clone(),
        );
        PackageManager::new(file_structure.clone(), index, None, "localhost:5000")
    }

    /// S-006 / C-011 (F-4): a first-invocation materialization leaves the local
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
        let top_digest = oci::Algorithm::Sha256.hash(image_index.as_bytes());
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
            !file_structure.index.root().exists(),
            "a deferred tool's materialization must leave the local index at zero bytes; found {} — \
             the dispatch object at {} is the usual culprit",
            file_structure.index.root().display(),
            file_structure
                .index
                .dispatch_object_path(package.registry(), package.repository(), &top_digest)
                .display()
        );
    }

    /// The claim set is the `bin/` listing: one entry per generated launcher,
    /// with the Windows `.exe`/`.shimref` siblings folded onto the same name
    /// rather than reported as three.
    ///
    /// The fixture names the sidecar `.shimref`, which is the extension a shim
    /// tree's `bin/` can actually hold — `.shim` belongs to `entrypoints/` and
    /// never lands here. Reds on a skip list that names `.shim`: `cmake.shimref`
    /// is then admitted as a fourth claimed name, and a Windows shim invoked as
    /// `cmake` would be refused for a name the store does list.
    #[tokio::test]
    async fn claimed_shim_names_reads_the_bin_listing() {
        let tmp = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(tmp.path().to_path_buf());
        let manager = manager_for(&file_structure);
        let package = pinned(&oci::Algorithm::Sha256.hash(b"content"));

        let bin = file_structure.shims.shim_dir(&package).bin();
        tokio::fs::create_dir_all(&bin).await.unwrap();
        for launcher in ["cmake", "cmake.exe", "cmake.shimref", "python3.12", "ctest"] {
            tokio::fs::write(bin.join(launcher), b"#!/bin/sh\n").await.unwrap();
        }

        let claimed = manager
            .claimed_shim_names(&package)
            .await
            .expect("the listing is readable");
        let names: Vec<&str> = claimed.iter().map(BinaryName::as_str).collect();
        assert_eq!(
            names,
            ["cmake", "ctest", "python3.12"],
            "a dotted name keeps its suffix; only the `.exe`/`.shimref` siblings are folded away"
        );
    }

    /// A publisher may claim `mytool.exe` outright — `BinaryName` permits
    /// interior dots and imposes no suffix rule — and `prepare_lazy` then writes
    /// exactly one launcher, with no extensionless partner.
    ///
    /// Reds on a fold that keys on the extension alone: `mytool.exe` drops out
    /// of the claim set while its launcher stays on `PATH`, so every invocation
    /// is refused as unclaimed — which names the wrong defect, since the
    /// publisher did claim it.
    #[tokio::test]
    async fn a_claimed_dot_exe_name_without_a_partner_is_not_a_generated_sibling() {
        let tmp = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(tmp.path().to_path_buf());
        let manager = manager_for(&file_structure);
        let package = pinned(&oci::Algorithm::Sha256.hash(b"content"));

        let bin = file_structure.shims.shim_dir(&package).bin();
        tokio::fs::create_dir_all(&bin).await.unwrap();
        // `mytool.exe` stands alone; `cmake` + `cmake.exe` are a real pair.
        for launcher in ["mytool.exe", "cmake", "cmake.exe"] {
            tokio::fs::write(bin.join(launcher), b"#!/bin/sh\n").await.unwrap();
        }

        let claimed = manager
            .claimed_shim_names(&package)
            .await
            .expect("the listing is readable");
        let names: Vec<&str> = claimed.iter().map(BinaryName::as_str).collect();
        assert_eq!(
            names,
            ["cmake", "mytool.exe"],
            "a `.exe` is folded away only when the extensionless launcher it belongs to is present"
        );
    }

    /// An absent shim directory claims nothing, so every name is refused — the
    /// fail-closed direction. Reds on a reader that treats "cannot enumerate"
    /// as "admit everything".
    #[tokio::test]
    async fn an_absent_shim_directory_claims_no_names() {
        let tmp = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(tmp.path().to_path_buf());
        let manager = manager_for(&file_structure);
        let package = pinned(&oci::Algorithm::Sha256.hash(b"content"));

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
