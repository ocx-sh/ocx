// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod composer;
pub mod concurrency;
pub mod error;
pub mod launcher;

pub mod tasks;

#[cfg(test)]
mod resolve_env_package_root_tests {
    use std::path::Path;
    use tempfile::tempdir;

    use crate::{
        file_structure::FileStructure,
        file_structure::{BlobStore, TagStore},
        oci::index::{ChainMode, Index, LocalConfig, LocalIndex},
    };

    /// Helper: construct a minimal offline PackageManager for unit testing.
    fn make_test_manager(ocx_home: &Path) -> super::PackageManager {
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            tag_store: TagStore::new(ocx_home.join("tags")),
            blob_store: BlobStore::new(ocx_home.join("blobs")),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        super::PackageManager::new(
            fs,
            index,
            None, // offline — no OCI client
            "localhost:5000",
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_env_from_missing_package_root_errors() {
        let tmp = tempdir().unwrap();
        let manager = make_test_manager(tmp.path());
        // Non-existent package root — PackageStore cannot read metadata.json.
        let result = manager
            .resolve_env_from_package_root(
                Path::new("/nonexistent/pkg"),
                false,
                super::PatchScope::NoProjectContext,
            )
            .await;
        assert!(result.is_err(), "missing package root must return Err");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_env_from_package_root_with_missing_metadata_errors() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        // Package root exists, but no metadata.json — must error.
        tokio::fs::create_dir_all(&pkg_root).await.unwrap();
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let manager = make_test_manager(tmp.path());
        let result = manager
            .resolve_env_from_package_root(&pkg_root, false, super::PatchScope::NoProjectContext)
            .await;
        assert!(result.is_err(), "missing metadata.json must return Err");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_env_from_package_root_with_missing_resolve_json_errors() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        // Write metadata.json but no resolve.json.
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{"key": "PATH", "type": "path", "required": true, "value": "${installPath}/bin"}]
        });
        tokio::fs::write(pkg_root.join("metadata.json"), meta.to_string().as_bytes())
            .await
            .unwrap();
        let manager = make_test_manager(tmp.path());
        let result = manager
            .resolve_env_from_package_root(&pkg_root, false, super::PatchScope::NoProjectContext)
            .await;
        assert!(result.is_err(), "missing resolve.json must return Err");
    }
}

#[cfg(test)]
mod install_info_identifier_tests {
    use std::path::Path;
    use tempfile::tempdir;

    use crate::{
        file_structure::{BlobStore, FileStructure, TagStore},
        oci::index::{ChainMode, Index, LocalConfig, LocalIndex},
    };

    fn make_test_manager(ocx_home: &Path) -> super::PackageManager {
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            tag_store: TagStore::new(ocx_home.join("tags")),
            blob_store: BlobStore::new(ocx_home.join("blobs")),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        super::PackageManager::new(fs, index, None, "localhost:5000")
    }

    /// Write the minimal set of files that make `install_info_from_package_root`
    /// succeed for a package rooted at `pkg_root` with the given SHA-256 hex.
    async fn write_minimal_package_root(pkg_root: &std::path::Path, hex: &str) {
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        // metadata.json — bare Bundle with no env vars (passes ValidMetadata).
        let meta = serde_json::json!({"type": "bundle", "version": 1, "env": []});
        tokio::fs::write(pkg_root.join("metadata.json"), meta.to_string().as_bytes())
            .await
            .unwrap();
        // resolve.json — leaf package with no dependencies.
        let resolve = serde_json::json!({"dependencies": []});
        tokio::fs::write(pkg_root.join("resolve.json"), resolve.to_string().as_bytes())
            .await
            .unwrap();
        // digest file written in the same format as `write_digest_file`.
        tokio::fs::write(pkg_root.join("digest"), format!("sha256:{hex}").as_bytes())
            .await
            .unwrap();
    }

    /// Two distinct package roots (as passed to `install_info_from_package_root`)
    /// must produce distinct repository components so they do not collapse onto
    /// the same key in the `seen_repos` dedup map inside `resolve_env`.
    #[tokio::test(flavor = "multi_thread")]
    async fn distinct_package_roots_yield_distinct_identifiers() {
        let tmp = tempdir().unwrap();
        let manager = make_test_manager(tmp.path());

        let root_a = tmp.path().join("pkg_a");
        let root_b = tmp.path().join("pkg_b");
        // Two distinct 64-char SHA-256 hex digests.
        let hex_a = "a".repeat(64);
        let hex_b = "b".repeat(64);

        write_minimal_package_root(&root_a, &hex_a).await;
        write_minimal_package_root(&root_b, &hex_b).await;

        let info_a = manager
            .install_info_from_package_root(&root_a)
            .await
            .expect("package root A must succeed");
        let info_b = manager
            .install_info_from_package_root(&root_b)
            .await
            .expect("package root B must succeed");

        assert_ne!(
            info_a.identifier().repository(),
            info_b.identifier().repository(),
            "distinct package roots must have distinct synthetic repository components"
        );
        assert!(
            info_a.identifier().repository().starts_with("file-url-mode/"),
            "synthetic repository must start with 'file-url-mode/'"
        );
        assert!(
            info_b.identifier().repository().starts_with("file-url-mode/"),
            "synthetic repository must start with 'file-url-mode/'"
        );
    }

    /// Two distinct content digests that share the first 16 hex characters
    /// must still yield DISTINCT synthetic repository components.
    ///
    /// Regression guard: an earlier shape truncated to `digest.hex()[..16]`,
    /// which collapsed any pair of package roots whose digests collided in
    /// their first 64 bits onto the same `(registry, repository)` dedup key
    /// inside `resolve_env`. One side then surfaced as a "conflicting digest"
    /// warning and was silently dropped. Using the full digest hex makes such
    /// a collision probabilistically impossible (full SHA-256 keyspace).
    #[tokio::test(flavor = "multi_thread")]
    async fn pkg_roots_with_same_16char_digest_prefix_do_not_collapse() {
        let tmp = tempdir().unwrap();
        let manager = make_test_manager(tmp.path());

        let root_a = tmp.path().join("pkg_a");
        let root_b = tmp.path().join("pkg_b");
        // Two 64-char SHA-256 hex digests sharing the first 16 hex chars
        // ("aaaaaaaaaaaaaaaa") but diverging from char 17 onwards.
        let prefix = "a".repeat(16);
        let hex_a = format!("{prefix}{}", "0".repeat(48));
        let hex_b = format!("{prefix}{}", "f".repeat(48));
        assert_eq!(&hex_a[..16], &hex_b[..16], "test fixture must share 16-char prefix");
        assert_ne!(hex_a, hex_b, "test fixture must diverge after the prefix");

        write_minimal_package_root(&root_a, &hex_a).await;
        write_minimal_package_root(&root_b, &hex_b).await;

        let info_a = manager
            .install_info_from_package_root(&root_a)
            .await
            .expect("package root A must succeed");
        let info_b = manager
            .install_info_from_package_root(&root_b)
            .await
            .expect("package root B must succeed");

        assert_ne!(
            info_a.identifier().repository(),
            info_b.identifier().repository(),
            "digests sharing the first 16 hex chars must still yield distinct synthetic repositories"
        );
        // Sanity: synthetic repo string must include the full 64-char digest hex,
        // not the truncated 16-char prefix that caused the original collision.
        assert!(
            info_a.identifier().repository().ends_with(&hex_a),
            "synthetic repo for A must embed the full 64-char digest hex"
        );
        assert!(
            info_b.identifier().repository().ends_with(&hex_b),
            "synthetic repo for B must embed the full 64-char digest hex"
        );
    }

    /// Calling `install_info_from_package_root` twice on the same root must
    /// yield the same repository component (idempotency).
    #[tokio::test(flavor = "multi_thread")]
    async fn same_package_root_yields_stable_identifier() {
        let tmp = tempdir().unwrap();
        let manager = make_test_manager(tmp.path());

        let root = tmp.path().join("pkg");
        write_minimal_package_root(&root, &"c".repeat(64)).await;

        let info_first = manager
            .install_info_from_package_root(&root)
            .await
            .expect("first call must succeed");
        let info_second = manager
            .install_info_from_package_root(&root)
            .await
            .expect("second call must succeed");

        assert_eq!(
            info_first.identifier().repository(),
            info_second.identifier().repository(),
            "repeated calls on the same root must produce a stable identifier"
        );
    }
}

// Re-export types needed by other modules and CLI commands.
pub use concurrency::Concurrency;
pub use error::DependencyError;
pub use tasks::auto_verify::{AutoVerify, AutoVerifyInput};
pub use tasks::clean::{CleanResult, CleanedObject};
pub use tasks::common::WireSelectionOutcome;
pub use tasks::hook::{AppliedSet, collect_applied};
pub use tasks::inspect::InspectResult;
pub use tasks::managed_config::{ManagedConfigRefreshOutcome, ManagedConfigUpdateResult};
pub use tasks::patch_publish::PatchPublishReport;
pub use tasks::patch_sync::PatchSyncReport;
pub use tasks::resolve::{ChainBlob, ChainRole, PatchProvenance, PatchScope, ResolvedChain, SitePatchRoots};
pub use tasks::update_check::{SelfUpdateResult, SkippedReason, TagProbe, UpdateCheckResult};

use crate::{config::patch::ResolvedPatchConfig, file_structure, oci, patch::PatchSnapshot};

/// Central facade for package operations (find, install, uninstall, etc.).
///
/// `PackageManager` holds all the context that tasks need — file structure,
/// index, OCI client — and is cheap to [`Clone`].
///
/// Environment variable resolution uses persisted `resolve.json` files
/// written at install time — see [`resolve_env`](Self::resolve_env).
///
/// Progress is rendered through a span-free [`ProgressManager`]
/// (`crate::cli::progress`); task code creates RAII bar/spinner guards
/// from it. See ADR adr_progress_architecture for why progress no longer
/// rides the `tracing` span tree.
///
/// The optional [`ResolvedPatchConfig`] enables the patch-discovery hook
/// after a user-requested base install. When `None`, the patch tier is
/// disabled and `discover_and_install_patches` is a no-op.
#[derive(Clone)]
pub struct PackageManager {
    file_structure: file_structure::FileStructure,
    index: oci::index::Index,
    client: Option<oci::Client>,
    default_registry: String,
    progress: crate::cli::progress::ProgressManager,
    /// Site-tier patch registry configuration.
    ///
    /// `None` = no patch tier configured (the `[patches]` section is absent
    /// from every loaded config tier). When `Some`, `discover_and_install_patches`
    /// runs after every user-requested base install (online only).
    patches: Option<ResolvedPatchConfig>,
    /// Frozen companion pinning snapshot for opt-in determinism.
    ///
    /// When `Some`, the compose overlay prefers the snapshot's pinned
    /// companion digests over live tag lookups.  `None` = live lookups only.
    /// Set via `OCX_PATCH_SNAPSHOT` / `ocx patch freeze`.
    patch_snapshot: Option<PatchSnapshot>,
    /// Dedicated OCI client for fetching the managed-config artifact itself.
    ///
    /// Built from the **local-only** mirror view (system/user/home/
    /// `OCX_CONFIG`/`--config`/`OCX_MIRRORS` — the managed payload's OWN
    /// `[mirrors]` excluded), so the tier can never redirect or hijack the
    /// route used to fetch itself (ADR "Mirror posture"). Deliberately
    /// separate from `client` (which routes through the FULL merged mirror
    /// map, managed payload included, for every other OCI operation).
    /// `None` when offline.
    managed_config_client: Option<oci::Client>,
    /// Policy-gated auto-verify configuration.
    ///
    /// `None` = no trust policy configured; the auto-verify hook in the pull
    /// pipeline is a no-op. When `Some`, [`maybe_auto_verify`](Self::maybe_auto_verify)
    /// runs after each package's manifest resolves and before download. Attached
    /// once on the shared manager in `Context::try_init` via
    /// [`with_auto_verify`](Self::with_auto_verify), so every install surface
    /// (install, pull, exec, env, run, patch discovery) inherits it, not just
    /// install/pull.
    auto_verify: Option<AutoVerify>,
}

impl PackageManager {
    /// Construct a new `PackageManager`.
    ///
    /// `patches` comes from `config_view.patches` — pass `None` when no
    /// `[patches]` section is configured. The patch tier is disabled when
    /// `None`; discovery is a no-op.
    pub fn new(
        file_structure: file_structure::FileStructure,
        index: oci::index::Index,
        client: Option<oci::Client>,
        default_registry: impl Into<String>,
    ) -> Self {
        let default_registry = default_registry.into();
        Self {
            file_structure,
            index,
            client,
            default_registry,
            progress: crate::cli::progress::ProgressManager::disabled(),
            patches: None,
            patch_snapshot: None,
            managed_config_client: None,
            auto_verify: None,
        }
    }

    /// Injects the dedicated managed-config-fetch client (built from the
    /// local-only mirror view). Called from `Context::try_init`. Returns
    /// `self` for builder-style chaining alongside `with_patches`.
    pub fn with_managed_config_client(mut self, client: Option<oci::Client>) -> Self {
        self.managed_config_client = client;
        self
    }

    /// Inject the policy-gated auto-verify configuration.
    ///
    /// Called once from `Context::try_init` with `Some(..)` only when at
    /// least one operator trust policy is configured; `None` disables the hook
    /// (no-op). install/pull refine the opt-out from their `--verify`/
    /// `--no-verify` flag via `conventions::manager_with_verify_flag` — every
    /// other surface relies on `OCX_NO_VERIFY` alone. Returns `self` for
    /// builder-style chaining.
    #[must_use]
    pub fn with_auto_verify(mut self, auto_verify: Option<AutoVerify>) -> Self {
        self.auto_verify = auto_verify;
        self
    }

    /// The injected auto-verify configuration, if any.
    pub fn auto_verify(&self) -> Option<&AutoVerify> {
        self.auto_verify.as_ref()
    }

    /// Inject the resolved patch configuration into this manager.
    ///
    /// Called from `Context::try_init` after `config_view.patches` is resolved.
    /// Returns `self` for builder-style chaining alongside `with_progress`.
    pub fn with_patches(mut self, patches: Option<ResolvedPatchConfig>) -> Self {
        self.patches = patches;
        self
    }

    /// Returns the resolved patch configuration, if any.
    pub fn patches(&self) -> Option<&ResolvedPatchConfig> {
        self.patches.as_ref()
    }

    /// Inject the active patch snapshot for opt-in compose determinism.
    ///
    /// When `Some`, the compose overlay prefers the snapshot's pinned
    /// companion digests over live tag lookups. Called from
    /// `Context::try_init` after loading the snapshot from
    /// `OCX_PATCH_SNAPSHOT`. Returns `self` for builder-style chaining.
    pub fn with_patch_snapshot(mut self, patch_snapshot: Option<PatchSnapshot>) -> Self {
        self.patch_snapshot = patch_snapshot;
        self
    }

    /// Returns the active patch snapshot, if any.
    pub fn patch_snapshot(&self) -> Option<&PatchSnapshot> {
        self.patch_snapshot.as_ref()
    }

    /// Sets the shared span-free progress manager. The CLI injects its
    /// stderr manager here; library/test consumers keep the disabled
    /// no-op default from [`new`](Self::new).
    pub fn with_progress(mut self, progress: crate::cli::progress::ProgressManager) -> Self {
        self.progress = progress;
        self
    }

    /// The shared progress manager. Task code calls
    /// [`spinner`](crate::cli::progress::ProgressManager::spinner) /
    /// [`bytes`](crate::cli::progress::ProgressManager::bytes) on it.
    pub fn progress(&self) -> &crate::cli::progress::ProgressManager {
        &self.progress
    }

    pub fn file_structure(&self) -> &file_structure::FileStructure {
        &self.file_structure
    }

    pub fn index(&self) -> &oci::index::Index {
        &self.index
    }

    /// Returns the OCI client as an `Option`. `None` indicates offline mode.
    /// Callers that need to fail loudly when no client is available should use
    /// [`require_client`][Self::require_client] instead.
    pub fn client(&self) -> Option<&oci::Client> {
        self.client.as_ref()
    }

    /// Returns the OCI client, or `Err(OfflineMode)` when no client is
    /// configured. Use this at sites that genuinely need network access.
    pub fn require_client(&self) -> crate::Result<&oci::Client> {
        self.client.as_ref().ok_or(crate::Error::OfflineMode)
    }

    pub fn default_registry(&self) -> &str {
        &self.default_registry
    }

    pub fn is_offline(&self) -> bool {
        self.client.is_none()
    }

    /// Builds an [`InstallInfo`] for an already-installed package whose
    /// on-disk **package root** is known. Used by `ocx launcher exec <pkg-root>`
    /// to bypass identifier resolution and read the installed metadata
    /// directly.
    ///
    /// Reads `metadata.json`, `resolve.json`, and the `digest` file from
    /// `pkg_root`. The returned [`InstallInfo`] holds `pkg_root` itself; env
    /// resolution interpolates `${installPath}` against `info.dir().content()`.
    ///
    /// # Errors
    ///
    /// Returns an error if `pkg_root` does not exist or any of the required
    /// files are missing or malformed.
    pub async fn install_info_from_package_root(
        &self,
        pkg_root: &std::path::Path,
    ) -> crate::Result<crate::package::install_info::InstallInfo> {
        use crate::file_structure::read_digest_file;
        use crate::package::install_info::InstallInfo;
        use crate::package::metadata::ValidMetadata;
        use crate::package::resolved_package::ResolvedPackage;
        use crate::prelude::SerdeExt;

        let objects = &self.file_structure.packages;

        // `*_for_content` accepts either a `content/` path or a package root
        // (see `package_dir_for_content`), so the same helpers serve both the
        // install-symlink chase and the `launcher exec` pkg-root flow.
        let metadata_path = objects.metadata_for_content(pkg_root)?;
        let resolve_path = objects.resolve_for_content(pkg_root)?;
        let (metadata_result, resolved_result) = tokio::join!(
            crate::package::metadata::Metadata::read_json(&metadata_path),
            ResolvedPackage::read_json(&resolve_path),
        );
        // Centralize publish-time validation at consumption: every on-disk
        // metadata blob must clear `ValidMetadata::try_from` before flowing
        // into env resolution (mirrors `tasks/common.rs::load_object_data`).
        // Catches stale or tampered metadata that predates current rules.
        let metadata: crate::package::metadata::Metadata = ValidMetadata::try_from(metadata_result?)?.into();
        let resolved = resolved_result?;

        // Reconstruct a PinnedIdentifier from the sibling `digest` file.
        // The identifier is used only for dedup tracking in resolve_env.
        // Uses the full content digest hex; collision-resistant. The synthetic
        // repository path is internal-only — never persisted in OCI manifests
        // and never compared with real registry repositories — so the longer
        // string is acceptable in exchange for full SHA-256 (~2^256) keyspace,
        // which makes a `(registry, repository)` collision between two distinct
        // pkg-roots probabilistically impossible.
        let digest_path = objects.digest_file_for_content(pkg_root)?;
        let digest = read_digest_file(&digest_path).await?;
        let repo_name = format!("file-url-mode/{}", digest.hex());
        let base_id = crate::oci::Identifier::new_registry(repo_name, &self.default_registry).clone_with_digest(digest);
        let pinned = crate::oci::PinnedIdentifier::try_from(base_id)?;

        Ok(InstallInfo::new(
            pinned,
            metadata,
            resolved,
            crate::file_structure::PackageDir {
                dir: pkg_root.to_path_buf(),
            },
        ))
    }

    /// Resolves environment entries for a package known only by its on-disk
    /// package-root path.
    ///
    /// Convenience wrapper around [`Self::install_info_from_package_root`] +
    /// [`Self::resolve_env`]. Returns the same `Vec<Entry>` shape as
    /// identifier mode so `ocx launcher exec <pkg-root>` has parity with
    /// `ocx exec <identifier>`.
    pub async fn resolve_env_from_package_root(
        &self,
        pkg_root: &std::path::Path,
        self_view: bool,
        scope: crate::package_manager::tasks::resolve::PatchScope,
    ) -> crate::Result<Vec<crate::package::metadata::env::entry::Entry>> {
        let info = self.install_info_from_package_root(pkg_root).await?;
        self.resolve_env(&[std::sync::Arc::new(info)], self_view, scope).await
    }

    /// Boundary primitive for hook-style commands (`shell-hook`, `hook-env`,
    /// future `generate direnv`) that must NOT contact any registry,
    /// regardless of the global `--remote` / `--offline` flags.
    ///
    /// Builds a fresh [`PackageManager`] using the supplied local cache
    /// `local_index` as the *only* index source: chain mode is forced to
    /// [`oci::index::ChainMode::Offline`], and the OCI client is dropped to
    /// `None`. Any incidental tag/manifest lookup short-circuits to the
    /// local cache; an attempt to use the (now-absent) client surfaces as
    /// `Error::OfflineMode`. This is the layer the security boundary docs
    /// in ADR §5B (decision 5B) reference — see
    /// `.claude/artifacts/adr_project_toolchain_config.md`.
    ///
    /// Caller passes the local-index handle separately because the manager
    /// holds a type-erased `Index` (which may be `Default`, `Remote`, or
    /// already `Offline`); reaching back through the type-erased boundary
    /// would couple this primitive to `ChainedIndex` internals. The CLI
    /// `Context` already exposes `local_index().clone()`, so the call site
    /// is `context.manager().offline_view(context.local_index().clone())`.
    pub fn offline_view(&self, local_index: oci::index::LocalIndex) -> Self {
        let offline_index = oci::index::Index::from_chained(local_index, Vec::new(), oci::index::ChainMode::Offline);
        Self {
            file_structure: self.file_structure.clone(),
            index: offline_index,
            client: None,
            default_registry: self.default_registry.clone(),
            progress: self.progress.clone(),
            // Preserve the patch config. `offline_view` disables the *network*
            // (client = None → `is_offline()` true), NOT the patch tier. These are
            // two separate concerns:
            //
            // - Phase 3 discovery (`discover_and_install_patches`) requires network
            //   to fetch descriptor blobs, and already short-circuits on
            //   `self.is_offline()` — so keeping `patches` here does NOT re-enable
            //   any network discovery on an offline view.
            // - Phase 4 site-overlay (`build_site_patch_set`) is compose-time and
            //   purely local (tag store + descriptor blobs + installed companions).
            //   It MUST still run on offline env paths (`ocx direnv export`, the
            //   global toolchain) so already-discovered companion overlays apply,
            //   and so a `required` companion that is unavailable **fails closed**.
            //
            // Dropping `patches` here would silently skip required overlays on
            // exactly those local-only exporters — a fail-OPEN gap violating the
            // ADR offline contract (C4 "works offline once synced", C6 zip-`OCX_HOME`
            // parity, C7 fail-closed). So the tier is carried through unchanged.
            patches: self.patches.clone(),
            // Carry the snapshot through so offline env paths (direnv export,
            // global toolchain) still resolve frozen companion digests when a
            // snapshot is active.
            patch_snapshot: self.patch_snapshot.clone(),
            // An offline view has no network route at all — the managed-config
            // fetch client is dropped along with the main client.
            managed_config_client: None,
            // `offline_view` is a hook-command primitive (env export, global
            // toolchain) that never installs, so the auto-verify hook is never
            // reached; carry the config through unchanged for parity.
            auto_verify: self.auto_verify.clone(),
        }
    }
}
