// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Rendering a toolchain home — trampolines and `links/<group>/<entry>` links
//! (`adr_toolchain_activation.md` § Rationale from code: render_toolchain).
//!
//! ```text
//! <home>/
//! ├── .gitignore                    "*", owned by `ToolchainHome`
//! ├── active -> shells/default      the depth-1 link every PATH route resolves through
//! ├── links/<group>/<entry>/        directory link to a package root, one per selected group
//! └── shells/<shell>/bin/<name>     one launcher trampoline per exposed name, DEFAULT group only
//!                                    (Windows: `<name>.exe` plus its `<name>.exec` sidecar — two entries)
//! ```
//!
//! Every write, prune, fingerprint and guard uses the physical `shell_bin()`, never the PATH-facing
//! `bin()`, so a repointed `active` redirects none of them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::PackageErrorKind;
use crate::launcher::{TrampolineTarget, exec_sidecar_body, trampoline_ocx_binary, unix_trampoline_body};
use ocx_project::{DEFAULT_GROUP, LockedTool, ProjectLock};
use ocx_store::file_structure::{
    BinEntryStamp, DEFAULT_SHELL, FileStructure, RenderStamp, RenderStampScope, RenderStampTarget, ShimBinStore,
    TREE_OWN_DEPTH1_NAMES, ToolchainHome,
};
use ocx_store::reference_manager::ReferenceManager;

use super::super::PackageManager;
use super::common::ClosureNode;
use super::toolchain_names::{NotEnumerablePolicy, exposed_names, fold_case_insensitive};

/// The `__OCX_TESTING_RENDER_FAULT` value that aborts a render between the first `bin/` entry
/// write and the stamp write.
#[cfg(any(test, feature = "__testing"))]
pub const FAULT_AFTER_FIRST_ENTRY_WRITE: &str = "after_first_entry_write";

/// The `__OCX_TESTING_RENDER_FAULT` value that aborts a render between `shells/<shell>/` and the
/// `active` link — the only point at which the heal order is observable.
#[cfg(any(test, feature = "__testing"))]
pub const FAULT_AFTER_SHELL_TREE: &str = "after_shell_tree";

/// Everything one [`PackageManager::render_toolchain`] call needs; the rendered tree is a pure
/// function of it plus the store root
/// (`adr_toolchain_activation.md` § Rationale from code: render_toolchain).
pub struct RenderRequest<'a> {
    /// The resolved home; its root may not exist yet, and the render creates it.
    pub home: &'a ToolchainHome,

    /// The tier. A project `dir` must be the canonical project directory, or the consent and render
    /// stamps hash to different `state/projects/<key>/` and the stamp gate never matches.
    pub scope: &'a RenderStampScope,

    /// The lock this render materialises.
    pub lock: &'a ProjectLock,

    /// The default group's closure from [`PackageManager::toolchain_surface`]; another group's closure
    /// compiles and silently renders `bin/` for the wrong group.
    pub surface: &'a [ClosureNode],

    /// Groups that get `<group>/<entry>` links; `bin/` is reconciled only when
    /// [`DEFAULT_GROUP`](ocx_project::DEFAULT_GROUP) is among them, else left untouched.
    pub groups: &'a [String],

    /// Suppresses the whole link pass (no writes, no prunes); trampolines are unaffected.
    pub pinned: bool,

    /// The platform whose leaf digest each `<group>/<entry>` link resolves to.
    pub platform: &'a ocx_oci::Platform,

    /// Report the delta and write nothing — no heal, no case-fold probe, no `projects/` registration.
    pub dry_run: bool,
}

/// What one render did to one artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedItem {
    pub artifact: RenderedArtifact,
    pub outcome: RenderOutcome,
}

/// One artifact a render is responsible for. `String`, not `OsString`, so a non-UTF-8 on-disk name
/// has no representation and is never reported or pruned
/// (`adr_toolchain_activation.md` § Rationale from code: render_toolchain).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderedArtifact {
    /// A `bin/` trampoline, one per on-disk file (Windows: `<name>.exe` and `<name>.exec` are two).
    Trampoline(String),
    /// A `links/<group>/<entry>` directory link to a package root.
    Link {
        group: String,
        /// The tool's local binding name inside that group.
        entry: String,
    },
    /// An orphaned `links/<group>/` directory.
    GroupDirectory(String),
    /// A home-root name outside the tree-own depth-1 set.
    RootEntry(String),
}

/// What a render did to one [`RenderedArtifact`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderOutcome {
    /// The artifact was created or replaced.
    Written,
    /// The artifact was already correct and was left byte-identical.
    Unchanged,
    /// The artifact left the computed set and was removed.
    Pruned,
    /// The write or removal failed and the render continued — not an error: the caller warns and
    /// exits 0.
    Skipped {
        /// The path the render could not write or remove.
        path: PathBuf,
        /// The underlying failure's `Display`: prose for a warn line, which nothing parses.
        reason: String,
    },
}

/// What one [`PackageManager::render_toolchain`] call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderReport {
    /// Every artifact the render was responsible for and its outcome; under
    /// [`RenderRequest::dry_run`], the delta it would apply.
    pub items: Vec<RenderedItem>,

    /// Whether `bin/` was reconciled at all. A field, not inferred from [`Self::items`]: zero
    /// trampolines means "all removed" or "never read", and only this tells them apart.
    pub bin_in_scope: bool,

    /// Whether this call wrote the render stamp. Written last, so `false` beside non-empty
    /// [`Self::items`] marks an incomplete render
    /// (`adr_toolchain_activation.md` § Rationale from code: render_toolchain).
    pub stamp_written: bool,
}

impl PackageManager {
    /// The merged closure [`RenderRequest::surface`] takes: each of `roots` (the default group's)
    /// followed by its metadata-only dependency closure, deps before dependents
    /// (`adr_toolchain_activation.md` § Rationale from code: render_toolchain).
    ///
    /// # Errors
    ///
    /// The first failing root's walk error: a partial surface renders a quietly incomplete `bin/`.
    pub async fn toolchain_surface(
        &self,
        roots: &[ocx_oci::PackageRef],
        platform: &ocx_oci::Platform,
    ) -> Result<Vec<ClosureNode>, PackageErrorKind> {
        let (file_structure, index) = (self.file_structure(), self.index());
        let mut merged = Vec::new();
        for root in roots {
            let resolved = self.resolve(root, platform.clone()).await?;
            // The walk reads each dependency's config blob from the blob store, so warm the chain.
            super::common::stage_chain_blobs(file_structure, index, &resolved).await?;
            super::common::stage_leaf_manifest(file_structure, index, &resolved.pinned).await?;

            let metadata =
                super::common::load_config_metadata(index, &resolved.pinned, &resolved.final_manifest).await?;
            let config_digest = super::common::config_blob_digest(&resolved.final_manifest)?;
            merged.extend(
                super::common::walk_closure_nodes(
                    file_structure,
                    index,
                    self.is_offline(),
                    &resolved.pinned,
                    &metadata,
                    config_digest,
                    platform,
                )
                .await?,
            );
        }
        Ok(merged)
    }

    /// Render `request.home`: trampolines, the selected groups' links, prunes, then the stamp last.
    /// Idempotent; a write, removal or lock-timeout failure is [`RenderOutcome::Skipped`], never an
    /// error (`adr_toolchain_activation.md` § Rationale from code: render_toolchain).
    ///
    /// # Errors
    ///
    /// - [`PackageErrorKind::ToolchainPath`] — a group or entry name cannot be a path component
    ///   (exit 78).
    /// - [`PackageErrorKind::ShimNameInvalid`] — an entry point name is not a valid
    ///   [`BinaryName`](ocx_package::metadata::BinaryName).
    /// - [`PackageErrorKind::Internal`] — a trampoline body or sidecar refusal, or a
    ///   `__OCX_TESTING_RENDER_FAULT` seam; never a filesystem failure.
    pub async fn render_toolchain(&self, request: RenderRequest<'_>) -> Result<RenderReport, PackageErrorKind> {
        let file_structure = self.file_structure();

        // Read once, here; outside a gated build the env var is never read (`subsystem-tests.md`).
        #[cfg(any(test, feature = "__testing"))]
        let fault = read_fault_hook();
        #[cfg(not(any(test, feature = "__testing")))]
        let fault: Option<String> = None;
        let fault = fault.as_deref();

        // Step 1: a dry run returns before the case-fold probe, whose write would move the home's mtime.
        if request.dry_run {
            return render_with(file_structure, request, false, fault).await;
        }

        // Step 2: the home root, before the probe and `.gitignore` (both write into it). The
        // symlinked-ancestor guard runs first: a symlink above the root relocates everything below it.
        let (scope, root) = (request.scope.clone(), request.home.root().to_path_buf());
        match tokio::task::spawn_blocking(move || {
            let home = ToolchainHome::new(root);
            refuse_symlinked_home(&scope, &home)?;
            ensure_home_root(&home)
        })
        .await
        {
            Ok(Ok(_created)) => {}
            Ok(Err(error)) => {
                // Warn, not debug: a tree ocx cannot own, or a committed symlink out of it.
                log::warn!(
                    "Skipping toolchain render: '{}' could not be prepared: {error}",
                    request.home.root().display()
                );
                return Ok(skipped_render());
            }
            Err(join_error) => {
                log::warn!("Skipping toolchain render: the home-root task failed: {join_error}");
                return Ok(skipped_render());
            }
        }

        // Step 3: `.gitignore`, best-effort — a repository-controlled tree may deny it.
        let gitignore_home = ToolchainHome::new(request.home.root().to_path_buf());
        match tokio::task::spawn_blocking(move || gitignore_home.ensure_gitignore()).await {
            Ok(Ok(_written)) => {}
            Ok(Err(error)) => log::warn!(
                "Toolchain home '{}' has no ignore file: {error}",
                request.home.root().display()
            ),
            Err(join_error) => log::warn!("The toolchain ignore-file task failed: {join_error}"),
        }

        // Step 4: one lock over the whole body, held until this call returns.
        let _render_lock = match render_lock_parameters(file_structure, &request).acquire().await {
            Ok(guard) => guard,
            Err(error) => {
                log::warn!(
                    "Skipping toolchain render of '{}': its render lock was not available: {error}",
                    request.home.root().display()
                );
                return Ok(skipped_render());
            }
        };

        // Step 5: the probe, against the root step 2 just created.
        let case_insensitive = match filesystem_is_case_insensitive(request.home.root()).await {
            Ok(answer) => answer,
            Err(error) => {
                log::warn!(
                    "Skipping toolchain render: the case-fold probe of '{}' failed: {error}",
                    request.home.root().display()
                );
                return Ok(skipped_render());
            }
        };

        // Step 6: `shells/<shell>/` then `active`, before any trampoline. A failure only warns: a bad
        // `active` fails `active_is_valid` so the gate withholds; returning would skip a read-only checkout.
        let root = request.home.root().to_path_buf();
        let shell_tree =
            tokio::task::spawn_blocking(move || ensure_shell_tree(&ToolchainHome::new(root), DEFAULT_SHELL)).await;

        // An abort here leaves `active` absent — a lookup miss, never a wrong answer.
        #[cfg(any(test, feature = "__testing"))]
        maybe_inject_fault(fault, RenderStage::AfterShellTree)?;

        let root = request.home.root().to_path_buf();
        let healed = match shell_tree {
            Ok(Ok(())) => {
                tokio::task::spawn_blocking(move || heal_active(&ToolchainHome::new(root), DEFAULT_SHELL)).await
            }
            other => other,
        };
        match healed {
            Ok(Ok(())) => {}
            Ok(Err(error)) => log::warn!(
                "Toolchain '{}' has no usable '{}' link: {error}",
                request.home.root().display(),
                request.home.active().display()
            ),
            Err(join_error) => log::warn!("The toolchain activation-link task failed: {join_error}"),
        }

        render_with(file_structure, request, case_insensitive, fault).await
    }
}

/// The report of a render skipped before the reconcile: nothing in `bin/` was read or touched.
fn skipped_render() -> RenderReport {
    RenderReport {
        items: Vec::new(),
        bin_in_scope: false,
        stamp_written: false,
    }
}

/// One `lock_scoped` call's arguments as a value, so a test can take the very lock the code under
/// test takes.
pub(crate) struct ScopedLockParameters {
    /// `$OCX_HOME/locks` — never a sidecar inside the guarded tree.
    pub(crate) locks_root: PathBuf,
    pub(crate) scope: &'static str,
    /// The directory whose file identity is the lock's primary key.
    pub(crate) guarded_directory: PathBuf,
    /// What distinguishes locks sharing a directory and a scope.
    pub(crate) discriminator: String,
    /// How long an acquire waits before the caller degrades to the skip path.
    pub(crate) timeout: Duration,
}

impl ScopedLockParameters {
    /// Take the lock these parameters describe.
    ///
    /// # Errors
    ///
    /// [`lock_scoped`](ocx_util::fs::lock_scoped)'s own: the lock file, or the [`timeout`](Self::timeout).
    pub(crate) async fn acquire(&self) -> crate::Result<ocx_util::fs::LockedFile> {
        ocx_util::fs::lock_scoped(
            &self.locks_root,
            self.scope,
            &self.guarded_directory,
            &self.discriminator,
            self.timeout,
        )
        .await
        .map_err(Into::into)
    }
}

const RENDER_LOCK_SCOPE: &str = "toolchain-render";

const HEAL_LOCK_SCOPE: &str = "toolchain-heal";

/// How long either lock waits before its caller degrades: `ocx pull`'s own lock budget.
const TOOLCHAIN_LOCK_TIMEOUT: Duration = super::pull::PULL_LOCAL_LOCK_TIMEOUT;

/// Test-only override of [`TOOLCHAIN_LOCK_TIMEOUT`], in milliseconds.
#[cfg(any(test, feature = "__testing"))]
const TESTING_LOCK_TIMEOUT_ENV: &str = "__OCX_TESTING_TOOLCHAIN_LOCK_TIMEOUT_MS";

/// [`TOOLCHAIN_LOCK_TIMEOUT`], or the [`TESTING_LOCK_TIMEOUT_ENV`] override. Panics on a malformed
/// value: a fallback would let a typo out-wait the real deadline and still pass.
#[cfg(any(test, feature = "__testing"))]
fn toolchain_lock_timeout() -> Duration {
    let Some(raw) = ocx_util::env::var(TESTING_LOCK_TIMEOUT_ENV) else {
        return TOOLCHAIN_LOCK_TIMEOUT;
    };
    Duration::from_millis(
        raw.trim().parse().unwrap_or_else(|_| {
            panic!("{TESTING_LOCK_TIMEOUT_ENV} must be a whole number of milliseconds, got {raw:?}")
        }),
    )
}

#[cfg(not(any(test, feature = "__testing")))]
fn toolchain_lock_timeout() -> Duration {
    TOOLCHAIN_LOCK_TIMEOUT
}

/// The lock for one render, discriminated by the stamp key so two homes never share a lock and one
/// home never takes two.
pub(crate) fn render_lock_parameters(
    file_structure: &FileStructure,
    request: &RenderRequest<'_>,
) -> ScopedLockParameters {
    ScopedLockParameters {
        locks_root: file_structure.locks.clone(),
        scope: RENDER_LOCK_SCOPE,
        guarded_directory: request.home.root().to_path_buf(),
        discriminator: stamp_key(request.scope),
        timeout: toolchain_lock_timeout(),
    }
}

/// The lock for one [`heal_links`] repoint, per entry so only repoints of the same link contend. Takes
/// the group directory itself: a re-joined group name is a second spelling that drifted once.
pub(crate) fn heal_lock_parameters(
    file_structure: &FileStructure,
    group_directory: PathBuf,
    entry: &str,
) -> ScopedLockParameters {
    ScopedLockParameters {
        locks_root: file_structure.locks.clone(),
        scope: HEAL_LOCK_SCOPE,
        guarded_directory: group_directory,
        discriminator: entry.to_string(),
        timeout: toolchain_lock_timeout(),
    }
}

/// `<root>/links`, as the parent of the grammar's own `links_group` so the name has one spelling.
/// The fallback is unreachable (`links_root_is_the_grammars_own_parent`).
fn links_root(home: &ToolchainHome) -> PathBuf {
    home.links_group(DEFAULT_GROUP)
        .ok()
        .as_deref()
        .and_then(Path::parent)
        .map_or_else(|| home.root().to_path_buf(), Path::to_path_buf)
}

/// The key a tier's render stamp and render lock are addressed by.
fn stamp_key(scope: &RenderStampScope) -> String {
    match scope {
        RenderStampScope::Global => "global".to_string(),
        RenderStampScope::Project(directory) => ReferenceManager::name_for_path(directory),
    }
}

/// The render body, with the case-fold probe and fault stage as parameters (a free function per
/// `subsystem-package-manager.md`).
///
/// Precondition: the home root exists — created above this call, or a first-ever `ocx pull` skips —
/// and the caller holds the render lock for the whole call.
///
/// # Errors
///
/// As [`PackageManager::render_toolchain`].
async fn render_with(
    file_structure: &FileStructure,
    request: RenderRequest<'_>,
    case_insensitive: bool,
    fault: Option<&str>,
) -> Result<RenderReport, PackageErrorKind> {
    let bin_in_scope = request.groups.iter().any(|group| group == DEFAULT_GROUP);
    let mut items: Vec<RenderedItem> = Vec::new();

    let landed = if bin_in_scope {
        reconcile_bin(file_structure, &request, case_insensitive, fault, &mut items).await?
    } else {
        BTreeSet::new()
    };

    if !request.pinned {
        reconcile_links(file_structure, &request, case_insensitive, &mut items).await?;
    }

    // ── The `projects/` GC ledger, then the stamp, last ─────────────────────
    if !request.dry_run
        && let RenderStampScope::Project(project_directory) = request.scope
        && let Err(error) = ocx_project::ProjectRegistry::new(file_structure.root())
            .register(project_directory)
            .await
    {
        log::warn!(
            "Project '{}' was not registered as a GC root: {error}",
            project_directory.display()
        );
    }

    // One blocking unit for the whole stamp: every step in it is a blocking filesystem call.
    let stamp_written = if request.dry_run {
        false
    } else {
        let root = request.home.root().to_path_buf();
        let (state, scope, landed) = (file_structure.state.clone(), request.scope.clone(), landed.clone());
        let written = blocking(root.clone(), move || {
            Ok(write_render_stamp(
                &state,
                &ToolchainHome::new(root),
                &scope,
                bin_in_scope,
                &landed,
            ))
        })
        .await;
        match written {
            Ok(written) => written,
            Err(error) => {
                log::warn!(
                    "Toolchain '{}' was rendered but its render-stamp task failed: {error}",
                    request.home.root().display()
                );
                false
            }
        }
    };

    Ok(RenderReport {
        items,
        bin_in_scope,
        stamp_written,
    })
}

/// Reconcile `bin/` and return the file names that landed (`Written` or `Unchanged`, never
/// `Skipped`): the stamp must certify only trampolines the composing side may trust.
async fn reconcile_bin(
    file_structure: &FileStructure,
    request: &RenderRequest<'_>,
    case_insensitive: bool,
    fault: Option<&str>,
    items: &mut Vec<RenderedItem>,
) -> Result<BTreeSet<String>, PackageErrorKind> {
    #[cfg(not(any(test, feature = "__testing")))]
    let _ = fault;

    let home = request.home;
    // Physical `shell_bin`, never `bin()`: through `active`, every write and the stamp would land in
    // and certify whatever directory `active` points at.
    let bin = home.shell_bin(DEFAULT_SHELL);

    // `Skip`, not `Refuse`: a package claiming no names contributes nothing and is not an error.
    let names = exposed_names(request.surface, NotEnumerablePolicy::Skip)?;
    let names = if case_insensitive {
        fold_case_insensitive(names)
    } else {
        names
    };

    // Both bodies on every platform, not `cfg`-gated, so each one's non-absolute-root refusal is
    // compiled on the CI leg that actually runs.
    let target = trampoline_target(request.scope);
    let ocx_binary = trampoline_ocx_binary(file_structure).await;
    let body = unix_trampoline_body(&target, ocx_binary.as_deref()).map_err(PackageErrorKind::Internal)?;
    // The sidecar bakes the resolved `ocx` too: Windows searches a bare `ocx` in `bin/` first, where a
    // package claiming the name has rendered `ocx.exe`.
    let sidecar = exec_sidecar_body(&target, ocx_binary.as_deref()).map_err(PackageErrorKind::Internal)?;

    let mut expected: BTreeSet<String> = BTreeSet::new();
    for name in names.keys() {
        expected.extend(trampoline_file_names(name.as_str()));
    }

    let mut writable = true;
    if !request.dry_run && !names.is_empty() {
        let target = bin.clone();
        // Owner-only at create, never the umask: `bin/` is on PATH, so group-writable is a write
        // primitive into the shell. Non-recursive, or a symlinked level is followed across the lock wait.
        if let Err(error) = blocking(bin.clone(), move || create_directory_owner_only(&target)).await {
            log::warn!("Toolchain '{}' is not writable: {error}", bin.display());
            writable = false;
        }
    }

    let mut landed: BTreeSet<String> = BTreeSet::new();
    let mut wrote_first = false;
    for name in names.keys() {
        let files = trampoline_file_names(name.as_str());
        let outcome = if writable {
            publish_bin_entry(
                &bin,
                name.as_str(),
                &body,
                &sidecar,
                &file_structure.shim_bin,
                request.dry_run,
            )
            .await
        } else {
            Err(crate::error::file_error(
                &bin,
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "the trampoline directory is not writable",
                ),
            ))
        };

        match outcome {
            Ok(written) => {
                let outcome = if written {
                    RenderOutcome::Written
                } else {
                    RenderOutcome::Unchanged
                };
                for file in &files {
                    landed.insert(file.clone());
                    items.push(RenderedItem {
                        artifact: RenderedArtifact::Trampoline(file.clone()),
                        outcome: outcome.clone(),
                    });
                }
                if written && !request.dry_run && !wrote_first {
                    wrote_first = true;
                    #[cfg(any(test, feature = "__testing"))]
                    maybe_inject_fault(fault, RenderStage::AfterFirstEntryWrite)?;
                }
            }
            Err(error) => {
                let reason = error.to_string();
                for file in &files {
                    items.push(RenderedItem {
                        artifact: RenderedArtifact::Trampoline(file.clone()),
                        outcome: RenderOutcome::Skipped {
                            path: bin.join(file),
                            reason: reason.clone(),
                        },
                    });
                }
            }
        }
    }

    // The prune half; a non-UTF-8 name is neither reported nor removed.
    for name in read_dir_utf8_names(&bin).await {
        if is_expected(&expected, &name, case_insensitive) {
            continue;
        }
        let artifact = RenderedArtifact::Trampoline(name);
        items.push(RenderedItem {
            outcome: prune_outcome(home, &artifact, request.dry_run).await,
            artifact,
        });
    }

    Ok(landed)
}

/// Publish one exposed name's `bin/` entry; `Ok(true)` when this call wrote. A runtime branch, not
/// a `cfg`, so the Windows arm stays testable on every host.
async fn publish_bin_entry(
    bin: &Path,
    name: &str,
    body: &str,
    sidecar: &str,
    shim_bin: &ShimBinStore,
    dry_run: bool,
) -> crate::Result<bool> {
    if cfg!(windows) {
        let exe = bin.join(format!("{name}.exe"));
        let sidecar_path = bin.join(format!("{name}.exec"));
        // The blob is what `<name>.exe` must be. `ensure()` publishes it, so a dry run skips it for the
        // weaker predicate — sound, since a dry run writes no stamp to bless its `Unchanged`.
        let blob = if dry_run { None } else { Some(shim_bin.ensure().await?) };

        let (exe_probe, sidecar_probe) = (exe.clone(), sidecar_path.clone());
        let sidecar_bytes = sidecar.to_string();
        let unchanged = blocking(exe.clone(), move || {
            windows_pair_unchanged(&exe_probe, &sidecar_probe, &sidecar_bytes, blob.as_deref())
        })
        .await?;
        if unchanged {
            return Ok(false);
        }
        if !dry_run {
            publish_windows_trampoline(shim_bin, &exe, &sidecar_path, sidecar).await?;
        }
        return Ok(true);
    }

    let path = bin.join(name);
    let body = body.to_string();
    blocking(path.clone(), move || {
        let existing = read_existing_trampoline(&path, body.len() as u64)?;
        // Mode is part of the predicate: a committed `100644` file with the exact body would stay
        // non-executable forever, as only `write_trampoline_atomic` sets the bit.
        if existing.as_deref() == Some(body.as_bytes()) && has_execute_bit(&path) {
            return Ok(false);
        }
        if !dry_run {
            write_trampoline_atomic(&path, &body)?;
        }
        Ok(true)
    })
    .await
}

/// Whether the Windows pair on disk is what this render would publish: the sidecar's bytes and the
/// `.exe`'s file identity — existence alone would certify a substituted `.exe`
/// (`adr_toolchain_activation.md` § Rationale from code: render_toolchain).
/// `blob` is `None` only under a dry run, where a regular-file check suffices.
///
/// # Errors
///
/// The sidecar read's own I/O failure.
fn windows_pair_unchanged(exe: &Path, sidecar_path: &Path, sidecar: &str, blob: Option<&Path>) -> crate::Result<bool> {
    let existing = read_existing_trampoline(sidecar_path, sidecar.len() as u64)?;
    if existing.as_deref() != Some(sidecar.as_bytes()) {
        return Ok(false);
    }
    match blob {
        // No file identity falls through to the write branch, fail-closed like `BinEntryStamp::file_id`.
        Some(blob) => Ok(matches!(
            (file_identity(exe), file_identity(blob)),
            (Some(published), Some(expected)) if published == expected
        )),
        None => Ok(file_identity(exe).is_some()),
    }
}

/// One file's identity (inode / file index), or `None` when absent, not a regular file or unreported.
/// Via [`BinEntryStamp::from_metadata`] so both ends of the prompt gate derive it alike.
fn file_identity(path: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    BinEntryStamp::from_metadata(path, &metadata, String::new()).file_id
}

/// Whether `path` has any execute bit. Not `== 0o755`: a narrowed mode is still correct, and
/// rewriting it on every render would break idempotence.
#[cfg(unix)]
fn has_execute_bit(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

/// No mode bits off Unix; never reached there, as the caller branches on the host first.
#[cfg(not(unix))]
fn has_execute_bit(_path: &Path) -> bool {
    true
}

/// Reconcile each selected group's links, then the orphans under `links/` (entries first, so a
/// departed group's directory empties), then the home root's depth-1 names. `case_insensitive` reaches
/// only the prune comparisons: lock names get no last-walked-wins fold.
async fn reconcile_links(
    file_structure: &FileStructure,
    request: &RenderRequest<'_>,
    case_insensitive: bool,
    items: &mut Vec<RenderedItem>,
) -> Result<(), PackageErrorKind> {
    let home = request.home;
    let mut selected: BTreeSet<&str> = BTreeSet::new();
    for group in request.groups {
        selected.insert(group.as_str());
    }

    for group in &selected {
        let mut expected: BTreeSet<String> = BTreeSet::new();
        for tool in request.lock.tools.iter().filter(|tool| tool.group == **group) {
            // `ocx.lock` ships with a hostile clone: names are validated as path components (exit 78).
            let entry = home.entry(&tool.group, &tool.name)?;
            expected.insert(tool.name.clone());

            // `select_best`, never an exact key; no compatible key skips the entry silently.
            let Some(target) = link_target(file_structure, tool, request.platform) else {
                continue;
            };
            items.push(RenderedItem {
                artifact: RenderedArtifact::Link {
                    group: tool.group.clone(),
                    entry: tool.name.clone(),
                },
                outcome: publish_link(&entry, &target, request.dry_run).await,
            });
        }

        for name in read_dir_link_side_names(&home.links_group(group)?).await {
            if is_expected(&expected, &name, case_insensitive) {
                continue;
            }
            let artifact = RenderedArtifact::Link {
                group: (*group).to_string(),
                entry: name,
            };
            items.push(RenderedItem {
                outcome: prune_outcome(home, &artifact, request.dry_run).await,
                artifact,
            });
        }
    }

    // `links/`'s own orphans, keyed on the lock: an unselected group is out of scope, not an orphan.
    let locked_groups: BTreeSet<&str> = request.lock.tools.iter().map(|tool| tool.group.as_str()).collect();
    for name in read_dir_link_side_names(&links_root(home)).await {
        if is_expected(&locked_groups, &name, case_insensitive) {
            continue;
        }
        // Entries before the group dir, or the non-recursive `remove_dir` fails `ENOTEMPTY` forever.
        // Joined raw: `links_group` would refuse this leftover name; `prune_within` contains it.
        for entry in read_dir_link_side_names(&links_root(home).join(&name)).await {
            let artifact = RenderedArtifact::Link {
                group: name.clone(),
                entry,
            };
            items.push(RenderedItem {
                outcome: prune_outcome(home, &artifact, request.dry_run).await,
                artifact,
            });
        }
        let artifact = RenderedArtifact::GroupDirectory(name);
        items.push(RenderedItem {
            outcome: prune_outcome(home, &artifact, request.dry_run).await,
            artifact,
        });
    }

    // Against the fixed `TREE_OWN_DEPTH1_NAMES`, never accessor-derived (`bin()` and `shell_bin()` share
    // a file name). Always case-folded, or a case collision prunes the tree's own directory
    // (`adr_toolchain_activation.md` § "Rationale from code: render_toolchain").
    for name in read_dir_link_side_names(home.root()).await {
        if TREE_OWN_DEPTH1_NAMES
            .iter()
            .any(|kept| kept.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        let artifact = RenderedArtifact::RootEntry(name);
        items.push(RenderedItem {
            outcome: prune_outcome(home, &artifact, request.dry_run).await,
            artifact,
        });
    }

    Ok(())
}

/// Whether `expected` retains `name` in the prune pass, ASCII-folded on a case-insensitive home. The
/// write pass keeps the winner's spelling (`Make`) while disk may hold `make`; an exact match would
/// prune it and stamp an empty set that passes the gate while the tool is gone.
fn is_expected<S>(expected: &BTreeSet<S>, name: &str, case_insensitive: bool) -> bool
where
    S: std::borrow::Borrow<str> + Ord,
{
    expected.contains(name)
        || (case_insensitive
            && expected
                .iter()
                .any(|candidate| candidate.borrow().eq_ignore_ascii_case(name)))
}

/// Write or repoint one `<group>/<entry>` link, off the runtime as one blocking unit.
async fn publish_link(entry: &Path, target: &Path, dry_run: bool) -> RenderOutcome {
    let (entry, target) = (entry.to_path_buf(), target.to_path_buf());
    let context = entry.clone();
    match blocking(context.clone(), move || {
        Ok(publish_link_within(&entry, &target, dry_run))
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => RenderOutcome::Skipped {
            path: context,
            reason: error.to_string(),
        },
    }
}

/// [`publish_link`]'s synchronous body. A symlinked `<group>/` is refused as a skip, before the
/// `dry_run` return so a dry run predicts it: the rename would otherwise write through it.
fn publish_link_within(entry: &Path, target: &Path, dry_run: bool) -> RenderOutcome {
    if std::fs::read_link(entry).is_ok_and(|current| current == target) {
        return RenderOutcome::Unchanged;
    }
    if let Some(parent) = entry.parent()
        && std::fs::symlink_metadata(parent).is_ok_and(|metadata| metadata.is_symlink())
    {
        return RenderOutcome::Skipped {
            path: entry.to_path_buf(),
            reason: refuse_symlink(parent).to_string(),
        };
    }
    if dry_run {
        return RenderOutcome::Written;
    }
    let published = (|| -> crate::Result<()> {
        if let Some(parent) = entry.parent() {
            // `ensure_link_group`, never `create_owner_only`: a recursive create follows a symlinked
            // `links/`, and the check above judges only the group.
            ensure_link_group(parent)?;
        }
        // A dereferencing copy (`cp -rL`, Docker `COPY`) leaves a real directory, and `rename(2)` fails
        // `EISDIR` on one. `remove_dir`, never `remove_dir_all`: a non-empty package copy must survive.
        if occupied_by_directory(entry) {
            let _ = std::fs::remove_dir(entry);
        }
        // `replace_atomic`, never `symlink::update`: remove-then-create briefly drops the link and trips
        // the per-entry degrade mid-repoint.
        ocx_util::fs::symlink::replace_atomic(target, entry).map_err(Into::into)
    })();
    match published {
        Ok(()) => RenderOutcome::Written,
        Err(error) => RenderOutcome::Skipped {
            path: entry.to_path_buf(),
            // Re-observed, so an entry that became or stopped being a directory gets the right reason.
            reason: if occupied_by_directory(entry) {
                refuse_dereferenced_copy(entry)
            } else {
                error.to_string()
            },
        },
    }
}

/// A real directory at `path`, never a link to one: `remove_dir` must not be reachable for a link
/// into someone else's tree. A failed stat reads as "no".
fn occupied_by_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// The remedy for a `<group>/<entry>` a dereferencing copy turned into a non-empty directory. The path
/// is not escaped: [`skipped_render_warnings`](crate::skipped_render_warnings) escapes the reason once.
fn refuse_dereferenced_copy(entry: &Path) -> String {
    format!(
        "a directory occupies this link's name — the mark of a symlink-dereferencing copy \
         (`cp -rL`, `unzip`, `rsync` without `-l`, Docker `COPY`); it is left in place rather \
         than deleted recursively. Remove '{}' and run `ocx pull` again",
        entry.display()
    )
}

/// The digest root one locked tool's link must name on this host, or `None` when the lock ships no
/// compatible platform. Shared with the heal, so render and heal never disagree on a platform key.
pub(crate) fn link_target(
    file_structure: &FileStructure,
    tool: &LockedTool,
    platform: &ocx_oci::Platform,
) -> Option<PathBuf> {
    // An ambiguous lock is skipped like a missing one: one unavailable tool must not fail the render.
    let pinned = tool.repository.pin_untagged(tool.host_leaf(platform).ok()?);
    Some(file_structure.packages.path(&pinned))
}

/// [`prune_within`] as a reportable outcome; a failure is a skip. The dry-run `Pruned` is only a
/// prediction: `ENOTEMPTY` or a containment refusal may skip it for real.
async fn prune_outcome(home: &ToolchainHome, artifact: &RenderedArtifact, dry_run: bool) -> RenderOutcome {
    if dry_run {
        return RenderOutcome::Pruned;
    }
    let path = artifact_path(home, artifact);
    let (root, owned) = (home.root().to_path_buf(), artifact.clone());
    let pruned = blocking(path.clone(), move || prune_within(&ToolchainHome::new(root), &owned)).await;
    match pruned {
        Ok(()) => RenderOutcome::Pruned,
        Err(error) => RenderOutcome::Skipped {
            reason: format!(
                "{error} — it is left in place rather than deleted recursively. Remove '{}' and run `ocx pull` again",
                path.display()
            ),
            path,
        },
    }
}

/// The sorted UTF-8 entry names of `directory`; absent or unreadable reads as empty, and a non-UTF-8
/// name is dropped, so it is never pruned.
async fn read_dir_utf8_names(directory: &Path) -> Vec<String> {
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return Vec::new();
    };
    let mut names = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_string());
        }
    }
    names.sort();
    names
}

/// [`read_dir_utf8_names`] minus OS metadata files, which are left in place unreported: Finder and
/// Explorer recreate them, so refusing one warned on every render. Never for `bin/`, which is on `PATH`.
async fn read_dir_link_side_names(directory: &Path) -> Vec<String> {
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return Vec::new();
    };
    let mut names = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        // Regular files only: `desktop.ini` is a valid tool name, and a stale link by it is still ours.
        if is_os_metadata_file(&name) && entry.file_type().await.is_ok_and(|kind| kind.is_file()) {
            log::debug!(
                "Toolchain render leaves OS metadata file '{}' in place",
                entry.path().display()
            );
            continue;
        }
        names.push(name);
    }
    names.sort();
    names
}

/// The path `artifact` occupies under `home`, unvalidated — containment is [`prune_within`]'s job.
fn artifact_path(home: &ToolchainHome, artifact: &RenderedArtifact) -> PathBuf {
    match artifact {
        RenderedArtifact::Trampoline(name) => home.shell_bin(DEFAULT_SHELL).join(name),
        RenderedArtifact::Link { group, entry } => links_root(home).join(group).join(entry),
        RenderedArtifact::GroupDirectory(group) => links_root(home).join(group),
        RenderedArtifact::RootEntry(name) => home.root().join(name),
    }
}

/// Write the render stamp, last; `true` when written. A failure is a skip, so the prompt gate
/// withholds. Heavily blocking: [`render_with`] runs it as one [`blocking`] unit.
fn write_render_stamp(
    state: &ocx_store::file_structure::StateStore,
    home: &ToolchainHome,
    scope: &RenderStampScope,
    bin_in_scope: bool,
    landed: &BTreeSet<String>,
) -> bool {
    let key = stamp_key(scope);
    let target = match scope {
        RenderStampScope::Global => RenderStampTarget::Global,
        RenderStampScope::Project(_) => RenderStampTarget::Project(&key),
    };

    // Without a `bin/` reconcile, carry the previous half forward rather than certify an unread directory.
    let bin_fingerprint = if bin_in_scope {
        // Physical directory: a fingerprint taken through `active` certifies wherever `active` points,
        // and `bin_matches_recorded` then passes by agreeing with it.
        fingerprint_bin(&home.shell_bin(DEFAULT_SHELL), landed)
    } else {
        state
            .render_stamp(target)
            .map(|previous| previous.bin_fingerprint)
            .unwrap_or_default()
    };

    let stamp = RenderStamp::new(
        home.root().to_path_buf(),
        scope.clone(),
        bin_fingerprint,
        observe_default_group_links(home),
    );
    match state.set_render_stamp(target, &stamp) {
        Ok(()) => true,
        Err(error) => {
            log::warn!(
                "Toolchain '{}' was rendered but its render stamp was not written: {error}",
                home.root().display()
            );
            false
        }
    }
}

/// One entry per landed on-disk file (Windows: `.exe` and `.exec` apart), via the same
/// [`BinEntryStamp::of_file`](ocx_store::file_structure::BinEntryStamp::of_file) the prompt gate checks.
fn fingerprint_bin(bin: &Path, landed: &BTreeSet<String>) -> BTreeMap<String, BinEntryStamp> {
    let mut fingerprint = BTreeMap::new();
    for name in landed {
        let path = bin.join(name);
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Some(stamp) = BinEntryStamp::of_file(&path, &metadata) else {
            continue;
        };
        fingerprint.insert(name.clone(), stamp);
    }
    fingerprint
}

/// The default group's links as they stand on disk. Observed, not derived from the lock, so a
/// `pinned` render still stamps a fingerprint matching the tree the gate reads.
fn observe_default_group_links(home: &ToolchainHome) -> BTreeMap<String, String> {
    let mut fingerprint = BTreeMap::new();
    let Ok(group_directory) = home.links_group(DEFAULT_GROUP) else {
        return fingerprint;
    };
    let Ok(entries) = std::fs::read_dir(group_directory) else {
        return fingerprint;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        fingerprint.insert(format!("{DEFAULT_GROUP}/{name}"), target.to_string_lossy().into_owned());
    }
    fingerprint
}

/// The file names one exposed name occupies in `bin/`: two on Windows, `.exe` plus its `.exec`.
fn trampoline_file_names(name: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.exec")]
    } else {
        vec![name.to_string()]
    }
}

/// Run one blocking unit off the runtime; a panicked or cancelled task is attributed to `context`.
async fn blocking<T, F>(context: PathBuf, work: F) -> crate::Result<T>
where
    F: FnOnce() -> crate::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(join_error) => Err(crate::error::file_error(&context, std::io::Error::other(join_error))),
    }
}

/// What one [`heal_links`] call did. `Healed` and `Refused` must stay distinct (hence `#[must_use]`):
/// collapsing them lets a symlinked `.ocx/toolchain` refuse every write, then be read into `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "a refused tree was never entered — reading through it composes an attacker's links"]
pub(crate) enum HealOutcome {
    /// The tree was walked and this many links repointed; per-entry failures stay uncounted here.
    Healed(usize),
    /// A whole-tree gate refused (a symlink guard, or the root creation); nothing under `home` was
    /// read or written, so nothing under it may be trusted.
    Refused {
        /// Why, as `debug` prose; nothing parses it.
        reason: String,
    },
}

/// Repoint every stale or absent `<group>/<entry>` link in `groups` at the digest root `lock` names,
/// under [`lock_scoped`](ocx_util::fs::lock_scoped). `groups` must list every group the caller
/// composes, or a stale link in an omitted group survives a branch switch. An unrepairable link is
/// left alone, uncounted; `file_structure` and `platform` are passed in (`subsystem-file-structure.md`).
///
/// # Errors
///
/// [`PackageErrorKind::ToolchainPath`] for a group or tool name that cannot be a path component
/// (exit 78).
pub(crate) async fn heal_links(
    file_structure: &FileStructure,
    home: &ToolchainHome,
    scope: &RenderStampScope,
    lock: &ProjectLock,
    groups: &[String],
    platform: &ocx_oci::Platform,
) -> Result<HealOutcome, PackageErrorKind> {
    // An empty set does no I/O, so a home that does not exist yet is not created for it.
    if groups.is_empty() {
        return Ok(HealOutcome::Healed(0));
    }

    // Both render gates, as one blocking unit: heal writes, so without them a clone symlinking
    // `.ocx/toolchain` to `$HOME` is written outside the project on every prompt.
    let guarded = ToolchainHome::new(home.root().to_path_buf());
    let guarded_scope = scope.clone();
    match blocking(home.root().to_path_buf(), move || {
        refuse_symlinked_project_path(&guarded_scope, &guarded)?;
        ensure_home_root(&guarded)
    })
    .await
    {
        Ok(_created) => {
            // After the root exists, so this write cannot create it under the umask; heal creates homes
            // on a fresh clone too, and without it a first `ocx env` leaves the links ungitignored.
            let gitignore_home = ToolchainHome::new(home.root().to_path_buf());
            if let Err(error) = blocking(home.root().to_path_buf(), move || {
                gitignore_home.ensure_gitignore().map_err(Into::into)
            })
            .await
            {
                log::debug!("Toolchain home '{}' has no ignore file: {error}", home.root().display());
            }
        }
        // Not an error, but not a clean pass: the caller must not read through a tree never entered.
        Err(error) => {
            log::debug!(
                "Toolchain links under '{}' were not healed: {error}",
                home.root().display()
            );
            return Ok(HealOutcome::Refused {
                reason: error.to_string(),
            });
        }
    }

    // Every candidate is named, and grammar-checked, before any path is probed.
    let selected: BTreeSet<&str> = groups.iter().map(String::as_str).collect();
    let mut candidates: Vec<(&str, PathBuf, PathBuf)> = Vec::new();
    for group in &selected {
        for tool in lock.tools.iter().filter(|tool| tool.group == **group) {
            // A bad `ocx.lock` name is refused before any path is touched — an error, not a degrade.
            let entry = home.entry(&tool.group, &tool.name)?;

            // The render's `select_best` rule, so render and heal never oscillate; no compatible key skips.
            let Some(target) = link_target(file_structure, tool, platform) else {
                continue;
            };
            candidates.push((tool.name.as_str(), entry, target));
        }
    }

    // One blocking unit for every probe, not one per tool: this is on `ocx env`'s per-prompt path.
    let probes: Vec<(PathBuf, PathBuf)> = candidates
        .iter()
        .map(|(_, entry, target)| (entry.clone(), target.clone()))
        .collect();
    let repairable = blocking(home.root().to_path_buf(), move || {
        Ok(probes
            .into_iter()
            .map(|(entry, target)| {
                // Against the lock's target, never a canonicalising read into a hostile tree.
                if std::fs::read_link(&entry).is_ok_and(|current| current == target) {
                    return false;
                }
                // Anything else at the name stays: heal has no delete authority in a repository's tree.
                !entry.exists() || ocx_util::fs::symlink::is_link(&entry)
            })
            .collect::<Vec<bool>>())
    })
    .await
    // A join failure makes the whole batch unrepairable — an uncounted degrade.
    .unwrap_or_else(|_| vec![false; candidates.len()]);

    let mut repaired = 0usize;
    for ((name, entry, target), repairable) in candidates.iter().zip(repairable) {
        if !repairable {
            continue;
        }
        match repoint_link(file_structure, name, entry, target).await {
            Ok(()) => repaired += 1,
            // Uncounted, never an error; the composing side degrades the entry to a digest path.
            Err(error) => log::debug!("Toolchain link '{}' was not repaired: {error}", entry.display()),
        }
    }
    Ok(HealOutcome::Healed(repaired))
}

/// Repoint one `<group>/<entry>` at `target` under its own lock.
///
/// # Errors
///
/// A symlinked `links/` or `<group>/` ([`ensure_link_group`]), its creation, the lock, or the replace;
/// [`heal_links`] counts each as one unrepaired entry, never a failure.
async fn repoint_link(file_structure: &FileStructure, name: &str, entry: &Path, target: &Path) -> crate::Result<()> {
    // `entry`'s parent by construction; re-joining the group name is a second spelling that drifted once.
    let Some(group_directory) = entry.parent().map(Path::to_path_buf) else {
        return Err(refuse_escape(entry));
    };

    // An absent link's group directory may not exist: create it owner-only and never through a
    // symlinked level, on the write path every composing emit takes.
    let created = group_directory.clone();
    blocking(group_directory.clone(), move || ensure_link_group(&created)).await?;

    let _guard = heal_lock_parameters(file_structure, group_directory, name)
        .acquire()
        .await?;
    let (entry, target) = (entry.to_path_buf(), target.to_path_buf());
    blocking(entry.clone(), move || {
        ocx_util::fs::symlink::replace_atomic(&target, &entry).map_err(Into::into)
    })
    .await
}

/// Create `home`'s root and missing ancestors owner-only at create time (a post-hoc `chmod` leaves a
/// rename window), and report whether this call created it. Every level: `toolchain_dir`'s
/// writability check judged only the nearest existing ancestor. A symlink at the root or a tree-own
/// directory (`active` exempt) is refused first, since `create_dir_all` follows one outside the home.
///
/// # Errors
///
/// The creation's own I/O failure or the symlink refusal; the renderer skips on either.
pub(crate) fn ensure_home_root(home: &ToolchainHome) -> crate::Result<bool> {
    let root = home.root();
    refuse_symlinked_home_leaves(home)?;
    let existed = match std::fs::symlink_metadata(root) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(crate::error::file_error(root, error)),
    };
    if !existed {
        create_owner_only(root)?;
    }
    Ok(!existed)
}

/// Refuse a symlink on any component from the project directory down through the home's tree-own
/// directories — `symlink_metadata` never judges an ancestor. Without it a committed
/// `.ocx/toolchain -> /` makes every `PATH` segment owned and deletable on each prompt
/// (`adr_toolchain_activation.md` § Rationale from code: render_toolchain).
///
/// # Errors
///
/// [`refuse_symlink`] naming the offending component.
pub(crate) fn refuse_symlinked_home(scope: &RenderStampScope, home: &ToolchainHome) -> crate::Result<()> {
    refuse_symlinked_project_path(scope, home)?;
    refuse_symlinked_home_leaves(home)
}

/// Refuse a symlink at the home root or a tree-own directory, outermost first, judging the physical
/// `shells/<shell>/bin` (through `active/bin` it would resolve the link). `links/` and `shells/` are
/// included because [`create_owner_only`] follows them; `active` must be a link.
fn refuse_symlinked_home_leaves(home: &ToolchainHome) -> crate::Result<()> {
    for path in tree_own_directories(home) {
        // `symlink_metadata`, never `metadata`, which follows the link.
        if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_symlink()) {
            return Err(refuse_symlink(&path));
        }
    }
    Ok(())
}

/// Every directory the rendered tree owns, outermost first, derived from the accessors rather than
/// re-joined names so the guard and the scan cannot drift.
fn tree_own_directories(home: &ToolchainHome) -> Vec<PathBuf> {
    let mut paths = vec![home.root().to_path_buf(), links_root(home)];
    // `ancestors` yields `bin`, `shells/<shell>`, `shells` first — never the root, already listed.
    let shell_bin = home.shell_bin(DEFAULT_SHELL);
    let mut chain: Vec<PathBuf> = shell_bin.ancestors().take(3).map(Path::to_path_buf).collect();
    chain.reverse();
    paths.extend(chain);
    paths
}

fn refuse_symlinked_project_path(scope: &RenderStampScope, home: &ToolchainHome) -> crate::Result<()> {
    let RenderStampScope::Project(project_directory) = scope else {
        return Ok(());
    };
    let Ok(relative) = home.root().strip_prefix(project_directory) else {
        return Ok(());
    };

    let mut current = project_directory.clone();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        current.push(component);
        // The home root itself is `ensure_home_root`'s to judge.
        if components.peek().is_none() {
            break;
        }
        if std::fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.is_symlink()) {
            return Err(refuse_symlink(&current));
        }
    }
    Ok(())
}

/// Create `root` and missing ancestors owner-only at create time, never by a post-hoc `chmod`.
#[cfg(unix)]
fn create_owner_only(root: &Path) -> crate::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    // `mode` applies to every level created, none of which `toolchain_dir`'s writability check saw.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)
        .map_err(|error| crate::error::file_error(root, error))
}

#[cfg(not(unix))]
fn create_owner_only(root: &Path) -> crate::Result<()> {
    // No umask or mode bits on Windows; the inherited ACL applies.
    std::fs::create_dir_all(root).map_err(|error| crate::error::file_error(root, error))
}

/// Create one directory owner-only without resolving a symlink at that name: `mkdir(2)` never follows
/// its final component, and `EEXIST` is re-judged by `symlink_metadata`. One call per level, or
/// `create_dir_all`'s symlink-following hole reopens. The window from `EEXIST` to the publish stays
/// open (closing it needs `openat` with `O_NOFOLLOW`).
///
/// # Errors
///
/// [`refuse_symlink`], a `NotADirectory` refusal for another occupant, or the create's own I/O failure.
fn create_directory_owner_only(directory: &Path) -> crate::Result<()> {
    // `mut` only inside the Unix arm: off Unix it is never mutated, and the denied `unused_mut` would
    // break the Windows build.
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();

    match builder.create(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // `symlink_metadata`: its `is_dir()` is false for a link, so one stat answers both.
            match std::fs::symlink_metadata(directory) {
                Ok(metadata) if metadata.is_dir() => Ok(()),
                Ok(metadata) if metadata.is_symlink() => Err(refuse_symlink(directory)),
                Ok(_) => Err(crate::error::file_error(
                    directory,
                    std::io::Error::new(
                        std::io::ErrorKind::NotADirectory,
                        "a rendered toolchain directory name is occupied by something that is not a directory",
                    ),
                )),
                Err(error) => Err(crate::error::file_error(directory, error)),
            }
        }
        Err(error) => Err(crate::error::file_error(directory, error)),
    }
}

/// Create `shells/` and `shells/<shell>/` one guarded level at a time — `create_dir_all` would follow
/// a committed symlink at either. `bin/` is [`reconcile_bin`]'s, or a `-g`-narrowed run would create it.
///
/// # Errors
///
/// Whatever [`create_directory_owner_only`] refuses.
fn ensure_shell_tree(home: &ToolchainHome, shell: &str) -> crate::Result<()> {
    // `shells/<shell>/bin`'s two ancestors below the root, outermost first.
    let shell_bin = home.shell_bin(shell);
    let mut levels: Vec<&Path> = shell_bin.ancestors().skip(1).take(2).collect();
    levels.reverse();
    for level in levels {
        create_directory_owner_only(level)?;
    }
    Ok(())
}

/// Create `links/` and `links/<group>/` one guarded level at a time — `create_dir_all` would follow a
/// `links` symlink swapped in during the render lock's wait.
///
/// # Errors
///
/// Whatever [`create_directory_owner_only`] refuses; non-recursive, so the home root must exist.
fn ensure_link_group(group_directory: &Path) -> crate::Result<()> {
    if let Some(links) = group_directory.parent() {
        create_directory_owner_only(links)?;
    }
    create_directory_owner_only(group_directory)
}

/// Bring `<root>/active` to its one legal shape by the kind observed: correct link, nothing; absent,
/// create; other link, `replace_atomic`; real directory (a dereferencing copy), `remove_dir_all` then
/// create; anything else, `remove_file` then create. The recursion is safe only because no untrusted
/// string contributes to the path (`adr_toolchain_activation.md` § "Defensive layout").
///
/// # Errors
///
/// The removal's or creation's own I/O failure; the caller warns and renders on.
fn heal_active(home: &ToolchainHome, shell: &str) -> crate::Result<()> {
    if home.active_is_valid(shell) {
        return Ok(());
    }
    let active = home.active();
    let target = home.expected_active_target(shell);

    // `symlink::is_link` first, never `Path::is_symlink`: a Windows junction is no symlink to std, and
    // `is_dir()` would otherwise claim it.
    if ocx_util::fs::symlink::is_link(&active) {
        log::debug!(
            "Toolchain link '{}' is repointed at its derived target",
            active.display()
        );
        return ocx_util::fs::symlink::replace_atomic(&target, &active).map_err(Into::into);
    }
    match std::fs::symlink_metadata(&active) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(crate::error::file_error(&active, error)),
        Ok(metadata) => {
            if metadata.is_dir() {
                std::fs::remove_dir_all(&active).map_err(|error| crate::error::file_error(&active, error))?;
            } else {
                // `remove_file`, never a rename over it, which would drop the bytes unobserved.
                std::fs::remove_file(&active).map_err(|error| crate::error::file_error(&active, error))?;
            }
            log::debug!("Toolchain link '{}' is replaced by its derived link", active.display());
        }
    }
    ocx_util::fs::symlink::create(&target, &active).map_err(Into::into)
}

/// A rendered toolchain path that is a symlink, refused on both the write and the delete side.
fn refuse_symlink(path: &Path) -> crate::Error {
    crate::error::file_error(
        path,
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a rendered toolchain path is a symlink; refusing to write or prune through it",
        ),
    )
}

/// A name holding something other than a link: ocx removes only what it wrote, so a foreign file or a
/// dereferenced copy is reported, never deleted — at every level, not just `<group>/<entry>`.
fn refuse_not_a_link(path: &Path) -> crate::Error {
    crate::error::file_error(
        path,
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "this name holds something other than a link ocx wrote; refusing to remove it",
        ),
    )
}

/// An artifact whose resolved path is not inside its resolved home.
fn refuse_escape(path: &Path) -> crate::Error {
    crate::error::file_error(
        path,
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a rendered toolchain artifact does not resolve inside its home; refusing to remove it",
        ),
    )
}

/// The case-probe file stem; one constant, since a stem drifted from [`is_leaked_case_probe`] would
/// leak a probe the prune then skips on every render.
const CASE_PROBE_PREFIX: &str = ".ocx-case-probe-";

/// Whether `name` is a leaked case probe: the stem in either ASCII case, then only digits and `-`.
/// Anything else is not ours, and [`prune_within`] will not delete it.
fn is_leaked_case_probe(name: &str) -> bool {
    let (name, prefix) = (name.as_bytes(), CASE_PROBE_PREFIX.as_bytes());
    name.len() > prefix.len()
        && name[..prefix.len()].eq_ignore_ascii_case(prefix)
        && name[prefix.len()..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || *byte == b'-')
}

/// Whether `name` is a file a file manager drops into any directory it opens: macOS `.DS_Store` and
/// `._*` AppleDouble files, Windows `desktop.ini` and `Thumbs.db`, KDE `.directory`. ASCII case-folded,
/// as the volumes that carry them fold case.
// ponytail: fixed list, the set is small and stable; a config key if a new OS file shows up often.
fn is_os_metadata_file(name: &str) -> bool {
    name.starts_with("._")
        || [".DS_Store", "desktop.ini", "Thumbs.db", ".directory"]
            .iter()
            .any(|known| known.eq_ignore_ascii_case(name))
}

/// Whether the filesystem holding `directory` folds ASCII case — probed, since it is per-volume.
/// Writes into `directory`, which must exist; a dry run never calls it.
///
/// # Errors
///
/// The probe's own I/O failure, left to the caller: guessing either way merges or drops a `bin/` entry.
pub(crate) async fn filesystem_is_case_insensitive(directory: &Path) -> crate::Result<bool> {
    let directory = directory.to_path_buf();
    blocking(directory.clone(), move || {
        // Only digits and `-` follow the stem, so the spellings differ in case alone; pid and nanos keep
        // concurrent probes apart.
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos())
        );
        let lower = directory.join(format!("{CASE_PROBE_PREFIX}{suffix}"));
        let upper = directory.join(format!("{}{suffix}", CASE_PROBE_PREFIX.to_ascii_uppercase()));

        std::fs::File::create(&lower).map_err(|error| crate::error::file_error(&lower, error))?;
        let answer = std::fs::symlink_metadata(&upper).is_ok();
        // Ignored: the next render's home-root arm prunes a leaked probe (`is_leaked_case_probe`).
        let _ = std::fs::remove_file(&lower);
        Ok(answer)
    })
    .await
}

/// Remove one artifact from inside `home` and nowhere else — the module's only delete path. A
/// symlinked root is refused, then the parent is checked with both sides canonicalised
/// (`adr_toolchain_activation.md` § "Rationale from code: render_toolchain").
///
/// # Errors
///
/// The removal's own I/O failure, or a containment refusal; the caller reports either as `Skipped`.
fn prune_within(home: &ToolchainHome, artifact: &RenderedArtifact) -> crate::Result<()> {
    let (parent, name) = match artifact {
        RenderedArtifact::Trampoline(name) => (home.shell_bin(DEFAULT_SHELL), name.clone()),
        RenderedArtifact::Link { group, entry } => (links_root(home).join(group), entry.clone()),
        RenderedArtifact::GroupDirectory(group) => (links_root(home), group.clone()),
        RenderedArtifact::RootEntry(name) => (home.root().to_path_buf(), name.clone()),
    };

    // Canonicalising cannot catch a symlinked root: both sides resolve alike and removals land outside.
    if std::fs::symlink_metadata(home.root()).is_ok_and(|metadata| metadata.is_symlink()) {
        return Err(refuse_symlink(home.root()));
    }

    // The name as `read_dir` gave it, not via `ToolchainHome::entry`, which would leave a hostile `C:`
    // or `a\b` group unprunable; it must be one component, as Windows `join` discards the base on `C:`.
    if !is_single_component(&name) {
        return Err(refuse_escape(&parent.join(&name)));
    }

    // Resolved, never a lexical `starts_with`: a symlinked `bin/` or `<group>/` keeps the prefix.
    let resolved_home =
        dunce::canonicalize(home.root()).map_err(|error| crate::error::file_error(home.root(), error))?;
    let resolved_parent = dunce::canonicalize(&parent).map_err(|error| crate::error::file_error(&parent, error))?;
    if !resolved_parent.starts_with(&resolved_home) {
        return Err(refuse_escape(&parent.join(&name)));
    }

    let victim = resolved_parent.join(&name);
    match artifact {
        RenderedArtifact::Trampoline(_) => {
            std::fs::remove_file(&victim).map_err(|error| crate::error::file_error(&victim, error))
        }
        // `symlink::remove` silently deletes a regular file at the name too, so a non-link is refused.
        RenderedArtifact::Link { .. } => {
            if !std::fs::symlink_metadata(&victim).is_ok_and(|metadata| metadata.is_symlink()) {
                return Err(refuse_not_a_link(&victim));
            }
            ocx_util::fs::symlink::remove(&victim).map_err(Into::into)
        }
        RenderedArtifact::GroupDirectory(_) | RenderedArtifact::RootEntry(_) => {
            let metadata =
                std::fs::symlink_metadata(&victim).map_err(|error| crate::error::file_error(&victim, error))?;
            if metadata.is_symlink() {
                ocx_util::fs::symlink::remove(&victim).map_err(Into::into)
            } else if metadata.is_dir() {
                // Never `remove_dir_all` on this attacker-controlled directory: a non-empty one fails
                // `ENOTEMPTY`, contents intact.
                std::fs::remove_dir(&victim).map_err(|error| crate::error::file_error(&victim, error))
            } else if is_leaked_case_probe(&name) {
                // Only a leaked probe gets `remove_file`; `remove_dir` would skip it on every render.
                std::fs::remove_file(&victim).map_err(|error| crate::error::file_error(&victim, error))
            } else {
                // Anything else refuses: `remove_file` here deletes a user's file with no warn line.
                Err(refuse_not_a_link(&victim))
            }
        }
    }
}

/// Whether `name` is one ordinary component to this platform's parser — the one the removal's
/// `join` uses, where `a\b` and `C:` differ by platform.
fn is_single_component(name: &str) -> bool {
    let mut components = Path::new(name).components();
    let first_is_the_whole_name =
        matches!(components.next(), Some(std::path::Component::Normal(value)) if value == std::ffi::OsStr::new(name));
    first_is_the_whole_name && components.next().is_none()
}

/// The selector every trampoline bakes, from the scope, never the home: `toolchain_dir` relocates
/// the home but not the baked `--project` root.
fn trampoline_target(scope: &RenderStampScope) -> TrampolineTarget {
    match scope {
        RenderStampScope::Global => TrampolineTarget::Global,
        RenderStampScope::Project(root) => TrampolineTarget::Project(root.clone()),
    }
}

/// Atomically publish one POSIX trampoline at `path`, executable. Blocking.
///
/// # Errors
///
/// The write's, permission change's or rename's own I/O failure.
fn write_trampoline_atomic(path: &Path, body: &str) -> crate::Result<()> {
    use std::io::Write as _;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut staged =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| crate::error::file_error(parent, error))?;
    staged
        .write_all(body.as_bytes())
        .map_err(|error| crate::error::file_error(path, error))?;
    staged.flush().map_err(|error| crate::error::file_error(path, error))?;

    // Mode on the temp file, before the rename, so `bin/<name>` is never on PATH non-executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        staged
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o755))
            .map_err(|error| crate::error::file_error(path, error))?;
    }

    ocx_util::fs::persist_temp_file(staged, path).map_err(|error| crate::error::file_error(path, error))
}

/// Publish one Windows trampoline: the `.exec` sidecar first, then the `.exe` as a hardlink of the
/// blob. Only `.exec`-without-`.exe` is recoverable — the reverse is live on `PATH` and fails to run
/// (`adr_toolchain_activation.md` § "Rationale from code: render_toolchain").
///
/// # Errors
///
/// The sidecar write's, blob publish's or hardlink's own I/O failure.
async fn publish_windows_trampoline(
    shim_bin: &ShimBinStore,
    exe_path: &Path,
    sidecar_path: &Path,
    sidecar_body: &str,
) -> crate::Result<()> {
    let sidecar_target = sidecar_path.to_path_buf();
    let sidecar_bytes = sidecar_body.as_bytes().to_vec();
    blocking(sidecar_path.to_path_buf(), move || {
        ocx_util::fs::write_bytes_atomic(&sidecar_target, &sidecar_bytes)
            .map_err(|error| crate::error::file_error(&sidecar_target, error))
    })
    .await?;

    let blob = shim_bin.ensure().await?;
    let exe_path = exe_path.to_path_buf();
    // `hardlink::update`, not `create`: a previous render's `.exe` may occupy the live slot.
    blocking(exe_path.clone(), move || {
        ocx_store::hardlink::update(&blob, &exe_path).map_err(Into::into)
    })
    .await
}

/// Read an existing `bin/` entry for the `Unchanged` compare; `Ok(None)` means take the write
/// branch. Gated by `symlink_metadata` then [`read_bounded`](ocx_util::fs::read_bounded): a plain
/// read hangs on a planted FIFO, trusts a matching symlink, and reads a planted 200 MB file (CWE-400).
///
/// # Errors
///
/// The stat's or read's own I/O failure other than "absent".
fn read_existing_trampoline(path: &Path, cap: u64) -> crate::Result<Option<Vec<u8>>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(crate::error::file_error(path, error)),
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    match ocx_util::fs::read_bounded(path, cap) {
        Ok(bytes) => Ok(Some(bytes)),
        // Nothing comparable: over `cap` cannot equal the body, and a non-regular file is no trampoline.
        Err(ocx_util::fs::BoundedReadError::TooLarge { .. })
        | Err(ocx_util::fs::BoundedReadError::NotRegularFile { .. }) => Ok(None),
        Err(ocx_util::fs::BoundedReadError::Io { path, source }) => Err(crate::error::file_error(&path, source)),
    }
}

/// The points at which a render fault may be injected.
#[cfg(any(test, feature = "__testing"))]
enum RenderStage {
    /// After the first `bin/` entry has landed, before the stamp is written.
    AfterFirstEntryWrite,
    /// After `shells/<shell>/` exists, before `active` is healed.
    AfterShellTree,
}

/// Read `__OCX_TESTING_RENDER_FAULT` once, at render entry; empty reads as unset. Gated with the
/// seam (`subsystem-tests.md`), so a release build lacks the path.
#[cfg(any(test, feature = "__testing"))]
fn read_fault_hook() -> Option<String> {
    std::env::var_os("__OCX_TESTING_RENDER_FAULT")
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
}

/// Fail the render when `fault` names `stage`.
///
/// # Errors
///
/// A synthetic [`PackageErrorKind::Internal`] when `fault` is `stage`'s `FAULT_*` value.
#[cfg(any(test, feature = "__testing"))]
fn maybe_inject_fault(fault: Option<&str>, stage: RenderStage) -> Result<(), PackageErrorKind> {
    let requested = match stage {
        RenderStage::AfterFirstEntryWrite => FAULT_AFTER_FIRST_ENTRY_WRITE,
        RenderStage::AfterShellTree => FAULT_AFTER_SHELL_TREE,
    };
    if fault == Some(requested) {
        return Err(PackageErrorKind::Internal(crate::error::file_error(
            Path::new(requested),
            std::io::Error::other("__OCX_TESTING_RENDER_FAULT aborted the render"),
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;
    use ocx_index::{ChainMode, Index, LocalConfig, LocalIndex};
    use ocx_package::metadata::visibility::Visibility;
    use ocx_package::metadata::{Binaries, BinaryName, EntrypointName};
    use ocx_project::{DECLARATION_HASH_VERSION, DEFAULT_GROUP, LockMetadata, LockVersion, LockedTool};
    use ocx_store::file_structure::{RenderStamp, RenderStampTarget};
    use ocx_store::reference_manager::ReferenceManager;

    // ── Fixtures ─────────────────────────────────────────────────────────────
    //
    // Everything here is built from a `tempfile::TempDir`: no ambient
    // `$OCX_HOME`, no registry, no client. `render_with` and `heal_links` take
    // the `FileStructure` as a parameter precisely so that holds.

    const REGISTRY: &str = "example.com";

    /// The platform every request in this module carries.
    ///
    /// A fixed value rather than the host's: `RenderRequest::platform` is a
    /// parameter, so a host-derived one would make every link assertion answer
    /// differently on the Windows leg for a reason that has nothing to do with
    /// the contract under test.
    const PLATFORM_KEY: &str = "linux/amd64";

    fn platform() -> ocx_oci::Platform {
        PLATFORM_KEY.parse().expect("the fixture platform key is canonical")
    }

    fn digest_of(seed: char) -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256(seed.to_string().repeat(64))
    }

    fn pinned(repository: &str, seed: char) -> ocx_oci::PinnedPackageRef {
        ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry(repository, REGISTRY).clone_with_digest(digest_of(seed)),
        )
        .expect("a digest-bearing identifier is pinned")
    }

    fn binary_name(value: &str) -> BinaryName {
        BinaryName::try_from(value).expect("the fixture binary name is valid")
    }

    fn binaries(names: &[&str]) -> Binaries {
        let set: BTreeSet<BinaryName> = names.iter().map(|n| binary_name(n)).collect();
        Binaries::try_from(set).expect("the fixture binaries claim is valid")
    }

    fn entrypoints(names: &[&str]) -> Vec<EntrypointName> {
        names
            .iter()
            .map(|n| EntrypointName::try_from((*n).to_string()).expect("the fixture entrypoint name is valid"))
            .collect()
    }

    /// A **root** closure node carrying only the two claim axes the name set is
    /// derived from; every other field is this axis's inert value.
    fn node(identifier: ocx_oci::PinnedPackageRef, claimed: Option<&[&str]>, entries: &[&str]) -> ClosureNode {
        ClosureNode {
            config_digest: identifier.digest(),
            identifier,
            effective_visibility: None,
            binaries: claimed.map(binaries),
            entrypoints: entrypoints(entries),
            env: Vec::new(),
            integrations: Vec::new(),
            dependencies: Vec::new(),
            is_root: true,
        }
    }

    /// A **dependency** node carrying an explicit effective visibility — the
    /// field `admitted_on_surface` gates on (C-023).
    fn dependency(
        identifier: ocx_oci::PinnedPackageRef,
        claimed: Option<&[&str]>,
        effective: Visibility,
    ) -> ClosureNode {
        let mut node = node(identifier, claimed, &[]);
        node.is_root = false;
        node.effective_visibility = Some(effective);
        node
    }

    /// The publisher asserting **zero** executables, distinct from `None`
    /// ("no claim at all"). Named because `Some(&[])` has no inferable element
    /// type at the call site.
    const ASSERTED_EMPTY: &[&str] = &[];

    fn locked_tool(name: &str, group: &str, repository: &str, platforms: &[(&str, char)]) -> LockedTool {
        LockedTool {
            name: name.to_string(),
            group: group.to_string(),
            repository: ocx_oci::Repository::new(REGISTRY, repository),
            platforms: platforms
                .iter()
                .map(|(key, seed)| ((*key).to_string(), digest_of(*seed)))
                .collect::<BTreeMap<String, ocx_oci::Digest>>(),
        }
    }

    fn lock_of(tools: Vec<LockedTool>) -> ProjectLock {
        ProjectLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V3,
                declaration_hash_version: DECLARATION_HASH_VERSION,
                declaration_hash: "0".repeat(64),
                generated_by: "wp-7 specification fixture".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            tools,
        }
    }

    fn groups_of(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    /// The digest root a `<group>/<entry>` link must name for `tool`, derived
    /// the way the contract says the renderer derives it — through the shared
    /// `select_best` helper (RUL-31), never by an exact key lookup.
    fn expected_link_target(file_structure: &FileStructure, tool: &LockedTool) -> PathBuf {
        let leaf = tool
            .host_leaf(&platform())
            .expect("the fixture lock ships a leaf compatible with the fixture platform");
        let pinned = tool.repository.pin_untagged(leaf);
        file_structure.packages.path(&pinned)
    }

    /// One project tree under one tempdir: an `$OCX_HOME`, a project directory
    /// and the home the renderer writes into.
    struct Tree {
        tmp: TempDir,
        file_structure: FileStructure,
        project_dir: PathBuf,
        home: ToolchainHome,
        /// The absolute `ocx` every golden body must bake — seeded, never
        /// inferred. See [`Tree::new`].
        // Read only by the `#[cfg(unix)]` trampoline-body assertions and by
        // `expected_unix_trampoline_body` beside them; the Windows arm asserts
        // over the `.exec` sidecar instead. `expect`, so this line fails the
        // day a Windows test reads it.
        #[cfg_attr(
            not(unix),
            expect(dead_code, reason = "read only by cfg(unix) trampoline-body tests")
        )]
        ocx_binary: PathBuf,
    }

    impl Tree {
        /// A tree whose rung-1 `ocx` **exists** (C-1, case 43).
        ///
        /// `launcher::generate::trampoline_ocx_binary`'s ladder has two rungs:
        /// the store-derived install path, then `std::env::current_exe()`. A
        /// fixture that controls only the `FileStructure` root and leaves the
        /// install path absent does not get the bare-name form — it falls to
        /// rung 2 and bakes the **test binary's own absolute path**, which
        /// varies by machine, by cargo profile and by the target-directory
        /// hash. Every golden in this module would then be stable locally and
        /// unstable on CI. Seeding a real file here is what makes rung 1 win
        /// deterministically.
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("a tempdir is creatable");
            let file_structure = FileStructure::with_root(tmp.path().join("ocx-home"));
            let project_dir = tmp.path().join("proj");
            std::fs::create_dir_all(&project_dir).expect("the project directory is creatable");

            let ocx_binary = file_structure
                .symlinks
                .current(&ocx_oci::ocx_cli_identifier())
                .join("content")
                .join("bin")
                .join(if cfg!(windows) { "ocx.exe" } else { "ocx" });
            std::fs::create_dir_all(ocx_binary.parent().expect("the binary has a parent"))
                .expect("the install tree is creatable");
            std::fs::write(&ocx_binary, b"#!/bin/sh\n").expect("the seeded ocx is writable");

            let home = ToolchainHome::new(project_dir.join(".ocx").join("toolchain"));
            Self {
                tmp,
                file_structure,
                project_dir,
                home,
                ocx_binary,
            }
        }

        fn scope(&self) -> RenderStampScope {
            RenderStampScope::Project(self.project_dir.clone())
        }

        fn key(&self) -> String {
            ReferenceManager::name_for_path(&self.project_dir)
        }

        fn stamp(&self) -> Option<RenderStamp> {
            self.file_structure
                .state
                .render_stamp(RenderStampTarget::Project(&self.key()))
        }

        fn stamp_file(&self) -> PathBuf {
            self.file_structure.state.render_stamp_file(&self.key())
        }

        /// `render_with`'s documented precondition: steps 2–6 have run — the
        /// home root and `shells/<shell>/` exist and are real directories, and
        /// `active` is the link they render. Every `render_with` caller in this
        /// module establishes it; `render_toolchain` callers do not, because
        /// establishing it is what they are testing.
        ///
        /// `shells/<shell>/bin` itself is deliberately **not** created — that
        /// is `reconcile_bin`'s, and only when the render has a name to put in
        /// it (RUL-25).
        fn create_home_root(&self) {
            seed_rendered_home(&self.home);
        }

        /// `<root>/shells/<shell>` — `shell_bin`'s parent.
        fn shell_directory(&self) -> PathBuf {
            shell_directory_of(&self.home)
        }

        /// Whether this home can hold `make` and `Make` as **two** files —
        /// read through the *production* probe, so a row and `render_with`
        /// cannot disagree about what "case-sensitive" means.
        ///
        /// The premise of every row that seeds a differently-cased twin. A
        /// macOS runner's APFS and every Windows volume are case-insensitive by
        /// default, so the twin collapses into the entry it was meant to
        /// shadow, the injected `case_insensitive` flag stops describing the
        /// disk it is asserted against, and the fixture cannot build its own
        /// precondition. Skip, do not weaken.
        ///
        /// Requires the home root to exist ([`Self::create_home_root`]) — the
        /// probe writes into the directory it judges.
        async fn holds_case_twins(&self) -> bool {
            !filesystem_is_case_insensitive(self.home.root())
                .await
                .expect("the case-fold probe answers for a home this test just created")
        }

        /// An offline manager over this tree's store — no client, no sources.
        fn manager(&self) -> PackageManager {
            let index = Index::from_chained(
                LocalIndex::new(LocalConfig {
                    index_store: ocx_index::IndexStore::machine_local(&self.file_structure),
                }),
                Vec::new(),
                ChainMode::Offline,
            );
            PackageManager::new(self.file_structure.clone(), index, None, REGISTRY)
        }

        /// The on-disk names in the **physical** trampoline directory, sorted.
        /// Absent reads as empty, which is the state a render that never
        /// touched it leaves.
        ///
        /// `shell_bin`, never `bin()`: reading through `active` would make
        /// every assertion below agree with whatever that link points at, which
        /// is the property half this module's guards exist to defend.
        fn bin_entries(&self) -> Vec<String> {
            read_dir_names(&self.home.shell_bin(DEFAULT_SHELL))
        }
    }

    /// Steps 2 and 6 of the render order, for **any** home — the root and
    /// `shells/<shell>/` as real directories, `active` as the link they render.
    ///
    /// A free function rather than a `Tree` method because several rows render
    /// into a *second* home (`toolchain_dir` relocation, two project
    /// directories) that the fixture never wrapped.
    fn seed_rendered_home(home: &ToolchainHome) {
        std::fs::create_dir_all(shell_directory_of(home)).expect("the shell directory is creatable");
        ocx_util::fs::symlink::create(home.expected_active_target(DEFAULT_SHELL), home.active())
            .expect("the activation link is creatable");
    }

    /// `<root>/shells/<shell>` — `shell_bin`'s parent, spelled through the
    /// accessor so no fixture can drift from the tree it seeds.
    fn shell_directory_of(home: &ToolchainHome) -> PathBuf {
        home.shell_bin(DEFAULT_SHELL)
            .parent()
            .expect("the trampoline directory has a parent")
            .to_path_buf()
    }

    /// The on-disk entry names of `directory`, sorted; an absent directory
    /// reads as empty.
    fn read_dir_names(directory: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.expect("a readable directory entry").file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The on-disk file names one exposed `name` occupies in `bin/`.
    ///
    /// One on POSIX; **two** on Windows — `<name>.exe` plus its `<name>.exec`
    /// sidecar (RUL-26). Every count and set assertion in this module goes
    /// through here rather than through a bare `name`, so a Windows-only
    /// off-by-one cannot pass on the POSIX leg.
    fn trampoline_files(name: &str) -> Vec<String> {
        if cfg!(windows) {
            vec![format!("{name}.exe"), format!("{name}.exec")]
        } else {
            vec![name.to_string()]
        }
    }

    /// The sorted `bin/` name set for a whole exposed-name set.
    fn expected_bin_entries(names: &[&str]) -> Vec<String> {
        let mut all: Vec<String> = names.iter().flat_map(|n| trampoline_files(n)).collect();
        all.sort();
        all
    }

    fn trampoline(name: &str) -> RenderedArtifact {
        RenderedArtifact::Trampoline(name.to_string())
    }

    fn link(group: &str, entry: &str) -> RenderedArtifact {
        RenderedArtifact::Link {
            group: group.to_string(),
            entry: entry.to_string(),
        }
    }

    fn group_directory(group: &str) -> RenderedArtifact {
        RenderedArtifact::GroupDirectory(group.to_string())
    }

    /// The outcome the report recorded for `artifact`, failing with the whole
    /// report when it is absent — a red then names what *was* reported instead
    /// of just "None".
    #[track_caller]
    fn outcome_of<'a>(report: &'a RenderReport, artifact: &RenderedArtifact) -> &'a RenderOutcome {
        report
            .items
            .iter()
            .find(|item| &item.artifact == artifact)
            .map(|item| &item.outcome)
            .unwrap_or_else(|| panic!("{artifact:?} must appear in the report; the report was {report:?}"))
    }

    fn is_skipped(outcome: &RenderOutcome) -> bool {
        matches!(outcome, RenderOutcome::Skipped { .. })
    }

    /// The POSIX trampoline body the renderer must produce for a project home.
    ///
    /// Restated here rather than called through
    /// `launcher::body::unix_trampoline_body`: a golden that asks the producer
    /// what it produces cannot tell a correct body from a changed one, and the
    /// producer's own goldens already pin it from the other side (C-034's
    /// paired-golden pattern). The marker is imported rather than spelled,
    /// because it is a *shared* constant the reader (`env::is_ocx_trampoline`)
    /// also reads — case 43(d).
    // Every caller is a `#[cfg(unix)]` test — the POSIX trampoline is a shell
    // script, and Windows renders an `.exe` plus a sidecar instead.
    #[cfg(unix)]
    fn expected_unix_trampoline_body(project_root: &Path, ocx_binary: &Path) -> String {
        let marker = ocx_config::env::TRAMPOLINE_MARKER;
        format!(
            "#!/bin/sh\n\
             {marker}\n\
             unset OCX_GLOBAL OCX_PROJECT\n\
             __ocx_binary='{binary}'\n\
             exec \"${{OCX_BINARY_PIN:-$__ocx_binary}}\" --project '{root}' exec -- \"${{0##*/}}\" \"$@\"\n",
            binary = ocx_binary.display(),
            root = project_root.display(),
        )
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::symlink_metadata(path)
            .unwrap_or_else(|e| panic!("{path:?} must exist: {e}"))
            .permissions()
            .mode()
    }

    #[cfg(unix)]
    fn inode_of(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::symlink_metadata(path)
            .unwrap_or_else(|e| panic!("{path:?} must exist: {e}"))
            .ino()
    }

    /// Make `directory` unwritable, and **prove the denial took**.
    ///
    /// A `chmod` is ignored for the superuser, so a test that merely set the
    /// bits and moved on would pass identically whether or not the code under
    /// test handles the failure — the green-that-never-ran shape. The probe
    /// write is what makes the precondition observed rather than assumed; it
    /// fails loudly under `root` instead of skipping silently.
    #[cfg(unix)]
    #[track_caller]
    fn deny_writes(directory: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o500))
            .unwrap_or_else(|e| panic!("{directory:?} must be chmod-able: {e}"));
        assert!(
            std::fs::write(directory.join("__write_probe"), b"x").is_err(),
            "precondition: writes into {directory:?} must actually be denied — under a uid that \
             ignores the mode bits this test would pass without exercising the skip path at all"
        );
    }

    #[cfg(unix)]
    fn allow_writes(directory: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
    }

    /// `mkfifo(2)`, the one filesystem object `std::fs` cannot create.
    ///
    /// Copied from `env.rs`'s and `oci/index/file_transport.rs`'s helpers
    /// rather than shared: three `#[cfg(test)]` modules in different
    /// subsystems, and the crate has no test-support home for a three-line
    /// libc call.
    #[cfg(unix)]
    #[track_caller]
    fn mkfifo(path: &Path) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;

        let c_path = CString::new(path.as_os_str().as_bytes()).expect("a tempdir path holds no NUL");
        // SAFETY: `c_path` is a NUL-terminated C string alive for the whole
        // call, and `mkfifo` only reads it.
        let created = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
        assert_eq!(created, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
    }

    /// One filesystem object, identified by everything a "wrote nothing" claim
    /// has to survive.
    ///
    /// Bytes alone are not enough: rewriting a file with identical content is
    /// a write, and so is re-creating it. The inode catches the re-creation
    /// and the nanosecond mtime catches the rewrite — both are the shapes a
    /// bytes-only or count-only assertion reads as "unchanged" (case 66).
    #[cfg(unix)]
    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct SubtreeEntry {
        relative: PathBuf,
        kind: &'static str,
        bytes: Option<Vec<u8>>,
        /// A symlink's **raw** target, `None` for every other kind (C-079).
        ///
        /// Without it every "the render wrote nothing" and "two renders are
        /// identical" assertion built on this helper is blind to a repoint:
        /// `active` swung from one directory to another keeps its kind, its
        /// inode and its mtime, so the snapshot compares equal to a tree whose
        /// one PATH-facing link now points somewhere else entirely.
        target: Option<PathBuf>,
        mtime_nsec: i64,
        inode: u64,
    }

    /// Every object under `root`, recursively, as [`SubtreeEntry`] values —
    /// sorted, so two snapshots compare as sets rather than as walk orders.
    /// An absent `root` snapshots as the empty vector, which is what makes
    /// "the dry run created no home at all" expressible.
    #[cfg(unix)]
    fn snapshot_subtree(root: &Path) -> Vec<SubtreeEntry> {
        use std::os::unix::fs::MetadataExt as _;

        fn walk(root: &Path, current: &Path, out: &mut Vec<SubtreeEntry>) {
            let Ok(entries) = std::fs::read_dir(current) else {
                return;
            };
            for entry in entries {
                let entry = entry.expect("a readable directory entry");
                let path = entry.path();
                let metadata = std::fs::symlink_metadata(&path).expect("a stat-able entry");
                let kind = if metadata.is_symlink() {
                    "symlink"
                } else if metadata.is_dir() {
                    "dir"
                } else {
                    "file"
                };
                out.push(SubtreeEntry {
                    relative: path
                        .strip_prefix(root)
                        .expect("every walked path is under the walk root")
                        .to_path_buf(),
                    kind,
                    bytes: if kind == "file" {
                        std::fs::read(&path).ok()
                    } else {
                        None
                    },
                    target: if kind == "symlink" {
                        std::fs::read_link(&path).ok()
                    } else {
                        None
                    },
                    mtime_nsec: metadata.mtime_nsec(),
                    inode: metadata.ino(),
                });
                if kind == "dir" {
                    walk(root, &path, out);
                }
            }
        }

        let mut out = Vec::new();
        if root.exists() {
            walk(root, root, &mut out);
        }
        out.sort();
        out
    }

    // ── 1. The name set — boundaries (C-021…C-025, C-044, C-045) ────────────

    /// C-044, case 1 — an **empty** computed set over a **non-empty** tree is
    /// the prune pass's whole job: every seeded entry reports `Pruned` and
    /// `bin/` ends empty.
    ///
    /// RED: early-return on an empty name set, skipping the prune pass.
    #[tokio::test]
    async fn an_empty_surface_prunes_every_entry_already_in_bin() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("cmake"), b"#!/bin/sh\n").unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("ctest"), b"#!/bin/sh\n").unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("an empty surface is an ordinary render, not a refusal");

        assert_eq!(
            outcome_of(&report, &trampoline("cmake")),
            &RenderOutcome::Pruned,
            "C-044 — a name no longer in the computed set is pruned"
        );
        assert_eq!(outcome_of(&report, &trampoline("ctest")), &RenderOutcome::Pruned);
        assert!(
            tree.bin_entries().is_empty(),
            "C-044 — and `bin/` is empty afterwards, not merely reported as pruned: {:?}",
            tree.bin_entries()
        );
    }

    /// C-044/C-048, case 2 — an empty surface over an empty tree still writes a
    /// stamp, carrying an **empty** `bin_fingerprint`.
    ///
    /// RED: gate the stamp on `!items.is_empty()`. C-061's gate then withholds
    /// forever on a legitimately toolless project, which is the permanent
    /// mismatch RUL-22/27 exist to prevent.
    #[tokio::test]
    async fn an_empty_render_still_writes_a_stamp_with_an_empty_fingerprint() {
        let tree = Tree::new();
        tree.create_home_root();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("a toolless project renders");

        assert_eq!(report.items, Vec::new(), "nothing to write and nothing to prune");
        assert!(
            report.stamp_written,
            "C-048 — the stamp is written even when the tree is empty"
        );
        let stamp = tree.stamp().expect("the stamp file must exist on disk");
        assert!(
            stamp.bin_fingerprint.is_empty(),
            "an empty tree stamps an empty fingerprint, not a withheld stamp"
        );
        assert!(stamp.names.is_empty());
    }

    /// C-022/C-045, case 3 — a root claiming neither `binaries` nor entry
    /// points contributes nothing and the render is `Ok`.
    ///
    /// RED: pass `NotEnumerablePolicy::Refuse` instead of `Skip` — an ordinary
    /// metadata-less package then hard-errors, which no contract sanctions.
    #[tokio::test]
    async fn a_root_claiming_no_names_contributes_nothing_and_does_not_refuse() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/plain", 'a'), None, &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("C-022 — the render path passes `Skip`, so an unenumerable node is ordinary");

        assert!(
            report.items.is_empty(),
            "the node contributes no trampoline: {report:?}"
        );
        assert!(tree.bin_entries().is_empty());
    }

    /// C-022, case 4 — `Some([])` ("the publisher asserted zero executables")
    /// is enumerable and distinct from `None`: it renders `Ok` and prunes
    /// nothing extra.
    ///
    /// RED: treat empty as absent in the consumer arm.
    #[tokio::test]
    async fn an_asserted_empty_binaries_claim_is_not_an_absent_claim() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/empty", 'a'), Some(ASSERTED_EMPTY), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("an asserted-empty claim renders");

        assert!(
            report.items.is_empty(),
            "no name is claimed, so no trampoline: {report:?}"
        );
        assert!(
            report.bin_in_scope,
            "RUL-25 — the default group was selected, so `bin/` was reconciled"
        );
    }

    /// C-021/C-023, case 5 — an interface-admitted dependency's claim lands in
    /// `bin/`; a private-only dependency's does not.
    ///
    /// RED: flip the surface flag on `admitted_on_surface` — the private
    /// dependency's name then appears on PATH.
    #[tokio::test]
    async fn an_interface_dependency_contributes_a_name_and_a_private_one_does_not() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![
            dependency(pinned("ns/exposed", 'a'), Some(&["exposed"]), Visibility::INTERFACE),
            dependency(pinned("ns/hidden", 'b'), Some(&["hidden"]), Visibility::PRIVATE),
            node(pinned("ns/root", 'c'), Some(&["root-tool"]), &[]),
        ];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["exposed", "root-tool"]),
            "C-023 — only the root and its interface-admitted dependency contribute names"
        );
    }

    /// C-013/C-044, cases 6 and 10 — a claimed name that collides with the
    /// home's own grammar word (`bin`) lands at `bin/bin`, a file, and leaves
    /// `ToolchainHome::bin()`'s directory itself untouched.
    ///
    /// Pinned rather than assumed: `bin` is a valid `BinaryName` and a
    /// reserved *component*, and the two rules are about different positions in
    /// the path. A future "validate bin entry names too" edit would silently
    /// change where this lands.
    #[tokio::test]
    async fn a_tool_named_bin_renders_inside_bin_and_not_at_the_home_root() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/bin", 'a'), Some(&["bin"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("`bin` is an ordinary claimed name");

        assert_eq!(tree.bin_entries(), expected_bin_entries(&["bin"]));
        assert!(
            tree.home.shell_bin(DEFAULT_SHELL).is_dir(),
            "C-013 — the trampoline directory itself stays a directory"
        );
        for file in trampoline_files("bin") {
            assert!(
                tree.home.shell_bin(DEFAULT_SHELL).join(&file).is_file(),
                "the claim lands at `bin/{file}`, never at the home root"
            );
            assert!(
                !tree.home.root().join(&file).is_file(),
                "…and nothing named {file} appears beside `bin/`"
            );
        }
    }

    /// C-025/D-V24/RUL-19, case 7 — on a case-insensitive home the twins fold
    /// to **one** entry, spelled as the greatest-`walk_index` winner's own
    /// spelling, in `bin/` **and** in `RenderStamp.names`.
    ///
    /// RED: pick the winner by `BTreeMap` key order (ASCII lowercase sorts
    /// above uppercase, so the all-lowercase spelling would always win), or
    /// lowercase the survivor.
    #[tokio::test]
    async fn case_twins_fold_to_the_last_walked_spelling_on_a_case_insensitive_home() {
        let tree = Tree::new();
        tree.create_home_root();

        // `make` is walked first, `Make` last — so `Make` is the winner and
        // `Make` is the spelling that must survive.
        let surface = vec![
            node(pinned("ns/first", 'a'), Some(&["make"]), &[]),
            node(pinned("ns/second", 'b'), Some(&["Make"]), &[]),
        ];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            true,
            None,
        )
        .await
        .expect("the fold is not a refusal");

        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["Make"]),
            "RUL-19 — one file, spelled as the last-walked claim spelled it"
        );
        let stamp = tree.stamp().expect("the stamp is written");
        assert_eq!(
            stamp.names.iter().cloned().collect::<Vec<_>>(),
            expected_bin_entries(&["Make"]),
            "…and the stamp records the same spelling, or C-061's readdir gate never matches"
        );
    }

    /// D-V18/R-W19(b), case 8 — on a **case-sensitive** home the fold must not
    /// run: `Make` and `make` are two tools and two files.
    ///
    /// RED: call `fold_case_insensitive` unconditionally — one tool silently
    /// disappears on Linux.
    #[tokio::test]
    async fn case_twins_stay_two_entries_on_a_case_sensitive_home() {
        let tree = Tree::new();
        tree.create_home_root();

        // The row's own precondition, read rather than presupposed. The logic
        // under test is platform-neutral (`case_insensitive` is driven `false`
        // below), but the two files it expects only stay two files on a
        // case-sensitive volume.
        if !tree.holds_case_twins().await {
            eprintln!(
                "skipped: the probe reported a case-insensitive home, so `make` and `Make` cannot be \
                 two files here"
            );
            return;
        }

        let surface = vec![
            node(pinned("ns/first", 'a'), Some(&["make"]), &[]),
            node(pinned("ns/second", 'b'), Some(&["Make"]), &[]),
        ];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("two distinct names render");

        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["Make", "make"]),
            "D-V18 — the fold runs only when the probe said the home is case-insensitive"
        );
        let stamp = tree.stamp().expect("the stamp is written");
        assert_eq!(stamp.names.len(), expected_bin_entries(&["Make", "make"]).len());
    }

    /// S-012/C-024/D-V19, case 9 — a package claiming the name `ocx` renders
    /// like any other name: exit 0, no refusal, and the body bakes an
    /// **absolute** ocx path so the trampoline cannot re-invoke itself.
    ///
    /// RED: restore the `ShimNameShadowsOcx` refusal (the ADR's own named
    /// mutation for validation item 16).
    #[tokio::test]
    async fn a_package_claiming_the_name_ocx_renders_without_a_refusal() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/ocx", 'a'), Some(&["ocx"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("S-012 — a project may pin its own ocx; this is never a refusal");

        assert_eq!(tree.bin_entries(), expected_bin_entries(&["ocx"]));
        assert!(
            report.items.iter().all(|item| !is_skipped(&item.outcome)),
            "…and never a skip either: {report:?}"
        );

        #[cfg(unix)]
        {
            let body = std::fs::read_to_string(tree.home.shell_bin(DEFAULT_SHELL).join("ocx")).unwrap();
            assert!(
                body.contains(&format!("__ocx_binary='{}'", tree.ocx_binary.display())),
                "D-V19 — the body re-enters ocx through the absolute install path, or a \
                 `bin/ocx` trampoline resolves itself through PATH: {body}"
            );
        }
    }

    /// RUL-25, case 11 — a `-g`-narrowed pull that does **not** select the
    /// default group leaves `bin/` entirely untouched and reports
    /// `bin_in_scope == false`.
    ///
    /// RED: derive `bin/`'s set from `request.groups`. A narrowed
    /// `ocx pull -g ci` then either empties `bin/` or resolves the default
    /// group's metadata the invocation never asked for.
    #[tokio::test]
    async fn a_narrowed_pull_that_excludes_the_default_group_does_not_touch_bin() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("cmake"), b"#!/bin/sh\nold\n").unwrap();

        let surface = vec![node(pinned("ns/other", 'a'), Some(&["other"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![locked_tool("ninja", "ci", "ns/ninja", &[(PLATFORM_KEY, 'd')])]);
        let groups = groups_of(&["ci"]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("a narrowed pull renders its own groups");

        assert!(
            !report.bin_in_scope,
            "RUL-25 — `bin/` was not in scope, and that is a field rather than an inference"
        );
        assert_eq!(
            tree.bin_entries(),
            vec!["cmake".to_string()],
            "…so the pre-existing entry is neither rewritten nor pruned"
        );
        assert_eq!(
            std::fs::read(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap(),
            b"#!/bin/sh\nold\n",
            "…byte-identical, not merely present"
        );
        assert!(
            report
                .items
                .iter()
                .all(|item| !matches!(item.artifact, RenderedArtifact::Trampoline(_))),
            "…and no trampoline item is reported at all: {report:?}"
        );
    }

    /// C-025, case 12 — one node claiming both case twins on its **own two
    /// axes** folds to one entry and never shadows itself.
    ///
    /// The render-observable half is the file count. The `NameOwner::shadowed`
    /// half is `toolchain_names.rs`'s (RUL-11) and is asserted there — a
    /// `RenderReport` carries no collision field (RUL-34), so this layer
    /// cannot see it.
    #[tokio::test]
    async fn one_node_claiming_both_case_twins_yields_one_entry() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/make", 'a'), Some(&["Make"]), &["make"])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            true,
            None,
        )
        .await
        .expect("a self-collision is never a refusal");

        assert_eq!(
            tree.bin_entries().len(),
            trampoline_files("make").len(),
            "one name survives the fold: {:?}",
            tree.bin_entries()
        );
    }

    // ── 2. Prune — validation item 31's four orphan classes (C-044) ─────────

    /// C-044, case 13 — an orphan **trampoline** (a `binaries` claim
    /// disappeared) is removed, and the `<group>/<entry>` link is untouched.
    ///
    /// RED: skip the `bin/` prune pass.
    #[tokio::test]
    async fn an_orphan_trampoline_is_pruned_and_its_group_link_is_left_alone() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("gone"), b"#!/bin/sh\n").unwrap();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(outcome_of(&report, &trampoline("gone")), &RenderOutcome::Pruned);
        assert_eq!(tree.bin_entries(), expected_bin_entries(&["cmake"]));
        let entry = tree
            .home
            .entry(DEFAULT_GROUP, "cmake")
            .expect("an admitted component pair");
        assert_eq!(
            std::fs::read_link(&entry).expect("the link exists"),
            expected_target,
            "C-044 — the link half of the tree is untouched by the trampoline prune"
        );
    }

    /// C-044, case 14 — an orphan **link** (its entry left the lock) takes its
    /// trampoline with it: both halves are pruned in one render.
    ///
    /// RED: prune only one of the two.
    #[tokio::test]
    async fn an_entry_that_left_the_lock_loses_both_its_link_and_its_trampoline() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("ninja"), b"#!/bin/sh\n").unwrap();
        let stale_entry = tree
            .home
            .entry(DEFAULT_GROUP, "ninja")
            .expect("an admitted component pair");
        std::fs::create_dir_all(stale_entry.parent().unwrap()).unwrap();
        std::fs::create_dir_all(tree.tmp.path().join("stale-package")).unwrap();
        ocx_util::fs::symlink::create(tree.tmp.path().join("stale-package"), &stale_entry).unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            outcome_of(&report, &link(DEFAULT_GROUP, "ninja")),
            &RenderOutcome::Pruned
        );
        assert_eq!(outcome_of(&report, &trampoline("ninja")), &RenderOutcome::Pruned);
        assert!(
            std::fs::symlink_metadata(&stale_entry).is_err(),
            "the link itself is gone, not merely reported"
        );
        assert!(tree.bin_entries().is_empty());
    }

    /// C-044, case 15 — a removed or renamed group leaves the whole
    /// `<group>/` **directory** orphaned, and it is reported as
    /// `RenderedArtifact::GroupDirectory` and removed.
    ///
    /// RED: leave the emptied directory behind — validation item 31's fourth
    /// orphan class then has no representation and no coverage.
    #[tokio::test]
    async fn a_group_no_longer_in_the_lock_loses_its_whole_directory() {
        let tree = Tree::new();
        tree.create_home_root();
        let stale_entry = tree.home.entry("ci", "ninja").expect("an admitted component pair");
        std::fs::create_dir_all(stale_entry.parent().unwrap()).unwrap();
        std::fs::create_dir_all(tree.tmp.path().join("stale-package")).unwrap();
        ocx_util::fs::symlink::create(tree.tmp.path().join("stale-package"), &stale_entry).unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP, "ci"]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(outcome_of(&report, &link("ci", "ninja")), &RenderOutcome::Pruned);
        assert_eq!(
            outcome_of(&report, &group_directory("ci")),
            &RenderOutcome::Pruned,
            "the emptied group directory is its own artifact, not an entry it no longer contains"
        );
        assert!(
            !tree.home.root().join("ci").exists(),
            "…and it is gone from disk: {:?}",
            read_dir_names(tree.home.root())
        );
    }

    /// C-044, case 16 — a renamed tool key is one render, not two: both old
    /// spellings are gone and both new ones are present afterwards.
    ///
    /// RED: implement the write pass without the prune pass — the old spelling
    /// survives on PATH beside the new one.
    #[tokio::test]
    async fn a_renamed_tool_key_replaces_both_halves_in_one_render() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("old-name"), b"#!/bin/sh\n").unwrap();
        let old_entry = tree.home.entry(DEFAULT_GROUP, "old-name").expect("an admitted pair");
        std::fs::create_dir_all(old_entry.parent().unwrap()).unwrap();
        std::fs::create_dir_all(tree.tmp.path().join("old-package")).unwrap();
        ocx_util::fs::symlink::create(tree.tmp.path().join("old-package"), &old_entry).unwrap();

        let tool = locked_tool("new-name", DEFAULT_GROUP, "ns/tool", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/tool", 'd'), Some(&["new-name"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(outcome_of(&report, &trampoline("old-name")), &RenderOutcome::Pruned);
        assert_eq!(
            outcome_of(&report, &link(DEFAULT_GROUP, "old-name")),
            &RenderOutcome::Pruned
        );
        assert_eq!(
            outcome_of(&report, &link(DEFAULT_GROUP, "new-name")),
            &RenderOutcome::Written
        );
        assert_eq!(tree.bin_entries(), expected_bin_entries(&["new-name"]));
        assert!(std::fs::symlink_metadata(&old_entry).is_err(), "the old link is gone");
    }

    /// S-003, case 17 — a file in `bin/` ocx never wrote is pruned. The
    /// **dotfile** is the discriminating half.
    ///
    /// RED: filter `.`-prefixed entries out of the prune `readdir`. A hostile
    /// clone's `.hidden` then survives every render inside a directory that is
    /// on someone's PATH.
    #[tokio::test]
    async fn a_committed_foreign_file_in_bin_is_pruned_including_a_dotfile() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(
            tree.home.shell_bin(DEFAULT_SHELL).join("cmake"),
            b"#!/bin/sh\nhostile\n",
        )
        .unwrap();
        std::fs::write(
            tree.home.shell_bin(DEFAULT_SHELL).join(".hidden"),
            b"#!/bin/sh\nhostile\n",
        )
        .unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(outcome_of(&report, &trampoline("cmake")), &RenderOutcome::Pruned);
        assert_eq!(
            outcome_of(&report, &trampoline(".hidden")),
            &RenderOutcome::Pruned,
            "S-003 — a dot-prefixed committed file is exactly as foreign as any other"
        );
        assert!(tree.bin_entries().is_empty());
    }

    /// RUL-32, case 18 — an orphan that is a **directory** is `Skipped` with a
    /// reason, never removed recursively, and the render continues to later
    /// entries.
    ///
    /// RED: propagate the error instead — one hostile directory then aborts
    /// every later entry.
    #[tokio::test]
    async fn an_orphan_directory_in_bin_is_skipped_and_does_not_abort_the_render() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL).join("hostile")).unwrap();
        std::fs::write(
            tree.home.shell_bin(DEFAULT_SHELL).join("hostile").join("payload"),
            b"keep me\n",
        )
        .unwrap();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("RUL-32 — a hostile directory is a skip, never an error");

        assert!(
            is_skipped(outcome_of(&report, &trampoline("hostile"))),
            "the orphan directory is reported as skipped: {report:?}"
        );
        assert!(
            tree.home
                .shell_bin(DEFAULT_SHELL)
                .join("hostile")
                .join("payload")
                .exists(),
            "…and nothing under it was removed recursively"
        );
        assert!(
            tree.bin_entries().contains(&trampoline_files("cmake")[0]),
            "…and the render continued to the entries after it: {:?}",
            tree.bin_entries()
        );
    }

    /// C-044/RUL-40, case 19 — an orphan whose on-disk name
    /// `ToolchainHome::entry`'s grammar would refuse is **pruned**, by that
    /// on-disk name exactly as `read_dir` yielded it.
    ///
    /// The validator exists to stop untrusted *input* becoming a path
    /// component; a name yielded by `read_dir` of a directory already proven to
    /// be inside the home is not that. Routing the prune through it would make
    /// a hostile clone's `<group>/` named `C:` or `a\b` unprunable forever — a
    /// permanent foothold inside a tree ocx owns. Containment is
    /// [`prune_within`]'s canonicalising check, which is the right guard.
    ///
    /// RED: route the prune through `ToolchainHome::entry`'s grammar validator
    /// — both directories then survive every render.
    ///
    /// `#[cfg(unix)]`, because the fixture cannot build its own premise
    /// anywhere else: neither `C:` nor `a\b` is a filename on Windows, and
    /// `Path::join` reinterprets both before they reach the disk — `C:` is a
    /// drive prefix that discards the home root outright, `a\b` is two
    /// components. `read_dir` there can never yield either name, so there is no
    /// orphan to prune and the row would assert against a tree it did not
    /// create. The Windows half of the same rule is
    /// [`is_single_component`], pinned by
    /// `the_single_component_test_reads_this_platforms_parser` below — that is
    /// where a `C:` reaching the prune is refused rather than joined.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_orphan_whose_name_the_grammar_refuses_is_reported_rather_than_dropped() {
        let tree = Tree::new();
        tree.create_home_root();
        for hostile in ["C:", "a\\b"] {
            std::fs::create_dir_all(tree.home.root().join(hostile)).unwrap();
        }

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("a hostile on-disk name is not an error");

        for hostile in ["C:", "a\\b"] {
            assert_eq!(
                outcome_of(&report, &root_entry(hostile)),
                &RenderOutcome::Pruned,
                "RUL-40 — `{hostile}` is pruned by its on-disk name, not left unprunable by a \
                 grammar validator that exists for untrusted input"
            );
            assert!(
                !tree.home.root().join(hostile).exists(),
                "…and it is gone from disk: {:?}",
                read_dir_names(tree.home.root())
            );
        }
    }

    /// RUL-40's containment half, on **every** host: the name the prune is
    /// about to `join` must be one ordinary component *to this platform's own
    /// parser*, because that same parser performs the removal.
    ///
    /// This is the Windows counterpart of the row above, which can only run on
    /// a host where `C:` and `a\b` are creatable filenames. Here the asymmetry
    /// is the assertion rather than the fixture, so both hosts are covered:
    /// what is one component on Unix is a drive prefix or two components on
    /// Windows, and a `PathBuf::join` on either discards or escapes the parent.
    ///
    /// RED: return `true` unconditionally from `is_single_component` — the
    /// Windows arm fires. RED: return `false` for every name — the shared arm
    /// fires, so this is not a check that only ever refuses.
    #[test]
    fn the_single_component_test_reads_this_platforms_parser() {
        for ordinary in ["cmake", "Make", "a.b", "..hidden"] {
            assert!(
                is_single_component(ordinary),
                "{ordinary} is one ordinary component on every platform"
            );
        }
        for escaping in ["a/b", "..", "/abs", ""] {
            assert!(
                !is_single_component(escaping),
                "{escaping:?} is not a single ordinary component on any platform"
            );
        }
        for windows_only in ["C:", r"a\b"] {
            assert_eq!(
                is_single_component(windows_only),
                cfg!(not(windows)),
                "{windows_only:?} is an ordinary name on Unix and a prefix or a separator on \
                 Windows — the prune must follow the parser that will do the join"
            );
        }
    }

    /// The `RenderedArtifact` doc limit, case 20 — a non-UTF-8 `bin/` entry
    /// has no representation in the report, is therefore **never pruned**, and
    /// survives the render as an untouched foreign file.
    ///
    /// Specified rather than left implicit: the alternative reading — "the
    /// prune is exact for every entry" — is false, and validation item 31's
    /// exactness claim has to be read against this limit.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_utf8_bin_entry_is_neither_reported_nor_pruned() {
        use std::os::unix::ffi::OsStrExt as _;

        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let hostile = tree
            .home
            .shell_bin(DEFAULT_SHELL)
            .join(std::ffi::OsStr::from_bytes(b"invalid-\xff-name"));
        if let Err(refused) = std::fs::write(&hostile, b"#!/bin/sh\n") {
            // APFS validates filenames as UTF-8 and rejects these bytes
            // outright, so the on-disk state under test cannot exist on a macOS
            // volume. Observed, not assumed via `target_os`: the row still runs
            // on any filesystem that accepts the name, and a refusal for any
            // other reason (an absent `bin/`) reds here rather than passing as
            // this carve-out. Same shape as `index_store`'s two non-UTF-8 rows.
            assert!(
                tree.home.shell_bin(DEFAULT_SHELL).exists(),
                "only the non-UTF-8 component may be refused; bin/ must exist: {refused}"
            );
            eprintln!("skipped: this filesystem refuses a non-UTF-8 filename: {refused}");
            return;
        }

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("an unnameable entry is not an error");

        assert!(
            report.items.is_empty(),
            "the entry cannot become a `RenderedArtifact`, so it is not reported: {report:?}"
        );
        assert!(
            std::fs::symlink_metadata(&hostile).is_ok(),
            "…and it survives as an untouched foreign file"
        );
    }

    /// C-004, case 21 — `.gitignore` is never pruned, including by a render
    /// whose computed set is empty.
    ///
    /// RED: widen the prune `readdir` from `bin/` to the home root.
    #[tokio::test]
    async fn the_gitignore_is_never_pruned_even_by_an_empty_render() {
        let tree = Tree::new();
        tree.create_home_root();
        tree.home.ensure_gitignore().expect("the ignore file is writable");
        let before = std::fs::read(tree.home.gitignore()).unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            std::fs::read(tree.home.gitignore()).unwrap(),
            before,
            "C-004 — the ignore file is the home's own, not an orphan"
        );
    }

    /// C-003, case 22 — a render touches nothing under
    /// `state/projects/<key>/` except the render stamp itself.
    ///
    /// RED: write the stamp inside the home and watch a root-level prune eat
    /// it, or widen the prune to the state store.
    #[tokio::test]
    async fn a_render_touches_nothing_in_the_project_state_directory_but_the_stamp() {
        let tree = Tree::new();
        tree.create_home_root();
        let state_dir = tree.file_structure.state.project_state_dir(&tree.key());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("consent.json"), b"{}\n").unwrap();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            std::fs::read(state_dir.join("consent.json")).unwrap(),
            b"{}\n",
            "C-003 — the consent stamp shares the `<key>` directory and is not the renderer's"
        );
        let mut names = read_dir_names(&state_dir);
        names.sort();
        assert_eq!(
            names,
            vec!["consent.json".to_string(), "render_stamp.json".to_string()],
            "the render adds exactly one file to that directory"
        );
    }

    /// C-045, case 23 — a non-default group the invocation did not select is
    /// byte-identical after `ocx pull -g default`.
    ///
    /// RED: reconcile the whole home root against `request.groups` — silent
    /// data loss across every group the invocation did not name.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_group_the_invocation_did_not_select_is_byte_identical_afterwards() {
        let tree = Tree::new();
        tree.create_home_root();
        let ci_entry = tree.home.entry("ci", "ninja").expect("an admitted component pair");
        std::fs::create_dir_all(ci_entry.parent().unwrap()).unwrap();
        std::fs::create_dir_all(tree.tmp.path().join("ci-package")).unwrap();
        ocx_util::fs::symlink::create(tree.tmp.path().join("ci-package"), &ci_entry).unwrap();
        let before = snapshot_subtree(&tree.home.links_group("ci").expect("an admitted group name"));

        let tool = locked_tool("ninja", "ci", "ns/ninja", &[(PLATFORM_KEY, 'd')]);
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            snapshot_subtree(&tree.home.links_group("ci").expect("an admitted group name")),
            before,
            "C-045 — `-g default` reconciles the default group and nothing else"
        );
    }

    /// V-30 / RUL-33 — a symlink at `<home>/bin` is refused **at the create**,
    /// never followed.
    ///
    /// Step 2's `refuse_symlinked_home_leaves` judges `bin/` and then the
    /// render **lock acquisition** runs, blocking for an unbounded time; a
    /// project home inside a group-writable checkout can have `bin/` swapped in
    /// that window. This test plants the link where such a swap leaves it — the
    /// check has already passed — and asserts the publish does not follow it.
    ///
    /// RED: `create_owner_only` in place of `create_bin_owner_only`. Its
    /// `create_dir_all` stats with `metadata`, which follows the link, returns
    /// `Ok`, and every trampoline lands in `outside/` — a directory that then
    /// goes on the user's `PATH`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_bin_directory_is_refused_at_the_create_never_followed() {
        let tree = Tree::new();
        tree.create_home_root();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, tree.home.shell_bin(DEFAULT_SHELL)).unwrap();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("RUL-33 degrades, never errors");

        assert!(
            std::fs::read_dir(&outside)
                .expect("the planted target is readable")
                .next()
                .is_none(),
            "nothing may be published through a symlinked `bin/` — the link's target must stay empty"
        );
        assert!(
            report
                .items
                .iter()
                .any(|item| matches!(item.outcome, RenderOutcome::Skipped { .. })),
            "C-050 — the refusal is reported as a skip, one warn line each"
        );
    }

    /// C-053, case 24 — setting and then unsetting `toolchain_dir` leaves the
    /// **abandoned** tree byte-identical and renders a complete new one.
    ///
    /// RED: pass a "previous home" to the prune pass — a mistaken `[managed]`
    /// push then destroys trees across a fleet.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_tree_at_a_home_no_longer_resolved_is_left_exactly_as_it_is() {
        let tree = Tree::new();
        tree.create_home_root();
        let relocated = ToolchainHome::new(tree.tmp.path().join("relocated").join("toolchain"));
        seed_rendered_home(&relocated);

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();

        // `toolchain_dir` unset: the project's own home renders.
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the in-project home renders");
        let abandoned = snapshot_subtree(tree.home.root());
        assert!(!abandoned.is_empty(), "precondition: there is a tree to abandon");

        // `toolchain_dir` set: the home re-resolves, and every later render
        // goes there. The tree at the old location is now unreachable to ocx.
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &relocated,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the relocated home renders again");

        assert_eq!(
            snapshot_subtree(tree.home.root()),
            abandoned,
            "C-053 — a tree at a location ocx no longer resolves to is left in place, never deleted"
        );
        assert_eq!(
            read_dir_names(&relocated.shell_bin(DEFAULT_SHELL)),
            expected_bin_entries(&["cmake"]),
            "…and the new home is complete"
        );
    }

    /// C-053/S-4/RUL-33, case 25(a) — a home root committed as a **symlink**
    /// (to `$HOME`, in the wild) is refused before anything is created.
    ///
    /// `create_dir_all` on an existing symlink-to-a-directory succeeds
    /// silently, so the first write would already be outside the project.
    #[cfg(unix)]
    #[test]
    fn ensure_home_root_refuses_a_symlinked_home_root() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(tree.home.root().parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, tree.home.root()).unwrap();

        assert!(
            ensure_home_root(&tree.home).is_err(),
            "RUL-33 — a symlinked home root is refused, judged by symlink_metadata"
        );
    }

    /// C-053/S-4/RUL-33, case 25(b) — a `bin/` committed as a symlink is
    /// refused too: every `bin/<name>` write and removal would otherwise land
    /// in the link's target directory.
    #[cfg(unix)]
    #[test]
    fn ensure_home_root_refuses_a_symlinked_bin_directory() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(tree.shell_directory()).unwrap();
        std::os::unix::fs::symlink(&outside, tree.home.shell_bin(DEFAULT_SHELL)).unwrap();

        assert!(
            ensure_home_root(&tree.home).is_err(),
            "RUL-33 — a symlinked `bin/` is the same hole one level down"
        );
    }

    /// C-053/S-4, case 25(b) delete side — `prune_within` refuses a
    /// `bin/<name>` that resolves outside the home through a symlinked `bin/`.
    ///
    /// RED: replace the canonicalising containment check with `starts_with` —
    /// the spelled path is still under the home, so the delete escapes.
    #[cfg(unix)]
    #[test]
    fn prune_within_refuses_a_trampoline_reached_through_a_symlinked_bin() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("cmake"), b"not ours\n").unwrap();
        std::fs::create_dir_all(tree.shell_directory()).unwrap();
        std::os::unix::fs::symlink(&outside, tree.home.shell_bin(DEFAULT_SHELL)).unwrap();

        assert!(
            prune_within(&tree.home, &trampoline("cmake")).is_err(),
            "C-053 — the delete path canonicalises both sides before comparing"
        );
        assert!(
            outside.join("cmake").exists(),
            "…and the file outside the home survives"
        );
    }

    /// C-053/S-4, case 25(c) — a `<group>/` that is a symlink out of the home
    /// does the same for every `<group>/<entry>`, and `prune_within` refuses
    /// that too.
    #[cfg(unix)]
    #[test]
    fn prune_within_refuses_an_entry_reached_through_a_symlinked_group_directory() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("cmake"), b"not ours\n").unwrap();
        std::fs::create_dir_all(links_root(&tree.home)).unwrap();
        std::os::unix::fs::symlink(&outside, links_root(&tree.home).join(DEFAULT_GROUP)).unwrap();

        assert!(
            prune_within(&tree.home, &link(DEFAULT_GROUP, "cmake")).is_err(),
            "C-053 — a group directory that is a symlink out of the home is the third route"
        );
        assert!(outside.join("cmake").exists());
    }

    /// C-044/C-053 — the positive half of the containment guard: an artifact
    /// that really is inside the home **is** removed.
    ///
    /// Without this, the three refusals above would be satisfied by a
    /// `prune_within` that refuses everything — the always-red shape that is
    /// half a proof.
    #[test]
    fn prune_within_removes_an_artifact_that_resolves_inside_the_home() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let victim = tree.home.shell_bin(DEFAULT_SHELL).join("cmake");
        std::fs::write(&victim, b"#!/bin/sh\n").unwrap();

        prune_within(&tree.home, &trampoline("cmake")).expect("an entry inside the home is removable");
        assert!(!victim.exists(), "C-044 — the prune half actually removes");
    }

    /// RUL-32 — `prune_within` never removes a group directory recursively: a
    /// directory still holding a foreign file fails its non-recursive
    /// `remove_dir` and the foreign file survives.
    ///
    /// RED: `remove_dir_all` — a recursive delete primitive reachable from a
    /// hostile `ocx.lock`.
    #[test]
    fn prune_within_never_removes_a_non_empty_group_directory_recursively() {
        let tree = Tree::new();
        let group = tree.home.links_group("ci").expect("an admitted group name");
        std::fs::create_dir_all(&group).unwrap();
        std::fs::write(group.join("foreign"), b"keep me\n").unwrap();

        assert!(
            prune_within(&tree.home, &group_directory("ci")).is_err(),
            "RUL-32 — a non-empty group directory fails `remove_dir` rather than being emptied"
        );
        assert!(
            group.join("foreign").exists(),
            "…and the foreign file the render did not put there survives"
        );
    }

    // ── 3. Concurrency and the atomic-publish shapes ────────────────────────

    /// C-048/C-061, case 27 — a trampoline rewrite is temp-file-plus-rename,
    /// never a truncate in place.
    ///
    /// The **inode** is the discriminating assertion: a truncating write
    /// leaves the bytes correct and the inode identical, and a reconciler
    /// reading `bin/` concurrently would then observe a torn file. A running
    /// trampoline keeping the old inode is the other half of the same
    /// property.
    ///
    /// RED: make the write truncate in place.
    #[cfg(unix)]
    #[test]
    fn write_trampoline_atomic_replaces_the_inode_rather_than_truncating_in_place() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let path = tree.home.shell_bin(DEFAULT_SHELL).join("cmake");
        std::fs::write(&path, b"#!/bin/sh\nold\n").unwrap();
        let before = inode_of(&path);

        write_trampoline_atomic(&path, "#!/bin/sh\nnew\n").expect("the write succeeds");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "#!/bin/sh\nnew\n");
        assert_ne!(
            inode_of(&path),
            before,
            "C-061 — the publish is a rename over the live file, so a running trampoline keeps \
             the old inode and no reader ever sees a half-written one"
        );
    }

    /// C-044, case 38 — a published trampoline is **owner-executable**.
    ///
    /// Asserted as `mode & 0o100 != 0`, never `== 0o755`: the exact mode
    /// depends on the ambient umask, so an equality assertion would be a CI
    /// flake rather than a check.
    ///
    /// RED: drop the mode set — the default `0o600` publishes a
    /// non-executable trampoline onto someone's PATH.
    #[cfg(unix)]
    #[test]
    fn write_trampoline_atomic_publishes_an_owner_executable_file() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let path = tree.home.shell_bin(DEFAULT_SHELL).join("cmake");

        write_trampoline_atomic(&path, "#!/bin/sh\n").expect("the write succeeds");

        assert_ne!(
            mode_of(&path) & 0o100,
            0,
            "C-044 — the owner-execute bit is set, and it is set on the temp file before the \
             rename so no window exists in which the entry is present and not executable"
        );
    }

    /// C-044, case 38 through the render — the same property must hold for
    /// what a whole render publishes, not only for the seam.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_rendered_trampoline_is_owner_executable() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_ne!(mode_of(&tree.home.shell_bin(DEFAULT_SHELL).join("cmake")) & 0o100, 0);
    }

    /// C-048, case 28 — the ADR's named negative control: a fault injected
    /// **between the first entry write and the stamp write** leaves `bin/`
    /// holding at least one entry, no new stamp, and any previous stamp
    /// unmodified.
    ///
    /// The fault is a **parameter**, not an environment write: the override
    /// table `ocx_util::env::overrides::EnvLock` maintains is consulted only by
    /// `ocx_util::env::var`, never by `std::env::var_os`, so an env-only seam
    /// would have forced `unsafe { std::env::set_var }` into this test.
    ///
    /// RED: write the stamp before the entry pass — the tree then reports as
    /// finished while it is half-written.
    #[tokio::test]
    async fn a_fault_between_the_first_entry_and_the_stamp_leaves_a_detectably_incomplete_tree() {
        let tree = Tree::new();
        tree.create_home_root();
        let state_dir = tree.file_structure.state.project_state_dir(&tree.key());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(tree.stamp_file(), b"{\"previous\": true}\n").unwrap();

        let surface = vec![
            node(pinned("ns/one", 'a'), Some(&["one"]), &[]),
            node(pinned("ns/two", 'b'), Some(&["two"]), &[]),
        ];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let result = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            Some(FAULT_AFTER_FIRST_ENTRY_WRITE),
        )
        .await;

        assert!(
            result.is_err(),
            "C-048 — the injected fault aborts the render: {result:?}"
        );
        assert!(
            !tree.bin_entries().is_empty(),
            "…after at least one entry landed, which is what makes the tree *partially* rendered"
        );
        assert_eq!(
            std::fs::read(tree.stamp_file()).unwrap(),
            b"{\"previous\": true}\n",
            "…and the previous stamp is untouched, so C-061's gate sees a mismatch rather than \
             a tree that looks finished"
        );
    }

    /// C-048 — the fault seam fires for its one stage and never otherwise.
    ///
    /// The positive control for the test above: without it, an
    /// always-`Ok` `maybe_inject_fault` and a correctly-ordered render are
    /// indistinguishable.
    #[test]
    fn the_fault_seam_fires_only_for_its_own_stage_and_value() {
        assert!(
            maybe_inject_fault(Some(FAULT_AFTER_FIRST_ENTRY_WRITE), RenderStage::AfterFirstEntryWrite).is_err(),
            "C-048 — the named value at the named stage aborts"
        );
        assert!(
            maybe_inject_fault(None, RenderStage::AfterFirstEntryWrite).is_ok(),
            "…and an absent fault never does"
        );
        assert!(
            maybe_inject_fault(Some("some-other-stage"), RenderStage::AfterFirstEntryWrite).is_ok(),
            "…nor does an unrelated value"
        );
    }

    /// C-051, case 30 — repointing a `<group>/<entry>` is an atomic replace: a
    /// reader in a tight loop sees the old target or the new one, never
    /// absence.
    ///
    /// RED: use remove-then-create — the reader observes `ENOENT`, and
    /// C-067's per-entry digest degrade fires spuriously on a link that is
    /// merely mid-repoint.
    ///
    /// One-sided by construction: a passing run does not prove the window is
    /// impossible, only that this reader never hit it. It is written this way
    /// because the alternative — asserting on the implementation's choice of
    /// primitive — is a source-text guard for a property with a real
    /// behavioural seam.
    #[cfg(unix)]
    #[tokio::test]
    async fn repointing_a_group_entry_never_makes_it_momentarily_absent() {
        let tree = Tree::new();
        tree.create_home_root();
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::create_dir_all(tree.tmp.path().join("stale-package")).unwrap();
        ocx_util::fs::symlink::create(tree.tmp.path().join("stale-package"), &entry).unwrap();

        let observed_absence = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (entry, observed_absence, stop) = (entry.clone(), observed_absence.clone(), stop.clone());
            // The deadline, not the flag, is what bounds this thread: a
            // panicking render never reaches `stop.store`, and a busy loop that
            // outlived its test would spin for the rest of the run.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) && std::time::Instant::now() < deadline {
                    if std::fs::symlink_metadata(&entry).is_err() {
                        observed_absence.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            })
        };

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        // The result is captured before the reader is stopped, so a panicking
        // render cannot leave the busy-loop thread spinning for the rest of
        // the run.
        let result = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader.join().expect("the reader thread joins");
        result.expect("the repoint succeeds");

        assert!(
            !observed_absence.load(std::sync::atomic::Ordering::Relaxed),
            "C-051 — the repoint is an atomic replace, so a concurrent reader never sees the \
             link absent"
        );
        assert_eq!(std::fs::read_link(&entry).unwrap(), expected_target);
    }

    /// RUL-30/RUL-38, case 26 — two renders against one home are **serialized**
    /// by the render lock, which is discriminated by the stamp key.
    ///
    /// Two terminals running `ocx pull` is an ordinary state; unlocked they
    /// interleave writes and prunes and race the stamp into a C-061 mismatch no
    /// later render clears, because each run's stamp describes a tree the other
    /// was still editing.
    ///
    /// Driven through [`render_lock_parameters`] so the test takes *the same*
    /// lock rather than one that merely resembles it — without that, "the render
    /// takes a lock" would be green in a way indistinguishable from never having
    /// run.
    ///
    /// **Asserted as an ordering, not a duration.** It was `elapsed >= hold`
    /// over a `tokio::join!` whose other arm sleeps `hold` — which the join
    /// satisfies whether or not the render ever waited, so the assertion held
    /// in both states and the test could not tell waiting from sleeping. A
    /// control that gave every `lock_scoped` key a unique suffix, so nothing
    /// in the workspace could contend, reddened four sibling lock tests and
    /// left this one green. The claim is that the render **could not return
    /// until the holder let go**, which is a fact about order, so the two
    /// events are recorded and their sequence is what is asserted.
    ///
    /// RED: remove the lock, or break the key it contends on — the render then
    /// returns during the hold and the events arrive the other way round.
    #[tokio::test]
    async fn two_renders_against_one_home_are_serialized_by_the_render_lock() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();

        let parameters = render_lock_parameters(
            &tree.file_structure,
            &RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
        );
        assert_eq!(parameters.scope, "toolchain-render");
        assert_eq!(
            parameters.discriminator,
            tree.key(),
            "RUL-38 — discriminated by the stamp key, so two homes never share a lock and one \
             home never takes two"
        );
        assert_eq!(parameters.guarded_directory, tree.home.root());
        let held = parameters.acquire().await.expect("the test can take the same lock");

        let hold = std::time::Duration::from_millis(150);
        let manager = tree.manager();
        // The holder records its release *before* it lets go, so a render that
        // never contended — returning somewhere inside the hold — lands its
        // event first. That is the whole discriminator.
        let events: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
        let (rendered, ()) = tokio::join!(
            async {
                let outcome = manager
                    .render_toolchain(RenderRequest {
                        home: &tree.home,
                        scope: &scope,
                        lock: &lock,
                        surface: &surface,
                        groups: &groups,
                        pinned: false,
                        platform: &platform,
                        dry_run: false,
                    })
                    .await;
                events
                    .lock()
                    .expect("the event log is not poisoned")
                    .push("render-returned");
                outcome
            },
            async {
                tokio::time::sleep(hold).await;
                events
                    .lock()
                    .expect("the event log is not poisoned")
                    .push("lock-released");
                drop(held);
            }
        );

        rendered.expect("the render succeeds once the lock is free");
        assert_eq!(
            *events.lock().expect("the event log is not poisoned"),
            ["lock-released", "render-returned"],
            "RUL-30 — the second render returned before the first let go, so it never waited for \
             the lock; it interleaved with it"
        );
        assert_eq!(tree.bin_entries(), expected_bin_entries(&["cmake"]));
    }

    /// The shipped lock budget, asserted without sleeping: both degradation
    /// tests above wait out [`TESTING_LOCK_TIMEOUT_ENV`]'s value instead of
    /// this one, so without this pin a change to the shipped budget would
    /// red nothing.
    ///
    /// Pins the *derivation* too — the render lock waits exactly as long as a
    /// foreground `ocx pull` does (D-V14, one budget, one spelling).
    #[test]
    fn the_shipped_toolchain_lock_timeout_is_the_pull_budget() {
        assert_eq!(TOOLCHAIN_LOCK_TIMEOUT, super::super::pull::PULL_LOCAL_LOCK_TIMEOUT);
        assert_eq!(TOOLCHAIN_LOCK_TIMEOUT, Duration::from_secs(5));

        let environment = ocx_util::env::overrides::lock();
        environment.remove(TESTING_LOCK_TIMEOUT_ENV);
        assert_eq!(
            toolchain_lock_timeout(),
            TOOLCHAIN_LOCK_TIMEOUT,
            "with no override in play the seam resolves to the shipped budget"
        );
        environment.set(TESTING_LOCK_TIMEOUT_ENV, "150");
        assert_eq!(
            toolchain_lock_timeout(),
            Duration::from_millis(150),
            "…and the override is what the tests above actually wait out, or they are pinning \
             a value nothing reads"
        );
    }

    /// C-050/RUL-30/RUL-38, case 52 — a render lock that cannot be acquired
    /// within its timeout is a **skip**, never an error.
    ///
    /// C-050's own trigger list names "lock timeout" outright: the call warns,
    /// returns empty `items` and `stamp_written: false`, and is `Ok`. The
    /// elapsed assertion is what makes this a timeout rather than any other
    /// acquisition failure.
    ///
    /// It costs the lock's own timeout in wall-clock — so the timeout it waits
    /// out is [`TESTING_LOCK_TIMEOUT_ENV`]'s, and the render reads the same
    /// seam the assertion below reads. The shipped budget is pinned without
    /// sleeping, at [`the_shipped_toolchain_lock_timeout_is_the_pull_budget`].
    ///
    /// RED: propagate the timeout — `ocx pull` then fails outright because a
    /// second terminal happened to be pulling.
    #[tokio::test]
    async fn a_render_lock_timeout_is_a_skip_and_never_an_error() {
        let environment = ocx_util::env::overrides::lock();
        environment.set(TESTING_LOCK_TIMEOUT_ENV, "150");

        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();

        let parameters = render_lock_parameters(
            &tree.file_structure,
            &RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
        );
        // Held for the whole call: the render must time out rather than acquire.
        let _held = parameters.acquire().await.expect("the test can take the same lock");

        let started = std::time::Instant::now();
        let report = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("C-050 — a lock timeout is a skip, never an error");
        let elapsed = started.elapsed();

        assert!(
            elapsed >= parameters.timeout,
            "the render actually waited out its own timeout rather than failing for another \
             reason: {elapsed:?} < {:?}",
            parameters.timeout
        );
        assert!(report.items.is_empty(), "nothing was rendered: {report:?}");
        assert!(!report.stamp_written, "…and nothing was stamped");
        assert!(tree.stamp().is_none());
        assert!(
            tree.bin_entries().is_empty(),
            "…and `bin/` was left exactly as the lock holder had it: {:?}",
            tree.bin_entries()
        );
    }

    // ── 4. Platform divergence ──────────────────────────────────────────────

    /// RUL-35/C-031, case 32 — the Windows publish hardlinks `<name>.exe` from
    /// the `ShimBinStore` blob: byte-equal **and sharing the blob's file
    /// identity**, which a copy would not.
    ///
    /// Not `#[cfg(windows)]`: both writes are cross-platform primitives, and
    /// gating the seam would put its one testable property behind a `cfg` the
    /// CI leg that actually runs never compiles.
    ///
    /// RED: copy the blob instead of hardlinking — the bytes still match, only
    /// the identity assertion reds.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_windows_publish_hardlinks_the_exe_from_the_shim_blob() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let exe = tree.home.shell_bin(DEFAULT_SHELL).join("cmake.exe");
        let sidecar = tree.home.shell_bin(DEFAULT_SHELL).join("cmake.exec");

        let blob = tree
            .file_structure
            .shim_bin
            .ensure()
            .await
            .expect("the committed shim blob publishes");
        publish_windows_trampoline(&tree.file_structure.shim_bin, &exe, &sidecar, "C:\\proj\n")
            .await
            .expect("the Windows publish succeeds");

        assert_eq!(
            std::fs::read(&exe).unwrap(),
            std::fs::read(&blob).unwrap(),
            "C-031 — the `.exe` is byte-identical to the published blob"
        );
        assert_eq!(
            inode_of(&exe),
            inode_of(&blob),
            "RUL-35 — …by construction, through a hardlink, so the Authenticode verbatim-copy \
             property survives; a copy would satisfy the bytes and fail here"
        );
        assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), "C:\\proj\n");
    }

    /// RUL-35, case 32's ordering postcondition — the **sidecar lands first**.
    ///
    /// The only recoverable partial state this function may leave is
    /// `.exec`-present / `.exe`-absent. The inverse is directly harmful:
    /// `which cmake` resolves `cmake.exe`, the shim runs, finds no selector
    /// sidecar and fails at invocation — worse than the PATH lookup it
    /// replaced. Driven by making the hardlink fail while the sidecar write
    /// can still succeed.
    ///
    /// RED: swap the two writes, following the `prepare_lazy` precedent whose
    /// premise (a staged tree nobody's PATH contains) does not hold for `bin/`.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_windows_publish_writes_the_sidecar_before_the_exe() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let sidecar = tree.home.shell_bin(DEFAULT_SHELL).join("cmake.exec");
        // A directory at the `.exe` path makes the hardlink fail while the
        // sidecar write beside it still succeeds — the one input that tells
        // the two orderings apart.
        let exe = tree.home.shell_bin(DEFAULT_SHELL).join("cmake.exe");
        std::fs::create_dir_all(&exe).unwrap();

        let result = publish_windows_trampoline(&tree.file_structure.shim_bin, &exe, &sidecar, "C:\\proj\n").await;

        assert!(result.is_err(), "the blocked `.exe` publish fails: {result:?}");
        assert!(
            sidecar.is_file(),
            "RUL-35 — the sidecar was already written when the `.exe` failed; the reverse order \
             would leave an `.exe` on PATH with no selector"
        );
    }

    /// RUL-26, case 33 — the stamp keys on the **on-disk file set**: after a
    /// render, `readdir(bin/)` and `bin_fingerprint` have the same length.
    ///
    /// Host-agnostic on purpose. It is satisfied trivially on POSIX (one file
    /// per name) and is the assertion a Windows-only defect reds: `bin/` holds
    /// two files per name there, so a stamp keyed on the bare stem is a
    /// permanent C-061 mismatch — the PATH entry withheld and the `ocx pull`
    /// hint printed on every prompt, forever.
    #[tokio::test]
    async fn the_stamp_fingerprint_has_one_entry_per_on_disk_bin_file() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake", "ctest"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        let stamp = tree.stamp().expect("the stamp is written");
        assert_eq!(
            stamp.bin_fingerprint.len(),
            tree.bin_entries().len(),
            "RUL-26 — one fingerprint entry per on-disk file, keyed by its full on-disk name"
        );
        assert_eq!(
            stamp.bin_fingerprint.keys().cloned().collect::<Vec<_>>(),
            tree.bin_entries(),
            "…and the keys are those names exactly, which is what C-061's readdir gate compares"
        );
    }

    /// RUL-20/D-V21, case 34 — `trampoline_target` performs **no** validation:
    /// a non-absolute project root constructs fine here, and the refusal fires
    /// downstream at the writer.
    ///
    /// RED: move the refusal into `trampoline_target` — the `.exec`-collision
    /// regression in `body.rs` (`Project("global")` emitting a sidecar
    /// byte-identical to the global home's) then loses its anchor, because the
    /// input that produces it can no longer be constructed.
    #[test]
    fn trampoline_target_maps_the_tier_and_validates_nothing() {
        assert!(
            matches!(trampoline_target(&RenderStampScope::Global), TrampolineTarget::Global),
            "C-028 — the global tier bakes the valueless `--global`"
        );
        let relative = PathBuf::from("global");
        assert!(
            matches!(
                trampoline_target(&RenderStampScope::Project(relative.clone())),
                TrampolineTarget::Project(root) if root == relative
            ),
            "RUL-20 — a non-absolute root constructs; `unix_trampoline_body` and the `.exec` \
             writer are where D-V21's refusal fires"
        );
    }

    /// C-028, case 41 and validation item 12's positive control — the same
    /// project rendered from two **different absolute directories** produces
    /// different bodies.
    ///
    /// This is the one input that rewrites a body, which is why it, and not a
    /// `pinned` flip, is item 12's control: C-046 asserts bodies stay
    /// byte-identical across a `pinned` flip on purpose.
    ///
    /// RED: derive `TrampolineTarget` from `request.home` instead of
    /// `request.scope`.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_same_project_rendered_from_two_directories_bakes_two_different_bodies() {
        let tree = Tree::new();
        let other_project = tree.tmp.path().join("proj-elsewhere");
        std::fs::create_dir_all(&other_project).unwrap();
        let other_home = ToolchainHome::new(other_project.join(".ocx").join("toolchain"));
        tree.create_home_root();
        seed_rendered_home(&other_home);

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();

        for (home, project_dir) in [
            (&tree.home, tree.project_dir.clone()),
            (&other_home, other_project.clone()),
        ] {
            let scope = RenderStampScope::Project(project_dir);
            render_with(
                &tree.file_structure,
                RenderRequest {
                    home,
                    scope: &scope,
                    lock: &lock,
                    surface: &surface,
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                },
                false,
                None,
            )
            .await
            .expect("both render");
        }

        let first = std::fs::read_to_string(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap();
        let second = std::fs::read_to_string(other_home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap();
        assert_ne!(
            first, second,
            "C-028 — the baked selector names the project, so two projects bake two bodies"
        );
        assert_eq!(
            first,
            expected_unix_trampoline_body(&tree.project_dir, &tree.ocx_binary),
            "…and each body is exactly the shipped grammar, not merely different"
        );
        assert_eq!(second, expected_unix_trampoline_body(&other_project, &tree.ocx_binary));
    }

    /// C-046 and validation item 33, case 42(a) — flipping `pinned` leaves
    /// `bin/` **byte-identical**, and RUL-23 leaves the link pass entirely
    /// alone: no writes and no prunes.
    ///
    /// RED: bake `pinned` (or any composition flag) into the body — the flag
    /// then stops taking effect without a re-render, and item 11's parity
    /// oracle breaks with it. Second RED: reconcile the link set to empty
    /// under `pinned` — switching it on would delete the tree.
    #[cfg(unix)]
    #[tokio::test]
    async fn flipping_pinned_leaves_both_halves_of_the_tree_byte_identical() {
        let tree = Tree::new();
        tree.create_home_root();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the following-lane render succeeds");
        let before = snapshot_subtree(tree.home.root());

        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: true,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the pinned render succeeds");

        assert_eq!(
            std::fs::read_to_string(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap(),
            expected_unix_trampoline_body(&tree.project_dir, &tree.ocx_binary),
            "C-046 — no digest and no `pinned` value is ever baked, so the body is unchanged"
        );
        assert!(
            report
                .items
                .iter()
                .all(|item| !matches!(item.artifact, RenderedArtifact::Link { .. })),
            "RUL-23 — `pinned = true` suppresses the entire link pass, writes and prunes alike: \
             {report:?}"
        );
        let after = snapshot_subtree(tree.home.root());
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        assert!(
            std::fs::symlink_metadata(&entry).is_ok(),
            "…so the link written by the previous render is still there"
        );
        assert_eq!(
            after.iter().filter(|e| e.kind == "symlink").count(),
            before.iter().filter(|e| e.kind == "symlink").count(),
            "…and no link was removed"
        );
    }

    /// RUL-27, case 42's stamp half — a **pinned** render still stamps the
    /// default group's links, observed by one `readlink` pass over what is on
    /// disk rather than derived from the lock.
    ///
    /// RED: stamp an empty `link_fingerprint` under `pinned` — C-061's gate
    /// then mismatches on every prompt forever in exactly the cells validation
    /// item 20's `activate × pinned` matrix has to cover.
    #[tokio::test]
    async fn a_pinned_render_stamps_the_links_that_are_on_disk() {
        let tree = Tree::new();
        tree.create_home_root();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        for pinned_flag in [false, true] {
            render_with(
                &tree.file_structure,
                RenderRequest {
                    home: &tree.home,
                    scope: &scope,
                    lock: &lock,
                    surface: &surface,
                    groups: &groups,
                    pinned: pinned_flag,
                    platform: &platform,
                    dry_run: false,
                },
                false,
                None,
            )
            .await
            .expect("both renders succeed");
        }

        let stamp = tree.stamp().expect("the pinned render still writes a stamp");
        assert!(
            !stamp.link_fingerprint.is_empty(),
            "RUL-27 — the stamp describes the tree as it stands on disk, never the tree the \
             render deliberately did not write: {stamp:?}"
        );
    }

    /// C-003 — the **persisted** `link_fingerprint` key is `"<group>/<entry>"`,
    /// and the tree's internal shape is no part of it.
    ///
    /// `observe_default_group_links` builds the key from the `DEFAULT_GROUP`
    /// constant and the entry name while it `read_dir`s `links/<group>`, so the
    /// well-meant "make the key match the path" edit is a change to a
    /// **persisted format that nothing in this crate reads back**: the producer,
    /// the type and some doc comments are the whole of `link_fingerprint`'s
    /// occurrences, and no comparison exists anywhere. It would break silently
    /// and forever. `state_store.rs`'s key-set test checks field *names* only
    /// and cannot see this.
    ///
    /// RED: prefix the key with `links/` in `observe_default_group_links`.
    #[tokio::test]
    async fn the_stamped_link_fingerprint_key_is_group_slash_entry() {
        let tree = Tree::new();
        tree.create_home_root();
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        std::fs::create_dir_all(entry.parent().expect("the entry has a parent")).unwrap();
        std::fs::create_dir_all(tree.tmp.path().join("some-package")).unwrap();
        ocx_util::fs::symlink::create(tree.tmp.path().join("some-package"), &entry).unwrap();

        let scope = tree.scope();
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'd')],
        )]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: true,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        let stamp = tree.stamp().expect("the render writes a stamp");
        assert_eq!(
            stamp.link_fingerprint.keys().cloned().collect::<Vec<_>>(),
            vec![format!("{DEFAULT_GROUP}/cmake")],
            "the key carries the group and the entry, with no `links/` and no other tree component"
        );
    }

    /// C-053, case 42(b) — setting `toolchain_dir` yields a **fresh tree whose
    /// bodies are byte-identical**: the selector names the project, which
    /// `toolchain_dir` does not move.
    ///
    /// Stated as a negative control precisely because it looks like a second
    /// item-12 control and is not one.
    #[cfg(unix)]
    #[tokio::test]
    async fn relocating_the_home_leaves_every_body_byte_identical() {
        let tree = Tree::new();
        tree.create_home_root();
        let relocated = ToolchainHome::new(tree.tmp.path().join("relocated").join("toolchain"));
        seed_rendered_home(&relocated);

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        for home in [&tree.home, &relocated] {
            render_with(
                &tree.file_structure,
                RenderRequest {
                    home,
                    scope: &scope,
                    lock: &lock,
                    surface: &surface,
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                },
                false,
                None,
            )
            .await
            .expect("both homes render");
        }

        assert_eq!(
            std::fs::read_to_string(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap(),
            std::fs::read_to_string(relocated.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap(),
            "C-053 — a `toolchain_dir` change is not a body-rewriting input"
        );
    }

    /// C-1/D-V19, case 43 — the golden's inputs are pinned, and the seeded
    /// rung-1 `ocx` is the one that must be baked.
    ///
    /// Without the seeding every golden in this module would silently bake
    /// `std::env::current_exe()` — the test binary's own path, which varies by
    /// machine, by cargo profile and by the target-directory hash. That is
    /// stable locally and unstable on CI, which is the one failure mode
    /// validation items 12 and 31 cannot afford.
    ///
    /// RED: delete the seeding from `Tree::new`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_rendered_body_bakes_the_seeded_install_path_and_not_the_running_binary() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        let body = std::fs::read_to_string(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap();
        assert_eq!(
            body,
            expected_unix_trampoline_body(&tree.project_dir, &tree.ocx_binary),
            "the golden pins all of: the project root, the store root, the baked ocx, the \
             trampoline marker and the platform"
        );
        let running = std::env::current_exe().expect("the test binary has a path");
        assert!(
            !body.contains(&running.display().to_string()),
            "rung 1 answered, so rung 2's machine-dependent path is nowhere in the body"
        );
    }

    /// C-028, case 44 — a **dangling** rung-1 `current` (a half-uninstalled
    /// tree) does not stop the render: the ladder drops to rung 2 and the body
    /// differs from the rung-1 golden.
    ///
    /// RED: make rung 1 win on path shape alone — the body then bakes a path
    /// to nothing and `/bin/sh` reports a naked `ENOENT` naming nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dangling_install_tree_still_renders_and_bakes_a_different_binary() {
        let tree = Tree::new();
        tree.create_home_root();
        let current = tree.file_structure.symlinks.current(&ocx_oci::ocx_cli_identifier());
        std::fs::remove_dir_all(&current).expect("the seeded install tree is removable");
        std::os::unix::fs::symlink(tree.tmp.path().join("collected-by-clean"), &current)
            .expect("the dangling link is creatable");

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("C-028 — a dangling install tree is not a render failure");

        let body = std::fs::read_to_string(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap();
        assert_ne!(
            body,
            expected_unix_trampoline_body(&tree.project_dir, &tree.ocx_binary),
            "rung 1 declined, so the body must not bake the path that no longer resolves"
        );
    }

    /// R-W19(b), case 35 — the case-fold answer is a **probe of the real
    /// directory**, not a `cfg!(target_os)` branch.
    ///
    /// Asserted against a probe the test performs itself on the same
    /// directory, so the check is about that filesystem rather than about the
    /// host: a case-sensitive APFS volume and a case-insensitive loopback are
    /// both ordinary, and a `cfg!` answer would be an assertion no test on the
    /// other host could red.
    #[tokio::test]
    async fn the_case_fold_probe_answers_for_the_directory_it_was_given() {
        let tree = Tree::new();
        let probe_dir = tree.tmp.path().join("probe");
        std::fs::create_dir_all(&probe_dir).unwrap();
        std::fs::write(probe_dir.join("probe-case-a"), b"").unwrap();
        let truth = probe_dir.join("PROBE-CASE-A").exists();

        let answer = filesystem_is_case_insensitive(&probe_dir)
            .await
            .expect("a writable directory is probeable");

        assert_eq!(
            answer, truth,
            "R-W19(b) — the probe must answer what this filesystem actually does with two names \
             differing only in ASCII case"
        );
    }

    /// RUL-33/C-050, case 36 — a probe that **fails** is C-050's skip, not a
    /// boolean default.
    ///
    /// The probe writes into the very home the render is about to write, so a
    /// probe that cannot run is a home that cannot be written. Inventing
    /// either answer picks a silent corruption: `false` on a case-insensitive
    /// host writes two names that are one file and the stamp's entry set can
    /// never match C-061's `readdir`; `true` on a case-sensitive host silently
    /// drops a tool from `bin/`.
    ///
    /// RED: `unwrap_or(false)`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_render_whose_case_fold_probe_cannot_run_skips_rather_than_guessing() {
        let tree = Tree::new();
        tree.create_home_root();
        deny_writes(tree.home.root());

        let surface = vec![
            node(pinned("ns/first", 'a'), Some(&["make"]), &[]),
            node(pinned("ns/second", 'b'), Some(&["Make"]), &[]),
        ];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let result = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await;
        allow_writes(tree.home.root());

        let report = result.expect("C-050 — an unwritable home is a skip, never an error");
        assert!(report.items.is_empty(), "nothing was rendered: {report:?}");
        assert!(!report.stamp_written, "…and nothing was stamped");
        assert!(tree.stamp().is_none());
    }

    /// C-051, case 37 — a `<group>/<entry>` link points at the package
    /// **root**, never at a file inside it.
    ///
    /// RED: point it at `content/bin/<name>` — POSIX still resolves, and the
    /// Windows leg reds because a junction's target must be a directory.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_group_entry_link_targets_the_package_root_and_not_a_file_inside_it() {
        let tree = Tree::new();
        tree.create_home_root();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        assert_eq!(
            std::fs::read_link(&entry).unwrap(),
            expected_target,
            "C-051 — the target is the lock-derived digest root itself"
        );
        assert!(
            !expected_target.ends_with("ocx"),
            "…and not a path down into `content/bin/<name>`: {expected_target:?}"
        );
    }

    // ── 5. Idempotence and the goldens (validation items 12 and 31) ─────────

    /// C-047, case 39 — `render(render(x)) == render(x)`: the second render
    /// reports every item `Unchanged` **and leaves every `bin/<name>` inode
    /// unchanged**.
    ///
    /// The inode is the discriminating half. An unconditional rewrite produces
    /// byte-identical content, so only the inode assertion reds — and every
    /// stat gate downstream (C-061's `(name, size, inode)` recomputation) is
    /// invalidated by exactly that rewrite.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_second_render_changes_nothing_and_replaces_no_inode() {
        let tree = Tree::new();
        tree.create_home_root();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let first = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the first render succeeds");
        let inode_before = inode_of(&tree.home.shell_bin(DEFAULT_SHELL).join("cmake"));
        let subtree_before = snapshot_subtree(tree.home.root());

        let second = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the second render succeeds");

        assert!(
            first.items.iter().any(|item| item.outcome == RenderOutcome::Written),
            "precondition: the first render actually wrote something: {first:?}"
        );
        assert!(
            second.items.iter().all(|item| item.outcome == RenderOutcome::Unchanged),
            "C-047 — an already-correct entry is `Unchanged`, never rewritten: {second:?}"
        );
        assert_eq!(
            inode_of(&tree.home.shell_bin(DEFAULT_SHELL).join("cmake")),
            inode_before,
            "…and the file was left exactly as it is, which bytes alone cannot show"
        );
        assert_eq!(
            snapshot_subtree(tree.home.root()),
            subtree_before,
            "…across the whole tree, not just the one entry"
        );
    }

    /// C-004, case 40 — `.gitignore` idempotence asserted **through the
    /// render**: the second render leaves the file's inode unchanged, which is
    /// `ensure_gitignore` reporting "nothing written" from the outside.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_second_render_leaves_the_gitignore_untouched() {
        let tree = Tree::new();
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let manager = tree.manager();

        for _ in 0..1 {
            manager
                .render_toolchain(RenderRequest {
                    home: &tree.home,
                    scope: &scope,
                    lock: &lock,
                    surface: &[],
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                })
                .await
                .expect("the first render succeeds");
        }
        let before = inode_of(&tree.home.gitignore());

        manager
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the second render succeeds");

        assert_eq!(
            inode_of(&tree.home.gitignore()),
            before,
            "C-004 — ensure-present, not rewrite-every-time"
        );
    }

    // ── 6. The skip path (C-050) and the stamp (RUL-22 / RUL-24 / RUL-27) ───

    /// RUL-24, case 45 — a read-only checkout with **no** home root yet warns,
    /// renders nothing, stamps nothing, and is `Ok`.
    ///
    /// RED: return `Internal` — `ocx pull` then exits non-zero in an ordinary
    /// CI container, which is precisely the state C-050's trigger list opens
    /// with.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_home_root_that_cannot_be_created_is_a_skip_and_not_an_error() {
        let tree = Tree::new();
        let checkout = tree.home.root().parent().expect("`.ocx` is the parent");
        std::fs::create_dir_all(checkout).unwrap();
        deny_writes(checkout);

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let result = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await;
        allow_writes(checkout);

        let report = result.expect("RUL-24 — a creation failure after a passing resolve is I/O, not policy");
        assert!(report.items.is_empty(), "{report:?}");
        assert!(!report.stamp_written);
        assert!(!tree.home.root().exists(), "…and nothing was created");
    }

    /// C-050/RUL-39, case 46 — the state store is denied, so the **stamp write
    /// itself** fails: every item `Skipped`, `stamp_written == false`, still
    /// `Ok`.
    ///
    /// Named for what it drives. `stamp_written == false` means exactly one
    /// thing — the stamp write failed — and denying the state store is the only
    /// input that produces it. `bin/` is denied too so there is something for
    /// every item to be skipped *about*; the sibling below covers the commoner
    /// shape, a read-only `bin/` beside a **writable** state store, where the
    /// stamp is written and records nothing.
    ///
    /// RED: propagate a stamp-write failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_stamp_write_reports_stamp_written_false_and_still_returns_ok() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::create_dir_all(tree.file_structure.state.root()).unwrap();
        deny_writes(&tree.home.shell_bin(DEFAULT_SHELL));
        deny_writes(tree.file_structure.state.root());

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake", "ctest"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let result = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await;
        allow_writes(&tree.home.shell_bin(DEFAULT_SHELL));
        allow_writes(tree.file_structure.state.root());

        let report = result.expect("C-050 — a read-only checkout composes digest paths and exits 0");
        assert!(
            !report.items.is_empty() && report.items.iter().all(|item| is_skipped(&item.outcome)),
            "every item is skipped with its own reason: {report:?}"
        );
        assert!(!report.stamp_written, "…and the failed stamp write is a skip too");
    }

    /// C-050/RUL-39 — the commoner half of case 46: a read-only `bin/` beside a
    /// **writable** state store. Every item is `Skipped` and the stamp **is**
    /// written, recording only what landed — which here is nothing.
    ///
    /// That is the correct outcome, not a defect: C-061 then compares an empty
    /// stamp against a `bin/` still holding the stale trampolines, mismatches,
    /// and withholds the PATH entry. `stamp_written == true` beside an empty
    /// fingerprint is a different state from `stamp_written == false`, and both
    /// are reachable — which is why they need two tests.
    ///
    /// RED: gate the stamp write on "nothing was skipped", or build the
    /// fingerprint from the intended name set — the stamp would then claim a
    /// `bin/` that was never written, and C-061 would match on a stale tree.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_read_only_bin_with_a_writable_state_store_still_stamps_what_landed() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        // What a previous render left, and what this one can neither rewrite
        // nor prune.
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("cmake"), b"#!/bin/sh\nstale\n").unwrap();
        deny_writes(&tree.home.shell_bin(DEFAULT_SHELL));

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let result = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await;
        allow_writes(&tree.home.shell_bin(DEFAULT_SHELL));

        let report = result.expect("C-050 — an unwritable `bin/` is a skip, never an error");
        assert!(
            !report.items.is_empty() && report.items.iter().all(|item| is_skipped(&item.outcome)),
            "every entry is skipped with its own reason: {report:?}"
        );
        assert!(
            report.stamp_written,
            "RUL-39 — the state store was writable, so the stamp is written"
        );
        let stamp = tree.stamp().expect("the stamp exists");
        assert!(
            stamp.names.is_empty(),
            "RUL-22 — …recording only what landed, which here is nothing: {stamp:?}"
        );
        assert_eq!(
            std::fs::read(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap(),
            b"#!/bin/sh\nstale\n",
            "…while `bin/` still holds the stale trampoline C-061's gate will mismatch on"
        );
    }

    /// RUL-22/27, case 47 and case 48 — one entry unwritable, the others
    /// succeed: that entry is `Skipped` naming its path, the rest are
    /// `Written`, a stamp **is** written, and `bin_fingerprint` carries only
    /// what actually landed.
    ///
    /// RED: build the stamp from the intended name set. C-061's `readdir` gate
    /// then mismatches on every prompt forever — the exact failure C-003 cites
    /// when it refuses to widen `link_fingerprint`. Second RED:
    /// `remove_dir_all` the blocking directory, turning a hostile clone's
    /// committed directory into a delete primitive.
    #[tokio::test]
    async fn a_partially_skipped_render_stamps_only_what_landed() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        // A directory where a trampoline must go: the write cannot succeed and
        // RUL-32 forbids removing it recursively.
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL).join(&trampoline_files("blocked")[0])).unwrap();
        std::fs::write(
            tree.home
                .shell_bin(DEFAULT_SHELL)
                .join(&trampoline_files("blocked")[0])
                .join("payload"),
            b"keep me\n",
        )
        .unwrap();

        let surface = vec![node(pinned("ns/tools", 'a'), Some(&["blocked", "fine"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("C-050 — one blocked entry does not fail the render");

        let blocked = outcome_of(&report, &trampoline(&trampoline_files("blocked")[0]));
        match blocked {
            RenderOutcome::Skipped { path, reason } => {
                assert!(
                    path.ends_with(&trampoline_files("blocked")[0]),
                    "RUL-32 — the skip names the path it could not write: {path:?}"
                );
                assert!(!reason.is_empty(), "…and carries a reason for the warn line");
            }
            other => panic!("the blocked entry must be skipped, not {other:?}"),
        }
        assert_eq!(
            outcome_of(&report, &trampoline(&trampoline_files("fine")[0])),
            &RenderOutcome::Written,
            "…and the render continued to the entries after it"
        );
        assert!(report.stamp_written, "RUL-22 — a partially-skipped render still stamps");
        let stamp = tree.stamp().expect("the stamp exists");
        assert_eq!(
            stamp.names.iter().cloned().collect::<Vec<_>>(),
            trampoline_files("fine"),
            "RUL-22 — …and records only what landed, never the intended set: {stamp:?}"
        );
        assert!(
            tree.home
                .shell_bin(DEFAULT_SHELL)
                .join(&trampoline_files("blocked")[0])
                .join("payload")
                .exists(),
            "RUL-32 — the blocking directory was never removed recursively"
        );
    }

    /// C-044, case 49 — a `bin/<name>` committed as a **symlink** to a file
    /// outside the tree is *replaced* by the rename; the link's target is
    /// byte-identical afterwards and `bin/<name>` is a regular file.
    ///
    /// RED: implement the write as `fs::write` + `set_permissions` — the write
    /// then follows the link and overwrites `~/.bashrc`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_committed_symlink_in_bin_is_replaced_and_its_target_is_not_written_through() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let victim = tree.tmp.path().join("bashrc");
        std::fs::write(&victim, b"# the user's shell profile\n").unwrap();
        std::os::unix::fs::symlink(&victim, tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"# the user's shell profile\n",
            "C-044 — nothing is ever written *through* a committed link"
        );
        let metadata = std::fs::symlink_metadata(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap();
        assert!(
            metadata.is_file() && !metadata.is_symlink(),
            "…and the entry is a regular file afterwards"
        );
    }

    /// S-3, case 50 — the idempotence compare never opens a planted **FIFO**.
    ///
    /// This is the one hazard in this module with no recovery path: `open` on
    /// a FIFO with no writer blocks indefinitely, so C-050 cannot catch it —
    /// C-050 turns a *returned* failure into a skip, and this call would never
    /// return. `Ok(None)` is "nothing comparable is there, take the write
    /// branch".
    ///
    /// RED: swap in an unbounded `std::fs::read` and watch the test hang
    /// rather than fail.
    #[cfg(unix)]
    #[test]
    fn reading_an_existing_entry_never_opens_a_planted_fifo() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let fifo = tree.home.shell_bin(DEFAULT_SHELL).join("cmake");
        mkfifo(&fifo);

        assert_eq!(
            read_existing_trampoline(&fifo, 64).expect("a FIFO is not an error, it is not comparable"),
            None,
            "S-3 — the type gate is `symlink_metadata`, so nothing is opened"
        );
    }

    /// C-047/S-3 — `read_existing_trampoline`'s other two refusals and its one
    /// acceptance, so the FIFO case above is not the only reachable branch.
    ///
    /// A symlink whose target happens to match would otherwise short-circuit
    /// to `Unchanged` and survive every render, leaving `bin/<name>`
    /// permanently a link rather than a trampoline; an over-cap regular file
    /// differs from a body of exactly `cap` bytes by definition (CWE-400).
    #[cfg(unix)]
    #[test]
    fn reading_an_existing_entry_refuses_a_symlink_and_an_over_cap_file() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let target = tree.tmp.path().join("elsewhere");
        std::fs::write(&target, b"body").unwrap();
        let linked = tree.home.shell_bin(DEFAULT_SHELL).join("linked");
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        let oversized = tree.home.shell_bin(DEFAULT_SHELL).join("oversized");
        std::fs::write(&oversized, vec![b'x'; 4096]).unwrap();
        let ordinary = tree.home.shell_bin(DEFAULT_SHELL).join("ordinary");
        std::fs::write(&ordinary, b"body").unwrap();

        assert_eq!(
            read_existing_trampoline(&linked, 4).expect("a symlink is not an error"),
            None,
            "a link is never read as if it were a trampoline"
        );
        assert_eq!(
            read_existing_trampoline(&oversized, 4).expect("an over-cap file is not an error"),
            None,
            "over-cap is the write branch, never an error"
        );
        assert_eq!(
            read_existing_trampoline(&ordinary, 4).expect("an ordinary file reads"),
            Some(b"body".to_vec()),
            "…and the acceptance branch is reachable, or the three refusals above prove nothing"
        );
        assert_eq!(
            read_existing_trampoline(&tree.home.shell_bin(DEFAULT_SHELL).join("absent"), 4)
                .expect("absence is not an error"),
            None
        );
    }

    /// C-004/C-050, case 53 — an unwritable `.gitignore` does not stop the
    /// render: the trampolines still land.
    ///
    /// RED: `?` on `ensure_gitignore` — one unwritable file in a tree the
    /// repository controls then blocks the whole feature.
    #[tokio::test]
    async fn an_unwritable_gitignore_does_not_stop_the_trampolines_from_landing() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        // A directory where the ignore file must go: `ensure_gitignore`'s
        // rename cannot land, and nothing else in the home is affected.
        std::fs::create_dir_all(tree.home.gitignore()).unwrap();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        tree.manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("C-050 — a failed `.gitignore` write is a skip, not an error");

        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["cmake"]),
            "the trampolines are the render's product; the ignore file is a convenience"
        );
    }

    /// R-W19(a)/C-4, case 54 — `ensure_home_root` runs **before**
    /// `ensure_gitignore`, and every directory it creates is owner-only-write.
    ///
    /// C-019 refuses a group-or-world-writable `toolchain_dir`, but it can
    /// only check a directory that exists — for an absent root it examines the
    /// nearest existing *ancestor*, so every directory this call creates is
    /// one C-019 never examined. `ensure_gitignore` does its own
    /// `create_dir_all` with the **ambient umask**, so reaching it with the
    /// root still absent would create that root with whatever the umask allows.
    ///
    /// RED: reorder the two calls — no other assertion in this suite notices.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_first_ever_render_creates_the_home_root_owner_only_writable() {
        let tree = Tree::new();
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();

        assert!(!tree.home.root().exists(), "precondition: this is a first-ever render");
        tree.manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the first render succeeds");

        assert_eq!(
            mode_of(tree.home.root()) & 0o077,
            0,
            "R-W19(a) — the home root is created owner-only-write, at create time, not by a \
             post-hoc chmod that leaves a group-writable window"
        );
        assert!(
            tree.home.gitignore().is_file(),
            "C-004 — …and the ignore file was still written, after the root existed"
        );
    }

    /// R-W19(a) — `ensure_home_root` reports whether **this** call created the
    /// root, and creates every missing ancestor owner-only-write.
    #[cfg(unix)]
    #[test]
    fn ensure_home_root_creates_every_missing_ancestor_and_reports_the_first_creation() {
        let tree = Tree::new();
        let deep = ToolchainHome::new(tree.tmp.path().join("relocated").join("project-key").join("toolchain"));

        assert!(
            ensure_home_root(&deep).expect("the root is creatable"),
            "the first call created it, and says so"
        );
        assert!(
            !ensure_home_root(&deep).expect("a second call is a no-op"),
            "…and the second call reports that it created nothing"
        );
        for ancestor in [
            tree.tmp.path().join("relocated"),
            tree.tmp.path().join("relocated").join("project-key"),
            deep.root().to_path_buf(),
        ] {
            assert_eq!(
                mode_of(&ancestor) & 0o077,
                0,
                "R-W19(a) — every level this call created, not only the leaf: {ancestor:?}"
            );
        }
    }

    /// C-052 and validation item 23 — rendering a **project** tree registers
    /// the project in the `projects/` GC ledger; rendering the **global** tree
    /// registers nothing.
    ///
    /// RED: register unconditionally — the global home's project dir is
    /// `$OCX_HOME`, and a self-link is barred by the no-self-link invariant.
    #[tokio::test]
    async fn a_project_render_registers_in_the_gc_ledger_and_a_global_render_does_not() {
        let tree = Tree::new();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let manager = tree.manager();

        let project_scope = tree.scope();
        manager
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &project_scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the project render succeeds");
        let after_project = read_dir_names(&tree.file_structure.root().join("projects"));
        assert_eq!(
            after_project.len(),
            1,
            "C-052 — a project render takes a GC-root ledger entry: {after_project:?}"
        );

        let global_home = tree.file_structure.toolchain.home().clone();
        manager
            .render_toolchain(RenderRequest {
                home: &global_home,
                scope: &RenderStampScope::Global,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the global render succeeds");

        assert_eq!(
            read_dir_names(&tree.file_structure.root().join("projects")),
            after_project,
            "C-052 — the global tier registers nothing; `register` is a no-op for $OCX_HOME"
        );
    }

    // ── 7. `heal_links` (C-051, C-070, RUL-29, RUL-31, RUL-36) ─────────────

    /// C-051, case 55 — an empty `groups` is a vacuous no-op:
    /// [`HealOutcome::Healed(0)`](HealOutcome::Healed), and no
    /// refusal.
    ///
    /// RED: add a refusal, putting an error on the one path C-051 says never
    /// errors — and one that would not catch the narrowing that actually bites
    /// (passing the default group when the invocation selected `ci`), which is
    /// a non-empty wrong answer.
    #[tokio::test]
    async fn healing_an_empty_group_set_is_a_vacuous_no_op() {
        let tree = Tree::new();
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'd')],
        )]);

        assert_eq!(
            heal_links(&tree.file_structure, &tree.home, &tree.scope(), &lock, &[], &platform())
                .await
                .expect("C-051 — an empty group set never errors"),
            HealOutcome::Healed(0)
        );
        assert!(
            !tree.home.root().exists(),
            "…and nothing was created for a call that had nothing to do"
        );
    }

    /// C-051, case 56 — a selected group the lock carries no entries for is
    /// [`HealOutcome::Healed(0)`](HealOutcome::Healed), and heal neither creates
    /// nor removes its directory.
    #[tokio::test]
    async fn healing_a_group_with_no_locked_entries_creates_and_removes_nothing() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'd')],
        )]);

        assert_eq!(
            heal_links(
                &tree.file_structure,
                &tree.home,
                &tree.scope(),
                &lock,
                &groups_of(&["ci"]),
                &platform()
            )
            .await
            .expect("an empty group is not an error"),
            HealOutcome::Healed(0)
        );
        assert!(
            !tree.home.links_group("ci").expect("an admitted group name").exists(),
            "heal creates no directory for a group with nothing in it"
        );
    }

    /// C-004, V-10 — healing a **fresh** home writes the ignore file.
    ///
    /// `ocx env` and `ocx exec` reach `heal_links` and never the render, so on
    /// a fresh clone with a committed `ocx.lock` and no `ocx pull` this is the
    /// only thing that creates `<project>/.ocx/toolchain` — after which RUL-29
    /// fills it with `<group>/<entry>` symlinks. Without the ignore file
    /// `git add -A` stages them, which is precisely the state C-004 exists to
    /// prevent, and `ensure_gitignore` had exactly one caller: the render.
    ///
    /// RED: drop the `ensure_gitignore` call from `heal_links` — the links
    /// below still land, so only this assertion discriminates.
    #[tokio::test]
    async fn healing_a_fresh_home_writes_the_ignore_file() {
        let tree = Tree::new();
        assert!(
            !tree.home.root().exists(),
            "the fresh-clone precondition: nothing has rendered this home yet"
        );
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'd')],
        )]);

        assert_eq!(
            heal_links(
                &tree.file_structure,
                &tree.home,
                &tree.scope(),
                &lock,
                &groups_of(&[DEFAULT_GROUP]),
                &platform(),
            )
            .await
            .expect("healing a fresh home is not an error"),
            HealOutcome::Healed(1),
            "the precondition for the assertion below: heal entered the tree and created the link"
        );
        assert!(
            tree.home.gitignore().is_file(),
            "C-004 — every path that creates the home writes the ignore file, not just the render"
        );
    }

    /// RUL-29, case 57 — an **absent** link is created, and counted.
    ///
    /// Not an edge: it is the commonest post-`git pull` state, since a lock
    /// that gained an entry has no link for it until something writes one, and
    /// on the composing path (C-070) nothing else will before the emit.
    ///
    /// RED: invert to skip — C-070's one purpose, that the composing emitter
    /// can trust the link it is about to emit, is then unsatisfied exactly
    /// when it matters.
    #[tokio::test]
    async fn healing_creates_an_absent_link_and_counts_it() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let lock = lock_of(vec![tool]);

        let repaired = heal_links(
            &tree.file_structure,
            &tree.home,
            &tree.scope(),
            &lock,
            &groups_of(&[DEFAULT_GROUP]),
            &platform(),
        )
        .await
        .expect("healing succeeds");

        assert_eq!(
            repaired,
            HealOutcome::Healed(1),
            "RUL-29 — a created link counts like a repointed one"
        );
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        assert_eq!(std::fs::read_link(&entry).unwrap(), expected_target);
    }

    /// C-051, case 58 — a `<group>/<entry>` that is a **regular file** rather
    /// than a link is left alone, counted as un-repaired, and is never an
    /// error and never deleted.
    ///
    /// RED: remove-and-recreate — heal then gains a delete path C-051 does not
    /// authorise, inside a tree the repository controls.
    #[tokio::test]
    async fn healing_leaves_a_regular_file_where_a_link_belongs_untouched() {
        let tree = Tree::new();
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(&entry, b"not a link\n").unwrap();
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'd')],
        )]);

        let repaired = heal_links(
            &tree.file_structure,
            &tree.home,
            &tree.scope(),
            &lock,
            &groups_of(&[DEFAULT_GROUP]),
            &platform(),
        )
        .await
        .expect("C-051 — a shape heal cannot repair is never an error");

        assert_eq!(
            repaired,
            HealOutcome::Healed(0),
            "the entry is uncounted because nothing was repaired"
        );
        assert_eq!(
            std::fs::read(&entry).unwrap(),
            b"not a link\n",
            "…and the file is byte-identical, not deleted"
        );
    }

    /// C-051, case 59 — a link pointing **outside** the store is repointed at
    /// the lock-derived digest root, and heal never reads *through* it.
    ///
    /// RED: compare canonicalised targets — a link into a hostile tree that
    /// happens to canonicalise onto the right place then reads as correct.
    #[cfg(unix)]
    #[tokio::test]
    async fn healing_repoints_a_link_that_targets_something_outside_the_store() {
        let tree = Tree::new();
        let hostile = tree.tmp.path().join("hostile-package");
        std::fs::create_dir_all(&hostile).unwrap();
        std::fs::write(hostile.join("payload"), b"hostile\n").unwrap();
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&hostile, &entry).unwrap();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let lock = lock_of(vec![tool]);

        let repaired = heal_links(
            &tree.file_structure,
            &tree.home,
            &tree.scope(),
            &lock,
            &groups_of(&[DEFAULT_GROUP]),
            &platform(),
        )
        .await
        .expect("healing succeeds");

        assert_eq!(repaired, HealOutcome::Healed(1));
        assert_eq!(std::fs::read_link(&entry).unwrap(), expected_target);
        assert!(
            hostile.join("payload").exists(),
            "…and the tree it used to point at is untouched: heal repoints, it does not clean up"
        );
    }

    /// C-051, case 60 — a link whose target digest root does not exist on disk
    /// is still **correct**: the lock names that digest, so heal repoints
    /// nothing.
    ///
    /// RED: add a `try_exists` gate on the target — heal then repoints (or
    /// worse, deletes) links for every package that is not currently
    /// materialised, which is the ordinary state of a lazily-loaded tool.
    #[tokio::test]
    async fn healing_leaves_a_correct_link_alone_even_when_its_target_does_not_exist() {
        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        ocx_util::fs::symlink::create(&expected_target, &entry).expect("a dangling link is creatable");
        assert!(
            !expected_target.exists(),
            "precondition: the package is not materialised, which is the lazy tool's ordinary state"
        );
        let lock = lock_of(vec![tool]);

        assert_eq!(
            heal_links(
                &tree.file_structure,
                &tree.home,
                &tree.scope(),
                &lock,
                &groups_of(&[DEFAULT_GROUP]),
                &platform()
            )
            .await
            .expect("healing succeeds"),
            HealOutcome::Healed(0),
            "C-051 — the comparison is against the lock, never against the filesystem"
        );
    }

    /// RUL-31, case 61 — render and heal select the platform through the same
    /// `select_best` helper, never an exact key lookup: (a) an exact key
    /// resolves, (b) a **compatible but non-identical** key resolves the same
    /// way the render did, and (c) no compatible key skips the entry silently.
    ///
    /// RED: exact-key lookup. Case (b) then makes the render write one target
    /// and the heal repoint it to another, on every invocation, forever — and
    /// the stamp's `link_fingerprint` never matches for two consecutive runs.
    #[tokio::test]
    async fn render_and_heal_agree_on_a_compatible_non_identical_platform_key() {
        let tree = Tree::new();
        tree.create_home_root();

        // The lock carries the bare `linux/amd64`; the request carries a
        // feature-bearing host that only `is_compatible` relates to it.
        let host: ocx_oci::Platform = "linux/amd64+libc.glibc"
            .parse()
            .expect("the fixture host platform is canonical");
        let exact = locked_tool("exact", DEFAULT_GROUP, "ns/exact", &[("linux/amd64+libc.glibc", 'e')]);
        let compatible = locked_tool("compatible", DEFAULT_GROUP, "ns/compatible", &[("linux/amd64", 'c')]);
        let incompatible = locked_tool(
            "incompatible",
            DEFAULT_GROUP,
            "ns/incompatible",
            &[("windows/amd64", 'w')],
        );
        let expected_exact = {
            let leaf = exact.host_leaf(&host).expect("exact resolves");
            tree.file_structure.packages.path(&exact.repository.pin_untagged(leaf))
        };
        let expected_compatible = {
            let leaf = compatible.host_leaf(&host).expect("compatible resolves");
            tree.file_structure
                .packages
                .path(&compatible.repository.pin_untagged(leaf))
        };
        let lock = lock_of(vec![exact, compatible, incompatible]);
        let scope = tree.scope();
        let groups = groups_of(&[DEFAULT_GROUP]);

        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &host,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        let entry_of = |name: &str| tree.home.entry(DEFAULT_GROUP, name).expect("an admitted pair");
        assert_eq!(std::fs::read_link(entry_of("exact")).unwrap(), expected_exact);
        assert_eq!(
            std::fs::read_link(entry_of("compatible")).unwrap(),
            expected_compatible,
            "RUL-31 — a compatible non-identical key resolves through `select_best`"
        );
        assert!(
            std::fs::symlink_metadata(entry_of("incompatible")).is_err(),
            "…and no compatible key skips the entry silently"
        );
        assert!(
            report
                .items
                .iter()
                .all(|item| item.artifact != link(DEFAULT_GROUP, "incompatible")),
            "…with no `Skipped` item and no error: {report:?}"
        );

        assert_eq!(
            heal_links(&tree.file_structure, &tree.home, &tree.scope(), &lock, &groups, &host)
                .await
                .expect("healing succeeds"),
            HealOutcome::Healed(0),
            "RUL-31 — the heal agrees with the render, so nothing oscillates"
        );
    }

    /// C-051, case 62 — **purity**, asserted behaviourally: an offline
    /// `FileStructure` with no client, no index and nothing installed still
    /// heals.
    ///
    /// A source-text grep for "no compose" would be a denylist that cannot
    /// enumerate every way to reach the network. Building the call out of a
    /// store that has no client at all is what makes the property structural.
    ///
    /// RED: resolve the entry through an install path "to be safe".
    #[tokio::test]
    async fn healing_is_pure_lock_arithmetic_over_a_store_with_no_client() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let lock = lock_of(vec![tool]);
        assert!(
            !tree.file_structure.packages.root().exists(),
            "precondition: nothing is installed, so a heal that resolved through an install path \
             could not answer at all"
        );

        assert_eq!(
            heal_links(
                &tree.file_structure,
                &tree.home,
                &tree.scope(),
                &lock,
                &groups_of(&[DEFAULT_GROUP]),
                &platform()
            )
            .await
            .expect("C-051 — no compose, no metadata read, no network"),
            HealOutcome::Healed(1)
        );
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        assert_eq!(std::fs::read_link(&entry).unwrap(), expected_target);
    }

    /// RUL-36 — a link heal cannot repair leaves the entry unrepaired,
    /// uncounted and logged; the call still returns
    /// [`HealOutcome::Healed`] — a per-entry degrade, never a whole-tree
    /// refusal.
    ///
    /// C-070 puts this function on `ocx env`'s and `ocx exec`'s critical path,
    /// so an ordinary contention turned into a failed command would break the
    /// path whose whole design premise is that it degrades instead.
    ///
    /// RED: propagate the failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn healing_never_errors_when_it_cannot_write() {
        let tree = Tree::new();
        let group_directory = tree
            .home
            .links_group(DEFAULT_GROUP)
            .expect("the default group is admitted");
        std::fs::create_dir_all(&group_directory).unwrap();
        deny_writes(&group_directory);

        let lock = lock_of(vec![
            locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]),
            locked_tool("ninja", DEFAULT_GROUP, "ns/ninja", &[(PLATFORM_KEY, 'n')]),
        ]);
        let result = heal_links(
            &tree.file_structure,
            &tree.home,
            &tree.scope(),
            &lock,
            &groups_of(&[DEFAULT_GROUP]),
            &platform(),
        )
        .await;
        allow_writes(&group_directory);

        assert_eq!(
            result.expect("RUL-36 — a repair that cannot land is never an error"),
            HealOutcome::Healed(0),
            "…and the unrepaired entries are uncounted, because the count is repairs that landed"
        );
    }

    /// RUL-36/RUL-38, case 64's contention clause — an entry whose per-repoint
    /// lock is held elsewhere is left unrepaired and **uncounted**, its
    /// uncontended sibling is repaired, and the call returns
    /// [`HealOutcome::Healed`].
    ///
    /// C-070 puts this function on `ocx env`'s and `ocx exec`'s critical path,
    /// so ordinary contention must degrade per entry rather than fail the
    /// command — the composing side turns an unrepaired entry into a digest
    /// path (C-067).
    ///
    /// Driven through [`heal_lock_parameters`] so the test contends on *the
    /// same* lock. It costs the lock's own timeout in wall-clock, for the same
    /// reason case 52 does — and shortens it through the same seam.
    ///
    /// RED: propagate the failure — one busy link then fails the whole compose.
    #[tokio::test]
    async fn healing_leaves_a_contended_entry_unrepaired_and_uncounted() {
        let environment = ocx_util::env::overrides::lock();
        environment.set(TESTING_LOCK_TIMEOUT_ENV, "150");

        let tree = Tree::new();
        // The per-entry lock keys on the group directory's file identity.
        std::fs::create_dir_all(tree.home.links_group(DEFAULT_GROUP).expect("admitted")).unwrap();

        let contended = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let free = locked_tool("ninja", DEFAULT_GROUP, "ns/ninja", &[(PLATFORM_KEY, 'n')]);
        let free_target = expected_link_target(&tree.file_structure, &free);
        let lock = lock_of(vec![contended, free]);

        let group_directory = tree
            .home
            .links_group(DEFAULT_GROUP)
            .expect("the default group is admitted");
        let _held = heal_lock_parameters(&tree.file_structure, group_directory, "cmake")
            .acquire()
            .await
            .expect("the test can take the same per-entry lock");

        let started = std::time::Instant::now();
        let repaired = heal_links(
            &tree.file_structure,
            &tree.home,
            &tree.scope(),
            &lock,
            &groups_of(&[DEFAULT_GROUP]),
            &platform(),
        )
        .await
        .expect("RUL-36 — contention is never an error");
        let elapsed = started.elapsed();

        assert!(
            elapsed >= toolchain_lock_timeout(),
            "the repair actually waited out its own lock timeout rather than skipping the entry \
             for another reason: {elapsed:?} < {:?}",
            toolchain_lock_timeout()
        );
        assert_eq!(
            repaired,
            HealOutcome::Healed(1),
            "the contended entry is uncounted, because the count is repairs that landed"
        );
        assert!(
            std::fs::symlink_metadata(tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair")).is_err(),
            "…and nothing was written for it"
        );
        assert_eq!(
            std::fs::read_link(tree.home.entry(DEFAULT_GROUP, "ninja").expect("an admitted pair")).unwrap(),
            free_target,
            "…while its uncontended sibling was repaired in the same call"
        );
    }

    // ── 8. `--dry-run` (C-049, S-007, RUL-28) ───────────────────────────────

    /// C-049, case 66 — "wrote nothing" asserted with the right instrument: a
    /// `(path, file type, bytes, mtime_nsec, inode)` snapshot of the whole
    /// home subtree, before and after, compared as sets.
    ///
    /// Empty stdout, a zero file count and "the file I checked is unchanged"
    /// are all silent-negative shapes here. A dry run that called
    /// `ensure_gitignore` would satisfy a bytes-only or count-only assertion
    /// and red only this one.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dry_run_leaves_every_object_in_the_home_bit_for_bit_identical() {
        let tree = Tree::new();
        tree.create_home_root();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the seeding render succeeds");
        // Something for the dry run to *want* to change, so the snapshot is
        // not comparing two empty trees.
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("stale"), b"#!/bin/sh\n").unwrap();
        let before = snapshot_subtree(tree.home.root());

        let report = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: true,
            })
            .await
            .expect("the dry run succeeds");

        assert_eq!(
            snapshot_subtree(tree.home.root()),
            before,
            "C-049 — nothing under the home changed identity, content or timestamp"
        );
        assert_eq!(
            outcome_of(&report, &trampoline("stale")),
            &RenderOutcome::Pruned,
            "…while the report still names the delta it *would* have applied"
        );
    }

    /// S-007/C-049, case 67 — a dry run over a **poisoned** link performs no
    /// heal: `readlink` is byte-identical afterwards, and the report names the
    /// delta.
    ///
    /// RED: call `heal_links` from the dry-run arm — the run then reports a
    /// delta it has itself already closed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dry_run_performs_no_heal_over_a_poisoned_link() {
        let tree = Tree::new();
        tree.create_home_root();
        let poison = tree.tmp.path().join("poison");
        std::fs::create_dir_all(&poison).unwrap();
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&poison, &entry).unwrap();

        let scope = tree.scope();
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'd')],
        )]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: true,
            })
            .await
            .expect("the dry run succeeds");

        assert_eq!(
            std::fs::read_link(&entry).unwrap(),
            poison,
            "S-007 — the poisoned link is still present afterwards"
        );
        assert_eq!(
            outcome_of(&report, &link(DEFAULT_GROUP, "cmake")),
            &RenderOutcome::Written,
            "…and the delta is reported as the write it would have performed"
        );
    }

    /// RUL-28/S-5, case 68 — a dry run on a never-rendered home creates
    /// **nothing**: no home root, no `state/projects/<key>/`, and no
    /// `projects/` GC ledger entry.
    ///
    /// The ledger is the case worth naming: registration is best-effort and
    /// costs nothing, so "register anyway" is tempting. It would make
    /// `ocx pull --dry-run` a GC-visible mutation of a store the user asked it
    /// not to touch.
    ///
    /// RED: hoist `ensure_home_root` above the `dry_run` branch.
    #[tokio::test]
    async fn a_dry_run_on_a_never_rendered_home_creates_nothing_at_all() {
        let tree = Tree::new();
        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();

        let report = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: true,
            })
            .await
            .expect("the dry run succeeds");

        assert!(!tree.home.root().exists(), "no home root");
        assert!(
            !tree.file_structure.state.project_state_dir(&tree.key()).exists(),
            "no per-project state directory"
        );
        assert!(
            read_dir_names(&tree.file_structure.root().join("projects")).is_empty(),
            "RUL-28 — and no GC ledger entry"
        );
        assert!(!report.stamp_written);
    }

    /// C-049, case 69 — the dry run **predicts what a real run applies**: the
    /// two `items` sets are equal modulo `Written`/`Unchanged`.
    ///
    /// RED: build the dry-run delta from a different code path than the write
    /// pass — the two then drift silently, and `--dry-run` becomes advice
    /// about a render that never happens.
    #[tokio::test]
    async fn a_dry_run_predicts_exactly_what_the_real_run_applies() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("stale"), b"#!/bin/sh\n").unwrap();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let manager = tree.manager();

        let predicted = manager
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: true,
            })
            .await
            .expect("the dry run succeeds");
        let applied = manager
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the real run succeeds");

        let normalise = |report: &RenderReport| {
            let mut rows: Vec<(RenderedArtifact, bool)> = report
                .items
                .iter()
                .map(|item| (item.artifact.clone(), is_skipped(&item.outcome)))
                .collect();
            rows.sort_by_key(|(artifact, _)| format!("{artifact:?}"));
            rows
        };
        assert_eq!(
            normalise(&predicted),
            normalise(&applied),
            "C-049 — the same delta, from the same code path"
        );
        assert_eq!(predicted.bin_in_scope, applied.bin_in_scope);
    }

    /// C-049, case 70 — `stamp_written` is always `false` under a dry run,
    /// **and the existing stamp file's mtime is unchanged**.
    ///
    /// The mtime is the discriminating half: validation item 32 makes the
    /// stamp's *fingerprint* mtime-independent, which is exactly what hides a
    /// dry run that rewrote the stamp with identical content. Asserting the
    /// *file's* mtime is what catches it.
    ///
    /// RED: write the stamp under dry run.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dry_run_writes_no_stamp_and_does_not_even_touch_the_existing_one() {
        let tree = Tree::new();
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let manager = tree.manager();

        manager
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the seeding render writes a stamp");
        let before = snapshot_subtree(&tree.file_structure.state.project_state_dir(&tree.key()));
        assert!(!before.is_empty(), "precondition: a stamp exists to be left alone");

        let report = manager
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: true,
            })
            .await
            .expect("the dry run succeeds");

        assert!(
            !report.stamp_written,
            "C-049 — a dry run wrote no tree, so it stamps nothing"
        );
        assert_eq!(
            snapshot_subtree(&tree.file_structure.state.project_state_dir(&tree.key())),
            before,
            "…and the existing stamp file's own mtime and inode are untouched"
        );
    }

    // ── 12. The review round's regressions (B1…B5, W8) ──────────────────────

    /// The directory's own `mtime`, in nanoseconds.
    ///
    /// The one observation that catches a file **created and then unlinked**
    /// inside a directory: a subtree snapshot of that directory cannot, because
    /// the entry is gone by the time it is taken, and the litter is the point
    /// of the claim, not the leftover.
    #[cfg(unix)]
    #[track_caller]
    fn dir_mtime_nsec(path: &Path) -> i64 {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::symlink_metadata(path)
            .unwrap_or_else(|e| panic!("{path:?} must exist: {e}"))
            .mtime_nsec()
    }

    /// B1/RUL-33 — `heal_links` refuses a home root that is a symlink out of
    /// the project, and creates **nothing** in the link's target.
    ///
    /// C-070 puts heal on every composing emit, so `ocx env` and `ocx exec`
    /// reach this on every prompt: without the guard a hostile clone that
    /// commits `.ocx/toolchain` as a symlink to `$HOME` turns the per-prompt
    /// path into a write primitive outside the project, since RUL-29 has heal
    /// *create* absent links and `repoint_link` `create_dir_all`s the group
    /// directory under them.
    ///
    /// The second half is the positive control: without it a `heal_links` that
    /// refused every home would pass the first half.
    ///
    /// RED: drop the `ensure_home_root` call at the top of `heal_links`.
    #[cfg(unix)]
    #[tokio::test]
    async fn healing_refuses_a_home_root_that_is_a_symlink_out_of_the_project() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(tree.home.root().parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, tree.home.root()).unwrap();
        let before = snapshot_subtree(&outside);

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);

        assert!(
            matches!(
                heal_links(
                    &tree.file_structure,
                    &tree.home,
                    &tree.scope(),
                    &lock,
                    &groups,
                    &platform()
                )
                .await
                .expect("RUL-36 — a refusal degrades, it never errors"),
                HealOutcome::Refused { .. }
            ),
            "an unrepairable home repairs nothing — and says so, rather than answering what a \
             clean pass answers"
        );
        assert_eq!(
            snapshot_subtree(&outside),
            before,
            "RUL-33 — and nothing was written into the symlink's target"
        );

        // The positive control: an ordinary home still heals.
        let honest = Tree::new();
        std::fs::create_dir_all(honest.home.root()).unwrap();
        assert_eq!(
            heal_links(
                &honest.file_structure,
                &honest.home,
                &honest.scope(),
                &lock,
                &groups,
                &platform()
            )
            .await
            .expect("an ordinary home heals"),
            HealOutcome::Healed(1),
            "the guard must not refuse a legitimate home — three refusals are otherwise \
             satisfied by a function that refuses everything"
        );
    }

    /// B2/C-050 — `publish_link` refuses a `<group>/` directory that is a
    /// symlink, and destroys nothing in its target.
    ///
    /// `prune_within` already refuses an entry reached this way (case 25(c));
    /// the write side had no counterpart, and `ensure_home_root` guards only
    /// the home root and `bin/`. `create_dir_all` succeeds silently through the
    /// link and `replace_atomic`'s POSIX arm is a `rename` that **replaces an
    /// existing regular file** — so a bare `ocx pull` destroyed an
    /// attacker-chosen file outside the home.
    ///
    /// RED: **two** mutations, because two guards now defend this and either
    /// alone leaves the row green — drop the `symlink_metadata(parent)` refusal
    /// in `publish_link_within` *and* give `ensure_link_group` back the
    /// recursive `create_owner_only`, whose `create_dir_all` follows the link.
    /// `outside/cmake` is then replaced by a symlink.
    #[cfg(unix)]
    #[tokio::test]
    async fn publishing_a_link_refuses_a_symlinked_group_directory() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("cmake"), b"someone else's file\n").unwrap();
        std::fs::create_dir_all(links_root(&tree.home)).unwrap();
        std::os::unix::fs::symlink(&outside, links_root(&tree.home).join(DEFAULT_GROUP)).unwrap();
        let before = snapshot_subtree(&outside);

        let target = tree.tmp.path().join("package-root");
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        let outcome = publish_link(&entry, &target, false).await;

        assert!(
            is_skipped(&outcome),
            "C-050 — a symlinked group directory is a skip, not a write and not an error; got {outcome:?}"
        );
        assert_eq!(
            snapshot_subtree(&outside),
            before,
            "…and the file the rename would have replaced is byte-for-byte intact"
        );

        // The positive control: a real group directory is still written.
        let honest = tree.home.links_group("ci").expect("an admitted group name");
        std::fs::create_dir_all(&honest).unwrap();
        let outcome = publish_link(&honest.join("cmake"), &target, false).await;
        assert_eq!(
            outcome,
            RenderOutcome::Written,
            "the guard must not refuse an ordinary group directory"
        );
        assert_eq!(std::fs::read_link(honest.join("cmake")).unwrap(), target);
    }

    /// S1 — `publish_link` refuses a symlinked **`links/`**, one level above
    /// the group directory it already refuses.
    ///
    /// `links` was the one tree-own directory nothing ever created under a
    /// guard: it came into existence only as `create_dir_all(<root>/links/
    /// <group>)`'s side effect, so its only check was `ensure_home_root`'s at
    /// step 2 — and the render lock's unbounded wait sits between that check and
    /// this write. A local writer that swaps `links` for a symlink inside that
    /// window sends the group create and the entry's `replace_atomic` into an
    /// attacker-chosen directory. `bin/` and `shells/` already re-judge at the
    /// moment of use; this is the third level joining them.
    ///
    /// **The plant stands in for that swap.** Through the public render entry,
    /// step 2 refuses a pre-existing symlinked `links` and the whole render
    /// skips, so the write-side re-judgement is only reachable by calling the
    /// write directly — which is precisely the post-step-2 state a concurrent
    /// writer produces. The positive control is the whole rest of this module:
    /// every render row publishes through a real `links/`, so a guard that
    /// refused everything could not stay green here.
    ///
    /// RED: give `ensure_link_group` back the recursive `create_owner_only` —
    /// `create_dir_all` follows the link, `outside/` gains the group directory,
    /// and the entry is published inside it.
    #[cfg(unix)]
    #[tokio::test]
    async fn publishing_a_link_refuses_a_symlinked_links_directory() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("bystander"), b"someone else's file\n").unwrap();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        std::os::unix::fs::symlink(&outside, links_root(&tree.home)).unwrap();
        let before = snapshot_subtree(&outside);

        let target = tree.tmp.path().join("package-root");
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        let outcome = publish_link(&entry, &target, false).await;

        assert!(
            is_skipped(&outcome),
            "a symlinked `links/` is a skip, not a write and not an error; got {outcome:?}"
        );
        assert_eq!(
            snapshot_subtree(&outside),
            before,
            "…and nothing was created inside the directory that link points at"
        );
    }

    /// B2/RUL-36 — the same refusal on the heal side: `repoint_link` never
    /// writes through a symlinked `<group>/`, and the entry stays uncounted.
    ///
    /// The seeded entry is **absent**, deliberately: RUL-29 has heal *create*
    /// one, and that is the only shape that reaches `repoint_link` at all. With
    /// a regular file planted at `<group>/<entry>` instead, heal's own
    /// shape probe (`entry.exists() && !is_link`) skips the entry before the
    /// group directory is ever touched — a second guard that makes this
    /// mutation green for the wrong reason.
    ///
    /// RED: give `ensure_link_group` back the recursive `create_owner_only` —
    /// `create_dir_all` follows the link, and the repoint lands in its target.
    #[cfg(unix)]
    #[tokio::test]
    async fn healing_refuses_a_symlinked_group_directory() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("bystander"), b"someone else's file\n").unwrap();
        std::fs::create_dir_all(links_root(&tree.home)).unwrap();
        std::os::unix::fs::symlink(&outside, links_root(&tree.home).join(DEFAULT_GROUP)).unwrap();
        let before = snapshot_subtree(&outside);

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let lock = lock_of(vec![tool]);

        assert_eq!(
            heal_links(
                &tree.file_structure,
                &tree.home,
                &tree.scope(),
                &lock,
                &groups_of(&[DEFAULT_GROUP]),
                &platform()
            )
            .await
            .expect("RUL-36 — never an error"),
            HealOutcome::Healed(0),
            "RUL-36 — an entry that cannot be repaired is uncounted, and the tree was still \
             entered: this is a per-entry degrade, not a whole-tree refusal"
        );
        assert_eq!(
            snapshot_subtree(&outside),
            before,
            "…and nothing outside the home was created, written or replaced"
        );

        // The positive control: a real group directory still gets its link.
        let honest = Tree::new();
        std::fs::create_dir_all(honest.home.root()).unwrap();
        assert_eq!(
            heal_links(
                &honest.file_structure,
                &honest.home,
                &honest.scope(),
                &lock,
                &groups_of(&[DEFAULT_GROUP]),
                &platform()
            )
            .await
            .expect("an ordinary home heals"),
            HealOutcome::Healed(1),
            "RUL-29 — the guard must not refuse an ordinary group directory"
        );
    }

    /// B3/RUL-46, the `bin/` half — on a **case-insensitive** home the prune
    /// comparison is ASCII-case-folded, so a pre-existing entry differing from
    /// the winner only in case is **not** removed.
    ///
    /// The bug this pins is data loss, and it needs the write pass and the
    /// prune pass read together. `expected` holds the winner's *original*
    /// spelling (RUL-19), so with `Make` winning and `bin/make` already on
    /// disk: on a case-insensitive filesystem the two names are one file, the
    /// write pass finds the body already correct and leaves it `Unchanged`
    /// under its on-disk name `make`, and a case-**sensitive** prune then sees
    /// `make` ∉ {`Make`} and removes it. `fingerprint_bin` afterwards stats a
    /// file that is gone, so the stamp records an empty name set that *matches*
    /// the now-empty `bin/` — C-061's gate is satisfied while the tool has
    /// silently vanished.
    ///
    /// On the case-**sensitive** filesystem this test runs on, `bin/make` and
    /// `bin/Make` are two files, so the end state differs; what is pinned here
    /// is the rule that prevents the loss — the folded entry survives the prune
    /// and is not reported — plus its gate, in the second half.
    ///
    /// RED: use `expected.contains(&name)` in `reconcile_bin`'s prune loop.
    #[tokio::test]
    async fn a_differently_cased_bin_entry_survives_the_prune_on_a_case_insensitive_home() {
        for case_insensitive in [true, false] {
            let tree = Tree::new();
            tree.create_home_root();
            // The row's premise, read rather than presupposed: `bin/make` is
            // the winner `Make`'s *twin* only where the two are separate files.
            // On a case-insensitive volume they are one, the injected flag stops
            // describing the disk it is asserted against, and neither arm's end
            // state is the one written down above.
            if !tree.holds_case_twins().await {
                eprintln!(
                    "skipped: the probe reported a case-insensitive home, so `make` and `Make` cannot \
                     be two files here"
                );
                return;
            }
            std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
            let twin = tree.home.shell_bin(DEFAULT_SHELL).join("make");
            std::fs::write(&twin, b"#!/bin/sh\n# already here\n").unwrap();

            let surface = vec![node(pinned("ns/make", 'a'), Some(&["Make"]), &[])];
            let scope = tree.scope();
            let lock = lock_of(Vec::new());
            let groups = groups_of(&[DEFAULT_GROUP]);
            let platform = platform();
            let report = render_with(
                &tree.file_structure,
                RenderRequest {
                    home: &tree.home,
                    scope: &scope,
                    lock: &lock,
                    surface: &surface,
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                },
                case_insensitive,
                None,
            )
            .await
            .expect("the render is not a refusal");

            let pruned = report
                .items
                .iter()
                .any(|item| item.artifact == trampoline("make") && item.outcome == RenderOutcome::Pruned);
            assert_eq!(
                twin.exists(),
                case_insensitive,
                "RUL-46 — `make` is the winner `Make` itself on a case-insensitive home and must \
                 survive; on a case-sensitive one it is an ordinary orphan and must not \
                 (case_insensitive = {case_insensitive})"
            );
            assert_eq!(
                pruned, !case_insensitive,
                "…and the report says the same thing (case_insensitive = {case_insensitive})"
            );
        }
    }

    /// B3/RUL-46, the `<group>/<entry>` half — the identical shape in
    /// `reconcile_links`, with the same gate.
    ///
    /// RED: use `expected.contains(&name)` in `reconcile_links`' prune loop.
    #[tokio::test]
    async fn a_differently_cased_group_entry_survives_the_prune_on_a_case_insensitive_home() {
        for case_insensitive in [true, false] {
            let tree = Tree::new();
            tree.create_home_root();
            // As in the `bin/` half above: `CMAKE` is `cmake`'s twin only where
            // the two are separate entries.
            if !tree.holds_case_twins().await {
                eprintln!(
                    "skipped: the probe reported a case-insensitive home, so `cmake` and `CMAKE` \
                     cannot be two entries here"
                );
                return;
            }
            let group = tree.home.links_group(DEFAULT_GROUP).expect("an admitted group name");
            std::fs::create_dir_all(&group).unwrap();
            let twin = group.join("CMAKE");
            ocx_util::fs::symlink::create(tree.tmp.path().join("some-package"), &twin).unwrap();

            let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
            let scope = tree.scope();
            let lock = lock_of(vec![tool]);
            let groups = groups_of(&[DEFAULT_GROUP]);
            let platform = platform();
            let report = render_with(
                &tree.file_structure,
                RenderRequest {
                    home: &tree.home,
                    scope: &scope,
                    lock: &lock,
                    surface: &[],
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                },
                case_insensitive,
                None,
            )
            .await
            .expect("the render is not a refusal");

            let pruned = report
                .items
                .iter()
                .any(|item| item.artifact == link(DEFAULT_GROUP, "CMAKE") && item.outcome == RenderOutcome::Pruned);
            assert_eq!(
                ocx_util::fs::symlink::is_link(&twin),
                case_insensitive,
                "RUL-46 — on a case-insensitive home `CMAKE` *is* the entry `cmake` the write pass \
                 just repointed (case_insensitive = {case_insensitive})"
            );
            assert_eq!(
                pruned, !case_insensitive,
                "…and the report says the same thing (case_insensitive = {case_insensitive})"
            );
        }
    }

    /// RUL-74/C-049 — a dry run writes no case-fold probe file **anywhere**:
    /// not into the project checkout of a never-rendered home, and not into a
    /// home root that already exists either.
    ///
    /// The shipped shape handed the probe `nearest_existing_directory(root)`,
    /// which for a never-rendered project home is the checkout itself — and
    /// `$HOME` when a configured `toolchain_dir`'s parents are absent — so
    /// every `ocx pull --dry-run` created and unlinked a
    /// `.ocx-case-probe-<pid>-<nanos>` there. Narrowing it to "probe only an
    /// existing home root" fixed the litter and kept the write: the probe still
    /// created and unlinked a file *inside* the home, moving that directory's
    /// `mtime` on every dry run, which `ocx pull --dry-run` over an already
    /// rendered tree reports as a changed path. C-049 is "writes nothing", and
    /// that beats an exact prediction in an edge case.
    ///
    /// The directory's `mtime` is the discriminating observation: the probe
    /// removes its own file, so a subtree snapshot taken afterwards is clean in
    /// both worlds. The third block is the positive control — it calls the
    /// probe directly and shows the `mtime` *does* move, so the two negatives
    /// above are the probe not running rather than the instrument not reading.
    ///
    /// RED: probe `request.home.root()` under `dry_run`, in either shape.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dry_run_writes_no_case_probe_anywhere() {
        let tree = Tree::new();
        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let manager = tree.manager();
        let request = || RenderRequest {
            home: &tree.home,
            scope: &scope,
            lock: &lock,
            surface: &surface,
            groups: &groups,
            pinned: false,
            platform: &platform,
            dry_run: true,
        };

        assert!(!tree.home.root().exists(), "precondition: the home was never rendered");
        let before = snapshot_subtree(&tree.project_dir);
        let before_mtime = dir_mtime_nsec(&tree.project_dir);

        manager.render_toolchain(request()).await.expect("the dry run succeeds");

        assert_eq!(
            snapshot_subtree(&tree.project_dir),
            before,
            "C-049 — the dry run left no object in the project checkout"
        );
        assert_eq!(
            dir_mtime_nsec(&tree.project_dir),
            before_mtime,
            "…and created none there either: a probe file that is created and unlinked is \
             invisible to a snapshot but moves the directory's own mtime"
        );

        // A home root that exists is not a licence to write into it either.
        tree.create_home_root();
        let seeded_mtime = dir_mtime_nsec(tree.home.root());
        manager.render_toolchain(request()).await.expect("the dry run succeeds");
        assert_eq!(
            dir_mtime_nsec(tree.home.root()),
            seeded_mtime,
            "RUL-74 — the probe writes into the directory it judges, so probing an existing home \
             root is still a write C-049 forbids"
        );

        // The positive control: the probe does move the mtime this test reads,
        // so the two negatives above are the probe not running — not
        // `dir_mtime_nsec` failing to observe it.
        filesystem_is_case_insensitive(tree.home.root())
            .await
            .expect("the probe answers for a directory it may write in");
        assert_ne!(
            dir_mtime_nsec(tree.home.root()),
            seeded_mtime,
            "the probe moves the home root's mtime — without this, a `dir_mtime_nsec` that never \
             changed would pass both assertions above"
        );
    }

    /// B5/C-003 — the Windows `Unchanged` predicate requires `<name>.exe` to
    /// **be** the published `ShimBinStore` blob, not merely to be a regular
    /// file beside a matching sidecar.
    ///
    /// The sidecar body is one line — the absolute project root plus `\n` — so
    /// it is predictable on a CI runner. With the existence-only predicate, a
    /// hostile clone shipping a substituted `bin/<name>.exe` beside a matching
    /// `<name>.exec` was reported `Unchanged`, never republished, landed in
    /// `landed`, was stamped by `fingerprint_bin` and then blessed by C-061's
    /// gate — defeating the ADR's R1 mitigation, whose whole claim is that the
    /// first `ocx pull` renders over it.
    ///
    /// Runs on every host: the predicate is deliberately not `#[cfg(windows)]`,
    /// for the reason `publish_windows_trampoline` states — a `cfg`-gated seam
    /// puts the property behind a `cfg` the CI leg that actually runs never
    /// compiles.
    ///
    /// RED: return `symlink_metadata(exe).is_ok_and(|m| m.is_file())` for the
    /// `Some(blob)` arm.
    #[test]
    fn the_windows_unchanged_predicate_requires_the_exe_to_be_the_published_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = tmp.path().join("0123abcd.exe");
        std::fs::write(&blob, b"MZ\x00the published shim blob\n").unwrap();
        let exe = tmp.path().join("cmake.exe");
        let sidecar_path = tmp.path().join("cmake.exec");
        let sidecar = "/home/someone/project\n";
        std::fs::write(&sidecar_path, sidecar).unwrap();

        // The positive control, first: a hardlink of the blob beside a matching
        // sidecar is exactly what the render publishes, and republishing it on
        // every invocation would break C-047's idempotence.
        std::fs::hard_link(&blob, &exe).unwrap();
        assert!(
            windows_pair_unchanged(&exe, &sidecar_path, sidecar, Some(&blob)).unwrap(),
            "the pair this render would publish is `Unchanged`"
        );

        // A substituted executable beside the same matching sidecar.
        std::fs::remove_file(&exe).unwrap();
        std::fs::write(&exe, b"MZ\x00attacker-authored\n").unwrap();
        assert!(
            !windows_pair_unchanged(&exe, &sidecar_path, sidecar, Some(&blob)).unwrap(),
            "C-003 — a substituted `.exe` takes the write branch, whatever the sidecar says"
        );

        // A byte-identical *copy* is still not the blob: #301's property is one
        // inode per store, so an `ocx` upgrade that re-signs the blob must
        // reach every published name.
        std::fs::copy(&blob, &exe).unwrap();
        assert!(
            !windows_pair_unchanged(&exe, &sidecar_path, sidecar, Some(&blob)).unwrap(),
            "the predicate is file identity, not content equality"
        );

        // And the sidecar half still decides on its own.
        std::fs::remove_file(&exe).unwrap();
        std::fs::hard_link(&blob, &exe).unwrap();
        std::fs::write(&sidecar_path, "/somewhere/else\n").unwrap();
        assert!(
            !windows_pair_unchanged(&exe, &sidecar_path, sidecar, Some(&blob)).unwrap(),
            "a sidecar naming another project is not this render's pair"
        );
    }

    /// W6/R-W19 — the POSIX `Unchanged` predicate decides on the mode as well
    /// as the bytes: a clone committing `bin/<name>` with the exact expected
    /// body at git mode `100644` is republished, not left non-executable
    /// forever.
    ///
    /// RED: drop `&& has_execute_bit(&path)` from the POSIX arm.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_committed_trampoline_body_without_its_execute_bit_is_republished() {
        use std::os::unix::fs::PermissionsExt as _;

        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let entry = tree.home.shell_bin(DEFAULT_SHELL).join("cmake");
        let body = expected_unix_trampoline_body(&tree.project_dir, &tree.ocx_binary);
        std::fs::write(&entry, body.as_bytes()).unwrap();
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o644)).unwrap();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render is not a refusal");

        assert_eq!(
            outcome_of(&report, &trampoline("cmake")),
            &RenderOutcome::Written,
            "R-W19 — matching bytes at a non-executable mode are not `Unchanged`"
        );
        assert!(
            mode_of(&entry) & 0o111 != 0,
            "…and the republished entry carries an execute bit"
        );
    }

    /// W5/R-W19(a) — **every** directory a render creates is owner-only at
    /// create time, like the home root, and never with the ambient umask.
    ///
    /// The trampoline directory is what all three PATH routes point at, so a
    /// group-writable one is a write primitive into everything the user's shell
    /// resolves — and `links/`, `shells/` and `shells/<shell>/` are each an
    /// ancestor of something on that path, so a group-writable one of those is
    /// a rename primitive over the whole subtree below it. This layout added
    /// three directories and nothing mode-checked them.
    ///
    /// RED: restore `tokio::fs::create_dir_all` in `reconcile_bin` and
    /// `std::fs::create_dir_all` in `publish_link_within` — under the ordinary
    /// `umask 002` both come out group-writable. RED for the three new rows:
    /// give `ensure_shell_tree` a plain `std::fs::create_dir_all`.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_directory_a_render_creates_is_owner_only() {
        let tree = Tree::new();
        // Not `create_home_root`: this row is about the modes the **render**
        // chooses, and a fixture that pre-created `shells/<shell>` with the
        // ambient umask would answer for itself.
        std::fs::create_dir_all(tree.home.root()).unwrap();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        // The **public** entry, so step 6 creates `shells/` and
        // `shells/<shell>/` under the code that chooses their modes.
        tree.manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the render is not a refusal");

        for directory in [
            tree.home.shell_bin(DEFAULT_SHELL),
            tree.shell_directory(),
            tree.home
                .links_group(DEFAULT_GROUP)
                .expect("the default group is admitted"),
            links_root(&tree.home),
            shell_directory_of(&tree.home)
                .parent()
                .expect("the shell directory has a parent")
                .to_path_buf(),
        ] {
            assert!(directory.is_dir(), "precondition: {directory:?} was created");
            assert_eq!(
                mode_of(&directory) & 0o077,
                0,
                "R-W19(a) — {directory:?} must grant nothing to group or other"
            );
            assert!(
                mode_of(&directory) & 0o700 == 0o700,
                "…while staying fully usable by its owner"
            );
        }
    }

    /// W3/RUL-32 — a leaked case-probe file at the home root is pruned as the
    /// orphan it is, because the home-root arm chooses its removal by the
    /// entry's **on-disk type**.
    ///
    /// `filesystem_is_case_insensitive` states that a probe file outliving its
    /// probe "is pruned by the very next render"; with an unconditional
    /// `remove_dir` there that was false — `ENOTDIR` on a regular file, so the
    /// litter was reported `Skipped` on every render forever.
    ///
    /// The second half is the control RUL-32 is about: a **non-empty**
    /// directory is still not removed, and still not removed recursively.
    ///
    /// RED: restore the unconditional `remove_dir` in the shared
    /// `GroupDirectory | RootEntry` arm.
    #[test]
    fn a_leaked_probe_file_at_the_home_root_is_pruned_and_a_directory_is_not_recursed() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        let litter = tree.home.root().join(".ocx-case-probe-4242-1");
        std::fs::write(&litter, b"").unwrap();

        prune_within(&tree.home, &root_entry(".ocx-case-probe-4242-1"))
            .expect("a regular file at the home root is removable");
        assert!(!litter.exists(), "W3 — the orphan the probe left is gone");

        let occupied = tree.home.links_group("ci").expect("an admitted group name");
        std::fs::create_dir_all(&occupied).unwrap();
        std::fs::write(occupied.join("foreign"), b"not ours\n").unwrap();
        assert!(
            prune_within(&tree.home, &group_directory("ci")).is_err(),
            "RUL-32 — a non-empty group directory is still refused, never recursed"
        );
        assert!(occupied.join("foreign").exists());
    }

    /// RUL-32 — a foreign **regular file** at `<home>/<name>` or at
    /// `<home>/links/<name>` survives byte-for-byte on an ordinary render, and
    /// is named in the report.
    ///
    /// Both names reach the one shared `GroupDirectory | RootEntry` prune arm,
    /// whose `remove_file` fallthrough dispatched on the on-disk type alone —
    /// so anything that was neither a symlink nor a directory was deleted and
    /// reported `Pruned`, which carries **no** warn line. A user's file
    /// therefore vanished on a plain `ocx pull` with nothing said. The
    /// `links/<name>` half is the one this layout newly reaches: before
    /// `links/` existed there was no second level for a foreign file to sit
    /// on. The rule the `Link` arm already follows now holds for the whole
    /// function — ocx removes what ocx wrote, and anything else survives and
    /// is named.
    ///
    /// RED: restore the unconditional `std::fs::remove_file` fallthrough in
    /// that arm (drop the `is_leaked_case_probe` guard and the
    /// `refuse_not_a_link` else). Both files are deleted and both outcomes
    /// read `Pruned`.
    #[tokio::test]
    async fn a_foreign_file_at_a_pruned_name_survives_and_is_reported() {
        let tree = Tree::new();
        std::fs::create_dir_all(links_root(&tree.home)).unwrap();
        let at_root = tree.home.root().join("notes.txt");
        let under_links = links_root(&tree.home).join("notes.txt");
        std::fs::write(&at_root, b"mine, not ocx's\n").unwrap();
        std::fs::write(&under_links, b"mine either\n").unwrap();

        let report = render_a_default_tool(&tree).await;

        // Both survivals in **one** assertion, so a red names both halves at
        // once: asserting them in the loop below stops at the depth-1 file and
        // says nothing about the `links/` level, which is the half this layout
        // newly reaches.
        assert_eq!(
            (std::fs::read(&at_root).ok(), std::fs::read(&under_links).ok()),
            (Some(b"mine, not ocx's\n".to_vec()), Some(b"mine either\n".to_vec())),
            "both planted files survive byte-for-byte"
        );

        for (planted, artifact) in [
            (&at_root, root_entry("notes.txt")),
            (&under_links, group_directory("notes.txt")),
        ] {
            let outcome = outcome_of(&report, &artifact);
            let RenderOutcome::Skipped { path, reason } = outcome else {
                panic!("{artifact:?} must be reported Skipped, not {outcome:?}");
            };
            assert_eq!(path, planted, "…and the report names the path it left alone");
            assert!(!reason.is_empty(), "…with a reason the user can act on");
        }
    }

    /// #591 — a file manager's metadata file on the link side is left in place
    /// and never reported, so it warns on no render; Finder recreates it anyway.
    ///
    /// Two discriminating halves: a *link* named `Thumbs.db` is still pruned (a
    /// valid tool name, so the name alone must not exempt it), and `bin/` still
    /// prunes its `.DS_Store`, since that directory is on `PATH`.
    ///
    /// RED: drop the `is_os_metadata_file` filter in `read_dir_link_side_names`.
    /// Every planted file comes back `Skipped` with a warn-line reason.
    #[tokio::test]
    async fn os_metadata_files_are_left_in_place_and_not_reported() {
        let tree = Tree::new();
        let default_group = links_root(&tree.home).join(DEFAULT_GROUP);
        std::fs::create_dir_all(&default_group).unwrap();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let planted = [
            tree.home.root().join(".DS_Store"),
            links_root(&tree.home).join(".DS_Store"),
            links_root(&tree.home).join("desktop.ini"),
            default_group.join(".DS_Store"),
            default_group.join("._cmake"),
        ];
        for path in &planted {
            std::fs::write(path, b"os-owned\n").unwrap();
        }
        let in_bin = tree.home.shell_bin(DEFAULT_SHELL).join(".DS_Store");
        std::fs::write(&in_bin, b"os-owned\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(tree.tmp.path(), default_group.join("Thumbs.db")).unwrap();

        let report = render_a_default_tool(&tree).await;

        for path in &planted {
            assert_eq!(
                std::fs::read(path).ok(),
                Some(b"os-owned\n".to_vec()),
                "{} survives byte-for-byte",
                path.display()
            );
        }
        let reported: Vec<&RenderedItem> = report
            .items
            .iter()
            .filter(|item| match &item.artifact {
                RenderedArtifact::Link { entry: name, .. }
                | RenderedArtifact::GroupDirectory(name)
                | RenderedArtifact::RootEntry(name) => name != "cmake" && name != "Thumbs.db",
                RenderedArtifact::Trampoline(_) => false,
            })
            .collect();
        assert!(reported.is_empty(), "no OS metadata file is reported: {reported:?}");

        assert_eq!(outcome_of(&report, &trampoline(".DS_Store")), &RenderOutcome::Pruned);
        assert!(std::fs::symlink_metadata(&in_bin).is_err(), "bin/ keeps pruning it");
        #[cfg(unix)]
        assert_eq!(
            outcome_of(&report, &link(DEFAULT_GROUP, "Thumbs.db")),
            &RenderOutcome::Pruned,
            "a link named like an OS file is still ocx's to prune"
        );
    }

    /// W8/RUL-44 — a symlink on a component **between** the project directory
    /// and the home root is refused, and the render skips (C-050).
    ///
    /// `ensure_home_root` judges the home root and `bin/`, and
    /// `symlink_metadata` does not follow only the last component — so
    /// committing `<project>/.ocx` as a symlink relocated the whole rendered
    /// tree while every existing guard still passed, `prune_within` included:
    /// its containment canonicalises both sides, so a home reached through the
    /// link is contained in itself.
    ///
    /// The second half is the positive control: an ordinary `<project>/.ocx`
    /// still renders, or the guard could refuse everything.
    ///
    /// RED: drop the `refuse_symlinked_project_path` call in
    /// `render_toolchain`'s step 2.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_component_above_the_home_root_skips_the_render() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, tree.project_dir.join(".ocx")).unwrap();
        let before = snapshot_subtree(&outside);

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("C-050 — a refusal is a skip, never an error");

        assert!(
            report.items.is_empty() && !report.stamp_written,
            "RUL-44 — the render reached nothing; got {report:?}"
        );
        assert_eq!(
            snapshot_subtree(&outside),
            before,
            "…and not one byte was written through the relocated component"
        );

        // The positive control: an ordinary `.ocx` renders.
        let honest = Tree::new();
        let scope = honest.scope();
        let report = honest
            .manager()
            .render_toolchain(RenderRequest {
                home: &honest.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("an ordinary project home renders");
        assert_eq!(
            honest.bin_entries(),
            expected_bin_entries(&["cmake"]),
            "the guard must not refuse an ordinary project home"
        );
        assert!(report.stamp_written);
    }

    /// B1/RUL-47 — `heal_links` refuses a symlink on a component **between**
    /// the project directory and the home root, and creates nothing in the
    /// link's target.
    ///
    /// The render's own RUL-44 guard is not enough on its own: C-070 puts heal
    /// on **every composing emit**, so `ocx env` and `ocx exec` reach this per
    /// prompt while `ocx pull` reaches the render occasionally — the more
    /// frequently travelled of the two writers was the unguarded one. A
    /// hostile clone committing `<project>/.ocx` as a symlink relocates
    /// everything RUL-29's create and `repoint_link`'s `create_owner_only`
    /// write, and `ensure_home_root` cannot see it: `symlink_metadata` does
    /// not follow only the *last* component, so the home root under the link
    /// reports as an ordinary directory.
    ///
    /// The second half is the positive control: an ordinary project home still
    /// heals its entry, or the widening would be satisfied by a `heal_links`
    /// that refuses every project.
    ///
    /// RED: drop the `refuse_symlinked_project_path` call in `heal_links`.
    #[cfg(unix)]
    #[tokio::test]
    async fn healing_refuses_a_symlinked_component_above_the_home_root() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, tree.project_dir.join(".ocx")).unwrap();
        let before = snapshot_subtree(&outside);

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);

        assert!(
            matches!(
                heal_links(
                    &tree.file_structure,
                    &tree.home,
                    &tree.scope(),
                    &lock,
                    &groups,
                    &platform()
                )
                .await
                .expect("RUL-36 — a refusal degrades, it never errors"),
                HealOutcome::Refused { .. }
            ),
            "RUL-47 — a relocated home repairs nothing, and reports the refusal rather than a \
             count a clean pass also produces"
        );
        assert_eq!(
            snapshot_subtree(&outside),
            before,
            "…and not one byte was written through the relocated component"
        );

        // The positive control: an ordinary project home still heals.
        let honest = Tree::new();
        std::fs::create_dir_all(honest.home.root()).unwrap();
        assert_eq!(
            heal_links(
                &honest.file_structure,
                &honest.home,
                &honest.scope(),
                &lock,
                &groups,
                &platform()
            )
            .await
            .expect("an ordinary home heals"),
            HealOutcome::Healed(1),
            "the guard must not refuse an ordinary project home"
        );
    }

    /// B1 — a **refusal** is distinguishable from a **clean pass** at
    /// `heal_links`' own signature, not only at whichever call site remembered
    /// to re-take the guard.
    ///
    /// The two sibling tests above pin that a refused tree repairs nothing.
    /// Neither can pin that a caller can *tell*: a clean pass repairs nothing
    /// either, so while the signature folded both into `Ok(0)` the two states
    /// were one value. That is the "degrade-to-`Ok` erases the refusal for
    /// every caller" shape — the guard then protects only the write path, and
    /// a composer that reads through the same symlink into `PATH` and
    /// `${installPath}` sees a return value indistinguishable from "every link
    /// was already correct".
    ///
    /// The inequality is the whole property, and it is asserted between two
    /// answers produced by the same call with the same lock and the same
    /// groups — the only difference is the tree. Anything weaker (asserting
    /// only the refused half) is satisfied by a `heal_links` that reports a
    /// refusal for every tree.
    ///
    /// RED: fold either arm back into a shared value — the two answers become
    /// one and this fails.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_tree_is_distinguishable_from_a_clean_pass() {
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);

        // A clean pass: heal once so the entry is correct, then again — there
        // is nothing left to repair, which is the state that used to share a
        // value with the refusal.
        let honest = Tree::new();
        std::fs::create_dir_all(honest.home.root()).unwrap();
        assert_eq!(
            heal_links(
                &honest.file_structure,
                &honest.home,
                &honest.scope(),
                &lock,
                &groups,
                &platform(),
            )
            .await
            .expect("the first heal repairs the entry"),
            HealOutcome::Healed(1),
            "the fixture's premise: there was something to repair"
        );
        let clean = heal_links(
            &honest.file_structure,
            &honest.home,
            &honest.scope(),
            &lock,
            &groups,
            &platform(),
        )
        .await
        .expect("the second heal has nothing to repair");

        // A refusal: a hostile clone commits `<project>/.ocx` as a symlink out
        // of the project (RUL-47).
        let hostile = Tree::new();
        let outside = hostile.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, hostile.project_dir.join(".ocx")).unwrap();
        let refused = heal_links(
            &hostile.file_structure,
            &hostile.home,
            &hostile.scope(),
            &lock,
            &groups,
            &platform(),
        )
        .await
        .expect("RUL-36 — a refusal degrades, it never errors");

        assert_ne!(
            clean, refused,
            "a refused tree must not answer what a clean pass answers — a caller that cannot \
             tell them apart reads through the refused tree as if its links were trustworthy"
        );
        // Which is which, so the inequality cannot be satisfied by swapping them.
        assert_eq!(
            clean,
            HealOutcome::Healed(0),
            "the clean pass had nothing left to repair"
        );
        assert!(
            matches!(refused, HealOutcome::Refused { .. }),
            "…and the refusal names itself, carrying the reason with it: {refused:?}"
        );
    }
    // ── 12. The closed depth-1 set, its guard, and `active` (C-074…C-083) ───

    /// The `links` component this module derives is the grammar's own — the
    /// drift guard for [`links_root`]'s `parent()` walk.
    ///
    /// The literal lives **here**, in a test, and nowhere in the production
    /// body: a second production spelling is what would let scan and guard
    /// disagree about which directory `links` is.
    ///
    /// RED: derive `links_root` from `home.root()` directly, or from any other
    /// component.
    #[test]
    fn links_root_is_the_grammars_own_parent() {
        let home = ToolchainHome::new(PathBuf::from("/ocx/toolchain"));
        assert_eq!(links_root(&home), PathBuf::from("/ocx/toolchain/links"));
        assert_eq!(
            home.links_group("ci").expect("an admitted group"),
            links_root(&home).join("ci"),
            "…and every group directory is a child of it, by construction"
        );
    }

    /// Every `RenderedArtifact` variant resolves to **one** path, whichever of
    /// the two independent derivations is asked.
    ///
    /// [`artifact_path`] computes what a `Skipped` item *reports*;
    /// [`prune_within`] computes, independently, what a prune *removes*. They
    /// are separate `match`es over the same enum and nothing compared them — so
    /// move one parent and not the other and every warning names a path the
    /// render never touched, and the prune deletes a path nothing reported.
    ///
    /// **Behavioural, not a second restatement**: the object is created at
    /// `artifact_path`'s answer and `prune_within` is then asked to remove it.
    /// A test that spelled the prune's parent a third time in test code would
    /// compare two things it wrote itself and observe neither function.
    ///
    /// RED: give any one variant a different parent in either function — the
    /// object survives and the assertion fires, or `prune_within` errors on a
    /// path that is not there.
    #[test]
    fn artifact_path_and_prune_within_agree_on_every_variant() {
        for artifact in [
            trampoline("cmake"),
            link("ci", "ninja"),
            group_directory("ci"),
            root_entry("legacy"),
        ] {
            let tree = Tree::new();
            std::fs::create_dir_all(tree.home.root()).unwrap();
            let reported = artifact_path(&tree.home, &artifact);
            std::fs::create_dir_all(reported.parent().expect("every artifact has a parent")).unwrap();
            match artifact {
                RenderedArtifact::Link { .. } => {
                    std::fs::create_dir_all(tree.tmp.path().join("package")).unwrap();
                    ocx_util::fs::symlink::create(tree.tmp.path().join("package"), &reported).unwrap();
                }
                RenderedArtifact::Trampoline(_) => std::fs::write(&reported, b"#!/bin/sh\n").unwrap(),
                _ => std::fs::create_dir_all(&reported).unwrap(),
            }
            assert!(reported.symlink_metadata().is_ok(), "precondition: {reported:?} exists");

            prune_within(&tree.home, &artifact)
                .unwrap_or_else(|error| panic!("{artifact:?} must be prunable at {reported:?}: {error}"));
            assert!(
                reported.symlink_metadata().is_err(),
                "the prune removed something other than the path the report names, for {artifact:?}"
            );
        }
    }

    /// C-074 — a depth-1 directory named after a **locked** group is pruned.
    ///
    /// Groups are no longer depth-1 entries, so a `ci/` at the home root is a
    /// leftover of the pre-`links/` layout and not a group, however loudly the
    /// lock mentions the name.
    ///
    /// RED: keep the lock-derived `locked_groups` skip in the depth-1 pass —
    /// the directory survives and no item names it.
    #[tokio::test]
    async fn a_depth_1_directory_named_after_a_locked_group_is_pruned() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.root().join("ci")).unwrap();

        let scope = tree.scope();
        let lock = lock_of(vec![locked_tool("ninja", "ci", "ns/ninja", &[(PLATFORM_KEY, 'd')])]);
        let groups = groups_of(&[DEFAULT_GROUP, "ci"]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            outcome_of(&report, &root_entry("ci")),
            &RenderOutcome::Pruned,
            "the lock declares group `ci`, and that keeps `links/ci` — never `<root>/ci`"
        );
        assert!(
            !tree.home.root().join("ci").exists(),
            "…and it is gone from disk: {:?}",
            read_dir_names(tree.home.root())
        );
    }

    /// C-074's trap, stated as a test — the depth-1 keep-set is the tree's own
    /// **names**, never a name derived from an accessor.
    ///
    /// `Path::file_name` yields the string `"bin"` for `bin()` and for
    /// `shell_bin()` alike, so the keep-set the shipped code had
    /// (`reserved_name(&home.bin())`) survives a mechanical
    /// `bin() → shell_bin()` rename **byte for byte** — it compiles, it runs,
    /// and a legacy `bin/` stays at the home root forever with C-074 and C-076
    /// unimplemented.
    ///
    /// RED, and it is the mutation that matters: restore the derived keep-set
    /// in either spelling —
    /// `let reserved = [reserved_name(&home.shell_bin(DEFAULT_SHELL)), reserved_name(&home.gitignore())];`
    /// with the `reserved.iter().any(…)` skip. Both spellings red this row
    /// identically, which is the point.
    #[tokio::test]
    async fn a_legacy_bin_directory_at_the_home_root_is_not_kept_by_the_depth_1_scan() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.root().join("bin")).unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            outcome_of(&report, &root_entry("bin")),
            &RenderOutcome::Pruned,
            "an **empty** legacy `bin/` is an ordinary depth-1 orphan: `remove_dir` takes it"
        );
        assert!(
            !tree.home.root().join("bin").exists(),
            "…and it is gone: {:?}",
            read_dir_names(tree.home.root())
        );
        assert_eq!(
            read_dir_names(tree.home.root()),
            vec!["active".to_string(), "shells".to_string()],
            "…leaving exactly the tree's own depth-1 names this render produced — `.gitignore` is \
             step 3's and `links/` appears once a group renders"
        );
    }

    /// C-076 — a **populated** legacy tree is reported with its remedy and
    /// never deleted, on every render.
    ///
    /// Two runs, because "reported once" was dropped as unimplementable: the
    /// report is derived from the tree as it stands, and the tree still stands.
    ///
    /// RED, and it is the one that matters (RUL-32): change `prune_within`'s
    /// directory branch from `std::fs::remove_dir` to `remove_dir_all` — the
    /// payload assertions red on the first run. RED for the *reported* half:
    /// put the legacy names back in the keep-set — the `Skipped` assertions red
    /// while the payloads still survive, so the two halves are independent and
    /// both are needed.
    #[tokio::test]
    async fn a_legacy_tree_at_the_home_root_is_reported_and_never_deleted() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.root().join("oldgroup").join("oldentry")).unwrap();
        std::fs::write(
            tree.home.root().join("oldgroup").join("oldentry").join("payload"),
            b"planted\n",
        )
        .unwrap();
        std::fs::create_dir_all(tree.home.root().join("bin")).unwrap();
        std::fs::write(tree.home.root().join("bin").join("oldtool"), b"#!/bin/sh\nlegacy\n").unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        for run in 1..=2 {
            let report = render_with(
                &tree.file_structure,
                RenderRequest {
                    home: &tree.home,
                    scope: &scope,
                    lock: &lock,
                    surface: &[],
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                },
                false,
                None,
            )
            .await
            .expect("a legacy tree is not an error");

            for legacy in ["oldgroup", "bin"] {
                let outcome = outcome_of(&report, &root_entry(legacy));
                assert!(
                    is_skipped(outcome),
                    "run {run}: a populated `{legacy}/` is reported, never removed: {outcome:?}"
                );
                let RenderOutcome::Skipped { path, reason } = outcome else {
                    unreachable!("just asserted");
                };
                assert_eq!(path, &tree.home.root().join(legacy), "run {run}: …naming its own path");
                assert!(!reason.is_empty(), "run {run}: …with a non-empty reason (RUL-41)");
            }
            assert_eq!(
                std::fs::read(tree.home.root().join("oldgroup").join("oldentry").join("payload")).unwrap(),
                b"planted\n",
                "run {run}: RUL-32 — the payload is byte-identical, never recursed into"
            );
            assert_eq!(
                std::fs::read(tree.home.root().join("bin").join("oldtool")).unwrap(),
                b"#!/bin/sh\nlegacy\n",
                "run {run}: …and so is the legacy trampoline"
            );
        }
    }

    /// C-074's second trap — **every** directory the tree owns is refused when
    /// it is a symlink, not only the home root and the trampoline directory.
    ///
    /// This layout inserts `links/` and `shells/` between the home root and
    /// every entry link, and no shipped guard judged them:
    /// `publish_link_within` checks an entry's *immediate* parent —
    /// `links/<group>`, never `links` — and `create_owner_only` is
    /// `DirBuilder::recursive(true)`, i.e. `create_dir_all`, which follows an
    /// existing symlink. A committed `.ocx/toolchain/links -> /outside` would
    /// otherwise put every group directory and every entry link outside the
    /// home on a plain `ocx pull`.
    ///
    /// RED: shrink `tree_own_directories` back to `[root, shell_bin]` — the
    /// `links` and `shells` rows write into `outside/` and both assertions
    /// fire.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_tree_own_directory_is_refused_when_it_is_a_symlink() {
        for relative in ["links", "shells", "shells/default", "shells/default/bin"] {
            let tree = Tree::new();
            std::fs::create_dir_all(tree.home.root()).unwrap();
            let outside = tree.tmp.path().join(format!("outside-{}", relative.replace('/', "-")));
            std::fs::create_dir_all(&outside).unwrap();

            let planted = tree.home.root().join(relative);
            std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
            ocx_util::fs::symlink::create(&outside, &planted).unwrap();

            let lock = lock_of(vec![locked_tool(
                "cmake",
                DEFAULT_GROUP,
                "ns/cmake",
                &[(PLATFORM_KEY, 'a')],
            )]);
            let groups = groups_of(&[DEFAULT_GROUP]);
            let platform = platform();
            let report = tree
                .manager()
                .render_toolchain(RenderRequest {
                    home: &tree.home,
                    scope: &tree.scope(),
                    lock: &lock,
                    surface: &[node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])],
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                })
                .await
                .expect("RUL-33 — a refused tree is a skip, never an error");

            assert!(
                report.items.is_empty() && !report.stamp_written,
                "`{relative}` as a symlink refuses the whole render before anything is written: {report:?}"
            );
            assert_eq!(
                read_dir_names(&outside),
                Vec::<String>::new(),
                "…and nothing was written through it into {outside:?}"
            );
        }
    }

    /// C-081, row 1 — an **absent** `active` is created, silently.
    ///
    /// RED: delete the create arm — `active` stays absent and `bin()` resolves
    /// to nothing for every PATH route.
    #[tokio::test]
    async fn an_absent_active_link_is_created_by_the_render() {
        let tree = Tree::new();
        let report = render_a_default_tool(&tree).await;

        assert!(
            ocx_util::fs::symlink::is_link(&tree.home.active()),
            "…and it is a link, not a directory: {:?}",
            read_dir_names(tree.home.root())
        );
        assert!(
            tree.home.active_is_valid(DEFAULT_SHELL),
            "C-080 — at exactly its derived target"
        );
        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["cmake"]),
            "…and the trampoline landed in the physical directory: {report:?}"
        );
    }

    /// C-081, corrupt-states row 21, the **heal** half — an `active` pointing
    /// outside the home is repointed before anything is written.
    ///
    /// RED: delete the `replace_atomic` arm from [`heal_active`] — the link
    /// stays pointed at `outside/` and both assertions fire.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_active_pointing_outside_the_home_is_repointed_by_the_render() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        ocx_util::fs::symlink::create(&outside, tree.home.active()).unwrap();

        let report = render_a_default_tool(&tree).await;

        assert!(
            tree.home.active_is_valid(DEFAULT_SHELL),
            "the hostile link is repointed at its derived target: {report:?}"
        );
        assert_eq!(
            read_dir_names(&outside),
            Vec::<String>::new(),
            "…and nothing was written through it on the way"
        );
    }

    /// C-080, corrupt-states row 21, the **anchor** half — the render writes,
    /// prunes and stamps through the *physical* directory even while `active`
    /// points outside the home.
    ///
    /// # Why this drives `render_with` and not the public entry
    ///
    /// Step 6 heals `active` before the body runs, so a hostile link planted
    /// before `ocx pull` is gone by the time `reconcile_bin` writes — and a row
    /// that plants it there therefore measures the heal and **cannot** observe
    /// the anchors at all. That was this row's first form, and mutating
    /// `reconcile_bin`, `fingerprint_bin` and `prune_within` back to `bin()`
    /// each left it green.
    ///
    /// The state this row is about is the one the anchors exist for and the
    /// heal cannot rule out: `active` is a symlink, and **any process that can
    /// write the home can swing it at any instant** — the render lock is
    /// ocx's own convention and stops no one (R12). So the link is swung after
    /// the fixture's steps 2–6 and before the body, which is exactly where a
    /// concurrent repoint lands.
    ///
    /// RED: revert `reconcile_bin`'s binding, `write_render_stamp`'s
    /// `fingerprint_bin` argument, `artifact_path`'s trampoline arm or
    /// `prune_within`'s trampoline parent to `home.bin()`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_render_writes_prunes_and_stamps_through_the_physical_directory() {
        let tree = Tree::new();
        tree.create_home_root();
        // A committed orphan, so the prune pass has something to act on too.
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("orphan"), b"#!/bin/sh\n").unwrap();

        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(outside.join("bin")).unwrap();
        std::fs::write(outside.join("bin").join("orphan"), b"not ours\n").unwrap();
        ocx_util::fs::symlink::replace_atomic(&outside, tree.home.active()).unwrap();
        assert!(
            !tree.home.active_is_valid(DEFAULT_SHELL),
            "precondition: `active` is swung outside the home, as a concurrent writer leaves it"
        );

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'a')],
        )]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["cmake"]),
            "the write and the prune both acted on the physical directory: {report:?}"
        );
        assert_eq!(
            read_dir_names(&outside.join("bin")),
            vec!["orphan".to_string()],
            "…and the attacker's directory was neither written into nor pruned"
        );
        assert_eq!(
            std::fs::read(outside.join("bin").join("orphan")).unwrap(),
            b"not ours\n",
            "…byte-identical, not merely present"
        );
        let stamp = tree.stamp().expect("the stamp exists");
        assert_eq!(
            stamp.bin_fingerprint.keys().cloned().collect::<Vec<_>>(),
            expected_bin_entries(&["cmake"]),
            "…and the stamp certifies what landed, never what `active` pointed at: {stamp:?}"
        );
    }

    /// C-080, corrupt-states row 22 — validity is **raw** equality with the
    /// derived target, never a containment or canonicalising test.
    ///
    /// `shells/../shells/default` is contained *and* resolves to exactly the
    /// right directory, and it is still not the value this tree writes. A
    /// containment test admits it; a canonicalising compare admits it; only raw
    /// equality repoints it.
    ///
    /// RED: swap `active_is_valid`'s `read_link` for `dunce::canonicalize`, or
    /// replace the equality with a `starts_with` containment check — the link
    /// is left as it is and the target assertion fires.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_active_that_resolves_correctly_by_a_different_spelling_is_still_repointed() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.shell_directory()).unwrap();
        ocx_util::fs::symlink::create(
            Path::new("shells").join("..").join("shells").join(DEFAULT_SHELL),
            tree.home.active(),
        )
        .unwrap();
        assert_eq!(
            std::fs::canonicalize(tree.home.active()).unwrap(),
            std::fs::canonicalize(tree.shell_directory()).unwrap(),
            "precondition: the planted spelling resolves to the right directory, so only a raw \
             comparison can reject it"
        );

        render_a_default_tool(&tree).await;

        assert_eq!(
            std::fs::read_link(tree.home.active()).unwrap(),
            tree.home.expected_active_target(DEFAULT_SHELL),
            "the raw target is the one this tree writes, not merely one that resolves there"
        );
    }

    /// C-081, row 4 (inverted from the corrupt-states matrix, which asserted
    /// the opposite) — a **real directory** at `active` is the `cp -rL` /
    /// Docker `COPY` / zip outcome, and it is healed: removed and replaced by
    /// the link. Its contents do **not** survive, because they are a copy of
    /// the tree this render is about to rewrite.
    ///
    /// RED: drop the `remove_dir_all` arm and refuse instead — `active` stays a
    /// directory and the link assertion fires. RED for the carve-out's bound:
    /// replace `remove_dir_all` with `remove_dir` — a populated copy fails
    /// `ENOTEMPTY` and the same assertion fires.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_real_directory_at_active_is_healed_rather_than_refused() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.active().join("bin")).unwrap();
        std::fs::write(tree.home.active().join("bin").join("stale"), b"copied\n").unwrap();

        render_a_default_tool(&tree).await;

        assert!(
            ocx_util::fs::symlink::is_link(&tree.home.active()),
            "the dereferenced copy is replaced by the link it was a copy of"
        );
        assert!(tree.home.active_is_valid(DEFAULT_SHELL), "…at its derived target");
        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["cmake"]),
            "…and the physical directory holds exactly this render's output"
        );
    }

    /// C-081, row 5 — `active` occupied by a **regular file**, or by any other
    /// non-directory, is cleared and re-created.
    ///
    /// **Both plants take the same arm, and neither discriminates the
    /// primitive.** On POSIX `rename(2)` replaces a regular file and a FIFO
    /// alike, so a heal that reached `symlink::replace_atomic` unconditionally
    /// leaves a finished tree byte-identical to this one — the occupant's
    /// bytes are gone either way, and no assertion over the result can tell
    /// the two apart. The FIFO is planted because it is the second corrupt
    /// state a user can reach, not because it observes the arm. What this row
    /// pins is that the occupant is cleared **before** the link is published,
    /// which is the whole of the non-directory arm's job.
    ///
    /// RED: delete the `remove_file` from `heal_active`'s non-directory branch
    /// and fall straight through to `symlink::create` — `symlink(2)` fails
    /// `EEXIST` against the occupant, the heal only warns, and `active` stays
    /// a file (or a FIFO) on both legs.
    #[cfg(unix)]
    #[tokio::test]
    async fn active_occupied_by_a_non_directory_is_removed_and_re_created() {
        for plant in ["file", "fifo"] {
            let tree = Tree::new();
            std::fs::create_dir_all(tree.home.root()).unwrap();
            if plant == "file" {
                std::fs::write(tree.home.active(), b"not a link\n").unwrap();
            } else {
                mkfifo(&tree.home.active());
            }

            render_a_default_tool(&tree).await;

            assert!(
                ocx_util::fs::symlink::is_link(&tree.home.active()),
                "a {plant} at `active` is removed and replaced by the link"
            );
            assert!(
                tree.home.active_is_valid(DEFAULT_SHELL),
                "…at its derived target ({plant})"
            );
        }
    }

    /// C-081, corrupt-states row 28 — a **self-referential** `active` is
    /// repointed, and the call returns.
    ///
    /// RED: make the heal canonicalise the existing link before deciding —
    /// `ELOOP` surfaces and the render never finishes the step. RED for the
    /// two-step cycle: an equality check against the link's own path catches
    /// `active -> active` and sails straight past `active -> b`, `b -> active`,
    /// which is why both are planted.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_cyclic_active_is_repointed_without_hanging() {
        for cycle in ["self", "two-step"] {
            let tree = Tree::new();
            std::fs::create_dir_all(tree.home.root()).unwrap();
            if cycle == "self" {
                ocx_util::fs::symlink::create("active", tree.home.active()).unwrap();
            } else {
                ocx_util::fs::symlink::create("b", tree.home.active()).unwrap();
                ocx_util::fs::symlink::create("active", tree.home.root().join("b")).unwrap();
            }

            render_a_default_tool(&tree).await;

            assert!(
                tree.home.active_is_valid(DEFAULT_SHELL),
                "the {cycle} cycle is repointed at the derived target"
            );
        }
    }

    /// C-083, corrupt-states row 31 — the shell directory is created **before**
    /// `active`, so an interrupted render never leaves a link to a directory
    /// that does not exist.
    ///
    /// A lookup through an absent `active` is a **miss**; a lookup through a
    /// link into nothing is an error every reader then has to be careful to
    /// read as a mismatch. The order is the whole guarantee.
    ///
    /// **Driven through the fault seam, because the finished tree cannot show
    /// it.** With both writes landed the tree is byte-identical either way —
    /// the swapped order publishes a dangling link and then creates its target
    /// — so an assertion over the finished state passes with the calls
    /// reversed. Measured: it did.
    ///
    /// RED: swap the two calls in step 6 — `active` is present, and dangling,
    /// at the abort.
    #[tokio::test]
    async fn an_interrupted_render_never_leaves_active_pointing_at_an_absent_shell() {
        let tree = Tree::new();
        let _lock = ocx_util::env::overrides::lock();
        // SAFETY: nextest gives every test its own process, and `EnvLock`
        // serialises this write against every test that goes through
        // `ocx_util::env::overrides`. `read_fault_hook` reads `std::env::var_os`, which
        // the override table does not reach, so the variable has to be real.
        unsafe { std::env::set_var("__OCX_TESTING_RENDER_FAULT", FAULT_AFTER_SHELL_TREE) };

        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'a')],
        )]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let outcome = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &tree.scope(),
                lock: &lock,
                surface: &[node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await;
        // SAFETY: as above.
        unsafe { std::env::remove_var("__OCX_TESTING_RENDER_FAULT") };

        assert!(outcome.is_err(), "precondition: the seam aborted the render");
        assert!(
            tree.shell_directory().is_dir(),
            "the shell directory had already landed at the abort: {:?}",
            read_dir_names(tree.home.root())
        );
        assert!(
            tree.home.active().symlink_metadata().is_err(),
            "…and `active` had not — an absent link is a miss, a dangling one is not"
        );
    }

    /// One render of one default-group tool, through the **public** entry, so
    /// step 6 runs. Returns the report.
    async fn render_a_default_tool(tree: &Tree) -> RenderReport {
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'a')],
        )]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        tree.manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &tree.scope(),
                lock: &lock,
                surface: &[node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the render succeeds")
    }

    fn root_entry(name: &str) -> RenderedArtifact {
        RenderedArtifact::RootEntry(name.to_string())
    }
}
