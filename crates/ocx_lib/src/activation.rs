// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The per-prompt session: the sequencing that binds the pure reconciler
//! pieces into one answer, and the consent gate that fronts it.
//!
//! [`plan`](reconcile::plan), [`capture_priors`](reconcile::capture_priors),
//! [`current_fingerprint`](reconcile::current_fingerprint) and the
//! [`Ledger`] codec are each pure and each testable alone. What they do not
//! say is the **order**: resolve the global tier, evaluate consent over the
//! walk's project, compose only what consent authorized, diff that against
//! the live environment, and record the result as the ledger the next prompt
//! plans against. That order is normative — C-018's capture ordering, C-028's
//! read-before-consent bound, A-11's determinacy rule — and it lived in
//! `ocx_cli` until [ocx-sh/ocx#343](https://github.com/ocx-sh/ocx/issues/343),
//! where `ocx shell state` had to re-derive it to explain it.
//!
//! Both callers now share one derivation: `ocx self activate --reconcile`
//! runs [`session`] and renders its [`Outcome`]; `ocx shell state` runs
//! [`evaluate_consent`] and reports the [`ProjectConsent`] it produced. The
//! command crate above keeps only argv, rendering and I/O.
//!
//! `Context`-free by construction: every input is a plain parameter
//! ([`SessionInput`]), so nothing here can reach for ambient CLI state.
//!
//! # Why this is not `shell::reconcile::session`
//!
//! Sequencing is application-layer, not shell-emit vocabulary, and putting it
//! under `shell/` made the two modules mutually dependent:
//! [`crate::project::consent`] reads `shell::coexistence::Observation` and
//! `shell::reconcile::ScopeId`, while this module reads `project::consent`. No
//! other file under `shell/` reaches for `project`, so the cycle was this one
//! alone — and a `use` cycle does not compile across a crate boundary, which
//! makes it a blocker for the planned `ocx_lib` split
//! ([ocx-sh/ocx#313](https://github.com/ocx-sh/ocx/issues/313),
//! [ocx-sh/ocx#324](https://github.com/ocx-sh/ocx/issues/324)). At the crate
//! root it may depend on both, and both stay independent of it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::env::Env;
use crate::oci;
use crate::package::metadata::env::entry::Entry;
use crate::package::metadata::env::modifier::ModifierKind;
use crate::project::consent::{self, ConsentStamp, Decision};
use crate::project::{LockCurrency, ProjectLock, lock::lock_path_for};
use crate::shell::coexistence;
use crate::shell::reconcile::{self, Ledger, LedgerEntry, Plan, ProjectScope, Scopes, Verdict};
use crate::{Config, ShellConsent, effective_consent, file_structure, package_manager};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// What one per-prompt session can fail with.
///
/// `ocx_lib` cannot use `anyhow`, so the two project-tier refusals the
/// activation path raises carry their own variants here. Both keep the exact
/// wording `ProjectContextError` uses for the same states — a user who hits
/// them from `ocx pull` and from a prompt must read the same sentence — and
/// both keep their exit codes through the [`ClassifyExitCode`] impl below
/// rather than through the caller.
///
/// [`ClassifyExitCode`]: crate::cli::ClassifyExitCode
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// `ocx.lock` is absent, or no longer describes the `ocx.toml` beside it.
    ///
    /// [`LockCurrency::Missing`] is reachable on the activation path because a
    /// `paths` grant is the one consent clause that holds without a readable
    /// lock. Wrapped rather than restated: `ProjectContextError` names the same
    /// two states for `ocx pull`, and a user meeting one from a command and
    /// from a prompt must read the same sentence.
    #[error("{0}")]
    Lock(#[from] LockCurrency),

    /// Any library error the composition raised. Display and `source()` both
    /// delegate, so `classify_error`'s chain walk reaches the inner type and
    /// classifies on it.
    #[error("{0}")]
    Library(#[from] crate::Error),

    /// Two contributors to one list key declared different separators.
    #[error("{0}")]
    ListSeparator(#[from] crate::env::ListSeparatorError),
}

impl crate::cli::ClassifyExitCode for SessionError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        match self {
            // 78 for an absent lock, 65 for a stale one — the mapping lives
            // with the wording it belongs to.
            Self::Lock(currency) => currency.classify(),
            // Defer to the wrapped library error's own classification, which
            // the chain walk reaches through `source()`.
            Self::Library(_) | Self::ListSeparator(_) => None,
        }
    }
}

impl From<crate::project::Error> for SessionError {
    fn from(error: crate::project::Error) -> Self {
        Self::Library(error.into())
    }
}

impl From<package_manager::error::Error> for SessionError {
    fn from(error: package_manager::error::Error) -> Self {
        Self::Library(error.into())
    }
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// The project the CWD walk resolved, and the two labels the ledger records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectIdentity {
    /// The resolved `ocx.toml`.
    pub config_path: PathBuf,
    /// Its canonical directory (A-30) — the project's identity.
    pub dir: PathBuf,
    /// `ReferenceManager::name_for_path` of `dir`: a lookup index, never the
    /// identity.
    pub key: String,
}

/// Why a resolved `ocx.toml` could not be turned into a [`ProjectIdentity`].
///
/// Both states are about a project that demonstrably exists — the walk found
/// the file — so both callers treat them as *indeterminate*, never as "no
/// project here".
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IdentityError {
    /// The filesystem would not canonicalize the resolved `ocx.toml`.
    #[error("could not canonicalize '{path}': {source}")]
    Canonicalize {
        /// The `ocx.toml` that would not resolve.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// The blocking hop itself failed.
    #[error("canonicalization task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

impl ProjectIdentity {
    /// Derive the identity of the project at `config_path` — A-30's canonical
    /// directory, then its lookup key.
    ///
    /// The **only** way to build one from a path. Both callers used to run this
    /// pair themselves: `ocx self activate --reconcile`'s walk and `ocx shell
    /// state`'s selection each did `canonical_project_dir` then
    /// `name_for_path`, which made `key` — the `state/projects/<key>/` stamp
    /// key — a value with two derivations. That is the residual second answer
    /// [#343](https://github.com/ocx-sh/ocx/issues/343) was about; the
    /// *selection* of which `ocx.toml` to resolve stays with each caller,
    /// because a prompt only ever walks the CWD while a diagnostic must answer
    /// for the `--project` the user typed.
    ///
    /// # Errors
    ///
    /// [`IdentityError`] when the path will not canonicalize, or the blocking
    /// hop fails. Both mean *indeterminate*, not *absent*.
    pub async fn resolve(config_path: PathBuf) -> Result<Self, IdentityError> {
        let canonical = config_path.clone();
        // `canonical_project_dir` is two `stat`-walking syscalls, so it goes to
        // a blocking thread rather than stalling the runtime — the same hop
        // both callers already made around it.
        let dir = tokio::task::spawn_blocking(move || consent::canonical_project_dir(&canonical))
            .await?
            .map_err(|source| IdentityError::Canonicalize {
                path: config_path.clone(),
                source,
            })?;
        let key = crate::reference_manager::ReferenceManager::name_for_path(&dir);
        Ok(Self { config_path, dir, key })
    }
}

/// Everything one session reads, as plain values.
///
/// Deliberately not a `Context`: the CLI's context type carries argv, colour
/// config and a network client, none of which a reconcile may reach for, and
/// taking it would put `ocx_lib` downstream of `ocx_cli`. The global tier's
/// entries arrive already resolved — that resolution is the login exporter's
/// (`ocx_cli::command::toolchain_env`), which composes `--env` overrides and
/// group selection the prompt never has.
pub struct SessionInput<'a> {
    /// The global toolchain tier's entries, resolved by the caller (A-44).
    pub global: Vec<Entry>,
    /// The offline-capable manager the project composition runs against.
    pub manager: &'a package_manager::PackageManager,
    /// The local index the manager's offline view is taken over.
    pub local_index: &'a oci::index::LocalIndex,
    /// The materialisation concurrency.
    pub concurrency: package_manager::Concurrency,
    /// `$OCX_HOME`'s layout — the package store consent corroborates against.
    pub file_structure: &'a file_structure::FileStructure,
    /// The merged config, for its `[shell]` table.
    pub config: &'a Config,
    /// The host platform the lock's per-platform digests resolve under.
    pub target: &'a oci::Platform,
    /// The project the CWD walk resolved, or `None` for a project-free prompt.
    pub project: Option<&'a ProjectIdentity>,
}

/// What one recomposition resolved: the entries each scope wants, the project
/// slot's identity, and every message the prompt owes the user.
pub struct Outcome {
    /// The global toolchain tier's entries, applied first (C-018).
    pub global: Vec<Entry>,
    /// The project tier's entries — empty when inert or yielded.
    pub project: Vec<Entry>,
    /// The project slot to record, or `None` to retire it.
    pub slot: Option<ProjectIdentity>,
    /// Whether the CWD walk resolved a project at all.
    ///
    /// `slot` cannot answer this: it is `None` both when no project was found
    /// **and** when one was found but yielded to direnv/mise or was refused by
    /// consent. Only "no project at all" is cacheable as
    /// [`Verdict::NoProject`] — a yield is expired by an env sentinel the
    /// fingerprint does not fold, so it must recompose every prompt.
    pub resolved: bool,
    /// Whether the resolved project was refused by consent (C-025).
    pub inert: bool,
    /// Deferred diagnostics, in emission order (A-21).
    pub messages: Vec<String>,
}

/// The C-028 gate, in its own module so its field is genuinely private.
///
/// A unit struct would be freely constructible anywhere in this file — Rust's
/// privacy is module-scoped — which would make the "compile-time obligation"
/// claim false for exactly the edit it is meant to stop. The private field
/// makes [`ConsentProof::of`] the only way to mint one from anywhere outside
/// these few lines.
mod consent_gate {
    use crate::project::consent::{Decision, Grant};

    /// Evidence that consent was evaluated for this project and said
    /// `Activate`, **and which clause said so** (C-028).
    ///
    /// A capability token, not decoration. C-028 is the design's mise-CVE
    /// lesson — *"the only project-supplied bytes read before consent is
    /// established are the CWD walk's `stat` calls and the `ocx.lock` parse the
    /// source-set predicate requires; `ProjectConfig` deserialization happens
    /// after"* — and it was previously guaranteed by statement order alone, so
    /// hoisting one call above another defeated it silently with every test
    /// still green.
    ///
    /// `project_entries` takes one of these, and one cannot exist
    /// without a [`Decision`] having been produced first, so the ordering is a
    /// **compile-time** obligation rather than a comment a future edit can
    /// step over.
    ///
    /// The carried [`Grant`] extends the same idea one step: the token now says
    /// *how much* was granted, so the project-file `[env]` channel is opened by
    /// a value rather than by the absence of a check.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ConsentProof(Grant);

    impl ConsentProof {
        /// Mint the proof, or `None` when consent refused.
        pub fn of(decision: &Decision) -> Option<Self> {
            match decision {
                Decision::Activate(grant) => Some(Self(*grant)),
                Decision::Inert(_) => None,
            }
        }

        /// Whether the granting clause authorizes the project-file `[env]`
        /// channel — clause 1 or clause 3, never clause 2.
        pub fn authorizes_project_env(self) -> bool {
            self.0.authorizes_project_env()
        }
    }
}

pub use consent_gate::ConsentProof;

// ---------------------------------------------------------------------------
// Consent — the one derivation both `self activate` and `shell state` use
// ---------------------------------------------------------------------------

/// The consent answer for one project, and the evidence that produced it.
///
/// **The struct is the seam.** Its fields are private and it has no public
/// constructor, so the only way to hold one is to have called
/// [`evaluate_consent`] — the same compile-time shape [`ConsentProof`] uses one
/// level down. That is what stops a second derivation from growing back: a
/// caller cannot assemble a `Decision` of its own and feed it to either
/// consumer, which is exactly how `ocx shell state` came to read the two
/// `OCX_CONSENT_*` variables as string literals where `ocx self activate` read
/// them through the constants
/// ([ocx-sh/ocx#343](https://github.com/ocx-sh/ocx/issues/343)).
#[derive(Debug)]
pub struct ProjectConsent {
    decision: Decision,
    stamp: Option<ConsentStamp>,
    lock: Option<ProjectLock>,
}

impl ProjectConsent {
    /// The activation predicate's answer.
    pub fn decision(&self) -> &Decision {
        &self.decision
    }

    /// Whether a usable stamp backs it (A-25 — an unusable stamp is an absent
    /// one).
    pub fn stamped(&self) -> bool {
        self.stamp.is_some()
    }

    /// The usable stamp itself, when there is one.
    ///
    /// Carried rather than reduced to a bool because `ocx shell state` reports
    /// *when* the stamp was written: a grant the user cannot see the age of is
    /// half the invisibility the stamp was faulted for. Reading the file a
    /// second time from the renderer would reopen the drift
    /// [#343](https://github.com/ocx-sh/ocx/issues/343) closed.
    pub fn stamp(&self) -> Option<&ConsentStamp> {
        self.stamp.as_ref()
    }

    /// The parsed `ocx.lock` the evidence hop read, or `None` when it was
    /// absent, unreadable or unparseable — one outcome, because all three
    /// leave the source-set predicate with nothing to quantify over.
    ///
    /// Handed back rather than re-read: two reads of the same file are two
    /// different byte sequences the moment a `git checkout` lands between
    /// them, and consent deciding on one while composition uses the other is
    /// the whole finding C-028 exists for.
    pub fn lock(&self) -> Option<&ProjectLock> {
        self.lock.as_ref()
    }

    /// Take the lock back out, for the composition that follows a grant.
    fn into_lock(self) -> Option<ProjectLock> {
        self.lock
    }
}

/// Evaluate consent over one project: read every input on one blocking hop,
/// then run the predicate.
///
/// The single entry point for the question "is this project activated, and if
/// not, why". `ocx self activate --reconcile` turns the answer into a
/// [`ConsentProof`] or an inert outcome; `ocx shell state` renders the same
/// answer as the report's reason. Neither derives it a second time.
///
/// Read-only (A-29): nothing here writes a stamp. A grant activates directly.
pub async fn evaluate_consent(
    whitelist: &ShellConsent,
    target: &oci::Platform,
    store: &file_structure::PackageStore,
    project: &ProjectIdentity,
) -> ProjectConsent {
    let lock_path = lock_path_for(&project.config_path);
    let lock = ProjectLock::load(&lock_path).await.ok();
    let sources = lock.as_ref().map(consent::lock_sources);
    let (lock, verified, stamp) = consent_evidence(lock, whitelist, target, store, &project.key).await;
    let decision = consent::evaluate_with_stamp(
        &project.dir,
        stamp.as_ref(),
        sources.as_ref(),
        verified.as_ref(),
        whitelist,
    );
    ProjectConsent { decision, stamp, lock }
}

/// Every piece of consent evidence the prompt needs, on **one** blocking hop
/// (C-028, C-044).
///
/// Two blocking reads used to sit on this path independently:
/// [`consent::verified_sources`] on its own `spawn_blocking`, and
/// [`consent::load`] — a `std::fs::read` — inline on the runtime thread. They
/// fold into a single hop for the same reason the fingerprint fold does (see
/// `run_reconcile`): one hop for the whole read set is cheaper than the exec
/// that reached it, and an inline `std::fs::read` is not a hop at all.
///
/// `lock` travels **in and back out** rather than being cloned. `ProjectLock`
/// derives `Clone` with no `Arc`, so a clone deep-copies every [`LockedTool`] —
/// two `String`s, an `Identifier` and a platform `BTreeMap` apiece — on every
/// prompt, and the caller needs the original afterwards for `project_entries`.
///
/// # The clause-2 gate
///
/// [`consent::verified_sources`] costs a `read_dir` plus a per-marker read for
/// **every** locked tool, and its result is unobservable unless
/// `[shell.consent] namespaces` is configured. Both branches of
/// [`consent::evaluate_with_stamp`] that read `verified` are reached only
/// through its `namespace_granted`, which returns `false` outright when
/// `whitelist.namespaces` is `None` — the grant branch cannot fire, and the
/// `UncorroboratedNamespace` refusal that would have carried `verified` in its
/// payload is behind the same short-circuit. So with no namespaces grant the
/// evidence is not read at all and `None` is passed on. `None` there is not the
/// weaker answer it is elsewhere: it is *indistinguishable* from any `Some(..)`,
/// because the only clause that could tell them apart is already refused.
///
/// # Failing closed
///
/// A join error yields `(None, None, None)`. No stamp and no corroboration is
/// strictly less consent than the hop would have produced, and the lost lock
/// reproduces `project_entries`' `LockMissing` arm, whose contract is "emit
/// nothing, retain what is applied" rather than a torn-down environment.
///
/// [`LockedTool`]: crate::project::lock::LockedTool
async fn consent_evidence(
    lock: Option<ProjectLock>,
    whitelist: &ShellConsent,
    target: &oci::Platform,
    store: &file_structure::PackageStore,
    key: &str,
) -> (Option<ProjectLock>, Option<BTreeSet<String>>, Option<ConsentStamp>) {
    let namespaces_configured = whitelist.namespaces.is_some();
    let target = target.clone();
    let store = store.clone();
    let key = key.to_owned();
    match tokio::task::spawn_blocking(move || {
        let verified = lock
            .as_ref()
            .filter(|_| namespaces_configured)
            .and_then(|lock| consent::verified_sources(lock, &target, &store));
        let stamp = consent::load(&key);
        (lock, verified, stamp)
    })
    .await
    {
        Ok(evidence) => evidence,
        Err(error) => {
            tracing::debug!("the consent-evidence read task failed: {error}");
            (None, None, None)
        }
    }
}

/// One info line **per observed tool** (C-049, A-37).
///
/// The count is the contract, not the wording: with both direnv and mise live,
/// both lines appear. This function owns the **renderer** half of A-37 — fan out
/// one line per observation, never just the first. The **detection** half (the
/// two sentinels being independent `if`s rather than an `elif` chain) lives in
/// `shell::coexistence::detect` and is guarded there by
/// `detect_both_sentinels_fire_independently_a37` (`coexistence.rs:227`), which
/// sets both env sentinels for real. An `elif` there would return one
/// `Observation` and this function would faithfully render one line, so the two
/// halves need their own guards and neither can stand in for the other.
pub fn yield_messages(yielded: &coexistence::Yield) -> Vec<String> {
    yielded
        .observed
        .iter()
        .map(|observation| {
            let tool = match observation.tool {
                coexistence::Tool::Direnv => "direnv",
                coexistence::Tool::Mise => "mise",
            };
            format!(
                "ocx: {tool} manages this directory ({signal}); applying the global toolchain only",
                signal = observation.signal,
            )
        })
        .collect()
}

/// Whether this prompt can answer from `stat`s alone (C-042).
///
/// **Only the negative verdicts are cached.** An `Activate` verdict is always
/// re-derived, because caching it would make the ledger a consent input, which
/// C-007 forbids — and caching a negative one can only ever cause ocx to do
/// *less*, which is the fail-safe direction. The cache is sound only because the
/// watch set it hangs off expires it (A-13).
///
/// [`Verdict::NoProject`] is the second negative and is **not** consent-derived
/// — there is no project to consent to — so it leaves C-007 exactly where it
/// was. Without it the fast path could only ever fire inside a consent-refused
/// project, and every `cd` in an ordinary directory paid `Context::try_init`,
/// `resolve_global_pinned_env` and a full plan to recompose the global tier the
/// fingerprint had already proved unchanged: 21.3 ms against 4.5 ms, measured on
/// a real bash with the real emitted hook.
pub fn is_stat_only(ledger: &Ledger, fingerprint: &str) -> bool {
    ledger.fp == fingerprint && matches!(ledger.verdict, Some(Verdict::Inert | Verdict::NoProject))
}

/// The project-file `[env]` channel, gated on the clause that granted.
///
/// Returns the entries to apply and whether a **declared** `[env]` was withheld
/// — the second half is what the caller owes a hint line for, and it is `false`
/// for a project that declares no `[env]` at all so an ordinary
/// namespace-granted project prints nothing every prompt.
///
/// This is the only call to [`project_env_entries`] on the activation path, and
/// it is the only consumer of `project_entries`' `consent` parameter: deleting
/// the gate leaves that parameter unused, which fails the build under
/// `-D warnings` rather than silently re-opening the channel.
///
/// [`project_env_entries`]: crate::project::project_env_entries
pub fn authorized_project_env(
    consent: ConsentProof,
    config: &crate::project::ProjectConfig,
    config_path: &Path,
    groups: &[String],
) -> (Vec<Entry>, bool) {
    let declared = crate::project::project_env_entries(config, config_path, groups);
    if consent.authorizes_project_env() {
        return (declared, false);
    }
    let withheld = !declared.is_empty();
    (Vec::new(), withheld)
}

/// Compose the consenting project's toolchain env, and report whether the
/// project's own `[env]` was withheld.
///
/// Deliberately **not** `load_project_with_lock_consenting`: nothing on the
/// activation path writes a stamp (A-26). A grant activates directly.
///
/// Also deliberately **not** `load_project_with_lock`: that helper re-runs the
/// precedence chain and re-reads `ocx.lock`, so consent could be decided on one
/// set of bytes and composition run on another, and an ambient `OCX_GLOBAL=1`
/// would re-target it at `$OCX_HOME/ocx.toml` behind the verdict's back. The
/// walk's `config_path` and the lock the source-set predicate already parsed are
/// passed in instead; only the staleness gate is reproduced here, verbatim.
async fn project_entries(
    input: &SessionInput<'_>,
    // C-028 — the parameter *is* the gate: there is no way to reach the
    // `ocx.toml` deserialization below without having evaluated consent first.
    // S1 — it is also *read*, by `authorized_project_env`, so the gate cannot be
    // removed without leaving an unused binding behind.
    consent: ConsentProof,
    project: &ProjectIdentity,
    lock_path: &Path,
    lock: Option<ProjectLock>,
) -> Result<(Vec<Entry>, bool), SessionError> {
    use crate::package_manager::composer::{ComposeRequest, Materialization};
    use crate::project::{DEFAULT_GROUP, ProjectConfig, compose_tool_set};

    // A `paths` grant is the one clause that holds without a readable lock, so
    // this arm is reachable — and it names the same state, in the same words,
    // that `ocx pull` reports for it, which `compose`'s `Err` contract turns
    // into "emit nothing, retain what is applied" rather than a torn-down
    // environment.
    let Some(lock) = lock else {
        return Err(LockCurrency::Missing {
            path: lock_path.to_path_buf(),
        }
        .into());
    };

    let config = ProjectConfig::from_path(&project.config_path).await?;
    // The staleness gate, unchanged: a lock whose stored hash disagrees with the
    // current `ocx.toml` is stale on-disk data.
    if crate::project::lock::is_stale(&lock, &config) {
        return Err(LockCurrency::Stale {
            lock_path: lock_path.to_path_buf(),
        }
        .into());
    }

    let groups = vec![DEFAULT_GROUP.to_owned()];
    let tools = compose_tool_set(&config, Some(&lock), &groups, &[], input.target)?;

    // `LocalOnly`, unconditionally: a prompt must never block on the network,
    // and a tool that is not materialised yet is an omission the next explicit
    // `ocx pull` fixes — never a hung shell.
    let manager = input.manager.offline_view(input.local_index.clone());
    let requests: Vec<ComposeRequest> = tools
        .iter()
        .map(|tool| ComposeRequest {
            identifier: tool.identifier.clone(),
            mode: crate::project::lazy_mode_for_tool(&config, &tool.identifier, None, None),
        })
        .collect();
    let roots = manager
        .compose_roots(&requests, input.target, Materialization::LocalOnly, input.concurrency)
        .await?;
    let (env, env_withheld) = authorized_project_env(consent, &config, &project.config_path, &groups);
    let scope = crate::package_manager::EnvScope::Project {
        no_patches: config.no_patches_repositories(),
        env,
    };
    let (mut entries, ..) = manager
        .resolve_env_with_attribution(&roots.roots, false, scope, input.target)
        .await?;
    crate::env::reconcile_list_separators(entries.iter_mut())?;
    Ok((entries, env_withheld))
}

/// Compose both scopes and decide the project slot (C-018, C-025, C-049).
///
/// An `Err` here emits **nothing at all**, which is the fail-safe outcome and
/// not a degraded one: the previous environment stays applied and the carrier
/// stays as it was, so a momentarily stale lock (mid-`git checkout`) retains the
/// scope rather than tearing it down. Emitting a partial plan built from a
/// `desired` that is missing the project scope would *revert* it — the wrong
/// direction — which is the same reasoning C-018 gives for an indeterminate
/// walk.
///
/// # Errors
///
/// [`SessionError::LockMissing`] and [`SessionError::StaleLock`] when the
/// consenting project's lock is absent or disagrees with its `ocx.toml`;
/// otherwise whatever the composition raised.
pub async fn session(input: SessionInput<'_>) -> Result<Outcome, SessionError> {
    // A-44 — the ocx home toolchain is ALWAYS consented, which is why the
    // caller resolves it before this function is even entered: before the
    // walk's project, before the lock read, before `evaluate_with_stamp`. Every
    // arm below returns this same `Outcome` and none of them clears `global`.
    // `$OCX_HOME/ocx.toml` is the user's own file, so no `[shell.consent]`
    // entry — nor the absence of one, nor a refused project sitting in the CWD
    // — may withhold it. Do not put a gate in front of this.
    let mut outcome = Outcome {
        global: input.global.clone(),
        project: Vec::new(),
        slot: None,
        resolved: input.project.is_some(),
        inert: false,
        messages: Vec::new(),
    };

    let shell_config = input.config.shell.as_ref();
    // C-034 — the managed tier dropped a `[shell.consent]` payload. `log::warn!`
    // goes to a stderr the shims discard, so the reason rides the prompt.
    if let Some(reason) = shell_config.and_then(|shell| shell.consent_strip_reason.as_ref()) {
        outcome.messages.push(format!("ocx: {reason}"));
    }

    let Some(project) = input.project else {
        return Ok(outcome);
    };

    // C-049 + A-37 — the two sentinels are **independent `if`s, never an `elif`
    // chain**: with both direnv and mise live, both lines appear. Narrowing
    // `desired` to the global scope and leaving `slot` at `None` is the whole
    // behaviour: C-016's retirement rule then retires the project's recorded
    // entries subtractively, with no new planner arm.
    let yielded = coexistence::detect(&project.dir);
    if !yielded.observed.is_empty() {
        outcome.messages.extend(yield_messages(&yielded));
        return Ok(outcome);
    }

    // C-028's bounded carve-out: the lock parse the source-set predicate needs,
    // and nothing else, before consent is established.
    //
    // Hoisted above the hop that consults it: `effective_consent` is the
    // already-loaded `[shell]` table plus two `OCX_CONSENT_*` env reads (C-031),
    // so it costs nothing here and lets the hop skip evidence the whitelist
    // makes unobservable.
    let whitelist = effective_consent(shell_config);
    let evaluated = evaluate_consent(&whitelist, input.target, &input.file_structure.packages, project).await;

    let Some(consent_proof) = ConsentProof::of(evaluated.decision()) else {
        outcome.inert = true;
        // The gesture first, the diagnostic second. Pointing only at
        // `ocx shell state` dead-ends: it is read-only, so a user who follows
        // the hint learns the reason and still has nothing to type.
        // `ocx shell allow` is the grant, and naming the directory is what
        // makes either half actionable at a prompt.
        outcome.messages.push(format!(
            "ocx: {dir} is not activated; run `ocx shell allow` to consent, or `ocx shell state` to see why",
            dir = project.dir.display(),
        ));
        return Ok(outcome);
    };
    // Consent said yes: only now is the project's own `ocx.toml` deserialized.
    //
    // **Read once.** The lock travels out of `evaluate_consent` rather than
    // being re-read here: two reads of the same file are two different byte
    // sequences the moment a `git checkout` lands between them, and consent
    // deciding on one while composition uses the other is the whole finding.
    let lock_path = lock_path_for(&project.config_path);
    let (entries, env_withheld) =
        project_entries(&input, consent_proof, project, &lock_path, evaluated.into_lock()).await?;
    outcome.project = entries;
    if env_withheld {
        // A namespaces grant is a fleet auto-enabler satisfied by the project's
        // own lock text, so it authorizes packages and not the project file's
        // own `[env]`. Same shape as the inert hint above: name the directory,
        // name the gesture that would widen it.
        outcome.messages.push(format!(
            "ocx: {dir}: its [env] is not applied - a namespaces grant covers packages only; run `ocx pull` here \
             once, or add the directory to `[shell.consent] paths`",
            dir = project.dir.display(),
        ));
    }
    outcome.slot = Some(project.clone());
    Ok(outcome)
}

/// The union `desired` set, global first, project second (C-018).
pub fn desired_entries(outcome: &Outcome) -> Vec<Entry> {
    let mut desired = outcome.global.clone();
    desired.extend(outcome.project.iter().cloned());
    desired
}

/// Diff `outcome` against the live environment, scoped by the previous ledger.
pub fn plan_for(previous: &Ledger, outcome: &Outcome, owned: &[&Path], current: &Env) -> Plan {
    reconcile::plan(&desired_entries(outcome), current, previous, owned)
}

/// The digest [`Ledger::messages_fp`] records for one prompt's deferred
/// diagnostics.
///
/// Length-prefixed per message, the same rule [`reconcile::fingerprint`]'s own
/// fold follows and for the same reason: without it `["ab", "c"]` and
/// `["a", "bc"]` are one byte stream, and a message set that changed would read
/// as unchanged. Truncated to 16 hex characters — this decides whether to
/// re-print a line, not whether to trust one, so the carrier budget matters more
/// than the remaining collision margin.
///
/// An empty list digests to the empty string rather than to SHA-256's digest of
/// nothing, so "this prompt had nothing to say" is spelled the same way
/// [`Ledger::empty`] spells it and costs no carrier bytes.
fn messages_fingerprint(messages: &[String]) -> String {
    use sha2::Digest as _;

    if messages.is_empty() {
        return String::new();
    }
    let mut hasher = sha2::Sha256::new();
    for message in messages {
        hasher.update((message.len() as u64).to_le_bytes());
        hasher.update(message.as_bytes());
    }
    hex::encode(hasher.finalize())[..16].to_owned()
}

/// Carry `previous` forward verbatim, recording `messages` as announced (A-21).
///
/// The refusal path's counterpart to [`next_ledger`], and deliberately not a
/// call to it. A prompt whose project lock is absent or stale emits **no plan**
/// — [`session`]'s `Err` contract is "emit nothing at all", so the applied
/// environment stays exactly as it stands — but it still owes the user the
/// sentence, and a sentence the shell repeats before every prompt until they
/// run `ocx lock` is the noise [`Ledger::messages_fp`] exists to stop. Building
/// a fresh ledger instead would drop the project scope this path is refusing to
/// tear down, and the *next* prompt would then have no record of it to revert.
pub fn announcing(previous: &Ledger, messages: &[String]) -> Ledger {
    Ledger {
        messages_fp: messages_fingerprint(messages),
        ..previous.clone()
    }
}

/// Build the ledger the next prompt will plan against (C-002, C-015, C-018).
///
/// `current` is the **pre-global** environment — the live shell as this prompt
/// found it. Both capture points are derived here rather than by the caller,
/// because C-018's ordering *is* the difference between them: the global scope's
/// priors are captured against `current`, then global is applied, then the
/// project's priors are captured against the result. Handing this function a
/// pre-applied environment would put that ordering somewhere no test of this
/// function can see it.
pub fn next_ledger(previous: &Ledger, fingerprint: &str, outcome: &Outcome, current: &Env) -> Ledger {
    // A-10 — `L ⊆ emittable(D)`. `plan` drops what no arm can emit, and what it
    // drops is never applied to the shell; recording it as applied anyway would
    // put a key in L that ocx claims to own and can never remove. The sharpest
    // case is A-02's refused `PATH`/`PATHEXT` constant: `plan` refuses it, and a
    // ledger that carried it would hand the whole variable's restore to a prior
    // captured from an apply that never happened.
    let desired_global: Vec<_> = reconcile::emittable_entries(&outcome.global)
        .into_iter()
        .cloned()
        .collect();
    let global: Vec<LedgerEntry> = desired_global.iter().map(LedgerEntry::from).collect();
    // R1 — against `current`, the environment *before* global applied, which is
    // the only place the user's own value for a global constant is still
    // visible. Without it a constant the global tier stops declaring
    // (`ocx remove --global <pkg>`) has no prior to restore and ocx's value
    // stays in the shell for its whole life.
    let global_priors = reconcile::capture_priors(
        &global,
        current,
        previous
            .scopes
            .global
            .as_deref()
            .map(|applied| (applied, &previous.scopes.global_priors)),
    );

    let project = outcome.slot.as_ref().map(|slot| {
        let desired: Vec<_> = reconcile::emittable_entries(&outcome.project)
            .into_iter()
            .cloned()
            .collect();
        let applied: Vec<LedgerEntry> = desired.iter().map(LedgerEntry::from).collect();
        // The same set the emitted global statements will apply, so the
        // project's priors are captured against the environment the shell will
        // actually be in (C-018).
        //
        // Built **only where it is read**: `capture_priors` skips every
        // non-`Constant` entry before touching its `current` argument, while
        // building this clones the whole process `Env` and replays every global
        // path entry through a full-`PATH` `move_to_front`. A path-only
        // toolchain — the common case — declares no constant at all, and paid
        // both on every prompt for an answer nothing consulted.
        let after_global = applied
            .iter()
            .any(|entry| matches!(entry.kind, ModifierKind::Constant))
            .then(|| {
                let mut env = current.clone();
                env.apply_entries(&desired_global);
                env
            });
        ProjectScope {
            key: slot.key.clone(),
            dir: slot.dir.clone(),
            // §10a — WP-1 shipped `capture_priors` precisely because nothing was
            // contracted to build the *next* ledger; the ordering is the
            // caller's (C-015 rules 3-4, C-018). Without this call a project
            // leave has no prior to restore and a global constant the project
            // overrode is lost for the shell's whole life.
            priors: reconcile::capture_priors(
                &applied,
                // Unread when `after_global` was not built, since the only
                // reader is the `Constant` arm that would have built it.
                after_global.as_ref().unwrap_or(current),
                previous
                    .scopes
                    .project
                    .as_ref()
                    .map(|scope| (scope.applied.as_slice(), &scope.priors)),
            ),
            applied,
        }
    });

    Ledger {
        v: previous.v,
        fp: fingerprint.to_owned(),
        // Carried forward for the same reason `tiers` is, one line down, but a
        // different one: `ws` describes the gate the **shell** currently holds,
        // and only the emission that redefines that gate
        // ([`crate::shell::hook::redefinition`]) may move it. Deriving it here
        // would record a membership the shell was never given, and the stale
        // gate would then never be noticed again.
        ws: previous.ws.clone(),
        // Carried forward, never re-derived: this process cannot see the
        // `--config` overlay the shell-start pass recorded (A-13, A-33).
        tiers: previous.tiers.clone(),
        // What this prompt owes the user, as a digest the next prompt compares
        // against to decide whether its own messages are news (A-21).
        messages_fp: messages_fingerprint(&outcome.messages),
        // C-042 — only a negative verdict is ever written. An `Activate` verdict
        // is re-derived every prompt; caching it would make the ledger a consent
        // input, which C-007 forbids.
        //
        // Three-way, and the third arm is the common one: a walk that resolved
        // no project caches `NoProject`, which is not consent-derived and so
        // touches C-007 not at all. The `else` covers both remaining states —
        // an activated project, and one that yielded to direnv/mise. Neither is
        // cacheable: the first by C-007, the second because the yield hangs off
        // an env sentinel `fingerprint` does not fold.
        verdict: match (outcome.inert, outcome.resolved) {
            (true, _) => Some(Verdict::Inert),
            (false, false) => Some(Verdict::NoProject),
            (false, true) => None,
        },
        // Empty on purpose, and **not** carried forward from `previous`: this
        // field is `encode`'s *input*, and `encode` derives the emitted marker
        // from the scopes this ledger actually carries (`Ledger::dropped_scopes`
        // unions it with whatever the ledger already names). Seeding it from the
        // previous prompt would make a ledger that now fits under the cap
        // advertise abandoned scopes it holds in full — and `ocx shell state`
        // would report `LedgerOverCap` for a healthy shell. The over-cap state
        // survives across prompts through the encoded carrier, which is what
        // `ledger_lines` compares against.
        over_cap: Vec::new(),
        scopes: Scopes {
            global: Some(global),
            global_priors,
            project,
        },
    }
}

/// A-11's determinacy check, run **only on the revert path** — never on the
/// no-op path, so it costs nothing on the prompt that matters.
///
/// The scope is retained when `recorded_dir` — the project directory the
/// previous ledger recorded — is still an ancestor-or-self of the CWD **and**
/// its `ocx.toml` is still a regular file. A genuine leave fails the ancestor
/// test, a genuine deletion fails the `symlink_metadata` test, and
/// `OCX_NO_PROJECT=1` is excluded outright — all three revert normally.
///
/// Takes the recorded directory rather than the whole [`Ledger`] because that
/// is all it reads, and because the caller hands it to a `spawn_blocking` hop:
/// two `Option<PathBuf>`s cross that boundary for free where a `Ledger` would
/// deep-copy both scopes' entries and priors on every revert.
///
/// **Blocking**: one `symlink_metadata`. Call it from a blocking context.
pub fn walk_is_indeterminate(recorded_dir: Option<&Path>, cwd: Option<&Path>) -> bool {
    let Some(recorded_dir) = recorded_dir else {
        // Nothing recorded, nothing to retain: the distinction cannot matter.
        return false;
    };
    // The carrier is untrusted (C-007), and this is the **one** place a recorded
    // `dir` reaches the filesystem, so it is validated here rather than trusted.
    // A-30 makes an honest one canonical, hence absolute; anything else is
    // forged or corrupt and gets the fail-safe answer.
    //
    // `""` is the case that bites, and it is not hypothetical — it is in the
    // forged set the carrier tests already enumerate. Every path
    // `starts_with("")`, so the ancestor test below waves it through, and
    // `Path::new("").join("ocx.toml")` is **relative**: the probe would quietly
    // become "does the process CWD hold an `ocx.toml`", a question the walk has
    // already answered, and a carrier could pin the recorded scope for the
    // shell's life from any directory that happens to hold one.
    if !recorded_dir.is_absolute() {
        return false;
    }
    // `OCX_NO_PROJECT=1` is a deliberate instruction, not a failed probe. It
    // must revert even though the directory and its file are both still there.
    if crate::env::flag("OCX_NO_PROJECT", false) {
        return false;
    }
    let Some(cwd) = cwd else {
        // No CWD to test against — the walk's answer is unknowable rather than
        // negative, and a recorded scope exists. Retain.
        return true;
    };
    if !cwd.starts_with(recorded_dir) {
        return false;
    }
    // `symlink_metadata`, not `metadata`: A-11 names it, and the CWD walk that
    // produced the ledger rejects a symlinked candidate too (A-12).
    std::fs::symlink_metadata(recorded_dir.join("ocx.toml")).is_ok_and(|meta| meta.file_type().is_file())
}

/// C-007 rule (a) at the **one** place the carrier's `dir` reaches the
/// filesystem.
///
/// `plan.rs` cannot host this guard: `plan` reads neither identity label and
/// constructs no path at all, so a forged-`dir` test there is invariant by
/// construction and green in every state — it names a guarantee it cannot
/// exercise. The constructor is here.
/// The one derivation of a project's identity (finding 23).
#[cfg(test)]
mod identity_tests {
    use super::ProjectIdentity;

    /// `key` is the `state/projects/<key>/` stamp key, and it must be A-30's
    /// canonical directory hashed — not the walked path.
    ///
    /// The prompt and `ocx shell state` each used to run
    /// `canonical_project_dir` then `name_for_path` themselves, so the stamp
    /// key had two derivations that agreed only as long as nobody edited one.
    /// This pins what the surviving one answers.
    ///
    /// Asserted through a symlinked approach to the same project, because that
    /// is where a non-canonical derivation diverges: both paths name one
    /// directory, so both must produce one key and one `dir`, while
    /// `config_path` keeps the route the caller actually selected.
    ///
    /// Red state: drop the `canonical_project_dir` hop and hash
    /// `config_path.parent()` — the two keys stop matching.
    #[tokio::test]
    #[cfg(unix)]
    async fn a030_the_stamp_key_is_the_canonical_directory_not_the_walked_one() {
        let home = tempfile::tempdir().expect("tempdir");
        let real = home.path().join("real");
        std::fs::create_dir_all(&real).expect("mkdir real");
        std::fs::write(real.join("ocx.toml"), "[tools]\n").expect("write ocx.toml");

        let link = home.path().join("via-link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let direct = ProjectIdentity::resolve(real.join("ocx.toml"))
            .await
            .expect("resolve the real path");
        let linked = ProjectIdentity::resolve(link.join("ocx.toml"))
            .await
            .expect("resolve through the symlink");

        assert_eq!(
            direct.key, linked.key,
            "one directory reached two ways is one project, so it is one stamp key"
        );
        assert_eq!(
            direct.dir, linked.dir,
            "A-30: `dir` is canonical, whichever route found it"
        );
        assert_ne!(
            direct.config_path, linked.config_path,
            "the selected route is the caller's answer and is kept verbatim"
        );
    }

    /// A path that will not canonicalize is *indeterminate*, not *absent* —
    /// both callers degrade rather than concluding "no project here".
    ///
    /// Red state: make `resolve` return `Ok` with an uncanonicalized `dir`.
    #[tokio::test]
    async fn a011_an_unresolvable_path_is_an_error_rather_than_an_identity() {
        let home = tempfile::tempdir().expect("tempdir");
        let error = ProjectIdentity::resolve(home.path().join("absent").join("ocx.toml"))
            .await
            .expect_err("a path that does not exist has no identity");
        assert!(
            error.to_string().contains("could not canonicalize"),
            "the message must name what failed: {error}"
        );
    }
}

#[cfg(test)]
mod forged_dir_tests {
    use std::path::Path;

    use super::walk_is_indeterminate;

    /// A recorded `dir` that is not absolute is refused before it can be
    /// joined onto.
    ///
    /// Asserted through the **unreadable-CWD** arm, which returns `true`
    /// (retain) for any recorded scope, so removing the absoluteness check
    /// flips this without needing the process's working directory to hold an
    /// `ocx.toml`. That matters: `Path::new("").join("ocx.toml")` is relative,
    /// so the real defect's observability depends on where the test binary
    /// happens to run, and a check that green because of `$PWD` is not a check.
    ///
    /// Red state: delete the `!recorded_dir.is_absolute()` early return and
    /// `""` retains the scope.
    ///
    /// EC-LEDGER-008 — rule (a) at the one place `dir` reaches the filesystem.
    #[test]
    fn c007a_a030_a_relative_recorded_dir_is_refused_before_it_names_a_file() {
        assert!(
            !walk_is_indeterminate(Some(Path::new("")), None),
            "an empty `dir` is a prefix of every path and joins relative to $PWD; it must never retain"
        );
        assert!(
            !walk_is_indeterminate(Some(Path::new("../../../../etc")), None),
            "a relative `dir` is forged or corrupt, and the fail-safe answer is to revert"
        );

        // The twin: the same call shape with an honest absolute `dir` does
        // retain, so the two refusals above are the absoluteness guard and not
        // the `None` CWD.
        //
        // Spelled per platform because `is_absolute` is what the guard tests
        // and the two platforms disagree about it: `/work/acme` is *rooted* on
        // Windows but not absolute — absolute there needs a drive or UNC
        // prefix — so a POSIX-only literal makes this twin fail on Windows for
        // the guard's own reason, and the refusals above stop proving anything.
        #[cfg(windows)]
        let honest = Path::new(r"C:\work\acme");
        #[cfg(not(windows))]
        let honest = Path::new("/work/acme");
        assert!(
            walk_is_indeterminate(Some(honest), None),
            "an unreadable CWD leaves the walk unknowable, and a recorded scope is retained (A-11)"
        );
    }

    /// The join is bounded to the live CWD's own ancestry: a `dir` the CWD is
    /// not under never reaches `symlink_metadata` at all.
    ///
    /// Red state: drop the `cwd.starts_with(recorded_dir)` check and the
    /// assertion below flips, because `/etc/ocx.toml` would then decide whether
    /// a shell in an unrelated directory keeps a project applied.
    ///
    /// EC-LEDGER-008 — rule (a) at the one place `dir` reaches the filesystem.
    #[test]
    fn c007a_the_probe_never_names_a_path_outside_the_cwds_ancestry() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let elsewhere = home.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("mkdir elsewhere");

        // A real, absolute, `ocx.toml`-bearing directory the CWD is NOT under.
        let unrelated = home.path().join("unrelated");
        std::fs::create_dir_all(&unrelated).expect("mkdir unrelated");
        std::fs::write(unrelated.join("ocx.toml"), "[tools]\n").expect("write ocx.toml");

        assert!(
            !walk_is_indeterminate(Some(&unrelated), Some(&elsewhere)),
            "a recorded directory the CWD has left is a genuine leave, however alive its file still is"
        );
        // The twin: the identical fixture, with the CWD inside it, retains — so
        // the refusal above is the ancestry bound and not a missing file.
        let inside = unrelated.join("src");
        std::fs::create_dir_all(&inside).expect("mkdir src");
        assert!(
            walk_is_indeterminate(Some(&unrelated), Some(&inside)),
            "a live recorded scope under the CWD is retained (A-11)"
        );
    }
}

/// ARCH-16 — `shell/` must not reach for `project/`.
///
/// This module exists because it did. `project::consent` reads
/// `shell::coexistence` and `shell::reconcile`, so a single `use crate::project`
/// anywhere under `shell/` closes a module cycle that will not compile once
/// `ocx_lib` splits ([ocx-sh/ocx#313](https://github.com/ocx-sh/ocx/issues/313)).
/// Nothing about that failure is visible to `cargo check` today — the whole
/// point of the finding — so the only thing that stops it growing back is a
/// guard over the source text.
///
/// A **directory walk**, not a list of [`include_str!`]s: the cycle came back
/// through a file that did not exist when the rule was written, and an
/// enumerated list is blind to exactly that.
#[cfg(test)]
mod no_project_dependency_under_shell {
    use std::path::{Path, PathBuf};

    /// Assembled, never written out, so the needle does **not** appear verbatim
    /// in this file. The twin below scans `activation.rs` itself, and a literal
    /// here would make the scanner match its own source in every state — a
    /// detector measuring itself, which returns the same answer whether or not
    /// the thing it looks for exists.
    const IMPORT_NEEDLE: &str = concat!("crate", "::project");

    /// `//`-prefixed lines are stripped first: a doc link naming the module is
    /// the right thing for a comment to do, and a guard that trips on it
    /// teaches people to stop documenting.
    fn imports_project(source: &str) -> bool {
        source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .any(|line| line.contains(IMPORT_NEEDLE))
    }

    fn rust_sources(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir)
                .expect("the shell source tree is readable")
                .flatten()
            {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    files.push(path);
                }
            }
        }
        files
    }

    fn source_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    #[test]
    fn shell_does_not_import_project() {
        let source_root = source_root();
        let mut files = rust_sources(&source_root.join("shell"));
        files.push(source_root.join("shell.rs"));

        let offenders: Vec<&Path> = files
            .iter()
            .filter(|path| imports_project(&std::fs::read_to_string(path).expect("a shell source file is readable")))
            .map(PathBuf::as_path)
            .collect();
        assert!(
            offenders.is_empty(),
            "`project` reads `shell`, so these files close a module cycle: {offenders:?} — application-layer \
             sequencing belongs in `crate::activation`, not under `shell/`"
        );

        // The non-vacuity twin. `activation.rs` is the file the offending
        // imports moved into, so the very same scanner must still find them
        // there — otherwise the assertion above is green because the needle
        // stopped matching, not because the cycle is gone.
        assert!(
            imports_project(&std::fs::read_to_string(source_root.join("activation.rs")).expect("readable")),
            "the scanner no longer recognises a `crate::project` import, so the assertion above proves nothing"
        );
        assert!(
            files.len() > 1,
            "the walk found no shell sources, so it scanned nothing"
        );
    }
}

/// The clause-2 evidence gate on [`consent_evidence`] (C-025, C-044).
///
/// The prompt path pays for [`consent::verified_sources`] on **every** prompt,
/// and its answer is unobservable to [`consent::evaluate_with_stamp`] unless a
/// `namespaces` grant exists. These tests pin both halves: the read is skipped
/// when it cannot matter, and it still happens — with the same answer — when it
/// can.
#[cfg(test)]
mod consent_evidence_tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    use crate::file_structure::{PackageStore, record_origin};
    use crate::project::consent;
    use crate::project::{DECLARATION_HASH_VERSION, LockMetadata, LockVersion, LockedTool, ProjectLock};
    use crate::trust::ScopeSpec;
    use crate::{ConsentScopeSpec, ShellConsent, oci};

    use super::consent_evidence;

    /// The org every fixture below both claims and genuinely resolved from, so
    /// a corroborated read has exactly one answer.
    const GRANTED_SOURCE: &str = "ocx.sh/acme-corp";

    /// The digest the fixture lock pins and the fixture store materializes.
    const LEAF_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn host() -> oci::Platform {
        "linux/amd64".parse().expect("valid host platform")
    }

    fn leaf_digest() -> oci::Digest {
        oci::Digest::Sha256(LEAF_HEX.to_owned())
    }

    fn identifier(registry: &str, repository: &str) -> oci::Identifier {
        oci::Identifier::new_registry(repository, registry)
    }

    /// A one-tool lock claiming `repository` at [`leaf_digest`] for the host.
    fn lock_claiming(repository: &str) -> ProjectLock {
        let (registry, path) = repository
            .split_once('/')
            .expect("fixture repository carries a registry");
        ProjectLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V3,
                declaration_hash_version: DECLARATION_HASH_VERSION,
                declaration_hash: String::new(),
                generated_by: String::new(),
                generated_at: String::new(),
            },
            tools: vec![LockedTool {
                name: "cmake".into(),
                group: "default".into(),
                repository: identifier(registry, path),
                platforms: BTreeMap::from([(host().to_string(), leaf_digest())]),
            }],
        }
    }

    /// Materialize the leaf package and record `repository` as an origin it was
    /// genuinely fetched under — through the **production** writer, so the
    /// fixture cannot mint a marker shape production never writes.
    async fn materialize_from(store: &PackageStore, repository: &str) {
        let (registry, path) = repository
            .split_once('/')
            .expect("fixture repository carries a registry");
        let pinned = oci::PinnedIdentifier::try_from(identifier(registry, "any/repo").clone_with_digest(leaf_digest()))
            .expect("a digest-bearing identifier is pinned");
        let package = store.package_dir(&pinned);
        std::fs::create_dir_all(package.content()).expect("materialize content/");
        record_origin(&package, &identifier(registry, path))
            .await
            .expect("record the pull origin");
    }

    fn namespaces_grant(pattern: &str) -> ShellConsent {
        ShellConsent {
            namespaces: Some(ConsentScopeSpec(ScopeSpec::Set {
                include: vec![pattern.to_owned()],
                exclude: Vec::new(),
            })),
            ..ShellConsent::default()
        }
    }

    fn granted_set() -> BTreeSet<String> {
        BTreeSet::from([GRANTED_SOURCE.to_owned()])
    }

    /// A store that genuinely corroborates the lock, so `None` from the hop can
    /// only ever mean "not read".
    async fn corroborating_fixture() -> (tempfile::TempDir, PackageStore, ProjectLock) {
        let home = tempfile::TempDir::new().expect("tempdir");
        let store = PackageStore::new(home.path());
        materialize_from(&store, "ocx.sh/acme-corp/cmake").await;
        let lock = lock_claiming("ocx.sh/acme-corp/cmake");
        assert_eq!(
            consent::verified_sources(&lock, &host(), &store),
            Some(granted_set()),
            "the fixture must be corroborable, or a `None` below proves nothing about the gate"
        );
        (home, store, lock)
    }

    /// C-044 — with no `namespaces` grant configured, the per-prompt hop does
    /// **not** read the package store's origin records.
    ///
    /// The store here genuinely corroborates the lock, so the `None` is the
    /// gate and not an empty answer. The second half is the non-vacuity pair: a
    /// grant makes the very same hop read the very same records, so a gate stuck
    /// closed cannot pass this test either.
    ///
    /// Red state: make the gate unconditional — replace
    /// `.filter(|_| namespaces_configured)` with `.filter(|_| true)`, which is
    /// exactly the shipped behaviour before this change — and the first
    /// assertion sees `Some({"ocx.sh/acme-corp"})`.
    #[tokio::test]
    async fn c044_clause_two_evidence_is_not_read_without_a_namespaces_grant() {
        let (_home, store, lock) = corroborating_fixture().await;

        let (_, ungranted, _) = consent_evidence(
            Some(lock.clone()),
            &ShellConsent::default(),
            &host(),
            &store,
            "fixture-key",
        )
        .await;
        assert_eq!(
            ungranted, None,
            "with no namespaces grant clause 2 can never fire, so its evidence must not be read at all"
        );

        let (_, granted, _) = consent_evidence(
            Some(lock),
            &namespaces_grant(GRANTED_SOURCE),
            &host(),
            &store,
            "fixture-key",
        )
        .await;
        assert_eq!(
            granted,
            Some(granted_set()),
            "a namespaces grant must still get the record, or the gate has retired clause 2"
        );
    }

    /// The guard rail on the gate: skipping the read cannot move the
    /// [`consent::Decision`].
    ///
    /// `None` and `Some(..)` are genuinely different operands to
    /// [`consent::evaluate_with_stamp`] — `Reason::UncorroboratedNamespace`
    /// carries one in its payload — so equivalence has to be asserted, not
    /// assumed. It holds only because both branches that read `verified` sit
    /// behind `namespace_granted`, which short-circuits on an unset
    /// `whitelist.namespaces`.
    ///
    /// Red state: give `evaluate_with_stamp` a third `verified` branch that is
    /// not behind that short-circuit, and the decisions diverge.
    #[tokio::test]
    async fn c025_the_skipped_clause_two_evidence_cannot_change_the_decision() {
        let (_home, store, lock) = corroborating_fixture().await;
        let no_grant = ShellConsent::default();
        let project = Path::new("/w/fixture");
        let sources = consent::lock_sources(&lock);

        let ungated = consent::verified_sources(&lock, &host(), &store);
        let (_, gated, _) = consent_evidence(Some(lock), &no_grant, &host(), &store, "fixture-key").await;
        assert!(
            ungated.is_some() && gated.is_none(),
            "the two operands must actually differ, or the equivalence below is trivially true"
        );

        assert_eq!(
            consent::evaluate_with_stamp(project, None, Some(&sources), gated.as_ref(), &no_grant),
            consent::evaluate_with_stamp(project, None, Some(&sources), ungated.as_ref(), &no_grant),
            "the evidence the gate withholds is unobservable to the predicate, or the gate is unsound"
        );
    }
}
