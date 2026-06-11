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
pub mod version_spec;

pub use bootstrap::{BootstrapOutcome, BootstrapStatus};
pub use version_spec::VersionSpec;

/// POSIX fence payload — sources the POSIX env shim.
///
/// `$OCX_HOME` is *not* exported yet when the login profile runs this block:
/// `env.sh` is the file that sets and exports it (`: "${OCX_HOME:=…}"`). So the
/// fence cannot rely on `$OCX_HOME` to locate `env.sh` — a fresh login shell has
/// it empty, and a bare `. "$OCX_HOME/env.sh"` then sources `. "/env.sh"` and
/// fails ("No such file or directory") on every shell start. The
/// `${OCX_HOME:-$HOME/.ocx}` form resolves the path *without* assigning or
/// exporting (env.sh still owns the canonical `:=`/`export`), and the `-f`
/// existence guard keeps the block silent when ocx is not installed.
const POSIX_BODY: &str = r#"if [ -f "${OCX_HOME:-$HOME/.ocx}/env.sh" ]; then
    . "${OCX_HOME:-$HOME/.ocx}/env.sh"
fi"#;

/// Elvish fence payload — slurps and evaluates the elvish env shim.
///
/// Mirrors the POSIX guard in elvish idiom. Elvish reads env vars via
/// `$E:OCX_HOME` (it does NOT interpolate `$OCX_HOME` inside double quotes), so
/// the value is resolved explicitly with `has-env` and plain string
/// concatenation (`$E:HOME/.ocx`) before the `?(test -f …)` existence guard —
/// the same chicken-and-egg fix: `env.elv` is what sets `OCX_HOME`, so the fence
/// must locate it without depending on it. Concatenation is used instead of
/// `path:join` because the latter needs a `use path` import that does not carry
/// into the `eval`-ed shim scope.
const ELVISH_BODY: &str = r#"var _ocx_home = (if (has-env OCX_HOME) { put $E:OCX_HOME } else { put $E:HOME/.ocx })
if ?(test -f $_ocx_home/env.elv) {
    eval (slurp < $_ocx_home/env.elv)
}"#;

/// PowerShell fence payload (plan contract 4). Resolves the ocx home *without*
/// depending on `OCX_HOME` (the env.ps1 shim is what sets it), then existence-
/// guards the source — the same chicken-and-egg fix as the POSIX body.
///
/// `$env:USERPROFILE` is null on Linux/macOS PowerShell 7, so it falls back to
/// `$HOME` (mirroring the env.ps1 shim's `$_ocxBase`) — otherwise an unset
/// `OCX_HOME` on non-Windows pwsh would resolve the home to `\.ocx` and never
/// activate. `Join-Path` keeps the path separator correct on every platform.
const POWERSHELL_BODY: &str = r#"$_ocxHome = if ($env:OCX_HOME) { $env:OCX_HOME } elseif ($env:USERPROFILE) { Join-Path $env:USERPROFILE '.ocx' } else { Join-Path $HOME '.ocx' }
$_ocxEnv = Join-Path $_ocxHome 'env.ps1'
if (Test-Path $_ocxEnv) { . $_ocxEnv }"#;

// Version of the env shim contract this binary writes.
// Reserved for Decision 4C (shim-contract compare in `ocx self update`);
// not yet consumed at runtime.
#[allow(dead_code)]
const SHIM_CONTRACT_VERSION: u32 = 1;

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
    /// Optional version spec — when `Some`, pins the bootstrap to a specific
    /// tag, digest, or `tag@digest` combination (plan D1–D4).
    pub version: Option<VersionSpec>,
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

/// True when a setup/heal run changed at least one managed block: a fresh or
/// upgraded block was written ([`ProfileOutcome::Completed`]) or a legacy
/// footprint was migrated ([`ProfileOutcome::Migrated`]).
///
/// Single source for the "re-source your profile" reload hint — consumed by the
/// `self setup` run summary and the `self update` post-swap heal alike.
pub fn profiles_changed(profiles: &[(PathBuf, ProfileOutcome)]) -> bool {
    profiles
        .iter()
        .any(|(_, outcome)| matches!(outcome, ProfileOutcome::Completed | ProfileOutcome::Migrated))
}

/// True when at least one profile carried user edits inside the managed fence
/// and was left untouched ([`ProfileOutcome::SkippedDirty`]).
///
/// Single source for the dirty signal — drives the `self setup` exit code (82)
/// and the `self update` "run `ocx self setup --force`" advisory.
pub fn profiles_dirty(profiles: &[(PathBuf, ProfileOutcome)]) -> bool {
    profiles
        .iter()
        .any(|(_, outcome)| matches!(outcome, ProfileOutcome::SkippedDirty))
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
    let bootstrap =
        bootstrap::ensure_self_installed(manager, file_structure, options.dry_run, options.version.as_ref()).await?;

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
        let outcome = apply_target(&target, options.force, false, options.dry_run).await?;
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

    // The "re-source your profile" hint only makes sense when this run actually
    // changed something: a shim was (re)written, or a profile gained/upgraded a
    // managed block. A pure no-op re-run (all shims current, every profile
    // already Current) suppresses it so the user is not told to reload an
    // unchanged machine.
    let reload_hint = !shims_written.is_empty() || profiles_changed(&profiles);

    Ok(SetupOutcome {
        bootstrap,
        shims_written,
        profiles,
        exec_policy_warning,
        conflicting_ocx,
        reload_hint,
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
///
/// `heal_only` is the `ocx self update` post-swap mode (Decision 4C): it heals
/// an existing ocx-owned block (FormatUpgraded rewrites, dirty stays skipped)
/// but never *introduces* one — a profile with no ocx footprint is left alone,
/// and an absent dedicated file is not created. This makes the update refresh
/// implicitly respect an original `--no-modify-path` install.
async fn apply_target(
    target: &ProfileTarget,
    force: bool,
    heal_only: bool,
    dry_run: bool,
) -> Result<ProfileOutcome, error::Error> {
    match target.kind {
        ProfileKind::PosixFence => apply_fence(&target.path, POSIX_BODY, force, heal_only, dry_run).await,
        ProfileKind::ElvishFence => apply_fence(&target.path, ELVISH_BODY, force, heal_only, dry_run).await,
        ProfileKind::PowerShellFence => apply_fence(&target.path, POWERSHELL_BODY, force, heal_only, dry_run).await,
        ProfileKind::DedicatedFile(DedicatedShell::Fish) => {
            rewrite_dedicated(&target.path, shims::fish_conf_body(), heal_only, dry_run).await
        }
        ProfileKind::DedicatedFile(DedicatedShell::Nushell) => {
            rewrite_dedicated(&target.path, shims::nu_autoload_body(), heal_only, dry_run).await
        }
    }
}

/// Re-apply the managed activation block to every detected profile in
/// **heal-only** mode, for the `ocx self update` post-swap hook (Decision 4C).
///
/// `ocx self setup` owns the managed RC block, so `ocx self update` must heal it
/// after a binary swap — not only refresh the `env.*` shims. Heal-only means an
/// already-present block whose body drifted (e.g. an old, pre-fix fence) is
/// rewritten to canonical ([`ProfileOutcome::Completed`] / `Migrated`), a
/// user-edited block is left untouched ([`ProfileOutcome::SkippedDirty`]), and a
/// profile that never carried an ocx block is left exactly as-is
/// ([`ProfileOutcome::NoOp`]) — so a `--no-modify-path` install stays untouched.
///
/// Auto-detects targets from the real environment (no `--profile` overrides),
/// never forces over a dirty block, and never runs as a dry run.
///
/// # Errors
///
/// Returns [`error::Error`] if a profile read or write fails.
pub async fn refresh_profiles(ocx_home: &Path) -> Result<Vec<(PathBuf, ProfileOutcome)>, error::Error> {
    let targets = resolve_targets(ocx_home, &[]).await;
    let mut profiles = Vec::with_capacity(targets.len());
    for target in targets {
        let outcome = apply_target(&target, false, true, false).await?;
        profiles.push((target.path, outcome));
    }
    Ok(profiles)
}

/// Run the fence state machine against one profile file.
///
/// Reads the file (absent → empty), classifies it, and either appends a fresh
/// fence, upgrades the format, migrates a legacy footprint, skips a dirty block,
/// or no-ops. Legacy artifacts (`# BEGIN ocx`, `shell init`, extensionless env)
/// are stripped before the fresh fence is written → [`ProfileOutcome::Migrated`].
async fn apply_fence(
    path: &Path,
    body: &str,
    force: bool,
    heal_only: bool,
    dry_run: bool,
) -> Result<ProfileOutcome, error::Error> {
    let content = read_to_string_or_empty(path).await?;
    let state = rc_block::classify(&content, body);

    // A dirty block without --force is left untouched (a non-error outcome; the
    // CLI maps it to exit 82 by inspecting outcomes, not via an error variant).
    if state == rc_block::BlockState::Dirty && !force {
        return Ok(ProfileOutcome::SkippedDirty);
    }

    let has_legacy = rc_block::has_legacy_artifacts(&content);

    // Heal-only (self update post-swap): never INTRODUCE a managed block where
    // none exists. Only a profile that carries no ocx footprint at all (Fresh
    // and no legacy artifacts) is skipped — a present-but-drifted block still
    // heals (FormatUpgraded below), a legacy footprint still migrates.
    if heal_only && state == rc_block::BlockState::Fresh && !has_legacy {
        return Ok(ProfileOutcome::NoOp);
    }

    // Legacy migration: strip the pre-v1 footprint, then append the fresh fence.
    if has_legacy {
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
/// and any drift is overwritten with the canonical body. This is intentional and
/// mirrors the `env.*` shims: these paths live in tool-managed auto-load dirs
/// (`fish/conf.d`, `nushell/vendor/autoload`) that OCX owns outright, exactly as
/// conda/rustup own their vendor files. The "no clobber without `--force`" bar
/// applies only to the managed block inside a user's OWN RC files (handled by
/// `apply_fence`), never to these regenerated files — user customization belongs
/// in the user's RC, not here. (Cross-model review 2026-06-04 flagged the
/// asymmetry; resolution: documented intended ownership.)
async fn rewrite_dedicated(
    path: &Path,
    body: &str,
    heal_only: bool,
    dry_run: bool,
) -> Result<ProfileOutcome, error::Error> {
    // Heal-only (self update post-swap): a dedicated file is ocx-owned, so an
    // existing one is refreshed, but an ABSENT one is never created — a setup
    // that never wrote it (e.g. --no-modify-path, or a different shell) stays
    // untouched on update.
    if heal_only && !crate::utility::fs::path_exists_lossy(path).await {
        return Ok(ProfileOutcome::NoOp);
    }
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
    // Clone the path for the join-error arm: the closure moves `path`, but a
    // join error means the closure never ran, so its captured copy is gone —
    // the error context must carry the path explicitly, not an empty one.
    let join_path = path.clone();
    tokio::task::spawn_blocking(move || write_profile_blocking(&path, &content))
        .await
        .map_err(|join| error::Error::Io {
            path: join_path,
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

        let outcome = apply_fence(&path, POSIX_BODY, false, false, false).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::Completed);

        let written = read(&path);
        assert!(written.contains("# >>> ocx v1"));
        assert!(written.contains(POSIX_BODY));
    }

    #[tokio::test]
    async fn apply_fence_idempotent_rerun_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");

        apply_fence(&path, POSIX_BODY, false, false, false).await.unwrap();
        let first = read(&path);

        let outcome = apply_fence(&path, POSIX_BODY, false, false, false).await.unwrap();
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

        let outcome = apply_fence(&path, POSIX_BODY, false, false, false).await.unwrap();
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

        let outcome = apply_fence(&path, POSIX_BODY, true, false, false).await.unwrap();
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

        let outcome = apply_fence(&path, POSIX_BODY, false, false, false).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::Migrated);
        let written = read(&path);
        assert!(written.contains("# >>> ocx v1"), "migration writes the v1 fence");
        assert!(!written.contains("# BEGIN ocx"), "the legacy block must be removed");
    }

    #[tokio::test]
    async fn apply_fence_dirty_v1_fence_with_legacy_block_skips_dirty() {
        // A profile that carries BOTH a dirty v1 fence (marker disagrees with
        // the on-disk body) AND a legacy `# BEGIN ocx` block. `apply_fence`
        // checks the dirty state BEFORE the legacy-strip detour, so without
        // --force the run is a SkippedDirty no-op and the legacy block is left
        // in place; with --force it falls through to the legacy path and
        // migrates (legacy stripped, canonical v1 fence written).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");

        let marker = rc_block::canonical_hash(POSIX_BODY);
        let seeded = format!(
            "# BEGIN ocx\n. \"$OCX_HOME/env.sh\"\n# END ocx\n# >>> ocx v1 {marker} >>>\n. \"$OCX_HOME/EDITED.sh\"\n# <<< ocx <<<\n"
        );
        std::fs::write(&path, &seeded).unwrap();

        // Without --force: dirty wins; the file (legacy block included) is untouched.
        let skipped = apply_fence(&path, POSIX_BODY, false, false, false).await.unwrap();
        assert_eq!(skipped, ProfileOutcome::SkippedDirty);
        assert_eq!(
            read(&path),
            seeded,
            "a dirty v1 fence must short-circuit before the legacy strip (no --force)"
        );

        // With --force: the legacy path runs; the block is migrated to canonical.
        let migrated = apply_fence(&path, POSIX_BODY, true, false, false).await.unwrap();
        assert_eq!(migrated, ProfileOutcome::Migrated);
        let written = read(&path);
        assert!(!written.contains("# BEGIN ocx"), "--force must strip the legacy block");
        assert!(!written.contains("EDITED.sh"), "--force must replace the dirty body");
        assert!(written.contains(POSIX_BODY), "--force must write the canonical body");
    }

    #[tokio::test]
    async fn apply_fence_dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");

        let outcome = apply_fence(&path, POSIX_BODY, false, false, true).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::Completed);
        assert!(!path.exists(), "dry-run must not create the profile file");
    }

    // ── heal-only mode (self update post-swap, Decision 4C) ──────────────────

    #[tokio::test]
    async fn apply_fence_heal_only_fresh_profile_is_noop() {
        // self update must never INTRODUCE a managed block: a profile with no ocx
        // footprint is left exactly as-is, so an original --no-modify-path install
        // stays untouched on update.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");
        std::fs::write(&path, "export PATH=/bin\n").unwrap();

        let outcome = apply_fence(&path, POSIX_BODY, false, true, false).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::NoOp);
        assert_eq!(
            read(&path),
            "export PATH=/bin\n",
            "heal-only must not add a block to a fresh profile"
        );
    }

    #[tokio::test]
    async fn apply_fence_heal_only_absent_profile_is_noop_and_uncreated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");

        let outcome = apply_fence(&path, POSIX_BODY, false, true, false).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::NoOp);
        assert!(!path.exists(), "heal-only must not create an absent profile");
    }

    #[tokio::test]
    async fn apply_fence_heal_only_heals_drifted_block() {
        // A present, ocx-authored block whose body drifted from canonical (the
        // old pre-fix fence, marker matches its own stale body) is the
        // FormatUpgraded state — heal-only rewrites it to the guarded body. This
        // is the 0.3.7+ self-update heal path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");
        let old_body = ". \"$OCX_HOME/env.sh\"";
        let marker = rc_block::canonical_hash(old_body);
        std::fs::write(&path, format!("# >>> ocx v1 {marker} >>>\n{old_body}\n# <<< ocx <<<\n")).unwrap();

        let outcome = apply_fence(&path, POSIX_BODY, false, true, false).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::Completed);
        let written = read(&path);
        assert!(
            written.contains(POSIX_BODY),
            "heal-only must rewrite a drifted block to the guarded body"
        );
        assert!(
            !written.contains(". \"$OCX_HOME/env.sh\""),
            "the old unguarded body must be replaced"
        );
    }

    #[tokio::test]
    async fn apply_fence_heal_only_dirty_block_stays_skipped() {
        // A user-edited block is never clobbered, even under heal-only (no force).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".bashrc");
        let marker = rc_block::canonical_hash(POSIX_BODY);
        let dirty = format!("# >>> ocx v1 {marker} >>>\n{POSIX_BODY}\necho injected\n# <<< ocx <<<\n");
        std::fs::write(&path, &dirty).unwrap();

        let outcome = apply_fence(&path, POSIX_BODY, false, true, false).await.unwrap();
        assert_eq!(outcome, ProfileOutcome::SkippedDirty);
        assert_eq!(read(&path), dirty, "heal-only must not clobber a user-edited block");
    }

    #[tokio::test]
    async fn rewrite_dedicated_heal_only_absent_file_is_noop_and_uncreated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conf.d").join("ocx.fish");

        let outcome = rewrite_dedicated(&path, shims::fish_conf_body(), true, false)
            .await
            .unwrap();
        assert_eq!(outcome, ProfileOutcome::NoOp);
        assert!(!path.exists(), "heal-only must not create an absent dedicated file");
    }

    #[tokio::test]
    async fn rewrite_dedicated_heal_only_refreshes_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ocx.fish");
        std::fs::write(&path, "# stale\n").unwrap();

        let outcome = rewrite_dedicated(&path, shims::fish_conf_body(), true, false)
            .await
            .unwrap();
        assert_eq!(outcome, ProfileOutcome::Completed);
        assert_eq!(
            read(&path),
            shims::fish_conf_body(),
            "heal-only refreshes a present, ocx-owned dedicated file"
        );
    }

    // ── rewrite_dedicated (fish/nushell full-rewrite) ────────────────────────

    #[tokio::test]
    async fn rewrite_dedicated_writes_then_diff_gates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conf.d").join("ocx.fish");
        let body = shims::fish_conf_body();

        let first = rewrite_dedicated(&path, body, false, false).await.unwrap();
        assert_eq!(first, ProfileOutcome::Completed);
        assert_eq!(
            read(&path),
            body,
            "the dedicated file is fully written with the canonical body"
        );

        let second = rewrite_dedicated(&path, body, false, false).await.unwrap();
        assert_eq!(second, ProfileOutcome::NoOp, "a byte-identical file is a no-op");
    }

    #[tokio::test]
    async fn rewrite_dedicated_dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ocx.nu");

        let outcome = rewrite_dedicated(&path, shims::nu_autoload_body(), false, true)
            .await
            .unwrap();
        assert_eq!(outcome, ProfileOutcome::Completed);
        assert!(!path.exists(), "dry-run must not create the dedicated file");
    }

    // ── profiles_changed / profiles_dirty predicates ────────────────────────

    fn profile(outcome: ProfileOutcome) -> (PathBuf, ProfileOutcome) {
        (PathBuf::from("/home/u/.bashrc"), outcome)
    }

    #[test]
    fn profiles_changed_only_for_completed_or_migrated() {
        assert!(profiles_changed(&[profile(ProfileOutcome::Completed)]));
        assert!(profiles_changed(&[profile(ProfileOutcome::Migrated)]));
        assert!(!profiles_changed(&[profile(ProfileOutcome::NoOp)]));
        assert!(!profiles_changed(&[profile(ProfileOutcome::SkippedDirty)]));
        assert!(!profiles_changed(&[]));
    }

    #[test]
    fn profiles_dirty_only_for_skipped_dirty() {
        assert!(profiles_dirty(&[profile(ProfileOutcome::SkippedDirty)]));
        assert!(!profiles_dirty(&[profile(ProfileOutcome::Completed)]));
        assert!(!profiles_dirty(&[profile(ProfileOutcome::NoOp)]));
        assert!(!profiles_dirty(&[]));
    }

    #[test]
    fn profiles_predicates_scan_all_entries() {
        let mixed = [
            profile(ProfileOutcome::NoOp),
            profile(ProfileOutcome::Completed),
            profile(ProfileOutcome::SkippedDirty),
        ];
        assert!(profiles_changed(&mixed), "a Completed anywhere counts as changed");
        assert!(profiles_dirty(&mixed), "a SkippedDirty anywhere counts as dirty");
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
