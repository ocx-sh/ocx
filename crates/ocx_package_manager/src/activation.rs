// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The per-prompt session: the sequencing that binds the pure reconciler pieces into one answer,
//! and the consent gate that fronts it.
//!
//! The order is normative: resolve the global tier, evaluate consent, compose only what consent
//! authorized, diff against the live environment, record the state. Not under `ocx_shell`: that
//! closes a `use` cycle.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::tasks::render_toolchain::HealOutcome;
use ocx_config::Config;
use ocx_config::env::Env;
use ocx_config::exec_state::{self, ExecState, Owner, PinnedTier, Selection, SessionCache, SessionToken};
use ocx_config::shell::{ShellConsent, effective_consent};
use ocx_package::launch::LaunchIdentities;
use ocx_package::metadata::env::apply::{EnvEntriesExt, ListSeparatorError, reconcile_list_separators};
use ocx_package::metadata::env::entry::Entry;
use ocx_package::metadata::env::modifier::ModifierKind;
use ocx_project::consent::{self, ConsentStamp, Decision};
use ocx_project::{LockCurrency, ProjectLock, lock::lock_path_for};
use ocx_shell::shell::coexistence;
use ocx_shell::shell::reconcile::{self, Plan};
use ocx_store::file_structure;

/// What one per-prompt session can fail with.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum SessionError {
    /// `ocx.lock` is absent, or no longer describes the `ocx.toml` beside it.
    #[error("{0}")]
    #[exit(delegate = 0)]
    Lock(#[from] LockCurrency),

    /// Any library error the composition raised.
    #[error("{0}")]
    #[exit(
        chain,
        fallback(
            Failure,
            slug = "activation_failed",
            summary = "Shell activation failed with an unclassified cause"
        )
    )]
    Library(#[from] crate::Error),

    /// Two contributors to one list key declared different separators.
    #[error("{0}")]
    #[exit(
        chain,
        fallback(
            Failure,
            slug = "activation_failed",
            summary = "Shell activation failed with an unclassified cause"
        )
    )]
    ListSeparator(#[from] ListSeparatorError),
}

impl From<ocx_project::Error> for SessionError {
    fn from(error: ocx_project::Error) -> Self {
        Self::Library(error.into())
    }
}

/// The project the CWD walk resolved, and the two labels the state records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectIdentity {
    /// The resolved `ocx.toml`.
    pub config_path: PathBuf,
    /// Its canonical directory — the project's identity.
    pub dir: PathBuf,
    /// `ReferenceManager::name_for_path` of `dir`: a lookup index, never the identity.
    pub key: String,
}

/// Why a resolved `ocx.toml` could not be turned into a [`ProjectIdentity`]; indeterminate, never "no project".
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
    /// Derive the identity of the project at `config_path`; the only derivation of its stamp `key`.
    ///
    /// # Errors
    ///
    /// [`IdentityError`] when the path will not canonicalize or the blocking hop fails.
    pub async fn resolve(config_path: PathBuf) -> Result<Self, IdentityError> {
        let canonical = config_path.clone();
        let dir = tokio::task::spawn_blocking(move || consent::canonical_project_dir(&canonical))
            .await?
            .map_err(|source| IdentityError::Canonicalize {
                path: config_path.clone(),
                source,
            })?;
        let key = ocx_store::reference_manager::ReferenceManager::name_for_path(&dir);
        Ok(Self { config_path, dir, key })
    }
}

/// Everything one session reads, as plain values — never the CLI `Context`, whose network client a reconcile must not reach.
pub struct SessionInput<'a> {
    /// The global toolchain tier's entries, resolved by the caller.
    pub global: Vec<Entry>,
    /// The names the global tier composed its packages under, for their launchers; `None` without a patch tier.
    pub global_identities: Option<LaunchIdentities>,
    /// The offline-capable manager the project composition runs against.
    pub manager: &'a crate::PackageManager,
    /// The local index the manager's offline view is taken over.
    pub local_index: &'a ocx_index::LocalIndex,
    /// The materialisation concurrency.
    pub concurrency: crate::Concurrency,
    /// `$OCX_HOME`'s layout — the package store consent corroborates against.
    pub file_structure: &'a file_structure::FileStructure,
    /// The merged config, for its `[shell]` table.
    pub config: &'a Config,
    /// The host platform the lock's per-platform digests resolve under.
    pub target: &'a ocx_oci::Platform,
    /// The project the CWD walk resolved, or `None` for a project-free prompt.
    ///
    /// Under a pinned selection: the project the pinned tier names, from [`pinned_identity`].
    pub project: Option<&'a ProjectIdentity>,
    /// The scopes this prompt composes; read before any consent or `ocx.toml` work, so a scope the
    /// composition omits costs nothing and can fail nothing.
    pub composition: reconcile::Composition,
    /// The `ocx.toml` groups the project composes; empty means the default group.
    pub groups: Vec<String>,
    /// Whether the shell is pinned: the project composes as `env` whatever its `activate` mode says,
    /// and no coexisting tool makes the hook yield.
    pub pinned: bool,
}

/// The session-level `PATH` directories, in desired-vector order — the reverse of `PATH` order,
/// since `export_path` prepends. Front to back on `PATH`: `project_bin`, `global_bin`, `install_bin`.
///
/// `setup::session_path_directories` lists front-first; copying its order reverses `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPath {
    /// The installed `ocx`'s directory, backmost on `PATH` so a toolchain-pinned `ocx` stays reachable
    /// (`adr_toolchain_activation.md` § Rationale from code: activation).
    pub install_bin: PathBuf,

    /// The global toolchain's trampolines (`$OCX_HOME/toolchain/active/bin`).
    // Never gated like the project slot: dropping it makes `repair_owned_segments` delete `ocx self setup`'s registration.
    pub global_bin: PathBuf,

    /// The consented project's `<home>/toolchain/active/bin`, only in `bin` mode past the render-stamp gate.
    pub project_bin: Option<PathBuf>,
}

impl SessionPath {
    /// The two unconditional session entries, with the project slot still empty.
    ///
    /// Desired on every arm: an omitted one is under an owned prefix, so the repair deletes
    /// the session-`PATH` registration `ocx self setup` wrote.
    #[must_use]
    pub fn new(file_structure: &file_structure::FileStructure) -> Self {
        Self {
            global_bin: file_structure.toolchain.bin(),
            project_bin: None,
            install_bin: file_structure.ocx_install_bin_path(),
        }
    }

    /// The three directories as `PATH` entries, in desired-vector order.
    pub(crate) fn entries(&self) -> Vec<Entry> {
        let mut entries = self.persistent_entries();
        entries.extend(self.project_entry());
        entries
    }

    /// The installed `ocx` and global trampoline directories: desired at every prompt, so never retired.
    fn persistent_entries(&self) -> Vec<Entry> {
        [&self.install_bin, &self.global_bin]
            .into_iter()
            .filter_map(|directory| path_entry(directory))
            .collect()
    }

    /// The project's trampoline directory, the one session directory that comes and goes with the project.
    fn project_entry(&self) -> Option<Entry> {
        self.project_bin.as_deref().and_then(path_entry)
    }
}

// A non-UTF-8 directory is skipped: a lossy rendering names another directory, added while the real one is removed each prompt.
fn path_entry(directory: &Path) -> Option<Entry> {
    match directory.to_str() {
        Some(value) => Some(Entry {
            key: "PATH".to_owned(),
            value: value.to_owned(),
            kind: ModifierKind::Path,
            separator: None,
        }),
        None => {
            log::debug!(
                "Session PATH directory '{}' is not valid UTF-8 and was not emitted",
                directory.display()
            );
            None
        }
    }
}

/// What one recomposition resolved: the entries each scope wants, the project
/// slot's identity, and every message the prompt owes the user.
pub struct Outcome {
    /// The session-level `PATH` directories, desired on every arm.
    pub session: SessionPath,
    /// Whether this prompt re-asserts `session` at the front of `PATH`.
    ///
    /// `false` when the prompt composes nothing (the project and the global tier are both inherited):
    /// moving the directories then would put the global trampolines ahead of what the outer
    /// `ocx exec` composed, which is the displacement the old exec yield existed to prevent.
    pub session_enforced: bool,

    /// The global toolchain tier's entries, applied first.
    pub global: Vec<Entry>,
    /// The project tier's entries — empty when inert or yielded.
    pub project: Vec<Entry>,
    /// The names the global tier's packages were composed under; recorded, never exported.
    pub global_identities: Option<LaunchIdentities>,
    /// The names the project tier's packages were composed under; recorded, never exported.
    pub project_identities: Option<LaunchIdentities>,

    /// The second owned prefix: the consented, in-scope project's toolchain home; `None` otherwise.
    // Owned prefixes delete live `PATH` segments: never the bare `toolchain_dir` (other projects' homes) or an unconsented project.
    pub owned_home: Option<PathBuf>,

    /// The project slot to record, or `None` to retire it.
    pub slot: Option<ProjectIdentity>,
    /// Whether the CWD walk resolved a project at all; only `false` is cacheable as [`exec_state::Verdict::NoProject`].
    pub resolved: bool,
    /// Whether the resolved project was refused by consent.
    pub inert: bool,
    /// Deferred diagnostics, in emission order.
    pub messages: Vec<String>,
}

/// The consent-before-read gate, in its own module so no other code in this file can mint a proof.
mod consent_gate {
    use ocx_project::consent::{Decision, Grant};

    /// Evidence that consent said `Activate`, and which clause said so.
    ///
    /// `project_entries` requires one, so reading `ocx.toml` before consent does not compile.
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

        /// Whether the granting clause authorizes the project-file `[env]` channel.
        pub fn authorizes_project_env(self) -> bool {
            self.0.authorizes_project_env()
        }
    }
}

pub use consent_gate::ConsentProof;

/// The consent answer for one project, and the evidence that produced it.
// No public constructor: `ocx shell state` and `ocx self activate` must share `evaluate_consent`, or their answers drift.
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

    /// Whether a usable stamp backs it (an unusable stamp counts as absent).
    pub fn stamped(&self) -> bool {
        self.stamp.is_some()
    }

    /// The usable stamp itself, when there is one.
    pub fn stamp(&self) -> Option<&ConsentStamp> {
        self.stamp.as_ref()
    }

    /// The parsed `ocx.lock` consent decided on, or `None` when absent, unreadable or unparseable.
    // Handed back, never re-read: a `git checkout` between two reads lets consent and composition see different locks.
    pub fn lock(&self) -> Option<&ProjectLock> {
        self.lock.as_ref()
    }

    fn into_lock(self) -> Option<ProjectLock> {
        self.lock
    }
}

/// Evaluate consent over one project; read-only, the one derivation `self activate` and `shell state` share.
pub async fn evaluate_consent(
    whitelist: &ShellConsent,
    target: &ocx_oci::Platform,
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

/// Every piece of consent evidence the prompt needs, on one blocking hop; `lock` moves through, never cloned.
///
/// `verified_sources` runs only when `namespaces` is configured, the only case that reads it.
/// A join error fails closed to `(None, None, None)`: strictly less consent.
async fn consent_evidence(
    lock: Option<ProjectLock>,
    whitelist: &ShellConsent,
    target: &ocx_oci::Platform,
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

/// One info line per observed tool, never just the first: with direnv and mise both live, both lines appear.
pub fn yield_messages(yielded: &coexistence::Yield) -> Vec<String> {
    yielded
        .observed
        .iter()
        .map(|observation| {
            let signal = &observation.signal;
            match observation.tool {
                coexistence::Tool::Direnv => {
                    format!("ocx: direnv manages this directory ({signal}); applying the global toolchain only")
                }
                coexistence::Tool::Mise => {
                    format!("ocx: mise manages this directory ({signal}); applying the global toolchain only")
                }
            }
        })
        .collect()
}

/// Whether this prompt can answer from `stat`s alone, over the execution state.
// Only negative verdicts: caching `Activate` would make the state a consent input.
#[must_use]
pub fn state_is_stat_only(state: &ExecState, fingerprint: &str) -> bool {
    state.session.fp == fingerprint
        && matches!(
            state.session.verdict,
            Some(exec_state::Verdict::Inert | exec_state::Verdict::NoProject)
        )
}

/// The project-file `[env]` channel, gated on the clause that granted: the entries to apply, and
/// whether a declared `[env]` was withheld.
pub fn authorized_project_env(
    consent: ConsentProof,
    config: &ocx_project::ProjectConfig,
    config_path: &Path,
    groups: &[String],
) -> (Vec<Entry>, bool) {
    let declared = ocx_project::project_env_entries(config, config_path, groups);
    if consent.authorizes_project_env() {
        return (declared, false);
    }
    let withheld = !declared.is_empty();
    (Vec::new(), withheld)
}

/// The groups a project composes: the requested ones, or the default group when none are.
fn composed_groups(requested: &[String]) -> Vec<String> {
    if requested.is_empty() {
        vec![ocx_project::DEFAULT_GROUP.to_owned()]
    } else {
        requested.to_vec()
    }
}

/// Compose the consenting project's toolchain env, and report whether its own `[env]` was withheld.
// Never `load_project_with_lock`: it re-reads `ocx.lock` and resolves the global tier itself, so composition could diverge from what consent decided.
async fn project_entries(
    input: &SessionInput<'_>,
    consent: ConsentProof,
    project: &ProjectIdentity,
    config: &ocx_project::ProjectConfig,
    lock: ProjectLock,
    // The vetted home from `owned_project_home`, never a second `project_home` call: its `Err` would no-op the hook in every project.
    home: Option<file_structure::ToolchainHome>,
) -> Result<(Vec<Entry>, bool, Option<LaunchIdentities>), SessionError> {
    use crate::composer::{ComposeRequest, Materialization};
    use ocx_project::compose_tool_set;

    let groups = composed_groups(&input.groups);
    let tools = compose_tool_set(config, Some(&lock), &groups, &[], input.target)?;

    // `LocalOnly`: a prompt must never block on the network.
    let manager = input.manager.offline_view(input.local_index.clone());
    let requests: Vec<ComposeRequest> = tools
        .iter()
        .map(|tool| ComposeRequest {
            identifier: tool.identifier.clone(),
            mode: ocx_project::lazy_mode_for_tool(config, &tool.identifier, None, None),
        })
        .collect();
    let roots = manager
        .compose_roots(&requests, input.target, Materialization::LocalOnly, input.concurrency)
        .await?;
    let (env, env_withheld) = authorized_project_env(consent, config, &project.config_path, &groups);
    // Heals exactly the groups it emits: the scope `compose_tool_set` was given.
    let toolchain = home.map(|home| {
        Box::new(crate::ToolchainLinks {
            pinned: crate::pinned_for_project(None, config),
            home,
            scope: file_structure::RenderStampScope::Project(project.dir.clone()),
            lock,
            groups: groups.clone(),
        })
    });
    let no_patches = config.no_patches_repositories();
    let scope = crate::EnvScope::Project {
        no_patches: no_patches.clone(),
        env,
        toolchain,
    };
    let (mut entries, ..) = manager
        .resolve_env_with_attribution(&roots.roots, false, scope, input.target)
        .await?;
    reconcile_list_separators(entries.iter_mut())?;
    // The global tier's names are folded in by `next_state`, the one place both scopes are known.
    let identities = manager.launch_identities(&roots.roots, &no_patches);
    Ok((entries, env_withheld, identities))
}

/// Compose both scopes and decide the project slot.
///
/// An `Err` must emit nothing at all: a partial plan lacking the project scope would revert it.
///
/// # Errors
///
/// [`SessionError::Lock`] when the consenting project's lock is absent or stale; otherwise
/// whatever the composition raised.
pub async fn session(input: SessionInput<'_>) -> Result<Outcome, SessionError> {
    let mut outcome = Outcome {
        // Above every `return`, or an arm drops these and the repair deletes `ocx self setup`'s registration.
        session: SessionPath::new(input.file_structure),
        session_enforced: input.composition != reconcile::Composition::default(),
        // Always consented (the user's own file): no consent gate may withhold or clear it.
        global: if input.composition.global {
            input.global.clone()
        } else {
            Vec::new()
        },
        project: Vec::new(),
        global_identities: input
            .composition
            .global
            .then(|| input.global_identities.clone())
            .flatten(),
        project_identities: None,
        owned_home: None,
        slot: None,
        resolved: input.project.is_some(),
        inert: false,
        messages: Vec::new(),
    };

    let shell_config = input.config.shell.as_ref();
    // Not `log::warn!`: the shims discard stderr.
    if let Some(reason) = shell_config.and_then(|shell| shell.consent_strip_reason.as_ref()) {
        outcome.messages.push(format!("ocx: {reason}"));
    }

    // Before consent and before any file is opened: a scope the composition omits is never evaluated.
    let Some(project) = input.project.filter(|_| input.composition.project.is_some()) else {
        return Ok(outcome);
    };

    // A pinned shell ignores the working directory, so what manages that directory has no say.
    // A yield leaves `slot` at `None`, so the retirement rule retires the project's recorded entries.
    if !input.pinned {
        let yielded = coexistence::detect(&project.dir);
        if !yielded.observed.is_empty() {
            outcome.messages.extend(yield_messages(&yielded));
            return Ok(outcome);
        }
    }

    let whitelist = effective_consent(shell_config);
    let evaluated = evaluate_consent(&whitelist, input.target, &input.file_structure.packages, project).await;

    let Some(consent_proof) = ConsentProof::of(evaluated.decision()) else {
        outcome.inert = true;
        outcome.messages.push(format!(
            "ocx: {dir} is not activated; run `ocx shell allow` to consent, or `ocx shell state` to see why",
            dir = project.dir.display(),
        ));
        return Ok(outcome);
    };
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

/// What one consenting project contributes to this prompt, per its resolved `activate` mode.
// The `ocx.toml` parse stays here, behind `consent`; hoisted into `session`, only statement order would gate it.
async fn project_contribution(
    input: &SessionInput<'_>,
    consent: ConsentProof,
    project: &ProjectIdentity,
    lock_path: &Path,
    lock: Option<ProjectLock>,
    outcome: &mut Outcome,
) -> Result<(), SessionError> {
    use ocx_project::activate::ActivateMode;

    let config = ocx_project::ProjectConfig::from_path(&project.config_path).await?;

    // Owned in every mode, not only `bin`, or a `bin → none` switch strands `<home>/bin` on `PATH`.
    let home = owned_project_home(input, project, outcome).await;
    outcome.owned_home = home.as_ref().map(|home| home.root().to_path_buf());

    // `ocx shell` composes the environment; a `bin` or `none` mode only shapes the per-prompt hook.
    let mode = if input.pinned {
        ActivateMode::Env
    } else {
        activate_mode(&config)
    };
    match mode {
        ActivateMode::None => {}

        ActivateMode::Bin => {
            let Some(home) = home else { return Ok(()) };
            let lock = current_lock(lock, &config, lock_path)?;
            match bin_mode_entry(input, project, &home, &lock).await? {
                Some(bin) => outcome.session.project_bin = Some(bin),
                // Not `log::debug!`: every emitted hook sends stderr to `/dev/null`.
                None => outcome.messages.push(format!(
                    "ocx: {dir}: its toolchain has not been rendered for this lock; run `ocx pull` here",
                    dir = project.dir.display(),
                )),
            }
        }

        ActivateMode::Env => {
            let lock = current_lock(lock, &config, lock_path)?;
            let (entries, env_withheld, identities) =
                project_entries(input, consent, project, &config, lock, home).await?;
            outcome.project = entries;
            outcome.project_identities = identities;
            if env_withheld {
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

/// The project's home when ocx may own it; `None`, never an error, which widens no deletion authority.
///
/// A symlinked home must be refused before it becomes an owned prefix, or a repository
/// linking `.ocx/toolchain` to `/` deletes the user's whole `PATH` each prompt. An `Err` would
/// make `session` emit nothing, so one bad `toolchain_dir` would no-op every project.
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
        crate::tasks::render_toolchain::refuse_symlinked_home(&scope, &probe)
    })
    .await
    {
        Ok(verdict) => verdict.err(),
        // A dead hop is no evidence the tree is safe: own nothing.
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

/// The project's resolved toolchain home.
///
/// # Errors
///
/// The `toolchain_dir` funnel's refusal (exit 78), or the canonicalisation's I/O failure.
// Re-runs the `toolchain_dir` funnel from `Config`: a bare root-path parameter would bypass
// `ToolchainRoot`'s validation.
async fn project_home(config: &Config, project_dir: &Path) -> Result<file_structure::ToolchainHome, SessionError> {
    let config = config.clone();
    let owned_dir = project_dir.to_path_buf();
    match tokio::task::spawn_blocking(move || {
        let root = ocx_config::ToolchainRoot::resolve(&config).map_err(ocx_config::error::Error::from)?;
        ocx_project::resolve_toolchain_home(&owned_dir, root.as_ref())
    })
    .await
    {
        Ok(home) => Ok(home?),
        Err(error) => Err(crate::error::file_error(project_dir, std::io::Error::other(error)).into()),
    }
}

/// The lock the two lock-reading modes need, current with `config`.
///
/// # Errors
///
/// [`LockCurrency::Missing`] (reachable under a `paths` grant), or [`LockCurrency::Stale`] when
/// the lock no longer binds to `ocx.toml`.
fn current_lock(
    lock: Option<ProjectLock>,
    config: &ocx_project::ProjectConfig,
    lock_path: &Path,
) -> Result<ProjectLock, SessionError> {
    let Some(lock) = lock else {
        return Err(LockCurrency::Missing {
            path: lock_path.to_path_buf(),
        }
        .into());
    };
    if !lock.is_current(config) {
        return Err(LockCurrency::Stale {
            lock_path: lock_path.to_path_buf(),
        }
        .into());
    }
    Ok(lock)
}

/// Resolve the `activate` ladder (`file ▸ environment ▸ floor`) for one toolchain tier; the only
/// resolver, so no tier carries a second floor (`adr_toolchain_activation.md` § Rationale from code: activation).
pub fn activate_mode(config: &ocx_project::ProjectConfig) -> ocx_project::activate::ActivateMode {
    ocx_project::ladder::Ladder {
        cli: None,
        file: config.activate,
        // Weakest tier; `from_env` makes an unrecognised value absent rather than the floor.
        environment: ocx_project::activate::ActivateMode::from_env(),
    }
    .resolve(ocx_project::activate::ACTIVATE_FLOOR)
}

/// The render-stamp gate: does `bin_dir` hold exactly what `stamp` recorded, in both directions?
/// Any unreadable or odd condition is a mismatch (`adr_toolchain_activation.md` § Rationale from code: activation).
pub(crate) async fn bin_stamp_matches(bin_dir: &Path, stamp: &file_structure::RenderStamp) -> bool {
    let bin_dir = bin_dir.to_path_buf();
    let recorded = stamp.bin_fingerprint.clone();
    match tokio::task::spawn_blocking(move || bin_matches_recorded(&bin_dir, &recorded)).await {
        Ok(matched) => matched,
        Err(error) => {
            tracing::debug!("the render-stamp comparison task failed: {error}");
            false
        }
    }
}

/// [`bin_stamp_matches`]' blocking half.
fn bin_matches_recorded(bin_dir: &Path, recorded: &BTreeMap<String, file_structure::BinEntryStamp>) -> bool {
    // `read_dir` follows a symlinked `bin/`, which would put a directory outside the home on `PATH`.
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
        // An on-disk entry the stamp does not name is a mismatch (a force-committed extra file).
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return false;
        };
        let Some(recorded) = recorded.get(&name) else {
            return false;
        };

        // Never `metadata`: following a link reads through a repository-controlled target.
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }

        // `(size, file id)` picks what to hash, never what to trust; never mtime, which an in-place overwrite forges.
        let stat_pair = file_structure::BinEntryStamp::from_metadata(&path, &metadata, recorded.content_hash.clone());
        if stat_pair.file_id.is_some() && stat_pair == *recorded {
            seen += 1;
            continue;
        }

        let Some(hashed) = file_structure::BinEntryStamp::of_file(&path, &metadata) else {
            return false;
        };
        if hashed.content_hash != recorded.content_hash {
            return false;
        }
        seen += 1;
    }

    // Every recorded name met: each entry matched a distinct key, as `read_dir` yields no name twice.
    seen == recorded.len()
}

/// The `bin`-mode arm: the project `PATH` entry, or `None` for the caller's hint.
///
/// Step order is the contract, and this path never prunes: a prompt-time delete would run in an
/// attacker-writable tree before every command (`adr_toolchain_activation.md` § Rationale from code: activation).
///
/// # Errors
///
/// Only what the heal raises (a lock group key that cannot become a path component, exit 78).
pub(crate) async fn bin_mode_entry(
    input: &SessionInput<'_>,
    project: &ProjectIdentity,
    home: &file_structure::ToolchainHome,
    lock: &ProjectLock,
) -> Result<Option<PathBuf>, SessionError> {
    use file_structure::{RenderStampScope, RenderStampTarget};

    // 1. This project's stamp, never `Global`.
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

    // 2. Identity before content, both halves: two projects colliding in `name_for_path` share a home and stamp.
    let scope = RenderStampScope::Project(project.dir.clone());
    if stamp.home != home.root() || stamp.scope != scope {
        return Ok(None);
    }

    // 3a. The returned `bin()` resolves through `active`, so a repointed link puts any directory on `PATH`.
    if !home.active_is_valid(file_structure::DEFAULT_SHELL) {
        return Ok(None);
    }

    // 3b. The physical directory: through `active`, the gate judges whatever `active` names.
    if !bin_stamp_matches(&home.shell_bin(file_structure::DEFAULT_SHELL), &stamp).await {
        return Ok(None);
    }

    // 4. Heal after the gate, default group only: the stamp covers no other group's links.
    let groups = [ocx_project::DEFAULT_GROUP.to_owned()];
    let healed =
        crate::tasks::render_toolchain::heal_links(input.file_structure, home, &scope, lock, &groups, input.target)
            .await
            .map_err(crate::Error::from)?;

    // 5. A refused heal means a symlinked home or `bin/`; `Some` would put the relocated `bin/` on `PATH`.
    if let HealOutcome::Refused { reason } = healed {
        tracing::debug!(
            "the toolchain home '{}' was not healed and is not exposed: {reason}",
            home.root().display()
        );
        return Ok(None);
    }

    Ok(Some(home.bin()))
}

// -- The session token: which shell process owns the session records -------------

/// The shell process the hook runs for: its pid, and its start time where ocx can read one cheaply.
///
/// `None` in place of a [`ShellProcess`] is token-less mode, the hook of an older emission or an
/// older `env.nu` shim that passes no `--shell-pid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellProcess {
    pid: u32,
    started: Option<u64>,
}

impl ShellProcess {
    /// A shell whose start time is already known, or known to be unreadable.
    #[must_use]
    pub fn new(pid: u32, started: Option<u64>) -> Self {
        Self { pid, started }
    }

    /// The shell `pid` names, with its start time read where the platform allows.
    pub async fn resolve(pid: u32) -> Self {
        Self::new(pid, process_start_time(pid).await)
    }

    /// The identity this shell writes into `session.token` when it claims the session.
    #[must_use]
    pub fn token(&self) -> SessionToken {
        SessionToken {
            pid: self.pid,
            started: self.started,
        }
    }
}

/// How the recorded token stands against the shell running the hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStanding {
    /// No shell claimed the session yet.
    Unclaimed,
    /// This shell's token, or any token while the hook runs token-less.
    Own,
    /// Another shell instance's token: a tmux or IDE snapshot, a nested shell, a reused pid.
    Foreign,
}

/// What [`claim_session`] did to the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// The session is this shell's already, or the hook is token-less and found nothing to claim.
    Held,
    /// An unclaimed state; this shell wrote its token.
    Claimed,
    /// Session records found under `FollowCwd` with no claim: retagged inherited, then claimed.
    RetaggedUnclaimed,
    /// Another shell's token: its records retagged inherited, the cache reset, this shell's token written.
    RetaggedForeign,
}

/// Classify `recorded` against `shell`.
///
/// A token without `started` is foreign whenever this shell's start time is readable, since a
/// start-less token cannot tell a reused pid from the original. Where no start time is readable
/// the pid alone decides, and a reused pid then passes for this shell (a named residual).
#[must_use]
pub fn token_standing(recorded: Option<&SessionToken>, shell: Option<&ShellProcess>) -> TokenStanding {
    let Some(recorded) = recorded else {
        return TokenStanding::Unclaimed;
    };
    let Some(shell) = shell else {
        return TokenStanding::Own;
    };
    if recorded.pid != shell.pid {
        return TokenStanding::Foreign;
    }
    match (recorded.started, shell.started) {
        (Some(recorded), Some(own)) if recorded == own => TokenStanding::Own,
        (Some(_), Some(_)) | (None, Some(_)) => TokenStanding::Foreign,
        (_, None) => TokenStanding::Own,
    }
}

/// Whether any record or identity record is session-owned.
fn holds_session_records(state: &ExecState) -> bool {
    let is_session = |owner: &Owner| matches!(owner, Owner::Session { .. });
    state.applied.iter().any(|record| is_session(&record.owner))
        || state
            .replay
            .launch_identities
            .iter()
            .any(|record| is_session(&record.owner))
}

/// Settle who owns the session before the hook composes anything.
///
/// A foreign token retags every session record and identity inherited, resets the session cache,
/// sets the selection to `FollowCwd` and claims. An unclaimed `FollowCwd` state holding session
/// records retags them before the claim: only `ocx shell` writes them, always `Pinned`, so under
/// `FollowCwd` with no claim they are foreign or forged. An unclaimed `Pinned` state is claimed as it
/// stands. A token-less hook (`shell` is `None`) never claims, never retags and never finds a token foreign.
pub fn claim_session(state: &mut ExecState, shell: Option<&ShellProcess>) -> Claim {
    match token_standing(state.session.token.as_ref(), shell) {
        TokenStanding::Own => Claim::Held,
        TokenStanding::Foreign => {
            // Retag reads the selection and the project hint, so it runs before either is reset.
            state.retag_sessions();
            state.selection = Selection::FollowCwd;
            state.session = SessionCache {
                token: shell.map(ShellProcess::token),
                ..SessionCache::default()
            };
            Claim::RetaggedForeign
        }
        TokenStanding::Unclaimed => {
            // A token-less hook never claims, so an unclaimed state would be retagged on every prompt.
            let retag =
                shell.is_some() && matches!(state.selection, Selection::FollowCwd) && holds_session_records(state);
            if retag {
                state.retag_sessions();
            }
            state.session.token = shell.map(ShellProcess::token);
            match (retag, shell) {
                (true, _) => Claim::RetaggedUnclaimed,
                (false, Some(_)) => Claim::Claimed,
                (false, None) => Claim::Held,
            }
        }
    }
}

/// The hook's desired composition under `FollowCwd` for the project the working directory resolved.
///
/// Subtracts what `inherited_scopes` already holds; `Pinned` sessions recompose their tier's
/// consented groups instead and never ask this.
#[must_use]
pub fn follow_cwd_composition(state: &ExecState, project: Option<&ProjectIdentity>) -> reconcile::Composition {
    reconcile::desired_composition(&state.inherited_scopes, project.map(|identity| identity.dir.as_path()))
}

/// The start time of process `pid` in clock ticks since boot: field 22 of `/proc/<pid>/stat`.
///
/// `None` when the file is unreadable or malformed.
#[cfg(target_os = "linux")]
pub async fn process_start_time(pid: u32) -> Option<u64> {
    let stat = tokio::fs::read(format!("/proc/{pid}/stat")).await.ok()?;
    parse_proc_start_time(&String::from_utf8_lossy(&stat))
}

/// No cheap reader on this platform: the pid alone identifies the shell.
#[cfg(not(target_os = "linux"))]
pub async fn process_start_time(_pid: u32) -> Option<u64> {
    None
}

/// Field 22 of a `/proc/<pid>/stat` line.
///
/// The command name (field 2) is parenthesised and may hold spaces and parentheses, so the fields
/// are counted from the last `)`: a hostile name cannot shift them.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_start_time(stat: &str) -> Option<u64> {
    let after_name = &stat[stat.rfind(')')? + 1..];
    // Field 3 (the state) is the first one after the name, so field 22 is the 20th.
    after_name.split_ascii_whitespace().nth(19)?.parse().ok()
}

/// The digest [`exec_state::Session::messages_fp`] records for one prompt's diagnostics; empty for none.
// Length-prefixed, or `["ab", "c"]` and `["a", "bc"]` digest alike.
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

// -- The pinned selection, and the execution state the hook records --------------------------

/// The project a pinned project tier names, probed without opening any file.
///
/// The directory comes from the received state, so it is untrusted: consent has to run before
/// `ocx.toml` is parsed, and consent itself reads `ocx.lock`. One `lstat` pair therefore refuses
/// anything that is not a regular file, a FIFO or a symlink included, before either is opened.
/// `None` means the layer cannot be refreshed and stays as it is.
pub async fn pinned_identity(tier: &PinnedTier) -> Option<ProjectIdentity> {
    let PinnedTier::Project { dir } = tier else {
        return None;
    };
    // Joined onto a relative `dir`, `ocx.toml` would name a file under the hook's working directory.
    if !dir.is_absolute() {
        return None;
    }
    let config = dir.join("ocx.toml");
    let regular = |metadata: &std::fs::Metadata| metadata.file_type().is_file();
    if !tokio::fs::symlink_metadata(&config).await.is_ok_and(|m| regular(&m)) {
        return None;
    }
    // An absent lock is consent's own business (a `paths` grant reaches it); only a non-file is refused.
    match tokio::fs::symlink_metadata(lock_path_for(&config)).await {
        Ok(metadata) if !regular(&metadata) => return None,
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return None,
        Ok(_) | Err(_) => {}
    }
    ProjectIdentity::resolve(config).await.ok()
}

/// The notice owed when a pinned project tier's `ocx.toml` is gone; `None` while it is there.
///
/// The layer stays either way. `NotFound` only: a config that exists but cannot be read is another
/// condition and keeps its silence.
pub async fn vanished_pin_notice(tier: &PinnedTier) -> Option<String> {
    let PinnedTier::Project { dir } = tier else {
        return None;
    };
    if !dir.is_absolute() {
        return None;
    }
    match tokio::fs::symlink_metadata(dir.join("ocx.toml")).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(format!(
            "ocx: pinned project {} no longer has an ocx.toml; keeping its layer",
            printable(&dir.to_string_lossy())
        )),
        _ => None,
    }
}

/// What a pinned selection composes: its own tier, never the working directory's scope.
#[must_use]
pub fn pinned_composition(tier: &PinnedTier, identity: Option<&ProjectIdentity>) -> reconcile::Composition {
    match tier {
        PinnedTier::Global => reconcile::Composition {
            global: true,
            project: None,
        },
        PinnedTier::Project { .. } => reconcile::Composition {
            global: false,
            project: identity.map(|identity| identity.dir.clone()),
        },
    }
}

/// The groups a pinned selection recomposes; none recorded means the default group.
#[must_use]
pub fn pinned_groups(pinned: &exec_state::Pinned) -> Vec<String> {
    composed_groups(&pinned.groups)
}

/// The project config the watch set stats: only a project this prompt composes.
///
/// An inherited project is a snapshot, so an edit to its files must change nothing.
#[must_use]
pub fn watched_config<'a>(
    composition: &reconcile::Composition,
    project: Option<&'a ProjectIdentity>,
) -> Option<&'a Path> {
    composition.project.as_ref()?;
    project.map(|identity| identity.config_path.as_path())
}

/// A state-derived string made safe and ASCII for a notice line.
#[must_use]
pub fn printable(text: &str) -> String {
    exec_state::display_safe(text)
        .chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .collect()
}

/// The one-line notice a pinned shell prints when the working directory enters another project.
///
/// `None` when the working directory is outside every project or inside the pinned one. The line
/// names the pinned selection and the project left unapplied, so a repeat in the same place is
/// the same line and is shown once through the messages digest.
#[must_use]
pub fn pinned_cd_notice(
    pinned: &exec_state::Pinned,
    pinned_project: Option<&ProjectIdentity>,
    cwd_project: Option<&ProjectIdentity>,
) -> Option<String> {
    let cwd = cwd_project?;
    let label = match &pinned.tier {
        PinnedTier::Global => "the global toolchain".to_owned(),
        PinnedTier::Project { dir } => {
            let own = pinned_project.map_or(dir.as_path(), |identity| identity.dir.as_path());
            if own == cwd.dir {
                return None;
            }
            format!("the project at {}", printable(&own.to_string_lossy()))
        }
    };
    Some(format!(
        "ocx: this shell is pinned to {label}; the project at {} is not applied",
        printable(&cwd.dir.to_string_lossy())
    ))
}

/// The notice for a state that lost records to the size limit; `None` when nothing was lost.
///
/// `encoded_len` is the size of the value this write exported, `None` when the notice reports a loss
/// the received state already carried.
#[must_use]
pub fn truncation_notice(truncated: exec_state::Truncation, encoded_len: Option<usize>) -> Option<String> {
    if truncated == exec_state::Truncation::None {
        return None;
    }
    let size = encoded_len.map_or_else(String::new, |len| format!(" ({len} bytes)"));
    Some(format!(
        "ocx: this shell's environment state outgrew its size limit{size} and was truncated: {}; \
         values ocx applied earlier may stay when you leave their project",
        crate::exec_env::truncation_words(truncated)
    ))
}

/// The encoded `__OCX_ENV_STATE` value and the notice owed when writing it dropped records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateWrite {
    /// The value to export; `None` removes the variable.
    pub value: Option<String>,
    /// Set when this write truncated beyond what the state already carried.
    pub notice: Option<String>,
}

/// Encode the state the hook leaves behind, through the codec's overflow ladder.
#[must_use]
pub fn write_state(state: &ExecState) -> StateWrite {
    let encoded = exec_state::encode(state);
    let notice = (encoded.truncated > state.truncated)
        .then(|| truncation_notice(encoded.truncated, Some(encoded.value.as_ref().map_or(0, String::len))))
        .flatten();
    StateWrite {
        value: encoded.value,
        notice,
    }
}

/// Whether `messages` are the ones the previous prompt already announced.
#[must_use]
pub fn already_announced(state: &ExecState, messages: &[String]) -> bool {
    !messages.is_empty() && state.session.messages_fp == messages_fingerprint(messages)
}

/// Carry `previous` forward verbatim, recording `messages` as announced.
// Not `next_state`: it would retire the scopes a refused prompt must keep.
#[must_use]
pub fn announced(previous: &ExecState, messages: &[String]) -> ExecState {
    let mut next = previous.clone();
    next.session.messages_fp = messages_fingerprint(messages);
    next
}

/// Whether this prompt leaves a pinned project's recorded layer alone.
///
/// A pinned project that was not composed (refused by consent, unreadable, or not identifiable)
/// keeps its spawn-time layer: nothing is removed and nothing is rewritten.
#[must_use]
pub fn keeps_pinned_layer(selection: &Selection, outcome: &Outcome) -> bool {
    matches!(
        selection,
        Selection::Pinned(exec_state::Pinned {
            tier: PinnedTier::Project { .. },
            ..
        })
    ) && outcome.slot.is_none()
}

/// The scope a `Composed` session owner refreshes; `None` for every other owner.
fn composed_scope(owner: &Owner) -> Option<exec_state::ScopeId> {
    match owner {
        Owner::Session {
            scope,
            source: exec_state::SessionSource::Composed,
        } => Some(*scope),
        _ => None,
    }
}

/// The union `desired` set the state planner diffs: `global ++ session ++ project`.
pub fn session_desired(outcome: &Outcome) -> Vec<Entry> {
    let mut desired = outcome.global.clone();
    if outcome.session_enforced {
        desired.extend(outcome.session.entries());
    }
    desired.extend(outcome.project.iter().cloned());
    desired
}

/// Diff `outcome` against the live environment, scoped by the state's records.
pub fn plan_for_state(state: &ExecState, outcome: &Outcome, owned: &[&Path], current: &Env) -> Plan {
    let desired = session_desired(outcome);
    if !keeps_pinned_layer(&state.selection, outcome) {
        return reconcile::plan_session(&desired, current, state, owned);
    }
    // Shielded as inherited, the kept layer is neither retired nor claimed by the diff.
    let mut shielded = state.clone();
    for record in &mut shielded.applied {
        if composed_scope(&record.owner) == Some(exec_state::ScopeId::Project) {
            record.owner = Owner::Inherited;
        }
    }
    reconcile::plan_session(&desired, current, &shielded, owned)
}

/// The state the next prompt reads: `previous` with the session records rebuilt from `outcome`.
///
/// Only `Composed` records of the scopes this prompt composed are replaced. Inherited and
/// `Invocation` records, the header, the policy, the replay env and the inherited scopes pass
/// through untouched. `current` is the live shell before this prompt's plan is applied.
#[must_use]
pub fn next_state(previous: &ExecState, outcome: &Outcome, current: &Env, fingerprint: &str) -> ExecState {
    use exec_state::{ProjectScopeHint, ScopeId};

    let kept = keeps_pinned_layer(&previous.selection, outcome);
    let rebuilt = |owner: &Owner| composed_scope(owner).is_some_and(|scope| scope == ScopeId::Global || !kept);

    let mut next = previous.clone();
    next.applied.retain(|record| !rebuilt(&record.owner));
    next.replay.launch_identities.retain(|record| !rebuilt(&record.owner));

    // Each scope's priors are read from the environment the one before it left.
    let mut scratch = current.clone();
    let global = reconcile::session_writable(&outcome.global, previous);
    next.applied
        .extend(session_records(ScopeId::Global, &global, previous, &mut scratch));
    scratch.apply_entries(&outcome.session.persistent_entries());
    if !kept {
        // The trampoline directory is recorded as the project's first element, so leaving `bin` mode or
        // the project retires it like any other element instead of waiting on the owned-prefix repair.
        let mut project: Vec<Entry> = outcome.session.project_entry().into_iter().collect();
        project.extend(outcome.project.iter().cloned());
        let project = reconcile::session_writable(&project, previous);
        next.applied
            .extend(session_records(ScopeId::Project, &project, previous, &mut scratch));
        next.replay
            .launch_identities
            .extend(identity_records(outcome.project_identities.as_ref(), ScopeId::Project));
    }
    // The project's own entry wins a digest both scopes name, as the innermost export does.
    let held: BTreeSet<String> = next
        .replay
        .launch_identities
        .iter()
        .filter(|record| composed_scope(&record.owner) == Some(ScopeId::Project))
        .map(|record| record.digest.clone())
        .collect();
    next.replay.launch_identities.extend(
        identity_records(outcome.global_identities.as_ref(), ScopeId::Global)
            .into_iter()
            .filter(|record| !held.contains(&record.digest)),
    );

    next.session.fp = fingerprint.to_owned();
    next.session.messages_fp = messages_fingerprint(&outcome.messages);
    // Never cache `Activate` (a consent input) or a yield (its sentinel is not fingerprinted).
    next.session.verdict = match (outcome.inert, outcome.resolved) {
        (true, _) => Some(exec_state::Verdict::Inert),
        (false, false) => Some(exec_state::Verdict::NoProject),
        (false, true) => None,
    };
    // Written with the records it describes, so the hint never outlives the project's layer.
    if !kept {
        next.session.project = outcome.slot.as_ref().map(|slot| ProjectScopeHint {
            key: slot.key.clone(),
            dir: slot.dir.clone(),
        });
    }
    next
}

/// The session identity records of the names a scope composed its packages under, if any.
fn identity_records(
    identities: Option<&LaunchIdentities>,
    scope: exec_state::ScopeId,
) -> Vec<exec_state::IdentityRecord> {
    identities
        .map(|identities| {
            identities.to_records(Owner::Session {
                scope,
                source: exec_state::SessionSource::Composed,
            })
        })
        .unwrap_or_default()
}

/// The `Composed` records for `entries`, applied in order to `scratch`.
///
/// A record `previous` already holds for the same value is carried over unchanged, so a refresh never
/// forgets what a key looked like before ocx first touched it.
fn session_records(
    scope: exec_state::ScopeId,
    entries: &[Entry],
    previous: &ExecState,
    scratch: &mut Env,
) -> Vec<exec_state::AppliedValue> {
    let owner = Owner::Session {
        scope,
        source: exec_state::SessionSource::Composed,
    };
    let earlier: Vec<&exec_state::AppliedValue> =
        previous.applied.iter().filter(|record| record.owner == owner).collect();
    let mut records: Vec<exec_state::AppliedValue> = Vec::new();
    for entry in entries {
        let value = exec_state::ComposedValue::from(entry);
        if let Some(record) = session_record(scratch, &value, &owner, &earlier) {
            fold_record(&mut records, record);
        }
        scratch.apply_entries(std::slice::from_ref(entry));
    }
    records
}

/// Adds `record`, folding a repeat of the same key: an element once, a constant newest-wins over the
/// oldest prior.
fn fold_record(records: &mut Vec<exec_state::AppliedValue>, mut record: exec_state::AppliedValue) {
    use exec_state::ValueKind;

    let same = |other: &exec_state::AppliedValue| other.kind == record.kind && same_key(&other.key, &record.key);
    match record.kind {
        ValueKind::Constant => {
            if let Some(at) = records.iter().position(same) {
                record.prior = records.remove(at).prior;
            }
        }
        ValueKind::Path | ValueKind::List => {
            if records.iter().any(|other| same(other) && other.value == record.value) {
                return;
            }
        }
    }
    records.push(record);
}

fn same_key(left: &str, right: &str) -> bool {
    ocx_config::env::EnvKey::new(left) == ocx_config::env::EnvKey::new(right)
}

/// Whether the secret rule forbids any record for `key`: a credential or an `OCX_AUTH_*` key.
fn is_never_recorded(key: &str) -> bool {
    const AUTH_PREFIX: &str = "OCX_AUTH_";
    ocx_env::keys_with(ocx_env::Propagation::Credential).any(|credential| credential.eq_ignore_ascii_case(key))
        || key
            .get(..AUTH_PREFIX.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(AUTH_PREFIX))
}

/// The record of one applied value, or `None` when it must not be recorded.
fn session_record(
    start: &Env,
    value: &exec_state::ComposedValue,
    owner: &Owner,
    earlier: &[&exec_state::AppliedValue],
) -> Option<exec_state::AppliedValue> {
    use exec_state::{Prior, RecordedValue, ValueKind};

    if is_never_recorded(&value.key) {
        return None;
    }
    let record = |recorded: RecordedValue, pre_existing: bool, separator: Option<String>, prior: Option<Prior>| {
        exec_state::AppliedValue {
            key: value.key.clone(),
            value: recorded,
            kind: value.kind,
            separator,
            pre_existing,
            owner: owner.clone(),
            prior,
        }
    };
    let held_before = |recorded: &RecordedValue| {
        earlier
            .iter()
            .find(|other| other.kind == value.kind && same_key(&other.key, &value.key) && &other.value == recorded)
            .map(|other| (*other).clone())
    };
    match value.kind {
        ValueKind::Path | ValueKind::List if value.value.is_empty() => None,
        ValueKind::Path => {
            let recorded = RecordedValue::Literal(value.value.clone());
            let present = start.get(&value.key).is_some_and(|existing| {
                let remaining = ocx_util::path::remove_segment(existing, std::ffi::OsStr::new(&value.value));
                let segments = |text: &std::ffi::OsStr| {
                    std::env::split_paths(text)
                        .filter(|segment| !segment.as_os_str().is_empty())
                        .count()
                };
                segments(&remaining) != segments(existing)
            });
            Some(held_before(&recorded).unwrap_or_else(|| record(recorded, present, None, None)))
        }
        ValueKind::List => {
            let recorded = RecordedValue::Literal(value.value.clone());
            let separator = value
                .separator
                .as_deref()
                .filter(|separator| !separator.is_empty())
                .unwrap_or(ocx_package::metadata::env::list::DEFAULT_SEPARATOR);
            let present = start.get(&value.key).is_some_and(|existing| {
                format!("{separator}{}{separator}", existing.to_string_lossy())
                    .contains(&format!("{separator}{}{separator}", value.value))
            });
            Some(held_before(&recorded).unwrap_or_else(|| record(recorded, present, Some(separator.to_owned()), None)))
        }
        // A constant `PATH` record would make the state unreadable.
        ValueKind::Constant if same_key(&value.key, "PATH") || same_key(&value.key, "PATHEXT") => None,
        ValueKind::Constant => {
            let recorded = RecordedValue::Literal(value.value.clone());
            let live = start.get(&value.key);
            let own_value_is_live = |old: &exec_state::AppliedValue| {
                let RecordedValue::Literal(text) = &old.value;
                live.is_some_and(|live| live == std::ffi::OsStr::new(text))
            };
            let older = earlier
                .iter()
                .find(|other| other.kind == ValueKind::Constant && same_key(&other.key, &value.key));
            // The prior survives only while the live value is still what ocx wrote.
            let carried = older.filter(|old| own_value_is_live(old));
            if let Some(old) = carried
                && old.value == recorded
            {
                return Some((*old).clone());
            }
            let prior = match carried {
                Some(old) => old.prior.clone(),
                None => Some(match live {
                    None => Prior::Unset,
                    Some(existing) => match existing.to_str() {
                        Some(existing) => Prior::Value(existing.to_owned()),
                        None => Prior::Withheld,
                    },
                }),
            };
            Some(record(recorded, false, None, prior))
        }
    }
}

/// Whether a revert must retain the recorded scope: `recorded_dir` is an ancestor-or-self of the CWD
/// and its `ocx.toml` is still a regular file. Blocking; revert path only.
pub fn walk_is_indeterminate(recorded_dir: Option<&Path>, cwd: Option<&Path>) -> bool {
    walk_is_indeterminate_in(recorded_dir, cwd, ocx_config::exec_state::received::policy())
}

/// [`walk_is_indeterminate`] with the received policy named: `OCX_NO_PROJECT` at its env tier.
fn walk_is_indeterminate_in(
    recorded_dir: Option<&Path>,
    cwd: Option<&Path>,
    received: Option<&ocx_config::exec_state::Policy>,
) -> bool {
    let Some(recorded_dir) = recorded_dir else {
        return false;
    };
    // The carrier is untrusted: a relative `dir` (even `""`) would probe the CWD and pin the scope forever.
    if !recorded_dir.is_absolute() {
        return false;
    }
    if ocx_config::exec_state::received::neutral_bool(
        &ocx_env::OCX_NO_PROJECT,
        received.and_then(|policy| policy.no_project),
    ) {
        return false;
    }
    let Some(cwd) = cwd else {
        // Unknowable is not negative: retain.
        return true;
    };
    if !cwd.starts_with(recorded_dir) {
        return false;
    }
    // Not `metadata`: the CWD walk rejects a symlinked `ocx.toml` too.
    std::fs::symlink_metadata(recorded_dir.join("ocx.toml")).is_ok_and(|meta| meta.file_type().is_file())
}

/// The carrier-`dir` guard lives at the **one** place that `dir` reaches the
/// filesystem: `plan` reads no identity label and builds no path, so a forged-`dir`
/// test there would be green in every state. Also: the one derivation of a
/// project's identity.
#[cfg(test)]
mod identity_tests {
    use super::ProjectIdentity;

    /// `key` is the `state/projects/<key>/` stamp key, and it must be the
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
    /// `shell/` may not import `ocx_project`, so
    /// `reconcile::watch_paths` derives the project's `ocx.lock` from
    /// `config_path.parent()` itself rather than calling
    /// [`ocx_project::lock::lock_path_for`]. That is a second derivation of
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
        let file_structure =
            ocx_store::file_structure::FileStructure::with_root(std::path::PathBuf::from("/tmp/ocx_home"));
        let config = std::path::Path::new("/work/proj/custom.toml");

        let paths = ocx_shell::shell::reconcile::watch_paths(&file_structure, Some(config), None, None);

        assert!(
            paths.contains(&ocx_project::lock::lock_path_for(config)),
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

    use super::{walk_is_indeterminate, walk_is_indeterminate_in};

    /// A received `no_project` stops the revert path from retaining a scope the walk would no longer
    /// find; the twin without it retains.
    #[test]
    fn a_received_no_project_ends_the_retention_of_a_recorded_scope() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        #[cfg(windows)]
        let honest = Path::new(r"C:\work\acme");
        #[cfg(not(windows))]
        let honest = Path::new("/work/acme");
        let received = ocx_config::exec_state::Policy {
            no_project: Some(true),
            ..Default::default()
        };
        assert!(
            walk_is_indeterminate_in(Some(honest), None, None),
            "control: an unreadable CWD retains an honest recorded scope"
        );
        assert!(
            !walk_is_indeterminate_in(Some(honest), None, Some(&received)),
            "a received no_project means no walk finds the project, so nothing is retained"
        );
    }

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

// `shell/` must not reach for `project/`. `ocx_shell`'s manifest cannot list
// `ocx_project` (which depends on it: a cycle Cargo refuses), so that import is an
// unresolved-crate error; the former source-walk guard could no longer go red and
// was deleted rather than re-pointed, since a guard that cannot fire reads as coverage.

/// The clause-2 evidence gate on [`consent_evidence`].
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

    use ocx_config::shell::ConsentScopeSpec;
    use ocx_config::shell::ShellConsent;
    use ocx_project::consent;
    use ocx_project::{DECLARATION_HASH_VERSION, LockMetadata, LockVersion, LockedTool, ProjectLock};
    use ocx_store::file_structure::{PackageStore, record_origin};
    use ocx_trust::ScopeSpec;

    use super::consent_evidence;

    /// The org every fixture below both claims and genuinely resolved from, so
    /// a corroborated read has exactly one answer.
    const GRANTED_SOURCE: &str = "ocx.sh/acme-corp";

    /// The digest the fixture lock pins and the fixture store materializes.
    const LEAF_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn host() -> ocx_oci::Platform {
        "linux/amd64".parse().expect("valid host platform")
    }

    fn leaf_digest() -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256(LEAF_HEX.to_owned())
    }

    fn identifier(registry: &str, repository: &str) -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry(repository, registry)
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
                declared: None,
                name: "cmake".into(),
                group: "default".into(),
                repository: ocx_oci::Repository::new(registry, path),
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
        let pinned =
            ocx_oci::PinnedPackageRef::try_from(identifier(registry, "any/repo").clone_with_digest(leaf_digest()))
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

    /// With no `namespaces` grant configured, the per-prompt hop does
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
// Specification tests: every case names the contract it traces to and the
// mutation that must turn it red.
// ---------------------------------------------------------------------------

/// What the session directories do to `PATH`.
///
/// **Everything here asserts the resulting `PATH` string, never the desired
/// vector's order**: `export_path` prepends, so the vector runs backwards, and a
/// vector assertion would agree with an implementation that copied
/// `setup::session_path_directories` verbatim — the very inversion forbidden.
/// [`EnvEntriesExt::apply_entries`] is the fold because `reconcile::plan`'s own
/// `settled_keys` uses it, so its answer *is* what the emitted lines produce. The
/// byte-exact end-to-end through a real `bash` sits beside `plan_lines` in
/// `crates/ocx_cli/src/command/self_group/activate.rs`.
#[cfg(test)]
mod session_path_tests {
    use std::path::{Path, PathBuf};

    use ocx_config::env::Env;
    use ocx_config::exec_state::{ExecState, Truncation};
    use ocx_package::metadata::env::apply::EnvEntriesExt;
    use ocx_package::metadata::env::entry::Entry;
    use ocx_package::metadata::env::modifier::ModifierKind;

    use super::{Outcome, ProjectIdentity, SessionPath, next_state, plan_for_state, session_desired};

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

    /// The two unconditional session entries, project slot empty —
    /// what `env` and `none` mode both carry.
    fn session() -> SessionPath {
        SessionPath {
            global_bin: global_bin(),
            project_bin: None,
            install_bin: install_bin(),
        }
    }

    /// The outcome every arm of `session` builds: the session pair is
    /// present whatever the `activate` mode said.
    fn outcome(session: SessionPath, global: Vec<Entry>, project: Vec<Entry>, owned_home: Option<PathBuf>) -> Outcome {
        Outcome {
            session,
            session_enforced: true,
            global,
            project,
            global_identities: None,
            project_identities: None,
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

    // ── The live defect ─────────────────────────────────────────────

    /// **The headline.** `ocx_install_bin_path` sits under `$OCX_HOME`,
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
    /// Red state — today's `goat`: `session_desired` returns
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
        // sharpest one — the session pair survives even here.
        let global = vec![path_entry(&format!("{OCX_HOME}/packages/aa/bb/content/bin"))];
        let applied = outcome(session(), global, Vec::new(), Some(PathBuf::from(PROJECT_HOME)));
        let state = next_state(&ExecState::default(), &applied, &Env::clean(), "fp-1");

        let plan = plan_for_state(&state, &applied, &[Path::new(OCX_HOME)], &current);

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

    /// And the survival is not passive: the desired set *contributes*
    /// both directories, so a shell that lost them (a `PATH` overwritten by a
    /// login script, the lost-state case) gets them back.
    ///
    /// Red state: make [`SessionPath::entries`] emit only what the mode
    /// selected, or drop the splice from `session_desired`.
    #[test]
    fn c059_a_path_that_lost_both_session_directories_gets_them_back() {
        let applied = outcome(session(), Vec::new(), Vec::new(), Some(PathBuf::from(PROJECT_HOME)));
        let path = resulting_path(&[Path::new("/usr/bin")], &session_desired(&applied));

        position(&path, &install_bin());
        position(&path, &global_bin());
    }

    // ── The order, read off `PATH` ─────────────────────────

    /// Front to back on `PATH`: the project's
    /// `<home>/toolchain/active/bin` (`bin` mode only), then `$OCX_HOME/toolchain/active/bin`,
    /// then `ocx_install_bin_path`.
    ///
    /// The most specific tier that pinned a name answers for it, `ocx`
    /// included — the installed binary is the floor, never a lid.
    ///
    /// Asserted on the *resulting `PATH`*, which is the whole ordering rule: the
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

    /// The project slot is `bin` mode's alone. In `env` and `none` mode
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

    // ── Where the session block sits in the desired vector ─────────

    /// The desired vector is
    /// `global ++ session ++ project`, so `PATH` front to back reads:
    /// project-composed ▸ project `bin/` ▸ `$OCX_HOME/toolchain/active/bin` ▸
    /// `install_bin` ▸ global-composed.
    ///
    /// The load-bearing half is the **last** comparison: the alternative
    /// splice (`global ++ project ++ session`, or session first) puts the
    /// *global* tier's composed entries ahead of the project's own `bin/`, so
    /// a globally installed `cmake` shadows the project's trampoline for the
    /// same name — the forbidden tier inversion.
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

        let path = resulting_path(&[Path::new("/usr/bin")], &session_desired(&applied));

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

    /// Stated as the property it exists to defend: a tool the **global**
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

        let path = resulting_path(&[Path::new("/usr/bin")], &session_desired(&applied));
        assert!(
            position(&path, &project_bin()) < position(&path, Path::new(&global_tool)),
            "RUL-87: a global `cmake` must not win over the project's `cmake` trampoline; got {path:#?}"
        );
    }

    /// **The global tier's own `env` → `bin` transition, in one plan.**
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
    /// (guess-unsetting it is forbidden, so the prior is the only operand).
    ///
    /// The session pair rides through untouched, which is the third
    /// assertion: `bin` composes nothing *and* keeps the global toolchain
    /// reachable through its trampolines.
    ///
    /// Red state: return `Vec::new()` for `next_state`'s global records — the record
    /// this plan consumes — and both retirements vanish.
    #[test]
    fn d3_leaving_global_env_mode_retires_the_element_and_restores_the_constant() {
        let tool_bin = format!("{OCX_HOME}/packages/gg/global/content/bin");
        let ocx_jdk = format!("{OCX_HOME}/packages/gg/jdk/content");

        // The shell before ocx ever composed — the only place the user's own
        // `JAVA_HOME` is still visible, which is what `next_state` captures.
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
        let state = next_state(&ExecState::default(), &was_env, &before, "fp-1");

        // The shell as that prompt left it.
        let mut current = before.clone();
        current.apply_entries(&session_desired(&was_env));

        // This prompt: `$OCX_HOME/ocx.toml` now states `activate = "bin"`, so
        // `global_prompt_entries` returns an empty vector — and the session pair stays in every
        // mode.
        let now_bin = outcome(session(), Vec::new(), Vec::new(), None);
        let plan = plan_for_state(&state, &now_bin, &[Path::new(OCX_HOME)], &current);

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
                ocx_util::path::remove_segment(&existing, std::ffi::OsStr::new(element.as_str())),
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

    // ── The deletion authority's scope ─────────────────────

    /// `owned_prefixes` is `$OCX_HOME` **plus** the
    /// consented, in-scope project's own home — and the second prefix is what
    /// lets the repair reach a stale `<home>/bin` at all: the project's home sits
    /// under the project directory, not under `$OCX_HOME`. The repair runs only
    /// after an all-applied-dropped truncation with no inherited scope,
    /// which these tests state by setting it on the recorded state.
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
        let mut state = next_state(&ExecState::default(), &was_bin, &Env::clean(), "fp-1");
        // Rung 2 drops every applied record along with marking the loss.
        state.applied.clear();
        state.truncated = Truncation::AllAppliedDropped;
        let now_none = outcome(session(), Vec::new(), Vec::new(), Some(PathBuf::from(PROJECT_HOME)));

        let owned: Vec<&Path> = vec![Path::new(OCX_HOME), Path::new(PROJECT_HOME)];
        let plan = plan_for_state(&state, &now_none, &owned, &current);
        let removed: Vec<&str> = plan.removes.iter().map(|(_, element, _)| element.as_str()).collect();
        assert!(
            removed.contains(&project_bin().to_string_lossy().as_ref()),
            "C-063: with the home owned, a `<home>/bin` no arm contributes is removed; got {removed:#?}"
        );

        let narrow = plan_for_state(&state, &now_none, &[Path::new(OCX_HOME)], &current);
        assert!(
            narrow.removes.is_empty(),
            "the two operands must genuinely differ, or the assertion above is vacuous; got {:#?}",
            narrow.removes
        );
    }

    /// A shell leaving `bin` mode, or the project, withdraws `<home>/bin` by the record that put it
    /// there: the state is intact (`truncated` stays `None`), so no owned-prefix repair is in play.
    #[test]
    fn leaving_bin_mode_or_the_project_withdraws_the_trampoline_dir_by_its_record() {
        let mut current = Env::clean();
        current.set(
            "PATH",
            std::env::join_paths([project_bin().as_os_str(), std::ffi::OsStr::new("/usr/bin")]).expect("joins"),
        );
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
        let state = next_state(&ExecState::default(), &was_bin, &Env::clean(), "fp-1");
        assert_eq!(
            state.truncated,
            Truncation::None,
            "the repair gate stays shut for an intact state"
        );

        let owned: Vec<&Path> = vec![Path::new(OCX_HOME), Path::new(PROJECT_HOME)];
        let mut project_free = outcome(session(), Vec::new(), Vec::new(), None);
        project_free.resolved = false;
        project_free.slot = None;
        let now_none = outcome(session(), Vec::new(), Vec::new(), Some(PathBuf::from(PROJECT_HOME)));
        for (label, next) in [("bin to none", now_none), ("leaving the project", project_free)] {
            let plan = plan_for_state(&state, &next, &owned, &current);
            let removed: Vec<&str> = plan.removes.iter().map(|(_, element, _)| element.as_str()).collect();
            assert_eq!(
                removed,
                [project_bin().to_string_lossy().into_owned()],
                "{label}: the trampoline dir leaves PATH by its record, and nothing else does"
            );
        }

        // Still `bin` mode: the dir is desired, so the record retires nothing.
        let plan = plan_for_state(&state, &was_bin, &owned, &current);
        assert!(
            plan.removes.is_empty(),
            "a held `bin` mode removes nothing: {:#?}",
            plan.removes
        );
    }

    /// A prompt that composes nothing leaves the session directories where an outer `ocx exec` put
    /// them: moving them to the front would displace the project entries it composed ahead of them.
    #[test]
    fn a_prompt_that_composes_nothing_does_not_reorder_the_session_directories() {
        let mut current = Env::clean();
        current.set(
            "PATH",
            std::env::join_paths([
                Path::new("/exec/composed/bin"),
                &global_bin(),
                &install_bin(),
                Path::new("/usr/bin"),
            ])
            .expect("joins"),
        );
        let mut standing = outcome(session(), Vec::new(), Vec::new(), None);
        standing.slot = None;
        let owned: Vec<&Path> = vec![Path::new(OCX_HOME)];

        let plan = plan_for_state(&ExecState::default(), &standing, &owned, &current);
        assert!(
            !plan.sets.is_empty(),
            "control: an enforcing prompt does move the session directories to the front"
        );

        standing.session_enforced = false;
        let plan = plan_for_state(&ExecState::default(), &standing, &owned, &current);
        assert!(
            plan.sets.is_empty() && plan.removes.is_empty(),
            "a prompt that composes nothing changes nothing: {plan:#?}"
        );
    }

    /// **The routed edge, refuted.** A dangling `active` does not shelter
    /// the physical spelling from the reconciler's repair.
    ///
    /// The concern routed here was that `owned_spellings`
    /// (`shell/reconcile/plan.rs:768-779`) derives its second spelling with
    /// `std::fs::canonicalize`, which fails on a broken link — so with `active`
    /// dangling only the literal spelling would count as owned and a stale
    /// `shells/<shell>/bin` segment would survive on `PATH`.
    ///
    /// It cannot happen, and the reason is what `owned_prefixes` holds:
    /// `$OCX_HOME` and the project's toolchain **home root**, never a
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
        let home = ocx_store::file_structure::ToolchainHome::new(&root);
        let physical = home.shell_bin(ocx_store::file_structure::DEFAULT_SHELL);
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
        let mut state = next_state(&ExecState::default(), &was_bin, &Env::clean(), "fp-1");
        // Rung 2 drops every applied record along with marking the loss.
        state.applied.clear();
        state.truncated = Truncation::AllAppliedDropped;
        let now_none = outcome(session(), Vec::new(), Vec::new(), Some(root.clone()));

        let owned: Vec<&Path> = vec![Path::new(OCX_HOME), root.as_path()];
        let plan = plan_for_state(&state, &now_none, &owned, &current);
        let removed: Vec<&str> = plan.removes.iter().map(|(_, element, _)| element.as_str()).collect();
        assert!(
            removed.contains(&physical.to_string_lossy().as_ref()),
            "C-078: ownership is component-wise against the home root, which a broken `active` \
             cannot reach; got {removed:#?}"
        );

        let narrow = plan_for_state(&state, &now_none, &[Path::new(OCX_HOME)], &current);
        assert!(
            narrow.removes.is_empty(),
            "the two operands must genuinely differ, or the assertion above is vacuous; got {:#?}",
            narrow.removes
        );
    }

    /// The negative half, and the security-relevant one:
    /// `owned_prefixes` may never carry the bare `toolchain_dir` root. That
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
    /// Red state: pass the configured root to `plan_for_state` in place of the
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
        let mut state = next_state(&ExecState::default(), &was_bin, &Env::clean(), "fp-1");
        // Rung 2 drops every applied record along with marking the loss.
        state.applied.clear();
        state.truncated = Truncation::AllAppliedDropped;
        let now_none = outcome(session(), Vec::new(), Vec::new(), Some(mine.clone()));

        let plan = plan_for_state(&state, &now_none, &[Path::new(OCX_HOME), mine.as_path()], &current);
        let removed: Vec<&str> = plan.removes.iter().map(|(_, element, _)| element.as_str()).collect();
        assert!(
            removed.contains(&mine.join("bin").to_string_lossy().as_ref()),
            "C-063: this project's own home is owned, so its stale entry is withdrawn; got {removed:#?}"
        );
        assert!(
            !removed.contains(&sibling.to_string_lossy().as_ref()),
            "C-063: `<toolchain_dir>/<other project>/toolchain/bin` is not this prompt's to delete; \
             got removes {removed:#?}"
        );
    }

    // ── The six `activate` transitions ──────────────────────────────────────

    /// The outcome each `activate` mode produces for one consented, in-scope
    /// project — the modelled contract, not a call into
    /// the resolver.
    fn outcome_for(mode: ocx_project::activate::ActivateMode) -> Outcome {
        use ocx_project::activate::ActivateMode;
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

    /// All six ordered `activate` transitions. In every one of
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
        use ocx_project::activate::ActivateMode::{Bin, Env as EnvMode, None as NoneMode};

        for (from, to) in [
            (EnvMode, Bin),
            (EnvMode, NoneMode),
            (Bin, EnvMode),
            (Bin, NoneMode),
            (NoneMode, EnvMode),
            (NoneMode, Bin),
        ] {
            let before = outcome_for(from);
            let state = next_state(&ExecState::default(), &before, &Env::clean(), "fp-1");
            let after = outcome_for(to);

            // The `PATH` a shell genuinely holds in the `from` mode, spelled
            // from the contract rather than folded from `session_desired` — a
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
            let plan = plan_for_state(&state, &after, &owned, &current);
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

    /// `none → bin`. The project's trampoline directory arrives
    /// **in front of** both session entries, and the pair behind it keeps
    /// `$OCX_HOME/toolchain/active/bin` ahead of the installed binary.
    ///
    /// Red state: leave [`SessionPath::project_bin`] empty on the transition and
    /// the directory never arrives at all; or swap the session pair, and a
    /// globally pinned `ocx` stops being reachable.
    #[test]
    fn c060_entering_bin_mode_puts_the_trampoline_directory_in_front_of_the_session_pair() {
        use ocx_project::activate::ActivateMode::{Bin, None as NoneMode};

        let before = outcome_for(NoneMode);
        let path_before = resulting_path(&[Path::new("/usr/bin")], &session_desired(&before));
        assert!(
            !path_before
                .iter()
                .any(|segment| segment == &project_bin().to_string_lossy()),
            "`none` mode must not put the trampoline directory on PATH; got {path_before:#?}"
        );

        let path_after = resulting_path(&[Path::new("/usr/bin")], &session_desired(&outcome_for(Bin)));
        let install = position(&path_after, &install_bin());
        let project = position(&path_after, &project_bin());
        let global = position(&path_after, &global_bin());
        assert!(
            project < global && global < install,
            "C-060: `none → bin` inserts in front of the pair; got {path_after:#?}"
        );
    }
}

/// The `bin`-mode stamp gate.
///
/// [`bin_stamp_matches`] answers one question: does `<home>/bin` still hold
/// exactly what the render recorded? Everything below drives that answer
/// directly, because the gate is the only thing standing between a
/// repository-controlled directory and a live shell's `PATH`.
#[cfg(test)]
mod stamp_gate_tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use ocx_store::file_structure::{BinEntryStamp, RenderStamp, RenderStampScope};

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

    /// **The discriminating case.** A fresh clone force-commits
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

    /// The other direction — a name the stamp records that is no longer
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

    /// **Content hashing is the authority; the stat pair only decides
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

    /// **Mtime is never consulted.** Touching an entry's modification
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

    /// **The accepted residual, recorded as a decision rather than
    /// discovered as a surprise.** An in-place overwrite (write, no rename)
    /// preserves the inode, and at the same size it changes none of
    /// `(name, size, file id)`. The entry is therefore never re-hashed and the
    /// stat gate lets it through. Nothing on the emit path re-hashes either, so
    /// the window is "until the next `pull`/`add`/`remove`/`lock`/`update`".
    ///
    /// If this case ever needs to fail, the stat-gate decision has been reopened and its
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

    /// A `file_id` the platform would not report falls **through** to
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

    /// Every filesystem condition is a **mismatch**, never an `Err`: a
    /// `bin/` that is not there is the fail-closed answer, because a prompt has
    /// nothing useful to do with an error and the gate's two negative outcomes
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

    /// An empty stamp over an empty `bin/` is a match, and an empty
    /// stamp over a populated `bin/` is not.
    ///
    /// The degenerate pair, and the second half is the hostile-clone case at its sharpest: a
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

    /// A `bin/` entry that is not a regular file is not silently
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

    /// **`bin/` itself, not only what is under it.** `read_dir`
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

    /// The gate reads `<home>/bin` and nothing else. A stamp is
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

/// The `activate` ladder's first production resolution.
#[cfg(test)]
mod activate_ladder_tests {
    use ocx_project::ProjectConfig;
    use ocx_project::activate::{ACTIVATE_FLOOR, ActivateMode};

    use super::activate_mode;

    /// An `ocx.toml` carrying `activate = <value>` (or none), parsed through
    /// the production reader — the post-consent step, so the fixture uses
    /// the same route the prompt does.
    async fn config_with(activate: Option<&str>) -> (tempfile::TempDir, ProjectConfig) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ocx.toml");
        let declared = activate.map_or_else(String::new, |mode| format!("activate = \"{mode}\"\n"));
        std::fs::write(&path, format!("{declared}\n[tools]\n")).expect("write ocx.toml");
        let config = ProjectConfig::from_path(&path).await.expect("the fixture parses");
        (dir, config)
    }

    /// The file tier outranks the environment tier. Every
    /// less-specific tier is set to a *different* value, so a resolver that
    /// consults them in the wrong order answers something else.
    ///
    /// Red state: order the ladder `environment.or(file)`.
    #[tokio::test]
    async fn c005_c006_the_file_tier_outranks_the_environment_tier() {
        let (_dir, config) = config_with(Some("bin")).await;
        let lock = ocx_env::overrides::lock();
        lock.set(&ocx_env::OCX_TOOLCHAIN_ACTIVATE, "none");

        assert_eq!(activate_mode(&config), ActivateMode::Bin, "C-006: `ocx.toml` decides");
    }

    /// With the file tier absent the environment tier is consulted.
    ///
    /// Red state: drop the environment tier, and this falls to the floor.
    #[tokio::test]
    async fn c006_an_absent_file_tier_falls_to_the_environment_tier() {
        let (_dir, config) = config_with(None).await;
        let lock = ocx_env::overrides::lock();
        lock.set(&ocx_env::OCX_TOOLCHAIN_ACTIVATE, "bin");

        assert_eq!(activate_mode(&config), ActivateMode::Bin);
    }

    /// Both tiers absent resolves to [`ACTIVATE_FLOOR`], which is
    /// asserted **by name**: the review convention this ladder's first
    /// production site starts enforcing is that the floor is passed as the
    /// constant, never re-spelled as `ActivateMode::Env`.
    ///
    /// Red state: pass a literal floor that later diverges from the constant.
    #[tokio::test]
    async fn c007_both_tiers_absent_resolves_to_the_named_floor() {
        let (_dir, config) = config_with(None).await;
        let lock = ocx_env::overrides::lock();
        lock.remove(&ocx_env::OCX_TOOLCHAIN_ACTIVATE);

        assert_eq!(activate_mode(&config), ACTIVATE_FLOOR);
    }

    /// An unrecognised `OCX_TOOLCHAIN_ACTIVATE` warns and leaves the
    /// tier *absent*, so the ladder continues rather than short-circuiting.
    /// With no file tier below it, that lands on the floor.
    ///
    /// Red state: read the variable with `FromStr` directly instead of through
    /// [`ActivateMode::from_env`], which turns an unknown value into an error
    /// or a hard default.
    #[tokio::test]
    async fn c006_an_unrecognised_environment_value_falls_through_to_the_floor() {
        let (_dir, config) = config_with(None).await;
        let lock = ocx_env::overrides::lock();
        lock.set(&ocx_env::OCX_TOOLCHAIN_ACTIVATE, "always");

        assert_eq!(activate_mode(&config), ACTIVATE_FLOOR);
    }

    /// The environment tier never overrides a project that states
    /// `none`, which is the arm a "the env var force-enables it" reading would
    /// break.
    ///
    /// Red state: treat `ActivateMode::None` in the file tier as "unset".
    #[tokio::test]
    async fn c006_a_file_tier_of_none_is_a_stated_value_not_an_absent_one() {
        let (_dir, config) = config_with(Some("none")).await;
        let lock = ocx_env::overrides::lock();
        lock.set(&ocx_env::OCX_TOOLCHAIN_ACTIVATE, "bin");

        assert_eq!(activate_mode(&config), ActivateMode::None);
    }
}

/// Seed the physical `<root>/shells/<shell>/bin` and plant the `active` link a
/// render publishes — **derived** from `ToolchainHome::expected_active_target`, the
/// derivation the `active` gate compares against, never spelled, so a fixture never
/// plants a link the gate rejects for an unintended reason. Every arena driving
/// [`bin_mode_entry`] needs it: the gate withholds on a non-derived `active`, so
/// bare directories would make every positive row unreachable.
#[cfg(test)]
fn plant_active(home: &file_structure::ToolchainHome) {
    std::fs::create_dir_all(home.shell_bin(file_structure::DEFAULT_SHELL)).expect("mkdir <root>/shells/<shell>/bin");
    let _ = std::fs::remove_file(home.active());
    ocx_util::fs::symlink::create(
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

    use crate::{Concurrency, PackageManager};
    use ocx_config::Config;
    use ocx_index::{ChainMode, Index, LocalConfig, LocalIndex};
    use ocx_package::metadata::env::entry::Entry;
    use ocx_project::{DECLARATION_HASH_VERSION, DEFAULT_GROUP, LockMetadata, LockVersion, LockedTool, ProjectLock};
    use ocx_store::file_structure::{
        BinEntryStamp, DEFAULT_SHELL, FileStructure, RenderStamp, RenderStampScope, RenderStampTarget,
    };

    use super::{ProjectIdentity, SessionInput, bin_mode_entry};

    const REGISTRY: &str = "ocx.sh";
    /// Branch A's pin and branch B's pin — "same path, different lock".
    const DIGEST_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const DIGEST_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn host() -> ocx_oci::Platform {
        "linux/amd64".parse().expect("a valid host platform")
    }

    fn identifier() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry("acme/cmake", REGISTRY)
    }

    fn pinned(hex: &str) -> ocx_oci::PinnedPackageRef {
        ocx_oci::PinnedPackageRef::try_from(identifier().clone_with_digest(ocx_oci::Digest::Sha256(hex.to_owned())))
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
                declared: None,
                name: "cmake".into(),
                group: DEFAULT_GROUP.into(),
                repository: ocx_oci::Repository::from(&identifier()),
                platforms: BTreeMap::from([(host().to_string(), ocx_oci::Digest::Sha256(hex.to_owned()))]),
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
        home: ocx_store::file_structure::ToolchainHome,
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

            let home = ocx_store::file_structure::ToolchainHome::new(dir.join(".ocx").join("toolchain"));
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
        /// and answer its stamp — where a render writes.
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

        /// Replace `<root>/active` with `state` (the four negative rows).
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
                    ocx_util::fs::symlink::create(target, &active).expect("repoint active");
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
    /// sorted — the "wrote nothing" oracle the no-prune rule needs.
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
    /// local index because the gate forbids this path composing anything at all —
    /// a call that reached the network would fail here rather than pass slowly.
    struct Session {
        manager: PackageManager,
        local_index: LocalIndex,
        config: Config,
        target: ocx_oci::Platform,
    }

    impl Session {
        fn new(file_structure: &FileStructure) -> Self {
            let local_index = LocalIndex::new(LocalConfig {
                index_store: ocx_index::IndexStore::machine_local(file_structure),
            });
            let index = Index::from_chained(
                LocalIndex::new(LocalConfig {
                    index_store: ocx_index::IndexStore::machine_local(file_structure),
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
                global_identities: None,
                manager: &self.manager,
                local_index: &self.local_index,
                concurrency: Concurrency::Unbounded,
                file_structure,
                config: &self.config,
                target: &self.target,
                project: None,
                composition: ocx_shell::shell::reconcile::Composition::default(),
                groups: Vec::new(),
                pinned: false,
            }
        }
    }

    // ── The stamp must be *this* home's and *this* project's ────────

    /// No stamp at all withholds the entry. The error column: a
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

    /// The positive control. Without it every withholding case below is
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

    // ── The gate withholds on every `active` that is
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

    /// Row 1 — `active` absent. The tree renders trampolines the stamp
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

    /// Row 2 — `active` is a **real directory**, the `cp -rL` / Docker
    /// `COPY` outcome. The prompt path may only read, so the directory
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

    /// Row 3 — `active` repointed **outside** the home. This is the
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

    /// Row 4 — `active` repointed at a **sibling `shells/<other>` that
    /// exists**, so it is contained, resolvable and still wrong.
    ///
    /// The row a containment test cannot catch, and the reason the check is an
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

    /// Identity before content. Under `toolchain_dir` two
    /// projects colliding in `name_for_path`'s 64 bits share one home *and* one
    /// stamp; the scope's project directory is what tells them apart. Without
    /// it, project B's prompt passes over trampolines that bake
    /// `--project '<A>'`.
    ///
    /// Red state: compare `stamp.home` alone and skip
    /// [`RenderStampScope`](ocx_store::file_structure::RenderStampScope).
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

    /// The `home` half of the identity pair: a stamp describing a
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

    /// A global stamp never stands in for a project's. The two live
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

    // ── The committed hostile `bin/` ────────────────────────────────

    /// **The hostile clone end to end.** A fresh clone force-commits
    /// `.ocx/toolchain/shells/default/bin/cmake`; consent is granted; the first prompt in
    /// `bin` mode emits **no** `PATH` entry, so the hostile file never reaches
    /// `PATH`.
    ///
    /// The stamp here is a legitimate one over `ninja`, which is what makes
    /// this the set-equality case rather than the trivial "no stamp" one: every name
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

    // ── The prompt path never prunes ────────────────────────────────

    /// The stale window between a lock change and the next composing
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

    /// The heal is **strictly after** the gate. A tree the gate
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

    // ── Heal before emit ────────────────────────────────────

    /// Branch switch at the same path, different lock, no
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

    /// A link the render never wrote is *created* by the heal,
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

    /// The heal is scoped to the **default group** alone. A
    /// non-default group's stale link is left for the composing side,
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

    /// No compose, no metadata read, no network. The manager handed in
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

    /// The heal's **refusal** withholds the entry, and the
    /// stamp gate cannot substitute for it.
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
        // where it stands.
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

/// **One directory, one derivation.** `$OCX_HOME/symlinks/<ocx cli id>/current/content/bin`
/// was derived in three production places:
/// [`FileStructure::ocx_install_bin_path`](ocx_store::file_structure::FileStructure::ocx_install_bin_path),
/// `crate::launcher::generate::trampoline_ocx_binary` and the
/// `shell::reconcile::fingerprint` watch set. Two disagreeing spellings make
/// `repair_owned_segments` **delete one and add the other on every prompt** (both
/// sit under the owned `$OCX_HOME`; only one is ever desired). Structural because
/// no behavioural test tells "two spellings that happen to agree" from "one
/// spelling", and the day they stop agreeing the prompt starts flapping.
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

    /// Exactly one file derives the directory, and it
    /// is `ocx_store`'s `file_structure.rs`, which owns the symlink store the
    /// derivation runs over. Every other reader calls
    /// [`FileStructure::ocx_install_bin_path`](ocx_store::file_structure::FileStructure::ocx_install_bin_path).
    ///
    /// The canonical home moved there from `setup.rs` with inversion 1.19: the
    /// three readers all sit below the installer, so asking `setup` for a store
    /// path was the last edge pointing up at it.
    ///
    /// Red state — today's `goat`: `launcher/generate.rs` and
    /// `shell/reconcile/fingerprint.rs` each re-derive it inline.
    #[test]
    fn only_the_store_derives_the_install_bin_directory() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        // Three roots: the derivation's home left for `ocx_store`,
        // and a guard that kept scanning only the two crates it is not in would
        // report a clean sweep over a tree that no longer holds its subject.
        let roots: [(PathBuf, &str); 3] = [
            (manifest.join("src"), "ocx_lib"),
            (manifest.join("../ocx_cli/src"), "ocx_cli"),
            (manifest.join("../ocx_store/src"), "ocx_store"),
        ];
        let mut files = Vec::new();
        for (root, _) in &roots {
            files.extend(rust_sources(root));
        }
        assert!(files.len() > 50, "the walk found almost nothing, so it scanned nothing");

        let canonical = manifest.join("../ocx_store/src/file_structure.rs");
        let named = |path: &Path| -> String {
            for (root, crate_name) in &roots {
                if let Ok(rel) = path.strip_prefix(root) {
                    return format!("{crate_name}/{}", rel.display());
                }
            }
            path.display().to_string()
        };
        let mut derivers: Vec<String> = files
            .iter()
            .filter(|path| derives_install_bin(&std::fs::read_to_string(path).expect("a source file is readable")))
            .map(|path| named(path))
            .collect();
        derivers.sort();

        // The non-vacuity twin, first: the scanner must still recognise the one
        // sanctioned derivation, or an empty offender list below is green
        // because the needle stopped matching.
        assert!(
            derives_install_bin(
                &std::fs::read_to_string(&canonical).expect("ocx_store's file_structure.rs is readable")
            ),
            "the scanner no longer recognises `FileStructure::ocx_install_bin_path`'s own \
             derivation, so the assertion below proves nothing"
        );

        assert_eq!(
            derivers,
            vec!["ocx_store/file_structure.rs".to_owned()],
            "RUL-63/RUL-86/RUL-90: one directory, one derivation — every other reader calls \
             `FileStructure::ocx_install_bin_path`, or the repair deletes one spelling and adds \
             the other on every prompt"
        );
    }
}

// ---------------------------------------------------------------------------
// What `session` itself composes
// ---------------------------------------------------------------------------

/// [`session`]'s own composition, end to end: which tier each contribution lands
/// in decides whether a globally installed tool shadows a project's trampoline.
/// Two rows, one per arm that composes anything: the `bin`-mode arm, asserted on
/// the resulting `PATH` (a vector assertion cannot tell the right answer from the
/// shadowing one), and the project-free arm, where the two session directories
/// are the whole answer. Both red under a mutation of [`session`] itself, never of
/// a caller, which would prove only the wiring.
#[cfg(test)]
mod session_composition_tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use crate::{Concurrency, PackageManager};
    use ocx_config::Config;
    use ocx_config::env::Env;
    use ocx_config::shell::ShellConfig;
    use ocx_config::shell::ShellConsent;
    use ocx_index::{ChainMode, Index, LocalConfig, LocalIndex};
    use ocx_package::metadata::env::apply::EnvEntriesExt;
    use ocx_package::metadata::env::entry::Entry;
    use ocx_package::metadata::env::modifier::ModifierKind;
    use ocx_project::DEFAULT_GROUP;
    use ocx_store::file_structure::{
        BinEntryStamp, DEFAULT_SHELL, FileStructure, RenderStamp, RenderStampScope, RenderStampTarget, ToolchainHome,
    };

    use super::{ProjectIdentity, SessionInput, session, session_desired};

    const REGISTRY: &str = "ocx.sh";
    const DIGEST: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    fn host() -> ocx_oci::Platform {
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
    /// as segments — the only operand the ordering rule permits an ordering claim about.
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
            let config = ocx_project::ProjectConfig::from_path(&config_path)
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
        /// leaves behind, which is what the stamp gate demands before the
        /// project's `bin/` may be emitted.
        fn render(&self, key: &str) {
            let path = self.home.shell_bin(DEFAULT_SHELL).join("cmake");
            let body = "#!/bin/sh\ncmake\n";
            std::fs::write(&path, body).expect("write the trampoline");
            let metadata = std::fs::symlink_metadata(&path).expect("stat the trampoline");
            let hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(body.as_bytes()));
            let stamp_entry = BinEntryStamp::from_metadata(&path, &metadata, hash);

            let identifier = ocx_oci::PackageRef::new_registry("acme/cmake", REGISTRY);
            let pinned = ocx_oci::PinnedPackageRef::try_from(
                identifier.clone_with_digest(ocx_oci::Digest::Sha256(DIGEST.to_owned())),
            )
            .expect("a digest-bearing identifier is pinned");
            let target = self.file_structure.packages.path(&pinned);
            std::fs::create_dir_all(target.join("content")).expect("materialize the package root");
            let entry = self.home.entry(DEFAULT_GROUP, "cmake").expect("a valid entry path");
            std::fs::create_dir_all(entry.parent().expect("the group directory")).expect("mkdir the group");
            // `ocx_util::fs::symlink`, not `std::os::unix::fs::symlink`: this module
            // is not `#[cfg(unix)]` — unlike `bin_mode_entry_tests`, whose
            // subject really is POSIX — so a raw POSIX call here is a Windows
            // compile error, and gating the module away to silence it would
            // delete the session-composition coverage from the platform whose
            // `<group>/<entry>` link is a junction. The renderer links these
            // entries through the same module.
            ocx_util::fs::symlink::update(&target, &entry).expect("link the entry");

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
    /// index: the gate forbids this path composing anything, so a call that
    /// reached the network fails here rather than passing slowly.
    struct Prompt {
        manager: PackageManager,
        local_index: LocalIndex,
        config: Config,
        target: ocx_oci::Platform,
    }

    impl Prompt {
        /// `consented` is written into `[shell.consent] paths`, the one clause
        /// that holds with no stamp on disk.
        fn new(file_structure: &FileStructure, consented: Option<&Path>) -> Self {
            let index = Index::from_chained(
                LocalIndex::new(LocalConfig {
                    index_store: ocx_index::IndexStore::machine_local(file_structure),
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
                    index_store: ocx_index::IndexStore::machine_local(file_structure),
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
                global_identities: None,
                manager: &self.manager,
                local_index: &self.local_index,
                concurrency: Concurrency::Unbounded,
                file_structure,
                config: &self.config,
                target: &self.target,
                project,
                composition: ocx_shell::shell::reconcile::Composition {
                    global: true,
                    project: project.map(|identity| identity.dir.clone()),
                },
                groups: Vec::new(),
                pinned: false,
            }
        }
    }

    /// **Composed by [`session`] rather than assembled by the
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
    /// the trampolines, and this is the forbidden shadowing.
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

        let path = resulting_path(&session_desired(&outcome));
        assert!(
            position(&path, &arena.home.bin()) < position(&path, Path::new(&global_tool)),
            "RUL-87: the project's trampolines must outrank the global tier's tools; got {path:#?}"
        );
    }

    /// A scope the composition omits costs nothing: an unconsented project the composition does not
    /// name is neither refused nor announced, because consent is never asked.
    ///
    /// The twin names the project and gets the refusal, so the first half is not an empty answer by accident.
    ///
    /// Red state: evaluate consent before reading the composition and the omitted project is refused too.
    #[tokio::test]
    async fn a_project_the_composition_omits_is_not_evaluated() {
        let arena = Arena::new("env").await;
        let project = ProjectIdentity::resolve(arena.dir.join("ocx.toml"))
            .await
            .expect("the fixture project resolves");
        let prompt = Prompt::new(&arena.file_structure, None);

        let mut omitted = prompt.input(&arena.file_structure, &[], Some(&project));
        omitted.composition.project = None;
        let skipped = session(omitted).await.expect("an omitted project is not an error");
        assert!(!skipped.inert && skipped.messages.is_empty() && skipped.slot.is_none());

        let refused = session(prompt.input(&arena.file_structure, &[], Some(&project)))
            .await
            .expect("a refusal is an outcome");
        assert!(refused.inert, "the twin: with the project named, consent refuses it");
    }

    /// The global tier is composed only when the composition names it.
    ///
    /// Red state: copy `input.global` regardless and an inherited global tier is composed again.
    #[tokio::test]
    async fn the_global_tier_is_composed_only_when_the_composition_names_it() {
        let arena = Arena::new("env").await;
        let prompt = Prompt::new(&arena.file_structure, None);
        let global = vec![path_entry("/g/bin")];

        let mut inherited = prompt.input(&arena.file_structure, &global, None);
        inherited.composition.global = false;
        assert!(session(inherited).await.expect("composes").global.is_empty());
        let named = prompt.input(&arena.file_structure, &global, None);
        assert_eq!(session(named).await.expect("composes").global.len(), 1);
    }

    /// A pinned shell composes its project as `env` whatever `activate` says.
    ///
    /// Red state: read the mode ladder for a pinned project and a `bin` project yields no env entries.
    #[tokio::test]
    async fn a_pinned_project_composes_as_env_whatever_its_activate_mode() {
        let arena = Arena::new("bin").await;
        let project = ProjectIdentity::resolve(arena.dir.join("ocx.toml"))
            .await
            .expect("the fixture project resolves");
        let prompt = Prompt::new(&arena.file_structure, Some(&arena.dir));

        let following = session(prompt.input(&arena.file_structure, &[], Some(&project)))
            .await
            .expect("bin mode composes");
        assert!(
            following
                .messages
                .iter()
                .any(|message| message.contains("has not been rendered")),
            "the control: a followed bin-mode project reports the missing render: {:#?}",
            following.messages
        );

        let mut pinned = prompt.input(&arena.file_structure, &[], Some(&project));
        pinned.pinned = true;
        let composed = session(pinned).await.expect("a pinned project composes as env");
        assert!(composed.slot.is_some());
        assert!(composed.session.project_bin.is_none());
        assert!(
            composed
                .messages
                .iter()
                .all(|message| !message.contains("has not been rendered")),
            "{:#?}",
            composed.messages
        );
    }

    /// **The session pair on the project-free arm.** A prompt outside every project runs
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

        let path = resulting_path(&session_desired(&outcome));
        let install = position(&path, &outcome.session.install_bin);
        let global_bin = position(&path, &outcome.session.global_bin);
        assert!(
            global_bin < install && install < position(&path, Path::new(&global_tool)),
            "C-059/C-060/RUL-87: $OCX_HOME/toolchain/bin ▸ install_bin ▸ global-composed, \
             even with no project; got {path:#?}"
        );
    }

    /// **A repository-committed symlink must not hand ocx a deletion authority
    /// over the filesystem**.
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
        let state = super::next_state(
            &ocx_config::exec_state::ExecState::default(),
            &outcome,
            &Env::clean(),
            "fp-1",
        );
        let plan = super::plan_for_state(&state, &outcome, &owned, &current);

        let removed: Vec<&str> = plan.removes.iter().map(|(_, element, _)| element.as_str()).collect();
        assert!(
            !removed.contains(&victim_bin.to_string_lossy().as_ref()),
            "C-063: the user's own PATH is not ocx's to delete; got removes {removed:#?}"
        );
    }

    /// The widening is **strictly after consent**, not merely usually
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

    /// And the refusal is *told*, on the stream the prompt actually
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

        // The session pair is unaffected: it is minted above every arm.
        let path = resulting_path(&session_desired(&outcome));
        position(&path, &outcome.session.install_bin);
        position(&path, &outcome.session.global_bin);
    }
}

#[cfg(test)]
mod current_lock_tests {
    use std::path::Path;

    use super::{SessionError, current_lock};

    /// A hash-matching lock whose entry names another repository must take the
    /// refusal path; as a library error it exits 65 into the hook's discarded stderr.
    #[test]
    fn a_repository_desync_under_a_matching_hash_is_a_lock_refusal() {
        let config = ocx_project::ProjectConfig::from_toml_str("[tools]\ncmake = \"ocx.sh/cmake:3.28\"\n")
            .expect("parse ocx.toml");
        let lock = ocx_project::ProjectLock::from_toml_str(&format!(
            "[metadata]\nlock_version = 3\ndeclaration_hash_version = 1\ndeclaration_hash = \"{hash}\"\n\
             generated_by = \"ocx test\"\ngenerated_at = \"2026-01-01T00:00:00Z\"\n\n\
             [[tool]]\nname = \"cmake\"\ngroup = \"default\"\nrepository = \"ghcr.io/cmake\"\n\n\
             [tool.platforms]\n\"linux/amd64\" = \"sha256:{leaf}\"\n",
            hash = config.declaration_hash_cached(),
            leaf = "a".repeat(64),
        ))
        .expect("parse ocx.lock");

        let result = current_lock(Some(lock), &config, Path::new("ocx.lock"));
        assert!(
            matches!(result, Err(SessionError::Lock(ocx_project::LockCurrency::Stale { .. }))),
            "{result:?}"
        );
    }
}

#[cfg(test)]
mod session_token_tests {
    use std::path::{Path, PathBuf};

    use ocx_config::exec_state::{
        AppliedValue, ExecState, IdentityRecord, Owner, Pinned, PinnedTier, ProjectScopeHint, RecordedValue, Scope,
        ScopeId, Selection, SessionCache, SessionSource, SessionToken, ValueKind,
    };

    use super::{
        Claim, ProjectIdentity, ShellProcess, TokenStanding, claim_session, follow_cwd_composition,
        parse_proc_start_time, process_start_time, token_standing,
    };

    const OWN_PID: u32 = 4242;
    const OTHER_PID: u32 = 777;

    fn token(pid: u32, started: Option<u64>) -> SessionToken {
        SessionToken { pid, started }
    }

    fn shell(started: Option<u64>) -> ShellProcess {
        ShellProcess::new(OWN_PID, started)
    }

    fn record(key: &str, owner: Owner) -> AppliedValue {
        AppliedValue {
            key: key.to_owned(),
            value: RecordedValue::Literal("v".to_owned()),
            kind: ValueKind::Constant,
            separator: None,
            pre_existing: false,
            owner,
            prior: None,
        }
    }

    fn session_owner(scope: ScopeId) -> Owner {
        Owner::Session {
            scope,
            source: SessionSource::Composed,
        }
    }

    fn pinned_to(dir: &Path) -> Selection {
        Selection::Pinned(Pinned {
            tier: PinnedTier::Project { dir: dir.to_path_buf() },
            groups: Vec::new(),
            with_names: Vec::new(),
            env_keys: Vec::new(),
            invocation_digest: "0".repeat(64),
            pinned_paths: None,
        })
    }

    /// A state holding one project and one global session record plus one session identity record.
    fn state_with_session_records(token: Option<SessionToken>, project: &Path) -> ExecState {
        let mut state = ExecState::default();
        state.applied.push(record("OCXS_G", session_owner(ScopeId::Global)));
        state.applied.push(record("OCXS_P", session_owner(ScopeId::Project)));
        state.applied.push(record("OCXS_OUTER", Owner::Inherited));
        state.replay.launch_identities.push(IdentityRecord {
            digest: format!("sha256:{}", "a".repeat(64)),
            names: vec!["ocx.sh/tool:1".to_owned()],
            owner: session_owner(ScopeId::Project),
            no_patches: false,
        });
        state.session = SessionCache {
            token,
            fp: "stale-fingerprint".to_owned(),
            ws: "stale-watch".to_owned(),
            project: Some(ProjectScopeHint {
                key: "0123456789abcdef".to_owned(),
                dir: project.to_path_buf(),
            }),
            ..SessionCache::default()
        };
        state
    }

    fn session_owned(state: &ExecState) -> usize {
        state
            .applied
            .iter()
            .map(|record| &record.owner)
            .chain(state.replay.launch_identities.iter().map(|record| &record.owner))
            .filter(|owner| matches!(owner, Owner::Session { .. }))
            .count()
    }

    // -- token_standing ---------------------------------------------------------------------

    /// An absent token is unclaimed whatever the shell can tell.
    ///
    /// Red state: treat an absent token as the shell's own and a first prompt never claims.
    #[test]
    fn an_absent_token_is_unclaimed() {
        assert_eq!(token_standing(None, Some(&shell(Some(9)))), TokenStanding::Unclaimed);
        assert_eq!(token_standing(None, None), TokenStanding::Unclaimed);
    }

    /// The same pid with the same start time is this shell: re-sourcing the rc file or
    /// `exec $SHELL` keeps it.
    ///
    /// Red state: compare the pid alone and a reused pid inherits another shell's session.
    #[test]
    fn the_same_pid_and_start_time_is_this_shell() {
        let standing = token_standing(Some(&token(OWN_PID, Some(9))), Some(&shell(Some(9))));
        assert_eq!(standing, TokenStanding::Own);
    }

    /// A tmux pane, an IDE terminal or a nested shell is another process.
    ///
    /// Red state: ignore the pid and every inherited session looks like this shell's.
    #[test]
    fn another_pid_is_foreign() {
        let standing = token_standing(Some(&token(OTHER_PID, Some(9))), Some(&shell(Some(9))));
        assert_eq!(standing, TokenStanding::Foreign);
        let pid_only = token_standing(Some(&token(OTHER_PID, None)), Some(&shell(None)));
        assert_eq!(
            pid_only,
            TokenStanding::Foreign,
            "the pid decides when no start time is readable"
        );
    }

    /// A reused pid carries a different start time.
    ///
    /// Red state: drop the start-time comparison and pid reuse passes for this shell.
    #[test]
    fn the_same_pid_with_another_start_time_is_foreign() {
        let standing = token_standing(Some(&token(OWN_PID, Some(8))), Some(&shell(Some(9))));
        assert_eq!(standing, TokenStanding::Foreign);
    }

    /// A start-less token cannot tell a reused pid from the original, so where this shell's
    /// start time is readable it is foreign.
    ///
    /// Red state: accept the start-less token on a pid match and a forged `{pid}` token claims the shell.
    #[test]
    fn a_token_without_a_start_time_is_foreign_where_this_shells_is_readable() {
        let standing = token_standing(Some(&token(OWN_PID, None)), Some(&shell(Some(9))));
        assert_eq!(standing, TokenStanding::Foreign);
    }

    /// Where no start time is readable, the pid alone decides, whatever the token holds.
    ///
    /// Red state: require a start time on both sides and a shell on a platform without a reader
    /// treats its own token as foreign every prompt.
    #[test]
    fn the_pid_alone_decides_where_this_shells_start_time_is_unreadable() {
        for recorded in [None, Some(9)] {
            let standing = token_standing(Some(&token(OWN_PID, recorded)), Some(&shell(None)));
            assert_eq!(standing, TokenStanding::Own, "recorded start time {recorded:?}");
        }
    }

    /// A token-less hook never finds a token foreign.
    ///
    /// Red state: classify against a zero pid and every claimed state looks foreign to an old hook.
    #[test]
    fn a_token_less_hook_treats_any_token_as_its_own() {
        let standing = token_standing(Some(&token(OTHER_PID, Some(1))), None);
        assert_eq!(standing, TokenStanding::Own);
    }

    // -- claim_session ----------------------------------------------------------------------

    /// A new follow-cwd session claims the token.
    ///
    /// Red state: leave the token unclaimed and the next prompt retags the shell's own records.
    #[test]
    fn a_fresh_state_is_claimed_with_this_shells_token() {
        let mut state = ExecState::default();
        let claim = claim_session(&mut state, Some(&shell(Some(9))));
        assert_eq!(claim, Claim::Claimed);
        assert_eq!(state.session.token, Some(token(OWN_PID, Some(9))));
    }

    /// This shell's own session is left exactly as it is.
    ///
    /// Red state: retag on every prompt and a shell strands its own layer.
    #[test]
    fn this_shells_own_token_keeps_its_records() {
        let mut state = state_with_session_records(Some(token(OWN_PID, Some(9))), Path::new("/work/p"));
        let before = state.clone();
        let claim = claim_session(&mut state, Some(&shell(Some(9))));
        assert_eq!(claim, Claim::Held);
        assert_eq!(state, before);
    }

    /// A foreign token: every session record and identity becomes inherited, their
    /// scopes enter `inherited_scopes`, the cache resets, the selection is `FollowCwd`, the shell claims.
    ///
    /// Red state: skip the retag and this shell reverts the other shell's layer; skip the selection
    /// reset and a tmux pane inside `ocx shell` stays pinned.
    #[test]
    fn a_foreign_token_retags_everything_resets_the_cache_and_claims() {
        let project = PathBuf::from("/work/p");
        let mut state = state_with_session_records(Some(token(OTHER_PID, Some(1))), &project);
        state.selection = pinned_to(&project);

        let claim = claim_session(&mut state, Some(&shell(Some(9))));

        assert_eq!(claim, Claim::RetaggedForeign);
        assert_eq!(session_owned(&state), 0, "no record stays session-owned");
        assert!(state.applied.iter().all(|record| record.owner == Owner::Inherited));
        assert!(
            state
                .replay
                .launch_identities
                .iter()
                .all(|record| record.owner == Owner::Inherited)
        );
        assert!(state.inherited_scopes.contains(&Scope::Global));
        assert!(
            state
                .inherited_scopes
                .contains(&Scope::Project { root: project.clone() }),
            "the pinned project enters as an inherited scope: {:?}",
            state.inherited_scopes
        );
        assert_eq!(state.selection, Selection::FollowCwd);
        assert_eq!(state.session.token, Some(token(OWN_PID, Some(9))));
        assert!(
            state.session.fp.is_empty() && state.session.ws.is_empty(),
            "the cache is reset"
        );
        assert!(state.session.project.is_none());
    }

    /// Unclaimed `FollowCwd` with session records: retag before the claim.
    ///
    /// Red state: claim without retagging and a forged state hands its Session records to this shell.
    #[test]
    fn an_unclaimed_follow_cwd_state_with_session_records_is_retagged_before_the_claim() {
        let project = PathBuf::from("/work/p");
        let mut state = state_with_session_records(None, &project);
        state.session.project = Some(ProjectScopeHint {
            key: "0123456789abcdef".to_owned(),
            dir: project.clone(),
        });

        let claim = claim_session(&mut state, Some(&shell(Some(9))));

        assert_eq!(claim, Claim::RetaggedUnclaimed);
        assert_eq!(session_owned(&state), 0);
        assert!(state.inherited_scopes.contains(&Scope::Project { root: project }));
        assert!(state.inherited_scopes.contains(&Scope::Global));
        assert_eq!(state.session.token, Some(token(OWN_PID, Some(9))));
        assert_eq!(state.selection, Selection::FollowCwd);
    }

    /// An unclaimed `Pinned` state is the entered shell: claimed as it stands.
    ///
    /// Red state: retag regardless of selection and `ocx shell` loses its own layer on the first prompt.
    #[test]
    fn an_unclaimed_pinned_state_is_claimed_without_a_retag() {
        let project = PathBuf::from("/work/p");
        let mut state = state_with_session_records(None, &project);
        state.selection = pinned_to(&project);
        let owned_before = session_owned(&state);

        let claim = claim_session(&mut state, Some(&shell(Some(9))));

        assert_eq!(claim, Claim::Claimed);
        assert_eq!(
            session_owned(&state),
            owned_before,
            "the pinned shell keeps its records"
        );
        assert!(state.inherited_scopes.is_empty());
        assert!(matches!(state.selection, Selection::Pinned(_)));
        assert_eq!(state.session.token, Some(token(OWN_PID, Some(9))));
    }

    /// A token-less hook never claims, and finds nothing to claim in a fresh state.
    ///
    /// Red state: write a zero-pid token and every later prompt of a token-bearing hook sees a foreign shell.
    #[test]
    fn a_token_less_hook_never_claims() {
        let mut state = ExecState::default();
        assert_eq!(claim_session(&mut state, None), Claim::Held);
        assert_eq!(state.session.token, None);
    }

    /// Token-less: Session records under a claimed token are its own, retagged by nothing.
    ///
    /// Red state: treat a claimed token as foreign when no pid was passed.
    #[test]
    fn a_token_less_hook_reconciles_a_claimed_sessions_records_as_its_own() {
        let mut state = state_with_session_records(Some(token(OTHER_PID, Some(1))), Path::new("/work/p"));
        let before = state.clone();
        assert_eq!(claim_session(&mut state, None), Claim::Held);
        assert_eq!(state, before);
    }

    /// A token-less hook over an unclaimed `FollowCwd` state keeps its session records.
    ///
    /// A token-less hook cannot claim, so an unclaimed state stays unclaimed forever; retagging would
    /// strand a legacy emission's own records on every prompt. A token-bearing hook still retags.
    ///
    /// Red state: retag without asking whether the hook holds a token and the records go inherited.
    #[test]
    fn a_token_less_hook_over_an_unclaimed_follow_cwd_state_keeps_its_records() {
        let mut state = state_with_session_records(None, Path::new("/work/p"));
        let owned_before = session_owned(&state);
        assert!(owned_before > 0, "the fixture must hold session records");

        assert_eq!(claim_session(&mut state, None), Claim::Held);

        assert_eq!(session_owned(&state), owned_before, "nothing is retagged");
        assert!(state.inherited_scopes.is_empty(), "no scope enters as inherited");
        assert_eq!(state.session.token, None, "token-less never claims");

        let mut claimed = state_with_session_records(None, Path::new("/work/p"));
        assert_eq!(
            claim_session(&mut claimed, Some(&shell(Some(9)))),
            Claim::RetaggedUnclaimed,
            "the twin: a token-bearing hook over the same state retags"
        );
    }

    // -- the /proc reader -------------------------------------------------------------------

    /// A `/proc/<pid>/stat` line: `fields_after_name` holds fields 3 through 52.
    fn stat_line(name: &str, start_time: u64) -> String {
        stat_line_with(name, &start_time.to_string())
    }

    fn stat_line_with(name: &str, start_field: &str) -> String {
        let mut fields: Vec<String> = (3..=52).map(|field| field.to_string()).collect();
        fields[0] = "S".to_owned();
        fields[22 - 3] = start_field.to_owned();
        format!("1234 ({name}) {}", fields.join(" "))
    }

    /// Field 22 is the start time.
    ///
    /// Red state: index from the start of the line and the pid or state is read instead.
    #[test]
    fn the_start_time_is_field_22_of_the_stat_line() {
        assert_eq!(parse_proc_start_time(&stat_line("bash", 987_654)), Some(987_654));
    }

    /// A command name holding spaces and parentheses cannot shift the fields.
    ///
    /// Red state: split the whole line on whitespace and a name like `a) S 1 (b` moves field 22.
    #[test]
    fn a_hostile_command_name_cannot_shift_the_fields() {
        let line = stat_line("a) S 1 (b c) 5 (d", 31_337);
        assert_eq!(parse_proc_start_time(&line), Some(31_337));
    }

    /// A malformed or truncated line yields no start time rather than a wrong one.
    ///
    /// Red state: default a missing field to zero and every unreadable shell claims start time 0.
    #[test]
    fn a_malformed_stat_line_has_no_start_time() {
        assert_eq!(parse_proc_start_time(""), None);
        assert_eq!(parse_proc_start_time("1234 bash S 1 2 3"), None);
        assert_eq!(parse_proc_start_time("1234 (bash) S 1 2 3"), None);
        assert_eq!(parse_proc_start_time(&stat_line_with("bash", "twenty-two")), None);
    }

    /// Linux reads this process's own start time, stably; a pid that cannot exist has none.
    ///
    /// Red state: return `None` unconditionally on Linux and the start-less rule never has a start to compare.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_reads_a_start_time_for_a_live_process_and_none_for_a_dead_pid() {
        let own = std::process::id();
        let first = process_start_time(own).await.expect("this process has a start time");
        assert_eq!(process_start_time(own).await, Some(first), "stable across reads");
        assert_eq!(process_start_time(u32::MAX).await, None);
    }

    /// Off Linux there is no reader, so the pid alone identifies the shell.
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn other_platforms_have_no_start_time() {
        assert_eq!(process_start_time(std::process::id()).await, None);
    }

    // -- the desired composition ------------------------------------------------------------

    fn identity(dir: &Path) -> ProjectIdentity {
        ProjectIdentity {
            config_path: dir.join("ocx.toml"),
            dir: dir.to_path_buf(),
            key: "0123456789abcdef".to_owned(),
        }
    }

    /// Inside an inherited project nothing is composed, the global tier included.
    ///
    /// Red state: ignore `inherited_scopes` and the hook reloads the layer an outer `ocx exec` froze.
    #[test]
    fn inside_an_inherited_project_nothing_is_desired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(dir.path()).expect("canonical");
        let mut state = ExecState::default();
        state.add_scope(Scope::Project { root: root.clone() });
        state.add_scope(Scope::Global);

        let desired = follow_cwd_composition(&state, Some(&identity(&root)));

        assert!(!desired.global && desired.project.is_none(), "{desired:?}");
    }

    /// An uninherited project Q brings the global tier (unless inherited) and Q.
    ///
    /// Red state: drop the global tier when any scope is inherited.
    #[test]
    fn an_uninherited_project_adds_global_unless_inherited_and_itself() {
        let inherited = tempfile::tempdir().expect("tempdir");
        let other = tempfile::tempdir().expect("tempdir");
        let other_root = dunce::canonicalize(other.path()).expect("canonical");
        let mut state = ExecState::default();
        state.add_scope(Scope::Project {
            root: dunce::canonicalize(inherited.path()).expect("canonical"),
        });

        let desired = follow_cwd_composition(&state, Some(&identity(&other_root)));
        assert!(desired.global);
        assert_eq!(desired.project, Some(other_root.clone()));

        state.add_scope(Scope::Global);
        let desired = follow_cwd_composition(&state, Some(&identity(&other_root)));
        assert!(!desired.global, "an inherited global tier is not composed again");
        assert_eq!(desired.project, Some(other_root));
    }

    /// Outside every project: the global tier, unless inherited.
    ///
    /// Red state: always compose global and a hook shell under `ocx exec -g x` doubles its layer.
    #[test]
    fn outside_a_project_the_global_tier_is_desired_unless_inherited() {
        let mut state = ExecState::default();
        let desired = follow_cwd_composition(&state, None);
        assert!(desired.global && desired.project.is_none());

        state.add_scope(Scope::Global);
        let desired = follow_cwd_composition(&state, None);
        assert!(!desired.global && desired.project.is_none());
    }
}

/// What the hook records about a prompt: the session records, the pinned selection and the notices.
#[cfg(test)]
mod hook_state_tests {
    use std::path::{Path, PathBuf};

    use ocx_config::env::Env;
    use ocx_config::exec_state::{
        AppliedValue, ExecState, IdentityRecord, Owner, Pinned, PinnedTier, Prior, ProjectScopeHint, RecordedValue,
        ScopeId, Selection, SessionSource, Truncation, ValueKind,
    };
    use ocx_package::launch::LaunchIdentities;
    use ocx_package::metadata::env::entry::Entry;
    use ocx_package::metadata::env::modifier::ModifierKind;

    use super::{
        Outcome, ProjectIdentity, SessionPath, already_announced, announced, keeps_pinned_layer, next_state,
        pinned_cd_notice, pinned_composition, pinned_groups, pinned_identity, plan_for_state, truncation_notice,
        vanished_pin_notice, watched_config, write_state,
    };

    const KEY: &str = "0123456789abcdef";
    const DIGEST_GLOBAL: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_PROJECT: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn constant(key: &str, value: &str) -> Entry {
        Entry {
            key: key.to_owned(),
            value: value.to_owned(),
            kind: ModifierKind::Constant,
            separator: None,
        }
    }

    fn path(value: &str) -> Entry {
        Entry {
            key: "PATH".to_owned(),
            value: value.to_owned(),
            kind: ModifierKind::Path,
            separator: None,
        }
    }

    /// Identities for `(digest, name, no_patches)` triples, built the way a replay record rebuilds them.
    fn identities(triples: &[(&str, &str, bool)]) -> LaunchIdentities {
        let records: Vec<IdentityRecord> = triples
            .iter()
            .map(|(digest, name, no_patches)| IdentityRecord {
                digest: (*digest).to_owned(),
                names: vec![(*name).to_owned()],
                owner: Owner::Inherited,
                no_patches: *no_patches,
            })
            .collect();
        LaunchIdentities::from_replay(&records)
    }

    fn identity(dir: &str) -> ProjectIdentity {
        let dir = PathBuf::from(dir);
        ProjectIdentity {
            config_path: dir.join("ocx.toml"),
            dir,
            key: KEY.to_owned(),
        }
    }

    fn outcome(global: Vec<Entry>, project: Vec<Entry>, slot: Option<ProjectIdentity>) -> Outcome {
        Outcome {
            session_enforced: true,
            session: SessionPath {
                install_bin: PathBuf::from("/ocx/install/bin"),
                global_bin: PathBuf::from("/ocx/toolchain/active/bin"),
                project_bin: None,
            },
            global,
            project,
            global_identities: None,
            project_identities: None,
            owned_home: None,
            resolved: slot.is_some(),
            inert: false,
            slot,
            messages: Vec::new(),
        }
    }

    fn owner(scope: ScopeId, source: SessionSource) -> Owner {
        Owner::Session { scope, source }
    }

    fn composed(scope: ScopeId) -> Owner {
        owner(scope, SessionSource::Composed)
    }

    fn record(key: &str, value: &str, kind: ValueKind, owner: Owner) -> AppliedValue {
        AppliedValue {
            key: key.to_owned(),
            value: RecordedValue::Literal(value.to_owned()),
            kind,
            separator: None,
            pre_existing: false,
            owner,
            prior: (kind == ValueKind::Constant).then_some(Prior::Unset),
        }
    }

    fn records<'a>(state: &'a ExecState, key: &str) -> Vec<&'a AppliedValue> {
        state.applied.iter().filter(|record| record.key == key).collect()
    }

    fn env_with(pairs: &[(&str, &str)]) -> Env {
        let mut env = Env::clean();
        for (key, value) in pairs {
            env.set(*key, *value);
        }
        env
    }

    /// A `PATH` value in the platform's separator: `:` on Unix, `;` on Windows.
    fn search_path(segments: &[&str]) -> String {
        std::env::join_paths(segments)
            .expect("fixture segments hold no separator")
            .into_string()
            .expect("fixture segments are utf-8")
    }

    fn pinned(dir: &str) -> Pinned {
        Pinned {
            tier: PinnedTier::Project {
                dir: PathBuf::from(dir),
            },
            groups: vec!["default".to_owned()],
            with_names: Vec::new(),
            env_keys: Vec::new(),
            invocation_digest: "0".repeat(64),
            pinned_paths: None,
        }
    }

    // -- next_state ---------------------------------------------------------------------------

    /// Each composed scope is recorded under its own `Session` owner, with the prior it displaced and the
    /// project hint written beside the records it describes.
    ///
    /// Red state: file the project's records under the global scope, or omit the hint.
    #[test]
    fn each_composed_scope_is_recorded_under_its_own_session_owner() {
        let outcome = outcome(
            vec![constant("OCXS_G", "g")],
            vec![path("/w/acme/bin"), constant("OCXS_P", "p")],
            Some(identity("/w/acme")),
        );
        let current = env_with(&[("PATH", "/usr/bin"), ("OCXS_P", "old")]);

        let state = next_state(&ExecState::default(), &outcome, &current, "fp-1");

        let global = records(&state, "OCXS_G");
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].owner, composed(ScopeId::Global));
        assert_eq!(global[0].prior, Some(Prior::Unset));
        let project = records(&state, "OCXS_P");
        assert_eq!(project[0].owner, composed(ScopeId::Project));
        assert_eq!(project[0].prior, Some(Prior::Value("old".to_owned())));
        let element = records(&state, "PATH");
        assert_eq!(element.len(), 1, "the session PATH directories are not recorded");
        assert_eq!(element[0].value, RecordedValue::Literal("/w/acme/bin".to_owned()));
        assert!(!element[0].pre_existing);
        assert_eq!(
            state.session.project,
            Some(ProjectScopeHint {
                key: KEY.to_owned(),
                dir: PathBuf::from("/w/acme"),
            })
        );
        assert_eq!(state.session.fp, "fp-1");
    }

    /// A constant refreshed while its live value is still ocx's keeps the prior it displaced the first time.
    ///
    /// Red state: take the prior from the live value on every refresh and `leave` restores ocx's own value.
    #[test]
    fn a_refresh_keeps_the_prior_while_the_live_value_is_still_ocxs() {
        let mut previous = ExecState::default();
        let mut held = record("OCXS_P", "p", ValueKind::Constant, composed(ScopeId::Project));
        held.prior = Some(Prior::Value("orig".to_owned()));
        previous.applied.push(held);
        let outcome = outcome(
            Vec::new(),
            vec![constant("OCXS_P", "refreshed")],
            Some(identity("/w/acme")),
        );

        let live = next_state(&previous, &outcome, &env_with(&[("OCXS_P", "p")]), "fp");
        assert_eq!(
            records(&live, "OCXS_P")[0].prior,
            Some(Prior::Value("orig".to_owned())),
            "the live value is still what ocx wrote"
        );

        let edited = next_state(&previous, &outcome, &env_with(&[("OCXS_P", "by-hand")]), "fp");
        assert_eq!(
            records(&edited, "OCXS_P")[0].prior,
            Some(Prior::Value("by-hand".to_owned())),
            "a value the user changed is the new prior"
        );
    }

    /// A record whose value is unchanged is carried over as it stands, `pre_existing` included.
    ///
    /// Red state: recompute it against the live `PATH`, which now holds the element ocx itself added.
    #[test]
    fn an_unchanged_composed_record_is_carried_over_verbatim() {
        let mut previous = ExecState::default();
        previous.applied.push(record(
            "PATH",
            "/w/acme/bin",
            ValueKind::Path,
            composed(ScopeId::Project),
        ));
        let outcome = outcome(Vec::new(), vec![path("/w/acme/bin")], Some(identity("/w/acme")));

        let state = next_state(
            &previous,
            &outcome,
            &env_with(&[("PATH", &search_path(&["/w/acme/bin", "/usr/bin"]))]),
            "fp",
        );

        assert_eq!(state.applied, previous.applied);
    }

    /// An `Invocation` record survives a refresh untouched and keeps the key to itself.
    ///
    /// Red state: record the composed value over it, or let the plan set the key.
    #[test]
    fn an_invocation_record_survives_a_refresh_and_blocks_a_composed_record_of_its_key() {
        let mut previous = ExecState::default();
        previous.applied.push(record(
            "OCXS_INV",
            "from-invocation",
            ValueKind::Constant,
            owner(ScopeId::Project, SessionSource::Invocation),
        ));
        previous.applied.push(record(
            "OCXS_COMPOSED",
            "one",
            ValueKind::Constant,
            composed(ScopeId::Project),
        ));
        let outcome = outcome(
            Vec::new(),
            vec![
                constant("OCXS_COMPOSED", "two"),
                constant("OCXS_INV", "edited-in-project"),
            ],
            Some(identity("/w/acme")),
        );
        let current = env_with(&[("OCXS_COMPOSED", "one"), ("OCXS_INV", "from-invocation")]);

        let plan = plan_for_state(&previous, &outcome, &[], &current);
        let state = next_state(&previous, &outcome, &current, "fp");

        let keys: Vec<&str> = plan.sets.iter().map(|entry| entry.key.as_str()).collect();
        assert!(
            keys.contains(&"OCXS_COMPOSED"),
            "the composed key follows the edit: {keys:?}"
        );
        assert!(
            !keys.contains(&"OCXS_INV"),
            "the invocation key is never written: {keys:?}"
        );
        let kept = records(&state, "OCXS_INV");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].owner, owner(ScopeId::Project, SessionSource::Invocation));
        assert_eq!(kept[0].value, RecordedValue::Literal("from-invocation".to_owned()));
        assert_eq!(
            records(&state, "OCXS_COMPOSED")[0].value,
            RecordedValue::Literal("two".to_owned())
        );
    }

    /// The hook writes session data only: inherited records, scopes, policy and replay pass through.
    ///
    /// Red state: rebuild `applied` from scratch and the inherited layer vanishes.
    #[test]
    fn inherited_records_and_replay_pass_through_untouched() {
        let mut previous = ExecState::default();
        previous
            .applied
            .push(record("OUTER", "o", ValueKind::Constant, Owner::Inherited));
        previous.inherited_scopes.push(ocx_config::exec_state::Scope::Global);
        previous.policy.remote = Some(false);
        previous.truncated = Truncation::InheritedDropped;
        let outcome = outcome(Vec::new(), Vec::new(), None);

        let state = next_state(&previous, &outcome, &Env::clean(), "fp");

        assert_eq!(state.applied, previous.applied);
        assert_eq!(state.inherited_scopes, previous.inherited_scopes);
        assert_eq!(state.policy, previous.policy);
        assert_eq!(state.truncated, previous.truncated);
        assert_eq!(state.replay, previous.replay);
    }

    /// A secret-named constant is recorded verbatim, and a credential key not at all.
    ///
    /// Red state: drop the literal of a token-named variable from the state, or record a credential key.
    #[test]
    fn secret_named_values_are_recorded_verbatim_and_credentials_not_at_all() {
        let outcome = outcome(
            Vec::new(),
            vec![
                constant("API_TOKEN", "fixture-secret-value"),
                constant("OCX_AUTH_USER", "fixture-user"),
            ],
            Some(identity("/w/acme")),
        );

        let state = next_state(&ExecState::default(), &outcome, &Env::clean(), "fp");

        let secret = records(&state, "API_TOKEN");
        assert_eq!(secret.len(), 1);
        assert_eq!(
            secret[0].value,
            RecordedValue::Literal("fixture-secret-value".to_owned())
        );
        assert!(records(&state, "OCX_AUTH_USER").is_empty());
    }

    /// Leaving the project drops its records and its hint, and the global scope's stay.
    ///
    /// Red state: keep the project hint after its records are gone.
    #[test]
    fn leaving_the_project_drops_its_records_and_hint_and_keeps_the_global_scope() {
        let mut previous = ExecState::default();
        previous
            .applied
            .push(record("OCXS_G", "g", ValueKind::Constant, composed(ScopeId::Global)));
        previous
            .applied
            .push(record("OCXS_P", "p", ValueKind::Constant, composed(ScopeId::Project)));
        previous.session.project = Some(ProjectScopeHint {
            key: KEY.to_owned(),
            dir: PathBuf::from("/w/acme"),
        });
        let outcome = outcome(vec![constant("OCXS_G", "g")], Vec::new(), None);

        let state = next_state(
            &previous,
            &outcome,
            &env_with(&[("OCXS_G", "g"), ("OCXS_P", "p")]),
            "fp",
        );

        assert_eq!(records(&state, "OCXS_G").len(), 1);
        assert!(records(&state, "OCXS_P").is_empty());
        assert_eq!(state.session.project, None);
    }

    // -- the pinned layer ---------------------------------------------------------------------

    fn pinned_state(dir: &str) -> ExecState {
        let mut state = ExecState {
            selection: Selection::Pinned(pinned(dir)),
            ..ExecState::default()
        };
        state
            .applied
            .push(record("OCXS_P", "p", ValueKind::Constant, composed(ScopeId::Project)));
        state
            .applied
            .push(record("PATH", "/w/p/binP", ValueKind::Path, composed(ScopeId::Project)));
        state.session.project = Some(ProjectScopeHint {
            key: KEY.to_owned(),
            dir: PathBuf::from(dir),
        });
        state
    }

    /// A pinned project that was not composed keeps its layer: no removal, no restore, no rewrite.
    ///
    /// The twin retires the same records under `FollowCwd`, so the first half is not an empty plan by accident.
    ///
    /// Red state: plan against the records as they stand and the unconsented layer is stripped.
    #[test]
    fn a_pinned_project_that_was_not_composed_keeps_its_layer() {
        let current = env_with(&[("PATH", &search_path(&["/w/p/binP", "/usr/bin"])), ("OCXS_P", "p")]);
        let mut refused = outcome(Vec::new(), Vec::new(), None);
        refused.resolved = true;
        refused.inert = true;

        let pinned_state = pinned_state("/w/p");
        let plan = plan_for_state(&pinned_state, &refused, &[], &current);
        let state = next_state(&pinned_state, &refused, &current, "fp");

        assert!(plan.removes.is_empty() && plan.restores.is_empty(), "{plan:?}");
        assert_eq!(state.applied, pinned_state.applied);
        assert_eq!(state.session.project, pinned_state.session.project);

        let mut following = pinned_state.clone();
        following.selection = Selection::FollowCwd;
        let retired = plan_for_state(&following, &refused, &[], &current);
        assert!(
            !retired.removes.is_empty() && !retired.restores.is_empty(),
            "the twin: a follow-cwd shell leaves the project and retires its layer: {retired:?}"
        );
    }

    /// A pinned project whose `ocx.toml` is gone owes one notice naming the directory; the layer stays.
    ///
    /// Red state: return `None` for a missing config and the shell loses its project silently.
    #[tokio::test]
    async fn a_pinned_project_without_an_ocx_toml_owes_the_vanished_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tier = PinnedTier::Project {
            dir: dir.path().to_path_buf(),
        };

        assert_eq!(
            vanished_pin_notice(&tier).await,
            Some(format!(
                "ocx: pinned project {} no longer has an ocx.toml; keeping its layer",
                super::printable(&dir.path().to_string_lossy())
            ))
        );

        std::fs::write(dir.path().join("ocx.toml"), "").expect("write ocx.toml");
        assert_eq!(vanished_pin_notice(&tier).await, None, "a present config owes nothing");

        let gone = PinnedTier::Project {
            dir: dir.path().join("deleted"),
        };
        assert!(
            vanished_pin_notice(&gone).await.is_some(),
            "a deleted directory owes it too"
        );
        assert_eq!(vanished_pin_notice(&PinnedTier::Global).await, None);
    }

    /// Only a pinned project tier without a composed slot keeps its layer.
    #[test]
    fn only_a_pinned_project_without_a_slot_keeps_its_layer() {
        let none = outcome(Vec::new(), Vec::new(), None);
        let some = outcome(Vec::new(), Vec::new(), Some(identity("/w/p")));
        let pinned_project = Selection::Pinned(pinned("/w/p"));
        let mut global = pinned("/w/p");
        global.tier = PinnedTier::Global;

        assert!(keeps_pinned_layer(&pinned_project, &none));
        assert!(!keeps_pinned_layer(&pinned_project, &some));
        assert!(!keeps_pinned_layer(&Selection::Pinned(global), &none));
        assert!(!keeps_pinned_layer(&Selection::FollowCwd, &none));
    }

    // -- launch identities --------------------------------------------------------------------

    fn identity_records<'a>(state: &'a ExecState, digest: &str) -> Vec<&'a IdentityRecord> {
        state
            .replay
            .launch_identities
            .iter()
            .filter(|record| record.digest == digest)
            .collect()
    }

    /// The project's record wins a digest both scopes name, and a global-only digest keeps its own.
    ///
    /// Red state: record both scopes' entries and the global names are merged into the project's package.
    #[test]
    fn the_project_identity_record_wins_a_digest_both_scopes_name() {
        let global = identities(&[
            (DIGEST_GLOBAL, "ocx.sh/jre:21", false),
            (DIGEST_PROJECT, "ocx.sh/plantuml:1", false),
        ]);
        let project = identities(&[(DIGEST_PROJECT, "ocx.sh/plantuml:1", true)]);
        let mut outcome = outcome(Vec::new(), Vec::new(), Some(identity("/w/acme")));
        outcome.global_identities = Some(global);
        outcome.project_identities = Some(project);

        let state = next_state(&ExecState::default(), &outcome, &Env::clean(), "fp");

        let shared = identity_records(&state, DIGEST_PROJECT);
        assert_eq!(shared.len(), 1, "one record for the digest both scopes name");
        assert_eq!(shared[0].owner, composed(ScopeId::Project));
        assert!(shared[0].no_patches, "the project's opt-out wins");
        let only_global = identity_records(&state, DIGEST_GLOBAL);
        assert_eq!(only_global.len(), 1);
        assert_eq!(only_global[0].owner, composed(ScopeId::Global));
    }

    /// Leaving the project retires its identity records.
    #[test]
    fn leaving_the_project_retires_its_identity_records() {
        let mut inside = outcome(Vec::new(), Vec::new(), Some(identity("/w/acme")));
        inside.project_identities = Some(identities(&[(DIGEST_PROJECT, "ocx.sh/plantuml:1", false)]));
        let state = next_state(&ExecState::default(), &inside, &Env::clean(), "fp");
        assert_eq!(identity_records(&state, DIGEST_PROJECT).len(), 1);

        let outside = outcome(Vec::new(), Vec::new(), None);
        let left = next_state(&state, &outside, &Env::clean(), "fp");
        assert!(identity_records(&left, DIGEST_PROJECT).is_empty());
    }

    // -- the pinned probe and composition -----------------------------------------------------

    /// A regular `ocx.toml` and an absent `ocx.lock` identify the project; the answer is canonical.
    #[tokio::test]
    async fn a_pinned_project_with_regular_files_is_identified_canonically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dunce::canonicalize(dir.path()).expect("canonicalize");
        std::fs::write(canonical.join("ocx.toml"), "[tools]\n").expect("write ocx.toml");

        let found = pinned_identity(&PinnedTier::Project { dir: canonical.clone() })
            .await
            .expect("a regular ocx.toml identifies the project");

        assert_eq!(found.dir, canonical);
        assert_eq!(found.config_path, canonical.join("ocx.toml"));
    }

    /// Nothing that is not a regular file is identified, and nothing is opened to find out.
    ///
    /// Red state: let `ocx.lock` through when it is a directory and consent opens it.
    #[tokio::test]
    async fn a_pinned_project_is_not_identified_unless_both_files_are_regular() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dunce::canonicalize(dir.path()).expect("canonicalize");
        let tier = |path: &Path| PinnedTier::Project {
            dir: path.to_path_buf(),
        };

        assert!(pinned_identity(&tier(&canonical)).await.is_none(), "no ocx.toml");
        assert!(pinned_identity(&PinnedTier::Global).await.is_none());
        assert!(pinned_identity(&tier(Path::new("relative/dir"))).await.is_none());

        std::fs::write(canonical.join("ocx.toml"), "[tools]\n").expect("write ocx.toml");
        assert!(pinned_identity(&tier(&canonical)).await.is_some(), "the control");

        std::fs::create_dir(canonical.join("ocx.lock")).expect("mkdir ocx.lock");
        assert!(
            pinned_identity(&tier(&canonical)).await.is_none(),
            "a lock that is not a regular file"
        );
    }

    /// A symlinked `ocx.toml` is refused, as the working-directory walk refuses it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_pinned_ocx_toml_is_not_identified() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dunce::canonicalize(dir.path()).expect("canonicalize");
        std::fs::write(canonical.join("real.toml"), "[tools]\n").expect("write");
        std::os::unix::fs::symlink(canonical.join("real.toml"), canonical.join("ocx.toml")).expect("symlink");

        let found = pinned_identity(&PinnedTier::Project { dir: canonical }).await;

        assert!(found.is_none());
    }

    /// A symlinked `ocx.lock` beside a regular `ocx.toml` is refused: consent would open its target.
    ///
    /// Red state: probe the lock with `metadata`, which follows the symlink to a regular file.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_pinned_ocx_lock_that_is_a_symlink_is_not_identified() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dunce::canonicalize(dir.path()).expect("canonicalize");
        std::fs::write(canonical.join("ocx.toml"), "[tools]\n").expect("write ocx.toml");
        std::fs::write(canonical.join("real.lock"), "").expect("write the symlink target");
        std::os::unix::fs::symlink(canonical.join("real.lock"), canonical.join("ocx.lock")).expect("symlink");

        let found = pinned_identity(&PinnedTier::Project { dir: canonical }).await;

        assert!(found.is_none(), "{found:?}");
    }

    /// A FIFO `ocx.lock` beside a regular `ocx.toml` is refused: consent opening it would block the prompt.
    ///
    /// Red state: refuse only a directory lock, and the FIFO passes the probe.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_pinned_ocx_lock_that_is_a_fifo_is_not_identified() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dunce::canonicalize(dir.path()).expect("canonicalize");
        std::fs::write(canonical.join("ocx.toml"), "[tools]\n").expect("write ocx.toml");
        ocx_test_support::fifo::mkfifo(&canonical.join("ocx.lock"));

        let found = pinned_identity(&PinnedTier::Project { dir: canonical }).await;

        assert!(found.is_none(), "{found:?}");
    }

    /// A FIFO `ocx.toml` is refused by `lstat` alone; opening it would block the prompt.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_pinned_ocx_toml_that_is_a_fifo_is_not_identified() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dunce::canonicalize(dir.path()).expect("canonicalize");
        ocx_test_support::fifo::mkfifo(&canonical.join("ocx.toml"));

        let found = pinned_identity(&PinnedTier::Project { dir: canonical }).await;

        assert!(found.is_none());
    }

    /// A relative pinned dir is refused even where it resolves, from the working directory, to a real project.
    ///
    /// Red state: drop the `is_absolute` test, and the state names a project under the hook's own cwd.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_relative_pinned_dir_is_not_identified_even_where_it_resolves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dunce::canonicalize(dir.path()).expect("canonicalize");
        std::fs::write(canonical.join("ocx.toml"), "[tools]\n").expect("write ocx.toml");
        let cwd = dunce::canonicalize(std::env::current_dir().expect("cwd")).expect("canonicalize cwd");
        let mut relative = PathBuf::new();
        for _ in cwd.components().skip(1) {
            relative.push("..");
        }
        relative.push(canonical.strip_prefix("/").expect("an absolute tempdir"));
        assert!(
            relative.is_relative() && relative.join("ocx.toml").is_file(),
            "fixture: {relative:?} must reach the project from the cwd"
        );

        let found = pinned_identity(&PinnedTier::Project { dir: relative }).await;

        assert!(found.is_none(), "{found:?}");
    }

    /// A pinned project tier composes its project alone; the global tier composes itself alone.
    #[test]
    fn a_pinned_selection_composes_its_own_tier_only() {
        let project = identity("/w/p");
        let tier = PinnedTier::Project {
            dir: PathBuf::from("/w/p"),
        };
        let composition = pinned_composition(&tier, Some(&project));
        assert!(!composition.global);
        assert_eq!(composition.project, Some(PathBuf::from("/w/p")));
        assert_eq!(pinned_composition(&tier, None), Default::default());
        let global = pinned_composition(&PinnedTier::Global, None);
        assert!(global.global && global.project.is_none());
    }

    #[test]
    fn a_pinned_selection_without_groups_composes_the_default_group() {
        let mut selection = pinned("/w/p");
        assert_eq!(pinned_groups(&selection), ["default"]);
        selection.groups = vec!["ci".to_owned(), "docs".to_owned()];
        assert_eq!(pinned_groups(&selection), ["ci", "docs"]);
        selection.groups.clear();
        assert_eq!(pinned_groups(&selection), ["default"]);
    }

    /// The groups a project composes are the requested ones; none requested is the default group.
    ///
    /// Red state: ignore the request and a pinned `ci` shell refreshes the default group.
    #[test]
    fn a_project_composes_the_requested_groups_or_the_default() {
        assert_eq!(super::composed_groups(&[]), ["default"]);
        assert_eq!(super::composed_groups(&["ci".to_owned()]), ["ci"]);
    }

    /// An inherited project's files stay out of the watch set; a composed project's go in.
    #[test]
    fn only_a_composed_projects_config_is_watched() {
        let project = identity("/w/p");
        let composed = ocx_shell::shell::reconcile::Composition {
            global: true,
            project: Some(PathBuf::from("/w/p")),
        };
        let inherited = ocx_shell::shell::reconcile::Composition::default();

        assert_eq!(
            watched_config(&composed, Some(&project)),
            Some(Path::new("/w/p/ocx.toml"))
        );
        assert_eq!(watched_config(&inherited, Some(&project)), None);
        assert_eq!(watched_config(&composed, None), None);
    }

    // -- notices ------------------------------------------------------------------------------

    /// One ASCII line starting `ocx: `, stable for a repeat so the messages digest shows it once.
    #[test]
    fn the_cd_notice_is_one_ascii_line_shown_for_a_foreign_project_only() {
        let own = identity("/w/p");
        let other = identity("/w/q");
        let selection = pinned("/w/p");

        assert_eq!(
            pinned_cd_notice(&selection, Some(&own), None),
            None,
            "outside every project"
        );
        assert_eq!(
            pinned_cd_notice(&selection, Some(&own), Some(&own)),
            None,
            "inside the pinned one"
        );
        let notice = pinned_cd_notice(&selection, Some(&own), Some(&other)).expect("a foreign project");
        assert!(
            notice.starts_with("ocx: ") && notice.is_ascii() && !notice.contains('\n'),
            "{notice}"
        );
        assert!(notice.contains("/w/p") && notice.contains("/w/q"), "{notice}");
        assert_eq!(
            pinned_cd_notice(&selection, Some(&own), Some(&other)),
            Some(notice.clone())
        );

        let mut global = pinned("/w/p");
        global.tier = PinnedTier::Global;
        let on_global =
            pinned_cd_notice(&global, None, Some(&other)).expect("any project is foreign to the global tier");
        assert!(on_global.contains("global toolchain"), "{on_global}");
    }

    /// A state-derived directory never reaches the terminal with a control or bidi character in it.
    ///
    /// Red state: format the recorded directory without `display_safe`.
    #[test]
    fn the_cd_notice_strips_controls_bidi_and_non_ascii_from_the_state() {
        let hostile = "/tmp/ocx-hostile\u{1b}[31m\u{202e}\u{61c}\u{9b}\u{7}evil-\u{e9}";
        let selection = pinned(hostile);

        let notice = pinned_cd_notice(&selection, None, Some(&identity("/w/q"))).expect("a notice");

        assert!(notice.is_ascii(), "{notice:?}");
        assert!(!notice.chars().any(char::is_control), "{notice:?}");
        assert!(
            notice.contains("evil-?"),
            "the stripped name stays recognisable: {notice:?}"
        );
    }

    #[test]
    fn a_truncation_notice_names_the_loss_in_one_ascii_line() {
        assert_eq!(truncation_notice(Truncation::None, Some(10)), None);
        for (rung, words) in [
            (
                Truncation::InheritedDropped,
                "the records inherited from outer frames were dropped",
            ),
            (Truncation::AllAppliedDropped, "every applied record was dropped"),
            (Truncation::PayloadDropped, "the whole payload was dropped"),
        ] {
            let notice = truncation_notice(rung, Some(1234)).expect("a loss is announced");
            assert!(
                notice.starts_with("ocx: ") && notice.is_ascii() && !notice.contains('\n'),
                "{notice}"
            );
            assert!(
                notice.contains(&format!(
                    "outgrew its size limit (1234 bytes) and was truncated: {words};"
                )),
                "the hook notice names the size and the rung words: {notice}"
            );
        }
        let carried = truncation_notice(Truncation::PayloadDropped, None).expect("a carried loss is announced");
        assert!(!carried.contains("bytes"), "{carried}");
    }

    /// A message already announced is not announced again; a refused prompt records it without moving a scope.
    #[test]
    fn announced_messages_are_recorded_and_recognised() {
        let previous = pinned_state("/w/p");
        let messages = vec!["ocx: one notice".to_owned()];
        assert!(!already_announced(&previous, &messages));

        let after = announced(&previous, &messages);

        assert!(already_announced(&after, &messages));
        assert!(!already_announced(&after, &["ocx: another".to_owned()]));
        assert!(
            !already_announced(&after, &[]),
            "no messages are never 'already announced'"
        );
        assert_eq!(after.applied, previous.applied);
    }

    // -- writing ------------------------------------------------------------------------------

    /// A state that fits is written whole and silently; one that does not drops records and says so once.
    ///
    /// Red state: encode and ignore the rung, and a truncated write is invisible.
    #[test]
    fn a_state_that_outgrows_the_limit_is_written_truncated_with_one_notice() {
        use sha2::Digest as _;

        let fits = write_state(&pinned_state("/w/p"));
        assert!(fits.value.is_some() && fits.notice.is_none());

        let mut crowded = ExecState::default();
        for index in 0..500u32 {
            let value = hex::encode(sha2::Sha256::digest(index.to_le_bytes()));
            crowded.applied.push(record(
                &format!("OUTER_{index}"),
                &value,
                ValueKind::Constant,
                Owner::Inherited,
            ));
        }
        let written = write_state(&crowded);
        let value = written.value.expect("the codec always yields a value");
        assert!(value.len() <= ocx_config::exec_state::MAX_ENCODED, "{}", value.len());
        let notice = written.notice.expect("a truncating write is announced");
        assert!(notice.starts_with("ocx: ") && notice.is_ascii());

        crowded.truncated = Truncation::InheritedDropped;
        assert_eq!(
            write_state(&crowded).notice,
            None,
            "a rung the state already carried was announced when it was reached"
        );
    }
}

#[cfg(test)]
mod stat_only_tests {
    use ocx_config::exec_state::{ExecState, SessionCache, Verdict};

    use super::state_is_stat_only;

    fn cached(verdict: Option<Verdict>) -> ExecState {
        ExecState {
            session: SessionCache {
                fp: "fp-1".to_owned(),
                verdict,
                ..SessionCache::default()
            },
            ..ExecState::default()
        }
    }

    /// Only a negative verdict is cached, and only for the fingerprint it was derived under.
    #[test]
    fn a_negative_verdict_answers_from_stats_while_the_fingerprint_holds() {
        for verdict in [Verdict::Inert, Verdict::NoProject] {
            assert!(
                state_is_stat_only(&cached(Some(verdict)), "fp-1"),
                "{verdict:?} short-circuits"
            );
            assert!(
                !state_is_stat_only(&cached(Some(verdict)), "fp-2"),
                "{verdict:?}: a moved fingerprint expires the cache"
            );
        }
    }

    /// A state with no cached verdict re-derives consent every prompt: the state is never a consent input.
    #[test]
    fn a_state_without_a_verdict_never_short_circuits() {
        assert!(!state_is_stat_only(&cached(None), "fp-1"));
    }
}
