// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx shell state` — read-only diagnostics for the shell integration.
//!
//! `--format` stays a **root/context** concern: no subcommand declares its own
//! `--format`/`--json`, per this repo's standing no-format-divergence rule.
//! `ocx --format json shell state` is the machine-readable form of the same
//! command.
//!
//! **No background work on the init path.** This command does not run the
//! background update check and does not require a managed snapshot — a
//! diagnostic that fails because the thing it is diagnosing is broken is
//! useless. `Shell::State` is listed in all three of `app.rs`'s command-skip
//! predicates (`should_check_for_update`,
//! `should_check_managed_config_refresh`, and through it
//! `should_enforce_managed_config_required`); without the third, a
//! `[managed] required = true` tier with no matching snapshot would exit 78
//! before this body ran. `should_check_for_update_skips_all_shell_variants_canary`
//! is the regression guard.
//!
//! **Read-only, absolutely** (A-29). Everything below is a `stat`, a read or a
//! pure derivation: nothing here writes a consent stamp, repairs a ledger or
//! emits a plan. `consent::record` is not called, and neither is any helper
//! that calls it — `load_project_with_lock_consenting` is deliberately *not*
//! the seam this command uses, because a stamp written from here would consent
//! to the very project it is diagnosing.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::project::consent::{self, Decision, Reason};
use ocx_lib::project::lock::ProjectLock;
use ocx_lib::reference_manager::ReferenceManager;
use ocx_lib::shell::coexistence;
use ocx_lib::shell::reconcile::{self, CARRIER_KEY, Ledger};
use ocx_lib::{ShellConsent, effective_consent, normalize_consent_path};

use crate::api::data::shell_state::{HookStatus, Note, ShellStateReport, VerboseShellState, WatchMember};
use crate::app::project_context::{self, ProjectContextError};
use crate::options::hook::{Hook, Rung};

/// The project-tier file name the CWD walk looks for, and the one whose
/// symlinked form A-12's reason row reports.
const PROJECT_FILE: &str = "ocx.toml";

/// Report the shell integration's state, and why it is inert when it is.
///
/// Prints the decoded `__OCX_ENV_STATE` ledger as fields, what each scope
/// applied, whether the watch-set fingerprint still matches, whether the
/// project scope's constant priors are intact, and — above all — the
/// enumerated reason the shell is not active: no consent stamp and no matching
/// grant, source-set drift naming the new source, the hook disabled naming the
/// deciding rung and tier, yielded to direnv or mise naming the live signal
/// observed, the ledger over cap, or the ledger absent versus corrupt.
///
/// Read-only: it never writes a consent stamp, never repairs the ledger and
/// never emits a plan. The repair gesture is `unset __OCX_ENV_STATE` (which
/// destroys the priors, so a new shell is the cleaner floor); this command is
/// how you check it worked.
///
/// Output is diagnostics for a human to read and is never valid shell source —
/// deliberately not interchangeable with `ocx self activate`.
///
/// Exits 0 in every reportable state, including an inert shell: the reason is
/// the payload, not a failure. Exits 74 only when `$OCX_HOME` cannot be read.
#[derive(Parser)]
pub struct ShellState {
    /// Add the diagnostics behind the answer - the decoded ledger, the
    /// fingerprint watch set and the hook ladder.
    ///
    /// Affects the plain rendering only. The structured report
    /// (`ocx --format json shell state`) carries every field either way.
    #[arg(short, long)]
    verbose: bool,
}

impl ShellState {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let report = derive(&context).await?;
        // Two report calls, one report: `--verbose` picks a plain rendering,
        // never a different payload. `VerboseShellState` serializes as its
        // inner `ShellStateReport`, so `--format json` is byte-identical with
        // and without the flag.
        if self.verbose {
            context.api().report(&VerboseShellState(report))?;
        } else {
            context.api().report(&report)?;
        }
        // C-051 — 0 in every reportable state. An inert shell, a corrupt
        // carrier, an over-cap ledger and a yielded project are all findings,
        // never failures; the only non-zero path is the unreadable `$OCX_HOME`
        // `derive` returns as an error above.
        Ok(ExitCode::SUCCESS)
    }
}

/// Build the report. The single fallible step is reading `$OCX_HOME`.
///
/// # Errors
///
/// [`ocx_lib::Error::InternalFile`] — classified as `IoError` (74) — when
/// `$OCX_HOME` exists but cannot be read. An **absent** home is not an error:
/// it is the ordinary state of a fresh install, and refusing to diagnose one
/// would break the command exactly where a user needs it.
async fn derive(context: &crate::app::Context) -> anyhow::Result<ShellStateReport> {
    let ocx_home = context.file_structure().root().to_path_buf();
    let ocx_home_present = read_ocx_home(&ocx_home).await?;

    // The carrier is untrusted input (C-007): decoding it names the revert set
    // and nothing else. Nothing below builds a path from it.
    let carrier = ocx_lib::env::var(CARRIER_KEY);
    let carrier_present = carrier.is_some();
    let carrier_bytes = carrier.as_deref().map_or(0, str::len);
    let ledger = carrier.as_deref().and_then(Ledger::decode);

    let hook = resolve_hook(context);
    let whitelist = effective_consent(context.config().shell.as_ref());

    // One consent read for the whole snapshot: two independent reads of the
    // very predicate this command exists to explain would be a gratuitous
    // divergence surface inside one report.
    let resolution = resolve_project(context, &whitelist).await;
    let project = resolution.project();
    let yielded_to = project
        .map(|project| coexistence::detect(&project.dir).observed)
        .unwrap_or_default();

    let mut notes = Vec::new();
    if let Resolution::Failed(detail) = &resolution {
        notes.push(Note::ProjectUnresolved { detail: detail.clone() });
    }
    if let Some(project) = project {
        // A-12's row is about the *CWD walk*. An explicit `--project`,
        // `OCX_PROJECT` or `--global` follows symlinks by design and skips no
        // candidate, so the walk-limb check is a precondition, not decoration.
        let env_project = ocx_lib::env::var("OCX_PROJECT");
        if walked_to_project(context.global(), context.project_path(), env_project.as_deref()) {
            notes.extend(symlinked_candidate_note(&project.config_path, &project.dir).await);
        }
        notes.extend(paths_grant_notes(&project.dir, &project.decision, &whitelist));
    }

    let inert_reason = inert_reason(&hook, &yielded_to, project, ledger.as_ref(), carrier_present);

    let paths = reconcile::watch_paths(
        context.file_structure(),
        project.map(|project| project.dir.as_path()),
        project.map(|project| project.key.as_str()),
        // The carrier's recorded list when the shell has one, so the report
        // shows the tiers the running shell actually watches — including a
        // `--config` overlay this process was not started with (A-13, A-33).
        ledger
            .as_ref()
            .map(|ledger| ledger.tiers.as_slice())
            .filter(|tiers| !tiers.is_empty()),
    );
    let watch_set = watch_set(&paths).await;
    // The reconciler's own fold, not a second one: `fingerprint` and the list it
    // folds are both `shell::reconcile`'s, so this comparison is the same
    // arithmetic the per-prompt path runs (A-13, C-019).
    let fingerprint_current = match ledger.as_ref().filter(|ledger| !ledger.fp.is_empty()) {
        Some(ledger) => {
            // One `stat` per member, so it goes to a blocking thread rather
            // than stalling the runtime — the same hop `self activate
            // --reconcile` makes, and the contract `reconcile::fingerprint`'s
            // own doc comment states.
            let watch = paths.clone();
            let dir = project.map(|project| project.dir.clone());
            let folded =
                tokio::task::spawn_blocking(move || reconcile::current_fingerprint(&watch, dir.as_deref())).await?;
            Some(ledger.fp == folded)
        }
        None => None,
    };
    let priors = ShellStateReport::priors_for(ledger.as_ref());

    Ok(ShellStateReport {
        ocx_home,
        ocx_home_present,
        carrier_present,
        carrier_bytes,
        ledger,
        fingerprint_current,
        watch_set,
        project_dir: project.map(|project| project.dir.clone()),
        project_key: project.map(|project| project.key.clone()),
        project_stamped: project.is_some_and(|project| project.stamped),
        priors,
        hook,
        yielded_to,
        inert_reason,
        notes,
    })
}

/// Read `$OCX_HOME`, returning whether it exists.
///
/// C-051's single non-zero path: a home that exists but cannot be read is
/// `IoError` (74). `NotFound` is not that — a fresh install has no home yet and
/// the diagnostic must still run.
async fn read_ocx_home(ocx_home: &Path) -> anyhow::Result<bool> {
    match tokio::fs::read_dir(ocx_home).await {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(ocx_lib::Error::InternalFile(ocx_home.to_path_buf(), e).into()),
    }
}

/// The resolved project, its consent verdict, and whether a usable stamp backs
/// it — everything the reason enumeration needs about the project tier.
struct ResolvedProject {
    /// The `ocx.toml` the precedence chain resolved.
    config_path: PathBuf,
    /// Its canonical directory (A-30), which is the project's identity.
    dir: PathBuf,
    /// `ReferenceManager::name_for_path` of `dir` — a lookup index, never the
    /// identity.
    key: String,
    /// Whether a usable stamp exists (A-25: an unusable stamp is an absent
    /// one).
    stamped: bool,
    /// The activation predicate's answer.
    decision: Decision,
}

/// What the project-tier resolution produced.
///
/// The three outcomes are kept apart deliberately: reporting *"no project
/// reachable"* for a directory whose `ocx.toml` exists but will not parse is a
/// falsehood emitted by the one command whose product is the explanation.
enum Resolution {
    /// No `ocx.toml` is reachable through the precedence chain.
    None,
    /// A project resolved, and consent was evaluated over it.
    Resolved(Box<ResolvedProject>),
    /// A project file was reachable but could not be resolved — an unparseable
    /// `ocx.toml`, a canonicalization failure, a join failure. The detail is
    /// rendered as a note; `log::debug!` alone would go to a stderr the hook
    /// discards (A-21).
    Failed(String),
}

impl Resolution {
    fn project(&self) -> Option<&ResolvedProject> {
        match self {
            Resolution::Resolved(project) => Some(project),
            Resolution::None | Resolution::Failed(_) => None,
        }
    }
}

/// Resolve the project tier and evaluate consent over it, without writing
/// anything (A-29).
async fn resolve_project(context: &crate::app::Context, whitelist: &ShellConsent) -> Resolution {
    let (config_path, lock_path) = match project_context::resolve_project_paths(context).await {
        Ok(paths) => paths,
        Err(ProjectContextError::NoProject { .. }) => return Resolution::None,
        Err(e) => return Resolution::Failed(format!("{e}")),
    };

    let canonical = config_path.clone();
    let dir = match tokio::task::spawn_blocking(move || consent::canonical_project_dir(&canonical)).await {
        Ok(Ok(dir)) => dir,
        Ok(Err(e)) => {
            return Resolution::Failed(format!("could not canonicalize '{}': {e}", config_path.display()));
        }
        Err(e) => return Resolution::Failed(format!("canonicalization task failed: {e}")),
    };

    let key = ReferenceManager::name_for_path(&dir);
    // `None` covers all three of absent, unreadable and unparseable — one
    // outcome, because all three leave the source-set predicate with nothing to
    // quantify over.
    let lock = ProjectLock::load(&lock_path).await.ok();
    let sources = lock.as_ref().map(consent::lock_sources);
    // Clause 2's evidence, off the same parsed lock: the store's own record of
    // which logical repository this host resolved each locked digest under.
    // Read here
    // rather than re-derived by the renderer, so this report explains the very
    // predicate `ocx self activate --reconcile` ran.
    let target = crate::conventions::platform_or_default(None);
    // One `read_dir` per locked tool, so it goes to a blocking thread rather
    // than stalling the runtime — the same hop `canonical_project_dir` makes
    // above, and the contract `verified_sources`' own doc comment states.
    let verified = match lock {
        Some(lock) => {
            let store = context.file_structure().packages.clone();
            match tokio::task::spawn_blocking(move || consent::verified_sources(&lock, &target, &store)).await {
                Ok(verified) => verified,
                // `Failed`, not a silent `None`: this command's product is the
                // explanation, and a debug log would go to a stderr the hook
                // discards (A-21).
                Err(e) => return Resolution::Failed(format!("recorded-origin read task failed: {e}")),
            }
        }
        None => None,
    };
    let stamp = consent::load(&key);
    let decision = consent::evaluate_with_stamp(&dir, stamp.as_ref(), sources.as_ref(), verified.as_ref(), whitelist);

    Resolution::Resolved(Box::new(ResolvedProject {
        config_path,
        dir,
        key,
        stamped: stamp.is_some(),
        decision,
    }))
}

/// Whether the CWD walk — rather than an explicit selector — decided the
/// project.
///
/// The precedence chain `ProjectConfig::resolve` applies is
/// `--global`/`OCX_GLOBAL` ▸ `--project` ▸ `OCX_PROJECT` ▸ CWD walk. Only the
/// last limb rejects symlinked candidates, so only it can have skipped one.
///
/// Takes the three limbs as arguments rather than reading them off `Context`
/// and the environment, so the precedence itself is testable without a
/// `Context` and without mutating the process environment from a test.
/// `OCX_PROJECT=""` is treated as unset, matching the loader (`OCX_CONFIG`'s
/// own escape hatch).
fn walked_to_project(global: bool, explicit_project: Option<&Path>, env_project: Option<&str>) -> bool {
    !global && explicit_project.is_none() && env_project.is_none_or(|value| value.is_empty())
}

/// Read C-038's ladder rather than re-deriving it.
///
/// `ocx shell state` declares no `--hook` / `--no-hook` pair, so rungs 1 and 2
/// are unreachable from here and the ladder answers from rung 3
/// (`OCX_NO_HOOK`), rung 4 (`[shell] hook`) or rung 5 (auto). `interactive` is
/// passed as `true` deliberately: rung 5 means "the shim's own probe decides at
/// shell start", which a diagnostic cannot observe, so the report says `auto`
/// rather than guessing. A-32 — the tier comes from the loader's runtime
/// provenance, so the answer is the tier that **actually** decided and never a
/// hard-coded "managed".
fn resolve_hook(context: &crate::app::Context) -> HookStatus {
    let shell_config = context.config().shell.as_ref();
    let configured = shell_config.and_then(|shell| shell.hook);
    let rung = Hook::default().rung(true, configured);
    let (rung, tier, enabled) = match rung {
        // Unreachable from this command's argv: it declares neither flag. Kept
        // as real arms rather than an `unreachable!()` so a future flag pair
        // cannot turn a contract into a panic.
        Rung::FlagOff => ("--no-hook", None, Some(false)),
        Rung::FlagOn => ("--hook", None, Some(true)),
        Rung::EnvOptOut => ("OCX_NO_HOOK", None, Some(false)),
        Rung::Configured => (
            "[shell] hook",
            shell_config
                .and_then(|shell| shell.hook_tier)
                .map(|tier| tier.to_string()),
            configured,
        ),
        Rung::Auto => ("auto", None, None),
    };
    HookStatus {
        rung: rung.to_owned(),
        tier,
        enabled,
    }
}

/// A-12 — the CWD walk skips a symlinked `ocx.toml` and continues upward. The
/// loader's `log::warn!` never reaches the prompt (the hook discards the
/// binary's stderr unconditionally, A-21), so this row is the user's only path
/// to that answer.
///
/// Observes rather than re-implements: it stats the candidates strictly between
/// the CWD and the project the walk actually settled on, and reports the first
/// symlinked one. Nothing here changes which project is resolved.
async fn symlinked_candidate_note(config_path: &Path, project_dir: &Path) -> Option<Note> {
    // An explicit `--project` / `OCX_PROJECT` follows symlinks by design, so
    // there is no skipped candidate to report when the walk did not run.
    let cwd = ocx_lib::env::current_dir().ok()?;
    let resolved_dir = config_path.parent()?;

    let mut current = cwd.as_path();
    loop {
        if current == resolved_dir || current == project_dir {
            return None;
        }
        let candidate = current.join(PROJECT_FILE);
        if let Ok(meta) = tokio::fs::symlink_metadata(&candidate).await
            && meta.file_type().is_symlink()
        {
            return Some(Note::SymlinkedCandidateSkipped {
                candidate,
                ancestor: project_dir.to_path_buf(),
            });
        }
        current = current.parent()?;
    }
}

/// A-26 and A-28 — the two `paths`-grant rows.
///
/// A-26: a matching entry activates unconditionally, and source-set drift is
/// **not** tracked for path grants, because nothing on the activation path
/// writes a stamp to drift against. A-28: an entry differing from the canonical
/// directory only by ASCII case does **not** grant — entries are compared as
/// literal bytes after separator and trailing-slash normalization — so the
/// near-miss earns a row of its own instead of a silent `Inert`.
fn paths_grant_notes(project_dir: &Path, decision: &Decision, whitelist: &ShellConsent) -> Vec<Note> {
    let canonical = normalize_consent_path(project_dir);
    // Advisory only: the near-miss arm below compares case-insensitively, and a
    // lossy render is the right operand for a *note*. The grant arm above never
    // goes near it — that comparison is the trust boundary and stays on the
    // path's own bytes.
    let canonical_lossy = canonical.to_string_lossy();
    let mut notes = Vec::new();
    for entry in &whitelist.paths {
        let normalized = normalize_consent_path(entry);
        if normalized == canonical {
            if matches!(decision, Decision::Activate(_)) {
                notes.push(Note::ActiveViaPathsGrant { entry: entry.clone() });
            }
        } else if normalized.to_string_lossy().eq_ignore_ascii_case(&canonical_lossy)
            && matches!(decision, Decision::Inert(_))
        {
            // Only when the project is actually inert. On an active project the
            // row reads as a contradiction next to `active: yes`, and A-28's
            // purpose is to explain a refusal, not to annotate a grant.
            notes.push(Note::PathsNearMiss {
                entry: entry.clone(),
                canonical: project_dir.to_path_buf(),
            });
        }
    }
    notes
}

/// The enumerated inertness reason, resolved by one ordered ladder so the
/// answer is deterministic (C-050).
///
/// Order, most-blocking first: a disabled hook means nothing reconciles at all;
/// a yield surrenders the project scope to another live tool; consent gates the
/// project tier; the over-cap marker names a scope the ledger abandoned; and a
/// carrier that carries no record is the last thing left to say. `None` — the
/// project is active — only when none of them fires.
fn inert_reason(
    hook: &HookStatus,
    yielded_to: &[coexistence::Observation],
    project: Option<&ResolvedProject>,
    ledger: Option<&Ledger>,
    carrier_present: bool,
) -> Option<Reason> {
    if hook.enabled == Some(false) {
        return Some(Reason::HookDisabled {
            rung: hook.rung.clone(),
            tier: hook.tier.clone(),
        });
    }

    if let Some(first) = yielded_to.first() {
        return Some(Reason::YieldedTo(first.clone()));
    }

    if let Some(project) = project
        && let Decision::Inert(reason) = &project.decision
    {
        return Some(reason.clone());
    }

    // A-01 — read from the marker the carrier still carries, never inferred
    // from an absent one. A named scope is reconciled exactly as an absent
    // scope; it is the one degradation that loses information rather than
    // repairing it.
    if let Some(scope) = ledger.and_then(|ledger| ledger.over_cap.first()) {
        return Some(Reason::LedgerOverCap { scope: *scope });
    }

    if ledger.is_none() {
        // C-006 — the two halves are different reasons, not one. An unset
        // carrier is the first prompt of a shell: nothing applied, nothing to
        // repair. A carrier that is present and will not decode is a corrupt
        // one: a scope was applied and its record is gone.
        return Some(Reason::LedgerUnreadable {
            first_prompt: !carrier_present,
        });
    }

    // A project resolved, consent said `Activate`, and the ledger decoded but
    // carries no record of the project scope. That is a **third** state, and it
    // is deliberately not a `Reason`: C-050 reason 6 enumerates exactly two
    // situations — an absent carrier and a corrupt one — and reusing the absent
    // half here would print "the carrier is unset" in the same report that has
    // already printed `present: yes` / `decoded: yes`. The renderer says
    // "not yet" from the ledger itself instead of inventing a reason.
    None
}

/// Render the fingerprint watch set (C-019, A-13), stat'd as it stands now.
///
/// Candidates, not survivors: a tier file that does not exist is recorded too,
/// because one becoming present is exactly the change the watch set must
/// notice. The member list itself comes from [`reconcile::watch_paths`] — the
/// reconciler's own definition — so this report can never disagree with the
/// fingerprint it prints beside it.
async fn watch_set(paths: &[PathBuf]) -> Vec<WatchMember> {
    let mut members = Vec::with_capacity(paths.len());
    for path in paths.iter().cloned() {
        let member = match tokio::fs::metadata(&path).await {
            Ok(meta) => WatchMember {
                present: true,
                size: Some(meta.len()),
                mtime: meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|since| since.as_secs()),
                path,
            },
            Err(_) => WatchMember {
                path,
                present: false,
                size: None,
                mtime: None,
            },
        };
        members.push(member);
    }
    members
}

#[cfg(test)]
mod tests {
    use ocx_lib::project::consent::Grant;
    use ocx_lib::shell::coexistence::{Observation, Tool};
    use ocx_lib::shell::reconcile::ScopeId;

    use super::*;

    fn auto_hook() -> HookStatus {
        HookStatus {
            rung: "auto".to_owned(),
            tier: None,
            enabled: None,
        }
    }

    fn direnv() -> Observation {
        Observation {
            tool: Tool::Direnv,
            signal: "DIRENV_DIR=/work/proj".to_owned(),
        }
    }

    /// C-050 — a disabled hook is the most-blocking reason and wins the ladder,
    /// naming the rung and the tier the ladder reported (A-32).
    #[test]
    fn c050_a032_hook_disabled_wins_the_reason_ladder() {
        let hook = HookStatus {
            rung: "[shell] hook".to_owned(),
            tier: Some("managed config".to_owned()),
            enabled: Some(false),
        };
        let reason = inert_reason(&hook, &[direnv()], None, None, false);
        assert_eq!(
            reason,
            Some(Reason::HookDisabled {
                rung: "[shell] hook".to_owned(),
                tier: Some("managed config".to_owned()),
            })
        );
    }

    /// C-050 reason 4 — a live yield outranks the ledger's own state.
    #[test]
    fn c050_yield_outranks_the_ledger_state() {
        let reason = inert_reason(&auto_hook(), &[direnv()], None, None, false);
        assert_eq!(reason, Some(Reason::YieldedTo(direnv())));
    }

    /// C-050 reason 5 + A-01 — the over-cap state is read from the marker on a
    /// ledger that still decodes, not inferred from an absent carrier.
    #[test]
    fn c050_a001_over_cap_comes_from_the_marker_not_from_absence() {
        let mut ledger = Ledger::empty();
        ledger.over_cap = vec![ScopeId::Project];
        let reason = inert_reason(&auto_hook(), &[], None, Some(&ledger), true);
        assert_eq!(
            reason,
            Some(Reason::LedgerOverCap {
                scope: ScopeId::Project
            })
        );

        // The same absent-project shape *without* the marker is the other
        // reason entirely — which is what "never inferred from an absent
        // carrier" means.
        let reason = inert_reason(&auto_hook(), &[], None, None, true);
        assert_eq!(reason, Some(Reason::LedgerUnreadable { first_prompt: false }));
    }

    /// C-050 reason 6 + C-006 — an unset carrier is the first prompt; a
    /// present-but-undecodable one is corrupt. Different reasons, not one.
    #[test]
    fn c050_c006_absent_and_corrupt_carriers_are_different_reasons() {
        assert_eq!(
            inert_reason(&auto_hook(), &[], None, None, false),
            Some(Reason::LedgerUnreadable { first_prompt: true })
        );
        assert_eq!(
            inert_reason(&auto_hook(), &[], None, None, true),
            Some(Reason::LedgerUnreadable { first_prompt: false })
        );
    }

    /// C-050 — with a decodable ledger, no yield, an enabled hook and no
    /// project, there is nothing left to report: the shell is active.
    #[test]
    fn c050_a_healthy_shell_has_no_inert_reason() {
        let ledger = Ledger::empty();
        assert_eq!(inert_reason(&auto_hook(), &[], None, Some(&ledger), true), None);
    }

    fn resolved(dir: &str, decision: Decision) -> ResolvedProject {
        ResolvedProject {
            config_path: PathBuf::from(dir).join(PROJECT_FILE),
            dir: PathBuf::from(dir),
            key: "0123456789abcdef".to_owned(),
            stamped: false,
            decision,
        }
    }

    /// SPEC-2 / C-006 — a consented project whose scope the **decoded** ledger
    /// does not record is not an inertness reason at all.
    ///
    /// It used to return `LedgerUnreadable { first_prompt: true }`, which made
    /// the report print "the carrier is unset" under `present: yes` /
    /// `decoded: yes`. C-050 reason 6 enumerates exactly two carrier
    /// situations; this is a third, and the renderer says `active: not yet`.
    #[test]
    fn c050_c006_a_decoded_ledger_without_the_project_scope_is_not_a_reason() {
        let project = resolved("/work/proj", Decision::Activate(Grant::Stamp));
        let ledger = Ledger::empty();
        assert!(ledger.scopes.project.is_none());
        assert_eq!(
            inert_reason(&auto_hook(), &[], Some(&project), Some(&ledger), true),
            None,
            "a decoded carrier must never be laundered through the absent-carrier reason"
        );
    }

    /// C-051 — the only non-zero exit path, both branches.
    ///
    /// `NotFound` is an ordinary fresh install and must report normally; every
    /// other read failure is `IoError` (74). Reading a *file* as a directory is
    /// the portable way to produce the second branch without a chmod.
    #[tokio::test]
    async fn c051_ocx_home_absent_is_zero_and_unreadable_is_74() {
        let temp = tempfile::tempdir().expect("tempdir");

        let absent = temp.path().join("no-such-home");
        assert!(
            !read_ocx_home(&absent).await.expect("an absent home is not an error"),
            "an absent $OCX_HOME must report as absent, never fail"
        );

        let not_a_dir = temp.path().join("home-is-a-file");
        std::fs::write(&not_a_dir, b"").expect("write");
        let err = read_ocx_home(&not_a_dir)
            .await
            .expect_err("reading a file as $OCX_HOME must fail");
        assert_eq!(
            ocx_lib::cli::classify_error(err.as_ref()),
            ocx_lib::cli::ExitCode::IoError,
            "an unreadable $OCX_HOME is the command's only non-zero path, and it is 74"
        );
    }

    /// SPEC-4 / A-12 — the symlinked-candidate row is about the **CWD walk**.
    ///
    /// Every explicit limb (`--global`, `--project`, `OCX_PROJECT`) follows
    /// symlinks by design and therefore skips no candidate; emitting the row
    /// then names an ancestor the walk never chose, which is worse than
    /// printing nothing. Only an unset-or-empty `OCX_PROJECT` with neither flag
    /// leaves the walk as the deciding limb.
    #[test]
    fn a012_the_symlink_row_is_gated_on_the_cwd_walk_limb() {
        let explicit = Path::new("/elsewhere/ocx.toml");
        for (global, project, env_project, expected) in [
            (false, None, None, true),
            (false, None, Some(""), true),
            (true, None, None, false),
            (false, Some(explicit), None, false),
            (false, None, Some("/elsewhere/ocx.toml"), false),
            (true, Some(explicit), Some("/elsewhere/ocx.toml"), false),
        ] {
            assert_eq!(
                walked_to_project(global, project, env_project),
                expected,
                "global={global} project={project:?} OCX_PROJECT={env_project:?}"
            );
        }
    }

    /// QUAL-4 — the near-miss row is an inertness diagnostic: it must not
    /// appear next to `active: yes`.
    #[test]
    fn a028_qual4_the_near_miss_row_is_suppressed_on_an_active_project() {
        let whitelist = ShellConsent {
            paths: vec![PathBuf::from("/Users/u/Repo")],
            namespaces: None,
        };
        assert!(
            paths_grant_notes(Path::new("/Users/u/repo"), &Decision::Activate(Grant::Path), &whitelist).is_empty(),
            "an active project must not carry a `does not grant` row"
        );
    }

    /// A-28 — a case-only difference does not grant, and earns a near-miss row.
    /// A-26 — an exact match grants, and the row says drift is not tracked.
    #[test]
    fn a026_a028_paths_grant_and_near_miss_rows() {
        let project_dir = Path::new("/Users/u/repo");

        let near = ShellConsent {
            paths: vec![PathBuf::from("/Users/u/Repo")],
            namespaces: None,
        };
        assert_eq!(
            paths_grant_notes(project_dir, &Decision::Inert(Reason::LockUnavailable), &near),
            vec![Note::PathsNearMiss {
                entry: PathBuf::from("/Users/u/Repo"),
                canonical: PathBuf::from("/Users/u/repo"),
            }]
        );

        let exact = ShellConsent {
            paths: vec![PathBuf::from("/Users/u/repo/")],
            namespaces: None,
        };
        assert_eq!(
            paths_grant_notes(project_dir, &Decision::Activate(Grant::Path), &exact),
            vec![Note::ActiveViaPathsGrant {
                entry: PathBuf::from("/Users/u/repo/"),
            }]
        );
    }
}
