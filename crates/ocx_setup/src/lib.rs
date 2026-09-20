// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Self-install: bootstrap, env shim files, managed RC blocks, profile detection.
//!
//! Shell-scaffold ownership for `ocx self setup`.
//!
//! This module is the single source of truth for OCX shell integration:
//! per-shell env shims, the versioned RC-block state machine, profile-target
//! detection, and the self-install bootstrap. The install scripts shrink to
//! bootstrap-only and hand off to [`run`]; `ocx self update` hands off to it
//! too, re-executing the newly pulled binary as `ocx self setup … --handoff`
//! ([`SetupOptions::handoff`]) so every surface is written by the new version's
//! own code instead of a subset of these phases run from the old binary.
//!
//! See `.claude/artifacts/adr_self_setup.md` (decisions 1B + 2A + 3D + 4C) for
//! the design record and `.claude/state/plans/plan_self_setup.md` for the
//! component contracts implemented here.

use std::path::{Path, PathBuf};

use crate::profiles::{DedicatedShell, HomeEnv, ProfileKind, ProfileTarget};
use ocx_package_manager::PackageManager;
use ocx_store::file_structure::FileStructure;

pub mod bootstrap;
pub mod error;
pub mod profiles;
pub mod rc_block;
pub mod session_path;
pub mod shell_config;
pub mod shims;
pub mod version_spec;

pub use bootstrap::{BootstrapOutcome, BootstrapStatus};
pub use session_path::{
    SessionPathError, SessionPathFormat, SessionPathOutcome, deregister_session_path, register_session_path,
    session_path_stores,
};
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

/// Options controlling a single `ocx self setup` run.
#[derive(Debug, Clone, Default)]
pub struct SetupOptions {
    /// Write the env shims but do not modify any shell profile, **and** leave
    /// the session PATH alone. The wider of the two refusals — see `profiles`.
    pub no_modify_path: bool,
    /// Which shell profiles this run writes:
    ///
    /// - `None` — auto-detect the target set from the environment.
    /// - `Some(&[])` — write **no** profile block at all (`--no-profile`).
    /// - `Some(paths)` — write exactly those files.
    ///
    /// `Some(&[])` is deliberately *not* `no_modify_path`: it suppresses only
    /// the profile blocks, leaving the env shims and the session-PATH arm
    /// running. Collapsing the two is the bug this tri-state exists to prevent.
    pub profiles: Option<Vec<PathBuf>>,
    /// Report intended actions without writing any byte.
    pub dry_run: bool,
    /// Overwrite a managed RC block that carries user edits (dirty state).
    pub force: bool,
    /// This run is the child half of an `ocx self update` hand-off, so it may
    /// **heal** a managed block but must never *introduce* one (Decision 4C).
    ///
    /// A genuinely first-time `ocx self setup` is indistinguishable from an
    /// update-triggered one from inside [`run`] — no config, no block, no shims
    /// either way — so the parent states which it is instead of this layer
    /// guessing. On a machine that is already set up the flag costs nothing: a
    /// drifted block still heals and a legacy footprint still migrates; only a
    /// profile with no ocx footprint at all is left alone.
    pub handoff: bool,
    /// Optional version spec — when `Some`, pins the bootstrap to a specific
    /// tag, digest, or `tag@digest` combination (plan D1–D4).
    pub version: Option<VersionSpec>,
    /// The effective managed-config tier to adopt, already resolved by the
    /// caller: `Some(ref)` adopts (sync fetch+persist, then fence write),
    /// `Some("")` clears, `None` leaves the tier untouched. The CLI applies the
    /// flag → `OCX_MANAGED_CONFIG` → `[managed].source` seed precedence before
    /// building these options; this layer just consumes the result.
    pub managed_config: Option<String>,
}

impl SetupOptions {
    /// Whether the session-PATH arm runs — the encoding preflight (phase 0) and
    /// the registration (phase 3.5) alike.
    ///
    /// Only `--no-modify-path` suppresses it. An empty `profiles` list does not:
    /// a session PATH is not a profile block, and the two opt-outs are separate
    /// promises. This is the single site that decides it, so the preflight and
    /// the writer can never disagree about which run touches a PATH store.
    fn touches_session_path(&self) -> bool {
        !self.no_modify_path
    }
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
/// Single source for the "re-source your profile" reload hint, in every mode:
/// a hand-off run ([`SetupOptions::handoff`]) reports the same outcomes, so a
/// healed block counts as changed and a profile left alone
/// ([`ProfileOutcome::NoOp`]) does not.
pub fn profiles_changed(profiles: &[(PathBuf, ProfileOutcome)]) -> bool {
    profiles
        .iter()
        .any(|(_, outcome)| matches!(outcome, ProfileOutcome::Completed | ProfileOutcome::Migrated))
}

/// True when at least one session-PATH store gained the ocx directories on
/// this run ([`SessionPathOutcome::Written`]).
///
/// The third input to the reload hint, beside [`profiles_changed`] and a
/// non-empty shim set. A session PATH is read once, when the session starts —
/// by the systemd user manager for an `environment.d` drop-in, by `launchd` at
/// login for the macOS agent, by the desktop shell for the Windows registry
/// value — so a store this run wrote reaches nothing already running, and the
/// user has to be told. Without it the common re-run (shims current, every
/// profile already `Current`, session PATH newly written) printed a `written`
/// row and no guidance at all.
pub fn session_path_written(session_path: &[(PathBuf, SessionPathOutcome)]) -> bool {
    session_path
        .iter()
        .any(|(_, outcome)| *outcome == SessionPathOutcome::Written)
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
    /// Whether this run changed a PATH surface, so the CLI should tell the
    /// user what to reload.
    ///
    /// True when a shim was written, a managed profile block changed, or a
    /// session-PATH store was written ([`session_path_written`]). Which
    /// sentence to print is the CLI's decision, not this flag's: a profile is
    /// re-sourced, a session PATH is only re-read at the next login.
    pub reload_hint: bool,
    /// Result of adopting/clearing the `--managed-config` tier (phase 1.5).
    pub managed_config: ManagedConfigSetupOutcome,
    /// Per-store session-PATH outcomes (C-036), keyed by the location each
    /// platform owns.
    ///
    /// **Always populated on a supported host**, in every state — including a
    /// `--no-modify-path` run, which reports
    /// [`SessionPathOutcome::SkippedOptOut`] for each store it did not touch.
    /// A [`SessionPathOutcome::Failed`] here is warned about and exits 0; the
    /// only session-PATH condition that changes the exit code is the C-037
    /// encoding refusal, and that never reaches this field because it is an
    /// `Err` from [`run`].
    pub session_path: Vec<(PathBuf, SessionPathOutcome)>,
    /// Result of persisting an installer-exported `OCX_EXTRA_CA_CERTS` value
    /// into `config.toml` (phase 0.5, C-008, ocx#448).
    pub extra_ca_certs: ExtraCaCertsOutcome,
}

/// Outcome of `ocx self setup` phase 0.5 (C-008, ocx#448): persisting an
/// installer-exported `OCX_EXTRA_CA_CERTS` value into the home-tier
/// `config.toml` as `extra_ca_certs_pem`, so the corp CA an installer used to
/// bootstrap survives past the `mktemp` file the installer deletes on exit
/// (D-5). `ocx config setup` does not run this phase.
#[derive(Debug, Clone)]
pub enum ExtraCaCertsOutcome {
    /// `OCX_EXTRA_CA_CERTS` was unset or `""` — nothing to do, zero I/O.
    NotConfigured,
    /// The resolved value equals what `config.toml` already carries —
    /// compared as the parsed TOML string, not file bytes (C-008) — so no
    /// write.
    Unchanged {
        /// Number of certificates the resolved bundle carries.
        certificates: usize,
    },
    /// The resolved, validated value was written.
    Persisted {
        /// Number of certificates the persisted bundle carries.
        certificates: usize,
    },
    /// `--dry-run`: a value resolved and validated, but nothing was written.
    WouldPersist {
        /// Number of certificates the resolved bundle carries.
        certificates: usize,
    },
    /// `OCX_EXTRA_CA_CERTS` was set, but the pair is system-locked
    /// (ocx#469): nothing was validated or written, since the loader takes
    /// the pair from `/etc/ocx/config.toml` alone and would ignore the
    /// home-tier value on every later invocation.
    SystemLocked,
}

/// Result of the managed-config adoption phase (1.5) inside `ocx self setup`.
#[derive(Debug, Clone)]
pub enum ManagedConfigSetupOutcome {
    /// No `--managed-config` flag/env/seed resolved — nothing to do.
    NotConfigured,
    /// The resolved ref matches the existing seed, a matching snapshot is on
    /// disk, and the re-sync produced no new content — either the registry
    /// still serves the recorded digest, or the refresh was deliberately
    /// skipped (digest-pinned seed, no network, in-force pause).
    AlreadyAdopted {
        /// The existing snapshot's verified digest (operator TOFU signal —
        /// decision 10: the digest is always visible on adopt paths).
        digest: ocx_oci::Digest,
    },
    /// An already-adopted seed was re-synced and the registry served newer
    /// content: the snapshot was replaced in place, the fence untouched.
    Refreshed {
        /// The digest the snapshot carried before this run.
        from: ocx_oci::Digest,
        /// The newly persisted digest.
        to: ocx_oci::Digest,
    },
    /// The refresh of an already-adopted seed failed before anything was
    /// written (fetch fault, source vanished from the registry, or a published
    /// payload that fails validation); the existing snapshot is kept and the
    /// run still succeeds (exit 0). Never reported as `AlreadyAdopted` — a
    /// refresh that did not run must not look healthy. A snapshot-*write*
    /// failure is NOT downgraded to this outcome: it can leave the new payload
    /// live under the old provenance, so it propagates as an error instead.
    RefreshUnavailable {
        /// The retained snapshot's digest — unchanged by the failed refresh.
        digest: ocx_oci::Digest,
        /// Why the refresh did not complete — the fetch error with its full
        /// `source()` chain flattened in (`ocx_util::error::render_chain`), since
        /// the outermost message alone names no cause.
        reason: String,
    },
    /// `--dry-run` against an already-adopted seed: a refresh would run, but
    /// nothing was fetched and nothing was written.
    WouldRefresh {
        /// The existing snapshot's digest.
        digest: ocx_oci::Digest,
    },
    /// A new or changed ref was adopted: synchronous fetch+persist succeeded
    /// before the fence was written (ADR "Setup ordering").
    Adopted {
        /// The newly persisted manifest digest.
        digest: ocx_oci::Digest,
    },
    /// `--managed-config ""` cleared the fence and deleted the snapshot dir.
    Cleared,
    /// The `[managed]` fence carries user edits and `--force` was not passed;
    /// left untouched. The CLI maps this to exit 82, mirroring
    /// [`ProfileOutcome::SkippedDirty`].
    Dirty,
    /// `--dry-run`: an adopt/re-adopt would run, but nothing was fetched or
    /// written.
    WouldAdopt,
}

/// Orchestrate a full `ocx self setup`: bootstrap the CAS, write the env
/// shims, and apply the RC-block state machine to each target profile.
///
/// # Hard ordering invariant (contract 1, item 2 — Block-tier)
///
/// The session-PATH encoding refusal runs **before** bootstrap, as phase 0:
/// an `$OCX_HOME` this host's PATH format cannot spell is refused with the
/// machine byte-identical — no snapshot fetched, no shim written, no profile
/// touched. It sits there rather than beside the registration it guards
/// because it is the only refusal that would otherwise fire after four
/// writing phases. `--no-modify-path` suppresses it along with the whole
/// session-PATH arm. Phase 0.5, `OCX_EXTRA_CA_CERTS` persistence (C-008,
/// ocx#448), is a second pre-write refusal in the same spirit: a refused or
/// oversized extra-CA value is caught before bootstrap ever fetches, leaving
/// the machine byte-identical.
///
/// Bootstrap runs first among the **writing** phases. If it fails, `run` returns the error immediately,
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
/// profiles touched), or if a shim / profile write fails; also
/// [`error::Error::ExtraCaCerts`], [`error::Error::ExtraCaCertsNotUtf8`] or
/// [`error::Error::RenderedConfigTooLarge`] if phase 0.5's
/// `OCX_EXTRA_CA_CERTS` persistence refuses before bootstrap runs.
pub async fn run(
    options: &SetupOptions,
    config: &ocx_config::Config,
    manager: &PackageManager,
    file_structure: &FileStructure,
) -> Result<SetupOutcome, error::Error> {
    // ── Phase 0: session-PATH encoding preflight (C-037) ──────────────────────
    // Ahead of every writing phase, because the refusal's whole promise is that
    // a refused run leaves the machine byte-identical. The two directories are
    // pure path joins off `file_structure` (no bootstrap, no config), so there
    // is nothing this check needs that a later phase produces. Suppressed under
    // `--no-modify-path` for the same reason phase 3.5 is (C-043): the arm never
    // runs, so an `$OCX_HOME` it could not spell is not this run's problem.
    if options.touches_session_path() {
        session_path::refuse_unencodable(&session_path_directories(file_structure))?;
    }

    // ── Phase 0.5: persist OCX_EXTRA_CA_CERTS (C-008, ocx#448) ────────────────
    // Before bootstrap and no network: a later fetch failure must still leave
    // the CA persisted, since the whole point is that the corp registry
    // bootstrap below can trust it.
    let extra_ca_certs = persist_extra_ca_certs(
        &file_structure.locks,
        file_structure.root(),
        ocx_util::env::var(ocx_config::env::keys::OCX_EXTRA_CA_CERTS).as_deref(),
        options.dry_run,
        config.extra_ca_certs_system_locked,
    )
    .await?;

    // ── Phase 1: bootstrap (hard gate, runs first) ────────────────────────────
    // On `Err`, propagate now — zero shims written, zero profiles touched.
    let bootstrap =
        bootstrap::ensure_self_installed(manager, file_structure, options.dry_run, options.version.as_ref()).await?;

    // ── Phase 1.5: managed-config adoption (ADR "Setup ordering (AMENDED)":
    // resolve ref (flag>env>seed) → synchronous fetch+persist FIRST → fence
    // write only on success) ──────────────────────────────────────────────
    let managed_config = apply_managed_config(
        config,
        options.managed_config.as_deref(),
        options.dry_run,
        options.force,
        manager,
        file_structure,
    )
    .await?;

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
    let profiles = apply_profile_phase(ocx_home, options).await?;

    // ── Phase 3.5: session PATH (unless --no-modify-path) ─────────────────────
    // Co-primary with the profile blocks rather than a fallback for them: a
    // profile reaches login shells, a session PATH reaches everything else.
    // The `?` below is C-037's encoding refusal (exit 78) and nothing else —
    // a *write* failure comes back inside the `Ok` as
    // `SessionPathOutcome::Failed`, which `emit_advisories` warns about and
    // which never changes the exit code (C-036). Phase 0 already applied that
    // same refusal to the same directories, so reaching it here means a caller
    // bypassed `run`; the check stays because the writers own it, not because
    // this path is expected to fire.
    let session_path = if !options.touches_session_path() {
        // C-043 suppresses the whole arm, so the writers are never called and
        // cannot name their own stores; the caller enumerates them instead.
        session_path::session_path_stores(ocx_home)
            .into_iter()
            .map(|store| (store, SessionPathOutcome::SkippedOptOut))
            .collect()
    } else {
        let directories = session_path_directories(file_structure);
        let retired = retired_session_path_directories(file_structure);
        tokio::task::spawn_blocking({
            let ocx_home = ocx_home.to_path_buf();
            let dry_run = options.dry_run;
            move || {
                // C-084, and strictly **before** the registration: Windows and
                // macOS merge into the stored value and never subtract what a
                // previous run wrote, so the entry an unreleased build left at
                // `<root>/bin` would sit there forever beside the current one.
                // Linux regenerates `ocx.conf` wholesale and self-heals, which
                // is why this call is unconditional rather than platform-gated
                // — a platform-gated one would be a branch no Linux run can
                // observe.
                //
                // The outcome is dropped on purpose: `Unchanged` is the answer
                // on every machine that never ran an unreleased build, and it
                // is not a fact a user asked about. The `?` is C-037's encoding
                // refusal only, and it is unreachable here — phase 0 already
                // applied that refusal to the longer `active/bin` spelling
                // under the same `$OCX_HOME`.
                session_path::deregister_session_path(&ocx_home, &retired, dry_run)?;
                session_path::register_session_path(&ocx_home, &directories, dry_run)
            }
        })
        .await
        .map_err(|join| error::Error::Io {
            path: ocx_home.to_path_buf(),
            source: std::io::Error::other(join.to_string()),
        })??
    };

    // ── Phase 4: exec-policy probe (non-fatal advisory) ───────────────────────
    let exec_policy_warning = if profiles::execution_policy_is_restricted().await {
        Some(EXEC_POLICY_ADVISORY.to_string())
    } else {
        None
    };

    // ── Phase 5: best-effort conflicting-ocx scan (never fails setup) ─────────
    let conflicting_ocx = conflicting_ocx_on_path(file_structure).await;

    // The reload hint only makes sense when this run actually changed a PATH
    // surface: a shim was (re)written, a profile gained/upgraded a managed
    // block, or a session-PATH store was written. A pure no-op re-run (all
    // shims current, every profile already Current, every store Unchanged)
    // suppresses it so the user is not told to reload an unchanged machine.
    // The three inputs stay separate rather than collapsing into one boolean
    // here, because the *remedy* differs per surface and the CLI renders one
    // line per surface from these same three predicates.
    let reload_hint = !shims_written.is_empty() || profiles_changed(&profiles) || session_path_written(&session_path);

    Ok(SetupOutcome {
        bootstrap,
        shims_written,
        profiles,
        exec_policy_warning,
        conflicting_ocx,
        reload_hint,
        managed_config,
        session_path,
        extra_ca_certs,
    })
}

/// Phase 0.5 of [`run`] (C-008, ocx#448): resolve `OCX_EXTRA_CA_CERTS` as
/// C-005's env arm ([`ocx_config::tls::from_env_value`]: a
/// value containing `-----BEGIN` is inline PEM, D-4; else a path read via
/// [`ocx_util::fs::read_bounded`]), validate it through
/// [`ocx_config::tls::parse_pem`] (C-004, the single choke
/// point), and persist it as root-level `extra_ca_certs_pem` in
/// `<home>/config.toml` through [`ocx_config::edit::edit`] — the one
/// locked read-modify-write every `config.toml` writer shares (ocx#468) —
/// removing a root `extra_ca_certs` key if present (XOR: `ocx self setup`
/// persists inline PEM only, D-5, never a path). Diff-gated on the parsed
/// TOML string, so a byte-identical re-run and a CRLF-escaped re-render both
/// count as unchanged. Refuses (before any write) when the rendered document
/// would exceed the config loader's 64 KiB ceiling.
///
/// `env_value` is `None` or `""` for "unset" (matching every other
/// `=""`-is-unset `OCX_*` variable in this crate) — a no-op with zero I/O.
/// `system_locked` is [`ocx_config::Config::extra_ca_certs_system_locked`]
/// (ocx#469): a set value under the lock is
/// [`ExtraCaCertsOutcome::SystemLocked`], also with zero I/O — the loader
/// would ignore whatever was persisted, and the same loader skips the env
/// value unvalidated, so setup neither validates nor writes it.
///
/// `home` is `file_structure.root()` (`$OCX_HOME`), never
/// `ConfigLoader::user_path()` — the same write target `shell_config::set`
/// uses; `locks_root` is `file_structure.locks`.
///
/// The validation (the bounded file read and
/// [`ocx_config::tls::parse_pem`]'s probe build, DX-11) runs on the
/// blocking pool ahead of the edit, so the lock is never held for it.
///
/// # Errors
///
/// [`error::Error::ExtraCaCerts`] for a path value the bounded read refuses
/// and for anything [`ocx_config::tls::parse_pem`] refuses
/// (C-004); [`error::Error::ExtraCaCertsNotUtf8`] for a valid bundle that is
/// not UTF-8 and so cannot be a TOML string;
/// [`error::Error::Io`] for a `config.toml` read/parse/write failure;
/// [`error::Error::RenderedConfigTooLarge`] when the document is, or would
/// render, over the size ceiling.
async fn persist_extra_ca_certs(
    locks_root: &Path,
    home: &Path,
    env_value: Option<&str>,
    dry_run: bool,
    system_locked: bool,
) -> Result<ExtraCaCertsOutcome, error::Error> {
    use ocx_config::edit::{self, EditError, EditOutcome};
    use ocx_config::tls::{from_env_value, parse_pem};

    const PEM_KEY: &str = "extra_ca_certs_pem";
    const PATH_KEY: &str = "extra_ca_certs";

    let Some(value) = env_value.filter(|value| !value.is_empty()) else {
        return Ok(ExtraCaCertsOutcome::NotConfigured);
    };
    if system_locked {
        return Ok(ExtraCaCertsOutcome::SystemLocked);
    }
    let config_path = home.join("config.toml");
    let io_error = |source: std::io::Error| error::Error::Io {
        path: config_path.clone(),
        source,
    };

    // C-005's env arm: a path is read now (D-5 — the installer's file is gone
    // on exit) and the text kept as read, so what lands in `config.toml` is
    // the operator's bundle, not a re-encoding of it.
    let value = value.to_owned();
    let (pem, certificates) = tokio::task::spawn_blocking(move || -> Result<_, error::Error> {
        let (pem, origin) = from_env_value(&value)?;
        let certificates = parse_pem(&pem, &origin)?.len();
        // Validated first, decoded second: only this phase needs text (a TOML
        // string is UTF-8 by definition), so a bundle every client already
        // trusts is refused here alone, and as data — the file's bytes are
        // what is wrong.
        let pem = String::from_utf8(pem).map_err(|error| error::Error::ExtraCaCertsNotUtf8 {
            bytes: error.as_bytes().len(),
        })?;
        Ok((pem, certificates))
    })
    .await
    .map_err(|join| io_error(std::io::Error::other(join.to_string())))??;

    let outcome = edit::edit(locks_root, &config_path, dry_run, move |document| {
        // The diff gate compares the PARSED string, so a CRLF bundle whose
        // `\r`s render escaped still counts as the same value on the next run.
        let already_persisted = document.get(PEM_KEY).and_then(toml_edit::Item::as_str) == Some(pem.as_str())
            && !document.contains_key(PATH_KEY);
        if already_persisted {
            return Ok(());
        }
        // Root-level values render ahead of every `[table]` regardless of
        // insertion order, so the key never lands inside a `[managed]` fence;
        // the XOR removal keeps the file loadable (the loader refuses both
        // keys).
        document.remove(PATH_KEY);
        document[PEM_KEY] = toml_edit::value(pem);
        Ok(())
    })
    .await
    .map_err(|error| match error {
        // Re-homed for its remedy (shrink the file, or use the path form);
        // every other refusal reads best in the edit's own words.
        EditError::TooLarge { path, bytes } => error::Error::RenderedConfigTooLarge { path, bytes },
        other => error::Error::ConfigEdit(other),
    })?;

    Ok(match (outcome, dry_run) {
        (EditOutcome::Unchanged, _) => ExtraCaCertsOutcome::Unchanged { certificates },
        (EditOutcome::Written, true) => ExtraCaCertsOutcome::WouldPersist { certificates },
        (EditOutcome::Written, false) => ExtraCaCertsOutcome::Persisted { certificates },
    })
}

/// The two directories `ocx self setup` registers at session level, **in the
/// order they must appear on PATH** (C-060).
///
/// `$OCX_HOME/toolchain/active/bin` leads and
/// [`FileStructure::ocx_install_bin_path`](ocx_store::file_structure::FileStructure::ocx_install_bin_path)
/// follows, so a
/// global toolchain that pins `ocx` is the one a session resolves and the
/// installed binary is the floor beneath it. D-4 removed the refusal that used
/// to stop a toolchain rendering that name; an ordering that kept the installed
/// binary in front would have left the pin rendered and permanently unreachable.
/// Every session-PATH writer takes this slice verbatim and prepends it in the
/// order given, so the order decided here is the order that reaches the registry
/// value, the `environment.d` line and the LaunchAgent script alike.
///
/// A named function rather than a `vec![]` inline in [`run`] because the order
/// is a decision with a contract number attached, and an ordering nobody can
/// call is an ordering no test can pin.
/// The toolchain half is [`ocx_store::file_structure::ToolchainStore::bin`], never
/// a literal `toolchain/active/bin` join: C-001 builds that store once from the
/// composite root precisely so no caller re-derives the tree location, and a
/// second spelling here is what would keep pointing at the old place the day
/// the layout moves.
pub fn session_path_directories(file_structure: &FileStructure) -> Vec<PathBuf> {
    vec![file_structure.toolchain.bin(), file_structure.ocx_install_bin_path()]
}

/// The session-PATH segment `<root>/bin`, which no longer exists (C-084).
///
/// The trampoline directory sat directly under the toolchain root until the
/// `active` indirection moved it one level down, so a machine that ran `ocx
/// self setup` against an unreleased build carries a session-PATH entry naming
/// a directory that will never be written again. [`run`] subtracts it before it
/// registers [`session_path_directories`].
///
/// A **named constant**, not an accessor: there is deliberately no live
/// derivation for a path the tree no longer has, and inventing one would keep a
/// dead shape spellable. It is deleted in the release after the one that
/// introduces it — the batched-window shape, with its removal release named
/// when the window opens.
///
/// Scope: only machines that ran `ocx self setup` against an unreleased build.
/// Everywhere else this is an `Unchanged` nobody sees.
pub fn retired_session_path_directories(file_structure: &FileStructure) -> Vec<PathBuf> {
    /// The name the trampoline directory carried directly under the root.
    const RETIRED_TOOLCHAIN_BIN: &str = "bin";

    vec![file_structure.toolchain.root().join(RETIRED_TOOLCHAIN_BIN)]
}

/// Adopt (or clear) the managed-config tier from an already-resolved
/// managed-config value.
///
/// The single implementation behind both adoption entry points: `ocx self
/// setup` (phase 1.5 of [`run`]) and `ocx config setup` (config-only, no
/// bootstrap/shims/profiles). The caller owns the precedence (flag >
/// `OCX_MANAGED_CONFIG` > `[managed].source` seed) and passes the resolved
/// value.
///
/// `None` — nothing resolved — short-circuits to
/// [`ManagedConfigSetupOutcome::NotConfigured`] without touching the filesystem
/// or network.
///
/// `Some("")` clears: removes the `[managed]` fence and deletes the
/// snapshot directory (no ghost tier), warning if `OCX_MANAGED_CONFIG` is
/// still exported (it would re-activate the tier on the next command).
///
/// `Some(ref)` adopts: re-parses `ref` as an [`ocx_oci::Identifier`]
/// (CWE-74 defense — the fence body below is real TOML serialization, never
/// `format!` interpolation of the raw ref), then follows ADR "Setup ordering":
/// synchronous fetch+persist FIRST, fence written only on success. A dirty
/// fence (user-edited) is left untouched without `force`
/// ([`ManagedConfigSetupOutcome::Dirty`]). `dry_run` short-circuits to
/// [`ManagedConfigSetupOutcome::WouldAdopt`] before any write.
///
/// # Refresh on re-run
///
/// A fence already `Current` for the same rendered body does **not** skip the
/// fetch: setup reconciles the tier on every run, so a newer fleet config is
/// picked up by the natural provisioning entry point
/// ([`ManagedConfigSetupOutcome::Refreshed`]; unchanged content reports
/// [`ManagedConfigSetupOutcome::AlreadyAdopted`], now verified rather than
/// assumed). The fence itself is never rewritten — `rc_block::apply` returns
/// `None` for a `Current` block.
///
/// The refresh is **best-effort only when an identity-matching snapshot is
/// already on disk**: a failed fetch then warns, keeps that snapshot, and
/// returns [`ManagedConfigSetupOutcome::RefreshUnavailable`] with exit 0.
/// First adoption and self-heal (fence current but the snapshot is wiped or
/// belongs to another source) have nothing to fall back on and keep the
/// hard-fail fetch-first ADR contract. A refresh is skipped entirely — without
/// a warning — for a digest-pinned seed, with no network, or under an in-force
/// `ocx config update --pause` (see [`refresh_skip_reason`]); `dry_run`
/// reports [`ManagedConfigSetupOutcome::WouldRefresh`] and never fetches.
///
/// # Errors
///
/// Returns [`error::Error`] when the ref does not parse as an OCI identifier,
/// the fetch+persist of a not-yet-adopted seed fails (no partial state — the
/// fence is not written), or a filesystem write fails. A system-locked tier
/// (the merged `config`'s `[managed] required = true`) rejects an explicit
/// clear or redirect with [`error::Error::ManagedConfigLocked`] (exit 78)
/// before any write, so a direct library caller cannot bypass the lock.
pub async fn apply_managed_config(
    config: &ocx_config::Config,
    managed_config: Option<&str>,
    dry_run: bool,
    force: bool,
    manager: &PackageManager,
    file_structure: &FileStructure,
) -> Result<ManagedConfigSetupOutcome, error::Error> {
    use crate::rc_block;
    use ocx_config::managed::{ManagedConfig, check_locked_managed_override};
    use ocx_package_manager::ManagedConfigUpdateResult;

    let Some(flag_value) = managed_config else {
        return Ok(ManagedConfigSetupOutcome::NotConfigured);
    };

    // Defense in depth: a system-locked tier may only be re-adopted with a
    // matching ref — never cleared (`""`) or redirected to a different source.
    // The CLI seam (`resolve_managed_config_arg`) enforces this before calling
    // in, but the public library function re-checks against the merged config
    // (which carries the system-tier `[managed] required = true` lock) so a
    // direct caller cannot bypass the lock and corrupt the required tier.
    check_locked_managed_override(config, flag_value)?;

    let config_path = file_structure.root().join("config.toml");
    // The decision read: which state the fence is in decides the outcome and
    // whether a fetch runs at all. The write below re-reads under the edit
    // lock, so an edit that lands during the fetch is not overwritten.
    let content = read_to_string_or_empty(&config_path).await?;

    if flag_value.is_empty() {
        if dry_run {
            return Ok(ManagedConfigSetupOutcome::WouldAdopt);
        }
        return clear_managed_config(&config_path, file_structure).await;
    }

    let identifier =
        ocx_oci::Identifier::parse_with_default_registry(flag_value, ocx_oci::DEFAULT_REGISTRY).map_err(|source| {
            error::Error::InvalidManagedConfigSource {
                value: flag_value.to_string(),
                source,
            }
        })?;

    let managed = ManagedConfig {
        source: Some(identifier.to_string()),
        required: Some(ManagedConfig::DEFAULT_REQUIRED),
        refresh: Some(ManagedConfig::DEFAULT_REFRESH),
        interval: Some(ManagedConfig::DEFAULT_INTERVAL.to_string()),
        system_locked: false,
    };
    // Real TOML serialization of the typed struct — never `format!`
    // interpolation of `flag_value` (Block-tier CWE-74 fix, ADR Decision C).
    let body = format!(
        "[managed]\n{}",
        toml::to_string(&managed).expect("ManagedConfig has no float/map keys and always serializes")
    );

    let state = rc_block::classify(&content, &body, rc_block::MANAGED_LABEL);
    if state == rc_block::BlockState::Dirty && !force {
        return Ok(ManagedConfigSetupOutcome::Dirty);
    }
    // The identity-matching snapshot already on disk, if any. Its presence is
    // the ONLY licence for the best-effort refresh arm below: a failed fetch
    // can fall back to it. First adopt and self-heal leave this `None`, so they
    // keep the hard-fail fetch-first contract and a `required = true` fence can
    // never be written with no snapshot behind it.
    let mut adopted: Option<ocx_config::managed::ManagedConfigSnapshot> = None;
    if state == rc_block::BlockState::Current {
        // W3: a `Current` fence alone does not prove the tier is healthy — the
        // snapshot may have been wiped or belong to a different source (e.g. a
        // restored $OCX_HOME). Only a present, identity-matching snapshot
        // counts as adopted; otherwise fall through to the fetch+persist below
        // to self-heal (the fence itself is never rewritten — `rc_block::apply`
        // returns `None` for a `Current` block).
        let snapshot =
            ocx_config::managed_config::read_managed_config_snapshot(&file_structure.state.managed_config()).await;
        match snapshot {
            Some(snapshot) if ocx_config::managed::snapshot_matches_source(&snapshot, &identifier) => {
                adopted = Some(snapshot);
            }
            _ => {
                log::info!("managed-config fence is current but the snapshot is absent or mismatched; re-syncing");
            }
        }
    }

    if let Some(snapshot) = &adopted {
        // A deliberate skip is not a failure: report the existing snapshot as
        // adopted and stay silent on stderr (no warn noise for `--offline`, a
        // digest-pinned seed, or an in-force pause).
        let paused = ocx_config::managed_config::read_pause(&file_structure.state.managed_config())
            .await
            .is_some();
        if let Some(reason) = refresh_skip_reason(&identifier, manager.can_fetch_managed_config(), paused) {
            log::debug!("managed-config refresh skipped ({reason})");
            return Ok(ManagedConfigSetupOutcome::AlreadyAdopted {
                digest: snapshot.digest.clone(),
            });
        }
        if dry_run {
            // Dry-run never fetches — `ocx config update --check` is the probe
            // surface. Report that a refresh would run, nothing more.
            return Ok(ManagedConfigSetupOutcome::WouldRefresh {
                digest: snapshot.digest.clone(),
            });
        }
    }

    if dry_run {
        return Ok(ManagedConfigSetupOutcome::WouldAdopt);
    }

    // Synchronous fetch+persist FIRST (ADR "Setup ordering"): a transient
    // network blip during onboarding must not leave a `required = true` fence
    // with no snapshot, which would brick every subsequent command.
    let resolved = ocx_config::managed::ResolvedManagedConfig {
        source: identifier,
        required: ManagedConfig::DEFAULT_REQUIRED,
        refresh: ManagedConfig::DEFAULT_REFRESH,
        interval: ocx_config::managed::parse_interval(ManagedConfig::DEFAULT_INTERVAL)
            .expect("DEFAULT_INTERVAL is always a valid interval"),
        system_required: false,
    };
    // An absent-in-registry source (`Ok(None)` from the fetch) surfaces as
    // `Err(ManagedConfigUpdateError::SourceNotFound)`, propagated through
    // `Error::ManagedConfigUpdateFailed` — no fence is written, no partial
    // state (ADR "Setup ordering"). A successful update always carries the
    // persisted digest.
    let result = match manager.update_managed_config(&resolved, None).await {
        Ok(result) => result,
        Err(error) => {
            // Best-effort ONLY behind an identity-matching snapshot, and ONLY
            // for errors proven to fire before any snapshot write: a registry
            // blip or a bad published payload (invalid TOML, a CA bundle this
            // host cannot load) must not fail a re-run, because the tier
            // stays usable on the content already on disk. A
            // `SnapshotWriteFailed` can fire AFTER the payload rename (the
            // metadata write is a separate atomic rename), leaving the new
            // payload live under the old provenance — reporting "kept the
            // existing snapshot" there would be false, so it propagates.
            let pre_write_failure = matches!(
                &error,
                ocx_config::managed_config::ManagedConfigUpdateError::Fetch(_)
                    | ocx_config::managed_config::ManagedConfigUpdateError::SourceNotFound { .. }
                    | ocx_config::managed_config::ManagedConfigUpdateError::Persist(
                        ocx_config::managed_config::ManagedConfigPersistError::InvalidToml { .. }
                            | ocx_config::managed_config::ManagedConfigPersistError::ExtraCaCertsInvalid { .. }
                    )
            );
            let Some(previous) = adopted.filter(|_| pre_write_failure) else {
                return Err(error.into());
            };
            // The error never reaches `main`, so nothing walks its `source()`
            // chain for us — and the dominant variant (`Fetch`) interpolates
            // nothing, so its bare `Display` would name no cause at all.
            let reason = ocx_util::error::render_chain(&error);
            log::warn!(
                "could not refresh the managed-config snapshot from '{}': {reason}; keeping the existing snapshot \
                 (run `ocx config update` to retry)",
                resolved.source
            );
            return Ok(ManagedConfigSetupOutcome::RefreshUnavailable {
                digest: previous.digest,
                reason,
            });
        }
    };

    // Fence written only after the fetch+persist above succeeded — through
    // the one `config.toml` edit lock (ocx#468), never across the fetch: the
    // state machine re-runs on the text as it is now, so a `[shell]` or
    // extra-CA edit that landed meanwhile is carried, not overwritten. The
    // dry-run gates above already returned, so this is never a dry run.
    ocx_config::edit::edit_text(&file_structure.locks, &config_path, false, move |current| {
        // `rc_block::apply` is infallible today and documents its `Result`
        // as a signature reservation; a refusal, should one arrive, is the
        // file's shape and lands as `Malformed` (74) with the file untouched.
        let rewritten = rc_block::apply(current, &body, force, rc_block::MANAGED_LABEL)
            .map_err(|_| "the [managed] fence could not be rewritten")?;
        Ok(rewritten.unwrap_or_else(|| current.to_owned()))
    })
    .await?;

    Ok(match (adopted, result) {
        (Some(previous), ManagedConfigUpdateResult::Updated { digest }) => ManagedConfigSetupOutcome::Refreshed {
            from: previous.digest,
            to: digest,
        },
        (Some(_), ManagedConfigUpdateResult::AlreadyCurrent { digest }) => {
            ManagedConfigSetupOutcome::AlreadyAdopted { digest }
        }
        (
            None,
            ManagedConfigUpdateResult::Updated { digest } | ManagedConfigUpdateResult::AlreadyCurrent { digest },
        ) => ManagedConfigSetupOutcome::Adopted { digest },
    })
}

/// Why a refresh of an **already-adopted** managed-config seed is skipped, or
/// `None` when it must run.
///
/// Pure decision, no I/O — the whole matrix is unit-testable without a
/// registry. Precedence is deliberate: a digest-pinned seed is content-
/// addressed and cannot drift, so it reports first; a missing client (offline)
/// outranks a pause because no fetch could happen either way.
///
/// The returned string is a diagnostic label, not a user-facing message — each
/// case reports [`ManagedConfigSetupOutcome::AlreadyAdopted`] and logs at
/// debug. `OCX_NO_CONFIG_REFRESH` is deliberately absent: that kill switch
/// gates the background tick only, and `--offline` is the no-network lever for
/// setup. `--frozen` is absent for a different reason: it scopes to the
/// package tier, so the managed tier behaves identically with and without it.
fn refresh_skip_reason(identifier: &ocx_oci::Identifier, can_fetch: bool, paused: bool) -> Option<&'static str> {
    if identifier.digest().is_some() {
        return Some("digest-pinned");
    }
    if !can_fetch {
        return Some("cannot-fetch");
    }
    if paused {
        return Some("paused");
    }
    None
}

/// Clears the `--managed-config` tier: removes the `[managed]` fence from
/// `config.toml` (if present) and deletes the snapshot directory entirely —
/// no ghost tier survives a clear. Warns if `OCX_MANAGED_CONFIG` is still
/// exported, since the env override would re-activate the tier on the very
/// next command.
async fn clear_managed_config(
    config_path: &Path,
    file_structure: &FileStructure,
) -> Result<ManagedConfigSetupOutcome, error::Error> {
    // Under the one `config.toml` edit lock (ocx#468); an unchanged strip
    // writes nothing.
    ocx_config::edit::edit_text(&file_structure.locks, config_path, false, |content| {
        Ok(crate::rc_block::remove_block(content, crate::rc_block::MANAGED_LABEL))
    })
    .await?;

    let managed_dir = file_structure.state.managed_config().dir();
    if ocx_util::fs::path_exists_lossy(&managed_dir).await {
        tokio::fs::remove_dir_all(&managed_dir)
            .await
            .map_err(|source| error::Error::Io {
                path: managed_dir,
                source,
            })?;
    }

    if ocx_util::env::var(ocx_config::env::keys::OCX_MANAGED_CONFIG).is_some_and(|value| !value.is_empty()) {
        log::warn!(
            "OCX_MANAGED_CONFIG is still exported; it will re-activate the managed-config tier \
             on the next command unless unset"
        );
    }

    Ok(ManagedConfigSetupOutcome::Cleared)
}

/// Non-fatal advisory printed when the current-user execution policy is
/// `Restricted` (a `$PROFILE` fence is inert until the user relaxes it). OCX
/// never auto-changes the policy — that is a user security decision.
const EXEC_POLICY_ADVISORY: &str =
    "run `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned` to allow the profile to load";

/// Resolve the profile files this run should target, in write order.
///
/// `Some(overrides)` skips auto-detection entirely — including `Some(&[])`,
/// which resolves to **no targets at all** (`--no-profile`). Explicit overrides
/// are treated as POSIX-fence targets (contract 1 edge). `None` auto-detects the
/// POSIX/dedicated-file set from the real environment and probes the PowerShell
/// `$PROFILE` via a subprocess.
async fn resolve_targets(ocx_home: &Path, overrides: Option<&[PathBuf]>) -> Vec<ProfileTarget> {
    // The probe is the one part of resolution that leaves the process, so it
    // runs here and the composition below stays a pure function of what it
    // answered. An override list short-circuits before it: a caller that named
    // its profiles is not asking which shells are installed.
    if overrides.is_some() {
        return compose_targets(ocx_home, overrides, None);
    }
    compose_targets(ocx_home, None, profiles::detect_powershell_profile().await)
}

/// [`resolve_targets`] with the PowerShell probe's answer already in hand.
///
/// Split out so the composition is testable without spawning a PowerShell
/// host: the unit test drove a real `pwsh` on any machine that has one, which
/// cost a second of the suite's wall clock and made what it asserted depend on
/// what was installed. `powershell` is the probe's result, `None` when no host
/// answered.
fn compose_targets(ocx_home: &Path, overrides: Option<&[PathBuf]>, powershell: Option<PathBuf>) -> Vec<ProfileTarget> {
    if let Some(overrides) = overrides {
        return overrides
            .iter()
            .map(|path| ProfileTarget {
                path: path.clone(),
                kind: ProfileKind::PosixFence,
            })
            .collect();
    }

    let mut targets = profiles::detect_targets(&home_env_from_environment(ocx_home));
    if let Some(profile) = powershell {
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

/// Phase 3 of [`run`]: resolve the profile targets and apply the managed block
/// to each, in write order.
///
/// The two profile opt-outs stay separate here. `--no-modify-path` skips the
/// phase outright (and, via [`SetupOptions::touches_session_path`], the
/// session-PATH arm with it); an empty `profiles` list resolves to zero targets
/// and stops there, leaving every other phase running.
///
/// `handoff` is passed straight through as `apply_target`'s `heal_only`: under
/// a hand-off no profile gains a block it did not already have, while a drifted
/// block still heals and a dirty one still reports
/// [`ProfileOutcome::SkippedDirty`] (exit 82, which the parent translates).
///
/// # Errors
///
/// Returns [`error::Error`] if a profile read or write fails.
async fn apply_profile_phase(
    ocx_home: &Path,
    options: &SetupOptions,
) -> Result<Vec<(PathBuf, ProfileOutcome)>, error::Error> {
    let targets = if options.no_modify_path {
        Vec::new()
    } else {
        resolve_targets(ocx_home, options.profiles.as_deref()).await
    };

    let mut profiles = Vec::with_capacity(targets.len());
    for target in targets {
        let outcome = apply_target(&target, options.force, options.handoff, options.dry_run).await?;
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
    let state = rc_block::classify(&content, body, rc_block::OCX_LABEL);

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
        if let Some(new_content) = rc_block::apply(&stripped, body, force, rc_block::OCX_LABEL)? {
            if !dry_run {
                write_profile(path, &new_content).await?;
            }
            return Ok(ProfileOutcome::Migrated);
        }
        // `strip_block` produced a Fresh file, so `apply` always returns Some;
        // this arm is unreachable, but reported as a no-op for totality.
        return Ok(ProfileOutcome::NoOp);
    }

    match rc_block::apply(&content, body, force, rc_block::OCX_LABEL)? {
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
    if heal_only && !ocx_util::fs::path_exists_lossy(path).await {
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

/// Blocking body of [`write_profile`]: create the parent dir, then write the
/// content atomically via [`ocx_util::fs::write_bytes_atomic`] (private
/// temp file in the parent, published over `path`).
fn write_profile_blocking(path: &Path, content: &str) -> Result<(), error::Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| error::Error::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    ocx_util::fs::write_bytes_atomic(path, content.as_bytes()).map_err(|source| error::Error::Io {
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
    let shim_bin_dir = file_structure.ocx_install_bin_path();

    let path_var = std::env::var_os("PATH")?;
    let executable = if cfg!(windows) { "ocx.exe" } else { "ocx" };

    for dir in std::env::split_paths(&path_var) {
        // Reaching the shim's own bin dir first means nothing shadows it.
        if dir == shim_bin_dir {
            return None;
        }
        let candidate = dir.join(executable);
        if ocx_util::fs::path_exists_lossy(&candidate).await {
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

    // ── C-060: the session-PATH directory order ──────────────────────────────

    /// C-060: `$OCX_HOME/toolchain/active/bin` leads, the install bin directory
    /// follows, and there are exactly those two.
    ///
    /// This is the **one** site that decides the order — the writers prepend
    /// the slice verbatim, so a swap here silently reorders the registry value,
    /// the `environment.d` line and the LaunchAgent script together. The
    /// per-writer tests assert their own file carries the slice in the order it
    /// was handed; none of them can see that the order handed over is wrong,
    /// which is why this assertion lives beside the decision rather than
    /// inside a writer.
    ///
    /// Asserted by index and by identity against
    /// [`FileStructure::ocx_install_bin_path`] — not by a spelled-out literal, which on
    /// Windows would drive-join and stop matching (`quality-rust.md`,
    /// "Cross-Platform Path Handling").
    #[test]
    fn the_session_path_directories_put_the_toolchain_bin_dir_ahead_of_the_install_one() {
        let home = tempfile::TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        let directories = session_path_directories(&file_structure);

        assert_eq!(
            directories.len(),
            2,
            "exactly two directories are registered: {directories:?}"
        );
        assert_eq!(
            directories[0],
            home.path().join("toolchain").join("active").join("bin"),
            "the PATH-facing trampoline directory must lead, so a pinned `ocx` can win"
        );
        assert_eq!(
            directories[1],
            file_structure.ocx_install_bin_path(),
            "the installed ocx's own bin directory is the floor beneath it"
        );
        assert_ne!(
            directories[0], directories[1],
            "the two entries must be distinct, or the ordering assertion above is vacuous"
        );
    }

    /// C-084 — the retired entry is the *old* spelling, and it is not one of
    /// the two the same run registers.
    ///
    /// Both halves matter. A retired set that answered `active/bin` would
    /// subtract the entry the very next call adds — on Windows and macOS the
    /// merge happens after, so the value would survive, but on a `--dry-run`
    /// the reported outcome would be a lie, and any reordering would leave the
    /// user with no toolchain on `PATH` at all.
    ///
    /// RED: derive it from `toolchain.bin()` and the disjointness assertion
    /// fails; spell it `<root>/active/bin` and the literal fails.
    #[test]
    fn the_retired_session_path_entry_is_the_old_root_level_bin_directory() {
        let home = tempfile::TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        let retired = retired_session_path_directories(&file_structure);
        let registered = session_path_directories(&file_structure);

        assert_eq!(
            retired,
            vec![home.path().join("toolchain").join("bin")],
            "C-084 retires exactly `<root>/bin`, the directory the tree used to expose"
        );
        assert!(
            retired.iter().all(|entry| !registered.contains(entry)),
            "the retired entry must not be one this run registers: {retired:?} vs {registered:?}"
        );
    }

    // ── W3: `--managed-config <ref>` Current-fence snapshot gate ─────────────

    /// Builds a manager whose managed-config client serves the v2 package
    /// shape for `identifier` (stub transport, no network).
    fn manager_with_stub(root: &Path, identifier: &ocx_oci::Identifier, config_toml: &str) -> PackageManager {
        let (client, _) = ocx_config::managed_config::test_support::stub_client_with_package(identifier, config_toml);
        let fs = FileStructure::with_root(root.to_path_buf());
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(&fs),
        });
        let index = ocx_index::Index::from_chained(local_index, vec![], ocx_index::ChainMode::Offline);
        PackageManager::new(fs, index, None, "localhost:5000").with_managed_config_client(Some(client))
    }

    /// W3 matrix — `Current` fence + missing snapshot: self-heals by
    /// re-fetching (outcome `Adopted`), fence untouched.
    #[tokio::test]
    async fn apply_managed_config_current_fence_missing_snapshot_self_heals() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = ocx_oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let manager = manager_with_stub(home.path(), &identifier, "[registry]\ndefault = \"healed\"\n");

        // First adopt writes fence + snapshot.
        let first = apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            false,
            false,
            &manager,
            &file_structure,
        )
        .await
        .expect("first adopt must succeed");
        assert!(matches!(first, ManagedConfigSetupOutcome::Adopted { .. }));
        let fence_before = read(&home.path().join("config.toml"));

        // Wipe the snapshot dir (restored $OCX_HOME scenario).
        std::fs::remove_dir_all(file_structure.state.managed_config().dir()).unwrap();

        let healed = apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            false,
            false,
            &manager,
            &file_structure,
        )
        .await
        .expect("re-run with a wiped snapshot must self-heal");
        assert!(
            matches!(healed, ManagedConfigSetupOutcome::Adopted { .. }),
            "a Current fence with a missing snapshot must re-fetch, got {healed:?}"
        );
        assert!(
            file_structure.state.managed_config().snapshot_file().exists(),
            "the snapshot must be re-persisted"
        );
        assert_eq!(
            read(&home.path().join("config.toml")),
            fence_before,
            "the fence itself is never rewritten by the self-heal"
        );
    }

    /// A system-locked tier rejects an explicit redirect or clear at the
    /// library boundary — a direct `apply_managed_config` caller cannot bypass
    /// the lock the CLI seam also enforces (swarm-review W2). The check fires
    /// before any store or filesystem access, so no fence is written.
    #[tokio::test]
    async fn apply_managed_config_rejects_locked_tier_override() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let locked_ref = "corp.example.com/ocx-config:user";
        let manager = manager_with_stub(
            home.path(),
            &ocx_oci::Identifier::parse(locked_ref).unwrap(),
            "[registry]\ndefault = \"x\"\n",
        );
        let locked = ocx_config::Config {
            managed: Some(ocx_config::managed::ManagedConfig {
                source: Some(locked_ref.to_string()),
                system_locked: true,
                ..Default::default()
            }),
            ..Default::default()
        };

        // Redirect to a different source → rejected.
        let redirect = apply_managed_config(
            &locked,
            Some("corp.example.com/evil:v9"),
            false,
            false,
            &manager,
            &file_structure,
        )
        .await;
        assert!(
            matches!(redirect, Err(error::Error::ManagedConfigLocked(_))),
            "a locked tier must reject a redirect, got {redirect:?}"
        );

        // Clearing a locked tier → rejected.
        let clear = apply_managed_config(&locked, Some(""), false, false, &manager, &file_structure).await;
        assert!(
            matches!(clear, Err(error::Error::ManagedConfigLocked(_))),
            "a locked tier must reject a clear, got {clear:?}"
        );

        // Neither rejected call touched config.toml.
        assert!(
            !home.path().join("config.toml").exists(),
            "a rejected locked-tier override must not write the fence"
        );
    }

    /// W3 matrix — `Current` fence + cross-repository snapshot: gate treats it
    /// as absent, self-heals by re-fetching.
    #[tokio::test]
    async fn apply_managed_config_current_fence_mismatched_snapshot_self_heals() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = ocx_oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let manager = manager_with_stub(home.path(), &identifier, "[registry]\ndefault = \"healed\"\n");

        apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            false,
            false,
            &manager,
            &file_structure,
        )
        .await
        .expect("first adopt must succeed");

        // Overwrite the snapshot with one recorded under a different repo
        // (metadata + payload sibling, so the gate sees a present-but-mismatched
        // snapshot rather than an absent one).
        let poisoned = serde_json::json!({
            "source": "other.example.com/poisoned-config:user",
            "digest": format!("sha256:{}", "d".repeat(64)),
            "fetched_at": "old",
        });
        std::fs::write(
            file_structure.state.managed_config().snapshot_file(),
            serde_json::to_vec(&poisoned).unwrap(),
        )
        .unwrap();
        std::fs::write(
            file_structure.state.managed_config().toml_file(),
            "[registry]\ndefault = \"poisoned\"\n",
        )
        .unwrap();

        let healed = apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            false,
            false,
            &manager,
            &file_structure,
        )
        .await
        .expect("re-run with a mismatched snapshot must self-heal");
        assert!(
            matches!(healed, ManagedConfigSetupOutcome::Adopted { .. }),
            "a Current fence with a cross-repo snapshot must re-fetch, got {healed:?}"
        );
        let snapshot = ocx_config::managed_config::read_managed_config_snapshot(&file_structure.state.managed_config())
            .await
            .expect("snapshot must exist after heal");
        assert_eq!(
            snapshot.source, reference,
            "the healed snapshot belongs to the seed source"
        );
    }

    // ── setup refresh: a Current fence + matching snapshot re-syncs ──────────

    /// Adopt `reference` against a stub serving `payload`, asserting the first
    /// run wrote the fence and the snapshot. Returns the persisted digest.
    async fn adopt(home: &Path, file_structure: &FileStructure, reference: &str, payload: &str) -> ocx_oci::Digest {
        let identifier = ocx_oci::Identifier::parse(reference).unwrap();
        let manager = manager_with_stub(home, &identifier, payload);
        let outcome = apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            false,
            false,
            &manager,
            file_structure,
        )
        .await
        .expect("first adopt must succeed");
        match outcome {
            ManagedConfigSetupOutcome::Adopted { digest } => digest,
            other => panic!("expected Adopted, got {other:?}"),
        }
    }

    /// A manager whose managed-config client serves a DIFFERENT repository, so
    /// every fetch for the seed under test resolves to `SourceNotFound` — the
    /// unit-test stand-in for an unreachable registry.
    fn manager_with_failing_fetch(root: &Path) -> PackageManager {
        let elsewhere = ocx_oci::Identifier::parse("other.example.com/unrelated-config:v1").unwrap();
        manager_with_stub(root, &elsewhere, "[registry]\ndefault = \"unrelated\"\n")
    }

    /// The `Current` fence + matching snapshot path RE-SYNCS: setup reconciles
    /// the managed tier on every run, so a newer payload published under the
    /// same tag is picked up here (`Refreshed`, from != to) and the snapshot on
    /// disk carries the new content. Restoring the old early return reds this.
    #[tokio::test]
    async fn apply_managed_config_current_fence_matching_snapshot_refreshes() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = ocx_oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        let first_digest = adopt(
            home.path(),
            &file_structure,
            reference,
            "[registry]\ndefault = \"adopted\"\n",
        )
        .await;
        let fence_before = read(&home.path().join("config.toml"));

        // The operator republishes the same tag with new content.
        let republished = manager_with_stub(home.path(), &identifier, "[registry]\ndefault = \"republished\"\n");
        let second = apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            false,
            false,
            &republished,
            &file_structure,
        )
        .await
        .expect("a re-run against newer content must succeed");

        match second {
            ManagedConfigSetupOutcome::Refreshed { from, to } => {
                assert_eq!(from, first_digest, "`from` is the digest the snapshot carried");
                assert_ne!(from, to, "a refresh to newer content must change the digest");
            }
            other => panic!("expected Refreshed, got {other:?}"),
        }

        let snapshot = ocx_config::managed_config::read_managed_config_snapshot(&file_structure.state.managed_config())
            .await
            .expect("the refreshed snapshot must be readable");
        assert!(
            snapshot.config.contains("republished"),
            "the persisted payload must be the newly published one, got {:?}",
            snapshot.config
        );
        assert_eq!(
            read(&home.path().join("config.toml")),
            fence_before,
            "the fence itself is never rewritten by a refresh"
        );
    }

    /// A re-run whose registry content is unchanged persists nothing and
    /// reports `AlreadyAdopted` with the same verified digest — now proven by
    /// a fetch rather than assumed from the fence.
    #[tokio::test]
    async fn apply_managed_config_rerun_same_content_stays_already_adopted() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = ocx_oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let payload = "[registry]\ndefault = \"adopted\"\n";

        let first_digest = adopt(home.path(), &file_structure, reference, payload).await;
        let fetched_at_before =
            ocx_config::managed_config::read_managed_config_snapshot(&file_structure.state.managed_config())
                .await
                .expect("snapshot must exist after adopt")
                .fetched_at;

        let unchanged = manager_with_stub(home.path(), &identifier, payload);
        let second = apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            false,
            false,
            &unchanged,
            &file_structure,
        )
        .await
        .expect("re-run must succeed");
        match second {
            ManagedConfigSetupOutcome::AlreadyAdopted { digest } => {
                assert_eq!(digest, first_digest, "AlreadyAdopted carries the verified digest");
            }
            other => panic!("expected AlreadyAdopted, got {other:?}"),
        }

        assert_eq!(
            ocx_config::managed_config::read_managed_config_snapshot(&file_structure.state.managed_config())
                .await
                .expect("snapshot must survive")
                .fetched_at,
            fetched_at_before,
            "unchanged content must not re-persist the snapshot"
        );
    }

    /// A failed refresh BEHIND an identity-matching snapshot is best-effort:
    /// the snapshot is kept at its original digest, the fence is untouched, and
    /// the run succeeds with `RefreshUnavailable` (never `AlreadyAdopted` — a
    /// refresh that did not run must not look healthy).
    #[tokio::test]
    async fn apply_managed_config_refresh_failure_keeps_snapshot() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        let first_digest = adopt(
            home.path(),
            &file_structure,
            reference,
            "[registry]\ndefault = \"adopted\"\n",
        )
        .await;

        let broken = manager_with_failing_fetch(home.path());
        let second = apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            false,
            false,
            &broken,
            &file_structure,
        )
        .await
        .expect("a failed refresh behind a matching snapshot must not fail the run");

        match second {
            ManagedConfigSetupOutcome::RefreshUnavailable { digest, reason } => {
                assert_eq!(digest, first_digest, "the retained snapshot's digest is reported");
                assert!(!reason.is_empty(), "the cause must be carried, got {reason:?}");
            }
            other => panic!("expected RefreshUnavailable, got {other:?}"),
        }

        let snapshot = ocx_config::managed_config::read_managed_config_snapshot(&file_structure.state.managed_config())
            .await
            .expect("the snapshot must survive a failed refresh");
        assert_eq!(snapshot.digest, first_digest, "the snapshot is kept, not replaced");
        assert!(snapshot.config.contains("adopted"), "the payload is kept verbatim");
    }

    /// A published payload that fails validation (`Persist(InvalidToml)`, or
    /// `Persist(ExtraCaCertsInvalid)` — a CA bundle this host cannot load,
    /// DX-20) is a pre-write fault — nothing on disk has moved — so behind a
    /// matching snapshot it stays best-effort, exactly like a fetch fault.
    #[tokio::test]
    async fn apply_managed_config_refresh_invalid_payload_stays_best_effort() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let unusable_pem = format!(
            "extra_ca_certs_pem = '''\n{}'''\n",
            ocx_test_support::pki::pem_block("CERTIFICATE", b"this is not DER at all")
        );
        for payload in ["not = [valid toml", unusable_pem.as_str()] {
            let home = tempfile::TempDir::new().unwrap();
            let reference = "corp.example.com/ocx-config:user";
            let identifier = ocx_oci::Identifier::parse(reference).unwrap();
            let file_structure = FileStructure::with_root(home.path().to_path_buf());

            let first_digest = adopt(
                home.path(),
                &file_structure,
                reference,
                "[registry]\ndefault = \"adopted\"\n",
            )
            .await;

            let invalid = manager_with_stub(home.path(), &identifier, payload);
            let second = apply_managed_config(
                &ocx_config::Config::default(),
                Some(reference),
                false,
                false,
                &invalid,
                &file_structure,
            )
            .await
            .expect("an invalid published payload must not fail a re-run behind a matching snapshot");
            assert!(
                matches!(second, ManagedConfigSetupOutcome::RefreshUnavailable { .. }),
                "expected RefreshUnavailable for {payload:?}, got {second:?}"
            );
            let snapshot =
                ocx_config::managed_config::read_managed_config_snapshot(&file_structure.state.managed_config())
                    .await
                    .expect("the snapshot must survive");
            assert_eq!(snapshot.digest, first_digest, "the snapshot is kept, not replaced");
        }
    }

    /// A snapshot-WRITE failure is never downgraded to `RefreshUnavailable`:
    /// the metadata write is a separate atomic rename after the payload
    /// rename, so a failure there can strand the new payload under the old
    /// provenance — claiming "kept the existing snapshot" would be false. The
    /// error propagates even behind a matching snapshot.
    #[cfg(unix)]
    #[tokio::test]
    async fn apply_managed_config_refresh_snapshot_write_failure_propagates() {
        use std::os::unix::fs::PermissionsExt as _;

        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = ocx_oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        let first_digest = adopt(
            home.path(),
            &file_structure,
            reference,
            "[registry]\ndefault = \"adopted\"\n",
        )
        .await;

        // Newer content forces a persist; a read-only snapshot dir makes the
        // write fail deterministically before anything moves.
        let newer = manager_with_stub(home.path(), &identifier, "[registry]\ndefault = \"newer\"\n");
        let dir = file_structure.state.managed_config().dir();
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&dir, perms).unwrap();

        let result = apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            false,
            false,
            &newer,
            &file_structure,
        )
        .await;

        let mut restore = std::fs::metadata(&dir).unwrap().permissions();
        restore.set_mode(0o755);
        std::fs::set_permissions(&dir, restore).unwrap();

        assert!(
            result.is_err(),
            "a snapshot-write failure must propagate, got {result:?}"
        );
        let snapshot = ocx_config::managed_config::read_managed_config_snapshot(&file_structure.state.managed_config())
            .await
            .expect("the original snapshot is still present");
        assert_eq!(snapshot.digest, first_digest, "the original snapshot is untouched");
    }

    /// ADR "Setup ordering" invariant, guarded against an over-broad
    /// best-effort arm: a FIRST adopt whose fetch fails still hard-fails, and
    /// no `[managed]` fence is written — a `required = true` fence must never
    /// exist with no snapshot behind it.
    #[tokio::test]
    async fn apply_managed_config_first_adopt_fetch_failure_hard_fails_without_fence() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let broken = manager_with_failing_fetch(home.path());

        let result = apply_managed_config(
            &ocx_config::Config::default(),
            Some("corp.example.com/ocx-config:user"),
            false,
            false,
            &broken,
            &file_structure,
        )
        .await;

        assert!(
            result.is_err(),
            "a first adopt with no snapshot to fall back on must hard-fail, got {result:?}"
        );
        assert!(
            !home.path().join("config.toml").exists(),
            "a failed first adopt must not write the [managed] fence"
        );
        assert!(
            !file_structure.state.managed_config().snapshot_file().exists(),
            "a failed first adopt must not leave a snapshot"
        );
    }

    /// Self-heal sibling of the invariant above: a `Current` fence whose
    /// snapshot was wiped has no fallback either, so a failing fetch propagates
    /// instead of reporting `RefreshUnavailable`.
    #[tokio::test]
    async fn apply_managed_config_self_heal_fetch_failure_hard_fails() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        adopt(
            home.path(),
            &file_structure,
            reference,
            "[registry]\ndefault = \"adopted\"\n",
        )
        .await;
        // Restored $OCX_HOME: fence current, snapshot gone.
        std::fs::remove_dir_all(file_structure.state.managed_config().dir()).unwrap();

        let broken = manager_with_failing_fetch(home.path());
        let result = apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            false,
            false,
            &broken,
            &file_structure,
        )
        .await;
        assert!(
            result.is_err(),
            "a self-heal with a wiped snapshot has no fallback and must hard-fail, got {result:?}"
        );
    }

    /// Offline (no managed-config client): the refresh is a deliberate skip,
    /// not a failure — `AlreadyAdopted`, never `RefreshUnavailable`, and no
    /// stderr warning.
    #[tokio::test]
    async fn apply_managed_config_offline_rerun_is_already_adopted() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = ocx_oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        let first_digest = adopt(
            home.path(),
            &file_structure,
            reference,
            "[registry]\ndefault = \"adopted\"\n",
        )
        .await;

        let offline = manager_with_stub(home.path(), &identifier, "[registry]\ndefault = \"newer\"\n")
            .with_managed_config_client(None);
        let second = apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            false,
            false,
            &offline,
            &file_structure,
        )
        .await
        .expect("an offline re-run must succeed");
        match second {
            ManagedConfigSetupOutcome::AlreadyAdopted { digest } => assert_eq!(digest, first_digest),
            other => panic!("expected AlreadyAdopted, got {other:?}"),
        }
    }

    /// A digest-pinned seed is content-addressed and cannot drift, so the
    /// refresh is skipped even with a client present — proved by using a client
    /// that could only fail the fetch.
    #[tokio::test]
    async fn apply_managed_config_digest_pinned_seed_skips_refresh() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let payload = "[registry]\ndefault = \"pinned\"\n";
        // The stub's index digest does not depend on the identifier, so it can
        // be computed first and then baked into the pinned reference.
        let (_, index_digest) = ocx_config::managed_config::test_support::stub_client_with_package(
            &ocx_oci::Identifier::parse("corp.example.com/ocx-config:user").unwrap(),
            payload,
        );
        let reference = format!("corp.example.com/ocx-config:user@{index_digest}");
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        let first_digest = adopt(home.path(), &file_structure, &reference, payload).await;

        let broken = manager_with_failing_fetch(home.path());
        let second = apply_managed_config(
            &ocx_config::Config::default(),
            Some(&reference),
            false,
            false,
            &broken,
            &file_structure,
        )
        .await
        .expect("a digest-pinned re-run must not fetch, so it cannot fail");
        match second {
            ManagedConfigSetupOutcome::AlreadyAdopted { digest } => assert_eq!(digest, first_digest),
            other => panic!("expected AlreadyAdopted, got {other:?}"),
        }
    }

    /// An in-force `ocx config update --pause` freezes the setup refresh too,
    /// and setup does NOT clear the pause (unlike `ocx config update`, setup is
    /// not an explicit fetch request).
    #[tokio::test]
    async fn apply_managed_config_pause_skips_refresh_and_keeps_pause() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = ocx_oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        let first_digest = adopt(
            home.path(),
            &file_structure,
            reference,
            "[registry]\ndefault = \"adopted\"\n",
        )
        .await;

        let pause =
            ocx_config::managed_config::ManagedConfigPause::for_duration(std::time::Duration::from_secs(3600), None);
        ocx_config::managed_config::write_pause(&file_structure.state.managed_config(), &pause)
            .await
            .unwrap();

        let republished = manager_with_stub(home.path(), &identifier, "[registry]\ndefault = \"republished\"\n");
        let second = apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            false,
            false,
            &republished,
            &file_structure,
        )
        .await
        .expect("a paused re-run must succeed");
        match second {
            ManagedConfigSetupOutcome::AlreadyAdopted { digest } => assert_eq!(digest, first_digest),
            other => panic!("expected AlreadyAdopted while paused, got {other:?}"),
        }
        assert!(
            ocx_config::managed_config::read_pause(&file_structure.state.managed_config())
                .await
                .is_some(),
            "setup must not clear the pause"
        );
    }

    /// `--dry-run` against an adopted seed never fetches: it reports
    /// `WouldRefresh` and leaves the snapshot exactly as it was.
    #[tokio::test]
    async fn apply_managed_config_dry_run_on_adopted_seed_would_refresh() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = ocx_oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        let first_digest = adopt(
            home.path(),
            &file_structure,
            reference,
            "[registry]\ndefault = \"adopted\"\n",
        )
        .await;

        let republished = manager_with_stub(home.path(), &identifier, "[registry]\ndefault = \"republished\"\n");
        let outcome = apply_managed_config(
            &ocx_config::Config::default(),
            Some(reference),
            true,
            false,
            &republished,
            &file_structure,
        )
        .await
        .expect("a dry-run re-run must succeed");
        match outcome {
            ManagedConfigSetupOutcome::WouldRefresh { digest } => assert_eq!(digest, first_digest),
            other => panic!("expected WouldRefresh, got {other:?}"),
        }

        let snapshot = ocx_config::managed_config::read_managed_config_snapshot(&file_structure.state.managed_config())
            .await
            .expect("the snapshot must survive a dry run");
        assert_eq!(snapshot.digest, first_digest, "dry-run must not persist anything");
        assert!(
            snapshot.config.contains("adopted"),
            "dry-run must not fetch the new payload"
        );
    }

    /// Full 2^3 matrix of the pure skip decision, including its precedence:
    /// a digest pin outranks a missing client, which outranks a pause.
    #[test]
    fn refresh_skip_reason_matrix() {
        let floating = ocx_oci::Identifier::parse("corp.example.com/ocx-config:user").unwrap();
        let pinned =
            ocx_oci::Identifier::parse(&format!("corp.example.com/ocx-config:user@sha256:{}", "a".repeat(64))).unwrap();

        // Not pinned.
        assert_eq!(refresh_skip_reason(&floating, true, false), None, "the refresh runs");
        assert_eq!(refresh_skip_reason(&floating, true, true), Some("paused"));
        assert_eq!(refresh_skip_reason(&floating, false, false), Some("cannot-fetch"));
        assert_eq!(
            refresh_skip_reason(&floating, false, true),
            Some("cannot-fetch"),
            "a missing client outranks a pause"
        );

        // Pinned — content-addressed, so it wins in every combination.
        for can_fetch in [true, false] {
            for paused in [true, false] {
                assert_eq!(
                    refresh_skip_reason(&pinned, can_fetch, paused),
                    Some("digest-pinned"),
                    "a digest pin outranks can_fetch={can_fetch} paused={paused}"
                );
            }
        }
    }

    // ── W5: `--managed-config ""` clear path ─────────────────────────────────

    /// The clear path removes both the `[managed]` fence from config.toml and
    /// the snapshot directory — no ghost tier survives.
    #[tokio::test]
    async fn clear_managed_config_removes_fence_and_snapshot_dir() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let config_path = home.path().join("config.toml");

        // A fenced [managed] block plus a user section outside the fence.
        let body = "[managed]\nsource = \"corp.example.com/ocx-config:user\"\n";
        let content = rc_block::apply(
            "[registry]\ndefault = \"keep.me\"\n\n",
            body,
            false,
            rc_block::MANAGED_LABEL,
        )
        .expect("apply infallible")
        .expect("fresh append produces content");
        std::fs::write(&config_path, &content).unwrap();

        // A snapshot dir with content.
        let managed_dir = file_structure.state.managed_config().dir();
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::write(managed_dir.join("snapshot.json"), b"{}").unwrap();

        let outcome = clear_managed_config(&config_path, &file_structure)
            .await
            .expect("clear must succeed");
        assert!(matches!(outcome, ManagedConfigSetupOutcome::Cleared));

        let after = read(&config_path);
        assert!(!after.contains("[managed]"), "the fence must be removed: {after:?}");
        assert!(after.contains("keep.me"), "content outside the fence survives");
        assert!(!managed_dir.exists(), "the snapshot directory must be deleted");
    }

    /// ocx#468 (review D1): the fence writers take the same `config-edit`
    /// lock the surgical writers do — a clear waits behind a held lock and
    /// lands once it is released. Same shape as `edit.rs`'s own proof.
    ///
    /// **Red-state**: write the fence through `write_profile` again (or key
    /// the lock differently) and the clear finishes inside the 50 ms.
    #[tokio::test(flavor = "multi_thread")]
    async fn clear_managed_config_waits_behind_the_config_edit_lock() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let config_path = home.path().join("config.toml");
        let content = rc_block::apply(
            "",
            "[managed]\nsource = \"corp.example.com/ocx-config:user\"\n",
            false,
            rc_block::MANAGED_LABEL,
        )
        .expect("apply infallible")
        .expect("fresh append produces content");
        std::fs::write(&config_path, &content).unwrap();
        let held = ocx_util::fs::lock_scoped(
            &file_structure.locks,
            "config-edit",
            home.path(),
            "config.toml",
            std::time::Duration::from_secs(5),
        )
        .await
        .unwrap();

        let task = tokio::spawn({
            let (config_path, file_structure) = (config_path.clone(), file_structure.clone());
            async move { clear_managed_config(&config_path, &file_structure).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !task.is_finished(),
            "the clear must wait behind a held config-edit lock"
        );

        drop(held);
        let outcome = task.await.unwrap().expect("the clear lands once the lock is released");
        assert!(matches!(outcome, ManagedConfigSetupOutcome::Cleared));
        assert!(!read(&config_path).contains("[managed]"), "the fence must be removed");
    }

    /// Clearing when nothing exists (no fence, no dir) is a no-op success —
    /// idempotent clears never error.
    #[tokio::test]
    async fn clear_managed_config_is_idempotent_when_nothing_exists() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let config_path = home.path().join("config.toml");

        let outcome = clear_managed_config(&config_path, &file_structure)
            .await
            .expect("an empty clear must succeed");
        assert!(matches!(outcome, ManagedConfigSetupOutcome::Cleared));
        assert!(!config_path.exists(), "no config.toml is created by a no-op clear");
    }

    /// W5: clearing while `OCX_MANAGED_CONFIG` is still exported succeeds and
    /// still removes the local state (the warn about the lingering env var is
    /// advisory, not a failure).
    #[tokio::test]
    async fn clear_managed_config_with_env_still_set_clears_and_succeeds() {
        let env = ocx_util::env::overrides::lock();
        env.set("OCX_MANAGED_CONFIG", "corp.example.com/ocx-config:user");
        let home = tempfile::TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let config_path = home.path().join("config.toml");

        let managed_dir = file_structure.state.managed_config().dir();
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::write(managed_dir.join("snapshot.json"), b"{}").unwrap();

        let outcome = clear_managed_config(&config_path, &file_structure)
            .await
            .expect("clear with a lingering env override must still succeed");
        assert!(matches!(outcome, ManagedConfigSetupOutcome::Cleared));
        assert!(!managed_dir.exists(), "the snapshot directory must be deleted");
    }

    // ── fence body constants round-trip through rc_block ─────────────────────

    #[test]
    fn fence_bodies_round_trip_through_rc_block_apply() {
        // Each fence body, appended to a fresh file, must classify as Current on
        // a re-run — the orchestrator relies on this for idempotency (NoOp).
        for body in [POSIX_BODY, ELVISH_BODY, POWERSHELL_BODY] {
            let appended = rc_block::apply("", body, false, rc_block::OCX_LABEL)
                .expect("apply infallible")
                .expect("fresh append produces content");
            assert_eq!(
                rc_block::classify(&appended, body, rc_block::OCX_LABEL),
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

    // ── phase 3: the profile tri-state and the hand-off guard ────────────────

    #[tokio::test]
    async fn empty_profile_list_writes_no_block_but_still_writes_shims() {
        // `--no-profile` is the narrow opt-out: no profile block anywhere, but
        // the env shims (phase 2) and the session PATH (phases 0 + 3.5) still
        // run. Routing it through the `--no-modify-path` arm would silently
        // widen it into "touch no PATH surface at all".
        let dir = tempfile::tempdir().unwrap();
        let options = SetupOptions {
            profiles: Some(Vec::new()),
            ..SetupOptions::default()
        };

        let outcomes = apply_profile_phase(dir.path(), &options).await.unwrap();
        assert!(
            outcomes.is_empty(),
            "an empty profile list resolves zero targets: {outcomes:?}"
        );

        assert!(
            options.touches_session_path(),
            "an empty profile list must not suppress the session-PATH arm — that is --no-modify-path's job"
        );
        let opted_out = SetupOptions {
            no_modify_path: true,
            ..SetupOptions::default()
        };
        assert!(
            !opted_out.touches_session_path(),
            "--no-modify-path does suppress it, or the assertion above is vacuous"
        );

        // Phase 2 reads no option at all, so the same home still gets its shims.
        let shims_written = shims::write_shims(dir.path(), false).unwrap();
        assert!(
            !shims_written.is_empty(),
            "the env shims are written whatever the profile list says"
        );
    }

    /// `None` is the third state: no overrides at all, so detection runs —
    /// exactly what an empty `Vec` used to mean before the list could say
    /// "none".
    ///
    /// Driven through [`compose_targets`] with the probe's answer supplied,
    /// rather than [`resolve_targets`] with a real one: spawning a PowerShell
    /// host cost a second of wall clock and made the appended target depend on
    /// whether the machine had `pwsh` — so on this one the append was asserted
    /// and on CI it was not. Supplying it pins the append everywhere.
    #[test]
    fn absent_profile_list_auto_detects() {
        let dir = tempfile::tempdir().unwrap();
        let probed = dir.path().join("Microsoft.PowerShell_profile.ps1");

        let detected = compose_targets(dir.path(), None, Some(probed.clone()));
        let expected = profiles::detect_targets(&home_env_from_environment(dir.path()));
        assert!(
            !expected.is_empty(),
            "detection always yields at least one target, or the comparison below is vacuous"
        );
        assert_eq!(
            detected.get(..expected.len()),
            Some(&expected[..]),
            "`None` must take the auto-detection path"
        );
        assert_eq!(
            detected.get(expected.len()..),
            Some(
                &[ProfileTarget {
                    path: probed.clone(),
                    kind: ProfileKind::PowerShellFence,
                }][..]
            ),
            "…and a PowerShell host that answered is appended to it, as a fence target"
        );
        assert_eq!(
            compose_targets(dir.path(), None, None),
            expected,
            "…while a host that did not answer appends nothing"
        );

        let none: Vec<PathBuf> = Vec::new();
        assert!(
            compose_targets(dir.path(), Some(none.as_slice()), Some(probed)).is_empty(),
            "…and `Some(&[])` must not, or the two states are indistinguishable — not even the \
             probed profile survives an explicit empty list"
        );
    }

    /// The probe runs for `None` and **only** for `None`: an override list is
    /// not a question about which shells are installed, and
    /// [`absent_profile_list_auto_detects`] drives the composition below the
    /// branch that decides it.
    #[tokio::test]
    async fn an_override_list_resolves_without_probing_for_a_shell_host() {
        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("profile.sh");

        assert_eq!(
            resolve_targets(dir.path(), Some(std::slice::from_ref(&named))).await,
            vec![ProfileTarget {
                path: named,
                kind: ProfileKind::PosixFence,
            }],
            "an override list is taken verbatim, with no probed target appended"
        );
    }

    #[tokio::test]
    async fn handoff_heals_but_never_introduces() {
        // The Decision 4C guard, wired end-to-end through phase 3: an update
        // may repair the shell integration a user already opted into, and may
        // not hand shell integration to a machine that never asked (a CI runner
        // where ocx came from a package manager and was never set up).
        let dir = tempfile::tempdir().unwrap();

        let drifted = dir.path().join("drifted.sh");
        let old_body = ". \"$OCX_HOME/env.sh\"";
        let marker = rc_block::canonical_hash(old_body);
        std::fs::write(
            &drifted,
            format!("# >>> ocx v1 {marker} >>>\n{old_body}\n# <<< ocx <<<\n"),
        )
        .unwrap();

        let untouched = dir.path().join("untouched.sh");
        std::fs::write(&untouched, "export PATH=/bin\n").unwrap();

        let options = SetupOptions {
            profiles: Some(vec![drifted.clone(), untouched.clone()]),
            handoff: true,
            ..SetupOptions::default()
        };
        let outcomes = apply_profile_phase(dir.path(), &options).await.unwrap();

        assert_eq!(
            outcomes,
            vec![
                (drifted.clone(), ProfileOutcome::Completed),
                (untouched.clone(), ProfileOutcome::NoOp),
            ]
        );
        assert!(
            read(&drifted).contains(POSIX_BODY),
            "a present-but-drifted block still heals under --handoff"
        );
        assert_eq!(
            read(&untouched),
            "export PATH=/bin\n",
            "--handoff must leave a profile with no ocx footprint byte-identical"
        );
    }

    #[tokio::test]
    async fn handoff_false_still_introduces() {
        // The other half of the guard: an ordinary first-time `ocx self setup`
        // is what actually introduces the block, and the flag did not break it.
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("first-time.sh");
        std::fs::write(&profile, "export PATH=/bin\n").unwrap();

        let options = SetupOptions {
            profiles: Some(vec![profile.clone()]),
            handoff: false,
            ..SetupOptions::default()
        };
        let outcomes = apply_profile_phase(dir.path(), &options).await.unwrap();

        assert_eq!(outcomes, vec![(profile.clone(), ProfileOutcome::Completed)]);
        let written = read(&profile);
        assert!(
            written.starts_with("export PATH=/bin\n"),
            "the user's existing content is preserved"
        );
        assert!(written.contains("# >>> ocx v1"), "a first-time setup writes the fence");
        assert!(written.contains(POSIX_BODY), "…carrying the canonical body");
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

    // ── persist_extra_ca_certs (phase 0.5, C-008, ocx#448) ──────────────────

    mod extra_ca {
        use super::*;
        use ocx_test_support::pki::{TestPki, mint_root, pem_block};

        /// Parsed root-level `extra_ca_certs_pem` of `<home>/config.toml`, or
        /// `None` when the file or the key is absent.
        fn persisted_pem(home: &Path) -> Option<String> {
            let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
            let doc: toml::Value = toml::from_str(&text).expect("config.toml must stay parseable");
            doc.get("extra_ca_certs_pem")?.as_str().map(str::to_owned)
        }

        fn config_text(home: &Path) -> String {
            std::fs::read_to_string(home.join("config.toml")).expect("config.toml exists")
        }

        /// A pre-existing table, so "renders above the first `[table]`" is a
        /// real position and a surgical edit has something to preserve.
        const PRESEEDED: &str = "[registries.\"corp.example\"]\nindex = \"https://index.corp.example\"\n";

        /// C-008: env unset (`None`) and `""` are both "not configured" — a
        /// no-op with no file created.
        #[tokio::test]
        async fn extra_ca_persist_unset_or_empty_is_not_configured() {
            let dir = tempfile::tempdir().unwrap();
            for value in [None, Some("")] {
                let outcome = persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), value, false, false)
                    .await
                    .expect("no-op");
                assert!(
                    matches!(outcome, ExtraCaCertsOutcome::NotConfigured),
                    "{value:?}: {outcome:?}"
                );
            }
            assert!(
                !dir.path().join("config.toml").exists(),
                "a no-op must not create config.toml"
            );
        }

        /// C-008 / D-5: a path value is read now and persisted INLINE as the
        /// root `extra_ca_certs_pem`, above a pre-existing table which survives.
        #[tokio::test]
        async fn extra_ca_persist_path_value_writes_inline_pem_at_root() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("config.toml"), PRESEEDED).unwrap();
            let pki = TestPki::mint();
            let ca_path = dir.path().join("corp-ca.pem");
            std::fs::write(&ca_path, pki.root_pem()).unwrap();

            let outcome = persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), ca_path.to_str(), false, false)
                .await
                .expect("persists");
            assert!(
                matches!(outcome, ExtraCaCertsOutcome::Persisted { certificates: 1 }),
                "{outcome:?}"
            );

            assert_eq!(persisted_pem(dir.path()).as_deref(), Some(pki.root_pem().as_str()));
            let text = config_text(dir.path());
            assert!(
                text.find("extra_ca_certs_pem").unwrap() < text.find("[registries").unwrap(),
                "root key must render above the first table:\n{text}"
            );
            assert!(
                !text.contains(ca_path.to_str().unwrap()),
                "the path is never persisted (D-5)"
            );
            let doc: toml::Value = toml::from_str(&text).unwrap();
            assert_eq!(
                doc["registries"]["corp.example"]["index"].as_str(),
                Some("https://index.corp.example"),
                "the pre-existing table survives the surgical edit"
            );
        }

        /// C-008 / D-4: inline PEM text (a value containing `-----BEGIN`) persists the
        /// same way; the file is created when absent.
        #[tokio::test]
        async fn extra_ca_persist_inline_value_creates_config_when_absent() {
            let dir = tempfile::tempdir().unwrap();
            let pki = TestPki::mint();
            let pem = pki.root_pem();

            let outcome = persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&pem), false, false)
                .await
                .expect("persists");
            assert!(
                matches!(outcome, ExtraCaCertsOutcome::Persisted { certificates: 1 }),
                "{outcome:?}"
            );
            assert_eq!(persisted_pem(dir.path()).as_deref(), Some(pem.as_str()));
        }

        /// C-008: an identical re-run is `Unchanged` and rewrites nothing —
        /// byte-identical file.
        #[tokio::test]
        async fn extra_ca_persist_rerun_is_unchanged_and_byte_identical() {
            let dir = tempfile::tempdir().unwrap();
            let pem = TestPki::mint().root_pem();
            persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&pem), false, false)
                .await
                .expect("first run persists");
            let first = std::fs::read(dir.path().join("config.toml")).unwrap();

            let outcome = persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&pem), false, false)
                .await
                .expect("re-run");
            assert!(
                matches!(outcome, ExtraCaCertsOutcome::Unchanged { certificates: 1 }),
                "{outcome:?}"
            );
            assert_eq!(std::fs::read(dir.path().join("config.toml")).unwrap(), first);
        }

        /// C-008 edge case: a CRLF bundle persists (the TOML string carries
        /// escaped `\r`), and the second run compares PARSED strings, not file
        /// bytes — so the same CRLF value is `Unchanged`, not re-persisted.
        #[tokio::test]
        async fn extra_ca_persist_crlf_pem_rerun_compares_parsed_strings() {
            let dir = tempfile::tempdir().unwrap();
            let (_, root) = mint_root("CN=ocx-test-ca");
            let crlf = pem::encode_config(
                &pem::Pem::new("CERTIFICATE", root.as_slice()),
                pem::EncodeConfig::new().set_line_ending(pem::LineEnding::CRLF),
            );
            assert!(crlf.contains("\r\n"), "fixture must carry CRLF");

            let outcome = persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&crlf), false, false)
                .await
                .expect("CRLF persists");
            assert!(
                matches!(outcome, ExtraCaCertsOutcome::Persisted { certificates: 1 }),
                "{outcome:?}"
            );
            assert_eq!(
                persisted_pem(dir.path()).as_deref(),
                Some(crlf.as_str()),
                "CRLF round-trips"
            );
            let first = std::fs::read(dir.path().join("config.toml")).unwrap();

            let outcome = persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&crlf), false, false)
                .await
                .expect("re-run");
            assert!(matches!(outcome, ExtraCaCertsOutcome::Unchanged { .. }), "{outcome:?}");
            assert_eq!(std::fs::read(dir.path().join("config.toml")).unwrap(), first);
        }

        /// ocx#469 (review D2): a value set under the system lock is reported
        /// `SystemLocked` and nothing is validated or written — not even the
        /// lock file — in either mode. The value is a private key on purpose:
        /// a validating path would refuse it, so the `Ok` proves the skip.
        #[tokio::test]
        async fn a_value_under_the_system_lock_is_reported_and_not_persisted() {
            let dir = tempfile::tempdir().unwrap();
            let key = pem_block("PRIVATE KEY", &TestPki::mint().leaf_key_pkcs8);

            for dry_run in [false, true] {
                let outcome = persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&key), dry_run, true)
                    .await
                    .expect("a locked pair is an outcome, not a refusal");
                assert!(matches!(outcome, ExtraCaCertsOutcome::SystemLocked), "{outcome:?}");
            }
            assert!(
                !dir.path().join("config.toml").exists(),
                "nothing is written under the lock"
            );
            assert!(!dir.path().join("locks").exists(), "nothing is locked under the lock");
        }

        /// C-008 / C-009: `dry_run` reports `WouldPersist` and writes nothing;
        /// over an already-persisted identical value it reports `Unchanged`.
        #[tokio::test]
        async fn extra_ca_persist_dry_run_writes_nothing() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("config.toml"), PRESEEDED).unwrap();
            let pem = TestPki::mint().root_pem();

            let outcome = persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&pem), true, false)
                .await
                .expect("dry run");
            assert!(
                matches!(outcome, ExtraCaCertsOutcome::WouldPersist { certificates: 1 }),
                "{outcome:?}"
            );
            assert_eq!(
                config_text(dir.path()),
                PRESEEDED,
                "dry-run must leave config.toml byte-identical"
            );

            persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&pem), false, false)
                .await
                .expect("real run");
            let outcome = persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&pem), true, false)
                .await
                .expect("dry run over persisted");
            assert!(
                matches!(outcome, ExtraCaCertsOutcome::Unchanged { certificates: 1 }),
                "{outcome:?}"
            );
        }

        /// C-008 (XOR): a root `extra_ca_certs` path key is removed when
        /// `extra_ca_certs_pem` is written — the loader refuses both together.
        #[tokio::test]
        async fn extra_ca_persist_removes_root_path_key() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("config.toml"),
                format!("extra_ca_certs = \"/etc/ssl/old-corp.pem\"\n{PRESEEDED}"),
            )
            .unwrap();
            let pem = TestPki::mint().root_pem();

            persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&pem), false, false)
                .await
                .expect("persists");

            let doc: toml::Value = toml::from_str(&config_text(dir.path())).unwrap();
            assert!(
                doc.get("extra_ca_certs").is_none(),
                "the path key must be removed: {doc}"
            );
            assert_eq!(doc["extra_ca_certs_pem"].as_str(), Some(pem.as_str()));
        }

        /// C-008 / edge case: a two-root bundle persists whole, `certificates: 2`.
        #[tokio::test]
        async fn extra_ca_persist_two_root_bundle_counts_two() {
            let dir = tempfile::tempdir().unwrap();
            let (_, first) = mint_root("CN=ocx-test-ca-1");
            let (_, second) = mint_root("CN=ocx-test-ca-2");
            let bundle = format!(
                "{}{}",
                pem_block("CERTIFICATE", &first),
                pem_block("CERTIFICATE", &second)
            );

            let outcome = persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&bundle), false, false)
                .await
                .expect("persists");
            assert!(
                matches!(outcome, ExtraCaCertsOutcome::Persisted { certificates: 2 }),
                "{outcome:?}"
            );
            assert_eq!(persisted_pem(dir.path()).as_deref(), Some(bundle.as_str()));
        }

        /// C-004 / C-008: a refused value (a `PRIVATE KEY` block) surfaces as
        /// `Error::ExtraCaCerts` BEFORE any write — the file is untouched.
        #[tokio::test]
        async fn extra_ca_persist_refused_value_writes_nothing() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("config.toml"), PRESEEDED).unwrap();
            let pki = TestPki::mint();
            let key = pem_block("PRIVATE KEY", &pki.leaf_key_pkcs8);

            match persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&key), false, false).await {
                Err(error::Error::ExtraCaCerts(ocx_config::tls::TlsError::NotACertificate { tag, .. })) => {
                    assert_eq!(tag, "PRIVATE KEY");
                }
                other => panic!("expected ExtraCaCerts(NotACertificate), got {other:?}"),
            }
            assert_eq!(
                config_text(dir.path()),
                PRESEEDED,
                "a refusal must leave config.toml byte-identical"
            );
        }

        /// C-008: a bundle that validates but is not UTF-8 (a Latin-1
        /// `subject=` label ahead of the block) cannot be a TOML string —
        /// refused as data (65) after validation, nothing written, and the
        /// message carries the byte count only (D-11).
        #[tokio::test]
        async fn extra_ca_persist_non_utf8_bundle_is_a_data_refusal_before_write() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("config.toml"), PRESEEDED).unwrap();
            let mut on_disk = b"subject=CN=corp caf\xe9\n".to_vec();
            on_disk.extend_from_slice(TestPki::mint().root_pem().as_bytes());
            let ca_path = dir.path().join("latin1-ca.pem");
            std::fs::write(&ca_path, &on_disk).unwrap();

            let error = persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), ca_path.to_str(), false, false)
                .await
                .expect_err("not UTF-8");
            match &error {
                error::Error::ExtraCaCertsNotUtf8 { bytes } => assert_eq!(*bytes, on_disk.len()),
                other => panic!("expected ExtraCaCertsNotUtf8, got {other:?}"),
            }
            assert!(
                !error.to_string().contains("caf"),
                "the bytes are never echoed: {error}"
            );
            assert!(
                error.to_string().contains("strip the non-UTF-8 label lines"),
                "the message names the remedy: {error}"
            );
            assert_eq!(
                config_text(dir.path()),
                PRESEEDED,
                "a refusal must leave config.toml byte-identical"
            );
        }

        /// C-008 / D-10: a path to a non-regular file is refused through the
        /// bounded read — `Unreadable`, nothing written, no hang.
        #[cfg(unix)]
        #[tokio::test]
        async fn extra_ca_persist_dev_zero_path_is_unreadable() {
            let dir = tempfile::tempdir().unwrap();
            match persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some("/dev/zero"), false, false).await {
                Err(error::Error::ExtraCaCerts(ocx_config::tls::TlsError::Unreadable { .. })) => {}
                other => panic!("expected ExtraCaCerts(Unreadable), got {other:?}"),
            }
            assert!(!dir.path().join("config.toml").exists());
        }

        /// C-008: a rendered document over `MAX_CONFIG_SIZE` is refused before
        /// any write. Positive control first — the same bundle persists into a
        /// small file — so the refusal is the rendered size, not the bundle's.
        #[tokio::test]
        async fn extra_ca_persist_oversize_rendered_config_is_refused_before_write() {
            use ocx_util::tls::MAX_EXTRA_CA_CERTS_BYTES;

            const MAX_CONFIG_SIZE: usize = ocx_config::loader::MAX_CONFIG_SIZE as usize;

            let mut bundle = String::new();
            let mut index = 0;
            while bundle.len() < MAX_EXTRA_CA_CERTS_BYTES - 2048 {
                let (_, root) = mint_root(&format!("CN=ocx-test-ca-{index}"));
                bundle.push_str(&pem_block("CERTIFICATE", &root));
                index += 1;
            }
            assert!(
                bundle.len() < MAX_EXTRA_CA_CERTS_BYTES,
                "fixture must sit under the value cap"
            );

            let control = tempfile::tempdir().unwrap();
            let outcome = persist_extra_ca_certs(
                &control.path().join("locks"),
                control.path(),
                Some(&bundle),
                false,
                false,
            )
            .await
            .expect("control persists");
            assert!(matches!(outcome, ExtraCaCertsOutcome::Persisted { .. }), "{outcome:?}");

            let dir = tempfile::tempdir().unwrap();
            let padding: String = std::iter::repeat_n(format!("# {}\n", "x".repeat(98)), 410).collect();
            let seeded = format!("{padding}{PRESEEDED}");
            assert!(
                seeded.len() < MAX_CONFIG_SIZE,
                "the seeded file must itself be loadable"
            );
            assert!(
                seeded.len() + bundle.len() > MAX_CONFIG_SIZE,
                "seeded + bundle must exceed the ceiling"
            );
            std::fs::write(dir.path().join("config.toml"), &seeded).unwrap();

            match persist_extra_ca_certs(&dir.path().join("locks"), dir.path(), Some(&bundle), false, false).await {
                Err(error @ error::Error::RenderedConfigTooLarge { .. }) => {
                    let error::Error::RenderedConfigTooLarge { path, bytes } = &error else {
                        unreachable!()
                    };
                    assert_eq!(*path, dir.path().join("config.toml"));
                    assert!(
                        *bytes > MAX_CONFIG_SIZE,
                        "reported size {bytes} must exceed the ceiling"
                    );
                    // The message names the bound and a remedy (review r1).
                    let rendered = error.to_string();
                    assert!(
                        rendered.contains(&format!("would leave {} at {bytes} bytes", path.display())),
                        "{rendered}"
                    );
                    assert!(
                        rendered.contains(&format!("over the {MAX_CONFIG_SIZE}-byte config limit")),
                        "{rendered}"
                    );
                    assert!(rendered.contains("extra_ca_certs = \"<path>\""), "{rendered}");
                }
                other => panic!("expected RenderedConfigTooLarge, got {other:?}"),
            }
            assert_eq!(
                config_text(dir.path()),
                seeded,
                "a refusal must leave config.toml byte-identical"
            );
        }
    }
}
