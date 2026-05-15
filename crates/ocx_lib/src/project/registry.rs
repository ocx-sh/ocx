// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-user GC ledger of projects whose `ocx.lock` pins packages on this
//! machine, expressed as a flat symlink store.
//!
//! The ledger answers "which projects pin tools here" so that `ocx clean` can
//! retain packages held by lockfiles in projects other than the active one.
//!
//! On disk this is a flat directory `$OCX_HOME/projects/`, one symlink per
//! registered project:
//!
//! ```text
//! $OCX_HOME/projects/
//!   1f3a9c0b5e7d2a84  ->  /home/alice/dev/proj-a   (symlink, target = project dir)
//!   9b2c4e6a8d0f1357  ->  /home/alice/dev/proj-b
//! ```
//!
//! - **Name** = first 16 hex of `SHA-256(canonical_abs_project_dir)` —
//!   identical scheme to [`crate::reference_manager::ReferenceManager::name_for_path`].
//!   Single source of truth for the hash; this module does not reinvent it.
//! - **Target** = the canonicalised absolute **project directory** (the
//!   directory containing `ocx.lock`). Never the lock file, never the config
//!   file. Browsable: `ls -l $OCX_HOME/projects/` resolves into projects.
//!
//! This collapses the project ledger into the same liveness model the rest of
//! the store already uses (`refs/symlinks/`): a symlink whose
//! existence-and-resolvability *is* the GC-root signal. There is no
//! `projects.json`, no `.projects.lock` sentinel, no schema version, no
//! whole-file rewrite. See [`adr_project_gc_symlink_ledger.md`] for the full
//! rationale (supersedes `adr_clean_project_backlinks.md`).
//!
//! ## Sanctioned `symlink::create/update` exception
//!
//! The ledger symlink targets an absolute path **outside** `$OCX_HOME` (the
//! project dir, Nix indirect-root shape). It therefore uses the low-level
//! [`crate::symlink`] primitives directly and **must not** route through
//! `symlink::validate_target` (whose containment policy is for `refs/`-internal
//! links). `projects/` links are categorically not install back-refs, so the
//! "never raw `symlink::create/update`, always `ReferenceManager`" rule does
//! not apply here — this is a named, documented carve-out (ARCH-4b).
//!
//! ## Accepted collision blast-radius (ARCH-1a)
//!
//! Reusing the 16-hex (64-bit) `name_for_path` scheme is sound at realistic
//! worktree counts (collision probability ≈10⁻¹³). The blast radius differs
//! from the `refs/symlinks/` precedent though: a `refs/symlinks/` collision is
//! a recoverable local mislink, whereas a `projects/` collision drops one of
//! two live projects from the GC root set → silent collection of its pinned
//! packages. Accepted at realistic scale; stated here so the reuse is a
//! conscious decision, not precedent inertia.

pub mod error;

use std::path::{Path, PathBuf};

pub use error::{Error, ProjectRegistryError, ProjectRegistryErrorKind};

/// Converts a [`crate::Error`] from the low-level `symlink` primitives into a
/// registry [`ProjectRegistryErrorKind::Io`], preserving the structural inner
/// [`std::io::Error`] (never `.to_string()`-erasing the source) when the
/// variant carries one.
fn into_io_kind(error: crate::Error) -> ProjectRegistryErrorKind {
    match error {
        crate::Error::InternalFile(_, io) => ProjectRegistryErrorKind::Io(io),
        // Unreachable today: `into_io_kind` is only fed errors from
        // `symlink::replace_atomic`, which emits exclusively
        // `crate::Error::InternalFile`. This arm flattens any other variant
        // into an opaque `io::Error::other`, **erasing the original
        // classification** (a `ConfigError`/`AuthError`/etc. would silently
        // become `IoError`). If `replace_atomic` ever grows a non-
        // `InternalFile` failure path, fix the source classification here —
        // do not let it reach this lossy flatten. The `debug_assert!` traps
        // the regression in debug/test builds without changing release
        // behaviour.
        other => {
            debug_assert!(
                false,
                "into_io_kind: non-InternalFile crate::Error reached the lossy flatten arm: {other:?}"
            );
            ProjectRegistryErrorKind::Io(std::io::Error::other(other))
        }
    }
}

/// Three-state outcome of a single ledger-entry liveness probe.
///
/// The distinction between [`ProbeResult::Dead`] and [`ProbeResult::Unknown`]
/// is the SEC-1 silent-data-loss guard: collapsing a transient probe `Err`
/// (NFS/automount/permission-flip) into "dead" would prune a live project's
/// ledger link and GC its pinned packages. `Dead` is acted on (pruned);
/// `Unknown` is retained and warned about (see [`ProjectRegistry::live_projects`]).
enum ProbeResult {
    /// The link resolves and `<target>/ocx.lock` exists — a live GC root.
    Live(PathBuf),
    /// Definitively absent: not a link, OR an `Ok` probe proved the target /
    /// `ocx.lock` is gone. Safe to prune.
    Dead,
    /// A transient I/O `Err` from `is_link`/`canonicalize`/`try_exists` — the
    /// filesystem was momentarily unreachable. Liveness is indeterminate;
    /// the link MUST be retained (never pruned, never collected as a root).
    Unknown,
}

/// Liveness probe for a single ledger entry.
///
/// Returns [`ProbeResult::Live`] when `entry_path` is a symlink whose target
/// resolves to a directory containing an `ocx.lock` (a live GC root);
/// [`ProbeResult::Dead`] when the entry is definitively not a live root (not
/// a link, or an `Ok` probe proved the target / `ocx.lock` is absent);
/// [`ProbeResult::Unknown`] when ANY probe step returned an `Err` (transient
/// unreachable filesystem — must not be treated as dead). Used twice per
/// non-`Live` entry to close the CODEX-BLOCK-1 TOCTOU window: a `Dead` is only
/// acted on (pruned) if a re-probe immediately before removal also yields
/// `Dead`.
fn probe_live_target(entry_path: &Path) -> ProbeResult {
    if !crate::symlink::is_link(entry_path) {
        return ProbeResult::Dead;
    }
    // Resolve the link target to an absolute, canonical directory. A broken
    // link (target removed) fails canonicalize with NotFound → Dead; any
    // other Err (EACCES, ESTALE, ETIMEDOUT, ...) is transient → Unknown.
    let target = match dunce::canonicalize(entry_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ProbeResult::Dead,
        Err(_) => return ProbeResult::Unknown,
    };
    match target.join("ocx.lock").try_exists() {
        Ok(true) => ProbeResult::Live(target),
        Ok(false) => ProbeResult::Dead,
        Err(_) => ProbeResult::Unknown,
    }
}

/// Resolves the GC-root directory for a [`ProbeResult::Unknown`] ledger
/// entry, or escalates to a fatal registry error.
///
/// `probe_live_target` returns no canonical target on `Unknown` (the transient
/// I/O error occurred *inside* `canonicalize`/`try_exists`), so the project
/// directory is recovered best-effort from the ledger link's stored target via
/// [`std::fs::read_link`]. [`ProjectRegistry::register`] always stores an
/// absolute canonical project directory as the link target, so `read_link`
/// alone (no `canonicalize`, no `ocx.lock` existence check) yields the root.
///
/// # Errors
///
/// Returns [`Error::Registry`] wrapping [`ProjectRegistryErrorKind::Io`] when
/// `read_link` itself fails — the entry's root cannot be recovered at all, so
/// per plan A1/A2 it fails closed (the caller aborts `ocx clean`) rather than
/// silently dropping a possibly-live GC root.
fn unknown_root_or_escalate(entry_path: &Path) -> Result<PathBuf, Error> {
    std::fs::read_link(entry_path).map_err(|e| {
        Error::Registry(ProjectRegistryError::new(
            entry_path.to_path_buf(),
            ProjectRegistryErrorKind::Io(e),
        ))
    })
}

/// Best-effort, **infallible** GC-ledger registration of the project that
/// owns `config_path`, performed after an `ocx.lock` write/commit succeeds.
///
/// This is the single shared derivation for the two callers that must record
/// a freshly-written lock in the per-user ledger so `ocx clean` retains the
/// packages it pins ([`super::lock::ProjectLock::save`] and
/// [`super::mutation::MutationGuard::commit`]). Both previously inlined the
/// same ~22-line canonicalize→parent→`register`→WARN block; they differ only
/// in their early-return *shape*, which is why this helper is **infallible**:
/// every failure mode is the silent-data-loss class and is handled internally
/// (WARN, then return) so the caller unconditionally continues its own
/// success path without an early-return divergence.
///
/// Steps, all non-fatal:
/// 1. Canonicalize the *config file* first (it exists on disk — the lock was
///    just written next to it), then take its parent. Canonicalizing the file
///    before taking the parent makes a bare relative `--project` basename
///    (e.g. `workspace.toml`, whose [`Path::parent`] is `Some("")`, NOT
///    `None`) resolve against the CWD instead of canonicalizing the empty
///    path. It also collapses aliased lookups (relative segments, symlinks)
///    to one ledger entry. The ledger keys on the project *directory*, never
///    the lock+config pair — safe ONLY because of the invariant
///    `lock_path_for(config) == <config.parent()>/ocx.lock` for every
///    `--project` basename (pinned by
///    `lock_path_for_always_produces_ocx_lock_in_config_dir`, ARCH-4d). If
///    that test is ever weakened the custom-`--project` multi-project GC
///    contract breaks.
/// 2. Register the canonical project directory.
///
/// Any failure (canonicalize of *this project's own* path, a parentless
/// canonical path, or `register`) is the silent-data-loss class: logged at
/// `log::warn!` (NOT debug — `feedback_no_warn_on_common_benign` explicitly
/// does not cover self-register failure, C1.1) and swallowed so the
/// `ocx.toml`/`ocx.lock` mutation is never aborted. The next `ocx lock`
/// re-registers.
pub async fn register_project_dir_best_effort(config_path: &Path, ocx_home: &Path) {
    let canonical_project_dir = match tokio::fs::canonicalize(config_path).await {
        Ok(canonical_config) => match canonical_config.parent() {
            Some(dir) => dir.to_path_buf(),
            None => {
                crate::log::warn!(
                    "Project registry: config path '{}' has no parent directory \
                     (non-fatal, registration skipped)",
                    canonical_config.display()
                );
                return;
            }
        },
        Err(e) => {
            crate::log::warn!(
                "Project registry: canonicalize of config path '{}' failed \
                 (non-fatal, registration skipped): {e}",
                config_path.display()
            );
            return;
        }
    };
    let registry = ProjectRegistry::new(ocx_home);
    if let Err(e) = registry.register(&canonical_project_dir).await {
        crate::log::warn!(
            "Project registry: registration of '{}' failed (non-fatal): {e}",
            canonical_project_dir.display()
        );
    }
}

/// Per-user GC ledger backed by the flat symlink store `$OCX_HOME/projects/`.
///
/// Populated whenever an `ocx.lock` is saved (`ProjectLock::save` /
/// `MutationGuard::commit` tails) and consulted by `ocx clean` to avoid
/// collecting packages pinned by any registered project.
///
/// Construct via [`ProjectRegistry::new`]; all I/O is async.
pub struct ProjectRegistry {
    /// Absolute path to `$OCX_HOME/projects/` (the flat symlink store).
    projects_dir: PathBuf,
}

impl ProjectRegistry {
    /// Constructs a [`ProjectRegistry`] rooted at `ocx_home`.
    ///
    /// Pure path arithmetic; performs no I/O. The store directory is
    /// `ocx_home/projects/`. The directory is **not** created here — it is
    /// created lazily by [`Self::register`], and its absence is a valid
    /// "no projects registered" state for [`Self::live_projects`].
    pub fn new(ocx_home: &Path) -> Self {
        Self {
            projects_dir: ocx_home.join("projects"),
        }
    }

    /// Records the GC-root symlink for `project_dir` in the ledger.
    ///
    /// Idempotent. `project_dir` is canonicalised; the link name is
    /// `ReferenceManager::name_for_path(canonical_project_dir)` (16-hex /
    /// 64-bit SHA-256 — reused, not reinvented) and the link target is the
    /// canonical project directory.
    ///
    /// # No self-link invariant (ARCH-1b — silent-data-loss class)
    ///
    /// **No-op** (returns `Ok(())`) when `project_dir` is the *same directory*
    /// as `$OCX_HOME`. The identity test MUST be device+inode identity
    /// ([`crate::utility::fs::same_dir`]: Unix `dev`/`ino`; Windows
    /// canonicalized-handle equivalence), **not** canonical-path byte
    /// equality. `tokio::fs::canonicalize` does not case-fold on
    /// case-insensitive/normalizing filesystems (macOS APFS default, Windows —
    /// both first-class platforms), so byte equality can return *false* when
    /// the paths denote the same directory, letting the forbidden
    /// `$OCX_HOME/projects/<hash> → $OCX_HOME` self-link slip through. The
    /// global toolchain (`$OCX_HOME/ocx.toml`) is already a GC root via its
    /// `current` install symlinks; the ledger never points at its own home.
    ///
    /// # Crash-safe atomic replace (CODEX-BLOCK-2)
    ///
    /// `symlink::update` is remove-then-create — NOT atomic; a crash or a
    /// concurrent [`Self::live_projects`] between the remove and the create
    /// transiently drops a live root. `register` instead stages the link at a
    /// temp name in the **same** `projects/` directory (`.tmp-<pid>-<rand>`)
    /// then `rename(2)`s it onto the final name (atomic same-dir rename on
    /// POSIX; `ReplaceFile`/equivalent on Windows) via
    /// [`crate::symlink::replace_atomic`]. [`Self::live_projects`] skips
    /// `.tmp-*` names. Uses low-level [`crate::symlink`] directly — **not**
    /// `validate_target` (target is an absolute external path by design).
    ///
    /// # Error handling (ARCH-3 — split by failure class)
    ///
    /// Failure to create/update **this** project's own link (store dir
    /// unwritable, ENOSPC, perms; or canonicalize failure of this project's
    /// own paths) is the silent-data-loss class — the user's just-pinned
    /// packages will not be a GC root. The caller logs at **`log::warn!`**
    /// (NOT debug; `feedback_no_warn_on_common_benign` explicitly does not
    /// cover this) and the failure is **non-fatal**: it never propagates and
    /// never blocks the `ocx.toml`/`ocx.lock` mutation. (Departed-*other*-
    /// project pruning is the benign common case and is debug-only — but that
    /// happens in [`Self::live_projects`], not here.)
    ///
    /// # Errors
    ///
    /// Returns [`Error::Registry`] wrapping [`ProjectRegistryErrorKind::Io`]
    /// on store-directory creation or atomic-replace failure. Callers treat
    /// this as non-fatal and log at WARN (see above).
    pub async fn register(&self, project_dir: &Path) -> Result<(), Error> {
        let projects_dir = self.projects_dir.clone();
        let project_dir = project_dir.to_path_buf();

        // All work is synchronous filesystem I/O (canonicalize, stat for the
        // dev/inode identity probe, symlink staging + rename). Run it on a
        // blocking thread so it never stalls the async runtime — same
        // convention as `ProjectLock::save`.
        tokio::task::spawn_blocking(move || -> Result<(), Error> {
            let canonical_project_dir = dunce::canonicalize(&project_dir)
                .map_err(|e| ProjectRegistryError::new(project_dir.clone(), ProjectRegistryErrorKind::Io(e)))?;

            // `$OCX_HOME` is the parent of `projects/`. The no-self-link
            // invariant (ARCH-1b) suppresses a `$OCX_HOME/projects/<hash> ->
            // $OCX_HOME` link. Identity is device+inode, NOT canonical-path
            // byte equality — byte equality is unsound on case-insensitive /
            // normalizing filesystems where the same dir has differing path
            // bytes. A failure to probe identity is treated as "not the same
            // dir, proceed" (the link is then created normally).
            let ocx_home = projects_dir.parent().unwrap_or(&projects_dir);
            if crate::utility::fs::same_dir(&canonical_project_dir, ocx_home).unwrap_or(false) {
                return Ok(());
            }

            let name = crate::reference_manager::ReferenceManager::name_for_path(&canonical_project_dir);
            let link_path = projects_dir.join(name);

            crate::symlink::replace_atomic(&canonical_project_dir, &link_path)
                .map_err(|e| ProjectRegistryError::new(link_path.clone(), into_io_kind(e)))?;
            Ok(())
        })
        .await
        .map_err(|e| {
            Error::Registry(ProjectRegistryError::new(
                PathBuf::new(),
                ProjectRegistryErrorKind::Io(std::io::Error::other(format!("register task panicked: {e}"))),
            ))
        })?
    }

    /// Returns the canonical project directories that are still live GC roots,
    /// pruning departed-project links as a side effect.
    ///
    /// Readdir `projects/` (skipping `.tmp-*` staging names). The per-entry
    /// liveness probe is **three-state** ([`ProbeResult`]): a transient probe
    /// `Err` (momentarily-unreachable filesystem: NFS/automount/permission-
    /// flip) yields [`ProbeResult::Unknown`] — NOT "dead". For each entry:
    /// - [`ProbeResult::Live`] → collect the canonical `<target>`.
    /// - non-`Live` → **re-probe the entry immediately before removing** and
    ///   only `symlink::remove(entry)` if BOTH probes return
    ///   [`ProbeResult::Dead`] (CODEX-BLOCK-1 TOCTOU guard: a concurrent
    ///   [`Self::register`] re-pointing the same hash between the snapshot and
    ///   the remove must not be deleted — if the re-check now resolves `Live`,
    ///   treat it as live, do not remove, collect it). A prune logs at
    ///   `log::debug!` (the common benign departed-*other*-project case —
    ///   never WARN).
    /// - either probe [`ProbeResult::Unknown`] → **retain the link AND return
    ///   the project as a live GC root for this run** (SEC-1 silent-data-loss
    ///   guard, fail-closed per plan A1: a transient EACCES/ESTALE on a *live*
    ///   project must not let `ocx clean` collect its pinned packages this
    ///   run). `probe_live_target` has no canonical target on `Unknown`
    ///   (`canonicalize` failed), so the root is resolved best-effort from the
    ///   ledger link's stored target via `std::fs::read_link` (absolute by
    ///   [`Self::register`] construction). If `read_link` *also* fails, that
    ///   single entry escalates to a fatal registry error (return `Err`,
    ///   classified to `IoError`) rather than silently dropping a live root.
    ///   The entry is `log::warn!`ed once either way (per ADR §Risks ARCH-3
    ///   policy — WARN not debug). The next `ocx clean` re-probes.
    ///
    /// Returns the collected dirs **sorted by link name** (deterministic, per
    /// `quality-rust.md` JoinSet/ordering rule).
    ///
    /// **Single fail-closed contract (plan A2″, Round-3 systemic).** The ONLY
    /// way this function yields a missing/empty ledger is *definitive
    /// absence* — a whole-`projects/` `read_dir` returning
    /// `ErrorKind::NotFound`, which maps to `Ok([])` (no projects registered
    /// is a valid state; the directory is **not** created as a side effect).
    /// EVERY other I/O error anywhere in enumeration — a non-`NotFound`
    /// directory-open failure, a per-entry `ReadDir` iterator `Err`, or any
    /// future enumeration step — propagates [`Error::Registry`] so `ocx clean`
    /// aborts (classified to `IoError`) rather than running destructive GC
    /// with zero roots against a live multi-project store. `--force` bypasses
    /// the registry entirely and is the sanctioned operator override. The
    /// guarantee is enforced at the function contract (a single
    /// `collect::<io::Result<_>>()?` over the directory iterator), not
    /// arm-by-arm: point-fixing each fail-open site relocated this
    /// silent-data-loss bug 3× (A2 `?`, A2′ dir-open, A2″ per-entry iter).
    ///
    /// Concurrency note: entries created mid-readdir by a concurrent
    /// [`Self::register`] may or may not be observed (POSIX
    /// implementation-defined). Acceptable — an already-installed package is
    /// retained by its `current` install symlink, and a missed first-ever
    /// registration is picked up at the next `ocx clean` (SOTA finding 1c).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Registry`] wrapping [`ProjectRegistryErrorKind::Io`]
    /// for any non-`NotFound` enumeration I/O failure on `projects/` —
    /// directory open *and* per-entry iterator `Err` (fail-closed, plan
    /// A2″) — and when an `Unknown`-probed entry's root cannot even be
    /// `read_link`-recovered (plan A1). Routine per-entry prune I/O on
    /// *other* projects' broken links stays debug-only and never surfaces.
    pub async fn live_projects(&self) -> Result<Vec<PathBuf>, Error> {
        let projects_dir = self.projects_dir.clone();

        // Synchronous readdir + per-entry stat/readlink/prune. Off the runtime
        // for the same reason as `register`.
        tokio::task::spawn_blocking(move || -> Result<Vec<PathBuf>, Error> {
            let read_dir = match std::fs::read_dir(&projects_dir) {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // `projects/` absent → no projects registered. Not an
                    // error, and the directory is NOT created as a side effect.
                    return Ok(Vec::new());
                }
                Err(e) => {
                    // Fail closed (plan A2′): a non-`NotFound` enumeration
                    // failure is indistinguishable downstream from "no
                    // projects registered". Returning `Ok(Vec::new())` here
                    // would let `ocx clean` run with zero GC roots against a
                    // live multi-project store and collect every live
                    // project's pinned packages — the exact fail-open
                    // silent-data-loss bug A2 exists to close, merely
                    // relocated to directory enumeration. Propagate so the
                    // caller aborts (classified to `IoError`); `--force`
                    // bypasses the registry entirely as the sanctioned
                    // operator override.
                    return Err(Error::Registry(ProjectRegistryError::new(
                        projects_dir.clone(),
                        ProjectRegistryErrorKind::Io(e),
                    )));
                }
            };

            // Collect (link_name, canonical_target) so the result is
            // deterministic (sorted by link name) per the ordering rule.
            let mut live: Vec<(std::ffi::OsString, PathBuf)> = Vec::new();

            // Plan A2″ (Round-3, systemic fail-closed contract): the ONLY path
            // to `Ok(Vec::new())` / a missing ledger entry is *definitive
            // absence* — the whole-`projects/` `read_dir` `NotFound` carve-out
            // above. EVERY other enumeration I/O error MUST propagate
            // `Err(Error::Registry(.. Io ..))` so `ocx clean` aborts (IoError;
            // `--force` is the sanctioned override) rather than running
            // destructive GC with zero roots against a live multi-project
            // store. Point-fixing each fail-open site relocated this
            // silent-data-loss bug 3×; close it once at the function contract
            // by materialising the directory iterator through
            // `collect::<io::Result<_>>()?` — a per-entry iterator `Err` is an
            // enumeration I/O failure (`readdir(3)` short read / EIO / ESTALE),
            // categorically NOT a per-entry liveness signal, so it short-
            // circuits to the same `Err` as the dir-open arm. `.tmp-*` skip +
            // per-entry `probe_live_target` Dead/Unknown classification stay
            // unchanged (those operate on a *successfully* enumerated entry and
            // are not enumeration I/O).
            let entries: Vec<std::fs::DirEntry> = read_dir.collect::<std::io::Result<Vec<_>>>().map_err(|e| {
                Error::Registry(ProjectRegistryError::new(
                    projects_dir.clone(),
                    ProjectRegistryErrorKind::Io(e),
                ))
            })?;

            for entry in entries {
                let file_name = entry.file_name();
                // Skip in-flight `.tmp-*` staging links from a concurrent
                // `register` — they are not ledger entries.
                if file_name.to_string_lossy().starts_with(".tmp-") {
                    continue;
                }
                let entry_path = entry.path();

                match probe_live_target(&entry_path) {
                    ProbeResult::Live(target) => live.push((file_name, target)),
                    ProbeResult::Unknown => {
                        // Transient probe failure (filesystem momentarily
                        // unreachable). SEC-1 silent-data-loss guard: this
                        // project may be perfectly live — pruning its link OR
                        // dropping it from this run's root set would let
                        // `ocx clean` GC its pinned packages (plan A1
                        // fail-closed). Retain the link AND return the project
                        // as a live root. `probe_live_target` carries no
                        // canonical target on `Unknown`; resolve the root
                        // best-effort from the link's stored absolute target.
                        let root = unknown_root_or_escalate(&entry_path)?;
                        crate::log::warn!(
                            "Project registry: liveness of '{}' is indeterminate (transient I/O); \
                             retaining link and the project as a GC root this run (fail-closed).",
                            entry_path.display()
                        );
                        live.push((file_name, root));
                    }
                    ProbeResult::Dead => {
                        // First check is Dead. Re-probe immediately before
                        // removing: a concurrent `register` may have just
                        // re-pointed this same hash to a now-live project
                        // (CODEX-BLOCK-1 TOCTOU). Only prune if BOTH probes
                        // are Dead; an Unknown re-probe (now unreachable) must
                        // also retain the link (SEC-1).
                        match probe_live_target(&entry_path) {
                            ProbeResult::Live(target) => live.push((file_name, target)),
                            ProbeResult::Unknown => {
                                // Re-probe is now Unknown (filesystem became
                                // unreachable between probes). Same A1
                                // fail-closed contract as the first-probe arm:
                                // retain the link AND the project as a root.
                                let root = unknown_root_or_escalate(&entry_path)?;
                                crate::log::warn!(
                                    "Project registry: liveness of '{}' is indeterminate (transient I/O \
                                     on re-probe); retaining link and the project as a GC root this run \
                                     (fail-closed).",
                                    entry_path.display()
                                );
                                live.push((file_name, root));
                            }
                            ProbeResult::Dead => {
                                // Departed-other-project (the common benign
                                // case) → debug only, never WARN (ARCH-3).
                                crate::log::debug!(
                                    "Project registry: pruning departed link '{}'.",
                                    entry_path.display()
                                );
                                if let Err(e) = crate::symlink::remove(&entry_path) {
                                    crate::log::debug!(
                                        "Project registry: prune of '{}' failed (non-fatal): {e}",
                                        entry_path.display()
                                    );
                                }
                            }
                        }
                    }
                }
            }

            live.sort_by(|a, b| a.0.cmp(&b.0));
            Ok(live.into_iter().map(|(_, target)| target).collect())
        })
        .await
        .map_err(|e| {
            Error::Registry(ProjectRegistryError::new(
                PathBuf::new(),
                ProjectRegistryErrorKind::Io(std::io::Error::other(format!("live_projects task panicked: {e}"))),
            ))
        })?
    }
}

#[cfg(test)]
mod tests {
    //! Specification tests for the flat-symlink-store `ProjectRegistry`
    //! (W1-P3, contract-first TDD).
    //!
    //! These tests encode the behavioural contract from
    //! `adr_project_gc_symlink_ledger.md` §Validation + the plan
    //! `plan_project_toolchain_hardening.md` Component Contracts C1.1–C1.4.
    //! They are authored **before** the W1-P4 implementation: every test that
    //! drives `register` / `live_projects` MUST fail by panicking at the
    //! `unimplemented!()` stub (or, for the helpers, `same_dir` /
    //! `replace_atomic`). A compile error here is a contract bug, not an
    //! expected stub failure.
    //!
    //! The obsolete JSON-ledger tests
    //! (`corrupt_registry_returns_corrupt_error`,
    //! `unknown_schema_version_returns_unknown_version_error`,
    //! `register_rejects_symlink_at_sentinel`,
    //! `load_and_prune_no_write_when_nothing_to_prune`) are intentionally
    //! absent: the symlink store has no JSON document, no schema version, no
    //! sentinel, and no rewrite concept, so those behaviours no longer exist.
    //!
    //! `collect_project_roots_io_failure_is_non_fatal` (plan W1-P3, F4) is
    //! **relocated** to the pytest acceptance file
    //! `test/tests/test_clean_project_backlinks.py::test_clean_io_failure_is_non_fatal`:
    //! `collect_project_roots` lives behind the private `package_manager::tasks`
    //! module (not re-exported — only `CleanResult`/`CleanedObject` are), so it
    //! is unreachable from this crate-internal test module and is not
    //! unit-testable here. The deliberate exit-78 elimination is therefore
    //! pinned at the acceptance level. See that pytest test's docstring.

    use super::*;
    use crate::symlink;
    use std::path::Path;

    /// Canonicalises like the production code (`tokio::fs::canonicalize`'s
    /// sync sibling via `dunce` to keep Windows verbatim prefixes off paths).
    fn canon(p: &Path) -> PathBuf {
        dunce::canonicalize(p).expect("canonicalize test path")
    }

    /// Creates a project directory containing an `ocx.lock` so that
    /// `live_projects`'s `try_exists(<target>/ocx.lock)` liveness probe
    /// observes it as a live GC root. Returns the canonical project dir.
    fn make_live_project(parent: &Path, name: &str) -> PathBuf {
        let dir = parent.join(name);
        std::fs::create_dir_all(&dir).expect("create project dir");
        std::fs::write(dir.join("ocx.lock"), b"# test lock\n").expect("write ocx.lock");
        canon(&dir)
    }

    /// The ledger link name for a project dir — the single-source-of-truth
    /// hash the registry must reuse (`ReferenceManager::name_for_path`,
    /// 16-hex / 64-bit SHA-256). Used to assert link *location*, never
    /// recomputed with a private scheme.
    fn link_name(project_dir: &Path) -> String {
        crate::reference_manager::ReferenceManager::name_for_path(project_dir)
    }

    // ── register: link creation + idempotency ────────────────────────────

    /// C1.1: `register` creates `$OCX_HOME/projects/<hash>` as a symlink
    /// resolving to the canonical project directory when none exists.
    #[tokio::test]
    async fn register_creates_symlink_when_absent() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = make_live_project(home.path(), "proj-a");
        let registry = ProjectRegistry::new(home.path());

        registry.register(&project).await.expect("register should succeed");

        let link = home.path().join("projects").join(link_name(&project));
        assert!(symlink::is_link(&link), "ledger entry must be a symlink");
        assert_eq!(
            canon(&std::fs::read_link(&link).expect("read_link")),
            project,
            "symlink must resolve to the canonical project dir"
        );
    }

    /// C1.1: re-registering the same project is idempotent — exactly one
    /// link, no error, still resolving to the project dir.
    #[tokio::test]
    async fn register_idempotent_same_project() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = make_live_project(home.path(), "proj-a");
        let registry = ProjectRegistry::new(home.path());

        registry.register(&project).await.expect("first register");
        registry.register(&project).await.expect("second register (idempotent)");

        let projects_dir = home.path().join("projects");
        let links: Vec<_> = std::fs::read_dir(&projects_dir)
            .expect("read projects dir")
            .map(|e| e.expect("dir entry").file_name())
            .filter(|n| !n.to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert_eq!(links.len(), 1, "exactly one ledger entry after double register");
        let link = projects_dir.join(link_name(&project));
        assert_eq!(canon(&std::fs::read_link(&link).expect("read_link")), project);
    }

    /// C1.1 / ADR §"No self-link invariant" (ARCH-1b): `register` is a no-op
    /// when `project_dir` is the *same directory* as `$OCX_HOME`. The
    /// identity test is device+inode identity, **not** canonical-path byte
    /// equality.
    ///
    /// Case-fold dimension (ARCH-1b): on a case-insensitive / normalising
    /// filesystem (macOS APFS default, Windows) the binding rule is that a
    /// case-differing spelling of `$OCX_HOME` (e.g. `OCX_HOME` typed back as
    /// `ocx_home`) still denotes the *same* directory by `dev`/`ino`, so the
    /// self-link must still be suppressed even though the path bytes differ.
    /// On case-sensitive Linux CI such a spelling is a *different* directory,
    /// so this test exercises the inode-identity path with the dev/inode-
    /// identical spelling (the same canonical dir) and asserts the no-op.
    /// Byte-equality alone would be insufficient on the case-insensitive
    /// platforms; this test pins the same-directory contract on the platform
    /// it runs on while documenting the case-fold dimension it cannot
    /// directly exercise on Linux.
    #[tokio::test]
    async fn register_noop_when_project_dir_is_ocx_home() {
        let home = tempfile::tempdir().expect("tempdir");
        let ocx_home = canon(home.path());
        let registry = ProjectRegistry::new(&ocx_home);

        // project_dir IS $OCX_HOME (dev/inode-identical) → must be a no-op.
        registry
            .register(&ocx_home)
            .await
            .expect("self-register must be a no-op returning Ok(())");

        let self_link = ocx_home.join("projects").join(link_name(&ocx_home));
        assert!(
            !symlink::is_link(&self_link),
            "no $OCX_HOME/projects/<hash> -> $OCX_HOME self-link may be created"
        );
    }

    /// C1.1 / ADR §Technical Details: the symlink target is the project
    /// **directory**, never the lock file and never the config file.
    #[tokio::test]
    async fn register_target_is_project_dir_not_lockfile() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = make_live_project(home.path(), "proj-a");
        let registry = ProjectRegistry::new(home.path());

        registry.register(&project).await.expect("register");

        let link = home.path().join("projects").join(link_name(&project));
        let target = canon(&std::fs::read_link(&link).expect("read_link"));
        assert_eq!(target, project, "target must be the project dir");
        assert!(target.is_dir(), "target must resolve to a directory");
        assert_ne!(target, project.join("ocx.lock"), "target must NOT be the lock file");
    }

    /// C1.1 / ADR §Risks (ARCH-3, silent-data-loss class): a failure to
    /// create **this** project's own link (store dir unwritable) is logged at
    /// WARN by the caller but is **non-fatal** — `register` itself never
    /// blocks the mutation. The contract for `register` is that the mutation
    /// is unaffected; the caller (`ProjectLock::save`) already swallows the
    /// `Err` to WARN. Here we assert the store-unwritable case does not panic
    /// the caller's transaction model: the call resolves (Ok or Err) without
    /// unwinding past the registry boundary, and a subsequent registration of
    /// a *different* writable project still succeeds (mutation unaffected).
    ///
    /// Skips gracefully when running as root (chmod 0 is ignored by root).
    #[tokio::test]
    #[cfg(unix)]
    async fn register_self_failure_is_warn_not_propagated() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().expect("tempdir");
        let project = make_live_project(home.path(), "proj-a");

        // Make the projects/ parent ($OCX_HOME) read-only so the registry
        // cannot create projects/ or stage a temp link inside it.
        let projects_parent = home.path().to_path_buf();
        let mut perms = std::fs::metadata(&projects_parent).expect("stat home").permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&projects_parent, perms).expect("chmod home read-only");

        // Root ignores DAC permission bits, so the unwritable condition
        // cannot be constructed. Detect that by probing: if a write into the
        // chmod-0555 dir still succeeds we are root — skip without failing
        // (no `unsafe` geteuid needed; the probe IS the precondition).
        let probe = projects_parent.join(".root-probe");
        if std::fs::write(&probe, b"x").is_ok() {
            let _ = std::fs::remove_file(&probe);
            let mut restore = std::fs::metadata(&projects_parent).expect("stat home").permissions();
            restore.set_mode(0o755);
            std::fs::set_permissions(&projects_parent, restore).expect("restore perms");
            eprintln!("skipping register_self_failure_is_warn_not_propagated: running as root");
            return;
        }

        let registry = ProjectRegistry::new(home.path());
        // Per ARCH-3 the failure is non-fatal at the registry boundary: it
        // either returns Err (caller logs WARN) or Ok (best-effort no-op).
        // It must NOT panic / unwind past this point — that would abort the
        // caller's ocx.lock save transaction.
        let _ = registry.register(&project).await;

        // Restore perms so the tempdir can be cleaned and so we can prove the
        // mutation path is unaffected by registering a writable project.
        let mut restore = std::fs::metadata(&projects_parent).expect("stat home").permissions();
        restore.set_mode(0o755);
        std::fs::set_permissions(&projects_parent, restore).expect("restore perms");

        let other = make_live_project(home.path(), "proj-writable");
        registry
            .register(&other)
            .await
            .expect("registration of a writable project must still succeed (mutation unaffected)");
    }

    // ── live_projects: prune + collect ───────────────────────────────────

    /// C1.1: a broken ledger link (target directory removed) is pruned and
    /// not returned as a live root.
    #[tokio::test]
    async fn live_projects_prunes_broken_link() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = make_live_project(home.path(), "proj-a");
        let registry = ProjectRegistry::new(home.path());
        registry.register(&project).await.expect("register");

        // Remove the project dir entirely — the ledger link now dangles.
        std::fs::remove_dir_all(&project).expect("remove project dir");

        let live = registry.live_projects().await.expect("live_projects");

        assert!(live.is_empty(), "broken link must not be a live root: {live:?}");
        let link = home.path().join("projects").join(link_name(&project));
        assert!(!symlink::is_link(&link), "broken link must be pruned");
    }

    /// C1.1: a link whose target directory exists but has no `ocx.lock` is
    /// pruned (lock-absent liveness probe fails).
    #[tokio::test]
    async fn live_projects_prunes_when_ocx_lock_absent() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = make_live_project(home.path(), "proj-a");
        let registry = ProjectRegistry::new(home.path());
        registry.register(&project).await.expect("register");

        // Project dir survives but its ocx.lock is deleted.
        std::fs::remove_file(project.join("ocx.lock")).expect("rm ocx.lock");

        let live = registry.live_projects().await.expect("live_projects");

        assert!(live.is_empty(), "lock-absent project must not be live: {live:?}");
        let link = home.path().join("projects").join(link_name(&project));
        assert!(!symlink::is_link(&link), "lock-absent link must be pruned");
    }

    /// C1.1 / ADR §Decision: a departed *other* project is pruned silently
    /// at DEBUG, never WARN — this is the common benign churn case.
    ///
    /// No log-capture facility exists in `ocx_lib` (`crate::log` is a bare
    /// re-export of `tracing_log::log`), so the no-WARN expectation cannot be
    /// asserted programmatically here. This test asserts the observable
    /// side-effect — the departed link is pruned and the surviving project is
    /// still collected — and documents the no-WARN contract: per
    /// `feedback_no_warn_on_common_benign` and ADR §Decision, the prune in
    /// `live_projects` MUST log at `log::debug!`, NOT `log::warn!`. (The
    /// WARN-level path is reserved exclusively for self-register failure in
    /// `register`, ARCH-3.)
    #[tokio::test]
    async fn live_projects_departed_other_project_is_debug_only() {
        let home = tempfile::tempdir().expect("tempdir");
        let surviving = make_live_project(home.path(), "proj-surviving");
        let departed = make_live_project(home.path(), "proj-departed");
        let registry = ProjectRegistry::new(home.path());
        registry.register(&surviving).await.expect("register surviving");
        registry.register(&departed).await.expect("register departed");

        // The "other" project departs (developer deleted the worktree).
        std::fs::remove_dir_all(&departed).expect("remove departed project");

        let live = registry.live_projects().await.expect("live_projects");

        assert_eq!(live, vec![surviving.clone()], "only the surviving project is live");
        let departed_link = home.path().join("projects").join(link_name(&departed));
        assert!(
            !symlink::is_link(&departed_link),
            "departed-other-project link is pruned (silently, at DEBUG — never WARN)"
        );
    }

    /// C1.1 / edge case: `projects/` absent → `[]`, no error, and the
    /// directory is NOT created as a side effect.
    #[tokio::test]
    async fn live_projects_empty_when_dir_absent() {
        let home = tempfile::tempdir().expect("tempdir");
        let registry = ProjectRegistry::new(home.path());

        let live = registry.live_projects().await.expect("live_projects on absent dir");

        assert!(live.is_empty(), "absent projects/ must yield []");
        assert!(
            !home.path().join("projects").exists(),
            "live_projects must not create the projects/ dir"
        );
    }

    /// SOTA-1c / CODEX-BLOCK-1: a concurrent `register` racing a
    /// `live_projects` readdir must be safe — the registered project is
    /// either observed-and-collected or not-yet-observed, but it is never
    /// corrupted and the `register`d link is never erroneously pruned (its
    /// target is a live project with an `ocx.lock`).
    #[tokio::test]
    async fn live_projects_concurrent_register_during_readdir_is_safe() {
        let home = tempfile::tempdir().expect("tempdir");
        let existing = make_live_project(home.path(), "proj-existing");
        let racing = make_live_project(home.path(), "proj-racing");
        let registry = ProjectRegistry::new(home.path());
        registry.register(&existing).await.expect("register existing");

        let reg_for_task = ProjectRegistry::new(home.path());
        let racing_clone = racing.clone();
        let register_task = tokio::spawn(async move { reg_for_task.register(&racing_clone).await });
        let live = registry
            .live_projects()
            .await
            .expect("live_projects during concurrent register");
        register_task
            .await
            .expect("join register task")
            .expect("concurrent register must still succeed");

        // The pre-existing project is always live. The racing one may or may
        // not be observed (POSIX implementation-defined) but its link must
        // never be pruned and live_projects must not corrupt either entry.
        assert!(
            live.contains(&existing),
            "pre-existing live project must always be collected: {live:?}"
        );
        let racing_link = home.path().join("projects").join(link_name(&racing));
        assert!(
            symlink::is_link(&racing_link),
            "concurrently-registered link must not be erroneously pruned"
        );
        assert!(
            live.iter().all(|p| p == &existing || p == &racing),
            "no spurious roots: {live:?}"
        );
    }

    /// C1.1: concurrent `register` of two *different* projects — independent
    /// per-project atomic paths, no contention, both links present.
    #[tokio::test]
    async fn register_concurrent_two_projects_both_present() {
        let home = tempfile::tempdir().expect("tempdir");
        let proj_a = make_live_project(home.path(), "proj-a");
        let proj_b = make_live_project(home.path(), "proj-b");

        let reg_a = ProjectRegistry::new(home.path());
        let reg_b = ProjectRegistry::new(home.path());
        let a = proj_a.clone();
        let b = proj_b.clone();
        let ta = tokio::spawn(async move { reg_a.register(&a).await });
        let tb = tokio::spawn(async move { reg_b.register(&b).await });
        ta.await.expect("join a").expect("register a");
        tb.await.expect("join b").expect("register b");

        let projects_dir = home.path().join("projects");
        let link_a = projects_dir.join(link_name(&proj_a));
        let link_b = projects_dir.join(link_name(&proj_b));
        assert!(symlink::is_link(&link_a), "proj-a link present");
        assert!(symlink::is_link(&link_b), "proj-b link present");
        assert_eq!(canon(&std::fs::read_link(&link_a).expect("readlink a")), proj_a);
        assert_eq!(canon(&std::fs::read_link(&link_b).expect("readlink b")), proj_b);
    }

    // ── A1: ProbeResult::Unknown ⇒ retained GC root (fail-closed) ────────

    /// Plan §"Living Design — Review-Fix Amendments" A1: when a registered
    /// project's ledger link probes to [`ProbeResult::Unknown`] (a transient,
    /// non-`NotFound` I/O error from `canonicalize`/`try_exists` — an
    /// EACCES/ESTALE momentarily-unreachable filesystem, *not* a deleted
    /// project), `live_projects` MUST:
    ///
    /// 1. **retain** the ledger link (never prune a live-but-unreachable
    ///    project — that would GC its pinned packages), AND
    /// 2. **return that project's directory in the live-root set for this
    ///    run** (fail-closed). The pre-A1 code only retained the link but
    ///    dropped the project from the returned roots, so `ocx clean` would
    ///    still collect its packages this run — the silent-data-loss bug A1
    ///    closes.
    ///
    /// `Unknown` is forced deterministically by making an intermediate path
    /// component of the symlink *target* untraversable (`chmod 0o000` on a
    /// parent directory of the project dir). `is_link` (an `lstat` on the
    /// entry itself) still returns `true`, but `dunce::canonicalize(entry)`
    /// must walk the target and fails with `PermissionDenied` (NOT
    /// `NotFound`) → `ProbeResult::Unknown`.
    ///
    /// Skips gracefully when running as root (root ignores DAC mode bits, so
    /// the unreachable condition cannot be constructed) — same skip pattern
    /// as `register_self_failure_is_warn_not_propagated`.
    #[tokio::test]
    #[cfg(unix)]
    async fn live_projects_unknown_probe_is_retained_root_and_link_kept() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().expect("tempdir");

        // The project lives two levels down: <home>/gate/proj-a. We will
        // chmod 0o000 the `gate/` component so traversing *into* proj-a
        // fails with EACCES (a transient-class error → Unknown), while the
        // ledger entry itself (an lstat-only `is_link`) is unaffected.
        let gate = home.path().join("gate");
        std::fs::create_dir_all(&gate).expect("create gate dir");
        let project = make_live_project(&gate, "proj-a");

        let registry = ProjectRegistry::new(home.path());
        registry.register(&project).await.expect("register");

        let link = home.path().join("projects").join(link_name(&project));
        assert!(symlink::is_link(&link), "precondition: ledger link created");

        // Make the intermediate `gate/` component untraversable. canonicalize
        // of the ledger link must now fail with PermissionDenied, NOT
        // NotFound → ProbeResult::Unknown.
        let mut perms = std::fs::metadata(&gate).expect("stat gate").permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&gate, perms).expect("chmod gate 0000");

        // Root ignores DAC bits — detect and skip (the probe IS the
        // precondition; no unsafe geteuid needed).
        if std::fs::read_dir(&project).is_ok() {
            let mut restore = std::fs::metadata(&gate).expect("stat gate").permissions();
            restore.set_mode(0o755);
            std::fs::set_permissions(&gate, restore).expect("restore gate perms");
            eprintln!("skipping live_projects_unknown_probe_is_retained_root_and_link_kept: running as root");
            return;
        }

        let live = registry.live_projects().await;

        // Restore perms before any assertion can unwind (so the tempdir is
        // cleanable regardless of outcome).
        let mut restore = std::fs::metadata(&gate).expect("stat gate").permissions();
        restore.set_mode(0o755);
        std::fs::set_permissions(&gate, restore).expect("restore gate perms");

        let live = live.expect("live_projects must not error on a transient probe");

        // A1 contract part 2 (fail-closed): the Unknown project's directory
        // MUST be in the returned live set for THIS run. `register` stored an
        // absolute target, so the root is best-effort `read_link`-derived.
        // Pre-A1 code returns an EMPTY set here → this assertion fails now,
        // pinning the silent-data-loss bug.
        assert!(
            live.iter().any(|p| p == &project || p == &gate.join("proj-a")),
            "A1: a transient-Unknown project must be RETAINED as a GC root \
             this run (fail-closed), not dropped; got live set: {live:?}"
        );

        // A1 contract part 1: the ledger link must still be present (an
        // Unknown probe must never prune).
        assert!(
            symlink::is_link(&link),
            "A1: an Unknown probe must NOT prune the ledger link"
        );
    }

    /// Plan A1 sibling: a genuinely [`ProbeResult::Dead`] entry alongside an
    /// [`ProbeResult::Unknown`] one ⇒ the Dead link is pruned while the
    /// Unknown link is retained AND its project returned as a live root. This
    /// pins that A1's fail-closed retention does not also suppress the normal
    /// departed-link pruning, and that the two outcomes are independent.
    ///
    /// Skips gracefully as root (same rationale as the sibling test).
    #[tokio::test]
    #[cfg(unix)]
    async fn live_projects_dead_pruned_while_unknown_retained() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().expect("tempdir");

        // Unknown project: behind an untraversable `gate/` component.
        let gate = home.path().join("gate");
        std::fs::create_dir_all(&gate).expect("create gate");
        let unknown_proj = make_live_project(&gate, "proj-unknown");

        // Dead project: a normal registered project we then delete entirely
        // (broken link → canonicalize NotFound → ProbeResult::Dead).
        let dead_proj = make_live_project(home.path(), "proj-dead");

        let registry = ProjectRegistry::new(home.path());
        registry.register(&unknown_proj).await.expect("register unknown");
        registry.register(&dead_proj).await.expect("register dead");

        std::fs::remove_dir_all(&dead_proj).expect("remove dead project dir");

        let mut perms = std::fs::metadata(&gate).expect("stat gate").permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&gate, perms).expect("chmod gate 0000");

        if std::fs::read_dir(&unknown_proj).is_ok() {
            let mut restore = std::fs::metadata(&gate).expect("stat gate").permissions();
            restore.set_mode(0o755);
            std::fs::set_permissions(&gate, restore).expect("restore gate perms");
            eprintln!("skipping live_projects_dead_pruned_while_unknown_retained: running as root");
            return;
        }

        let live = registry.live_projects().await;

        let mut restore = std::fs::metadata(&gate).expect("stat gate").permissions();
        restore.set_mode(0o755);
        std::fs::set_permissions(&gate, restore).expect("restore gate perms");

        let live = live.expect("live_projects must not error");

        let unknown_link = home.path().join("projects").join(link_name(&unknown_proj));
        let dead_link = home.path().join("projects").join(link_name(&dead_proj));

        // Dead → pruned.
        assert!(!symlink::is_link(&dead_link), "a definitively Dead link must be pruned");
        // Unknown → retained.
        assert!(
            symlink::is_link(&unknown_link),
            "an Unknown link must be retained alongside a pruned Dead one"
        );
        // Unknown → returned as a live root this run (A1 fail-closed); Dead →
        // not returned. Pre-A1 returns neither → first assertion fails now.
        assert!(
            live.iter()
                .any(|p| p == &unknown_proj || p == &gate.join("proj-unknown")),
            "A1: the Unknown project must be a retained GC root this run; got {live:?}"
        );
        assert!(
            !live.iter().any(|p| p == &dead_proj),
            "the Dead project must NOT be a GC root: {live:?}"
        );
    }

    // ── invariant guard for the dropped canonical_config capture ──────────

    /// C1.3 / ARCH-4d regression: the register call sites dropped the
    /// separate `canonical_config` capture (the old JSON model needed it to
    /// record a custom `--project=<custom>.toml` basename). That is safe
    /// **only** because `lock_path_for(config)` is invariantly
    /// `<config.parent()>/ocx.lock` for *every* config basename. This test
    /// guards that invariant directly so a future weakening of
    /// `lock_path_for` (which would silently break the custom-`--project`
    /// multi-project GC contract) fails here loudly.
    ///
    /// This intentionally duplicates the assertion of
    /// `lock::tests::lock_path_for_always_produces_ocx_lock_in_config_dir`:
    /// the duplication is the point — it pins the invariant *from the
    /// registry's perspective* (the consumer that depends on it for the
    /// dropped capture) so the coupling is visible at the registry site.
    #[test]
    fn lock_path_for_invariant_protects_custom_project_basename() {
        use crate::project::lock::lock_path_for;

        assert_eq!(
            lock_path_for(Path::new("/dev/proj-a/ocx.toml")),
            PathBuf::from("/dev/proj-a/ocx.lock"),
            "standard basename"
        );
        assert_eq!(
            lock_path_for(Path::new("/dev/proj-a/workspace.toml")),
            PathBuf::from("/dev/proj-a/ocx.lock"),
            "custom --project=workspace.toml basename must still derive <dir>/ocx.lock"
        );
        assert_eq!(
            lock_path_for(Path::new("/dev/proj-a/Manifest")),
            PathBuf::from("/dev/proj-a/ocx.lock"),
            "extension-free custom basename"
        );
    }

    // ── A2′: read_dir enumeration failure must fail CLOSED ────────────────

    /// Plan §"Living Design — Review-Fix Amendments" A2′ (Round-2 correction):
    /// a **non-`NotFound`** `std::fs::read_dir(projects_dir)` error MUST NOT be
    /// swallowed into `Ok(Vec::new())`. Downstream that is indistinguishable
    /// from "no projects registered" ⇒ `ocx clean` runs with zero project
    /// roots ⇒ every live project's pinned packages collected — the exact
    /// fail-open silent-data-loss bug A2 exists to close, merely relocated
    /// from the per-entry probe to the directory enumeration.
    ///
    /// Deterministic seam: make `projects_dir` itself a **regular file** (not
    /// a directory). `std::fs::read_dir` on a non-directory fails with
    /// `ErrorKind::NotADirectory` (Linux) / `Other` — categorically NOT
    /// `NotFound`. The A2′ contract requires this to propagate as
    /// `Err(Error::Registry(.. ProjectRegistryErrorKind::Io ..))` so the
    /// caller (`collect_project_roots`) aborts (`IoError`) instead of GC-ing
    /// a live multi-project store. No chmod, no root-skip.
    ///
    /// FAILS against current code: the `Err(e) => { debug!; Ok(Vec::new()) }`
    /// arm at `registry.rs:385-394` returns `Ok(empty)` here.
    #[tokio::test]
    async fn live_projects_read_dir_non_notfound_err_fails_closed() {
        let home = tempfile::tempdir().expect("tempdir");

        // $OCX_HOME/projects is a REGULAR FILE, not a directory. read_dir on
        // it yields NotADirectory/Other — a non-NotFound enumeration error.
        let projects_path = home.path().join("projects");
        std::fs::write(&projects_path, b"not a directory").expect("write projects-as-file");
        assert!(
            projects_path.is_file(),
            "precondition: $OCX_HOME/projects must be a regular file"
        );

        let registry = ProjectRegistry::new(home.path());
        let result = registry.live_projects().await;

        let err = result.expect_err(
            "A2′: a non-NotFound read_dir error on projects/ MUST fail closed \
             (Err), not degrade to Ok(empty) — that is the fail-open \
             silent-data-loss bug",
        );
        match err {
            Error::Registry(ProjectRegistryError {
                kind: ProjectRegistryErrorKind::Io(_),
                ..
            }) => {}
        }
    }

    /// A2′ paired carve-out guard: a **genuinely absent** `projects/` directory
    /// MUST stay `Ok(Vec::new())` (no projects registered is a valid state,
    /// not an error). This pins the `NotFound` exception so the A2′ fix does
    /// not over-correct into failing closed on a fresh `$OCX_HOME`. Must
    /// remain GREEN both before and after the fix.
    #[tokio::test]
    async fn live_projects_read_dir_notfound_stays_ok_empty() {
        let home = tempfile::tempdir().expect("tempdir");
        // No projects/ entry exists at all.
        assert!(
            !home.path().join("projects").exists(),
            "precondition: projects/ genuinely absent"
        );

        let registry = ProjectRegistry::new(home.path());
        let live = registry
            .live_projects()
            .await
            .expect("absent projects/ (NotFound) must stay Ok(empty), never fail closed");

        assert!(live.is_empty(), "absent projects/ ⇒ no roots: {live:?}");
        assert!(
            !home.path().join("projects").exists(),
            "live_projects must not create projects/ as a side effect"
        );
    }

    /// Plan A2″ (Round-3, systemic fail-closed): the per-entry `ReadDir`
    /// iterator `Err` arm previously did `debug!`+`continue`, silently
    /// dropping a possibly-live registered project (the 3rd relocation of the
    /// same silent-data-loss bug — A2 `?`, A2′ dir-open, A2″ per-entry iter).
    /// The fix closes the class at the function contract by materialising the
    /// directory iterator through `collect::<io::Result<_>>()?`: any per-entry
    /// `Err` short-circuits to `Err(Error::Registry(.. Io ..))`, the same
    /// fail-closed outcome as the dir-open arm.
    ///
    /// A per-entry `readdir(3)` iterator `Err` (a short read / EIO / ESTALE
    /// *mid-enumeration*) is **not deterministically forceable in-process**
    /// without root-only fault injection (FUSE error device, `seccomp`, or an
    /// NFS server kill between `opendir` and `readdir`) — none available in a
    /// hermetic `cargo nextest` run. The systemic contract is therefore pinned
    /// at the boundary it *is* forceable (the dir-open arm,
    /// `live_projects_read_dir_non_notfound_err_fails_closed`) plus the pytest
    /// regression `test_clean_project_backlinks.py::
    /// test_clean_survives_departed_project_before_survivor`. This test pins
    /// the complementary half: the materialisation MUST NOT regress the happy
    /// path — every successfully-enumerated registered live project still
    /// becomes a GC root after the `collect()` change (no entry silently lost,
    /// deterministic sort preserved).
    #[tokio::test]
    async fn live_projects_collects_all_registered_after_materialisation() {
        let home = tempfile::tempdir().expect("tempdir");
        let proj_a = make_live_project(home.path(), "proj-a");
        let proj_b = make_live_project(home.path(), "proj-b");
        let proj_c = make_live_project(home.path(), "proj-c");

        let registry = ProjectRegistry::new(home.path());
        registry.register(&proj_a).await.expect("register a");
        registry.register(&proj_b).await.expect("register b");
        registry.register(&proj_c).await.expect("register c");

        let mut live = registry
            .live_projects()
            .await
            .expect("all-live registry must enumerate without failing closed");

        // Sorted by link name internally; assert as a set so the test does not
        // couple to hash-ordering, then assert determinism separately.
        let mut expected = vec![proj_a, proj_b, proj_c];
        expected.sort();
        live.sort();
        assert_eq!(
            live, expected,
            "every successfully-enumerated registered live project must be a \
             GC root (collect::<io::Result<_>>()? must not drop entries)"
        );

        let again = registry
            .live_projects()
            .await
            .expect("second enumeration must also succeed");
        assert_eq!(
            registry.live_projects().await.expect("third enumeration"),
            again,
            "enumeration must be deterministic across runs"
        );
    }

    // ── unknown_root_or_escalate: Err escalation (helper-level) ───────────

    /// Plan A1/A2 escalation contract, gap #3: [`unknown_root_or_escalate`]
    /// is the seam the `live_projects` `Unknown` arms delegate their
    /// fail-closed root recovery to. Its documented contract: on a valid
    /// ledger symlink it returns the `read_link` target; on a non-symlink or
    /// an absent path `read_link` fails ⇒ it escalates to
    /// `Err(Error::Registry(.. ProjectRegistryErrorKind::Io ..))` (the entry's
    /// root cannot be recovered ⇒ fail closed rather than silently dropping a
    /// possibly-live GC root).
    ///
    /// Deterministic: no chmod, no root-skip. Two escalation inputs —
    /// (a) a path that is a **regular file** (not a symlink), and (b) an
    /// **absent** path — both must yield the `Io` registry error. The
    /// happy-path (valid symlink ⇒ `Ok(read_link target)`) is asserted too so
    /// the contract is pinned end to end.
    ///
    /// FAILS against current code only if the escalation/return contract is
    /// not honoured; authored here so the helper-level guarantee the re-probe
    /// `[Dead, Unknown]` arm relies on is explicitly covered (see
    /// `live_projects_dead_pruned_while_unknown_retained` docstring for why
    /// the single-entry Dead→Unknown transition is not in-process
    /// deterministically forceable).
    #[test]
    fn unknown_root_or_escalate_errs_on_non_symlink_and_absent() {
        let home = tempfile::tempdir().expect("tempdir");

        // (a) regular file — read_link fails (not a symlink).
        let regular = home.path().join("regular-entry");
        std::fs::write(&regular, b"x").expect("write regular file");
        let err_a = unknown_root_or_escalate(&regular).expect_err("read_link on a non-symlink must escalate to Err");
        match err_a {
            Error::Registry(ProjectRegistryError {
                kind: ProjectRegistryErrorKind::Io(_),
                ..
            }) => {}
        }

        // (b) absent path — read_link fails with NotFound.
        let absent = home.path().join("does-not-exist");
        let err_b = unknown_root_or_escalate(&absent).expect_err("read_link on an absent path must escalate to Err");
        match err_b {
            Error::Registry(ProjectRegistryError {
                kind: ProjectRegistryErrorKind::Io(_),
                ..
            }) => {}
        }

        // Happy path: a valid ledger symlink ⇒ Ok(read_link target). Pins the
        // recovery side of the contract the Unknown arms depend on.
        let target = make_live_project(home.path(), "proj-recover");
        let link = home.path().join("ledger-link");
        symlink::create(&target, &link).expect("create ledger symlink");
        let recovered = unknown_root_or_escalate(&link).expect("valid symlink must recover its read_link root");
        assert_eq!(
            canon(&recovered),
            target,
            "recovered root must be the link's stored target"
        );
    }
}
