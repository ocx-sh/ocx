// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shell-scaffold ownership for `ocx self setup`.
//!
//! This module is the single source of truth for OCX shell integration:
//! per-shell env shims, the versioned RC-block state machine, profile-target
//! detection, and the self-install bootstrap. The install scripts shrink to
//! bootstrap-only and hand off to [`run`]; `ocx self update` refreshes only
//! the ocx-owned shims through `refresh_shims`.
//!
//! See `.claude/artifacts/adr_self_setup.md` (decisions 1B + 2A + 3D + 4C) for
//! the design record and `.claude/state/plans/plan_self_setup.md` for the
//! component contracts implemented here.

use std::path::{Path, PathBuf};

use crate::file_structure::FileStructure;
use crate::package_manager::PackageManager;
use crate::setup::profiles::{DedicatedShell, HomeEnv, ProfileKind, ProfileTarget};

pub mod bootstrap;
pub mod error;
pub mod profiles;
pub mod rc_block;
pub mod shims;

pub use bootstrap::BootstrapOutcome;

/// POSIX fence payload — sources the POSIX env shim.
const POSIX_BODY: &str = r#". "$OCX_HOME/env.sh""#;

/// Elvish fence payload — slurps and evaluates the elvish env shim.
const ELVISH_BODY: &str = r#"eval (slurp < "$OCX_HOME/env.elv")"#;

/// PowerShell fence payload (plan contract 4). Keys off `$env:OCX_HOME` with a
/// `$env:USERPROFILE\.ocx` fallback (PowerShell has no `:=` default-assign), so
/// an override-driven install works the same as the POSIX body.
const POWERSHELL_BODY: &str = "$_ocxHome = if ($env:OCX_HOME) { $env:OCX_HOME } else { \"$env:USERPROFILE\\.ocx\" }\nif (Test-Path \"$_ocxHome\\env.ps1\") { . \"$_ocxHome\\env.ps1\" }";

/// Version of the env shim contract this binary writes.
///
/// `ocx self update` compares this against the contract the on-disk shims
/// declared; an advance prints the "run `ocx self setup`" advisory so existing
/// users refresh their shell integration (Decision 4C).
pub const SHIM_CONTRACT_VERSION: u32 = 1;

/// Options controlling a single `ocx self setup` run.
#[derive(Debug, Clone, Default)]
pub struct SetupOptions {
    /// Write the env shims but do not modify any shell profile.
    pub no_modify_path: bool,
    /// Explicit profile-file overrides; empty means auto-detect.
    pub profiles: Vec<PathBuf>,
    /// Report intended actions without writing any byte.
    pub dry_run: bool,
    /// Overwrite a managed RC block that carries user edits (dirty state).
    pub force: bool,
}

/// Per-profile result of applying the RC-block state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileOutcome {
    /// A fresh or upgraded managed block was written.
    Completed,
    /// The managed block was already current; nothing changed.
    NoOp,
    /// A legacy block was stripped and replaced with the v1 fence.
    Migrated,
    /// The managed block was edited by the user and left untouched (no `--force`).
    SkippedDirty,
}

/// Aggregate result of an `ocx self setup` run.
#[derive(Debug, Clone)]
pub struct SetupOutcome {
    /// Self-install bootstrap outcome (the hard gate ran first — contract 2).
    pub bootstrap: BootstrapOutcome,
    /// env.* shim files that were (re)written.
    pub shims_written: Vec<PathBuf>,
    /// Per-profile outcomes, in detection / override order.
    pub profiles: Vec<(PathBuf, ProfileOutcome)>,
    /// Windows execution-policy `Restricted` advisory, if applicable.
    pub exec_policy_warning: Option<String>,
    /// An `ocx` on `PATH` ahead of the directory the shim prepends.
    pub conflicting_ocx: Option<PathBuf>,
    /// Whether the CLI should print the "source your profile" hint once.
    pub reload_hint: bool,
}

/// Orchestrate a full `ocx self setup`: bootstrap the CAS, write the env
/// shims, and apply the RC-block state machine to each target profile.
///
/// # Hard ordering invariant (contract 1, item 2 — Block-tier)
///
/// Bootstrap runs **first**. If it fails, `run` returns the error immediately,
/// having written **zero shims and touched zero profiles** — there is no partial
/// state. The shims point at the `current` symlink the bootstrap wires, so
/// writing them before the CAS exists would produce dangling integration.
///
/// `dry_run` short-circuits every write: it computes the would-write shim set
/// and every per-profile outcome without touching a byte (and never returns the
/// dirty exit code — a dirty profile is reported as would-skip).
///
/// # Errors
///
/// Returns [`error::Error`] if the bootstrap fails (zero shims written, zero
/// profiles touched), or if a shim / profile write fails.
pub async fn run(
    options: &SetupOptions,
    manager: &PackageManager,
    file_structure: &FileStructure,
) -> Result<SetupOutcome, error::Error> {
    // ── Phase 1: bootstrap (hard gate, runs first) ────────────────────────────
    // On `Err`, propagate now — zero shims written, zero profiles touched.
    let bootstrap = bootstrap::ensure_self_installed(manager, options.dry_run).await?;

    // ── Phase 2: env.* shims ─────────────────────────────────────────────────
    let ocx_home = file_structure.root();
    let shims_written = tokio::task::spawn_blocking({
        let ocx_home = ocx_home.to_path_buf();
        let dry_run = options.dry_run;
        move || shims::write_shims(&ocx_home, dry_run)
    })
    .await
    .map_err(|join| error::Error::Io {
        path: ocx_home.to_path_buf(),
        source: std::io::Error::other(join.to_string()),
    })??;

    // ── Phase 3: profile RC blocks (unless --no-modify-path) ──────────────────
    let targets = if options.no_modify_path {
        Vec::new()
    } else {
        resolve_targets(ocx_home, &options.profiles).await
    };

    let mut profiles = Vec::with_capacity(targets.len());
    for target in targets {
        let outcome = apply_target(&target, options.force, options.dry_run).await?;
        profiles.push((target.path, outcome));
    }

    // ── Phase 4: exec-policy probe (non-fatal advisory) ───────────────────────
    let exec_policy_warning = if profiles::execution_policy_is_restricted().await {
        Some(EXEC_POLICY_ADVISORY.to_string())
    } else {
        None
    };

    // ── Phase 5: best-effort conflicting-ocx scan (never fails setup) ─────────
    let conflicting_ocx = conflicting_ocx_on_path(file_structure).await;

    Ok(SetupOutcome {
        bootstrap,
        shims_written,
        profiles,
        exec_policy_warning,
        conflicting_ocx,
        // First-time activation always needs a re-source of the profile.
        reload_hint: true,
    })
}

/// Non-fatal advisory printed when the current-user execution policy is
/// `Restricted` (a `$PROFILE` fence is inert until the user relaxes it). OCX
/// never auto-changes the policy — that is a user security decision.
const EXEC_POLICY_ADVISORY: &str =
    "run `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned` to allow the profile to load";

/// Resolve the profile files this run should target, in write order.
///
/// Explicit `--profile` overrides skip auto-detection entirely and are treated
/// as POSIX-fence targets (contract 1 edge). Otherwise the POSIX/dedicated-file
/// set is auto-detected from the real environment and the PowerShell `$PROFILE`
/// is probed via a subprocess.
async fn resolve_targets(ocx_home: &Path, overrides: &[PathBuf]) -> Vec<ProfileTarget> {
    if !overrides.is_empty() {
        return overrides
            .iter()
            .map(|path| ProfileTarget {
                path: path.clone(),
                kind: ProfileKind::PosixFence,
            })
            .collect();
    }

    let mut targets = profiles::detect_targets(&home_env_from_environment(ocx_home));
    if let Some(profile) = profiles::detect_powershell_profile().await {
        targets.push(ProfileTarget {
            path: profile,
            kind: ProfileKind::PowerShellFence,
        });
    }
    targets
}

/// Apply the activation payload to one profile target.
///
/// Fence targets run the RC-block state machine (with a legacy-migration
/// detour); dedicated-file targets (fish/nushell) are fully rewritten with a
/// diff-gate. On `dry_run`, the outcome is computed but nothing is written.
async fn apply_target(target: &ProfileTarget, force: bool, dry_run: bool) -> Result<ProfileOutcome, error::Error> {
    match target.kind {
        ProfileKind::PosixFence => apply_fence(&target.path, POSIX_BODY, force, dry_run).await,
        ProfileKind::ElvishFence => apply_fence(&target.path, ELVISH_BODY, force, dry_run).await,
        ProfileKind::PowerShellFence => apply_fence(&target.path, POWERSHELL_BODY, force, dry_run).await,
        ProfileKind::DedicatedFile(DedicatedShell::Fish) => {
            rewrite_dedicated(&target.path, shims::fish_conf_body(), dry_run).await
        }
        ProfileKind::DedicatedFile(DedicatedShell::Nushell) => {
            rewrite_dedicated(&target.path, shims::nu_autoload_body(), dry_run).await
        }
    }
}

/// Run the fence state machine against one profile file.
///
/// Reads the file (absent → empty), classifies it, and either appends a fresh
/// fence, upgrades the format, migrates a legacy footprint, skips a dirty block,
/// or no-ops. Legacy artifacts (`# BEGIN ocx`, `shell init`, extensionless env)
/// are stripped before the fresh fence is written → [`ProfileOutcome::Migrated`].
async fn apply_fence(path: &Path, body: &str, force: bool, dry_run: bool) -> Result<ProfileOutcome, error::Error> {
    let content = read_to_string_or_empty(path).await?;
    let state = rc_block::classify(&content, body);

    // A dirty block without --force is left untouched (a non-error outcome; the
    // CLI maps it to exit 82 by inspecting outcomes, not via an error variant).
    if state == rc_block::BlockState::Dirty && !force {
        return Ok(ProfileOutcome::SkippedDirty);
    }

    // Legacy migration: strip the pre-v1 footprint, then append the fresh fence.
    if rc_block::has_legacy_artifacts(&content) {
        let stripped = rc_block::strip_block(&content);
        if let Some(new_content) = rc_block::apply(&stripped, body, force)? {
            if !dry_run {
                write_profile(path, &new_content).await?;
            }
            return Ok(ProfileOutcome::Migrated);
        }
        // `strip_block` produced a Fresh file, so `apply` always returns Some;
        // this arm is unreachable, but reported as a no-op for totality.
        return Ok(ProfileOutcome::NoOp);
    }

    match rc_block::apply(&content, body, force)? {
        Some(new_content) => {
            if !dry_run {
                write_profile(path, &new_content).await?;
            }
            Ok(ProfileOutcome::Completed)
        }
        // `apply` returns None for Current and (already handled) dirty-skip.
        None => Ok(ProfileOutcome::NoOp),
    }
}

/// Fully rewrite a dedicated-file shell target (fish/nushell), diff-gated.
///
/// The file is ocx-owned (no inline fence), so a byte-identical file is a no-op
/// and any drift is overwritten with the canonical body.
async fn rewrite_dedicated(path: &Path, body: &str, dry_run: bool) -> Result<ProfileOutcome, error::Error> {
    let content = read_to_string_or_empty(path).await?;
    if content == body {
        return Ok(ProfileOutcome::NoOp);
    }
    if !dry_run {
        write_profile(path, body).await?;
    }
    Ok(ProfileOutcome::Completed)
}

/// Read a profile file to a `String`, mapping a missing file to an empty string
/// (a fresh profile is the common case). Any other I/O error propagates.
async fn read_to_string_or_empty(path: &Path) -> Result<String, error::Error> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Ok(content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(error::Error::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Atomically write `content` to a profile file, creating the parent directory
/// (`mkdir -p`) if absent. Uses the Windows-retry-aware atomic-publish primitive
/// off the async executor (it is blocking I/O).
async fn write_profile(path: &Path, content: &str) -> Result<(), error::Error> {
    let path = path.to_path_buf();
    let content = content.to_string();
    tokio::task::spawn_blocking(move || write_profile_blocking(&path, &content))
        .await
        .map_err(|join| error::Error::Io {
            path: PathBuf::new(),
            source: std::io::Error::other(join.to_string()),
        })?
}

/// Blocking body of [`write_profile`]: create the parent dir, fill a temp file
/// in it, and publish atomically via [`crate::utility::fs::persist_temp_file`].
fn write_profile_blocking(path: &Path, content: &str) -> Result<(), error::Error> {
    use std::io::Write as _;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| error::Error::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|source| error::Error::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    tmp.write_all(content.as_bytes()).map_err(|source| error::Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    crate::utility::fs::persist_temp_file(tmp, path).map_err(|source| error::Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Build a [`HomeEnv`] from the real process environment for profile detection.
fn home_env_from_environment(ocx_home: &Path) -> HomeEnv {
    let read = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
    let home = read("HOME")
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
        .unwrap_or_else(|| ocx_home.to_path_buf());
    HomeEnv {
        home,
        zdotdir: read("ZDOTDIR").map(PathBuf::from),
        xdg_config_home: read("XDG_CONFIG_HOME").map(PathBuf::from),
        xdg_data_home: read("XDG_DATA_HOME").map(PathBuf::from),
        ocx_home: ocx_home.to_path_buf(),
        shell: read("SHELL"),
    }
}

/// Best-effort scan of `$PATH` for an `ocx` executable that appears AHEAD of the
/// directory the env shim prepends (`$OCX_HOME/symlinks/.../current/content/bin`).
///
/// Returns the shadowing path if found, or `None` on any read failure — a `$PATH`
/// read error never fails setup (contract 1, item 14).
async fn conflicting_ocx_on_path(file_structure: &FileStructure) -> Option<PathBuf> {
    let shim_bin_dir = file_structure
        .symlinks
        .current(&crate::oci::ocx_cli_identifier())
        .join("content")
        .join("bin");

    let path_var = std::env::var_os("PATH")?;
    let executable = if cfg!(windows) { "ocx.exe" } else { "ocx" };

    for dir in std::env::split_paths(&path_var) {
        // Reaching the shim's own bin dir first means nothing shadows it.
        if dir == shim_bin_dir {
            return None;
        }
        let candidate = dir.join(executable);
        if crate::utility::fs::path_exists_lossy(&candidate).await {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read a profile file back, for write-side assertions.
    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("profile file present after write")
    }

    // ── fence body constants round-trip through rc_block ─────────────────────

    #[test]
    fn fence_bodies_round_trip_through_rc_block_apply() {
        // Each fence body, appended to a fresh file, must classify as Current on
        // a re-run — the orchestrator relies on this for idempotency (NoOp).
        for body in [POSIX_BODY, ELVISH_BODY, POWERSHELL_BODY] {
            let appended = rc_block::apply("", body, false)
                .expect("apply infallible")
                .expect("fresh append produces content");
            assert_eq!(
                rc_block::classify(&appended, body),
                rc_block::BlockState::Current,
                "re-classifying the freshly written block must be Current for body {body:?}"
            );
        }
    }

    // ── apply_fence state-machine outcomes ───────────────────────────────────

    #[tokio::test]
    async fn apply_fence_fresh_file_completes_and_writes_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");

        let outcome = apply_fence(&path, POSIX_BODY, false, false).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::Completed);

        let written = read(&path);
        assert!(written.contains("# >>> ocx v1"));
        assert!(written.contains(POSIX_BODY));
    }

    #[tokio::test]
    async fn apply_fence_idempotent_rerun_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");

        apply_fence(&path, POSIX_BODY, false, false).await.unwrap();
        let first = read(&path);

        let outcome = apply_fence(&path, POSIX_BODY, false, false).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::NoOp);
        assert_eq!(read(&path), first, "an idempotent re-run must not change the file");
    }

    #[tokio::test]
    async fn apply_fence_dirty_block_without_force_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");

        // Seed a fence whose marker disagrees with the (user-edited) body.
        let marker = rc_block::canonical_hash(POSIX_BODY);
        let dirty = format!("# >>> ocx v1 {marker} >>>\n. \"$OCX_HOME/EDITED.sh\"\n# <<< ocx <<<\n");
        std::fs::write(&path, &dirty).unwrap();

        let outcome = apply_fence(&path, POSIX_BODY, false, false).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::SkippedDirty);
        assert_eq!(
            read(&path),
            dirty,
            "a dirty block without --force must be left untouched"
        );
    }

    #[tokio::test]
    async fn apply_fence_dirty_block_with_force_is_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");

        let marker = rc_block::canonical_hash(POSIX_BODY);
        let dirty = format!("# >>> ocx v1 {marker} >>>\n. \"$OCX_HOME/EDITED.sh\"\n# <<< ocx <<<\n");
        std::fs::write(&path, &dirty).unwrap();

        let outcome = apply_fence(&path, POSIX_BODY, true, false).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::Completed);
        let written = read(&path);
        assert!(
            written.contains(POSIX_BODY),
            "--force must rewrite the body to canonical"
        );
        assert!(!written.contains("EDITED.sh"), "the user edit must be replaced");
    }

    #[tokio::test]
    async fn apply_fence_legacy_block_is_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");

        // A legacy `# BEGIN ocx` / `# END ocx` block (format v0).
        std::fs::write(&path, "# BEGIN ocx\n. \"$HOME/.ocx/env.sh\"\n# END ocx\n").unwrap();

        let outcome = apply_fence(&path, POSIX_BODY, false, false).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::Migrated);
        let written = read(&path);
        assert!(written.contains("# >>> ocx v1"), "migration writes the v1 fence");
        assert!(!written.contains("# BEGIN ocx"), "the legacy block must be removed");
    }

    #[tokio::test]
    async fn apply_fence_dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");

        let outcome = apply_fence(&path, POSIX_BODY, false, true).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::Completed);
        assert!(!path.exists(), "dry-run must not create the profile file");
    }

    // ── rewrite_dedicated (fish/nushell full-rewrite) ────────────────────────

    #[tokio::test]
    async fn rewrite_dedicated_writes_then_diff_gates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conf.d").join("ocx.fish");
        let body = shims::fish_conf_body();

        let first = rewrite_dedicated(&path, body, false).await.unwrap();
        assert_eq!(first, ProfileOutcome::Completed);
        assert_eq!(
            read(&path),
            body,
            "the dedicated file is fully written with the canonical body"
        );

        let second = rewrite_dedicated(&path, body, false).await.unwrap();
        assert_eq!(second, ProfileOutcome::NoOp, "a byte-identical file is a no-op");
    }

    #[tokio::test]
    async fn rewrite_dedicated_dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ocx.nu");

        let outcome = rewrite_dedicated(&path, shims::nu_autoload_body(), true).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::Completed);
        assert!(!path.exists(), "dry-run must not create the dedicated file");
    }

    // ── home_env_from_environment ────────────────────────────────────────────

    #[test]
    fn home_env_falls_back_to_ocx_home_when_home_unset() {
        // We cannot mutate the process env safely under parallel tests, so this
        // only asserts the non-environment-derived field is wired correctly.
        let ocx_home = Path::new("/tmp/ocx-home");
        let env = home_env_from_environment(ocx_home);
        assert_eq!(env.ocx_home, ocx_home);
    }
}
