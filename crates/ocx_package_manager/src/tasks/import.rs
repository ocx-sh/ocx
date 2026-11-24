// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package import` and `ocx import`: assemble packages from an export directory with no registry traffic.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ocx_index::{
    FileIndexTransport, Index, IndexStore, LocalConfig, LocalIndex, OcxIndex, OcxIndexConfig, PlannedTag, TagOutcome,
    TagPlan,
};
use ocx_oci::blob_lookup::export_marker::{self, ExportMarker, ExportMarkerError};
use ocx_oci::blob_lookup::{BlobLookup, BlobWriter, LookupError, MAX_REFERRERS_PER_SUBJECT, markers};
use ocx_oci::client::OverlayMode;
use ocx_oci::{Digest, Manifest, PackageRef, PinnedPackageRef, Platform, Selection};

use super::auto_verify::{ReferrerSource, SIGNATURE_ARTIFACT_TYPES};
use super::common::{find_in_store, lookup_error};
use super::export::LockPin;
use super::resolve::{PatchRootScope, snapshot_companion_roots};
use crate::PackageManager;
use crate::error::{Error, PackageErrorKind, Result};

/// What `ocx package import` was asked to import.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    /// The export directory (`ocx-export.json`, `index/`, `blobs/`).
    pub input: PathBuf,
    /// Packages named on the command line; empty imports everything the export lists.
    /// A named package may move a home pin (`replaced`); an unnamed one only reports a `conflict`.
    pub packages: Vec<PackageRef>,
    /// Platforms to materialize; empty means the host platform.
    pub platforms: Vec<Platform>,
}

/// What `ocx import` (the root form) was asked to import: a project's lock, from an export.
#[derive(Debug, Clone)]
pub struct ProjectImportRequest {
    pub input: PathBuf,
    /// The pins of the selected groups and platforms.
    pub lock_pins: Vec<LockPin>,
    pub platforms: Vec<Platform>,
    /// The project directory to register; `None` under `--global`, which registers nothing.
    pub project_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportPlatformStatus {
    Materialized,
    /// Already in the home.
    Present,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedPlatform {
    pub platform: Platform,
    pub digest: Digest,
    pub status: ImportPlatformStatus,
}

#[derive(Debug, Clone)]
pub struct ImportedPackage {
    /// As listed; a lock pin reads `registry/repository@sha256:...`.
    pub identifier: PackageRef,
    /// `None` for a digest identifier and a root import, which pin no tag.
    pub tags: Option<Vec<TagOutcome>>,
    /// Platforms materialized for `added`, `unchanged` and `replaced` tags; a `conflict` tag adds none.
    pub platforms: Vec<ImportedPlatform>,
}

#[derive(Debug, Clone, Default)]
pub struct ImportOutcome {
    pub items: Vec<ImportedPackage>,
}

impl PackageManager {
    /// Assembles the packages of an export directory in the home: content first, pins last. Creates no install symlink.
    ///
    /// Content is read from `DIR/blobs` and the configured lookup only, addressed by logical registry and
    /// digest; an export root's `repository` pointer is never resolved or dialled. Each package commits its
    /// pins only after all its platforms are in the home, so a failure leaves earlier packages complete and
    /// the failing one unpinned.
    ///
    /// # Errors
    ///
    /// [`crate::Error::ExportDirInvalid`] for a missing marker, an unmapped directory or a root that
    /// disagrees with its catalog; [`crate::Error::ExportNotFound`] for a registry, package, tag, platform or
    /// blob the export lacks; verification and I/O failures.
    pub async fn import(&self, request: ImportRequest) -> Result<ImportOutcome> {
        let export = ExportDir::open(&request.input).await?;
        let plans = export.plan(&request.packages)?;
        let platforms = if request.platforms.is_empty() {
            vec![Platform::current().unwrap_or_else(Platform::any)]
        } else {
            request.platforms.clone()
        };

        let mut outcome = ImportOutcome::default();
        for plan in plans {
            outcome
                .items
                .push(self.import_package(&export, plan, &platforms).await?);
        }
        Ok(outcome)
    }

    /// Assembles a project's locked tools from an export directory and registers the project. Writes no index
    /// pin; the caller renders the toolchain home, as after a pull.
    ///
    /// # Errors
    ///
    /// As [`Self::import`]; a pin whose registry the export's marker does not name is
    /// [`crate::Error::ExportNotFound`].
    pub async fn import_project(&self, request: ProjectImportRequest) -> Result<ImportOutcome> {
        let export = ExportDir::open(&request.input).await?;
        let mut pins: Vec<&LockPin> = Vec::with_capacity(request.lock_pins.len());
        for pin in &request.lock_pins {
            let registry = pin.identifier.registry();
            if !export.registries.iter().any(|known| known == registry) {
                return Err(export.not_found(format!("registry '{registry}'")));
            }
            if !pins
                .iter()
                .any(|seen| seen.identifier == pin.identifier && seen.platform == pin.platform)
            {
                pins.push(pin);
            }
        }

        let mut outcome = ImportOutcome::default();
        for pin in pins {
            let identifier = PackageRef::from(pin.identifier.clone());
            let view = self.import_view(&export.dir, identifier.registry());
            let leaf = self
                .import_leaf(&export, &view, &identifier, pin.platform.clone())
                .await?;
            outcome.items.push(ImportedPackage {
                identifier,
                tags: None,
                platforms: vec![leaf],
            });
        }

        self.import_snapshot(&export, &outcome).await?;

        // Last, so a failed import leaves no ledger entry for a project whose tools are not all in the home.
        if let Some(project_dir) = &request.project_dir {
            ocx_project::registry::register_project_dir_if_absent_best_effort(
                &project_dir.join("ocx.toml"),
                self.file_structure().root(),
            )
            .await;
        }
        Ok(outcome)
    }
}

impl PackageManager {
    /// Assembles the companions and copies the descriptors an active patch snapshot pins, so the composed
    /// environment resolves them from the home once the export is gone.
    async fn import_snapshot(&self, export: &ExportDir, imported: &ImportOutcome) -> Result<()> {
        let Some(snapshot) = self.patch_snapshot() else {
            return Ok(());
        };
        let mut platforms: Vec<Platform> = Vec::new();
        for platform in imported
            .items
            .iter()
            .flat_map(|item| &item.platforms)
            .map(|p| &p.platform)
        {
            if !platforms.contains(platform) {
                platforms.push(platform.clone());
            }
        }
        for (identifier, digest) in snapshot_companion_roots(self, PatchRootScope::RecordedAndSnapshot) {
            let pinned = identifier.clone_with_digest(digest.clone());
            let view = self.import_view(&export.dir, pinned.registry());
            let fetched = view
                .index()
                .fetch_manifest_raw_bytes_with_origin(&pinned)
                .await?
                .ok_or_else(|| export.not_found(format!("patch companion '{pinned}'")))?;
            let leaves = match fetched.value.2 {
                Manifest::ImageIndex(index) => {
                    let wanted = if platforms.is_empty() {
                        vec![Platform::any()]
                    } else {
                        platforms.clone()
                    };
                    select_leaves(export, &pinned, &index, &wanted)?
                }
                Manifest::Image(_) => vec![(digest.clone(), Platform::any())],
            };
            let mut done: Vec<Digest> = Vec::with_capacity(leaves.len());
            for (leaf_digest, platform) in leaves {
                if done.contains(&leaf_digest) {
                    continue;
                }
                let leaf = identifier.clone_with_digest(leaf_digest.clone());
                self.import_leaf(export, &view, &leaf, platform).await?;
                done.push(leaf_digest);
            }
        }
        let home = self.file_structure().blobs.root().to_path_buf();
        for (key, digest) in &snapshot.descriptors {
            let Some((registry, _)) = key.split_once('/') else {
                continue;
            };
            let lookup = self.import_view(&export.dir, registry).blob_lookup().clone();
            let missing = || export.not_found(format!("patch descriptor {digest} of '{key}'"));
            let manifest_bytes = lookup
                .read_metadata(registry, digest, MAX_REFERRER_BYTES)
                .await
                .map_err(|error| lookup_error(&export.dir, error))?
                .ok_or_else(missing)?;
            let manifest: ocx_oci::ImageManifest = serde_json::from_slice(&manifest_bytes)?;
            write_blob_to_home(&export.dir, &home, registry, digest, manifest_bytes).await?;
            for layer in &manifest.layers {
                let layer_digest = Digest::try_from(layer.digest.as_str())?;
                // Strict, unlike `copy_into_home`: a copy failing its digest check is a data error, not absent.
                let layer_bytes = lookup
                    .read_metadata(registry, &layer_digest, MAX_REFERRER_BYTES)
                    .await
                    .map_err(|error| lookup_error(&export.dir, error))?
                    .ok_or_else(missing)?;
                write_blob_to_home(&export.dir, &home, registry, &layer_digest, layer_bytes).await?;
            }
        }
        Ok(())
    }
}

/// What an export directory holds, read and checked before anything is written to the home.
struct ExportDir {
    dir: PathBuf,
    index: IndexStore,
    /// Registry as written in the marker, with the roots the export's index holds for it.
    roots: Vec<ExportRoot>,
    registries: Vec<String>,
}

struct ExportRoot {
    registry: String,
    repository: String,
    tags: BTreeMap<String, Digest>,
}

/// One package to import: the tags it adopts, or the single digest a digest identifier names.
struct PackagePlan {
    identifier: PackageRef,
    named: bool,
    tags: Vec<(String, Digest)>,
    digest: Option<Digest>,
}

/// Ceiling on a referrer manifest or blob read into memory.
const MAX_REFERRER_BYTES: u64 = 64 * 1024 * 1024;

fn join_error(path: &Path, join: tokio::task::JoinError) -> Error {
    Error::InternalFile(path.to_path_buf(), std::io::Error::other(join))
}

impl ExportDir {
    fn invalid(&self, reason: impl Into<String>) -> Error {
        Error::ExportDirInvalid {
            dir: self.dir.clone(),
            reason: reason.into(),
        }
    }

    fn not_found(&self, what: impl Into<String>) -> Error {
        Error::ExportNotFound {
            dir: self.dir.clone(),
            what: what.into(),
        }
    }

    /// Reads the marker and every root of the export's index, checking each root against its catalog entry.
    async fn open(dir: &Path) -> Result<Self> {
        let marker: ExportMarker = {
            let path = dir.to_path_buf();
            tokio::task::spawn_blocking(move || export_marker::read_marker(&path))
                .await
                .map_err(|join| join_error(dir, join))?
                .map_err(|error| match error {
                    ExportMarkerError::Read(error) => ocx_index::error::Error::from(error).into(),
                    other => Error::ExportDirInvalid {
                        dir: dir.to_path_buf(),
                        reason: other.to_string(),
                    },
                })?
                .ok_or_else(|| Error::ExportDirInvalid {
                    dir: dir.to_path_buf(),
                    reason: format!("it has no {}", export_marker::MARKER_FILE),
                })?
        };
        let mut export = Self {
            dir: dir.to_path_buf(),
            index: IndexStore::new(dir.join("index")).refusing_links(),
            roots: Vec::new(),
            registries: marker.registries.values().cloned().collect(),
        };

        let walked = {
            let path = dir.to_path_buf();
            tokio::task::spawn_blocking(move || check_index_tree(&path))
                .await
                .map_err(|join| join_error(dir, join))?
                .map_err(|error| Error::InternalFile(dir.join("index"), error))?
        };
        if let Some(reason) = walked {
            return Err(export.invalid(reason));
        }

        let index_dir = dir.join("index");
        match tokio::fs::read_dir(&index_dir).await {
            Ok(mut entries) => {
                while let Some(entry) = entries
                    .next_entry()
                    .await
                    .map_err(|error| Error::InternalFile(index_dir.clone(), error))?
                {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    // `locks` is the writer's own lock directory, not a registry.
                    if name != "locks" && !marker.registries.contains_key(&name) {
                        return Err(
                            export.invalid(format!("index directory '{name}' is not a registry the marker names"))
                        );
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::InternalFile(index_dir, error)),
        }

        for registry in marker.registries.values() {
            let catalog = export
                .index
                .read_source_catalog(registry)
                .await
                .map_err(|error| index_read_error(&export, error))?
                .unwrap_or_default();
            let repositories = export
                .index
                .list_wire_repositories(registry)
                .await
                .map_err(|error| index_read_error(&export, error))?;
            for repository in repositories {
                let bytes = export
                    .index
                    .read_root_document_bytes(registry, &repository)
                    .await
                    .map_err(|error| index_read_error(&export, error))?
                    .ok_or_else(|| {
                        export.invalid(format!(
                            "root of '{registry}/{repository}' is unreadable; re-run export"
                        ))
                    })?;
                let actual = IndexStore::root_catalog_entry(&bytes);
                if catalog.get(&repository) != Some(&actual) {
                    return Err(export.invalid(format!(
                        "root of '{registry}/{repository}' does not match its catalog entry; re-run export"
                    )));
                }
                let tags = parse_root_tags(&bytes).map_err(|reason| {
                    export.invalid(format!("root of '{registry}/{repository}': {reason}; re-run export"))
                })?;
                export.roots.push(ExportRoot {
                    registry: registry.clone(),
                    repository,
                    tags,
                });
            }
        }
        Ok(export)
    }

    /// Which packages and tags to import: every root when nothing is named, else the named ones.
    fn plan(&self, named: &[PackageRef]) -> Result<Vec<PackagePlan>> {
        if named.is_empty() {
            if self.roots.is_empty() {
                return Err(self.invalid("it has no index to import; a project export imports with `ocx import`"));
            }
            return Ok(self
                .roots
                .iter()
                .map(|root| PackagePlan {
                    identifier: PackageRef::new_registry(&root.repository, &root.registry),
                    named: false,
                    tags: root
                        .tags
                        .iter()
                        .map(|(tag, digest)| (tag.clone(), digest.clone()))
                        .collect(),
                    digest: None,
                })
                .collect());
        }

        let mut plans = Vec::with_capacity(named.len());
        for package in named {
            if !self.registries.iter().any(|registry| registry == package.registry()) {
                return Err(self.not_found(format!("registry '{}'", package.registry())));
            }
            if let Some(digest) = package.digest() {
                plans.push(PackagePlan {
                    identifier: package.clone(),
                    named: true,
                    tags: Vec::new(),
                    digest: Some(digest),
                });
                continue;
            }
            let root = self
                .roots
                .iter()
                .find(|root| root.registry == package.registry() && root.repository == package.repository())
                .ok_or_else(|| self.not_found(format!("package '{}/{}'", package.registry(), package.repository())))?;
            let tags = match package.tag() {
                Some(tag) => {
                    let digest = root.tags.get(tag).ok_or_else(|| {
                        self.not_found(format!("tag '{tag}' of '{}/{}'", root.registry, root.repository))
                    })?;
                    vec![(tag.to_owned(), digest.clone())]
                }
                None => root
                    .tags
                    .iter()
                    .map(|(tag, digest)| (tag.clone(), digest.clone()))
                    .collect(),
            };
            plans.push(PackagePlan {
                identifier: PackageRef::new_registry(&root.repository, &root.registry),
                named: true,
                tags,
                digest: None,
            });
        }
        Ok(plans)
    }
}

/// The `tag -> dispatch digest` rows of a root document.
fn parse_root_tags(bytes: &[u8]) -> std::result::Result<BTreeMap<String, Digest>, String> {
    let root: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let Some(rows) = root["tags"].as_object() else {
        return Err("it lists no tags object".to_owned());
    };
    rows.iter()
        .map(|(tag, row)| {
            let content = row["content"]
                .as_str()
                .ok_or_else(|| format!("tag '{tag}' has no content digest"))?;
            let digest = Digest::try_from(content).map_err(|error| format!("tag '{tag}': {error}"))?;
            Ok((tag.clone(), digest))
        })
        .collect()
}

/// The pins the home holds for `registry/repository`; empty when it has no root.
async fn home_tags(store: &IndexStore, registry: &str, repository: &str) -> Result<BTreeMap<String, Digest>> {
    match store.read_root_document_bytes(registry, repository).await? {
        Some(bytes) => parse_root_tags(&bytes).map_err(|reason| Error::ExportDirInvalid {
            dir: store.root().to_path_buf(),
            reason: format!("the home root of '{registry}/{repository}': {reason}"),
        }),
        None => Ok(BTreeMap::new()),
    }
}

fn plan_tags(tags: &[(String, Digest)], home: &BTreeMap<String, Digest>, named: bool) -> TagPlan {
    let mut plan = TagPlan::default();
    for (tag, digest) in tags {
        let mut planned = PlannedTag {
            tag: tag.clone(),
            digest: digest.clone(),
            home_digest: None,
        };
        match home.get(tag) {
            None => plan.added.push(planned),
            Some(current) if current == digest => plan.unchanged.push(planned),
            Some(current) => {
                planned.home_digest = Some(current.clone());
                if named {
                    plan.replaced.push(planned);
                } else {
                    plan.conflict.push(planned);
                }
            }
        }
    }
    plan
}

/// Surfaces an [`Error::ExportNotFound`] a pull's shared setup error carries, so the import answers with the
/// typed error; every other failure keeps its chain for the caller's walker.
fn peel_missing(error: Error) -> Error {
    let Error::Dependency(crate::DependencyError::SetupFailed(shared)) = &error else {
        return error;
    };
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(shared);
    while let Some(current) = cause {
        let inner = match current.downcast_ref::<PackageErrorKind>() {
            Some(PackageErrorKind::Internal(inner)) => Some(inner),
            _ => current.downcast_ref::<Error>(),
        };
        if let Some(Error::ExportNotFound { dir, what }) = inner {
            return Error::ExportNotFound {
                dir: dir.clone(),
                what: what.clone(),
            };
        }
        cause = current.source();
    }
    error
}

/// The `file://` URL of `path`, which `FileIndexTransport` matches by string prefix.
fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

impl PackageManager {
    /// A manager that reads blobs from `DIR/blobs` and the configured lookup and never from `registry`'s
    /// network, writing nothing into the index; verification reads referrers from the same lookup.
    fn import_view(&self, input: &Path, registry: &str) -> Self {
        let lookup = Arc::new(self.blob_lookup().with_leading_dir(input.join("blobs")));
        let client = self
            .client()
            .cloned()
            .unwrap_or_else(|| ocx_oci::ClientBuilder::new().build())
            .with_blob_overlay(
                OverlayMode::LookupOnly,
                self.file_structure().blobs.root().to_path_buf(),
                Arc::clone(&lookup),
                registry,
                Vec::new(),
            );
        Self {
            // No source and Offline mode: a leaf the export lacks fails here, never over the network.
            index: ocx_index::Index::from_chained_with_content_store(
                ocx_index::LocalIndex::new(ocx_index::LocalConfig {
                    index_store: self.effective_index_store(),
                }),
                Vec::new(),
                ocx_index::ChainMode::Offline,
                self.file_structure().blobs.clone(),
            )
            .with_blob_lookup(Arc::clone(&lookup)),
            client: Some(client),
            auto_verify: self
                .auto_verify
                .clone()
                .map(|verify| verify.with_referrer_source(ReferrerSource::LookupOnly(Arc::clone(&lookup)))),
            blob_lookup: lookup,
            ..self.clone()
        }
    }

    async fn import_package(
        &self,
        export: &ExportDir,
        plan: PackagePlan,
        platforms: &[Platform],
    ) -> Result<ImportedPackage> {
        let registry = plan.identifier.registry().to_owned();
        let view = self.import_view(&export.dir, &registry);

        let home = match plan.digest {
            Some(_) => BTreeMap::new(),
            None => home_tags(&self.effective_index_store(), &registry, plan.identifier.repository()).await?,
        };
        let tag_plan = plan_tags(&plan.tags, &home, plan.named);

        // Dispatch digests whose platforms are assembled: every tag that is not a conflict.
        let mut dispatches: Vec<Digest> = tag_plan
            .added
            .iter()
            .chain(&tag_plan.replaced)
            .chain(&tag_plan.unchanged)
            .map(|planned| planned.digest.clone())
            .collect();
        dispatches.sort();
        dispatches.dedup();
        let mut leaves: Vec<(Digest, Platform)> = Vec::new();
        for dispatch in &dispatches {
            let bytes = export
                .index
                .read_dispatch_object(&registry, plan.identifier.repository(), dispatch)
                .await
                .map_err(|error| index_read_error(export, error))?
                .ok_or_else(|| {
                    export.invalid(format!(
                        "a root of '{}' names dispatch object {dispatch} the export lacks",
                        plan.identifier
                    ))
                })?;
            // The dispatch's raw bytes belong in the home's blob store too: a pull caches them there, and
            // a later exec reads them from it instead of the registry.
            write_blob_to_home(
                &export.dir,
                self.file_structure().blobs.root(),
                &registry,
                dispatch,
                bytes.clone(),
            )
            .await?;
            let Manifest::ImageIndex(index) = serde_json::from_slice::<Manifest>(&bytes)? else {
                return Err(export.invalid(format!(
                    "dispatch object {dispatch} of '{}' is not an image index",
                    plan.identifier
                )));
            };
            leaves.extend(select_leaves(export, &plan.identifier, &index, platforms)?);
        }
        if let Some(digest) = &plan.digest {
            let named = plan.identifier.clone_with_digest(digest.clone());
            let fetched = view
                .index()
                .fetch_manifest_raw_bytes_with_origin(&named)
                .await?
                .ok_or_else(|| export.not_found(format!("'{named}'")))?;
            match fetched.value.2 {
                Manifest::ImageIndex(index) => {
                    leaves.extend(select_leaves(export, &plan.identifier, &index, platforms)?)
                }
                Manifest::Image(_) => leaves.push((
                    digest.clone(),
                    match platforms {
                        [only] => only.clone(),
                        _ => Platform::any(),
                    },
                )),
            }
        }
        leaves.sort_by(|left, right| left.0.cmp(&right.0));
        leaves.dedup_by(|left, right| left.0 == right.0);

        let mut assembled = Vec::with_capacity(leaves.len());
        for (digest, platform) in leaves {
            let leaf = plan.identifier.clone_with_digest(digest);
            assembled.push(self.import_leaf(export, &view, &leaf, platform).await?);
        }
        for dispatch in &dispatches {
            self.import_referrers(export, &view, &registry, dispatch).await?;
        }

        let tags = if plan.digest.is_some() {
            None
        } else {
            let source = export_source(&export.dir, &registry)?;
            let local = LocalIndex::new(LocalConfig {
                index_store: self.effective_index_store(),
            });
            let kind = self.index().source_kind_for(&registry);
            Some(local.import_tags(&plan.identifier, &source, kind, &tag_plan).await?)
        };
        Ok(ImportedPackage {
            identifier: plan.digest.map_or(plan.identifier.clone(), |digest| {
                plan.identifier.clone_with_digest(digest)
            }),
            tags,
            platforms: assembled,
        })
    }
}

impl PackageManager {
    /// Pulls one digest-pinned leaf through `view` and carries its referrers into the home.
    async fn import_leaf(
        &self,
        export: &ExportDir,
        view: &Self,
        leaf: &PackageRef,
        platform: Platform,
    ) -> Result<ImportedPlatform> {
        let pinned = PinnedPackageRef::try_from(leaf.clone())
            .map_err(|_| export.invalid(format!("'{leaf}' cannot be pinned")))?;
        let digest = pinned.digest();
        let present = find_in_store(&self.file_structure().packages, &pinned)
            .await
            .map_err(|kind| Error::package(leaf.clone(), kind))?
            .is_some();
        view.pull(leaf, platform.clone()).await.map_err(|kind| match kind {
            PackageErrorKind::NotFound => export.not_found(format!("platform {platform} of '{leaf}'")),
            kind => peel_missing(Error::package(leaf.clone(), kind)),
        })?;
        self.import_referrers(export, view, leaf.registry(), &digest).await?;
        Ok(ImportedPlatform {
            platform,
            digest,
            status: if present {
                ImportPlatformStatus::Present
            } else {
                ImportPlatformStatus::Materialized
            },
        })
    }

    /// Copies the referrer manifests and blobs `view`'s lookup holds for `subject` into the home. A marker
    /// names a referrer only once auto-verify accepted it for `subject`, so an untrusted export cannot fill the
    /// home's candidate list.
    async fn import_referrers(&self, export: &ExportDir, view: &Self, registry: &str, subject: &Digest) -> Result<()> {
        let accepted = view
            .auto_verify()
            .map(|verify| verify.accepted_referrers(registry, subject))
            .unwrap_or_default();
        copy_referrers(
            view.blob_lookup(),
            &export.dir,
            self.file_structure().blobs.root(),
            registry,
            subject,
            &accepted,
            &SIGNATURE_ARTIFACT_TYPES,
        )
        .await
    }
}

/// Copies the referrers `lookup` lists for `subject` into `home`, marking only those in `accepted`.
///
/// Every referrer whose manifest names `subject` is copied, whatever its type. The cap counts only such
/// candidates, never raw marker names, and takes those typed in `preferred_types` first, so junk names or
/// junk-typed referrers cannot push a signature past the cap.
async fn copy_referrers(
    lookup: &BlobLookup,
    export_dir: &Path,
    home: &Path,
    registry: &str,
    subject: &Digest,
    accepted: &[Digest],
    preferred_types: &[&str],
) -> Result<()> {
    let listing = lookup
        .referrers(registry, subject)
        .await
        .map_err(|error| lookup_error(export_dir, error))?;
    let mut preferred = Vec::new();
    let mut others = Vec::new();
    for referrer in &listing.digests {
        // A candidate that is absent, corrupt or does not name `subject` is untrusted input: skipped, never fatal.
        let manifest = match lookup.read_metadata(registry, referrer, MAX_REFERRER_BYTES).await {
            Ok(Some(bytes)) => serde_json::from_slice::<Manifest>(&bytes).ok(),
            Ok(None) => None,
            Err(error) => {
                log::warn!("skipping referrer {referrer} of {subject}: {error}");
                None
            }
        };
        let Some(Manifest::Image(image)) = manifest else {
            continue;
        };
        if image.subject.as_ref().map(|named| named.digest.as_str()) != Some(subject.to_string().as_str()) {
            continue;
        }
        // The OCI rule: an absent `artifactType` is the config media type.
        let artifact_type = image
            .artifact_type
            .as_deref()
            .unwrap_or(image.config.media_type.as_str());
        if preferred_types.contains(&artifact_type) {
            preferred.push((referrer, image));
        } else {
            others.push((referrer, image));
        }
    }
    if preferred.len() + others.len() > MAX_REFERRERS_PER_SUBJECT {
        log::warn!("more than {MAX_REFERRERS_PER_SUBJECT} referrers of {subject} in the export; the rest are ignored");
    }
    for (referrer, image) in preferred.into_iter().chain(others).take(MAX_REFERRERS_PER_SUBJECT) {
        let mut blobs = Vec::with_capacity(image.layers.len() + 2);
        blobs.push(referrer.clone());
        for descriptor in std::iter::once(&image.config).chain(&image.layers) {
            match Digest::try_from(descriptor.digest.as_str()) {
                Ok(digest) => blobs.push(digest),
                Err(error) => log::warn!("referrer {referrer} lists an unreadable digest: {error}"),
            }
        }
        let mut complete = true;
        for blob in &blobs {
            if !copy_into_home(lookup, export_dir, home, registry, blob).await? {
                complete = false;
                break;
            }
        }
        // Only the referrer manifest itself being accepted earns a marker: a layer or config blob shared with an
        // accepted referrer proves nothing about this manifest.
        if complete && accepted.contains(referrer) {
            let (home, registry, subject, referrer) = (
                home.to_path_buf(),
                registry.to_owned(),
                subject.clone(),
                referrer.clone(),
            );
            tokio::task::spawn_blocking(move || markers::write_marker(&home, &registry, &subject, &referrer))
                .await
                .map_err(|join| join_error(&PathBuf::new(), join))?
                .map_err(|error| lookup_error(export_dir, error))?;
        }
    }
    Ok(())
}

/// Puts one verified referrer blob from `lookup` into the home; `false` when no source holds a good copy.
async fn copy_into_home(
    lookup: &BlobLookup,
    export_dir: &Path,
    home: &Path,
    registry: &str,
    digest: &Digest,
) -> Result<bool> {
    let bytes = match lookup.read_metadata(registry, digest, MAX_REFERRER_BYTES).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(false),
        Err(error) => {
            log::warn!("referrer blob {digest} not copied: {error}");
            return Ok(false);
        }
    };
    write_blob_to_home(export_dir, home, registry, digest, bytes).await?;
    Ok(true)
}

/// Writes `bytes` as `digest`'s blob in the home; the writer rejects bytes that do not hash to it.
async fn write_blob_to_home(
    export_dir: &Path,
    home: &Path,
    registry: &str,
    digest: &Digest,
    bytes: Vec<u8>,
) -> Result<()> {
    let (home, registry) = (home.to_path_buf(), registry.to_owned());
    let digest = digest.clone();
    tokio::task::spawn_blocking(move || {
        let mut writer = BlobWriter::root_for(&home, &registry, &digest)?;
        writer.write(&bytes).map_err(|source| LookupError::Io {
            path: home.clone(),
            source,
        })?;
        writer.finish().map(drop)
    })
    .await
    .map_err(|join| join_error(Path::new(""), join))?
    .map_err(|error| lookup_error(export_dir, error))
}

/// An export index read the store refused (over the cap, not a regular file) is an invalid export (65); every
/// other index failure keeps its own classification.
fn index_read_error(export: &ExportDir, error: ocx_index::error::Error) -> Error {
    match error {
        ocx_index::error::Error::File(ref file) if file.cause.kind() == std::io::ErrorKind::InvalidInput => {
            export.invalid(error.to_string())
        }
        other => other.into(),
    }
}

/// Refuses a symlink or any non-regular, non-directory entry under the export's `index/`, and a tree larger
/// than `MAX_INDEX_ENTRIES`: the export directory is untrusted, and a link would read outside it.
fn check_index_tree(dir: &Path) -> std::io::Result<Option<String>> {
    check_index_tree_capped(dir, MAX_INDEX_ENTRIES)
}

fn check_index_tree_capped(dir: &Path, cap: usize) -> std::io::Result<Option<String>> {
    // `read_dir` follows a link, so the root is checked on its own: a linked `index` would walk, and later read,
    // a tree outside the export.
    match std::fs::symlink_metadata(dir.join("index")) {
        Ok(metadata) if !metadata.is_dir() => return Ok(Some("its index is a link or not a directory".to_string())),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let mut pending = vec![dir.join("index")];
    let mut seen = 0usize;
    while let Some(current) = pending.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && current == dir.join("index") => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            seen += 1;
            if seen > cap {
                return Ok(Some(format!("its index lists more than {cap} entries")));
            }
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                // `locks` holds the writer's own lock files, never index content.
                if !(current == dir.join("index") && entry.file_name() == "locks") {
                    pending.push(path);
                }
            } else if !file_type.is_file() {
                return Ok(Some(format!(
                    "'{}' is a link or not a regular file",
                    path.strip_prefix(dir).unwrap_or(&path).display()
                )));
            }
        }
    }
    Ok(None)
}

/// Ceiling on entries in an export's index, which bounds the walk and the repository listing.
const MAX_INDEX_ENTRIES: usize = 1 << 16;

/// The export's index for `registry` as a source `import_tags` reads dispatch objects and the root from.
fn export_source(input: &Path, registry: &str) -> Result<Index> {
    let root = std::path::absolute(input.join("index").join(ocx_store::file_structure::slugify(registry)))
        .map_err(|error| Error::InternalFile(input.to_path_buf(), error))?;
    let url = file_url(&root);
    Ok(Index::from_source(OcxIndex::new(OcxIndexConfig {
        transport: Box::new(FileIndexTransport::new(url.clone(), root)),
        base_url: url,
        namespace: registry.to_owned(),
        client: ocx_oci::ClientBuilder::new().build(),
        allow_yanked: true,
        trusted_hosts: Vec::new(),
        insecure_hosts: Vec::new(),
        proxy_rules: ocx_oci::ssrf::proxy_rules(),
    })))
}

/// The leaf each requested platform selects from `index`.
fn select_leaves(
    export: &ExportDir,
    identifier: &PackageRef,
    index: &ocx_oci::ImageIndex,
    platforms: &[Platform],
) -> Result<Vec<(Digest, Platform)>> {
    let mut candidates = Vec::with_capacity(index.manifests.len());
    for entry in &index.manifests {
        let digest = Digest::try_from(entry.digest.as_str())
            .map_err(|error| export.invalid(format!("'{identifier}' lists an unreadable digest: {error}")))?;
        if let Ok(platform) = Platform::try_from(entry.platform.clone()) {
            candidates.push((digest, platform));
        }
    }
    let mut leaves = Vec::with_capacity(platforms.len());
    for platform in platforms {
        match ocx_oci::select_best(platform, &candidates) {
            Selection::Found(digest) => {
                let offered = candidates
                    .iter()
                    .find(|(candidate, _)| *candidate == digest)
                    .map_or_else(|| platform.clone(), |(_, offered)| offered.clone());
                leaves.push((digest, offered));
            }
            Selection::Ambiguous(_) => {
                return Err(export.not_found(format!("one platform matching {platform} of '{identifier}'")));
            }
            Selection::None => return Err(export.not_found(format!("platform {platform} of '{identifier}'"))),
        }
    }
    Ok(leaves)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ocx_exit::{ClassifyExitCode, ExitCode};
    use ocx_index::{IndexOperation, TagStatus};
    use ocx_oci::PackageRef;
    use ocx_oci::blob_lookup::{BlobLookup, blob_dir_path};
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
    use tempfile::TempDir;

    use super::*;
    use crate::Error;
    use crate::test_support::export_fixture::{
        Package, REGISTRY, REPO, linux_amd64, offline_manager, online_manager, tagged,
    };

    fn request(dir: &std::path::Path, packages: Vec<PackageRef>) -> ImportRequest {
        ImportRequest {
            input: dir.to_path_buf(),
            packages,
            platforms: vec![linux_amd64()],
        }
    }

    /// An offline home under `home` whose client records every request it is asked to make.
    fn dialling_home(home: &std::path::Path) -> (PackageManager, StubTransportData) {
        let data = StubTransportData::new();
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(data.clone())));
        let manager = PackageManager::new(
            offline_manager(home).file_structure().clone(),
            offline_manager(home).index().clone(),
            Some(client),
            REGISTRY,
        )
        .with_blob_lookup(Arc::new(BlobLookup::default()));
        (manager, data)
    }

    async fn pinned_in_home(manager: &PackageManager) -> Option<ocx_oci::Digest> {
        manager
            .index()
            .fetch_manifest_digest(&tagged(), IndexOperation::Query)
            .await
            .unwrap()
    }

    /// The exit code of the first classified cause: a shared pull error classifies `None` itself and leaves the
    /// verdict to the chain walk.
    fn chain_exit_code(error: &Error) -> Option<ExitCode> {
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
        while let Some(current) = cause {
            let code = match (
                current.downcast_ref::<crate::error::PackageErrorKind>(),
                current.downcast_ref::<Error>(),
            ) {
                (Some(crate::error::PackageErrorKind::Internal(inner)), _) => inner.classify(),
                (Some(kind), _) => kind.classify(),
                (None, Some(inner)) => inner.classify(),
                (None, None) => None,
            };
            if code.is_some() {
                return code;
            }
            cause = current.source();
        }
        None
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_marker_is_invalid() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();

        let error = offline_manager(home.path())
            .import(request(export.path(), Vec::new()))
            .await
            .expect_err("a directory without ocx-export.json is not an export");

        assert!(matches!(error, Error::ExportDirInvalid { .. }), "{error}");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_mismatch_invalid() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        package.write_export_dir(export.path(), false).await;
        let root = export
            .path()
            .join("index")
            .join(REGISTRY)
            .join("p")
            .join(format!("{REPO}.json"));
        let mut bytes = std::fs::read(&root).unwrap();
        bytes.push(b'\n');
        std::fs::write(&root, bytes).unwrap();

        let error = offline_manager(home.path())
            .import(request(export.path(), Vec::new()))
            .await
            .expect_err("the root no longer hashes to its catalog entry");

        assert!(matches!(error, Error::ExportDirInvalid { .. }), "{error}");
        assert!(pinned_in_home(&offline_manager(home.path())).await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_platform_named() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        Package::layerless().write_export_dir(export.path(), false).await;
        let mut arm = request(export.path(), Vec::new());
        arm.platforms = vec!["linux/arm64".parse().unwrap()];

        let error = offline_manager(home.path())
            .import(arm)
            .await
            .expect_err("the export holds linux/amd64 only");

        assert!(matches!(error, Error::ExportNotFound { .. }), "{error}");
        assert_eq!(error.classify(), Some(ExitCode::NotFound));
        assert!(
            error.to_string().contains("linux/arm64"),
            "the message names the platform: {error}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn never_dials() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        Package::layerless().write_export_dir(export.path(), false).await;
        let (manager, dialled) = dialling_home(home.path());

        let outcome = manager.import(request(export.path(), Vec::new())).await.unwrap();

        assert_eq!(outcome.items.len(), 1);
        assert!(
            dialled.read().calls.is_empty(),
            "import made requests: {:?}",
            dialled.read().calls
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn import_never_resolves_export_repository_host() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        Package::layerless().write_export_dir(export.path(), false).await;
        let (manager, dialled) = dialling_home(home.path());

        manager.import(request(export.path(), Vec::new())).await.unwrap();

        let routed = manager.index().route(&tagged()).await.unwrap();
        assert_eq!(
            routed.registry(),
            REGISTRY,
            "the pin routes to this registry, never the export root's pointer (ghcr.io)"
        );
        assert!(dialled.read().calls.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn content_before_pins() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        // The layer archive is listed by the manifest and absent from `blobs/`.
        Package::new().write_export_dir(export.path(), false).await;

        let error = offline_manager(home.path())
            .import(request(export.path(), Vec::new()))
            .await
            .expect_err("a blob no source holds");

        assert!(matches!(error, Error::ExportNotFound { .. }), "{error}");
        assert!(
            pinned_in_home(&offline_manager(home.path())).await.is_none(),
            "no pin is written before the content is in the home"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_package_commits_nothing() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        package.write_export_dir(export.path(), false).await;
        let config = ocx_oci::blob_lookup::blob_data_path(&export.path().join("blobs"), REGISTRY, &package.config);
        std::fs::write(&config, b"corrupt").unwrap();

        let error = offline_manager(home.path())
            .import(request(export.path(), Vec::new()))
            .await
            .expect_err("a corrupt blob directory copy with no clean copy anywhere");

        assert_eq!(chain_exit_code(&error), Some(ExitCode::DataError), "{error}");
        assert!(pinned_in_home(&offline_manager(home.path())).await.is_none());
    }

    /// A home pinning `TAG` to the layered package while the export carries the layerless one.
    async fn home_pinned_elsewhere(home: &std::path::Path) -> Package {
        let other = Package::new();
        let manager = online_manager(home, other.source());
        manager
            .index()
            .fetch_manifest(&tagged(), IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("the source serves the tag");
        other
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conflict_materializes_nothing_exits_zero() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let other = home_pinned_elsewhere(home.path()).await;
        Package::layerless().write_export_dir(export.path(), false).await;

        let outcome = offline_manager(home.path())
            .import(request(export.path(), Vec::new()))
            .await
            .expect("a conflict is a report row, not an error");

        let tags = outcome.items[0].tags.as_ref().expect("tag rows");
        assert_eq!(tags[0].status, TagStatus::Conflict);
        assert_eq!(tags[0].home_digest, Some(other.dispatch.clone()));
        assert!(
            outcome.items[0].platforms.is_empty(),
            "a conflict tag materializes nothing"
        );
        assert_eq!(
            pinned_in_home(&offline_manager(home.path())).await,
            Some(other.dispatch)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn named_package_replaces() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let other = home_pinned_elsewhere(home.path()).await;
        let exported = Package::layerless();
        exported.write_export_dir(export.path(), false).await;
        let named = PackageRef::new_registry(REPO, REGISTRY);

        let outcome = offline_manager(home.path())
            .import(request(export.path(), vec![named]))
            .await
            .unwrap();

        let tags = outcome.items[0].tags.as_ref().expect("tag rows");
        assert_eq!(tags[0].status, TagStatus::Replaced);
        assert_eq!(tags[0].home_digest, Some(other.dispatch));
        assert_eq!(
            pinned_in_home(&offline_manager(home.path())).await,
            Some(exported.dispatch)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_verify_writes_no_markers() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        package.write_export_dir(export.path(), false).await;
        // A marker whose referrer manifest the export does not carry: nothing proves it names the subject.
        let planted = blob_dir_path(&export.path().join("blobs"), REGISTRY, &package.leaf).join("referrers");
        std::fs::create_dir_all(&planted).unwrap();
        std::fs::write(planted.join(format!("sha256-{}", "e".repeat(64))), b"").unwrap();
        let manager = offline_manager(home.path());

        manager.import(request(export.path(), Vec::new())).await.unwrap();

        let home_blobs = manager.file_structure().blobs.root().to_path_buf();
        assert!(
            !blob_dir_path(&home_blobs, REGISTRY, &package.leaf)
                .join("referrers")
                .is_dir(),
            "a marker auto-verify did not accept never reaches the home"
        );
    }

    /// A signature-shaped referrer of `package`'s leaf, its manifest and empty config written into `blobs`.
    async fn write_referrer(blobs: &std::path::Path, package: &Package) -> (Digest, Digest) {
        write_typed_referrer(
            blobs,
            package,
            ocx_oci::referrer::media_types::COSIGN_SIG_ARTIFACT_TYPE,
            0,
        )
        .await
    }

    /// A referrer of `package`'s leaf declaring `artifact_type`; `nonce` varies the digest.
    async fn write_typed_referrer(
        blobs: &std::path::Path,
        package: &Package,
        artifact_type: &str,
        nonce: usize,
    ) -> (Digest, Digest) {
        let store = ocx_store::file_structure::BlobStore::new(blobs.to_path_buf());
        let config_bytes = b"{}".to_vec();
        let config = ocx_oci::Algorithm::Sha256.hash(&config_bytes);
        let manifest = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","artifactType":"{artifact_type}","config":{{"mediaType":"application/vnd.oci.empty.v1+json","digest":"{config}","size":2}},"layers":[],"annotations":{{"nonce":"{nonce}"}},"subject":{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{}","size":{}}}}}"#,
            package.leaf,
            package.leaf_bytes.len()
        )
        .into_bytes();
        let referrer = ocx_oci::Algorithm::Sha256.hash(&manifest);
        store.write_blob(REGISTRY, &config, &config_bytes).await.unwrap();
        store.write_blob(REGISTRY, &referrer, &manifest).await.unwrap();
        (referrer, config)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn junk_typed_referrers_past_the_cap_do_not_hide_a_signature() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        let blobs = export.path().join("blobs");
        let mut junk = Vec::new();
        for index in 0..MAX_REFERRERS_PER_SUBJECT + 44 {
            let (digest, _) = write_typed_referrer(&blobs, &package, "application/x-junk", index).await;
            markers::write_marker(&blobs, REGISTRY, &package.leaf, &digest).unwrap();
            junk.push(digest);
        }
        // Listed after every junk name, so a cap that counted them would stop first.
        let mut nonce = 0;
        let referrer = loop {
            let (digest, _) = write_typed_referrer(
                &blobs,
                &package,
                ocx_oci::referrer::media_types::COSIGN_SIG_ARTIFACT_TYPE,
                nonce,
            )
            .await;
            if junk.iter().all(|other| other.to_string() < digest.to_string()) {
                break digest;
            }
            nonce += 1;
        };
        markers::write_marker(&blobs, REGISTRY, &package.leaf, &referrer).unwrap();
        let lookup = BlobLookup::new(vec![blobs], None);

        copy_referrers(
            &lookup,
            export.path(),
            home.path(),
            REGISTRY,
            &package.leaf,
            std::slice::from_ref(&referrer),
            &SIGNATURE_ARTIFACT_TYPES,
        )
        .await
        .unwrap();

        let listed = markers::read_markers(home.path(), REGISTRY, &package.leaf, 4096).unwrap();
        assert_eq!(
            listed.digests,
            vec![referrer],
            "the cap counts accepted-type candidates only"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_carried_referrer_of_any_type_reaches_the_home_blobs() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        let blobs = export.path().join("blobs");
        let (referrer, config) = write_typed_referrer(&blobs, &package, "application/vnd.example.sbom+json", 0).await;
        markers::write_marker(&blobs, REGISTRY, &package.leaf, &referrer).unwrap();
        let lookup = BlobLookup::new(vec![blobs], None);

        copy_referrers(
            &lookup,
            export.path(),
            home.path(),
            REGISTRY,
            &package.leaf,
            &[],
            &SIGNATURE_ARTIFACT_TYPES,
        )
        .await
        .unwrap();

        for digest in [&referrer, &config] {
            assert!(
                blob_dir_path(home.path(), REGISTRY, digest).join("data").is_file(),
                "a non-signature referrer and its blobs land in the home: {digest}"
            );
        }
        let listed = markers::read_markers(home.path(), REGISTRY, &package.leaf, 4096).unwrap();
        assert!(listed.digests.is_empty(), "an unaccepted referrer earns no marker");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn junk_markers_past_the_cap_do_not_hide_a_real_referrer() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        let blobs = export.path().join("blobs");
        let (referrer, _) = write_referrer(&blobs, &package).await;
        // Sorted ahead of the real referrer, and more of them than the cap allows.
        for index in 0..MAX_REFERRERS_PER_SUBJECT + 44 {
            let junk = Digest::try_from(format!("sha256:{index:064x}")).unwrap();
            markers::write_marker(&blobs, REGISTRY, &package.leaf, &junk).unwrap();
        }
        markers::write_marker(&blobs, REGISTRY, &package.leaf, &referrer).unwrap();
        let lookup = BlobLookup::new(vec![blobs], None);

        copy_referrers(
            &lookup,
            export.path(),
            home.path(),
            REGISTRY,
            &package.leaf,
            std::slice::from_ref(&referrer),
            &SIGNATURE_ARTIFACT_TYPES,
        )
        .await
        .unwrap();

        let listed = markers::read_markers(home.path(), REGISTRY, &package.leaf, 4096).unwrap();
        assert_eq!(
            listed.digests,
            vec![referrer],
            "the cap counts filtered candidates, not raw names"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn marker_needs_the_referrer_manifest_itself_accepted() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        let blobs = export.path().join("blobs");
        let (referrer, config) = write_referrer(&blobs, &package).await;
        markers::write_marker(&blobs, REGISTRY, &package.leaf, &referrer).unwrap();
        let lookup = BlobLookup::new(vec![blobs], None);

        // Auto-verify accepted a blob this manifest merely lists, not the manifest.
        copy_referrers(
            &lookup,
            export.path(),
            home.path(),
            REGISTRY,
            &package.leaf,
            &[config],
            &SIGNATURE_ARTIFACT_TYPES,
        )
        .await
        .unwrap();

        let listed = markers::read_markers(home.path(), REGISTRY, &package.leaf, 4096).unwrap();
        assert!(listed.digests.is_empty(), "no marker for a referrer nothing accepted");
        assert!(
            blob_dir_path(home.path(), REGISTRY, &referrer).join("data").is_file(),
            "its blobs still land in the home"
        );

        copy_referrers(
            &lookup,
            export.path(),
            home.path(),
            REGISTRY,
            &package.leaf,
            std::slice::from_ref(&referrer),
            &SIGNATURE_ARTIFACT_TYPES,
        )
        .await
        .unwrap();

        let listed = markers::read_markers(home.path(), REGISTRY, &package.leaf, 4096).unwrap();
        assert_eq!(listed.digests, vec![referrer]);
    }

    #[test]
    fn shard_collision_is_an_invalid_export() {
        let error = lookup_error(
            Path::new("the-export"),
            LookupError::ShardCollision {
                path: PathBuf::from("blobs/example.com"),
                existing: "sha256:a".into(),
                digest: "sha256:b".into(),
            },
        );

        match &error {
            Error::ExportDirInvalid { dir, reason } => {
                assert_eq!(dir, Path::new("the-export"), "dir is the export root");
                assert!(reason.contains("blobs/example.com"), "{reason}");
            }
            other => panic!("{other}"),
        }
        assert_eq!(error.classify(), Some(ExitCode::DataError));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn symlink_in_the_index_is_invalid() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        package.write_export_dir(export.path(), false).await;
        // A file the import never opens: only the walk can see this link, no reader would trip on it first.
        let outside = export.path().join("outside.txt");
        std::fs::write(&outside, b"x").unwrap();
        let stray = export.path().join("index").join("stray");
        std::os::unix::fs::symlink(&outside, &stray).unwrap();

        let error = offline_manager(home.path())
            .import(request(export.path(), Vec::new()))
            .await
            .expect_err("a linked index entry is refused");

        assert!(matches!(error, Error::ExportDirInvalid { .. }), "{error}");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
    }

    #[cfg(unix)]
    #[test]
    fn linked_index_root_is_refused() {
        let export = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), export.path().join("index")).unwrap();

        let reason = check_index_tree(export.path())
            .unwrap()
            .expect("a linked root is refused");
        assert!(reason.contains("link"), "{reason}");
    }

    #[test]
    fn index_tree_past_the_entry_cap_is_refused() {
        let export = TempDir::new().unwrap();
        let index = export.path().join("index");
        std::fs::create_dir_all(index.join("reg")).unwrap();
        for name in ["a", "b", "c"] {
            std::fs::write(index.join("reg").join(name), b"").unwrap();
        }
        assert!(check_index_tree_capped(export.path(), 4).unwrap().is_none());
        let reason = check_index_tree_capped(export.path(), 3)
            .unwrap()
            .expect("four entries exceed three");
        assert!(reason.contains("more than 3 entries"), "{reason}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn oversized_index_document_is_invalid() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        package.write_export_dir(export.path(), false).await;
        // An unbounded read would pull the file in and fail its digest check instead: a different error.
        let object =
            IndexStore::new(export.path().join("index")).dispatch_object_path(REGISTRY, REPO, &package.dispatch);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&object)
            .unwrap()
            .set_len(ocx_oci::client::MAX_INDEX_DOCUMENT_BYTES as u64 + 1)
            .unwrap();

        let error = offline_manager(home.path())
            .import(request(export.path(), Vec::new()))
            .await
            .expect_err("a dispatch object over the index-document cap is refused unread");

        assert!(matches!(error, Error::ExportDirInvalid { .. }), "{error}");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn index_entry_linked_after_the_walk_is_invalid() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        package.write_export_dir(export.path(), false).await;
        let opened = ExportDir::open(export.path()).await.unwrap();
        // Same bytes behind the link, so only the link refusal can reject the read.
        let object =
            IndexStore::new(export.path().join("index")).dispatch_object_path(REGISTRY, REPO, &package.dispatch);
        let outside = export.path().join("outside.json");
        std::fs::copy(&object, &outside).unwrap();
        std::fs::remove_file(&object).unwrap();
        std::os::unix::fs::symlink(&outside, &object).unwrap();

        let plan = opened.plan(&[]).unwrap().remove(0);
        let error = offline_manager(home.path())
            .import_package(&opened, plan, &[linux_amd64()])
            .await
            .expect_err("a dispatch object swapped for a link after the walk is refused");

        assert!(matches!(error, Error::ExportDirInvalid { .. }), "{error}");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
    }

    /// A manager whose patch snapshot pins one descriptor whose single layer is in `export`'s blobs; returns the
    /// layer's data file.
    async fn snapshot_descriptor_in_export(
        home: &std::path::Path,
        export: &std::path::Path,
    ) -> (PackageManager, PathBuf) {
        Package::layerless().write_export_dir(export, false).await;
        let blobs = export.join("blobs");
        let store = ocx_store::file_structure::BlobStore::new(blobs.clone());
        let layer_bytes = br#"{"version":1,"rules":[]}"#.to_vec();
        let layer = ocx_oci::Algorithm::Sha256.hash(&layer_bytes);
        let config_bytes = b"{}".to_vec();
        let config = ocx_oci::Algorithm::Sha256.hash(&config_bytes);
        let manifest = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.empty.v1+json","digest":"{config}","size":2}},"layers":[{{"mediaType":"application/json","digest":"{layer}","size":{}}}]}}"#,
            layer_bytes.len()
        )
        .into_bytes();
        let descriptor = ocx_oci::Algorithm::Sha256.hash(&manifest);
        store.write_blob(REGISTRY, &layer, &layer_bytes).await.unwrap();
        store.write_blob(REGISTRY, &descriptor, &manifest).await.unwrap();
        let snapshot = crate::patch::snapshot::PatchSnapshot {
            version: crate::patch::snapshot::SnapshotVersion::CURRENT,
            companions: BTreeMap::new(),
            descriptors: BTreeMap::from([(format!("{REGISTRY}/patches"), descriptor)]),
        };
        let manager = offline_manager(home).with_patch_snapshot(Some(snapshot));
        (manager, ocx_oci::blob_lookup::blob_data_path(&blobs, REGISTRY, &layer))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn damaged_patch_descriptor_layer_is_a_data_error() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let (manager, layer) = snapshot_descriptor_in_export(home.path(), export.path()).await;
        std::fs::write(&layer, b"damaged").unwrap();
        let opened = ExportDir::open(export.path()).await.unwrap();

        let error = manager
            .import_snapshot(&opened, &ImportOutcome::default())
            .await
            .expect_err("a layer that fails its digest check");

        assert!(!matches!(error, Error::ExportNotFound { .. }), "{error}");
        assert_eq!(error.classify(), Some(ExitCode::DataError), "{error}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn absent_patch_descriptor_layer_is_not_found() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let (manager, layer) = snapshot_descriptor_in_export(home.path(), export.path()).await;
        std::fs::remove_file(&layer).unwrap();
        let opened = ExportDir::open(export.path()).await.unwrap();

        let error = manager
            .import_snapshot(&opened, &ImportOutcome::default())
            .await
            .expect_err("a layer no source holds");

        assert!(matches!(error, Error::ExportNotFound { .. }), "{error}");
        assert_eq!(error.classify(), Some(ExitCode::NotFound));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn damaged_companion_leaf_layer_is_a_data_error() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::new();
        package.write_export_dir(export.path(), true).await;
        let layer = package.layer.clone().expect("the package carries a layer");
        // A digest-pinned companion read resolves its dispatch from the blobs, not the index objects.
        ocx_store::file_structure::BlobStore::new(export.path().join("blobs"))
            .write_blob(REGISTRY, &package.dispatch, &package.dispatch_bytes)
            .await
            .unwrap();
        std::fs::write(
            ocx_oci::blob_lookup::blob_data_path(&export.path().join("blobs"), REGISTRY, &layer),
            b"damaged",
        )
        .unwrap();
        let snapshot = crate::patch::PatchSnapshot {
            version: crate::patch::SnapshotVersion::CURRENT,
            companions: [(
                crate::patch::snapshot::companion_key(&tagged()),
                package.dispatch.clone(),
            )]
            .into(),
            descriptors: BTreeMap::new(),
        };
        let manager = offline_manager(home.path()).with_patch_snapshot(Some(snapshot));
        let opened = ExportDir::open(export.path()).await.unwrap();
        // The companion's platform comes from what the import just assembled.
        let imported = ImportOutcome {
            items: vec![ImportedPackage {
                identifier: tagged(),
                tags: None,
                platforms: vec![ImportedPlatform {
                    platform: linux_amd64(),
                    digest: package.leaf.clone(),
                    status: ImportPlatformStatus::Present,
                }],
            }],
        };

        let error = manager
            .import_snapshot(&opened, &imported)
            .await
            .expect_err("a companion layer that fails its digest check");

        assert!(!matches!(error, Error::ExportNotFound { .. }), "{error}");
        // The pull wraps the mismatch in unclassified setup failures; the innermost link carries the verdict.
        let innermost = std::iter::successors(Some(&error as &dyn std::error::Error), |cause| cause.source())
            .last()
            .and_then(|cause| cause.downcast_ref::<crate::error::PackageErrorKind>())
            .expect("the chain ends in a package failure");
        let crate::error::PackageErrorKind::Internal(inner) = innermost else {
            panic!("the chain ends in {innermost:?}");
        };
        assert!(format!("{inner:?}").contains("DigestMismatch"), "{inner:?}");
        assert_eq!(inner.classify(), Some(ExitCode::DataError), "{inner:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn damaged_referrer_blob_is_skipped() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        let blobs = export.path().join("blobs");
        let (referrer, config) = write_referrer(&blobs, &package).await;
        markers::write_marker(&blobs, REGISTRY, &package.leaf, &referrer).unwrap();
        std::fs::write(
            ocx_oci::blob_lookup::blob_data_path(&blobs, REGISTRY, &config),
            b"damaged",
        )
        .unwrap();
        let lookup = BlobLookup::new(vec![blobs], None);

        copy_referrers(
            &lookup,
            export.path(),
            home.path(),
            REGISTRY,
            &package.leaf,
            std::slice::from_ref(&referrer),
            &SIGNATURE_ARTIFACT_TYPES,
        )
        .await
        .expect("a damaged referrer blob is skipped, never fatal");

        let listed = markers::read_markers(home.path(), REGISTRY, &package.leaf, 4096).unwrap();
        assert!(listed.digests.is_empty(), "an incomplete referrer earns no marker");
        assert!(!blob_dir_path(home.path(), REGISTRY, &config).join("data").is_file());
    }

    fn project_with_lock(dir: &std::path::Path) -> std::path::PathBuf {
        let project = dir.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("ocx.toml"), "").unwrap();
        // The ledger lists a project only while its lock exists.
        std::fs::write(project.join("ocx.lock"), "").unwrap();
        project
    }

    fn project_request(
        export: &std::path::Path,
        package: &Package,
        project: Option<std::path::PathBuf>,
    ) -> ProjectImportRequest {
        ProjectImportRequest {
            input: export.to_path_buf(),
            lock_pins: vec![LockPin {
                identifier: package.leaf_id(),
                platform: linux_amd64(),
            }],
            platforms: vec![linux_amd64()],
            project_dir: project,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn root_import_registers_project() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        package.write_export_dir(export.path(), false).await;
        let project = project_with_lock(home.path());

        offline_manager(home.path())
            .import_project(project_request(export.path(), &package, Some(project.clone())))
            .await
            .unwrap();

        let live = ocx_project::registry::ProjectRegistry::new(home.path())
            .live_projects()
            .await
            .unwrap();
        let project = dunce::canonicalize(&project).unwrap();
        assert!(live.contains(&project), "the project is registered: {live:?}");
        assert!(
            pinned_in_home(&offline_manager(home.path())).await.is_none(),
            "a root import writes no pin"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn global_root_import_registers_nothing() {
        let home = TempDir::new().unwrap();
        let export = TempDir::new().unwrap();
        let package = Package::layerless();
        package.write_export_dir(export.path(), false).await;

        offline_manager(home.path())
            .import_project(project_request(export.path(), &package, None))
            .await
            .unwrap();

        let live = ocx_project::registry::ProjectRegistry::new(home.path())
            .live_projects()
            .await
            .unwrap();
        assert!(live.is_empty(), "--global registers no project: {live:?}");
    }
}
