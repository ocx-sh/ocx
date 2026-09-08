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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::env::Env;
use crate::oci;
use crate::package::metadata::env::entry::Entry;
use crate::package::metadata::env::modifier::ModifierKind;
use crate::package_manager::tasks::render_toolchain::HealOutcome;
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

/// The session-level `PATH` directories, **in desired-vector order — which is
/// the reverse of their `PATH` order** (C-059, C-060, RUL-62).
///
/// # The order is inverted on purpose, and reusing the sibling slice is the trap
///
/// [`Shell::export_path`](crate::shell::Shell::export_path) *prepends*
/// ([`utility::path::move_to_front`](crate::utility::path::move_to_front)), so
/// the **last** entry a plan emits ends up frontmost on `PATH`. C-060's order,
/// front to back, is [`Self::project_bin`], then [`Self::global_bin`], then
/// [`Self::install_bin`] — so the desired vector runs the other way, and the
/// field declaration order below *is* that vector.
///
/// [`setup::session_path_directories`](crate::setup::session_path_directories)
/// looks like the same list and is not: it documents `directories[0]` as
/// nearest the front, because its consumers build `PATH=d0:d1:$PATH`
/// themselves. Copying that slice here reverses C-060's order with every unit
/// test on the vector still green — which is why the ordering is pinned by
/// asserting the **resulting `PATH` string**, never this vector.
///
/// # Why `install_bin` is backmost, stated honestly
///
/// Because a toolchain that pins `ocx` has to be able to win. D-4 removed the
/// `ShimNameShadowsOcx` refusal so that a project — or the global tier — may
/// pin its own `ocx`, and an ordering that put the installed binary in front of
/// both made that pin unreachable by construction: the name rendered, and
/// nothing could ever resolve it. `ocx` therefore reads the same way as every
/// other name — project, then global, then the installed binary as the floor.
///
/// What used to justify the other order does not survive. A rendered trampoline
/// bakes an **absolute** `ocx` path (`launcher::generate`'s
/// `trampoline_ocx_binary`), so it does not resolve `ocx` through `PATH` at all,
/// and the one remaining bare-name case — a trampoline rendered when both rungs
/// of that ladder declined — is defended where the resolution actually happens
/// rather than by ordering: [`resolve_command_excluding`](crate::env) drops
/// every trampoline directory from its **lookup copy** of `PATH`, and
/// `is_ocx_trampoline` re-checks the resolved answer independently.
///
/// # Why a struct and not a `Vec<PathBuf>`
///
/// The project slot is filled a long way after the other two — after consent,
/// after the C-061 stamp gate, after the C-062 heal. A `Vec` would take a
/// `push` or an `insert(n, …)`, and neither states which end it meant: a `push`
/// is correct for today's order and was silently wrong for the one this file
/// carried until the tiers were re-ordered. A named `Option` field cannot be
/// filled at the wrong position, whatever the order becomes next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPath {
    /// [`setup::ocx_install_bin_path`](crate::setup::ocx_install_bin_path) —
    /// the directory the installed `ocx` itself resolves from. Backmost of the
    /// three on `PATH`, so it is emitted **first**: it is the floor a bare
    /// `ocx` falls back to when no toolchain pins one, never a lid over a
    /// toolchain that does.
    ///
    /// One spelling of this directory, workspace-wide (RUL-63, R-W29): a second
    /// derivation makes `repair_owned_segments` delete one spelling and add the
    /// other on every prompt, because both sit under `$OCX_HOME` and only one of
    /// them is ever in the desired set.
    pub install_bin: PathBuf,

    /// `$OCX_HOME/toolchain/active/bin` — the global rendered toolchain's trampolines.
    /// Between the other two on `PATH`, so it is emitted **second**: a global
    /// pin shadows the installed binary, and a project's pin shadows it in turn.
    ///
    /// [`ToolchainStore::bin`](crate::file_structure::ToolchainStore::bin),
    /// never a literal `toolchain/active/bin` join (C-001).
    pub global_bin: PathBuf,

    /// The consented project's `<home>/toolchain/active/bin`, in `bin` mode only —
    /// `None` in `env` and `none` mode, and `None` in `bin` mode whenever the
    /// C-061 stamp gate withheld it.
    ///
    /// Frontmost of the three on `PATH`, so it is emitted **last**: the most
    /// specific tier that pinned a name is the one that answers for it.
    pub project_bin: Option<PathBuf>,
}

impl SessionPath {
    /// The two **unconditional** session entries (C-059), with the project slot
    /// still empty.
    ///
    /// Both directories are session-level facts rather than activation
    /// decisions, so this is minted before the walk's project is even looked at
    /// and survives every early return [`session`] takes — including the
    /// `activate = "none"` one. Dropping them from the desired set does not mean
    /// "left alone": `owned_prefixes` contains `$OCX_HOME`, both directories sit
    /// under it, and `repair_owned_segments` removes every owned segment the
    /// desired set does not contribute — so an arm that returned early without
    /// them would **delete the session-`PATH` registration `ocx self setup` had
    /// just written**.
    #[must_use]
    pub fn new(file_structure: &file_structure::FileStructure) -> Self {
        Self {
            global_bin: file_structure.toolchain.bin(),
            project_bin: None,
            install_bin: crate::setup::ocx_install_bin_path(file_structure),
        }
    }

    /// The three directories as `PATH` entries, in desired-vector order.
    ///
    /// The one place this struct's field order becomes a sequence — see the type
    /// docs for why that order is the reverse of the `PATH` order (RUL-62), and
    /// why the test that pins it must read the emitted `PATH` string rather than
    /// this vector.
    ///
    /// Every entry is [`ModifierKind::Path`] on the `PATH` key; a directory
    /// whose bytes will not survive the platform's `PATH` grammar is dropped by
    /// [`reconcile::plan`]'s A-10 pass, exactly as any other contributor's is.
    ///
    /// A directory whose bytes are not UTF-8 is skipped here rather than
    /// rendered lossily: `to_string_lossy` would name a *different* directory,
    /// and the repair would then add that one and remove the real one on every
    /// prompt. Skipping is also the answer `repair_owned_segments` already
    /// gives for such a segment — it cannot name it, so it never removes it —
    /// which leaves the two halves agreeing rather than fighting.
    pub(crate) fn entries(&self) -> Vec<Entry> {
        // The desired vector, back to front on `PATH`: see the type docs
        // (RUL-62) for why this order is the reverse of the documented one.
        [
            Some(&self.install_bin),
            Some(&self.global_bin),
            self.project_bin.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter_map(|directory| match directory.to_str() {
            Some(value) => Some(Entry {
                key: "PATH".to_owned(),
                value: value.to_owned(),
                kind: ModifierKind::Path,
                separator: None,
            }),
            None => {
                crate::log::debug!(
                    "Session PATH directory '{}' is not valid UTF-8 and was not emitted",
                    directory.display()
                );
                None
            }
        })
        .collect()
    }
}

/// What one recomposition resolved: the entries each scope wants, the project
/// slot's identity, and every message the prompt owes the user.
pub struct Outcome {
    /// The session-level `PATH` directories (C-059) — desired in **every**
    /// `activate` mode, on every arm, including the ones that compose nothing.
    ///
    /// Not an `Option` and not a `Vec` that an arm could leave empty: the field
    /// is minted in the one [`Outcome`] literal [`session`] builds, above every
    /// early return, which is what makes "unconditional" a property of the type
    /// rather than of a rule each arm has to remember.
    pub session: SessionPath,

    /// The global toolchain tier's entries, applied first (C-018).
    pub global: Vec<Entry>,
    /// The project tier's entries — empty when inert or yielded.
    pub project: Vec<Entry>,

    /// The **second** owned prefix: the resolved toolchain home of the
    /// consented, in-scope project (C-063, RUL-68). `None` on every other arm.
    ///
    /// `owned_prefixes` is a **deletion authority over a live shell's `PATH`** —
    /// `repair_owned_segments` removes every segment under an owned prefix the
    /// desired set does not contribute — so what may be listed is exactly
    /// `$OCX_HOME` plus this. Never the bare `toolchain-dir` root (it holds
    /// *other projects'* homes), never a consent-refused project, never a
    /// project the walk found but did not put in scope.
    ///
    /// Carried on the outcome rather than re-derived at the call site because
    /// the call site cannot: `plan_for`'s caller has a `FileStructure` and a
    /// `Ledger`, and the home depends on the project directory *and* on the
    /// merged `toolchain-dir` tier. Minted only after [`ConsentProof`] is in
    /// hand, which is what keeps "strictly after consent" a property of where
    /// the field is assigned.
    pub owned_home: Option<PathBuf>,

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
/// config the caller already deserialized and the lock the source-set predicate
/// already parsed are passed in instead.
async fn project_entries(
    input: &SessionInput<'_>,
    // C-028 — S1: the proof is *read* here, by `authorized_project_env`, so the
    // project-file `[env]` channel is opened by a value rather than by the
    // absence of a check, and deleting the gate leaves an unused binding that
    // fails the build under `-D warnings`.
    consent: ConsentProof,
    project: &ProjectIdentity,
    config: &crate::project::ProjectConfig,
    // Taken **by value** so it can be moved into `ToolchainLinks` below instead
    // of cloned there. `ProjectLock` derives `Clone` with no `Arc`, so a clone
    // deep-copies every `LockedTool` — two `String`s, an `Identifier` and a
    // platform `BTreeMap` apiece — on every prompt. That is the same cost
    // `consent_evidence` travels in-and-back-out to avoid (see its doc), and
    // this was the one site still paying it. `ActivateMode::Env` is the last
    // reader of the lock in `emit_for_mode`, so moving it costs the caller
    // nothing.
    lock: ProjectLock,
    // The **already ownership-vetted** home from `owned_project_home`, never a
    // second `project_home` call: that helper's own doc records why resolving it
    // twice is a regression, and its `Err` travels to `session`, whose contract
    // is "emit nothing at all" — so one refused `toolchain-dir` would silently
    // no-op the hook in every project, C-059's two global directories included.
    //
    // `None` is C-067's digest lane, and it is the correct answer for exactly
    // the states that produced it: a refused `toolchain-dir`, and a home ocx may
    // not own.
    home: Option<file_structure::ToolchainHome>,
) -> Result<(Vec<Entry>, bool), SessionError> {
    use crate::package_manager::composer::{ComposeRequest, Materialization};
    use crate::project::{DEFAULT_GROUP, compose_tool_set};

    let groups = vec![DEFAULT_GROUP.to_owned()];
    let tools = compose_tool_set(config, Some(&lock), &groups, &[], input.target)?;

    // `LocalOnly`, unconditionally: a prompt must never block on the network,
    // and a tool that is not materialised yet is an omission the next explicit
    // `ocx pull` fixes — never a hung shell.
    let manager = input.manager.offline_view(input.local_index.clone());
    let requests: Vec<ComposeRequest> = tools
        .iter()
        .map(|tool| ComposeRequest {
            identifier: tool.identifier.clone(),
            mode: crate::project::lazy_mode_for_tool(config, &tool.identifier, None, None),
        })
        .collect();
    let roots = manager
        .compose_roots(&requests, input.target, Materialization::LocalOnly, input.concurrency)
        .await?;
    let (env, env_withheld) = authorized_project_env(consent, config, &project.config_path, &groups);
    // C-065/C-070: the `env`-mode hook is a composing emitter, so it heals the
    // groups it is about to emit. That set is `[DEFAULT_GROUP]` here — not
    // C-062's cost-driven narrowing of a *wider* selection, but the actual
    // scope of this composition, which `compose_tool_set` above was given.
    //
    // No CLI tier: the hook has no `--pinned`, so `ocx.toml` and
    // `OCX_TOOLCHAIN_PINNED` decide (C-007).
    let toolchain = home.map(|home| {
        Box::new(crate::package_manager::ToolchainLinks {
            pinned: crate::package_manager::pinned_for_project(None, config),
            home,
            scope: file_structure::RenderStampScope::Project(project.dir.clone()),
            lock,
            groups: groups.clone(),
        })
    });
    let scope = crate::package_manager::EnvScope::Project {
        no_patches: config.no_patches_repositories(),
        env,
        toolchain,
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
    // — may withhold it. Do not put a *consent* gate in front of this.
    //
    // D-3 carves out exactly one gate, and it is not a consent one: the caller
    // resolves `input.global` through the global tier's own `activate`
    // (`ocx_cli::command::self_group::activate`'s `global_prompt_entries`), so
    // `bin` and `none` arrive here as an empty `global` rather than as a
    // withheld one. That is the user's own file deciding for the user's own
    // toolchain — the same authority the project tier has over its own. Nothing
    // else may narrow it, and the gate stays *outside* this function so every
    // arm below still carries whatever `global` the caller resolved.
    //
    // C-059 — and the two session `PATH` directories are minted in the same
    // literal, for the same reason one step out: they are session-level facts,
    // not activation decisions, so every arm below — the yielded one, the
    // consent-refused one, `activate = "none"` — carries them. An arm that
    // dropped them would not "leave them alone"; both sit under `$OCX_HOME`,
    // which is an owned prefix, so the repair would delete the registration
    // `ocx self setup` wrote. Do not move this below any `return`.
    let mut outcome = Outcome {
        session: SessionPath::new(input.file_structure),
        global: input.global.clone(),
        project: Vec::new(),
        owned_home: None,
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
    // Consent said yes: everything the project's own bytes decide happens
    // beyond this point, behind the proof.
    //
    // **Read once.** The lock travels out of `evaluate_consent` rather than
    // being re-read here: two reads of the same file are two different byte
    // sequences the moment a `git checkout` lands between them, and consent
    // deciding on one while composition uses the other is the whole finding.
    let lock_path = lock_path_for(&project.config_path);
    project_contribution(
        &input,
        consent_proof,
        project,
        &lock_path,
        evaluated.into_lock(),
        &mut outcome,
    )
    .await?;
    outcome.slot = Some(project.clone());
    Ok(outcome)
}

/// What one consenting project contributes to this prompt, per its resolved
/// `activate` mode (C-005, C-006, C-061, C-063).
///
/// The **one** post-consent reader of the project's own bytes, and the reason
/// the mode dispatch lives here rather than in [`session`]: `activate` is an
/// `ocx.toml` key, so resolving it needs the deserialization C-028 puts behind
/// the proof. Hoisting the parse into `session` would move that read out from
/// behind the parameter that gates it and leave the ordering guaranteed by
/// statement order alone — which is exactly the shape [`ConsentProof`] exists to
/// replace.
///
/// Writes into `outcome` rather than returning a tuple of four optional things:
/// the three modes contribute to three different fields, and a struct whose
/// every field is `None` on two arms out of three is a worse shape than the
/// mutation.
///
/// # Errors
///
/// The `ocx.toml` parse; the home resolution (C-017–C-019's `toolchain-dir`
/// funnel, or the canonicalisation's own I/O failure);
/// [`LockCurrency`] for the two modes that read the lock; and whatever the
/// composition or the heal raised.
async fn project_contribution(
    input: &SessionInput<'_>,
    // C-028 — the parameter *is* the gate: there is no way to reach the
    // `ocx.toml` deserialization below without having evaluated consent first.
    consent: ConsentProof,
    project: &ProjectIdentity,
    lock_path: &Path,
    lock: Option<ProjectLock>,
    outcome: &mut Outcome,
) -> Result<(), SessionError> {
    use crate::activate::ActivateMode;

    let config = crate::project::ProjectConfig::from_path(&project.config_path).await?;

    // C-063, RUL-91 — the second owned prefix, and it widens for **every**
    // mode, not only `bin`. `<home>/bin` lives outside `$OCX_HOME`, so under a
    // `bin`-only reading a `bin → none` transition would have nothing
    // authorised to remove the segment it had just stopped contributing, and it
    // would strand on `PATH` for the shell's whole life. Assigned here — after
    // the proof, before any mode branch — so "strictly after consent" is a
    // property of where the field is written.
    //
    // `None` is the refusing answer, and it is deliberately **not** an error:
    // stranding a stale `<home>/bin` on `PATH` is strictly safer than owning a
    // prefix the repository chose.
    let home = owned_project_home(input, project, outcome).await;
    outcome.owned_home = home.as_ref().map(|home| home.root().to_path_buf());

    match activate_mode(&config) {
        // C-059 — and this arm still carries the two session directories,
        // because they were minted in `session`'s one `Outcome` literal above
        // every branch. `none` contributes nothing of its own and reads no
        // lock: a lock it never opens cannot be stale for it.
        ActivateMode::None => {}

        ActivateMode::Bin => {
            // A home ocx may not own cannot be put on `PATH` either. The line
            // naming why was already pushed by `owned_project_home`; a second
            // one here would say the same thing twice on one prompt.
            let Some(home) = home else { return Ok(()) };
            let lock = current_lock(lock, &config, lock_path)?;
            match bin_mode_entry(input, project, &home, &lock).await? {
                Some(bin) => outcome.session.project_bin = Some(bin),
                // C-061's two negative outcomes — no stamp, and a mismatch —
                // have one behaviour, so they have one sentence. It rides
                // `messages` rather than `log::debug!` because every emitted
                // hook redirects this process's stderr to `/dev/null`, and
                // S-001 promises the user sees it (RUL-88).
                None => outcome.messages.push(format!(
                    "ocx: {dir}: its toolchain has not been rendered for this lock; run `ocx pull` here",
                    dir = project.dir.display(),
                )),
            }
        }

        ActivateMode::Env => {
            let lock = current_lock(lock, &config, lock_path)?;
            // `home`, not a second resolution: the following lane may only
            // follow a tree this prompt was already willing to own.
            let (entries, env_withheld) = project_entries(input, consent, project, &config, lock, home).await?;
            outcome.project = entries;
            if env_withheld {
                // A namespaces grant is a fleet auto-enabler satisfied by the
                // project's own lock text, so it authorizes packages and not the
                // project file's own `[env]`. Same shape as the inert hint one
                // level up: name the directory, name the gesture that would
                // widen it.
                outcome.messages.push(format!(
                    "ocx: {dir}: its [env] is not applied - a namespaces grant covers packages only; run `ocx pull` \
                     here once, or add the directory to `[shell.consent] paths`",
                    dir = project.dir.display(),
                ));
            }
        }
    }
    Ok(())
}

/// The project's home **when ocx may own it** (C-063, RUL-33, RUL-44).
///
/// `None` is not an error and never propagates one: it says "this prompt widens
/// no deletion authority to this project", which costs a stale `<home>/bin`
/// stranded on `PATH` and buys back the two states below. The two session
/// directories still emit either way, because they are minted one level up.
///
/// # Why the prompt path needs its own call to the render-side guard
///
/// A guard on the write path did nothing for the read path.
/// [`crate::project::resolve_toolchain_home`] builds the default home
/// **lexically** — `<canonical project>/.ocx/toolchain`, no component stat'ed —
/// and [`shell::reconcile::plan`](crate::shell::reconcile::plan) canonicalises
/// each owned prefix, which resolves that final component, then decides
/// ownership by `starts_with`. So a repository committing `.ocx/toolchain` as a
/// symlink to `/` put `/` in the owned set, and `repair_owned_segments` emits a
/// removal for every `PATH` segment the desired set does not contribute — the
/// user's whole ambient `PATH`, on every prompt. `ocx pull` refused to *render*
/// through that link the whole time; nothing refused to *own* it. The refusal
/// runs **before** the assignment, never after.
///
/// A `namespaces` grant needs no gesture from the user at all, so "they
/// consented" is not a mitigation — it is the premise.
///
/// # The other refusing state
///
/// A `toolchain-dir` the config tier declares and C-017–C-019 refuse (exit 78)
/// used to travel out of here as an error, and `session`'s `Err` contract is
/// "emit nothing" — so one bad `[managed]` push turned every prompt in every
/// project into a silent no-op, C-059's directories included. It degrades here
/// for the same reason the `Lock` arm refuses to: a config the user cannot see
/// the effect of is worse than a toolchain that is not activated.
async fn owned_project_home(
    input: &SessionInput<'_>,
    project: &ProjectIdentity,
    outcome: &mut Outcome,
) -> Option<file_structure::ToolchainHome> {
    let home = match project_home(input.config, &project.dir).await {
        Ok(home) => home,
        Err(error) => {
            outcome.messages.push(format!(
                "ocx: {dir}: its toolchain home is unusable ({error}); no toolchain is activated here",
                dir = project.dir.display(),
            ));
            return None;
        }
    };

    let scope = file_structure::RenderStampScope::Project(project.dir.clone());
    let probe = home.clone();
    let refusal = match tokio::task::spawn_blocking(move || {
        package_manager::tasks::render_toolchain::refuse_symlinked_home(&scope, &probe)
    })
    .await
    {
        Ok(verdict) => verdict.err(),
        // The hop dying is not evidence the tree is safe, and this is the one
        // decision in the session where the fail-safe direction is "own
        // nothing".
        Err(error) => Some(crate::error::file_error(home.root(), std::io::Error::other(error))),
    };
    match refusal {
        None => Some(home),
        Some(error) => {
            outcome.messages.push(format!(
                "ocx: {dir}: its toolchain home is not a tree ocx can own ({error}); no toolchain is activated here",
                dir = project.dir.display(),
            ));
            None
        }
    }
}

/// The project's resolved toolchain home (C-002, C-063).
///
/// The `toolchain-dir` funnel is re-run here rather than taken as a parameter:
/// [`SessionInput`] is a plain-value contract, and a bare `Option<&Path>`
/// parameter is the compile-legal bypass R-W20 closed —
/// [`ToolchainRoot`](crate::config::ToolchainRoot) is constructible only by its
/// own resolver, so the validated value cannot be handed in without widening the
/// input type to something that can also be handed the unvalidated one.
///
/// # What a consented prompt actually pays
///
/// One `Config` clone, one `spawn_blocking` hop, and one
/// `dunce::canonicalize` of the project directory — **every** prompt in a
/// consented project, in every mode, because that is where C-063's owned prefix
/// is decided. Only the funnel's own containment `stat`s are conditional on a
/// tier having declared a root; the clone and the canonicalisation are not.
///
/// The clone is the price of the closed type above: `ToolchainRoot::resolve`
/// takes the whole `Config`, its two tiers (`config.toml` then
/// `OCX_TOOLCHAIN_DIR`) are spelled once inside `config.rs`, and extracting
/// just the declared value here would be a second spelling of that ladder — the
/// drift class RUL-63 exists to forbid.
///
/// # Errors
///
/// The funnel's own refusal (exit 78), or the canonicalisation's I/O failure.
async fn project_home(config: &Config, project_dir: &Path) -> Result<file_structure::ToolchainHome, SessionError> {
    let config = config.clone();
    let owned_dir = project_dir.to_path_buf();
    match tokio::task::spawn_blocking(move || {
        let root = crate::config::ToolchainRoot::resolve(&config).map_err(crate::config::error::Error::from)?;
        crate::project::resolve_toolchain_home(&owned_dir, root.as_ref())
    })
    .await
    {
        Ok(home) => Ok(home?),
        // The blocking unit itself dying — a panic, or a shutting-down runtime
        // — is a state the unit under it cannot report, so it is attributed to
        // the path it was resolving.
        Err(error) => Err(crate::error::file_error(project_dir, std::io::Error::other(error)).into()),
    }
}

/// The lock the two lock-reading modes need, current with `config`.
///
/// The pair of refusals `project_entries` used to make inline, lifted so `bin`
/// mode makes the same two in the same words. Both name the state `ocx pull`
/// reports for it, which `session`'s `Err` contract turns into "emit nothing,
/// retain what is applied" rather than a torn-down environment.
///
/// # Errors
///
/// [`LockCurrency::Missing`] — a `paths` grant is the one clause that holds
/// without a readable lock, so this arm is reachable — and
/// [`LockCurrency::Stale`] when the lock's stored hash disagrees with the
/// current `ocx.toml`.
fn current_lock(
    lock: Option<ProjectLock>,
    config: &crate::project::ProjectConfig,
    lock_path: &Path,
) -> Result<ProjectLock, SessionError> {
    let Some(lock) = lock else {
        return Err(LockCurrency::Missing {
            path: lock_path.to_path_buf(),
        }
        .into());
    };
    if crate::project::lock::is_stale(&lock, config) {
        return Err(LockCurrency::Stale {
            lock_path: lock_path.to_path_buf(),
        }
        .into());
    }
    Ok(lock)
}

/// Resolve the `activate` ladder for one toolchain tier (C-005, C-006).
///
/// `cli ▸ file ▸ environment ▸ floor`, and this is the **only** site that
/// resolves it — the site [`Ladder::resolve`](crate::ladder::Ladder::resolve)
/// names as where its review convention starts being checked. Two halves of that
/// convention:
///
/// - the floor is [`ACTIVATE_FLOOR`](crate::activate::ACTIVATE_FLOOR), passed by
///   name, never a re-spelled `ActivateMode::Env`;
/// - the `cli` tier is `None` **as a statement**, not an omission: there is no
///   `--activate` flag by design (C-006/C-042 put the choice in `ocx.toml` and
///   `ocx self setup`).
///
/// The environment tier is the **weakest**, below the file tier, and is read
/// through [`ActivateMode::from_env`](crate::activate::ActivateMode::from_env)
/// so an unrecognised `OCX_TOOLCHAIN_ACTIVATE` warns and falls through to the
/// floor rather than short-circuiting to it.
///
/// Takes the already-deserialized config: the `ocx.toml` parse is C-028's
/// post-consent step, so this cannot become the thing that reads project bytes
/// early.
///
/// # Both tiers, one ladder
///
/// Three production call sites across two tiers, and the tier is decided
/// entirely by *which* `ocx.toml` the caller deserialized:
///
/// - the **project** tier — [`project_contribution`], over a consenting
///   project's own file, after C-028's consent step;
/// - the **global** tier — the per-prompt hook
///   (`ocx_cli::command::self_group::activate`), over `$OCX_HOME/ocx.toml`,
///   which ADR `adr_toolchain_activation.md` D-3 makes the clean-shell case's
///   own control;
/// - **either** tier — `ocx shell state`, over the manifest belonging to the
///   scope it reports. It is the command whose product is *"why does my shell
///   do nothing"*, so it must answer from the resolver the prompt obeyed and
///   never from a second copy of the ladder.
///
/// A tier that re-spelled the ladder rather than calling this would be a second
/// floor no reader of [`crate::activate::ACTIVATE_FLOOR`] can see — the drift
/// [`crate::activate`]'s module doc warns about.
pub fn activate_mode(config: &crate::project::ProjectConfig) -> crate::activate::ActivateMode {
    crate::ladder::Ladder {
        // There is no `--activate` flag, by design (C-006/C-042): the choice
        // lives in `ocx.toml` and in `ocx self setup`. Spelled as a `None`
        // rather than omitted so a reader sees the tier was considered.
        cli: None,
        file: config.activate,
        // The **weakest** tier, below the file tier — an exported
        // `OCX_TOOLCHAIN_ACTIVATE` never overrides a project that states a
        // value. Read through `from_env` so an unrecognised value warns and
        // leaves this tier *absent* rather than short-circuiting to the floor.
        environment: crate::activate::ActivateMode::from_env(),
    }
    .resolve(crate::activate::ACTIVATE_FLOOR)
}

/// C-061's stamp gate: does `bin_dir` still hold exactly what `stamp` recorded?
///
/// **No compose, no metadata read, no network** — one `readdir` over `bin/` plus
/// one `fstatat` per entry, and a content hash only for the entries the cheap
/// comparison already found suspect.
///
/// # What "matches" means (RUL-67 — set equality, both directions)
///
/// [`RenderStamp::names`](crate::file_structure::RenderStamp::names) documents itself as *the on-disk entry set*, and this
/// honours that literally:
///
/// - every name the stamp records must be present, **and**
/// - every name present must be recorded by the stamp.
///
/// A one-way lookup over [`RenderStamp::bin_fingerprint`](crate::file_structure::RenderStamp::bin_fingerprint)'s keys passes the
/// moment every recorded entry matches — which is precisely S-003, where a
/// hostile clone force-commits an extra `bin/cmake` beside a legitimate tree.
/// That file would reach `PATH` through the gate built to stop it.
///
/// # The stat pair gates the hash; it never substitutes for it
///
/// `(size, file id)` from [`BinEntryStamp`](crate::file_structure::BinEntryStamp) decides *what to hash*, never *what
/// to trust*: an entry whose stat pair differs is hashed and compared, an entry
/// whose stat pair agrees is taken as unchanged. **mtime is not consulted**
/// (C-003) — it is forgeable and is preserved by an in-place overwrite, so it
/// reports "unchanged" for exactly the edit the stamp exists to notice. The
/// accepted residual of the cheap half is on [`BinEntryStamp`](crate::file_structure::BinEntryStamp) itself (R-W4):
/// a same-name, same-size, same-inode in-place overwrite passes.
///
/// A `file_id` of `None` — the platform or filesystem would not report one —
/// falls through to the content hash rather than guessing.
///
/// # Not this function's job
///
/// The **identity** check — that the stamp's [`RenderStamp::scope`](crate::file_structure::RenderStamp::scope) names this
/// project and its `home` names this home — belongs to the caller
/// ([`bin_mode_entry`]), which is the level that holds both. Two projects
/// colliding in the 64 bits of `name_for_path` under one `toolchain-dir` share a
/// stamp, and this function compares a directory against a stamp it is handed;
/// it cannot know whose.
///
/// # Never an error
///
/// An unreadable `bin/`, an entry that vanished mid-walk, a name that is not
/// UTF-8 — every one of them is a **mismatch**, which is the fail-closed answer:
/// the entry is withheld and `PATH` does not change. A prompt has nothing useful
/// to do with an `Err` here, and C-061's two negative outcomes ("no stamp" and
/// "a mismatch") already collapse to one behaviour.
pub(crate) async fn bin_stamp_matches(bin_dir: &Path, stamp: &file_structure::RenderStamp) -> bool {
    let bin_dir = bin_dir.to_path_buf();
    let recorded = stamp.bin_fingerprint.clone();
    // One blocking unit for the whole walk: this is the per-prompt path, so a
    // `read_dir` plus one `fstatat` per entry must not run on the runtime.
    match tokio::task::spawn_blocking(move || bin_matches_recorded(&bin_dir, &recorded)).await {
        Ok(matched) => matched,
        Err(error) => {
            // The join failing is one more condition with nothing useful to
            // report at a prompt, so it takes the same fail-closed answer every
            // filesystem condition takes.
            tracing::debug!("the render-stamp comparison task failed: {error}");
            false
        }
    }
}

/// [`bin_stamp_matches`]' blocking half — the walk itself.
///
/// Split out so the `spawn_blocking` closure stays one call, and so the
/// early-return-per-condition shape reads as the fail-closed ladder it is:
/// every `return false` is a mismatch, never an error.
fn bin_matches_recorded(bin_dir: &Path, recorded: &BTreeMap<String, file_structure::BinEntryStamp>) -> bool {
    // `read_dir` **follows** a symlinked `bin/`, so a `bin` replaced by a link
    // after a successful render enumerates the link's target and every entry
    // below can match — putting a directory outside the 0700 home on `PATH`.
    // `owned_project_home` refuses the same shape one step earlier; this is the
    // second, independent guard, on the function that actually reads the tree.
    if std::fs::symlink_metadata(bin_dir).is_ok_and(|metadata| metadata.is_symlink()) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(bin_dir) else {
        return false;
    };

    let mut seen = 0usize;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        // RUL-67, the direction a lookup over the stamp's own keys misses: an
        // on-disk entry the stamp does not name is a mismatch. S-003's hostile
        // clone force-commits exactly one such file beside a legitimate tree.
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return false;
        };
        let Some(recorded) = recorded.get(&name) else {
            return false;
        };

        // `symlink_metadata`, never `metadata`: the stamp's producer records
        // regular files, so a link under a recorded name is an entry the stamp
        // cannot describe — and following it would read *through* a
        // repository-controlled link into an arbitrary path. The `is_file()`
        // decision below is also what licenses `from_metadata` to re-open the
        // path for its file identity on Windows: the name has been judged not
        // to be a link, and the re-open refuses to traverse one anyway.
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }

        // The cheap half of the gate: `(size, file id)`, both derived through
        // the one function that owns `file_id`'s platform split, so the
        // producing and the comparing end cannot disagree. The hash slot is
        // seeded from the record precisely because the hash is not what this
        // comparison asks — equality here means "size and file id both agree".
        //
        // mtime is not read, here or anywhere below (C-003): it is forgeable
        // and is preserved by an in-place overwrite, so it reports "unchanged"
        // for exactly the edit the stamp exists to notice.
        let stat_pair = file_structure::BinEntryStamp::from_metadata(&path, &metadata, recorded.content_hash.clone());
        // A `file_id` the platform or filesystem would not report falls
        // **through** to the content hash rather than being guessed either way.
        if stat_pair.file_id.is_some() && stat_pair == *recorded {
            seen += 1;
            continue;
        }

        // The stat pair decided *what* to hash; the hash decides what to trust.
        let Some(hashed) = file_structure::BinEntryStamp::of_file(&path, &metadata) else {
            return false;
        };
        if hashed.content_hash != recorded.content_hash {
            return false;
        }
        seen += 1;
    }

    // RUL-67's other direction: every recorded name had to be met. Counting
    // rather than re-walking works because each on-disk entry matched a
    // *distinct* recorded key — `read_dir` yields no name twice.
    seen == recorded.len()
}

/// The `bin`-mode arm: the project `PATH` entry, or `None` and a hint (C-061,
/// C-062, C-064).
///
/// Sequenced, and the order is the contract:
///
/// 1. Read the render stamp for this home's tier
///    ([`StateStore::render_stamp`](crate::file_structure::StateStore::render_stamp)).
///    Absent, unreadable, or at an unrecognised schema version — all of which
///    that accessor already reports as absent — yields `Ok(None)`.
/// 2. Check identity before content: the stamp's `home` must be this home and
///    its [`RenderStampScope`](crate::file_structure::RenderStampScope) must
///    name this canonical project directory (D-V13). Under `toolchain-dir`, two
///    projects colliding in `name_for_path`'s 64 bits share one home *and* one
///    stamp, and the project half is what tells them apart — without it project
///    B's prompt passes over trampolines that bake `--project '<A>'`.
/// 3. Two gates, in order. First C-080: `<home>/active` must be the derived
///    link, or the value step 5 returns names whatever a repoint chose — the
///    CWE-426 primitive the clause exists to close. Then
///    [`bin_stamp_matches`] over the **physical** `<home>/shells/<shell>/bin`,
///    never through `active`. Either negative yields `Ok(None)`.
/// 4. Only then, C-062: heal the **default group's** links
///    ([`heal_links`](crate::package_manager::tasks::render_toolchain)) so the
///    trampolines the entry is about to expose dereference to the digests the
///    lock names. Strictly after the gate, and strictly after consent — the
///    caller cannot reach here without a [`ConsentProof`].
/// 5. Return the PATH-facing `<home>/active/bin` — **unless the heal refused
///    the tree**, which is one
///    more `None`. A refusal means a symlink on the home root, on `bin/`, or on
///    a component above them, and the return value of this function goes on
///    `PATH`; the repair count is not consulted, only the refusal.
///
/// # C-064 — this path never prunes
///
/// There is no delete here and none may be added. The stale window between a
/// lock change and the next composing trigger is **designed**: emit nothing,
/// print one hint, leave the stale trampolines on disk. A prompt that pruned
/// would be a whole-directory delete inside an attacker-writable tree, running
/// before every command the user types, with no `--dry-run` in front of it.
///
/// # The hint is the caller's
///
/// Both negative outcomes are one `None`, because C-061 gives them one
/// behaviour: withhold the entry, say `ocx pull` once, change nothing. The
/// sentence rides [`Outcome::messages`] rather than `log::debug!` — every
/// emitted hook redirects this process's stderr to `/dev/null`, so a log line
/// here is a line nobody reads.
///
/// # Errors
///
/// Only what the heal raises — a group key from `ocx.lock` that cannot become a
/// path component (exit 78). Every filesystem condition on the gate is a
/// mismatch, not an error.
pub(crate) async fn bin_mode_entry(
    input: &SessionInput<'_>,
    project: &ProjectIdentity,
    home: &file_structure::ToolchainHome,
    lock: &ProjectLock,
) -> Result<Option<PathBuf>, SessionError> {
    use file_structure::{RenderStampScope, RenderStampTarget};

    // 1. This project's tier, never the global one — reading `Global` here is
    //    the `RenderStampTarget` hazard that type is named for. Absent,
    //    unreadable or at an unrecognised `v` all arrive as `None`.
    let state = input.file_structure.state.clone();
    let key = project.key.clone();
    let stamp = match tokio::task::spawn_blocking(move || state.render_stamp(RenderStampTarget::Project(&key))).await {
        Ok(stamp) => stamp,
        Err(error) => {
            tracing::debug!("the render-stamp read task failed: {error}");
            None
        }
    };
    let Some(stamp) = stamp else {
        return Ok(None);
    };

    // 2. Identity before content, both halves (D-V13). `home` alone is not
    //    enough: under `toolchain-dir`, two projects colliding in the 64 bits
    //    of `name_for_path` share one home *and* one stamp, and the scope's
    //    canonical project directory is what tells them apart — without it
    //    project B's prompt passes over trampolines that bake `--project '<A>'`.
    //    The scope alone is not enough either, for a stamp describing some
    //    other tree.
    let scope = RenderStampScope::Project(project.dir.clone());
    if stamp.home != home.root() || stamp.scope != scope {
        return Ok(None);
    }

    // 3a. C-080, before the content gate and for a different question: is
    //     `<root>/active` the derived link? `bin()` — the value step 5 returns
    //     and the value `PATH` receives — resolves *through* it, so a repoint
    //     is an arbitrary-directory-on-PATH primitive (CWE-426). Read-only: a
    //     `false` withholds here and is healed by the render (C-081), because
    //     a prompt must never write. Two syscalls, `lstat` + `readlink`, which
    //     is why this stays inline rather than taking a blocking hop.
    if !home.active_is_valid(file_structure::DEFAULT_SHELL) {
        return Ok(None);
    }

    // 3b. The content gate, over the **physical** directory (C-078, C-080's
    //     second amendment). `lstat` does not follow a path's final component
    //     but does follow every intermediate one, so a gate reading through
    //     `active` would judge whatever `active` currently names — and would
    //     agree with a stamp written through the same link. A mismatch is the
    //     same answer no stamp gives: withhold.
    if !bin_stamp_matches(&home.shell_bin(file_structure::DEFAULT_SHELL), &stamp).await {
        return Ok(None);
    }

    // 4. C-062, and strictly after the gate — a tree the gate refused is never
    //    written to (C-064). The **default group only**: that is the group
    //    `bin/` exposes, and it is the one the stamp's `link_fingerprint`
    //    covers. Widening it would make every non-default repoint mismatch on
    //    every prompt; those groups are C-070's, on the composing side.
    let groups = [crate::project::DEFAULT_GROUP.to_owned()];
    let healed = package_manager::tasks::render_toolchain::heal_links(
        input.file_structure,
        home,
        &scope,
        lock,
        &groups,
        input.target,
    )
    .await
    .map_err(crate::Error::from)?;

    // 5. A refused tree withholds the entry — the same `None` every other
    //    negative outcome takes (C-061), and for a stronger reason than the
    //    others: the heal refuses exactly when the home root, `bin/`, or a
    //    component above them is a symlink, and the value this function
    //    returns is put on `PATH` for every command the user types. Returning
    //    `Some` here would expose an attacker-relocated `bin/` on the strength
    //    of a heal that declined to touch it.
    //
    //    `owned_project_home` takes the same refusal one level up, before this
    //    function is entered — this is the second, independent check, on the
    //    call that actually walks the tree, and it closes the window between
    //    the two. No hint rides it: C-061 gives every negative outcome one
    //    behaviour, and the caller already pushed a message for this shape.
    if let HealOutcome::Refused { reason } = healed {
        tracing::debug!(
            "the toolchain home '{}' was not healed and is not exposed: {reason}",
            home.root().display()
        );
        return Ok(None);
    }

    Ok(Some(home.bin()))
}

/// The union `desired` set: `global ++ session ++ project` (C-018, C-059,
/// RUL-87).
///
/// The session block sits **between** the two tiers, and the position is the
/// contract rather than an arrangement. Entries later in this vector end up
/// nearer the front of `PATH` ([`Shell::export_path`](crate::shell::Shell::export_path)
/// prepends), so front to back a shell reads: the project's composed entries,
/// the project's `bin/`, `$OCX_HOME/toolchain/active/bin`,
/// [`SessionPath::install_bin`], the global tier's composed entries.
///
/// Putting the session block last instead would place the **global** tier's
/// composed entries ahead of the project's own trampoline directory, so a
/// globally installed `cmake` would shadow the project's — the tier inversion
/// C-018 forbids. Putting it first would put both tiers behind the global
/// tier's composed entries, which is the same inversion one tier up.
pub fn desired_entries(outcome: &Outcome) -> Vec<Entry> {
    let mut desired = outcome.global.clone();
    desired.extend(outcome.session.entries());
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

    /// EC-SCOPE-010 — the lock the watch set stats is the lock the loader
    /// reads.
    ///
    /// `shell/` may not import `crate::project` (A-45), so
    /// `reconcile::watch_paths` derives the project's `ocx.lock` from
    /// `config_path.parent()` itself rather than calling
    /// [`crate::project::lock::lock_path_for`]. That is a second derivation of
    /// one location, and two derivations drift. This module is the layer that
    /// can see both, so it holds them together.
    ///
    /// Asserted through a project file that is *not* named `ocx.toml`, because
    /// a fixture named `ocx.toml` agrees with a hardcoded join by accident.
    ///
    /// Red state: derive the member from the canonical directory instead, and
    /// the two spellings part company for any project reached through a
    /// symlink.
    #[test]
    fn the_watch_set_lock_member_is_the_lock_the_loader_reads() {
        let file_structure = crate::file_structure::FileStructure::with_root(std::path::PathBuf::from("/tmp/ocx_home"));
        let config = std::path::Path::new("/work/proj/custom.toml");

        let paths = crate::shell::reconcile::watch_paths(&file_structure, Some(config), None, None);

        assert!(
            paths.contains(&crate::project::lock::lock_path_for(config)),
            "the watched lock must be the one `lock_path_for` names; got: {paths:?}"
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

// ---------------------------------------------------------------------------
// WP-9 — Specify phase
// ---------------------------------------------------------------------------
//
// Written from `plan_toolchain_activation.md` (C-059-C-064, S-001, S-003,
// S-006, R-W4) and `.tmp/hex/w3/wp9-rulings.md` (RUL-62, RUL-67, RUL-68,
// RUL-87, RUL-89), never from a stub body. Every case names the contract it
// traces to and the mutation that must turn it red.

/// C-059, C-060, RUL-62, RUL-87 — what the session directories do to `PATH`.
///
/// **Everything here asserts the resulting `PATH` string, never the desired
/// vector's order** (RUL-62). [`Shell::export_path`](crate::shell::Shell::export_path)
/// prepends, so the desired vector runs backwards relative to `PATH`; a test
/// that asserted the vector would agree with an implementation that copied
/// [`setup::session_path_directories`](crate::setup::session_path_directories)
/// verbatim, which is exactly the inversion C-060 exists to forbid.
///
/// [`Env::apply_entries`] is the fold used here rather than a hand-rolled
/// prepend: `reconcile::plan`'s own `settled_keys` uses it to decide whether
/// the emitted arms would change anything, so "this fold's answer" *is* "what
/// the emitted lines produce" by the same contract the shell arms are held to.
/// The byte-exact end-to-end through a real `bash` lives beside `plan_lines`
/// in `crates/ocx_cli/src/command/self_group/activate.rs`.
#[cfg(test)]
mod session_path_tests {
    use std::path::{Path, PathBuf};

    use crate::env::Env;
    use crate::package::metadata::env::entry::Entry;
    use crate::package::metadata::env::modifier::ModifierKind;
    use crate::shell::reconcile::Ledger;

    use super::{Outcome, ProjectIdentity, SessionPath, desired_entries, next_ledger, plan_for};

    const OCX_HOME: &str = "/tmp/ocx-home";
    const PROJECT_DIR: &str = "/work/acme";
    /// `<project>/.ocx/toolchain` — the default project home, outside `$OCX_HOME`.
    const PROJECT_HOME: &str = "/work/acme/.ocx/toolchain";

    fn install_bin() -> PathBuf {
        PathBuf::from(OCX_HOME).join("symlinks/ocx.sh/ocx/cli/current/content/bin")
    }

    fn global_bin() -> PathBuf {
        PathBuf::from(OCX_HOME).join("toolchain/bin")
    }

    fn project_bin() -> PathBuf {
        PathBuf::from(PROJECT_HOME).join("bin")
    }

    /// A `PATH`-kind entry, the only kind any of this pins.
    fn path_entry(value: &str) -> Entry {
        Entry {
            key: "PATH".to_owned(),
            value: value.to_owned(),
            kind: ModifierKind::Path,
            separator: None,
        }
    }

    fn identity() -> ProjectIdentity {
        ProjectIdentity {
            config_path: PathBuf::from(PROJECT_DIR).join("ocx.toml"),
            dir: PathBuf::from(PROJECT_DIR),
            key: "a1b2c3d4e5f60718".to_owned(),
        }
    }

    /// The two unconditional session entries (C-059), project slot empty —
    /// what `env` and `none` mode both carry.
    fn session() -> SessionPath {
        SessionPath {
            global_bin: global_bin(),
            project_bin: None,
            install_bin: install_bin(),
        }
    }

    /// C-059 — the outcome every arm of `session` builds: the session pair is
    /// present whatever the `activate` mode said.
    fn outcome(session: SessionPath, global: Vec<Entry>, project: Vec<Entry>, owned_home: Option<PathBuf>) -> Outcome {
        Outcome {
            session,
            global,
            project,
            owned_home,
            resolved: true,
            slot: Some(identity()),
            inert: false,
            messages: Vec::new(),
        }
    }

    /// The `PATH` a shell ends up holding after `entries` are applied over
    /// `seed`, as a `Vec` of segments.
    ///
    /// The whole assertion surface of this module. Segments rather than one
    /// string so a failure names the offending position instead of printing
    /// two 300-byte lines and leaving the reader to diff them.
    fn resulting_path(seed: &[&Path], entries: &[Entry]) -> Vec<String> {
        let mut env = Env::clean();
        env.set(
            "PATH",
            std::env::join_paths(seed.iter().map(|path| path.as_os_str())).expect("the seed joins"),
        );
        env.apply_entries(entries);
        let value = env.get("PATH").expect("PATH survives the fold").to_owned();
        std::env::split_paths(&value)
            .map(|segment| segment.to_string_lossy().into_owned())
            .collect()
    }

    fn position(path: &[String], needle: &Path) -> usize {
        let needle = needle.to_string_lossy();
        path.iter()
            .position(|segment| segment.as_str() == &*needle)
            .unwrap_or_else(|| panic!("{needle} must be on PATH; got {path:#?}"))
    }

    // ── C-059 — the live defect ─────────────────────────────────────────────

    /// **C-059, the headline.** `ocx_install_bin_path` sits under `$OCX_HOME`,
    /// so `repair_owned_segments` owns it — and only `ocx self setup`'s startup
    /// stream ever emits it. Unless the desired set contributes it on every
    /// prompt, the **first prompt of any shell deletes the session-`PATH`
    /// registration** the installer just wrote.
    ///
    /// Driven at the deletion authority itself: one reconcile whose desired set
    /// declares some other `PATH` entry (which is what makes `PATH` a key
    /// `repair_owned_segments` examines at all), against a live `PATH` that
    /// carries both session directories.
    ///
    /// Red state — today's `goat`: `desired_entries` returns
    /// `global ++ project` and never splices [`Outcome::session`], so both
    /// directories are owned, uncontributed, and removed.
    #[test]
    fn c059_one_reconcile_never_removes_the_two_session_directories() {
        let live = [
            install_bin(),
            global_bin(),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
        ];
        let mut current = Env::clean();
        current.set(
            "PATH",
            std::env::join_paths(live.iter().map(|path| path.as_os_str())).expect("the seed joins"),
        );

        // `activate = "none"`: the arm that composes nothing, and therefore the
        // sharpest one — C-059 says the session pair survives even here.
        let global = vec![path_entry(&format!("{OCX_HOME}/packages/aa/bb/content/bin"))];
        let applied = outcome(session(), global, Vec::new(), Some(PathBuf::from(PROJECT_HOME)));
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &applied, &Env::clean());

        let plan = plan_for(&ledger, &applied, &[Path::new(OCX_HOME)], &current);

        let removed: Vec<&str> = plan.removes.iter().map(|(_, element, _)| element.as_str()).collect();
        assert!(
            !removed.contains(&install_bin().to_string_lossy().as_ref()),
            "C-059: the install directory is a session-level fact, not an activation decision — \
             removing it strips what `ocx self setup` registered; got removes {removed:#?}"
        );
        assert!(
            !removed.contains(&global_bin().to_string_lossy().as_ref()),
            "C-059: `$OCX_HOME/toolchain/bin` is the other unconditional entry; got removes {removed:#?}"
        );
    }

    /// C-059 — and the survival is not passive: the desired set *contributes*
    /// both directories, so a shell that lost them (a `PATH` overwritten by a
    /// login script, C-006's lost-ledger case) gets them back.
    ///
    /// Red state: make [`SessionPath::entries`] emit only what the mode
    /// selected, or drop the splice from `desired_entries`.
    #[test]
    fn c059_a_path_that_lost_both_session_directories_gets_them_back() {
        let applied = outcome(session(), Vec::new(), Vec::new(), Some(PathBuf::from(PROJECT_HOME)));
        let path = resulting_path(&[Path::new("/usr/bin")], &desired_entries(&applied));

        position(&path, &install_bin());
        position(&path, &global_bin());
    }

    // ── C-060 / RUL-62 — the order, read off `PATH` ─────────────────────────

    /// **C-060, RUL-62.** Front to back on `PATH`: the project's
    /// `<home>/toolchain/active/bin` (`bin` mode only), then `$OCX_HOME/toolchain/active/bin`,
    /// then `ocx_install_bin_path`.
    ///
    /// The most specific tier that pinned a name answers for it, `ocx`
    /// included — the installed binary is the floor, never a lid (D-4).
    ///
    /// Asserted on the *resulting `PATH`*, which is the whole of RUL-62: the
    /// desired vector runs the other way because `export_path` prepends, so an
    /// implementation that copies `setup::session_path_directories`' slice
    /// verbatim produces the reverse of this and satisfies any assertion made
    /// on the vector.
    ///
    /// Red state: emit [`SessionPath`]'s three fields in the order they are
    /// *documented on `PATH`* rather than in the reverse — i.e. hand
    /// `session_path_directories`' slice to the splice.
    #[test]
    fn c060_rul62_the_resulting_path_leads_with_project_bin_then_global_bin_then_install_bin() {
        let session = SessionPath {
            global_bin: global_bin(),
            project_bin: Some(project_bin()),
            install_bin: install_bin(),
        };
        let path = resulting_path(&[Path::new("/usr/bin")], &session.entries());

        let install = position(&path, &install_bin());
        let project = position(&path, &project_bin());
        let global = position(&path, &global_bin());
        assert!(
            project < global && global < install,
            "C-060: front to back is project bin/ ▸ $OCX_HOME/toolchain/bin ▸ install_bin; got {path:#?}"
        );
    }

    /// C-060 — the project slot is `bin` mode's alone. In `env` and `none` mode
    /// the two global entries are adjacent on `PATH` with nothing between them.
    ///
    /// Red state: emit `<home>/toolchain/active/bin` unconditionally.
    #[test]
    fn c060_without_the_project_slot_the_two_global_entries_are_adjacent() {
        let path = resulting_path(&[Path::new("/usr/bin")], &session().entries());

        assert_eq!(
            position(&path, &install_bin()),
            position(&path, &global_bin()) + 1,
            "C-060: with no project slot the pair is adjacent, global_bin in front; got {path:#?}"
        );
    }

    // ── RUL-87 — where the session block sits in the desired vector ─────────

    /// **RUL-87 (C-018 × C-059).** The desired vector is
    /// `global ++ session ++ project`, so `PATH` front to back reads:
    /// project-composed ▸ project `bin/` ▸ `$OCX_HOME/toolchain/active/bin` ▸
    /// `install_bin` ▸ global-composed.
    ///
    /// The load-bearing half is the **last** comparison: the alternative
    /// splice (`global ++ project ++ session`, or session first) puts the
    /// *global* tier's composed entries ahead of the project's own `bin/`, so
    /// a globally installed `cmake` shadows the project's trampoline for the
    /// same name — the tier inversion C-018 forbids.
    ///
    /// Red state: splice the session block anywhere but between the two tiers.
    #[test]
    fn rul87_the_session_block_sits_between_the_global_and_project_tiers() {
        let global_tool = format!("{OCX_HOME}/packages/gg/global/content/bin");
        let project_tool = format!("{OCX_HOME}/packages/pp/project/content/bin");
        let applied = outcome(
            SessionPath {
                global_bin: global_bin(),
                project_bin: Some(project_bin()),
                install_bin: install_bin(),
            },
            vec![path_entry(&global_tool)],
            vec![path_entry(&project_tool)],
            Some(PathBuf::from(PROJECT_HOME)),
        );

        let path = resulting_path(&[Path::new("/usr/bin")], &desired_entries(&applied));

        let composed_project = position(&path, Path::new(&project_tool));
        let install = position(&path, &install_bin());
        let project = position(&path, &project_bin());
        let global = position(&path, &global_bin());
        let composed_global = position(&path, Path::new(&global_tool));
        assert!(
            composed_project < project && project < global && global < install && install < composed_global,
            "RUL-87: project-composed ▸ project bin/ ▸ global toolchain bin ▸ install_bin ▸ global-composed; \
             got {path:#?}"
        );
    }

    /// RUL-87, stated as the property it exists to defend: a tool the **global**
    /// tier composes can never shadow the project's own trampoline directory.
    ///
    /// Separate from the ordering test above because this is the consequence a
    /// reader must be able to find by name — an ordering assertion that broke
    /// would leave nothing saying *why* the order mattered.
    ///
    /// Red state: any splice putting `global` after the session block.
    #[test]
    fn rul87_a_global_tools_directory_never_precedes_the_projects_trampolines() {
        let global_tool = format!("{OCX_HOME}/packages/gg/global/content/bin");
        let applied = outcome(
            SessionPath {
                global_bin: global_bin(),
                project_bin: Some(project_bin()),
                install_bin: install_bin(),
            },
            vec![path_entry(&global_tool)],
            Vec::new(),
            Some(PathBuf::from(PROJECT_HOME)),
        );

        let path = resulting_path(&[Path::new("/usr/bin")], &desired_entries(&applied));
        assert!(
            position(&path, &project_bin()) < position(&path, Path::new(&global_tool)),
            "RUL-87: a global `cmake` must not win over the project's `cmake` trampoline; got {path:#?}"
        );
    }

    /// **D-3 — the global tier's own `env` → `bin` transition, in one plan.**
    ///
    /// Every limb of this was traced and each is sound on its own: the global
    /// manifest is watch-set member 3 so editing it expires the stat-only
    /// cache, `applied_in_emission_order` chains global first, and
    /// `repair_owned_segments` is a second guard under `$OCX_HOME`. What was
    /// unpinned is the **conjunction** — the plan's six mode-transition
    /// scenarios are all project-tier, and the global tier had no counterpart.
    ///
    /// The two halves fail differently and neither implies the other: an
    /// unretired element leaves a stale `PATH` segment, while an unrestored
    /// constant leaves ocx's `JAVA_HOME` in the shell for its whole life
    /// (C-006 forbids guess-unsetting it, so the prior is the only operand).
    ///
    /// C-059's session pair rides through untouched, which is the third
    /// assertion: `bin` composes nothing *and* keeps the global toolchain
    /// reachable through its trampolines.
    ///
    /// Red state: return `Vec::new()` for `next_ledger`'s `global` — the record
    /// this plan consumes — and both retirements vanish.
    #[test]
    fn d3_leaving_global_env_mode_retires_the_element_and_restores_the_constant() {
        let tool_bin = format!("{OCX_HOME}/packages/gg/global/content/bin");
        let ocx_jdk = format!("{OCX_HOME}/packages/gg/jdk/content");

        // The shell before ocx ever composed — the only place the user's own
        // `JAVA_HOME` is still visible, which is what `next_ledger` captures.
        let mut before = Env::clean();
        before.set("PATH", "/usr/bin");
        before.set("JAVA_HOME", "/usr/lib/jvm");

        // Last prompt: `activate = "env"`, so the global tier contributed both
        // kinds.
        let was_env = outcome(
            session(),
            vec![
                path_entry(&tool_bin),
                Entry {
                    key: "JAVA_HOME".to_owned(),
                    value: ocx_jdk.clone(),
                    kind: ModifierKind::Constant,
                    separator: None,
                },
            ],
            Vec::new(),
            None,
        );
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &was_env, &before);

        // The shell as that prompt left it.
        let mut current = before.clone();
        current.apply_entries(&desired_entries(&was_env));

        // This prompt: `$OCX_HOME/ocx.toml` now states `activate = "bin"`, so
        // `global_prompt_entries` returns an empty vector — and C-059 keeps the
        // session pair in every mode.
        let now_bin = outcome(session(), Vec::new(), Vec::new(), None);
        let plan = plan_for(&ledger, &now_bin, &[Path::new(OCX_HOME)], &current);

        assert_eq!(
            plan.removes,
            vec![("PATH".to_owned(), tool_bin.clone(), None)],
            "the element the global tier stopped declaring is withdrawn, and nothing else is"
        );
        assert_eq!(
            plan.restores,
            vec![("JAVA_HOME".to_owned(), Some("/usr/lib/jvm".to_owned()))],
            "the global scope's own prior is what a retired global constant reverts to"
        );

        let mut after = current.clone();
        for (key, element, _) in &plan.removes {
            let existing = after.get(key).map(std::ffi::OsString::from).expect("the key is set");
            after.set(
                key.as_str(),
                crate::utility::path::remove_segment(&existing, std::ffi::OsStr::new(element.as_str())),
            );
        }
        let segments: Vec<String> = after
            .get("PATH")
            .map(|value| {
                std::env::split_paths(&value)
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            segments.contains(&global_bin().to_string_lossy().into_owned()),
            "C-059: `bin` mode keeps the session directory the global tools resolve through; got {segments:#?}"
        );
        assert!(
            !segments.contains(&tool_bin),
            "and drops the composed one; got {segments:#?}"
        );
    }

    // ── C-063 / RUL-68 — the deletion authority's scope ─────────────────────

    /// **C-063, RUL-68.** `owned_prefixes` is `$OCX_HOME` **plus** the
    /// consented, in-scope project's own home — and the second prefix is what
    /// makes leaving `bin` mode subtractive at all: the project's home sits
    /// under the project directory, not under `$OCX_HOME`, so without it a
    /// stale `<home>/bin` stays on `PATH` for the shell's whole life.
    ///
    /// Red state: assemble `owned` from `$OCX_HOME` alone (today's `goat`),
    /// and the removal disappears.
    #[test]
    fn c063_the_consented_projects_home_is_an_owned_prefix() {
        let mut current = Env::clean();
        current.set(
            "PATH",
            std::env::join_paths([project_bin().as_os_str(), std::ffi::OsStr::new("/usr/bin")]).expect("joins"),
        );

        // `bin` mode last prompt; `none` mode this prompt, so the project slot
        // is empty and `<home>/bin` is no longer contributed.
        let was_bin = outcome(
            SessionPath {
                global_bin: global_bin(),
                project_bin: Some(project_bin()),
                install_bin: install_bin(),
            },
            Vec::new(),
            Vec::new(),
            Some(PathBuf::from(PROJECT_HOME)),
        );
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &was_bin, &Env::clean());
        let now_none = outcome(session(), Vec::new(), Vec::new(), Some(PathBuf::from(PROJECT_HOME)));

        let owned: Vec<&Path> = vec![Path::new(OCX_HOME), Path::new(PROJECT_HOME)];
        let plan = plan_for(&ledger, &now_none, &owned, &current);
        let removed: Vec<&str> = plan.removes.iter().map(|(_, element, _)| element.as_str()).collect();
        assert!(
            removed.contains(&project_bin().to_string_lossy().as_ref()),
            "C-063: with the home owned, a `<home>/bin` no arm contributes is removed; got {removed:#?}"
        );

        let narrow = plan_for(&ledger, &now_none, &[Path::new(OCX_HOME)], &current);
        assert!(
            narrow.removes.is_empty(),
            "the two operands must genuinely differ, or the assertion above is vacuous; got {:#?}",
            narrow.removes
        );
    }

    /// **C-078's routed edge, refuted.** A dangling `active` does not shelter
    /// the physical spelling from the reconciler's repair.
    ///
    /// The concern routed here was that `owned_spellings`
    /// (`shell/reconcile/plan.rs:768-779`) derives its second spelling with
    /// `std::fs::canonicalize`, which fails on a broken link — so with `active`
    /// dangling only the literal spelling would count as owned and a stale
    /// `shells/<shell>/bin` segment would survive on `PATH`.
    ///
    /// It cannot happen, and the reason is what `owned_prefixes` holds:
    /// `$OCX_HOME` and the project's toolchain **home root** (C-063), never a
    /// `bin` directory and never `active`. `is_owned` is a component-wise
    /// `starts_with` against those roots, so both `<root>/active/bin` and
    /// `<root>/shells/<shell>/bin` are owned by their shared prefix, and
    /// canonicalising `<root>` never traverses `<root>/active` at all.
    ///
    /// Driven over a **real** tree rather than this module's synthetic paths,
    /// because the claim is about what `canonicalize` does: the home root
    /// resolves, `active` does not, and the removal happens anyway. The narrow
    /// twin below keeps it from passing for the wrong reason.
    ///
    /// RED: drop the project home from `owned_prefixes` (the `narrow` half), or
    /// re-derive ownership from the resolved `bin()` path instead of the root.
    #[test]
    #[cfg(unix)]
    fn c078_a_dangling_active_does_not_shelter_the_physical_segment_from_the_repair() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("toolchain");
        let home = crate::file_structure::ToolchainHome::new(&root);
        let physical = home.shell_bin(crate::file_structure::DEFAULT_SHELL);
        std::fs::create_dir_all(&physical).expect("the physical tree is creatable");
        std::os::unix::fs::symlink("shells/gone", home.active()).expect("a dangling `active` is creatable");

        // The premise, asserted rather than assumed: this is the state the
        // routed concern is about, and the home root still resolves.
        assert!(
            std::fs::canonicalize(home.active()).is_err(),
            "precondition: `active` must not resolve"
        );
        assert!(
            std::fs::canonicalize(&root).is_ok(),
            "precondition: the owned prefix resolves — `active` is not on its path"
        );

        let mut current = Env::clean();
        current.set(
            "PATH",
            std::env::join_paths([physical.as_os_str(), std::ffi::OsStr::new("/usr/bin")]).expect("joins"),
        );

        // `bin` mode last prompt, `none` this one, so nothing under the home is
        // contributed and every owned segment is stale.
        let was_bin = outcome(
            SessionPath {
                global_bin: global_bin(),
                project_bin: Some(home.bin()),
                install_bin: install_bin(),
            },
            Vec::new(),
            Vec::new(),
            Some(root.clone()),
        );
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &was_bin, &Env::clean());
        let now_none = outcome(session(), Vec::new(), Vec::new(), Some(root.clone()));

        let owned: Vec<&Path> = vec![Path::new(OCX_HOME), root.as_path()];
        let plan = plan_for(&ledger, &now_none, &owned, &current);
        let removed: Vec<&str> = plan.removes.iter().map(|(_, element, _)| element.as_str()).collect();
        assert!(
            removed.contains(&physical.to_string_lossy().as_ref()),
            "C-078: ownership is component-wise against the home root, which a broken `active` \
             cannot reach; got {removed:#?}"
        );

        let narrow = plan_for(&ledger, &now_none, &[Path::new(OCX_HOME)], &current);
        assert!(
            narrow.removes.is_empty(),
            "the two operands must genuinely differ, or the assertion above is vacuous; got {:#?}",
            narrow.removes
        );
    }

    /// C-063 — the negative half, and the security-relevant one:
    /// `owned_prefixes` may never carry the bare `toolchain-dir` root. That
    /// root holds **other projects'** homes, so owning it makes this prompt a
    /// deletion authority over a sibling project's `PATH` segments — a project
    /// this shell never consented to and may never have entered.
    ///
    /// One prompt, one `PATH`, two segments under the same configured root:
    /// this project's home leaves `bin` mode and is withdrawn, the sibling's is
    /// untouched. Asserting both in one plan is what makes the negative half
    /// discriminating — a reconcile that examined nothing satisfies it too, and
    /// the positive half rules that out.
    ///
    /// Red state: pass the configured root to `plan_for` in place of the
    /// resolved home, and the sibling's segment is removed with it.
    #[test]
    fn c063_owning_the_home_never_reaches_a_sibling_projects_home() {
        let root = PathBuf::from("/tmp/toolchains");
        let mine = root.join("acme-a1b2/toolchain");
        let sibling = root.join("other-c3d4/toolchain/bin");

        let mut current = Env::clean();
        current.set(
            "PATH",
            std::env::join_paths([
                mine.join("bin").as_os_str(),
                sibling.as_os_str(),
                std::ffi::OsStr::new("/usr/bin"),
            ])
            .expect("joins"),
        );

        // Last prompt was `bin` mode; this one is not, so `<mine>/bin` is no
        // longer contributed and becomes withdrawable.
        let was_bin = outcome(
            SessionPath {
                global_bin: global_bin(),
                project_bin: Some(mine.join("bin")),
                install_bin: install_bin(),
            },
            Vec::new(),
            Vec::new(),
            Some(mine.clone()),
        );
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &was_bin, &Env::clean());
        let now_none = outcome(session(), Vec::new(), Vec::new(), Some(mine.clone()));

        let plan = plan_for(&ledger, &now_none, &[Path::new(OCX_HOME), mine.as_path()], &current);
        let removed: Vec<&str> = plan.removes.iter().map(|(_, element, _)| element.as_str()).collect();
        assert!(
            removed.contains(&mine.join("bin").to_string_lossy().as_ref()),
            "C-063: this project's own home is owned, so its stale entry is withdrawn; got {removed:#?}"
        );
        assert!(
            !removed.contains(&sibling.to_string_lossy().as_ref()),
            "C-063: `<toolchain-dir>/<other project>/toolchain/bin` is not this prompt's to delete; \
             got removes {removed:#?}"
        );
    }

    // ── The six `activate` transitions ──────────────────────────────────────

    /// The outcome each `activate` mode produces for one consented, in-scope
    /// project — the modelled contract (C-059, C-060, C-061), not a call into
    /// the resolver.
    fn outcome_for(mode: crate::activate::ActivateMode) -> Outcome {
        use crate::activate::ActivateMode;
        let session = SessionPath {
            global_bin: global_bin(),
            project_bin: matches!(mode, ActivateMode::Bin).then(project_bin),
            install_bin: install_bin(),
        };
        let project = match mode {
            // `env` composes; the composed entry is a package-store path.
            ActivateMode::Env => vec![path_entry(&format!("{OCX_HOME}/packages/pp/project/content/bin"))],
            ActivateMode::Bin | ActivateMode::None => Vec::new(),
        };
        outcome(session, Vec::new(), project, Some(PathBuf::from(PROJECT_HOME)))
    }

    /// C-059, C-063 — all six ordered `activate` transitions. In every one of
    /// them the two session directories stay on `PATH`, and the only thing that
    /// moves is the project's own contribution.
    ///
    /// Both `bin → none` and `none → bin` have no coverage anywhere else, and
    /// each has its own named case below; this one exists so a seventh
    /// transition cannot be added without a decision.
    ///
    /// Red state: drop the session splice (every cell fails), or make the
    /// splice conditional on the mode (the `none` destinations fail).
    #[test]
    fn c059_every_activate_transition_keeps_both_session_directories() {
        use crate::activate::ActivateMode::{Bin, Env as EnvMode, None as NoneMode};

        for (from, to) in [
            (EnvMode, Bin),
            (EnvMode, NoneMode),
            (Bin, EnvMode),
            (Bin, NoneMode),
            (NoneMode, EnvMode),
            (NoneMode, Bin),
        ] {
            let before = outcome_for(from);
            let ledger = next_ledger(&Ledger::empty(), "fp-1", &before, &Env::clean());
            let after = outcome_for(to);

            // The `PATH` a shell genuinely holds in the `from` mode, spelled
            // from the contract rather than folded from `desired_entries` — a
            // seed derived from the thing under test cannot red when that
            // thing drops the session pair.
            let mut live: Vec<PathBuf> = vec![install_bin()];
            live.extend(before.session.project_bin.clone());
            live.push(global_bin());
            live.extend(before.project.iter().map(|entry| PathBuf::from(&entry.value)));
            live.push(PathBuf::from("/usr/bin"));
            let mut current = Env::clean();
            current.set(
                "PATH",
                std::env::join_paths(live.iter().map(|path| path.as_os_str())).expect("joins"),
            );

            let owned: Vec<&Path> = vec![Path::new(OCX_HOME), Path::new(PROJECT_HOME)];
            let plan = plan_for(&ledger, &after, &owned, &current);
            let removed: Vec<&str> = plan.removes.iter().map(|(_, element, _)| element.as_str()).collect();
            for kept in [install_bin(), global_bin()] {
                assert!(
                    !removed.contains(&kept.to_string_lossy().as_ref()),
                    "C-059: {from} → {to} must keep {}; got removes {removed:#?}",
                    kept.display()
                );
            }
        }
    }

    /// C-063, C-060 — `bin → none`. The project's trampoline directory is
    /// withdrawn, which only works because the home is an owned prefix.
    ///
    /// Red state: leave `Outcome::owned_home` at `None` (today's `goat`) and
    /// the segment stays on `PATH` forever.
    #[test]
    fn c063_leaving_bin_mode_withdraws_the_projects_trampoline_directory() {
        use crate::activate::ActivateMode::{Bin, None as NoneMode};

        let before = outcome_for(Bin);
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &before, &Env::clean());
        let mut current = Env::clean();
        current.set(
            "PATH",
            std::env::join_paths([project_bin().as_os_str(), std::ffi::OsStr::new("/usr/bin")]).expect("joins"),
        );

        let owned: Vec<&Path> = vec![Path::new(OCX_HOME), Path::new(PROJECT_HOME)];
        let plan = plan_for(&ledger, &outcome_for(NoneMode), &owned, &current);
        let removed: Vec<&str> = plan.removes.iter().map(|(_, element, _)| element.as_str()).collect();
        assert!(
            removed.contains(&project_bin().to_string_lossy().as_ref()),
            "C-063: `bin → none` withdraws `<home>/bin`; got removes {removed:#?}"
        );
    }

    /// C-059, C-060 — `none → bin`. The project's trampoline directory arrives
    /// **in front of** both session entries, and the pair behind it keeps
    /// `$OCX_HOME/toolchain/active/bin` ahead of the installed binary.
    ///
    /// Red state: leave [`SessionPath::project_bin`] empty on the transition and
    /// the directory never arrives at all; or swap the session pair, and a
    /// globally pinned `ocx` stops being reachable.
    #[test]
    fn c060_entering_bin_mode_puts_the_trampoline_directory_in_front_of_the_session_pair() {
        use crate::activate::ActivateMode::{Bin, None as NoneMode};

        let before = outcome_for(NoneMode);
        let path_before = resulting_path(&[Path::new("/usr/bin")], &desired_entries(&before));
        assert!(
            !path_before
                .iter()
                .any(|segment| segment == &project_bin().to_string_lossy()),
            "`none` mode must not put the trampoline directory on PATH; got {path_before:#?}"
        );

        let path_after = resulting_path(&[Path::new("/usr/bin")], &desired_entries(&outcome_for(Bin)));
        let install = position(&path_after, &install_bin());
        let project = position(&path_after, &project_bin());
        let global = position(&path_after, &global_bin());
        assert!(
            project < global && global < install,
            "C-060: `none → bin` inserts in front of the pair; got {path_after:#?}"
        );
    }
}

/// C-061, C-003, RUL-67, R-W4 — the `bin`-mode stamp gate.
///
/// [`bin_stamp_matches`] answers one question: does `<home>/bin` still hold
/// exactly what the render recorded? Everything below drives that answer
/// directly, because the gate is the only thing standing between a
/// repository-controlled directory and a live shell's `PATH` (S-003).
#[cfg(test)]
mod stamp_gate_tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use crate::file_structure::{BinEntryStamp, RenderStamp, RenderStampScope};

    use super::bin_stamp_matches;

    /// A rendered `bin/` under a fresh home, plus the stamp that describes it
    /// exactly — the state every case below perturbs by one step.
    ///
    /// Returns the tempdir (kept alive by the caller), the `bin/` directory and
    /// the matching stamp.
    fn rendered_bin(entries: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, RenderStamp) {
        let home = tempfile::tempdir().expect("tempdir");
        let root = home.path().join("toolchain");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir bin");

        let mut fingerprint = BTreeMap::new();
        for (name, body) in entries {
            let path = bin.join(name);
            std::fs::write(&path, body).expect("write trampoline");
            fingerprint.insert((*name).to_owned(), stamp_of(&path));
        }
        let stamp = RenderStamp::new(
            root,
            RenderStampScope::Project(home.path().join("project")),
            fingerprint,
            BTreeMap::new(),
        );
        (home, bin, stamp)
    }

    /// One entry's stamp, taken from the file as it stands on disk.
    ///
    /// Spelled here rather than reached for in the renderer: the Specify phase
    /// writes from the contract, and `(size, file id, sha256 of the bytes)` is
    /// what [`BinEntryStamp`] documents itself to be.
    fn stamp_of(path: &Path) -> BinEntryStamp {
        let metadata = std::fs::symlink_metadata(path).expect("stat the entry");
        let bytes = std::fs::read(path).expect("read the entry");
        let hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
        BinEntryStamp::from_metadata(path, &metadata, hash)
    }

    /// The positive control every negative case below is measured against. A
    /// gate that answered `false` unconditionally would satisfy each of them.
    #[tokio::test]
    async fn c061_a_bin_directory_that_still_matches_its_stamp_passes() {
        let (_home, bin, stamp) = rendered_bin(&[("cmake", "#!/bin/sh\ncmake\n"), ("ninja", "#!/bin/sh\nninja\n")]);
        assert!(
            bin_stamp_matches(&bin, &stamp).await,
            "C-061: an untouched rendered tree is what the gate is supposed to let through"
        );
    }

    /// **S-003, RUL-67 — the discriminating case.** A fresh clone force-commits
    /// `.ocx/toolchain/shells/default/bin/cmake` beside a legitimately rendered tree. Every
    /// name the stamp records is present and matches; the hostile file is the
    /// one thing the stamp does *not* name.
    ///
    /// This is the case a one-way lookup over `bin_fingerprint`'s keys passes,
    /// and it is the whole reason the gate exists: pass it and the committed
    /// `cmake` reaches `PATH` on the first prompt.
    ///
    /// Red state: compare only "every recorded name is present and matches",
    /// i.e. iterate `stamp.bin_fingerprint` and never enumerate the directory.
    #[tokio::test]
    async fn s003_rul67_an_on_disk_entry_the_stamp_does_not_name_is_a_mismatch() {
        let (_home, bin, stamp) = rendered_bin(&[("ninja", "#!/bin/sh\nninja\n")]);
        std::fs::write(bin.join("cmake"), "#!/bin/sh\nexfiltrate\n").expect("force-commit the hostile entry");

        assert!(
            !bin_stamp_matches(&bin, &stamp).await,
            "RUL-67: `names` is *the on-disk entry set*, so an unrecorded entry is a mismatch — \
             otherwise S-003's committed `bin/cmake` reaches PATH"
        );
    }

    /// RUL-67, the other direction — a name the stamp records that is no longer
    /// on disk. Set equality means both inclusions, and this is the one a
    /// directory-driven comparison would miss.
    ///
    /// Red state: iterate the directory and never check the stamp's names.
    #[tokio::test]
    async fn c061_rul67_a_recorded_entry_that_is_absent_is_a_mismatch() {
        let (_home, bin, stamp) = rendered_bin(&[("cmake", "#!/bin/sh\ncmake\n"), ("ninja", "#!/bin/sh\nninja\n")]);
        std::fs::remove_file(bin.join("ninja")).expect("remove a rendered entry");

        assert!(
            !bin_stamp_matches(&bin, &stamp).await,
            "RUL-67: a recorded name that vanished is a mismatch"
        );
    }

    /// **C-061 — content hashing is the authority; the stat pair only decides
    /// what to hash.** A replacement written through a rename carries a new
    /// inode, so the cheap comparison already disagrees and the entry is
    /// hashed; the hash is what refuses it.
    ///
    /// Red state: treat a stat-pair disagreement as the *answer* — accept the
    /// entry once it is re-stat'ed, or fall back to trusting the recorded hash
    /// without recomputing.
    #[tokio::test]
    async fn c061_a_replaced_entry_of_the_same_size_is_a_mismatch() {
        let (home, bin, stamp) = rendered_bin(&[("cmake", "#!/bin/sh\naaaa\n")]);
        let replacement = home.path().join("staged");
        std::fs::write(&replacement, "#!/bin/sh\nbbbb\n").expect("stage the replacement");
        assert_eq!(
            std::fs::metadata(&replacement).expect("stat").len(),
            std::fs::metadata(bin.join("cmake")).expect("stat").len(),
            "the fixture must keep the size equal, or the gate refuses on size and proves nothing"
        );
        std::fs::rename(&replacement, bin.join("cmake")).expect("swap it in");

        assert!(
            !bin_stamp_matches(&bin, &stamp).await,
            "C-061: same name, same size, new content — the content hash is the authority"
        );
    }

    /// **C-003 — mtime is never consulted.** Touching an entry's modification
    /// time changes nothing the gate reads, so a tree whose bytes are intact
    /// still passes. mtime is forgeable and is *preserved* by an in-place
    /// overwrite, so consulting it would report "unchanged" for exactly the
    /// edit the stamp exists to notice.
    ///
    /// Red state: add mtime to [`BinEntryStamp`]'s comparison, or compare the
    /// directory's own mtime against anything.
    #[tokio::test]
    #[cfg(unix)]
    async fn c003_c061_an_mtime_bump_alone_is_not_a_mismatch() {
        let (_home, bin, stamp) = rendered_bin(&[("cmake", "#!/bin/sh\ncmake\n")]);
        let entry = bin.join("cmake");
        let ahead = std::fs::FileTimes::new()
            .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2_000_000_000));
        std::fs::File::options()
            .write(true)
            .open(&entry)
            .expect("open for times")
            .set_times(ahead)
            .expect("bump mtime");

        assert!(
            bin_stamp_matches(&bin, &stamp).await,
            "C-003: mtime is not a field of the stamp and must not be a field of the comparison"
        );
    }

    /// **R-W4 — the accepted residual, recorded as a decision rather than
    /// discovered as a surprise.** An in-place overwrite (write, no rename)
    /// preserves the inode, and at the same size it changes none of
    /// `(name, size, file id)`. The entry is therefore never re-hashed and the
    /// stat gate lets it through. Nothing on the emit path re-hashes either, so
    /// the window is "until the next `pull`/`add`/`remove`/`lock`/`update`".
    ///
    /// If this case ever needs to fail, OQ-2 has been reopened and C-061's
    /// stat-gate sentence has reverted to an unconditional hash — a contract
    /// change, not a bug fix. **Delete this test in that same change.**
    ///
    /// Red state: hash every entry unconditionally (which is the alternative
    /// OQ-2 priced and declined).
    #[tokio::test]
    #[cfg(unix)]
    async fn r_w4_an_in_place_same_size_overwrite_passes_the_stat_gate() {
        use std::io::{Seek, Write};

        let (_home, bin, stamp) = rendered_bin(&[("cmake", "#!/bin/sh\naaaa\n")]);
        let entry = bin.join("cmake");
        let before = std::fs::metadata(&entry).expect("stat");

        let mut file = std::fs::File::options()
            .write(true)
            .open(&entry)
            .expect("open in place");
        file.rewind().expect("rewind");
        file.write_all(b"#!/bin/sh\nbbbb\n").expect("overwrite in place");
        file.sync_all().expect("flush");
        drop(file);

        let after = std::fs::metadata(&entry).expect("stat");
        assert_eq!(before.len(), after.len(), "the fixture must not change the size");
        assert_eq!(
            std::os::unix::fs::MetadataExt::ino(&before),
            std::os::unix::fs::MetadataExt::ino(&after),
            "the fixture must not change the inode, or it is not testing the residual"
        );
        assert_ne!(
            std::fs::read(&entry).expect("read"),
            b"#!/bin/sh\naaaa\n",
            "the bytes really did change, which is what makes this a residual rather than a no-op"
        );

        assert!(
            bin_stamp_matches(&bin, &stamp).await,
            "R-W4: the stat gate accepts an in-place same-size overwrite; this is the priced residual"
        );
    }

    /// C-061 — a `file_id` the platform would not report falls **through** to
    /// the content hash rather than guessing. Both answers are asserted: intact
    /// bytes pass, changed bytes at the same size do not.
    ///
    /// Red state: treat a `None` `file_id` as "unchanged" (the entry is skipped
    /// and the second half passes), or as "changed" without hashing (the first
    /// half fails).
    #[tokio::test]
    async fn c061_an_absent_file_id_falls_through_to_the_content_hash() {
        let (_home, bin, mut stamp) = rendered_bin(&[("cmake", "#!/bin/sh\naaaa\n")]);
        for entry in stamp.bin_fingerprint.values_mut() {
            entry.file_id = None;
        }
        assert!(
            bin_stamp_matches(&bin, &stamp).await,
            "C-061: no file id means hash it, and the hash agrees"
        );

        std::fs::write(bin.join("cmake"), "#!/bin/sh\nbbbb\n").expect("same-size rewrite");
        assert!(
            !bin_stamp_matches(&bin, &stamp).await,
            "C-061: with no file id the hash is the only authority, and it disagrees"
        );
    }

    /// C-061 — every filesystem condition is a **mismatch**, never an `Err`: a
    /// `bin/` that is not there is the fail-closed answer, because a prompt has
    /// nothing useful to do with an error and C-061's two negative outcomes
    /// already collapse to one behaviour.
    ///
    /// Red state: return `true` for an unreadable directory (fail-open), or
    /// make the signature fallible.
    #[tokio::test]
    async fn c061_an_absent_bin_directory_is_a_mismatch_not_an_error() {
        let (home, _bin, stamp) = rendered_bin(&[("cmake", "#!/bin/sh\ncmake\n")]);
        let absent = home.path().join("no-such-tree").join("bin");

        assert!(
            !bin_stamp_matches(&absent, &stamp).await,
            "C-061: fail closed — withhold the entry and change nothing"
        );
    }

    /// C-061 — an empty stamp over an empty `bin/` is a match, and an empty
    /// stamp over a populated `bin/` is not.
    ///
    /// The degenerate pair, and the second half is S-003 at its sharpest: a
    /// render that produced nothing followed by a clone that committed
    /// everything.
    ///
    /// Red state: short-circuit on an empty `bin_fingerprint`.
    #[tokio::test]
    async fn c061_an_empty_stamp_matches_only_an_empty_bin_directory() {
        let (_home, bin, stamp) = rendered_bin(&[]);
        assert!(
            bin_stamp_matches(&bin, &stamp).await,
            "an empty render over an empty tree is a match"
        );

        std::fs::write(bin.join("cmake"), "#!/bin/sh\nexfiltrate\n").expect("commit into an unrendered tree");
        assert!(
            !bin_stamp_matches(&bin, &stamp).await,
            "RUL-67: nothing was rendered, so nothing may be exposed"
        );
    }

    /// C-061 — a `bin/` entry that is not a regular file is not silently
    /// accepted. The stamp's producer records regular files only, so a symlink
    /// appearing under a recorded name is an entry the stamp cannot describe.
    ///
    /// Red state: follow the link and hash its target, which is a read *through*
    /// a repository-controlled link into an arbitrary path.
    #[tokio::test]
    #[cfg(unix)]
    async fn c061_a_symlink_where_a_trampoline_belongs_is_a_mismatch() {
        let (home, bin, stamp) = rendered_bin(&[("cmake", "#!/bin/sh\ncmake\n")]);
        let elsewhere = home.path().join("decoy");
        std::fs::write(&elsewhere, "#!/bin/sh\ncmake\n").expect("write the decoy");
        std::fs::remove_file(bin.join("cmake")).expect("remove the trampoline");
        std::os::unix::fs::symlink(&elsewhere, bin.join("cmake")).expect("swap in a link");

        assert!(
            !bin_stamp_matches(&bin, &stamp).await,
            "C-061: the gate compares files it stat'ed, never targets it followed"
        );
    }

    /// **C-061 — `bin/` itself, not only what is under it.** `read_dir`
    /// *follows* a symlink, so a `bin` replaced by a link after a successful
    /// render enumerates the link's target: every recorded name is present,
    /// every body hashes the same, and the gate would hand `<home>/bin` — a
    /// directory outside the 0700 home — to `PATH`.
    ///
    /// The render side already refuses this shape (`ensure_home_root`), which is
    /// exactly why it needed pinning here: that guard runs under `ocx pull`, and
    /// the per-prompt path is the more travelled one.
    ///
    /// Red state: drop the `symlink_metadata(bin_dir)` check at the top of
    /// `bin_matches_recorded` — the decoy tree below matches entry for entry.
    #[tokio::test]
    #[cfg(unix)]
    async fn c061_a_symlinked_bin_directory_is_a_mismatch_however_well_its_contents_match() {
        let (home, bin, stamp) = rendered_bin(&[("cmake", "#!/bin/sh\ncmake\n")]);
        // A decoy `bin/` whose single entry is byte-identical to the rendered
        // one, so nothing below the directory can tell the two apart.
        let decoy = home.path().join("decoy");
        std::fs::create_dir_all(&decoy).expect("mkdir the decoy");
        std::fs::write(decoy.join("cmake"), "#!/bin/sh\ncmake\n").expect("write the decoy trampoline");
        std::fs::remove_dir_all(&bin).expect("remove the rendered bin");
        std::os::unix::fs::symlink(&decoy, &bin).expect("swap in a link");

        assert!(
            !bin_stamp_matches(&bin, &stamp).await,
            "C-061: `bin/` is judged by `symlink_metadata` before it is read, or `read_dir` follows the link"
        );
    }

    /// C-061 — the gate reads `<home>/bin` and nothing else. A stamp is
    /// compared against the directory it is handed; whose stamp it is belongs
    /// to [`super::bin_mode_entry`], one level up.
    ///
    /// Red state: re-derive the directory from `stamp.home` inside the gate,
    /// which silently makes the caller's argument decorative.
    #[tokio::test]
    async fn c061_the_gate_reads_the_directory_it_is_given_not_the_stamps_home() {
        let (home, _bin, stamp) = rendered_bin(&[("cmake", "#!/bin/sh\ncmake\n")]);
        let other = home.path().join("other").join("bin");
        std::fs::create_dir_all(&other).expect("mkdir other/bin");

        assert!(
            !bin_stamp_matches(&other, &stamp).await,
            "an empty directory does not match a one-entry stamp, whatever `stamp.home` says"
        );
    }
}

/// C-005, C-006, C-007 — the `activate` ladder's first production resolution.
#[cfg(test)]
mod activate_ladder_tests {
    use crate::activate::{ACTIVATE_FLOOR, ActivateMode};
    use crate::env::keys::OCX_TOOLCHAIN_ACTIVATE;
    use crate::project::ProjectConfig;

    use super::activate_mode;

    /// An `ocx.toml` carrying `activate = <value>` (or none), parsed through
    /// the production reader — C-028's post-consent step, so the fixture uses
    /// the same route the prompt does.
    async fn config_with(activate: Option<&str>) -> (tempfile::TempDir, ProjectConfig) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ocx.toml");
        let declared = activate.map_or_else(String::new, |mode| format!("activate = \"{mode}\"\n"));
        std::fs::write(&path, format!("{declared}\n[tools]\n")).expect("write ocx.toml");
        let config = ProjectConfig::from_path(&path).await.expect("the fixture parses");
        (dir, config)
    }

    /// C-005, C-006 — the file tier outranks the environment tier. Every
    /// less-specific tier is set to a *different* value, so a resolver that
    /// consults them in the wrong order answers something else.
    ///
    /// Red state: order the ladder `environment.or(file)`.
    #[tokio::test]
    async fn c005_c006_the_file_tier_outranks_the_environment_tier() {
        let (_dir, config) = config_with(Some("bin")).await;
        let lock = crate::test::env::lock();
        lock.set(OCX_TOOLCHAIN_ACTIVATE, "none");

        assert_eq!(activate_mode(&config), ActivateMode::Bin, "C-006: `ocx.toml` decides");
    }

    /// C-006 — with the file tier absent the environment tier is consulted.
    ///
    /// Red state: drop the environment tier, and this falls to the floor.
    #[tokio::test]
    async fn c006_an_absent_file_tier_falls_to_the_environment_tier() {
        let (_dir, config) = config_with(None).await;
        let lock = crate::test::env::lock();
        lock.set(OCX_TOOLCHAIN_ACTIVATE, "bin");

        assert_eq!(activate_mode(&config), ActivateMode::Bin);
    }

    /// C-007 — both tiers absent resolves to [`ACTIVATE_FLOOR`], which is
    /// asserted **by name**: the review convention this ladder's first
    /// production site starts enforcing is that the floor is passed as the
    /// constant, never re-spelled as `ActivateMode::Env`.
    ///
    /// Red state: pass a literal floor that later diverges from the constant.
    #[tokio::test]
    async fn c007_both_tiers_absent_resolves_to_the_named_floor() {
        let (_dir, config) = config_with(None).await;
        let lock = crate::test::env::lock();
        lock.remove(OCX_TOOLCHAIN_ACTIVATE);

        assert_eq!(activate_mode(&config), ACTIVATE_FLOOR);
    }

    /// C-006 — an unrecognised `OCX_TOOLCHAIN_ACTIVATE` warns and leaves the
    /// tier *absent*, so the ladder continues rather than short-circuiting.
    /// With no file tier below it, that lands on the floor.
    ///
    /// Red state: read the variable with `FromStr` directly instead of through
    /// [`ActivateMode::from_env`], which turns an unknown value into an error
    /// or a hard default.
    #[tokio::test]
    async fn c006_an_unrecognised_environment_value_falls_through_to_the_floor() {
        let (_dir, config) = config_with(None).await;
        let lock = crate::test::env::lock();
        lock.set(OCX_TOOLCHAIN_ACTIVATE, "always");

        assert_eq!(activate_mode(&config), ACTIVATE_FLOOR);
    }

    /// C-006 — the environment tier never overrides a project that states
    /// `none`, which is the arm a "the env var force-enables it" reading would
    /// break.
    ///
    /// Red state: treat `ActivateMode::None` in the file tier as "unset".
    #[tokio::test]
    async fn c006_a_file_tier_of_none_is_a_stated_value_not_an_absent_one() {
        let (_dir, config) = config_with(Some("none")).await;
        let lock = crate::test::env::lock();
        lock.set(OCX_TOOLCHAIN_ACTIVATE, "bin");

        assert_eq!(activate_mode(&config), ActivateMode::None);
    }
}

/// C-061, C-062, C-064, S-003, S-006 — the `bin`-mode arm end to end.
///
/// [`bin_mode_entry`] sequences stamp identity ▸ the C-061 gate ▸ the C-062
/// heal ▸ the entry, and the order is the contract. Everything below drives
/// that one function against a real tree.
/// Seed the physical `<root>/shells/<shell>/bin` and plant the `active` link a
/// render publishes (C-078, C-081) — **derived**, never spelled.
///
/// The target comes from `ToolchainHome::expected_active_target`, the same
/// derivation C-080's predicate compares against, so a fixture can never plant
/// a link the gate would reject for a reason the test did not intend.
///
/// Every arena that drives [`bin_mode_entry`] needs it: after C-080 the gate
/// withholds on an `active` that is not the derived link, so a fixture that
/// only created directories would make every positive row unreachable.
#[cfg(test)]
fn plant_active(home: &file_structure::ToolchainHome) {
    std::fs::create_dir_all(home.shell_bin(file_structure::DEFAULT_SHELL)).expect("mkdir <root>/shells/<shell>/bin");
    let _ = std::fs::remove_file(home.active());
    crate::symlink::create(
        home.expected_active_target(file_structure::DEFAULT_SHELL),
        home.active(),
    )
    .expect("plant <root>/active");
}

#[cfg(test)]
#[cfg(unix)]
mod bin_mode_entry_tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use crate::file_structure::{
        BinEntryStamp, DEFAULT_SHELL, FileStructure, RenderStamp, RenderStampScope, RenderStampTarget,
    };
    use crate::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};
    use crate::package::metadata::env::entry::Entry;
    use crate::package_manager::{Concurrency, PackageManager};
    use crate::project::{DECLARATION_HASH_VERSION, DEFAULT_GROUP, LockMetadata, LockVersion, LockedTool, ProjectLock};
    use crate::{Config, oci};

    use super::{ProjectIdentity, SessionInput, bin_mode_entry};

    const REGISTRY: &str = "ocx.sh";
    /// Branch A's pin and branch B's pin — S-006's "same path, different lock".
    const DIGEST_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const DIGEST_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn host() -> oci::Platform {
        "linux/amd64".parse().expect("a valid host platform")
    }

    fn identifier() -> oci::Identifier {
        oci::Identifier::new_registry("acme/cmake", REGISTRY)
    }

    fn pinned(hex: &str) -> oci::PinnedIdentifier {
        oci::PinnedIdentifier::try_from(identifier().clone_with_digest(oci::Digest::Sha256(hex.to_owned())))
            .expect("a digest-bearing identifier is pinned")
    }

    /// A one-tool lock in the default group, pinned to `hex` for this host.
    fn lock_pinning(hex: &str) -> ProjectLock {
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
                group: DEFAULT_GROUP.into(),
                repository: identifier(),
                platforms: BTreeMap::from([(host().to_string(), oci::Digest::Sha256(hex.to_owned()))]),
            }],
        }
    }

    /// Everything one `bin`-mode prompt needs: `$OCX_HOME`, a project at
    /// `<tmp>/work/acme`, its home at `<project>/.ocx/toolchain` with one
    /// rendered `bin/cmake` and one `default/cmake` link.
    struct Arena {
        _tmp: tempfile::TempDir,
        file_structure: FileStructure,
        project: ProjectIdentity,
        home: crate::file_structure::ToolchainHome,
    }

    /// What `<root>/active` holds, for [`Arena::corrupt_active`].
    enum ActiveState<'a> {
        Absent,
        RealDirectory,
        LinkTo(&'a Path),
    }

    use super::plant_active;

    impl Arena {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            // Canonical, because `RenderStampScope::Project` is contracted to
            // carry the canonical directory and the heal's own symlink refusal
            // walks the components between the two.
            let base = dunce::canonicalize(tmp.path()).expect("canonicalize the arena");

            let ocx_home = base.join("ocx-home");
            std::fs::create_dir_all(&ocx_home).expect("mkdir $OCX_HOME");
            let file_structure = FileStructure::with_root(ocx_home);

            let dir = base.join("work").join("acme");
            std::fs::create_dir_all(&dir).expect("mkdir the project");
            std::fs::write(dir.join("ocx.toml"), "[tools]\n").expect("write ocx.toml");

            let home = crate::file_structure::ToolchainHome::new(dir.join(".ocx").join("toolchain"));
            plant_active(&home);

            let project = ProjectIdentity {
                config_path: dir.join("ocx.toml"),
                dir: dir.clone(),
                key: "a1b2c3d4e5f60718".to_owned(),
            };
            Self {
                _tmp: tmp,
                file_structure,
                project,
                home,
            }
        }

        /// Write one trampoline into the **physical** `<home>/shells/default/bin`
        /// and answer its stamp — where a render writes (C-078).
        fn render_trampoline(&self, name: &str, body: &str) -> BinEntryStamp {
            let path = self.home.shell_bin(DEFAULT_SHELL).join(name);
            std::fs::write(&path, body).expect("write the trampoline");
            let metadata = std::fs::symlink_metadata(&path).expect("stat the trampoline");
            let hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(body.as_bytes()));
            BinEntryStamp::from_metadata(&path, &metadata, hash)
        }

        /// Point `default/cmake` at `hex`'s package root, materialising it so
        /// the tree is the one a real render leaves behind.
        fn link_default_cmake_at(&self, hex: &str) {
            let target = self.file_structure.packages.path(&pinned(hex));
            std::fs::create_dir_all(target.join("content")).expect("materialize the package root");
            let entry = self.home.entry(DEFAULT_GROUP, "cmake").expect("a valid entry path");
            std::fs::create_dir_all(entry.parent().expect("the group directory")).expect("mkdir the group");
            let _ = std::fs::remove_file(&entry);
            std::os::unix::fs::symlink(&target, &entry).expect("link the entry");
        }

        fn package_root(&self, hex: &str) -> PathBuf {
            self.file_structure.packages.path(&pinned(hex))
        }

        /// Replace `<root>/active` with `state` (C-080's four negative rows).
        ///
        /// Removes whatever is there first, by observed kind, so one helper
        /// serves the link rows and the real-directory row alike.
        fn corrupt_active(&self, state: ActiveState<'_>) {
            let active = self.home.active();
            if std::fs::symlink_metadata(&active).is_ok_and(|meta| meta.is_dir() && !meta.is_symlink()) {
                std::fs::remove_dir_all(&active).expect("remove the directory at active");
            } else {
                let _ = std::fs::remove_file(&active);
            }
            match state {
                ActiveState::Absent => {}
                ActiveState::RealDirectory => {
                    std::fs::create_dir_all(active.join("bin")).expect("mkdir a real active/bin");
                }
                ActiveState::LinkTo(target) => {
                    crate::symlink::create(target, &active).expect("repoint active");
                }
            }
        }

        /// Persist `stamp` under this project's key, the way the renderer does.
        fn persist(&self, stamp: &RenderStamp) {
            self.file_structure
                .state
                .set_render_stamp(RenderStampTarget::Project(&self.project.key), stamp)
                .expect("persist the render stamp");
        }

        /// A stamp describing this home for this project, over `entries`.
        fn stamp(&self, entries: BTreeMap<String, BinEntryStamp>) -> RenderStamp {
            RenderStamp::new(
                self.home.root().to_path_buf(),
                RenderStampScope::Project(self.project.dir.clone()),
                entries,
                BTreeMap::new(),
            )
        }
    }

    /// `(path, kind, bytes, mtime_nsec, inode)` for every node under `root`,
    /// sorted — the "wrote nothing" oracle C-064 needs.
    ///
    /// **Not an empty-output check**: the prompt path emits no output either
    /// way, so the only observable difference between "withheld the entry" and
    /// "withheld the entry after pruning it" is on disk.
    fn snapshot(root: &Path) -> Vec<String> {
        use std::os::unix::fs::MetadataExt as _;

        let mut rows = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let metadata = std::fs::symlink_metadata(&path).expect("stat the node");
                let kind = if metadata.is_symlink() {
                    format!("link->{}", std::fs::read_link(&path).expect("readlink").display())
                } else if metadata.is_dir() {
                    stack.push(path.clone());
                    "dir".to_owned()
                } else {
                    format!(
                        "file:{}",
                        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(
                            std::fs::read(&path).expect("read the node")
                        ))
                    )
                };
                rows.push(format!(
                    "{} {kind} {} {} {}",
                    path.display(),
                    metadata.len(),
                    metadata.mtime_nsec(),
                    metadata.ino()
                ));
            }
        }
        rows.sort();
        rows
    }

    /// A `SessionInput` for one prompt. The manager is offline with an empty
    /// local index because C-061 forbids this path composing anything at all —
    /// a call that reached the network would fail here rather than pass slowly.
    struct Session {
        manager: PackageManager,
        local_index: LocalIndex,
        config: Config,
        target: oci::Platform,
    }

    impl Session {
        fn new(file_structure: &FileStructure) -> Self {
            let local_index = LocalIndex::new(LocalConfig {
                index_store: file_structure.index.clone(),
            });
            let index = Index::from_chained(
                LocalIndex::new(LocalConfig {
                    index_store: file_structure.index.clone(),
                }),
                Vec::new(),
                ChainMode::Offline,
            );
            Self {
                manager: PackageManager::new(file_structure.clone(), index, None, REGISTRY),
                local_index,
                config: Config::default(),
                target: host(),
            }
        }

        fn input<'a>(&'a self, file_structure: &'a FileStructure, global: &'a [Entry]) -> SessionInput<'a> {
            SessionInput {
                global: global.to_vec(),
                manager: &self.manager,
                local_index: &self.local_index,
                concurrency: Concurrency::Unbounded,
                file_structure,
                config: &self.config,
                target: &self.target,
                project: None,
            }
        }
    }

    // ── C-061 — the stamp must be *this* home's and *this* project's ────────

    /// C-061 — no stamp at all withholds the entry. S-001's error column: a
    /// coworker sets `activate = "bin"` and has never run `ocx pull`.
    ///
    /// Red state: emit `<home>/bin` whenever the directory exists.
    #[tokio::test]
    async fn c061_s001_no_stamp_withholds_the_entry() {
        let arena = Arena::new();
        arena.render_trampoline("cmake", "#!/bin/sh\ncmake\n");
        let session = Session::new(&arena.file_structure);

        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_A),
        )
        .await
        .expect("a missing stamp is not an error");

        assert_eq!(entry, None, "C-061: no stamp means no PATH entry and no PATH change");
    }

    /// C-061 — the positive control. Without it every withholding case below is
    /// satisfied by a function that returns `None` unconditionally.
    #[tokio::test]
    async fn c061_a_matching_stamp_emits_the_homes_bin_directory() {
        let arena = Arena::new();
        let cmake = arena.render_trampoline("cmake", "#!/bin/sh\ncmake\n");
        arena.link_default_cmake_at(DIGEST_A);
        arena.persist(&arena.stamp(BTreeMap::from([("cmake".to_owned(), cmake)])));
        let session = Session::new(&arena.file_structure);

        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_A),
        )
        .await
        .expect("a matching stamp is not an error");

        assert_eq!(
            entry,
            Some(arena.home.bin()),
            "C-061: the gate's whole purpose is to let a rendered tree through"
        );
    }

    // ── C-080 / ADR item 43 — the gate withholds on every `active` that is
    //    not the derived link ─────────────────────────────────────────────────

    /// A fully valid arena — matching stamp, healed link — so the *only*
    /// variable in the four rows below is what `<root>/active` holds.
    ///
    /// Shared so each row is one line of setup: a row that rebuilt the fixture
    /// itself could withhold for a reason other than the one it names, and the
    /// positive control (`c061_a_matching_stamp_emits_the_homes_bin_directory`)
    /// proves this same shape emits.
    async fn valid_arena() -> Arena {
        let arena = Arena::new();
        let cmake = arena.render_trampoline("cmake", "#!/bin/sh\ncmake\n");
        arena.link_default_cmake_at(DIGEST_A);
        arena.persist(&arena.stamp(BTreeMap::from([("cmake".to_owned(), cmake)])));
        arena
    }

    async fn entry_of(arena: &Arena) -> Option<PathBuf> {
        let session = Session::new(&arena.file_structure);
        bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_A),
        )
        .await
        .expect("an invalid `active` is a withhold, never an error")
    }

    /// C-080 row 1 — `active` absent. The tree renders trampolines the stamp
    /// still matches, and `bin()` names a path that does not resolve.
    ///
    /// RED: drop the `active_is_valid` call from `bin_mode_entry` — the stamp
    /// gate reads the *physical* directory and passes, so `Some(active/bin)`
    /// is emitted for a directory that does not exist.
    #[tokio::test]
    async fn c080_an_absent_active_withholds_the_entry() {
        let arena = valid_arena().await;
        arena.corrupt_active(ActiveState::Absent);

        assert_eq!(
            entry_of(&arena).await,
            None,
            "C-080: `bin()` resolves through `active`; without it there is nothing to put on PATH"
        );
    }

    /// C-080 row 2 — `active` is a **real directory**, the `cp -rL` / Docker
    /// `COPY` outcome (C-081). The prompt path may only read, so the directory
    /// must still be there afterwards: healing it is the render's job.
    ///
    /// RED: weaken the predicate to "exists" and the row emits; add a heal here
    /// and the surviving-payload assertion fails.
    #[tokio::test]
    async fn c080_a_real_directory_at_active_withholds_the_entry_and_is_not_healed() {
        let arena = valid_arena().await;
        arena.corrupt_active(ActiveState::RealDirectory);

        assert_eq!(
            entry_of(&arena).await,
            None,
            "C-080: a dereferenced copy is not the derived link"
        );
        assert!(
            std::fs::symlink_metadata(arena.home.active())
                .expect("the directory is still there")
                .is_dir(),
            "C-080 is read-only at the prompt (C-064): the heal is the render's, not this path's"
        );
    }

    /// C-080 row 3 — `active` repointed **outside** the home. This is the
    /// CWE-426 primitive the clause exists to close: whatever the link names
    /// would go on `PATH` for every command the user types.
    ///
    /// RED: replace the equality with a containment test and this row still
    /// fails, but replace it with `is_link` alone and it emits an
    /// attacker-owned directory.
    #[tokio::test]
    async fn c080_an_active_repointed_outside_the_home_withholds_the_entry() {
        let arena = valid_arena().await;
        let outside = arena.project.dir.with_file_name("attacker");
        std::fs::create_dir_all(outside.join("bin")).expect("the outside tree is creatable");
        arena.corrupt_active(ActiveState::LinkTo(&outside));

        assert_eq!(
            entry_of(&arena).await,
            None,
            "C-080: a link that escapes the home is an arbitrary-directory-on-PATH primitive"
        );
    }

    /// C-080 row 4 — `active` repointed at a **sibling `shells/<other>` that
    /// exists**, so it is contained, resolvable and still wrong.
    ///
    /// The row a containment test cannot catch, and the reason C-080 is an
    /// equality against a derived value: the target is inside the home and
    /// names a real `bin` directory holding a real executable.
    ///
    /// RED: weaken the predicate to a containment test, or to `is_link` alone,
    /// and this row emits another shell's tree.
    #[tokio::test]
    async fn c080_an_active_repointed_at_a_sibling_shell_withholds_the_entry() {
        let arena = valid_arena().await;
        let sibling = arena.home.shell_bin("other");
        std::fs::create_dir_all(&sibling).expect("the sibling shell tree is creatable");
        std::fs::write(sibling.join("cmake"), "#!/bin/sh\nother\n").expect("write the sibling trampoline");
        let target = sibling.parent().expect("<root>/shells/other");
        let relative = target
            .strip_prefix(arena.home.root())
            .expect("the sibling is under the root")
            .to_path_buf();
        arena.corrupt_active(ActiveState::LinkTo(&relative));

        assert_eq!(
            entry_of(&arena).await,
            None,
            "C-080: contained and resolvable is not the same question as derived"
        );
    }

    /// C-061, D-V13 — identity before content. Under `toolchain-dir` two
    /// projects colliding in `name_for_path`'s 64 bits share one home *and* one
    /// stamp; the scope's project directory is what tells them apart. Without
    /// it, project B's prompt passes over trampolines that bake
    /// `--project '<A>'`.
    ///
    /// Red state: compare `stamp.home` alone and skip
    /// [`RenderStampScope`](crate::file_structure::RenderStampScope).
    #[tokio::test]
    async fn c061_a_stamp_scoped_to_another_project_withholds_the_entry() {
        let arena = Arena::new();
        let cmake = arena.render_trampoline("cmake", "#!/bin/sh\ncmake\n");
        let mut stamp = arena.stamp(BTreeMap::from([("cmake".to_owned(), cmake)]));
        stamp = RenderStamp::new(
            stamp.home.clone(),
            RenderStampScope::Project(arena.project.dir.with_file_name("other")),
            stamp.bin_fingerprint.clone(),
            stamp.link_fingerprint.clone(),
        );
        arena.persist(&stamp);
        let session = Session::new(&arena.file_structure);

        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_A),
        )
        .await
        .expect("an identity mismatch is not an error");

        assert_eq!(
            entry, None,
            "D-V13: a stamp that names another project is not this project's"
        );
    }

    /// C-061 — the `home` half of the identity pair: a stamp describing a
    /// different toolchain home is not this home's, whatever its scope says.
    ///
    /// Red state: compare the scope alone and skip `stamp.home`.
    #[tokio::test]
    async fn c061_a_stamp_for_another_home_withholds_the_entry() {
        let arena = Arena::new();
        let cmake = arena.render_trampoline("cmake", "#!/bin/sh\ncmake\n");
        let stamp = RenderStamp::new(
            arena.home.root().with_file_name("elsewhere"),
            RenderStampScope::Project(arena.project.dir.clone()),
            BTreeMap::from([("cmake".to_owned(), cmake)]),
            BTreeMap::new(),
        );
        arena.persist(&stamp);
        let session = Session::new(&arena.file_structure);

        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_A),
        )
        .await
        .expect("an identity mismatch is not an error");

        assert_eq!(entry, None, "C-061: a stamp for another home describes another tree");
    }

    /// C-061 — a global stamp never stands in for a project's. The two live
    /// under different keys, and reading the wrong one is the
    /// `RenderStampTarget` hazard stated on that type.
    ///
    /// Red state: read `RenderStampTarget::Global` for a project home.
    #[tokio::test]
    async fn c061_a_global_stamp_is_not_this_projects_stamp() {
        let arena = Arena::new();
        let cmake = arena.render_trampoline("cmake", "#!/bin/sh\ncmake\n");
        arena
            .file_structure
            .state
            .set_render_stamp(
                RenderStampTarget::Global,
                &RenderStamp::new(
                    arena.home.root().to_path_buf(),
                    RenderStampScope::Global,
                    BTreeMap::from([("cmake".to_owned(), cmake)]),
                    BTreeMap::new(),
                ),
            )
            .expect("persist the global stamp");
        let session = Session::new(&arena.file_structure);

        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_A),
        )
        .await
        .expect("a missing project stamp is not an error");

        assert_eq!(entry, None, "C-061: the project's tier has no stamp of its own");
    }

    // ── S-003 — the committed hostile `bin/` ────────────────────────────────

    /// **S-003 end to end.** A fresh clone force-commits
    /// `.ocx/toolchain/shells/default/bin/cmake`; consent is granted; the first prompt in
    /// `bin` mode emits **no** `PATH` entry, so the hostile file never reaches
    /// `PATH`.
    ///
    /// The stamp here is a legitimate one over `ninja`, which is what makes
    /// this the RUL-67 case rather than the trivial "no stamp" one: every name
    /// the stamp records is present and matches.
    ///
    /// Red state: a one-way lookup over `bin_fingerprint`'s keys in the gate.
    #[tokio::test]
    async fn s003_a_force_committed_bin_entry_never_reaches_path() {
        let arena = Arena::new();
        let ninja = arena.render_trampoline("ninja", "#!/bin/sh\nninja\n");
        arena.persist(&arena.stamp(BTreeMap::from([("ninja".to_owned(), ninja)])));
        std::fs::write(
            arena.home.shell_bin(DEFAULT_SHELL).join("cmake"),
            "#!/bin/sh\nexfiltrate\n",
        )
        .expect("force-commit");
        let session = Session::new(&arena.file_structure);

        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_A),
        )
        .await
        .expect("a mismatch is not an error");

        assert_eq!(entry, None, "S-003: the committed `bin/cmake` must not reach PATH");
    }

    // ── C-064 — the prompt path never prunes ────────────────────────────────

    /// **C-064.** The stale window between a lock change and the next composing
    /// trigger is designed: emit nothing, leave the stale trampolines on disk.
    /// A prompt that pruned would be a whole-directory delete inside an
    /// attacker-writable tree, running before every command the user types,
    /// with no `--dry-run` in front of it.
    ///
    /// "Wrote nothing" is a `(path, kind, bytes, mtime_nsec, inode)` snapshot of
    /// the whole home, not an empty-output check: the prompt prints nothing
    /// either way.
    ///
    /// Red state: delete or rewrite anything under `<home>` on the mismatch arm.
    #[tokio::test]
    async fn c064_a_withheld_entry_leaves_the_home_byte_for_byte_unchanged() {
        let arena = Arena::new();
        let ninja = arena.render_trampoline("ninja", "#!/bin/sh\nninja\n");
        arena.link_default_cmake_at(DIGEST_A);
        arena.persist(&arena.stamp(BTreeMap::from([("ninja".to_owned(), ninja)])));
        std::fs::write(arena.home.shell_bin(DEFAULT_SHELL).join("cmake"), "#!/bin/sh\nstale\n")
            .expect("the stale trampoline");

        let before = snapshot(arena.home.root());
        assert!(
            before.len() >= 3,
            "the fixture must have something to lose; got {before:#?}"
        );

        let session = Session::new(&arena.file_structure);
        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_B),
        )
        .await
        .expect("a mismatch is not an error");

        assert_eq!(entry, None, "the precondition: this is the withholding arm");
        assert_eq!(
            snapshot(arena.home.root()),
            before,
            "C-064: the prompt path never prunes and never heals on a mismatch"
        );
    }

    /// C-064, C-062 — the heal is **strictly after** the gate. A tree the gate
    /// refuses is not healed either, so a stale `default/cmake` link stays
    /// exactly as it is.
    ///
    /// Separate from the snapshot above because this is the ordering claim: a
    /// heal that ran before the gate would repoint the link and still return
    /// `None`, which the snapshot test would catch but not explain.
    ///
    /// Red state: call `heal_links` before [`super::bin_stamp_matches`].
    #[tokio::test]
    async fn c062_the_heal_never_runs_on_a_tree_the_gate_refused() {
        let arena = Arena::new();
        let ninja = arena.render_trampoline("ninja", "#!/bin/sh\nninja\n");
        arena.link_default_cmake_at(DIGEST_A);
        arena.persist(&arena.stamp(BTreeMap::from([("ninja".to_owned(), ninja)])));
        std::fs::write(
            arena.home.shell_bin(DEFAULT_SHELL).join("cmake"),
            "#!/bin/sh\nhostile\n",
        )
        .expect("force-commit");

        let session = Session::new(&arena.file_structure);
        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_B),
        )
        .await
        .expect("a mismatch is not an error");
        assert_eq!(entry, None, "the precondition: this is the withholding arm");

        let link = arena.home.entry(DEFAULT_GROUP, "cmake").expect("a valid entry path");
        assert_eq!(
            std::fs::read_link(&link).expect("readlink"),
            arena.package_root(DIGEST_A),
            "C-062: heal runs *after* the gate, so a refused tree is never written to"
        );
    }

    // ── C-062 / S-006 — heal before emit ────────────────────────────────────

    /// **C-062 + S-006.** Branch switch at the same path, different lock, no
    /// `ocx pull`. `bin/` is gitignored so the trampolines are unchanged and
    /// the stamp still matches — which is exactly why the default group's links
    /// must be healed **before** the entry is emitted. Without it the
    /// trampolines dereference to the *other* branch's package while the lock
    /// says otherwise.
    ///
    /// Red state: emit `<home>/bin` without healing, or heal a group set that
    /// is not the default group.
    #[tokio::test]
    async fn s006_c062_a_branch_switch_heals_the_default_group_before_emitting() {
        let arena = Arena::new();
        let cmake = arena.render_trampoline("cmake", "#!/bin/sh\ncmake\n");
        // Rendered on branch A…
        arena.link_default_cmake_at(DIGEST_A);
        arena.persist(&arena.stamp(BTreeMap::from([("cmake".to_owned(), cmake)])));
        // …and the package branch B pins is materialised, but nothing points at it.
        std::fs::create_dir_all(arena.package_root(DIGEST_B).join("content")).expect("materialize B");

        let session = Session::new(&arena.file_structure);
        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_B),
        )
        .await
        .expect("the heal succeeds");

        assert_eq!(
            entry,
            Some(arena.home.bin()),
            "S-006: the stamp still matches, so the entry is emitted — after the heal"
        );
        let link = arena.home.entry(DEFAULT_GROUP, "cmake").expect("a valid entry path");
        assert_eq!(
            std::fs::read_link(&link).expect("readlink"),
            arena.package_root(DIGEST_B),
            "S-006: never a silent dereference to the other branch's package"
        );
    }

    /// C-062, RUL-29 — a link the render never wrote is *created* by the heal,
    /// which is the commonest post-`git pull` state.
    ///
    /// Red state: repoint only links that already exist.
    #[tokio::test]
    async fn c062_an_absent_default_group_link_is_created_before_the_entry_is_emitted() {
        let arena = Arena::new();
        let cmake = arena.render_trampoline("cmake", "#!/bin/sh\ncmake\n");
        arena.persist(&arena.stamp(BTreeMap::from([("cmake".to_owned(), cmake)])));
        std::fs::create_dir_all(arena.package_root(DIGEST_A).join("content")).expect("materialize A");

        let session = Session::new(&arena.file_structure);
        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_A),
        )
        .await
        .expect("the heal succeeds");

        assert_eq!(entry, Some(arena.home.bin()));
        let link = arena.home.entry(DEFAULT_GROUP, "cmake").expect("a valid entry path");
        assert_eq!(
            std::fs::read_link(&link).expect("readlink"),
            arena.package_root(DIGEST_A),
            "C-062/RUL-29: the heal creates an absent link"
        );
    }

    /// C-062 — the heal is scoped to the **default group** alone. A
    /// non-default group's stale link is left for the composing side (C-070),
    /// because widening the stamp's `link_fingerprint` would make every
    /// non-default repoint mismatch on every prompt and print the `ocx pull`
    /// hint forever.
    ///
    /// Red state: hand `heal_links` every group in the lock.
    #[tokio::test]
    async fn c062_the_prompt_heals_the_default_group_and_no_other() {
        let arena = Arena::new();
        let cmake = arena.render_trampoline("cmake", "#!/bin/sh\ncmake\n");
        arena.persist(&arena.stamp(BTreeMap::from([("cmake".to_owned(), cmake)])));
        std::fs::create_dir_all(arena.package_root(DIGEST_B).join("content")).expect("materialize B");

        // The same tool, in a group the prompt does not select.
        let mut lock = lock_pinning(DIGEST_B);
        lock.tools[0].group = "ci".into();

        let session = Session::new(&arena.file_structure);
        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock,
        )
        .await
        .expect("the heal succeeds");

        assert_eq!(entry, Some(arena.home.bin()), "the precondition: the gate passed");
        assert!(
            !arena.home.root().join("ci").exists(),
            "C-062: a non-default group is C-070's, not the prompt's; got {:#?}",
            snapshot(arena.home.root())
        );
    }

    /// C-061 — no compose, no metadata read, no network. The manager handed in
    /// is offline over an empty local index, so a path that tried to resolve a
    /// package would fail; asserting the *positive* answer is therefore also
    /// asserting that nothing was resolved.
    ///
    /// Stated as its own case because it is the contract sentence a future edit
    /// is most likely to break by reaching for `compose_roots` to "check the
    /// trampolines are current".
    ///
    /// Red state: compose the tool set on this path.
    #[tokio::test]
    async fn c061_the_gate_composes_nothing() {
        let arena = Arena::new();
        let cmake = arena.render_trampoline("cmake", "#!/bin/sh\ncmake\n");
        arena.persist(&arena.stamp(BTreeMap::from([("cmake".to_owned(), cmake)])));
        // No package root is materialised at all: a compose would miss.
        let session = Session::new(&arena.file_structure);

        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_A),
        )
        .await
        .expect("an unmaterialised package is not this path's problem");

        assert_eq!(
            entry,
            Some(arena.home.bin()),
            "C-061: the stamp is the authority; materialisation is `ocx pull`'s job"
        );
    }

    /// C-062, RUL-47 — the heal's **refusal** withholds the entry, and the
    /// C-061 gate cannot substitute for it.
    ///
    /// The tree is relocated behind a symlinked `<project>/.ocx` *after* the
    /// stamp is written, and moved rather than copied so `bin/`'s inode and
    /// mtime — the two [`BinEntryStamp`] records — survive. The gate therefore
    /// still matches, which the first assertion pins: `bin_stamp_matches`
    /// guards `bin/` being a symlink and nothing above it, so without the heal's
    /// verdict this function would put an attacker-relocated `bin/` on `PATH`
    /// for every command the user types.
    ///
    /// `c061_a_matching_stamp_emits_the_homes_bin_directory` is the positive
    /// control: the same tree, unrelocated, yields `Some`.
    ///
    /// RED: fold the heal's `Refused` back into the healed arm — the entry is
    /// then emitted, because every earlier step passed.
    #[tokio::test]
    async fn a_refused_heal_withholds_the_entry() {
        let arena = Arena::new();
        let cmake = arena.render_trampoline("cmake", "#!/bin/sh\ncmake\n");
        arena.link_default_cmake_at(DIGEST_A);
        let stamp = arena.stamp(BTreeMap::from([("cmake".to_owned(), cmake)]));
        arena.persist(&stamp);

        // `<project>/.ocx` becomes a symlink out of the project — one component
        // above the home root, the hole `ensure_home_root` cannot see from
        // where it stands (RUL-44/RUL-47).
        let dot_ocx = arena.project.dir.join(".ocx");
        let elsewhere = arena.project.dir.with_file_name("elsewhere");
        std::fs::rename(&dot_ocx, &elsewhere).expect("the tree is relocatable");
        std::os::unix::fs::symlink(&elsewhere, &dot_ocx).expect("the hostile link is creatable");

        // The premise, asserted rather than assumed: this test would pass for
        // the wrong reason if the gate rejected the relocated tree, because
        // step 3 returns `None` before the heal is ever reached.
        assert!(
            super::bin_stamp_matches(&arena.home.shell_bin(DEFAULT_SHELL), &stamp).await,
            "the relocation is invisible to the C-061 gate — that is what makes the heal's \
             refusal load-bearing here"
        );

        let session = Session::new(&arena.file_structure);
        let entry = bin_mode_entry(
            &session.input(&arena.file_structure, &[]),
            &arena.project,
            &arena.home,
            &lock_pinning(DIGEST_A),
        )
        .await
        .expect("RUL-36 — a refusal degrades, it never errors");

        assert_eq!(
            entry, None,
            "a tree the heal refused to enter must not be exposed on `PATH`"
        );
    }
}

/// RUL-63, RUL-86, RUL-90, R-W29 — **one directory, one derivation.**
///
/// `$OCX_HOME/symlinks/<ocx cli id>/current/content/bin` is derived in three
/// production places on `goat`:
/// [`setup::ocx_install_bin_path`](crate::setup::ocx_install_bin_path),
/// `package_manager::launcher::generate::trampoline_ocx_binary` (RUL-86) and
/// `shell::reconcile::fingerprint`'s watch set (RUL-90). Two spellings that
/// disagree make `repair_owned_segments` **delete one and add the other on
/// every prompt** — both sit under `$OCX_HOME`, so both are owned, and only
/// one of them is ever in the desired set.
///
/// Structural because the defect is: nothing a behavioural test can observe
/// distinguishes "two spellings that happen to agree" from "one spelling", and
/// the day they stop agreeing is the day the prompt starts flapping.
#[cfg(test)]
mod one_install_bin_derivation {
    use std::path::{Path, PathBuf};

    /// Assembled, never written out, so the needle does **not** appear verbatim
    /// in this file — the scanner walks `activation.rs` too, and a literal here
    /// would make it match its own source in every state.
    const TAIL_NEEDLE: &str = concat!(".join(", "\"content\"", ").join(", "\"bin\"", ")");

    /// Whether `source`'s **production** span derives the install `bin`
    /// directory itself.
    ///
    /// Two normalisations, both load-bearing. Everything from the first
    /// `#[cfg(test)]` onwards is dropped, because a test fixture reproducing
    /// the store layout is not a second production spelling. All whitespace is
    /// then removed, because the chain is written across five lines at one site
    /// and one line at another — a line-oriented search finds neither.
    fn derives_install_bin(source: &str) -> bool {
        let production = source.split("#[cfg(test)]").next().unwrap_or_default();
        let squeezed: String = production.chars().filter(|c| !c.is_whitespace()).collect();
        squeezed.contains(TAIL_NEEDLE)
    }

    fn rust_sources(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).expect("the source tree is readable").flatten() {
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

    /// RUL-63, RUL-86, RUL-90 — exactly one file derives the directory, and it
    /// is `setup.rs`. Every other reader calls
    /// [`setup::ocx_install_bin_path`](crate::setup::ocx_install_bin_path).
    ///
    /// Red state — today's `goat`: `launcher/generate.rs` and
    /// `shell/reconcile/fingerprint.rs` each re-derive it inline.
    #[test]
    fn only_setup_derives_the_install_bin_directory() {
        let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let cli = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ocx_cli/src");
        let mut files = rust_sources(&lib);
        files.extend(rust_sources(&cli));
        assert!(files.len() > 50, "the walk found almost nothing, so it scanned nothing");

        let canonical = lib.join("setup.rs");
        let mut derivers: Vec<String> = files
            .iter()
            .filter(|path| derives_install_bin(&std::fs::read_to_string(path).expect("a source file is readable")))
            .map(|path| path.strip_prefix(&lib).unwrap_or(path).display().to_string())
            .collect();
        derivers.sort();

        // The non-vacuity twin, first: the scanner must still recognise the one
        // sanctioned derivation, or an empty offender list below is green
        // because the needle stopped matching.
        assert!(
            derives_install_bin(&std::fs::read_to_string(&canonical).expect("setup.rs is readable")),
            "the scanner no longer recognises `setup::ocx_install_bin_path`'s own derivation, \
             so the assertion below proves nothing"
        );

        assert_eq!(
            derivers,
            vec!["setup.rs".to_owned()],
            "RUL-63/RUL-86/RUL-90: one directory, one derivation — every other reader calls \
             `setup::ocx_install_bin_path`, or the repair deletes one spelling and adds the \
             other on every prompt"
        );
    }
}

// ---------------------------------------------------------------------------
// WP-9 — what `session` itself composes
// ---------------------------------------------------------------------------

/// [`session`]'s own composition, end to end (C-018, C-059, RUL-87).
///
/// Every other module here pins one piece: `session_path_tests` the splice,
/// `bin_mode_entry_tests` the stamp gate and the heal, `activate_ladder_tests`
/// the mode. [`session`] is the function that puts those pieces into **one**
/// `Outcome`, and which tier each contribution lands in is what decides whether
/// a globally installed tool shadows a project's own trampoline. Two rows, one
/// per arm that composes anything:
///
/// - the `bin`-mode arm, asserted on the resulting `PATH` rather than on the
///   vector, for RUL-62's reason — the vector runs backwards, so a vector
///   assertion cannot tell the right answer from the shadowing one;
/// - the project-free arm, where `project_contribution` never runs and the two
///   session directories are the whole of the answer.
///
/// Both red under a mutation of [`session`] itself, never of a caller: a
/// mutation one level up would prove the caller is wired, not that this
/// function composes correctly.
#[cfg(test)]
mod session_composition_tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use crate::env::Env;
    use crate::file_structure::{
        BinEntryStamp, DEFAULT_SHELL, FileStructure, RenderStamp, RenderStampScope, RenderStampTarget, ToolchainHome,
    };
    use crate::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};
    use crate::package::metadata::env::entry::Entry;
    use crate::package::metadata::env::modifier::ModifierKind;
    use crate::package_manager::{Concurrency, PackageManager};
    use crate::project::DEFAULT_GROUP;
    use crate::{Config, ShellConfig, ShellConsent, oci};

    use super::{ProjectIdentity, SessionInput, desired_entries, session};

    const REGISTRY: &str = "ocx.sh";
    const DIGEST: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    fn host() -> oci::Platform {
        "linux/amd64".parse().expect("a valid host platform")
    }

    fn path_entry(value: &str) -> Entry {
        Entry {
            key: "PATH".to_owned(),
            value: value.to_owned(),
            kind: ModifierKind::Path,
            separator: None,
        }
    }

    /// The `PATH` a shell holds after `entries` fold over one ambient segment,
    /// as segments — the only operand RUL-62 permits an ordering claim about.
    fn resulting_path(entries: &[Entry]) -> Vec<String> {
        let mut env = Env::clean();
        env.set("PATH", "/usr/bin");
        env.apply_entries(entries);
        let value = env.get("PATH").expect("PATH survives the fold").to_owned();
        std::env::split_paths(&value)
            .map(|segment| segment.to_string_lossy().into_owned())
            .collect()
    }

    fn position(path: &[String], needle: &Path) -> usize {
        let needle = needle.to_string_lossy();
        path.iter()
            .position(|segment| segment.as_str() == &*needle)
            .unwrap_or_else(|| panic!("{needle} must be on PATH; got {path:#?}"))
    }

    /// `$OCX_HOME` plus a consenting `bin`-mode project whose home is rendered
    /// and whose lock is current — the state one settled prompt sees.
    struct Arena {
        _tmp: tempfile::TempDir,
        file_structure: FileStructure,
        dir: PathBuf,
        home: ToolchainHome,
    }

    impl Arena {
        /// `activate` is written into the project's own `ocx.toml`, because that
        /// is the tier the ladder reads it from — the env tier is the weakest
        /// one and would be overridden by an ambient `OCX_TOOLCHAIN_ACTIVATE`.
        async fn new(activate: &str) -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let base = dunce::canonicalize(tmp.path()).expect("canonicalize the arena");

            let ocx_home = base.join("ocx-home");
            std::fs::create_dir_all(&ocx_home).expect("mkdir $OCX_HOME");
            let file_structure = FileStructure::with_root(ocx_home);

            let dir = base.join("work").join("acme");
            std::fs::create_dir_all(&dir).expect("mkdir the project");
            let config_path = dir.join("ocx.toml");
            std::fs::write(
                &config_path,
                format!("activate = \"{activate}\"\n\n[tools]\ncmake = \"{REGISTRY}/acme/cmake:3.28\"\n"),
            )
            .expect("write ocx.toml");

            // The lock's stored hash is taken from the config that was just
            // written, so the staleness gate answers "current" for a reason the
            // fixture owns rather than for a constant it copied.
            let config = crate::project::ProjectConfig::from_path(&config_path)
                .await
                .expect("the fixture ocx.toml parses");
            std::fs::write(
                dir.join("ocx.lock"),
                format!(
                    "[metadata]\nlock_version = 3\ndeclaration_hash_version = 1\n\
                     declaration_hash = \"{hash}\"\ngenerated_by = \"ocx test\"\n\
                     generated_at = \"2026-01-01T00:00:00Z\"\n\n\
                     [[tool]]\nname = \"cmake\"\ngroup = \"{DEFAULT_GROUP}\"\n\
                     repository = \"{REGISTRY}/acme/cmake\"\n\n\
                     [tool.platforms]\n\"linux/amd64\" = \"sha256:{DIGEST}\"\n",
                    hash = config.declaration_hash_cached(),
                ),
            )
            .expect("write ocx.lock");

            let home = ToolchainHome::new(dir.join(".ocx").join("toolchain"));
            super::plant_active(&home);
            Self {
                _tmp: tmp,
                file_structure,
                dir,
                home,
            }
        }

        /// Render one trampoline, link the default group at the locked digest,
        /// and persist the stamp that describes both — the tree `ocx pull`
        /// leaves behind, which is what the C-061 gate demands before the
        /// project's `bin/` may be emitted.
        fn render(&self, key: &str) {
            let path = self.home.shell_bin(DEFAULT_SHELL).join("cmake");
            let body = "#!/bin/sh\ncmake\n";
            std::fs::write(&path, body).expect("write the trampoline");
            let metadata = std::fs::symlink_metadata(&path).expect("stat the trampoline");
            let hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(body.as_bytes()));
            let stamp_entry = BinEntryStamp::from_metadata(&path, &metadata, hash);

            let identifier = oci::Identifier::new_registry("acme/cmake", REGISTRY);
            let pinned =
                oci::PinnedIdentifier::try_from(identifier.clone_with_digest(oci::Digest::Sha256(DIGEST.to_owned())))
                    .expect("a digest-bearing identifier is pinned");
            let target = self.file_structure.packages.path(&pinned);
            std::fs::create_dir_all(target.join("content")).expect("materialize the package root");
            let entry = self.home.entry(DEFAULT_GROUP, "cmake").expect("a valid entry path");
            std::fs::create_dir_all(entry.parent().expect("the group directory")).expect("mkdir the group");
            // `crate::symlink`, not `std::os::unix::fs::symlink`: this module
            // is not `#[cfg(unix)]` — unlike `bin_mode_entry_tests`, whose
            // subject really is POSIX — so a raw POSIX call here is a Windows
            // compile error, and gating the module away to silence it would
            // delete the session-composition coverage from the platform whose
            // `<group>/<entry>` link is a junction. The renderer links these
            // entries through the same module.
            crate::symlink::update(&target, &entry).expect("link the entry");

            self.file_structure
                .state
                .set_render_stamp(
                    RenderStampTarget::Project(key),
                    &RenderStamp::new(
                        self.home.root().to_path_buf(),
                        RenderStampScope::Project(self.dir.clone()),
                        BTreeMap::from([("cmake".to_owned(), stamp_entry)]),
                        BTreeMap::new(),
                    ),
                )
                .expect("persist the render stamp");
        }
    }

    /// The plain-value inputs one prompt reads. Offline, with an empty local
    /// index: C-061 forbids this path composing anything, so a call that
    /// reached the network fails here rather than passing slowly.
    struct Prompt {
        manager: PackageManager,
        local_index: LocalIndex,
        config: Config,
        target: oci::Platform,
    }

    impl Prompt {
        /// `consented` is written into `[shell.consent] paths`, the one clause
        /// that holds with no stamp on disk (C-025 clause 3).
        fn new(file_structure: &FileStructure, consented: Option<&Path>) -> Self {
            let index = Index::from_chained(
                LocalIndex::new(LocalConfig {
                    index_store: file_structure.index.clone(),
                }),
                Vec::new(),
                ChainMode::Offline,
            );
            let config = Config {
                shell: Some(ShellConfig {
                    consent: Some(ShellConsent {
                        paths: consented.map(Path::to_path_buf).into_iter().collect(),
                        ..ShellConsent::default()
                    }),
                    ..ShellConfig::default()
                }),
                ..Config::default()
            };
            Self {
                manager: PackageManager::new(file_structure.clone(), index, None, REGISTRY),
                local_index: LocalIndex::new(LocalConfig {
                    index_store: file_structure.index.clone(),
                }),
                config,
                target: host(),
            }
        }

        fn input<'a>(
            &'a self,
            file_structure: &'a FileStructure,
            global: &'a [Entry],
            project: Option<&'a ProjectIdentity>,
        ) -> SessionInput<'a> {
            SessionInput {
                global: global.to_vec(),
                manager: &self.manager,
                local_index: &self.local_index,
                concurrency: Concurrency::Unbounded,
                file_structure,
                config: &self.config,
                target: &self.target,
                project,
            }
        }
    }

    /// **C-018 x RUL-87, composed by [`session`] rather than assembled by the
    /// test.** A `bin`-mode project whose home is rendered contributes its
    /// `<home>/toolchain/active/bin`, and that directory lands **ahead** of everything
    /// the global tier composed: a globally installed `cmake` must never win
    /// over the project's own `cmake` trampoline.
    ///
    /// The three `session_path_tests` rows assert the same order over an
    /// `Outcome` the test wrote by hand. This one asserts it over the `Outcome`
    /// `session` **built**, which is the only operand that can catch the
    /// project's contribution being filed under the wrong tier.
    ///
    /// Red state: in [`session`]'s `Outcome` literal, file `input.global` under
    /// `project` instead of `global` — the desired vector then reads
    /// `[] ++ session ++ global-composed`, the global tool ends up in front of
    /// the trampolines, and this is the shadowing C-018 forbids.
    #[tokio::test]
    async fn c018_rul87_a_bin_mode_projects_trampolines_outrank_the_global_tiers_tools() {
        let arena = Arena::new("bin").await;
        let project = ProjectIdentity::resolve(arena.dir.join("ocx.toml"))
            .await
            .expect("the fixture project resolves");
        arena.render(&project.key);

        let global_tool = format!(
            "{}/packages/gg/global/content-bin",
            arena.file_structure.root().display()
        );
        let global = vec![path_entry(&global_tool)];
        let prompt = Prompt::new(&arena.file_structure, Some(&arena.dir));

        let outcome = session(prompt.input(&arena.file_structure, &global, Some(&project)))
            .await
            .expect("a consenting, rendered, current bin-mode project composes");

        assert_eq!(
            outcome.session.project_bin.as_deref(),
            Some(arena.home.bin().as_path()),
            "C-061/C-062: the rendered home must reach the middle slot; messages were {:#?}",
            outcome.messages
        );

        let path = resulting_path(&desired_entries(&outcome));
        assert!(
            position(&path, &arena.home.bin()) < position(&path, Path::new(&global_tool)),
            "RUL-87: the project's trampolines must outrank the global tier's tools; got {path:#?}"
        );
    }

    /// **C-059 on the project-free arm.** A prompt outside every project runs
    /// no `project_contribution` at all — and still contributes both session
    /// directories, because they are session-level facts minted above the
    /// early return rather than activation decisions taken below it.
    ///
    /// Dropping them here would not "leave them alone": both sit under
    /// `$OCX_HOME`, an owned prefix, so `repair_owned_segments` would delete
    /// the registration `ocx self setup` wrote the moment the user `cd`s out of
    /// their last project.
    ///
    /// Red state: clear `outcome.session` in [`session`]'s project-free early
    /// return — the shape a reader gets by assuming the mint belongs with the
    /// project.
    #[tokio::test]
    async fn c059_a_project_free_prompt_still_contributes_both_session_directories() {
        let arena = Arena::new("bin").await;
        let global_tool = format!(
            "{}/packages/gg/global/content-bin",
            arena.file_structure.root().display()
        );
        let global = vec![path_entry(&global_tool)];
        let prompt = Prompt::new(&arena.file_structure, None);

        let outcome = session(prompt.input(&arena.file_structure, &global, None))
            .await
            .expect("a project-free prompt is not an error");

        assert!(
            outcome.slot.is_none(),
            "no project was resolved, so no slot is recorded"
        );
        assert!(!outcome.resolved, "C-007: nothing was resolved on this prompt");
        assert!(
            outcome.owned_home.is_none(),
            "C-063: with no project there is no second owned prefix to widen to"
        );

        let path = resulting_path(&desired_entries(&outcome));
        let install = position(&path, &outcome.session.install_bin);
        let global_bin = position(&path, &outcome.session.global_bin);
        assert!(
            global_bin < install && install < position(&path, Path::new(&global_tool)),
            "C-059/C-060/RUL-87: $OCX_HOME/toolchain/bin ▸ install_bin ▸ global-composed, \
             even with no project; got {path:#?}"
        );
    }

    /// **A repository-committed symlink must not hand ocx a deletion authority
    /// over the filesystem** (C-063, RUL-33/RUL-44).
    ///
    /// `owned_prefixes` is read by `repair_owned_segments` as authority to
    /// delete every `PATH` segment beneath it that the desired set does not
    /// contribute. The home is built **lexically** and the prefix set is
    /// **canonicalised**, so `.ocx/toolchain -> /somewhere` silently promoted
    /// `/somewhere` into that set — in `none` mode, with no stamp, no lock and
    /// no render between the `git clone` and the widening, on a project a
    /// `namespaces` grant consents to with no user gesture at all.
    ///
    /// Both halves are asserted, because either alone is satisfiable by the
    /// wrong fix: `owned_home` staying `None`, **and** the victim segment
    /// surviving the plan the CLI would build from it.
    ///
    /// Red state: assign `owned_home` before the guard — i.e. restore
    /// `outcome.owned_home = Some(home.root().to_path_buf())` above the
    /// `owned_project_home` call.
    #[tokio::test]
    #[cfg(unix)]
    async fn c063_a_symlinked_project_home_is_never_taken_as_an_owned_prefix() {
        let arena = Arena::new("none").await;
        // The link's target holds a live `PATH` segment — the shape that makes
        // the widening a deletion rather than a curiosity.
        let victim_root = arena.dir.parent().expect("the workspace").join("victim");
        let victim_bin = victim_root.join("bin");
        std::fs::create_dir_all(&victim_bin).expect("mkdir the victim");
        std::fs::remove_dir_all(arena.home.root()).expect("remove the honest home");
        std::os::unix::fs::symlink(&victim_root, arena.home.root()).expect("commit the hostile link");

        let project = ProjectIdentity::resolve(arena.dir.join("ocx.toml"))
            .await
            .expect("the fixture project resolves");
        let prompt = Prompt::new(&arena.file_structure, Some(&arena.dir));
        let outcome = session(prompt.input(&arena.file_structure, &[], Some(&project)))
            .await
            .expect("a hostile home degrades; it does not tear the environment down");

        assert!(
            outcome.owned_home.is_none(),
            "C-063: a home ocx may not own is never widened into the owned set; got {:?}",
            outcome.owned_home
        );

        // And the consequence, built the way the CLI builds it: `$OCX_HOME`
        // plus whatever `owned_home` carries.
        let mut current = Env::clean();
        current.set(
            "PATH",
            std::env::join_paths([victim_bin.as_os_str(), std::ffi::OsStr::new("/usr/bin")]).expect("the seed joins"),
        );
        let mut owned: Vec<&Path> = vec![arena.file_structure.root()];
        if let Some(home) = outcome.owned_home.as_deref() {
            owned.push(home);
        }
        let ledger = super::next_ledger(
            &crate::shell::reconcile::Ledger::empty(),
            "fp-1",
            &outcome,
            &Env::clean(),
        );
        let plan = super::plan_for(&ledger, &outcome, &owned, &current);

        let removed: Vec<&str> = plan.removes.iter().map(|(_, element, _)| element.as_str()).collect();
        assert!(
            !removed.contains(&victim_bin.to_string_lossy().as_ref()),
            "C-063: the user's own PATH is not ocx's to delete; got removes {removed:#?}"
        );
    }

    /// C-063 — the widening is **strictly after consent**, not merely usually
    /// after it: a project with no grant at all leaves `owned_home` unset, so a
    /// refused project can never contribute a deletion authority.
    ///
    /// Red state: hoist the `owned_home` assignment out of
    /// `project_contribution` into `session`, above the `ConsentProof` gate.
    #[tokio::test]
    async fn c063_a_consent_refused_project_widens_no_owned_prefix() {
        let arena = Arena::new("bin").await;
        let project = ProjectIdentity::resolve(arena.dir.join("ocx.toml"))
            .await
            .expect("the fixture project resolves");
        arena.render(&project.key);
        // No `paths` entry: the project is rendered and current, and still inert.
        let prompt = Prompt::new(&arena.file_structure, None);

        let outcome = session(prompt.input(&arena.file_structure, &[], Some(&project)))
            .await
            .expect("an inert project is not an error");

        assert!(
            outcome.inert,
            "the fixture must actually be refused, or this proves nothing"
        );
        assert!(
            outcome.owned_home.is_none(),
            "C-063: no proof, no owned prefix; got {:?}",
            outcome.owned_home
        );
        assert!(
            outcome.session.project_bin.is_none(),
            "…and nothing of the project reaches PATH either"
        );
    }

    /// C-063 — and the refusal is *told*, on the stream the prompt actually
    /// reads. A degrade nobody can see is the silent forever-failure this
    /// whole path exists to avoid.
    #[tokio::test]
    #[cfg(unix)]
    async fn c063_a_refused_home_says_so_and_still_contributes_the_session_directories() {
        let arena = Arena::new("bin").await;
        let outside = arena.dir.parent().expect("the workspace").join("outside");
        std::fs::create_dir_all(&outside).expect("mkdir outside");
        std::fs::remove_dir_all(arena.home.root()).expect("remove the honest home");
        std::os::unix::fs::symlink(&outside, arena.home.root()).expect("commit the hostile link");

        let project = ProjectIdentity::resolve(arena.dir.join("ocx.toml"))
            .await
            .expect("the fixture project resolves");
        let prompt = Prompt::new(&arena.file_structure, Some(&arena.dir));
        let outcome = session(prompt.input(&arena.file_structure, &[], Some(&project)))
            .await
            .expect("a hostile home degrades");

        assert!(
            outcome
                .messages
                .iter()
                .any(|line| line.contains(&arena.dir.display().to_string())),
            "the line must name the directory the user would have to fix; got {:#?}",
            outcome.messages
        );

        // C-059 is unaffected: the session pair is minted above every arm.
        let path = resulting_path(&desired_entries(&outcome));
        position(&path, &outcome.session.install_bin);
        position(&path, &outcome.session.global_bin);
    }
}
