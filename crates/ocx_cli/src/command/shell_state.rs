// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx shell state` — read-only diagnostics for the shell integration.
//!
//! Never calls `consent::record`, directly or via `load_project_with_lock_consenting`: that would
//! consent to the project being diagnosed.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use ocx_config::shell::ShellConsent;
use ocx_config::shell::consent_entry_defect;
use ocx_config::shell::consent_path_matches;
use ocx_config::shell::effective_consent;
use ocx_package_manager::activation::{self, ProjectIdentity};
use ocx_project::consent::{Decision, Reason};
use ocx_shell::shell::coexistence;
use ocx_shell::shell::reconcile::{self, CARRIER_KEY, Ledger};

use crate::api::data::shell_state::{HookStatus, Note, ShellStateReport, VerboseShellState, WatchMember};
use crate::app::project_context::{self, ProjectContextError};
use crate::options::hook::{Hook, Rung};

/// The project file name the CWD walk looks for.
const PROJECT_FILE: &str = "ocx.toml";

/// Report the shell integration's state, and why it is inert when it is.
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
        // `--verbose` picks a plain rendering, never a different payload.
        if self.verbose {
            context.api().report(&VerboseShellState(report))?;
        } else {
            context.api().report(&report)?;
        }
        // Exit 0 in every reportable state: an inert, corrupt, over-cap or yielded shell is a finding, not a failure.
        Ok(ExitCode::SUCCESS)
    }
}

/// Build the report.
///
/// # Errors
///
/// [`ocx_util::error::FileError`] (exit 74) when `$OCX_HOME` exists but cannot be read; an absent home is a
/// fresh install, not an error.
async fn derive(context: &crate::app::Context) -> anyhow::Result<ShellStateReport> {
    let ocx_home = context.file_structure().root().to_path_buf();
    let ocx_home_present = read_ocx_home(&ocx_home).await?;
    let shell_integration_installed = shell_integration_installed(&ocx_home).await;

    // The carrier is untrusted input: nothing below builds a path from it.
    let carrier = ocx_util::env::var(CARRIER_KEY);
    let carrier_present = carrier.is_some();
    let carrier_bytes = carrier.as_deref().map_or(0, str::len);
    let ledger = carrier.as_deref().and_then(Ledger::decode);

    let hook = resolve_hook(context);
    let whitelist = effective_consent(context.config().shell.as_ref());

    // One consent read for the whole snapshot, or the report can disagree with itself.
    let resolution = resolve_project(context, &whitelist).await;
    let project = resolution.project();
    let yielded_to = project
        .map(|project| coexistence::detect(&project.identity.dir).observed)
        .unwrap_or_default();

    let mut notes = Vec::new();
    if let Resolution::Failed(detail) = &resolution {
        notes.push(Note::ProjectUnresolved { detail: detail.clone() });
    }
    if let Some(project) = project {
        // Only the CWD walk skips symlinked candidates; an explicit selector follows them.
        let env_project = ocx_util::env::var("OCX_PROJECT");
        if walked_to_project(context.global(), context.project_path(), env_project.as_deref()) {
            notes.extend(symlinked_candidate_note(&project.identity.config_path, &project.identity.dir).await);
        }
        notes.extend(paths_grant_notes(&project.identity.dir, &project.decision, &whitelist));
    }

    let inert_reason = inert_reason(&hook, &yielded_to, project, ledger.as_ref(), carrier_present);

    let paths = reconcile::watch_paths(
        context.file_structure(),
        project.map(|project| project.identity.config_path.as_path()),
        project.map(|project| project.identity.key.as_str()),
        // The carrier's recorded tiers when present, including a `--config` overlay this process lacks.
        ledger
            .as_ref()
            .map(|ledger| ledger.tiers.as_slice())
            .filter(|tiers| !tiers.is_empty()),
    );
    let watch_set = watch_set(&paths).await;
    // The reconciler's own fold, so this compares with the per-prompt path's arithmetic.
    let fingerprint_current = match ledger.as_ref().filter(|ledger| !ledger.fp.is_empty()) {
        Some(ledger) => {
            // One `stat` per member, so off the runtime.
            let watch = paths.clone();
            let dir = project.map(|project| project.identity.dir.clone());
            let folded =
                tokio::task::spawn_blocking(move || reconcile::current_fingerprint(&watch, dir.as_deref())).await?;
            Some(ledger.fp == folded)
        }
        None => None,
    };
    let priors = ShellStateReport::priors_for(ledger.as_ref());
    let toolchain = toolchain_state(context, project).await?;
    notes.extend(toolchain.note);

    Ok(ShellStateReport {
        ocx_home,
        ocx_home_present,
        shell_integration_installed,
        toolchain_home: toolchain.home,
        toolchain_bin: toolchain.bin,
        activate: toolchain.activate,
        pinned: toolchain.pinned,
        lock_refusal: project.and_then(|project| project.lock_refusal.clone()),
        carrier_present,
        carrier_bytes,
        ledger,
        fingerprint_current,
        watch_set,
        project_dir: project.map(|project| project.identity.dir.clone()),
        project_key: project.map(|project| project.identity.key.clone()),
        project_stamped: project.is_some_and(|project| project.stamped),
        grant: project.and_then(|project| match &project.decision {
            Decision::Activate(grant) => Some(*grant),
            Decision::Inert(_) => None,
        }),
        stamp_written_at: project.and_then(|project| project.stamp_written_at.clone()),
        priors,
        hook,
        yielded_to,
        inert_reason,
        notes,
    })
}

/// The toolchain facts the report adds, plus the note an unparseable manifest owes.
struct ToolchainState {
    /// The resolved home — see [`ShellStateReport::toolchain_home`].
    home: PathBuf,
    /// The PATH-facing trampoline directory — see [`ShellStateReport::toolchain_bin`].
    bin: PathBuf,
    /// The effective `activate` mode, past the whole ladder.
    activate: ocx_project::activate::ActivateMode,
    /// The effective `pinned` value, past the same ladder.
    pinned: bool,
    /// [`Note::ToolchainManifestUnparsed`] when the scope's `ocx.toml` exists but will not parse.
    note: Option<Note>,
}

/// Resolve the home and the two effective toolchain settings for the scope in effect.
///
/// # Errors
///
/// The home's canonicalisation failure (exit 74).
async fn toolchain_state(
    context: &crate::app::Context,
    project: Option<&ResolvedProject>,
) -> anyhow::Result<ToolchainState> {
    let (scope, config_path) = scope_and_config_path(
        context.file_structure().root(),
        project.map(|project| &project.identity),
        context.global(),
    );
    let home = context.manager().toolchain_home(&scope, context.toolchain_root())?;
    let (activate, pinned, note) = toolchain_settings(&config_path).await;

    Ok(ToolchainState {
        home: home.root().to_path_buf(),
        // The home's accessor, never `home.join("bin")`, or the report drifts from every PATH route.
        bin: home.bin(),
        activate,
        pinned,
        note,
    })
}

/// The scope in effect and the `ocx.toml` answering for it, minted together so they name one tier.
///
/// A project-free shell reads `$OCX_HOME/ocx.toml`, as the per-prompt hook does, never no file at all.
fn scope_and_config_path(
    ocx_home: &Path,
    project: Option<&ProjectIdentity>,
    global: bool,
) -> (ocx_store::file_structure::RenderStampScope, PathBuf) {
    use ocx_store::file_structure::RenderStampScope;

    match project {
        Some(project) if !global => (
            RenderStampScope::Project(project.dir.clone()),
            project.config_path.clone(),
        ),
        Some(_) | None => (
            RenderStampScope::Global,
            ocx_project::ProjectConfig::global_manifest_path(ocx_home),
        ),
    }
}

/// The two effective toolchain settings for one `file` tier, through the calls the per-prompt hook makes.
///
/// An unparseable manifest leaves the tier absent with a note, not an error, keeping exit 0 in every state.
async fn toolchain_settings(config_path: &Path) -> (ocx_project::activate::ActivateMode, bool, Option<Note>) {
    let (config, note) = match ocx_project::ProjectConfig::from_path(config_path).await {
        Ok(config) => (config, None),
        Err(error) if is_absent(&error) => (ocx_project::ProjectConfig::default(), None),
        Err(error) => {
            // The hook discards this stderr, so the note is where a typo'd `activate` gets reported.
            log::debug!("the toolchain manifest did not parse, so its settings are absent: {error}");
            (
                ocx_project::ProjectConfig::default(),
                Some(Note::ToolchainManifestUnparsed {
                    manifest: config_path.to_path_buf(),
                    detail: error.to_string(),
                }),
            )
        }
    };

    (
        activation::activate_mode(&config),
        ocx_package_manager::pinned_for_project(None, &config),
        note,
    )
}

/// Whether a project-tier read failed because the file is not there, the commonest benign state.
fn is_absent(error: &ocx_project::Error) -> bool {
    use ocx_project::error::ProjectErrorKind;

    // Exhaustive, never `_ => false`: a new variant that could mean "absent" must be classified here.
    match error {
        ocx_project::Error::Project(error) => {
            matches!(&error.kind, ProjectErrorKind::Io(io) if io.kind() == std::io::ErrorKind::NotFound)
        }
        // Unreachable here: the read raises only `Project(Io(..))`.
        ocx_project::Error::OciClient(_)
        | ocx_project::Error::OciIndex(_)
        | ocx_project::Error::Config(_)
        | ocx_project::Error::InternalFile(_, _) => false,
    }
}

/// Whether `ocx self setup` wired this machine's shell, probed via the env shim every profile fence sources.
///
/// An I/O error counts as installed: an unreadable home already surfaced as exit 74.
async fn shell_integration_installed(ocx_home: &Path) -> bool {
    tokio::fs::try_exists(ocx_home.join(ocx_setup::shims::WITNESS_SHIM))
        .await
        .unwrap_or(true)
}

/// Read `$OCX_HOME`, returning whether it exists; absent is a fresh install, not an error.
async fn read_ocx_home(ocx_home: &Path) -> anyhow::Result<bool> {
    match tokio::fs::read_dir(ocx_home).await {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(ocx_util::error::FileError::new(ocx_home, e).into()),
    }
}

/// The resolved project, its consent verdict, and whether a usable stamp backs it.
struct ResolvedProject {
    /// The `ocx.toml`, its canonical directory and the lookup key.
    identity: ProjectIdentity,
    /// Whether a usable stamp exists (an unusable stamp counts as absent).
    stamped: bool,
    /// The instant that stamp records, read off the stamp, never `stat`'d.
    stamp_written_at: Option<String>,
    /// The activation predicate's answer.
    decision: Decision,
    /// Why this project's `ocx.lock` refuses composition, as `compose` reports it.
    lock_refusal: Option<String>,
}

/// What the project-tier resolution produced; an unparseable `ocx.toml` is `Failed`, never "no project".
enum Resolution {
    /// No `ocx.toml` is reachable through the precedence chain.
    None,
    /// A project resolved, and consent was evaluated over it.
    Resolved(Box<ResolvedProject>),
    /// A project file was reachable but could not be resolved; the detail is rendered as a note.
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

/// Resolve the project over the full precedence chain and evaluate consent through the prompt's own
/// [`activation::evaluate_consent`]; writes nothing.
async fn resolve_project(context: &crate::app::Context, whitelist: &ShellConsent) -> Resolution {
    let (config_path, _) = match project_context::resolve_project_paths(context, None).await {
        Ok(paths) => paths,
        Err(ProjectContextError::NoProject { .. } | ProjectContextError::NoProjectIn { .. }) => {
            return Resolution::None;
        }
        Err(e) => return Resolution::Failed(format!("{e}")),
    };

    let identity = match ProjectIdentity::resolve(config_path).await {
        Ok(identity) => identity,
        Err(e) => return Resolution::Failed(format!("{e}")),
    };
    let target = crate::conventions::platform_or_default(None);
    let evaluated =
        activation::evaluate_consent(whitelist, &target, &context.file_structure().packages, &identity).await;

    Resolution::Resolved(Box::new(ResolvedProject {
        stamped: evaluated.stamped(),
        stamp_written_at: evaluated.stamp().map(|stamp| stamp.stamped_at.clone()),
        decision: evaluated.decision().clone(),
        lock_refusal: lock_refusal(&identity, evaluated.lock()).await,
        identity,
    }))
}

/// The refusal `compose` would return, in its words, asked of the lock consent decided on: a second read can differ.
///
/// An unparseable `ocx.toml` yields `None`: staleness against an uncomputable hash would be a guess.
async fn lock_refusal(project: &ProjectIdentity, lock: Option<&ocx_project::ProjectLock>) -> Option<String> {
    let lock_path = ocx_project::lock::lock_path_for(&project.config_path);
    let Some(lock) = lock else {
        return Some(ocx_project::LockCurrency::Missing { path: lock_path }.to_string());
    };
    let config = ocx_project::ProjectConfig::from_path(&project.config_path).await.ok()?;
    (!lock.is_current(&config)).then(|| ocx_project::LockCurrency::Stale { lock_path }.to_string())
}

/// Whether the CWD walk, not an explicit selector, decided the project; `OCX_PROJECT=""` counts as unset.
fn walked_to_project(global: bool, explicit_project: Option<&Path>, env_project: Option<&str>) -> bool {
    !global && explicit_project.is_none() && env_project.is_none_or(|value| value.is_empty())
}

/// Read the shared hook ladder; `interactive` is `true` so the auto rung reports `auto` instead of guessing.
fn resolve_hook(context: &crate::app::Context) -> HookStatus {
    let shell_config = context.config().shell.as_ref();
    let configured = shell_config.and_then(|shell| shell.hook);
    let rung = Hook::default().rung(true, configured);
    let (rung, tier, enabled) = match rung {
        // Unreachable today, but real arms, so a future flag pair cannot turn them into a panic.
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

/// The first symlinked `ocx.toml` the CWD walk skipped below the resolved project.
///
/// The hook discards the loader's warning, so this row is the user's only view of the skip.
async fn symlinked_candidate_note(config_path: &Path, project_dir: &Path) -> Option<Note> {
    let cwd = ocx_util::env::current_dir().ok()?;
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

/// The `paths`-grant rows: an active grant, a malformed entry, or an ASCII-case near miss on an inert project.
///
/// A malformed entry is checked first and not gated on `decision`, so no accidental match hides the defect.
fn paths_grant_notes(project_dir: &Path, decision: &Decision, whitelist: &ShellConsent) -> Vec<Note> {
    let mut notes = Vec::new();
    for entry in &whitelist.paths {
        if let Some(defect) = consent_entry_defect(entry) {
            notes.push(Note::PathsDefect {
                entry: entry.clone(),
                defect: defect.to_string(),
            });
        } else if consent_path_matches(entry, project_dir) {
            if matches!(decision, Decision::Activate(_)) {
                notes.push(Note::ActiveViaPathsGrant { entry: entry.clone() });
            }
        } else if ascii_case_near_miss(entry, project_dir) && matches!(decision, Decision::Inert(_)) {
            // Only when inert: next to `active: yes` the row reads as a contradiction.
            notes.push(Note::PathsNearMiss {
                entry: entry.clone(),
                canonical: project_dir.to_path_buf(),
            });
        }
    }
    notes
}

/// Whether `entry` would grant `project_dir` under ASCII case folding; advisory only.
///
/// The grant itself is a trust boundary and must keep comparing the path's own bytes.
fn ascii_case_near_miss(entry: &Path, project_dir: &Path) -> bool {
    let folded = |path: &Path| PathBuf::from(path.to_string_lossy().to_ascii_lowercase());
    consent_path_matches(&folded(entry), &folded(project_dir))
}

/// The inertness reason by one ordered ladder, most-blocking first: hook, yield, consent, over-cap, carrier.
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

    // Read from the marker the carrier carries, never inferred from an absent scope.
    if let Some(scope) = ledger.and_then(|ledger| ledger.over_cap.first()) {
        return Some(Reason::LedgerOverCap { scope: *scope });
    }

    if ledger.is_none() {
        // An unset carrier is a first prompt; a present one that will not decode is corrupt.
        return Some(Reason::LedgerUnreadable {
            first_prompt: !carrier_present,
        });
    }

    // Active, decoded, no project scope yet: not a `Reason`; the renderer says "not yet".
    None
}

/// Render the fingerprint watch set as it stands now, absent candidates included: one appearing is a change.
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
    use ocx_project::consent::Grant;
    use ocx_shell::shell::coexistence::{Observation, Tool};
    use ocx_shell::shell::reconcile::ScopeId;

    use super::*;

    /// A POSIX-spelled fixture, as this platform actually spells an absolute
    /// path.
    ///
    /// Every `paths` fixture below stands in for a **canonical** project
    /// directory or for an entry meant to match one, and a canonical directory
    /// comes out of `dunce::canonicalize` — so on Windows it always carries a
    /// drive prefix. A driveless `/w/acme` is therefore `RelativePath`-defective
    /// there, not a well-formed entry, and a fixture that skips the prefix
    /// exercises the defect path instead of the row under test. Case is
    /// preserved: the case-fold tests below turn on it.
    fn abs(posix: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{}", posix.replace('/', "\\")))
        } else {
            PathBuf::from(posix)
        }
    }

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

    /// The reason this
    /// command reports and the verdict `ocx self activate --reconcile` gates on
    /// are two **readings of one call**, not two derivations.
    ///
    /// The whole point of the issue: `ocx shell state` used to re-derive the
    /// activation predicate to explain it, and the two copies had already
    /// drifted once — this command read the two `OCX_CONSENT_*` variables as
    /// string literals where `activate.rs` read them through the constants, in
    /// a function whose doc comment claimed they "can never disagree". Below,
    /// [`activation::evaluate_consent`] is called **once** and both consumers read
    /// its result: [`ConsentProof::of`] is the activation gate, `inert_reason`
    /// is this command's product. The assertion is that the sentence printed to
    /// the user names the very refusal the prompt acted on.
    ///
    /// The structural half is not assertable at runtime and does not need to
    /// be: `ProjectConsent`'s fields are private and it has no public
    /// constructor, so neither consumer can be handed a `Decision` that
    /// `evaluate_consent` did not produce.
    ///
    /// Red state: give `resolve_project` its own `evaluate_with_stamp` call
    /// with any operand that differs from the shared one — an empty whitelist,
    /// a stamp read under a different key — and `reported` stops being the
    /// gate's own reason.
    #[tokio::test]
    async fn c050_343_the_reported_reason_is_the_one_the_activation_gate_refused_on() {
        use ocx_package_manager::activation::ConsentProof;
        use ocx_store::file_structure::PackageStore;

        // No stamp, no lock, no grant — the ordinary first-encounter refusal,
        // and the one state both commands have to agree about.
        let home = tempfile::tempdir().expect("tempdir");
        let project = home.path().join("proj");
        std::fs::create_dir_all(&project).expect("project dir");
        let config_path = project.join(PROJECT_FILE);
        std::fs::write(&config_path, "").expect("ocx.toml");
        let identity = ProjectIdentity::resolve(config_path)
            .await
            .expect("resolve the fixture");
        let whitelist = ShellConsent::default();
        let target = "linux/amd64".parse().expect("valid host platform");

        // ONE call. Everything below reads it; nothing below re-derives it.
        let evaluated =
            activation::evaluate_consent(&whitelist, &target, &PackageStore::new(home.path()), &identity).await;

        let gate = ConsentProof::of(evaluated.decision());
        let Decision::Inert(refusal) = evaluated.decision().clone() else {
            panic!("the fixture must be refused, or the agreement below is vacuous");
        };
        assert!(
            gate.is_none(),
            "a refusal must not mint the proof `session` composes behind"
        );

        let resolved = ResolvedProject {
            identity,
            stamped: evaluated.stamped(),
            stamp_written_at: evaluated.stamp().map(|stamp| stamp.stamped_at.clone()),
            decision: evaluated.decision().clone(),
            lock_refusal: None,
        };
        assert_eq!(
            inert_reason(&auto_hook(), &[], Some(&resolved), Some(&Ledger::empty()), true),
            Some(refusal),
            "the reason this command prints must be the refusal the prompt actually acted on"
        );
    }

    /// A disabled hook is the most-blocking reason and wins the ladder,
    /// naming the rung and the tier the ladder reported.
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

    /// A live yield outranks the ledger's own state.
    #[test]
    fn c050_yield_outranks_the_ledger_state() {
        let reason = inert_reason(&auto_hook(), &[direnv()], None, None, false);
        assert_eq!(reason, Some(Reason::YieldedTo(direnv())));
    }

    /// The over-cap state is read from the marker on a
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

    /// An unset carrier is the first prompt; a
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

    /// With a decodable ledger, no yield, an enabled hook and no
    /// project, there is nothing left to report: the shell is active.
    #[test]
    fn c050_a_healthy_shell_has_no_inert_reason() {
        let ledger = Ledger::empty();
        assert_eq!(inert_reason(&auto_hook(), &[], None, Some(&ledger), true), None);
    }

    fn resolved(dir: &str, decision: Decision) -> ResolvedProject {
        ResolvedProject {
            identity: ProjectIdentity {
                config_path: PathBuf::from(dir).join(PROJECT_FILE),
                dir: PathBuf::from(dir),
                key: "0123456789abcdef".to_owned(),
            },
            stamped: false,
            stamp_written_at: None,
            decision,
            lock_refusal: None,
        }
    }

    /// A consented project whose scope the **decoded** ledger
    /// does not record is not an inertness reason at all.
    ///
    /// It used to return `LedgerUnreadable { first_prompt: true }`, which made
    /// the report print "the carrier is unset" under `present: yes` /
    /// `decoded: yes`. The reason ladder enumerates exactly two carrier
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

    /// The only non-zero exit path, both branches.
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
            crate::exit::classify_library_error(err.as_ref()),
            ocx_exit::ExitCode::IoError,
            "an unreadable $OCX_HOME is the command's only non-zero path, and it is 74"
        );
    }

    /// Finding 97 — all three answers of the lock probe, on a project the test
    /// owns.
    ///
    /// The two refusing states are the ones the prompt hits and swallows: an
    /// absent lock (reachable because a `paths` grant is the one consent clause
    /// that holds without one) and a stale lock (`ocx.toml` edited, `ocx lock`
    /// forgotten). The third — a lock that composes — is what keeps the other
    /// two from being a constant.
    ///
    /// Red state: return `None` unconditionally, or drop the `is_current` call so
    /// only absence refuses; either reds one of the three below.
    ///
    /// EC-REC-008 — the probe half of the lock-refusal split.
    #[tokio::test]
    async fn f097_the_lock_probe_answers_absent_stale_and_current() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().to_path_buf();
        let config_path = dir.join(PROJECT_FILE);
        std::fs::write(&config_path, "[tools]\n").expect("write ocx.toml");
        let identity = ProjectIdentity {
            config_path: config_path.clone(),
            dir,
            key: "0123456789abcdef".to_owned(),
        };

        let absent = lock_refusal(&identity, None).await;
        assert!(
            absent.as_deref().is_some_and(|text| text.contains("not found")),
            "an absent lock refuses composition, and a paths grant makes that reachable: {absent:?}"
        );

        let mut lock = ocx_project::ProjectLock::from_toml_str(
            "[metadata]\nlock_version = 3\ndeclaration_hash_version = 1\n\
             declaration_hash = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\n\
             generated_by = \"ocx 0.5.8\"\ngenerated_at = \"2026-08-27T00:00:00Z\"\n",
        )
        .expect("parse lock");

        let stale = lock_refusal(&identity, Some(&lock)).await;
        assert!(
            stale.as_deref().is_some_and(|text| text.contains("stale")),
            "a lock recording a hash the config no longer has is stale: {stale:?}"
        );

        let config = ocx_project::ProjectConfig::from_path(&config_path)
            .await
            .expect("parse ocx.toml");
        lock.metadata.declaration_hash = config.declaration_hash_cached().to_owned();
        assert_eq!(
            lock_refusal(&identity, Some(&lock)).await,
            None,
            "a lock that still describes its ocx.toml composes, and must not be reported as a refusal"
        );
    }

    /// A fresh hash does not make an entry naming another repository current:
    /// `compose` refuses it, so the probe must too.
    #[tokio::test]
    async fn the_lock_probe_refuses_an_entry_naming_another_repository() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().to_path_buf();
        let config_path = dir.join(PROJECT_FILE);
        std::fs::write(&config_path, "[tools]\ncmake = \"ocx.sh/cmake:3.28\"\n").expect("write ocx.toml");
        let identity = ProjectIdentity {
            config_path: config_path.clone(),
            dir,
            key: "0123456789abcdef".to_owned(),
        };
        let config = ocx_project::ProjectConfig::from_path(&config_path)
            .await
            .expect("parse ocx.toml");
        let lock = ocx_project::ProjectLock::from_toml_str(&format!(
            "[metadata]\nlock_version = 3\ndeclaration_hash_version = 1\n\
             declaration_hash = \"{hash}\"\n\
             generated_by = \"ocx 0.5.8\"\ngenerated_at = \"2026-08-27T00:00:00Z\"\n\n\
             [[tool]]\nname = \"cmake\"\ngroup = \"default\"\nrepository = \"ghcr.io/cmake\"\n\n\
             [tool.platforms]\n\"linux/amd64\" = \"sha256:{leaf}\"\n",
            hash = config.declaration_hash_cached(),
            leaf = "1".repeat(64),
        ))
        .expect("parse lock");

        let refusal = lock_refusal(&identity, Some(&lock)).await;
        assert!(
            refusal.as_deref().is_some_and(|text| text.contains("run `ocx lock`")),
            "a lock entry naming another repository refuses composition: {refusal:?}"
        );
    }

    /// Finding 90 — the probe that makes the "setup has not run" arm reachable.
    ///
    /// Both states on a directory the test owns, because a probe that only ever
    /// returns one answer is indistinguishable from one that never ran: absent
    /// shim is the bare-binary install the finding is about, present shim is
    /// the ordinary one.
    ///
    /// Red state: return a constant from `shell_integration_installed`, or drop
    /// the `join(WITNESS_SHIM)` so it probes the home itself — the home exists
    /// in both halves below, so that mutation reds the first assertion.
    ///
    /// EC-REC-007 — the probe half of the setup-never-run split.
    #[tokio::test]
    async fn f090_the_setup_probe_answers_both_ways_on_a_home_the_test_owns() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("ocx-home");
        std::fs::create_dir_all(&home).expect("mkdir home");

        assert!(
            !shell_integration_installed(&home).await,
            "a home with no env shim is a bare-binary install: setup has not run"
        );

        std::fs::write(home.join(ocx_setup::shims::WITNESS_SHIM), b"# shim\n").expect("write shim");
        assert!(
            shell_integration_installed(&home).await,
            "the shim `ocx self setup` writes is the witness that it ran"
        );
    }

    /// The symlinked-candidate row is about the **CWD walk**.
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

    /// The near-miss row is an inertness diagnostic: it must not
    /// appear next to `active: yes`.
    #[test]
    fn a028_qual4_the_near_miss_row_is_suppressed_on_an_active_project() {
        let entry = abs("/Users/u/Repo");
        let whitelist = ShellConsent {
            paths: vec![entry.clone()],
            namespaces: None,
        };
        let notes = paths_grant_notes(&abs("/Users/u/repo"), &Decision::Activate(Grant::Path), &whitelist);
        assert!(
            !notes.iter().any(|note| matches!(note, Note::PathsNearMiss { .. })),
            "an active project must not carry a `does not grant` row: {notes:?}"
        );
        // On Windows the case fold is part of the match, so this entry grants
        // outright and earns the ordinary exact-match row. The invariant here is
        // about the near-miss specifically, which is why the assert above names
        // it rather than demanding an empty list.
        let expected = if cfg!(windows) {
            vec![Note::ActiveViaPathsGrant { entry }]
        } else {
            vec![]
        };
        assert_eq!(notes, expected);
    }

    /// A case-only difference does not grant, and earns a near-miss row.
    /// An exact match grants, and the row says drift is not tracked.
    #[test]
    fn a026_a028_paths_grant_and_near_miss_rows() {
        let project_dir = abs("/Users/u/repo");
        let project_dir = project_dir.as_path();

        let near = ShellConsent {
            paths: vec![abs("/Users/u/Repo")],
            namespaces: None,
        };
        // On Windows the case fold is part of the match, so this entry grants
        // outright — and the grant row is gated on an `Activate` decision, so
        // an inert project earns no row at all. The near-miss describes a
        // refusal only a case-sensitive filesystem can produce.
        let expected = if cfg!(windows) {
            vec![]
        } else {
            vec![Note::PathsNearMiss {
                entry: abs("/Users/u/Repo"),
                canonical: abs("/Users/u/repo"),
            }]
        };
        assert_eq!(
            paths_grant_notes(project_dir, &Decision::Inert(Reason::LockUnavailable), &near),
            expected
        );

        let exact = ShellConsent {
            paths: vec![abs("/Users/u/repo/")],
            namespaces: None,
        };
        assert_eq!(
            paths_grant_notes(project_dir, &Decision::Activate(Grant::Path), &exact),
            vec![Note::ActiveViaPathsGrant {
                entry: abs("/Users/u/repo/"),
            }]
        );
    }

    /// A subtree entry (`/*`) grants a project beneath it, exactly
    /// once.
    ///
    /// Red state: in `paths_grant_notes`, change the grant check from
    /// `consent_path_matches(entry, project_dir)` to `entry == project_dir` —
    /// the subtree entry no longer covers `/Users/u/repo/tools`, and this
    /// test reds on an empty note list.
    #[test]
    fn a026_subtree_entry_grants_a_project_beneath_it() {
        let whitelist = ShellConsent {
            paths: vec![abs("/Users/u/repo/*")],
            namespaces: None,
        };
        assert_eq!(
            paths_grant_notes(
                &abs("/Users/u/repo/tools"),
                &Decision::Activate(Grant::Path),
                &whitelist
            ),
            vec![Note::ActiveViaPathsGrant {
                entry: abs("/Users/u/repo/*"),
            }]
        );
    }

    /// A subtree entry that would grant only if ASCII case were
    /// folded earns exactly one near-miss row on an inert project, the same
    /// as the exact-entry case.
    ///
    /// Red state: in `ascii_case_near_miss`, replace the `consent_path_matches`
    /// call with a bytewise `folded(entry) == folded(project_dir)` check —
    /// that drops the subtree arm, the case-differing subtree entry no
    /// longer near-misses, and this test reds on an empty note list.
    #[test]
    fn a028_subtree_entry_ascii_case_near_miss() {
        let whitelist = ShellConsent {
            paths: vec![abs("/Users/ACME/*")],
            namespaces: None,
        };
        // Windows: same as the exact case above — the subtree entry folds and
        // grants, so there is no near miss left to report.
        let expected = if cfg!(windows) {
            vec![]
        } else {
            vec![Note::PathsNearMiss {
                entry: abs("/Users/ACME/*"),
                canonical: abs("/Users/acme/tools"),
            }]
        };
        assert_eq!(
            paths_grant_notes(
                &abs("/Users/acme/tools"),
                &Decision::Inert(Reason::LockUnavailable),
                &whitelist
            ),
            expected
        );
    }

    /// An entry naming an unrelated directory earns no note at all on an
    /// inert project — neither a grant nor a near-miss.
    ///
    /// Red state: stub `ascii_case_near_miss` to `-> true` unconditionally —
    /// an unrelated entry then earns a near-miss row it must never get, and
    /// this test reds on a non-empty note list.
    #[test]
    fn unrelated_entry_earns_no_note() {
        let whitelist = ShellConsent {
            paths: vec![abs("/elsewhere/other")],
            namespaces: None,
        };
        assert!(
            paths_grant_notes(
                &abs("/Users/acme/tools"),
                &Decision::Inert(Reason::LockUnavailable),
                &whitelist
            )
            .is_empty(),
            "an entry naming an unrelated directory must not earn a row of either kind"
        );
    }

    // ── the paths-entry-defect diagnostic ─────────────────────────────────

    /// One deterministic example per defect class the entry's own bytes
    /// decide. `UnresolvableHome` is excluded: whether a leading `~` resolves
    /// depends on the host's home directory, and neither `consent_entry_defect`
    /// nor this crate exposes a test seam to override it.
    ///
    /// Red state: delete the `if let Some(defect) = consent_entry_defect(entry)`
    /// arm from `paths_grant_notes` — every case below reds on an empty note
    /// list.
    #[test]
    fn each_deterministic_defect_class_earns_its_own_row() {
        for entry in [
            "/w/*/tools",
            "/w/acme*",
            "*",
            "/w/acme/../etc",
            "~alice/project",
            "relative/path",
        ] {
            let path = PathBuf::from(entry);
            let defect =
                consent_entry_defect(&path).unwrap_or_else(|| panic!("{entry:?} must be classified as defective"));
            let whitelist = ShellConsent {
                paths: vec![path.clone()],
                namespaces: None,
            };
            let notes = paths_grant_notes(
                &abs("/unrelated/project"),
                &Decision::Inert(Reason::LockUnavailable),
                &whitelist,
            );
            assert_eq!(
                notes,
                vec![Note::PathsDefect {
                    entry: path,
                    defect: defect.to_string(),
                }],
                "entry {entry:?} must earn exactly one defect row"
            );
        }
    }

    /// A well-formed entry — exact, or an exact `/*` subtree — is never
    /// flagged as defective, whether or not it happens to match the project
    /// in view.
    ///
    /// Red state: change the `consent_entry_defect` guard in
    /// `paths_grant_notes` to `if true` (always taking the defect branch) —
    /// this reds on a non-empty note list.
    #[test]
    fn a_well_formed_entry_earns_no_defect_row() {
        for entry in [abs("/Users/u/repo"), abs("/Users/u/repo/*")] {
            assert!(
                consent_entry_defect(&entry).is_none(),
                "a well-formed entry must not be classified as defective: {}",
                entry.display()
            );
        }
        let whitelist = ShellConsent {
            paths: vec![abs("/Users/u/repo/*")],
            namespaces: None,
        };
        let notes = paths_grant_notes(
            &abs("/elsewhere/other"),
            &Decision::Inert(Reason::LockUnavailable),
            &whitelist,
        );
        assert!(
            notes.iter().all(|note| !matches!(note, Note::PathsDefect { .. })),
            "a well-formed, non-matching entry must earn no defect row: {notes:?}"
        );
    }

    /// A defect takes priority over a near-miss for the same entry —
    /// structural by the `else if` chain, pinned here with an entry that
    /// would ALSO ASCII-case-near-miss the project if the defect check were
    /// skipped, so the two branches genuinely compete for it.
    ///
    /// Red state: change the near-miss `else if` in `paths_grant_notes` to a
    /// second, independent `if` (so both branches can fire for one entry) —
    /// this reds on two notes instead of one.
    #[test]
    fn a_defective_entry_never_also_earns_a_near_miss_row() {
        let entry = abs("/W/*/tools");
        let entry = entry.as_path();
        let project_dir = abs("/w/*/tools");
        let project_dir = project_dir.as_path();
        assert!(
            ascii_case_near_miss(entry, project_dir),
            "the fixture must actually reach the near-miss predicate, or this proves nothing about priority"
        );
        let defect = consent_entry_defect(entry).expect("the fixture entry must be defective");

        let whitelist = ShellConsent {
            paths: vec![entry.to_path_buf()],
            namespaces: None,
        };
        let notes = paths_grant_notes(project_dir, &Decision::Inert(Reason::LockUnavailable), &whitelist);
        assert_eq!(
            notes,
            vec![Note::PathsDefect {
                entry: entry.to_path_buf(),
                defect: defect.to_string(),
            }],
            "a defective entry must not also earn a near-miss row for the same problem: {notes:?}"
        );
    }

    /// A defect is reported even when the entry **matches** the project — the
    /// case the old `if matches / else if defect` ordering could not reach,
    /// because a malformed entry that happens to name the project literally
    /// took the grant arm and, on an inert project, produced no row at all.
    ///
    /// The fixture is a project directory whose own name contains a `*`, which
    /// is what makes the two conditions overlap on a case-sensitive filesystem
    /// as well — this is not a Windows-only shape.
    ///
    /// Red state: restore the old order in `paths_grant_notes` (grant arm
    /// first, defect in the `else if`) — this reds on an empty note list.
    #[test]
    fn a_defective_entry_that_matches_the_project_still_earns_its_defect_row() {
        let entry = abs("/w/*/tools");
        let whitelist = ShellConsent {
            paths: vec![entry.clone()],
            namespaces: None,
        };
        assert!(
            consent_path_matches(&entry, &entry),
            "the fixture must actually match, or it proves nothing about the ordering"
        );
        let defect = consent_entry_defect(&entry).expect("the fixture entry must be defective");
        assert_eq!(
            paths_grant_notes(&entry, &Decision::Inert(Reason::LockUnavailable), &whitelist),
            vec![Note::PathsDefect {
                entry,
                defect: defect.to_string(),
            }]
        );
    }

    /// Unlike the near-miss, a defect row fires even when the project
    /// activated through another clause — a malformed entry is a config error
    /// on its own bytes, not a fact about whether this project got in.
    ///
    /// Red state: add `&& matches!(decision, Decision::Inert(_))` to the
    /// defect branch in `paths_grant_notes` (mirroring the near-miss's gate)
    /// — this reds on an empty note list.
    #[test]
    fn a_defective_entry_is_reported_even_on_an_active_project() {
        let whitelist = ShellConsent {
            paths: vec![abs("/w/*/tools")],
            namespaces: None,
        };
        let notes = paths_grant_notes(
            &abs("/unrelated/project"),
            &Decision::Activate(Grant::Stamp),
            &whitelist,
        );
        assert!(
            matches!(notes.as_slice(), [Note::PathsDefect { .. }]),
            "a malformed entry is a config error regardless of what granted the project: {notes:?}"
        );
    }

    // ── the file tier is the reported scope's own manifest ──────────────────

    /// **The defect this pairing exists to prevent.** With no project and no
    /// `--global`, the report says `scope: global` and
    /// `toolchain_home: $OCX_HOME/toolchain` — so its `activate` must come from
    /// `$OCX_HOME/ocx.toml`, the very file the per-prompt hook reads
    /// (`ocx_cli::command::self_group::activate`'s `global_activate_mode`,
    /// `adr_toolchain_activation.md`).
    ///
    /// Before this, the `file` tier was built from the resolved project alone
    /// and `ProjectConfig::resolve` makes a CWD-walk miss a hard `None` with no
    /// home-tier fallback — so after `ocx self setup --toolchain-activate bin`
    /// a project-free shell composed nothing while this command answered
    /// `activate: env`, from `OCX_TOOLCHAIN_ACTIVATE` ▸ floor. The one command
    /// whose product is *"why does my shell do nothing"* contradicted the
    /// reconciler in exactly the clean-shell case this fix shipped for.
    ///
    /// `pinned` rides along: it is the same file tier, resolved from the same
    /// read.
    ///
    /// Red state: return `PathBuf::new()` from `scope_and_config_path`'s global
    /// arm (the pre-fix "no file tier at all") — the read then misses, the
    /// ladder falls to its floors and both assertions flip.
    #[tokio::test]
    async fn d3_a_project_free_report_answers_from_the_ocx_home_manifest() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            home.path().join(PROJECT_FILE),
            "activate = \"bin\"\npinned = true\n[tools]\n",
        )
        .expect("write the global manifest");

        let (scope, config_path) = scope_and_config_path(home.path(), None, false);
        assert_eq!(
            scope,
            ocx_store::file_structure::RenderStampScope::Global,
            "no project and no --global is the global tier"
        );
        assert_eq!(
            config_path,
            home.path().join(PROJECT_FILE),
            "and the global tier's own manifest is what answers for it"
        );

        let (activate, pinned, note) = toolchain_settings(&config_path).await;
        assert_eq!(
            activate,
            ocx_project::activate::ActivateMode::Bin,
            "the global file states `bin`, and the hook obeys it, so the report must say it"
        );
        assert!(pinned, "the same read answers `pinned` for the same tier");
        assert!(note.is_none(), "a manifest that parses owes no note; got {note:#?}");
    }

    /// The other two arms of the same pairing, so the global one above is not
    /// the function's only demonstrated behaviour.
    ///
    /// Under `--global` the resolved project *is* `$OCX_HOME/ocx.toml`
    /// (`ProjectConfig::resolve` retargets on the selector), so the global arm
    /// names the same file whichever route reached it.
    ///
    /// Red state: drop the `if !global` guard and the third assertion reports
    /// the project's manifest for a `--global` invocation.
    #[tokio::test]
    async fn the_file_tier_follows_the_reported_scope_on_every_arm() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = home.path().join("proj");
        std::fs::create_dir_all(&project).expect("project dir");
        let manifest = project.join(PROJECT_FILE);
        std::fs::write(&manifest, "activate = \"none\"\n").expect("write the project manifest");
        let identity = ProjectIdentity::resolve(manifest).await.expect("resolve the fixture");

        let (scope, config_path) = scope_and_config_path(home.path(), Some(&identity), false);
        assert_eq!(
            scope,
            ocx_store::file_structure::RenderStampScope::Project(identity.dir.clone())
        );
        assert_eq!(config_path, identity.config_path);
        assert_eq!(
            toolchain_settings(&config_path).await.0,
            ocx_project::activate::ActivateMode::None,
            "a project in effect answers from its own manifest"
        );

        let (scope, config_path) = scope_and_config_path(home.path(), Some(&identity), true);
        assert_eq!(
            scope,
            ocx_store::file_structure::RenderStampScope::Global,
            "--global reports the global tier even standing in a project"
        );
        assert_eq!(
            config_path,
            home.path().join(PROJECT_FILE),
            "--global's file tier is the home's manifest, never the project's"
        );
    }

    /// A typo'd `activate` fails **open** on the prompt path — the hook must
    /// stay lenient over `$OCX_HOME/ocx.toml`, so the ladder falls to `env`,
    /// the most-composing mode — and the hook discards the binary's stderr.
    /// This note is therefore the user's only route to the answer.
    ///
    /// An **absent** manifest is not a broken one: it is the ordinary state of
    /// every machine that never set the key, and a note there would be a
    /// warning on the commonest benign state.
    ///
    /// Red state: return `None` instead of the `Some(Note::..)` in
    /// `toolchain_settings`' error arm, and the second assertion flips.
    #[tokio::test]
    async fn an_unparseable_manifest_falls_to_the_floor_and_says_so() {
        let home = tempfile::tempdir().expect("tempdir");

        let (absent, pinned, note) = toolchain_settings(&home.path().join(PROJECT_FILE)).await;
        assert_eq!(absent, ocx_project::activate::ActivateMode::Env, "the floor answers");
        assert!(!pinned, "the `pinned` floor answers too");
        assert!(note.is_none(), "an absent manifest is benign; got {note:#?}");

        std::fs::write(home.path().join(PROJECT_FILE), "activate = \"nnone\"\n").expect("write");
        let (activate, _, note) = toolchain_settings(&home.path().join(PROJECT_FILE)).await;
        assert_eq!(
            activate,
            ocx_project::activate::ActivateMode::Env,
            "the prompt fails open here, so the report must report the mode that actually applies"
        );
        assert!(
            matches!(note, Some(Note::ToolchainManifestUnparsed { .. })),
            "a restriction silently ignored is the state the note exists for; got {note:#?}"
        );
    }
}
