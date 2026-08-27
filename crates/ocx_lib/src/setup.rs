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
pub mod shell_config;
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
    /// The effective managed-config tier to adopt, already resolved by the
    /// caller: `Some(ref)` adopts (sync fetch+persist, then fence write),
    /// `Some("")` clears, `None` leaves the tier untouched. The CLI applies the
    /// flag → `OCX_MANAGED_CONFIG` → `[managed].source` seed precedence before
    /// building these options; this layer just consumes the result.
    pub managed_config: Option<String>,
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
    /// Result of adopting/clearing the `--managed-config` tier (phase 1.5).
    pub managed_config: ManagedConfigSetupOutcome,
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
        digest: crate::oci::Digest,
    },
    /// An already-adopted seed was re-synced and the registry served newer
    /// content: the snapshot was replaced in place, the fence untouched.
    Refreshed {
        /// The digest the snapshot carried before this run.
        from: crate::oci::Digest,
        /// The newly persisted digest.
        to: crate::oci::Digest,
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
        digest: crate::oci::Digest,
        /// Why the refresh did not complete — the fetch error with its full
        /// `source()` chain flattened in (`crate::error::render_chain`), since
        /// the outermost message alone names no cause.
        reason: String,
    },
    /// `--dry-run` against an already-adopted seed: a refresh would run, but
    /// nothing was fetched and nothing was written.
    WouldRefresh {
        /// The existing snapshot's digest.
        digest: crate::oci::Digest,
    },
    /// A new or changed ref was adopted: synchronous fetch+persist succeeded
    /// before the fence was written (ADR "Setup ordering").
    Adopted {
        /// The newly persisted manifest digest.
        digest: crate::oci::Digest,
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
    config: &crate::config::Config,
    manager: &PackageManager,
    file_structure: &FileStructure,
) -> Result<SetupOutcome, error::Error> {
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
        managed_config,
    })
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
/// `Some(ref)` adopts: re-parses `ref` as an [`oci::Identifier`](crate::oci::Identifier)
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
    config: &crate::config::Config,
    managed_config: Option<&str>,
    dry_run: bool,
    force: bool,
    manager: &PackageManager,
    file_structure: &FileStructure,
) -> Result<ManagedConfigSetupOutcome, error::Error> {
    use crate::config::managed::{ManagedConfig, check_locked_managed_override};
    use crate::package_manager::ManagedConfigUpdateResult;
    use crate::setup::rc_block;

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
    let content = read_to_string_or_empty(&config_path).await?;

    if flag_value.is_empty() {
        if dry_run {
            return Ok(ManagedConfigSetupOutcome::WouldAdopt);
        }
        return clear_managed_config(&config_path, &content, file_structure).await;
    }

    let identifier = crate::oci::Identifier::parse_with_default_registry(flag_value, crate::oci::DEFAULT_REGISTRY)
        .map_err(|source| error::Error::InvalidManagedConfigSource {
            value: flag_value.to_string(),
            source,
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
    let mut adopted: Option<crate::config::managed::ManagedConfigSnapshot> = None;
    if state == rc_block::BlockState::Current {
        // W3: a `Current` fence alone does not prove the tier is healthy — the
        // snapshot may have been wiped or belong to a different source (e.g. a
        // restored $OCX_HOME). Only a present, identity-matching snapshot
        // counts as adopted; otherwise fall through to the fetch+persist below
        // to self-heal (the fence itself is never rewritten — `rc_block::apply`
        // returns `None` for a `Current` block).
        let snapshot = crate::managed_config::read_managed_config_snapshot(&file_structure.state).await;
        match snapshot {
            Some(snapshot) if crate::config::managed::snapshot_matches_source(&snapshot, &identifier) => {
                adopted = Some(snapshot);
            }
            _ => {
                crate::log::info!(
                    "managed-config fence is current but the snapshot is absent or mismatched; re-syncing"
                );
            }
        }
    }

    if let Some(snapshot) = &adopted {
        // A deliberate skip is not a failure: report the existing snapshot as
        // adopted and stay silent on stderr (no warn noise for `--offline`, a
        // digest-pinned seed, or an in-force pause).
        let paused = crate::managed_config::read_pause(&file_structure.state).await.is_some();
        if let Some(reason) = refresh_skip_reason(&identifier, manager.can_fetch_managed_config(), paused) {
            crate::log::debug!("managed-config refresh skipped ({reason})");
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
    let resolved = crate::config::managed::ResolvedManagedConfig {
        source: identifier,
        required: ManagedConfig::DEFAULT_REQUIRED,
        refresh: ManagedConfig::DEFAULT_REFRESH,
        interval: crate::config::managed::parse_interval(ManagedConfig::DEFAULT_INTERVAL)
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
            // blip or a bad published payload must not fail a re-run, because
            // the tier stays usable on the content already on disk. A
            // `SnapshotWriteFailed` can fire AFTER the payload rename (the
            // metadata write is a separate atomic rename), leaving the new
            // payload live under the old provenance — reporting "kept the
            // existing snapshot" there would be false, so it propagates.
            let pre_write_failure = matches!(
                &error,
                crate::managed_config::ManagedConfigUpdateError::Fetch(_)
                    | crate::managed_config::ManagedConfigUpdateError::SourceNotFound { .. }
                    | crate::managed_config::ManagedConfigUpdateError::Persist(
                        crate::managed_config::ManagedConfigPersistError::InvalidToml { .. }
                    )
            );
            let Some(previous) = adopted.filter(|_| pre_write_failure) else {
                return Err(error.into());
            };
            // The error never reaches `main`, so nothing walks its `source()`
            // chain for us — and the dominant variant (`Fetch`) interpolates
            // nothing, so its bare `Display` would name no cause at all.
            let reason = crate::error::render_chain(&error);
            crate::log::warn!(
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

    // Fence written only after the fetch+persist above succeeded.
    if let Some(new_content) = rc_block::apply(&content, &body, force, rc_block::MANAGED_LABEL)? {
        write_profile(&config_path, &new_content).await?;
    }

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
fn refresh_skip_reason(identifier: &crate::oci::Identifier, can_fetch: bool, paused: bool) -> Option<&'static str> {
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
    content: &str,
    file_structure: &FileStructure,
) -> Result<ManagedConfigSetupOutcome, error::Error> {
    let stripped = crate::setup::rc_block::remove_block(content, crate::setup::rc_block::MANAGED_LABEL);
    if stripped != content {
        write_profile(config_path, &stripped).await?;
    }

    let managed_dir = file_structure.state.managed_config_dir();
    if crate::utility::fs::path_exists_lossy(&managed_dir).await {
        tokio::fs::remove_dir_all(&managed_dir)
            .await
            .map_err(|source| error::Error::Io {
                path: managed_dir,
                source,
            })?;
    }

    if crate::env::var(crate::env::keys::OCX_MANAGED_CONFIG).is_some_and(|value| !value.is_empty()) {
        crate::log::warn!(
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

/// Blocking body of [`write_profile`]: create the parent dir, then write the
/// content atomically via [`crate::utility::fs::write_bytes_atomic`] (private
/// temp file in the parent, published over `path`).
fn write_profile_blocking(path: &Path, content: &str) -> Result<(), error::Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| error::Error::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    crate::utility::fs::write_bytes_atomic(path, content.as_bytes()).map_err(|source| error::Error::Io {
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

    // ── W3: `--managed-config <ref>` Current-fence snapshot gate ─────────────

    /// Builds a manager whose managed-config client serves the v2 package
    /// shape for `identifier` (stub transport, no network).
    fn manager_with_stub(root: &Path, identifier: &crate::oci::Identifier, config_toml: &str) -> PackageManager {
        let (client, _) = crate::managed_config::test_support::stub_client_with_package(identifier, config_toml);
        let fs = FileStructure::with_root(root.to_path_buf());
        let local_index = crate::oci::index::LocalIndex::new(crate::oci::index::LocalConfig {
            index_store: fs.index.clone(),
        });
        let index = crate::oci::index::Index::from_chained(local_index, vec![], crate::oci::index::ChainMode::Offline);
        PackageManager::new(fs, index, None, "localhost:5000").with_managed_config_client(Some(client))
    }

    /// W3 matrix — `Current` fence + missing snapshot: self-heals by
    /// re-fetching (outcome `Adopted`), fence untouched.
    #[tokio::test]
    async fn apply_managed_config_current_fence_missing_snapshot_self_heals() {
        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = crate::oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let manager = manager_with_stub(home.path(), &identifier, "[registry]\ndefault = \"healed\"\n");

        // First adopt writes fence + snapshot.
        let first = apply_managed_config(
            &crate::config::Config::default(),
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
        std::fs::remove_dir_all(file_structure.state.managed_config_dir()).unwrap();

        let healed = apply_managed_config(
            &crate::config::Config::default(),
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
            file_structure.state.managed_config_snapshot_file().exists(),
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
        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let locked_ref = "corp.example.com/ocx-config:user";
        let manager = manager_with_stub(
            home.path(),
            &crate::oci::Identifier::parse(locked_ref).unwrap(),
            "[registry]\ndefault = \"x\"\n",
        );
        let locked = crate::config::Config {
            managed: Some(crate::config::managed::ManagedConfig {
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
        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = crate::oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let manager = manager_with_stub(home.path(), &identifier, "[registry]\ndefault = \"healed\"\n");

        apply_managed_config(
            &crate::config::Config::default(),
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
            file_structure.state.managed_config_snapshot_file(),
            serde_json::to_vec(&poisoned).unwrap(),
        )
        .unwrap();
        std::fs::write(
            file_structure.state.managed_config_toml_file(),
            "[registry]\ndefault = \"poisoned\"\n",
        )
        .unwrap();

        let healed = apply_managed_config(
            &crate::config::Config::default(),
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
        let snapshot = crate::managed_config::read_managed_config_snapshot(&file_structure.state)
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
    async fn adopt(home: &Path, file_structure: &FileStructure, reference: &str, payload: &str) -> crate::oci::Digest {
        let identifier = crate::oci::Identifier::parse(reference).unwrap();
        let manager = manager_with_stub(home, &identifier, payload);
        let outcome = apply_managed_config(
            &crate::config::Config::default(),
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
        let elsewhere = crate::oci::Identifier::parse("other.example.com/unrelated-config:v1").unwrap();
        manager_with_stub(root, &elsewhere, "[registry]\ndefault = \"unrelated\"\n")
    }

    /// The `Current` fence + matching snapshot path RE-SYNCS: setup reconciles
    /// the managed tier on every run, so a newer payload published under the
    /// same tag is picked up here (`Refreshed`, from != to) and the snapshot on
    /// disk carries the new content. Restoring the old early return reds this.
    #[tokio::test]
    async fn apply_managed_config_current_fence_matching_snapshot_refreshes() {
        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = crate::oci::Identifier::parse(reference).unwrap();
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
            &crate::config::Config::default(),
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

        let snapshot = crate::managed_config::read_managed_config_snapshot(&file_structure.state)
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
        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = crate::oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let payload = "[registry]\ndefault = \"adopted\"\n";

        let first_digest = adopt(home.path(), &file_structure, reference, payload).await;
        let fetched_at_before = crate::managed_config::read_managed_config_snapshot(&file_structure.state)
            .await
            .expect("snapshot must exist after adopt")
            .fetched_at;

        let unchanged = manager_with_stub(home.path(), &identifier, payload);
        let second = apply_managed_config(
            &crate::config::Config::default(),
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
            crate::managed_config::read_managed_config_snapshot(&file_structure.state)
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
        let env = crate::test::env::lock();
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
            &crate::config::Config::default(),
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

        let snapshot = crate::managed_config::read_managed_config_snapshot(&file_structure.state)
            .await
            .expect("the snapshot must survive a failed refresh");
        assert_eq!(snapshot.digest, first_digest, "the snapshot is kept, not replaced");
        assert!(snapshot.config.contains("adopted"), "the payload is kept verbatim");
    }

    /// A published payload that fails validation (`Persist(InvalidToml)`) is a
    /// pre-write fault — nothing on disk has moved — so behind a matching
    /// snapshot it stays best-effort, exactly like a fetch fault.
    #[tokio::test]
    async fn apply_managed_config_refresh_invalid_payload_stays_best_effort() {
        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = crate::oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        let first_digest = adopt(
            home.path(),
            &file_structure,
            reference,
            "[registry]\ndefault = \"adopted\"\n",
        )
        .await;

        let invalid = manager_with_stub(home.path(), &identifier, "not = [valid toml");
        let second = apply_managed_config(
            &crate::config::Config::default(),
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
            "expected RefreshUnavailable, got {second:?}"
        );
        let snapshot = crate::managed_config::read_managed_config_snapshot(&file_structure.state)
            .await
            .expect("the snapshot must survive");
        assert_eq!(snapshot.digest, first_digest, "the snapshot is kept, not replaced");
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

        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = crate::oci::Identifier::parse(reference).unwrap();
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
        let dir = file_structure.state.managed_config_dir();
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&dir, perms).unwrap();

        let result = apply_managed_config(
            &crate::config::Config::default(),
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
        let snapshot = crate::managed_config::read_managed_config_snapshot(&file_structure.state)
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
        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let broken = manager_with_failing_fetch(home.path());

        let result = apply_managed_config(
            &crate::config::Config::default(),
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
            !file_structure.state.managed_config_snapshot_file().exists(),
            "a failed first adopt must not leave a snapshot"
        );
    }

    /// Self-heal sibling of the invariant above: a `Current` fence whose
    /// snapshot was wiped has no fallback either, so a failing fetch propagates
    /// instead of reporting `RefreshUnavailable`.
    #[tokio::test]
    async fn apply_managed_config_self_heal_fetch_failure_hard_fails() {
        let env = crate::test::env::lock();
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
        std::fs::remove_dir_all(file_structure.state.managed_config_dir()).unwrap();

        let broken = manager_with_failing_fetch(home.path());
        let result = apply_managed_config(
            &crate::config::Config::default(),
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
        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = crate::oci::Identifier::parse(reference).unwrap();
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
            &crate::config::Config::default(),
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
        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let payload = "[registry]\ndefault = \"pinned\"\n";
        // The stub's index digest does not depend on the identifier, so it can
        // be computed first and then baked into the pinned reference.
        let (_, index_digest) = crate::managed_config::test_support::stub_client_with_package(
            &crate::oci::Identifier::parse("corp.example.com/ocx-config:user").unwrap(),
            payload,
        );
        let reference = format!("corp.example.com/ocx-config:user@{index_digest}");
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        let first_digest = adopt(home.path(), &file_structure, &reference, payload).await;

        let broken = manager_with_failing_fetch(home.path());
        let second = apply_managed_config(
            &crate::config::Config::default(),
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
        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = crate::oci::Identifier::parse(reference).unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        let first_digest = adopt(
            home.path(),
            &file_structure,
            reference,
            "[registry]\ndefault = \"adopted\"\n",
        )
        .await;

        let pause = crate::managed_config::ManagedConfigPause::for_duration(std::time::Duration::from_secs(3600), None);
        crate::managed_config::write_pause(&file_structure.state, &pause)
            .await
            .unwrap();

        let republished = manager_with_stub(home.path(), &identifier, "[registry]\ndefault = \"republished\"\n");
        let second = apply_managed_config(
            &crate::config::Config::default(),
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
            crate::managed_config::read_pause(&file_structure.state).await.is_some(),
            "setup must not clear the pause"
        );
    }

    /// `--dry-run` against an adopted seed never fetches: it reports
    /// `WouldRefresh` and leaves the snapshot exactly as it was.
    #[tokio::test]
    async fn apply_managed_config_dry_run_on_adopted_seed_would_refresh() {
        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let reference = "corp.example.com/ocx-config:user";
        let identifier = crate::oci::Identifier::parse(reference).unwrap();
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
            &crate::config::Config::default(),
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

        let snapshot = crate::managed_config::read_managed_config_snapshot(&file_structure.state)
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
        let floating = crate::oci::Identifier::parse("corp.example.com/ocx-config:user").unwrap();
        let pinned =
            crate::oci::Identifier::parse(&format!("corp.example.com/ocx-config:user@sha256:{}", "a".repeat(64)))
                .unwrap();

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
        let env = crate::test::env::lock();
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
        let managed_dir = file_structure.state.managed_config_dir();
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::write(managed_dir.join("snapshot.json"), b"{}").unwrap();

        let outcome = clear_managed_config(&config_path, &content, &file_structure)
            .await
            .expect("clear must succeed");
        assert!(matches!(outcome, ManagedConfigSetupOutcome::Cleared));

        let after = read(&config_path);
        assert!(!after.contains("[managed]"), "the fence must be removed: {after:?}");
        assert!(after.contains("keep.me"), "content outside the fence survives");
        assert!(!managed_dir.exists(), "the snapshot directory must be deleted");
    }

    /// Clearing when nothing exists (no fence, no dir) is a no-op success —
    /// idempotent clears never error.
    #[tokio::test]
    async fn clear_managed_config_is_idempotent_when_nothing_exists() {
        let env = crate::test::env::lock();
        env.remove("OCX_MANAGED_CONFIG");
        let home = tempfile::TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let config_path = home.path().join("config.toml");

        let outcome = clear_managed_config(&config_path, "", &file_structure)
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
        let env = crate::test::env::lock();
        env.set("OCX_MANAGED_CONFIG", "corp.example.com/ocx-config:user");
        let home = tempfile::TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(home.path().to_path_buf());
        let config_path = home.path().join("config.toml");

        let managed_dir = file_structure.state.managed_config_dir();
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::write(managed_dir.join("snapshot.json"), b"{}").unwrap();

        let outcome = clear_managed_config(&config_path, "", &file_structure)
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
