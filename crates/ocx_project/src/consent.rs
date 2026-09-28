// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project activation consent: the per-project stamp and the predicate for
//! whether a project's environment may apply. Projects only: the ocx home is
//! always consented. Consent gates the parse, not just the apply: only `stat`s
//! and the `ocx.lock` parse run before it, never the untrusted `ocx.toml`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ProjectError, ProjectErrorKind};
use crate::lock::ProjectLock;
use ocx_config::shell::{ShellConsent, consent_path_matches};
use ocx_oci::PackageRef;
use ocx_shell::shell::coexistence::Observation;
use ocx_shell::shell::reconcile::ScopeId;
use ocx_store::file_structure::StateStore;
use ocx_store::reference_manager::ReferenceManager;

/// The only stamp schema version this binary writes or accepts.
const STAMP_VERSION: u8 = 1;

/// A recorded consent grant at `state/projects/<key>/consent.json`, replaced
/// whole, never edited in place. All four fields required (no
/// `#[serde(default)]`), so a truncated stamp never deserializes as valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentStamp {
    /// Schema version; an unrecognised one makes the stamp absent.
    pub v: u8,

    /// The canonical project directory: the identity, since the 8-byte key is
    /// only a lookup index.
    pub project_dir: PathBuf,

    /// The normalized source set consented to.
    pub sources: BTreeSet<String>,

    /// RFC 3339 UTC instant the stamp was written.
    pub stamped_at: String,
}

/// The activation predicate's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Activates, naming the clause: the clauses grant different channels.
    Activate(Grant),
    /// Zero env change, one hint line. A fresh clone is inert.
    Inert(Reason),
}

/// Which consent clause granted activation, and therefore how much it granted.
///
/// `stamp` and `path` authorize both the tool channel and the project's own
/// `[env]` channel; `namespace` authorizes tools only.
/// A `paths` entry `/w/acme/*` grants `/w/acme` and every project beneath it,
/// including ones that do not exist yet, but not the sibling `/w/acme-evil`;
/// no `*` spelling reaches the filesystem root.
// Clause 2 reads `verified_sources`, never the lock's claim: anyone can write
// `ocx.sh/<granted-org>/anything` (mise: GHSA-436v-8fw5-4mj8).
// Argument: adr_shell_env_overhaul.md § Decision 4 — Consent and the activation whitelist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    /// Clause 1 — a valid stamp for this directory whose source set covers the
    /// lock's. A human ran one of the stamp-writing commands here.
    Stamp,
    /// Clause 2 — every repository the store recorded for the lock's tools
    /// resolves to a source matching `[shell.consent] namespaces`, bounded by
    /// what this host actually pulled under that name rather than by what the
    /// lock claims.
    // A warm-store pull mints the origin record without fetching, so it proves
    // no publish credential; see `verified_sources`.
    Namespace,
    /// Clause 3 — the canonical directory is covered by `[shell.consent]
    /// paths`: by an entry naming it exactly, or by a trailing-`/*` entry
    /// naming it or an ancestor of it and granting that whole subtree. Opens
    /// the project `[env]` channel, also for a checkout the entry never named.
    // A `/*` entry opens `[env]` for every project beneath it, future clones
    // included; bounded by the operator's `config.toml`, never project bytes.
    Path,
}

impl Grant {
    /// Whether this grant opens the project-file `[env]` channel; `false` for
    /// `Namespace`, which still composes tools.
    #[must_use]
    pub fn authorizes_project_env(self) -> bool {
        match self {
            Grant::Stamp | Grant::Path => true,
            // `[env]` has no publisher: a relative path value fronts `PATH` with
            // `<clone>/bin`, so only a human's own gesture opens it.
            Grant::Namespace => false,
        }
    }
}

/// Why a shell is not active — the enumerated set `ocx shell state` renders.
// Each variant must be individually reachable and tested: `ocx shell state`
// exists to report them all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum Reason {
    /// No valid stamp and no matching grant — with the project's derived source
    /// set and the grants it was tested against, so the user can see what to
    /// add. Also surfaces a `paths` **near-miss**: an entry differing from the
    /// canonical directory only by ASCII case.
    NoStampNoGrant {
        /// The source set derived from `ocx.lock`.
        derived_sources: BTreeSet<String>,
        /// The `paths` entries compared against the canonical directory.
        paths_tested: Vec<PathBuf>,
        /// The `namespaces` patterns the derived sources were matched against.
        namespaces_tested: Vec<String>,
    },
    /// A stamp exists but the current lock's source set is not a subset of it —
    /// **naming the source that is new**.
    SourceSetDrift {
        /// Sources present in the lock and absent from the stamp.
        new_sources: BTreeSet<String>,
    },
    /// Every source the lock *claims* matches `[shell.consent] namespaces`, but
    /// the package store's record of where the locked digests came from does
    /// not corroborate it, so clause 2 refuses.
    ///
    /// `claimed_sources` is what `ocx.lock` says, `verified_sources` what
    /// `refs/origins/` proves. `None` means nothing could be verified (no host
    /// leaf, not materialized, or predating origin recording) and clears at the
    /// next `ocx pull` — except for a digest already materialized under another
    /// repository, which never clears. A `Some` that disagrees means a stored
    /// digest came from a repository outside the granted namespace.
    // It never clears: a store hit mints no marker for the lock's repository.
    UncorroboratedNamespace {
        /// The source set derived from `ocx.lock`'s repository fields.
        claimed_sources: BTreeSet<String>,
        /// The source set derived from the store's recorded pull origins, or
        /// `None` when no complete record exists.
        verified_sources: Option<BTreeSet<String>>,
    },
    /// The hook is disabled — naming which of the five enablement rungs decided
    /// it and the tier that set it, including the managed tier winning over a
    /// user's own file.
    // Name the tier that actually decided, never a hard-coded "managed".
    // Rendered names: the rung enum is `ocx_cli`'s `Hook`, a dependency cycle away.
    HookDisabled {
        /// The deciding rung, rendered (`--no-hook`, `OCX_NO_HOOK`, `[shell] hook`, …).
        rung: String,
        /// The config tier that set it, when rung 4 decided.
        tier: Option<String>,
    },
    /// Yielded to another live per-prompt hook — naming the **live signal
    /// observed**. One row per observed tool.
    // Name the observed signal: a user staring at an `.envrc` guesses the wrong cause.
    YieldedTo(Observation),
    /// The ledger's payload exceeded the 16 KiB cap and this scope was
    /// abandoned, its record lost rather than repaired.
    // Read from the carrier's `over_cap` marker, never inferred from an absent carrier.
    LedgerOverCap {
        /// The scope whose payload was dropped.
        scope: ScopeId,
    },
    /// The carrier is absent, truncated, or carries an unrecognised envelope
    /// tag — **distinguishing** the first prompt of a shell (nothing applied,
    /// nothing to repair) from a corrupt carrier (a scope was applied and its
    /// record is gone).
    LedgerUnreadable {
        /// `true` for the ordinary first-prompt absence.
        first_prompt: bool,
    },
    /// `ocx.lock` is absent, unreadable or unparseable.
    LockUnavailable,
}

/// The consent source of one locked coordinate: `<registry>/<first path
/// segment>`, host lowercased, port kept. Logical, never the routed address
/// (`adr_lock_records_physical_address.md`); the `[mirrors]` residual is
/// answered by `[[trust.policy]]` plus signature verification.
#[must_use]
pub fn source_of(identifier: &PackageRef) -> String {
    // Every `PackageRef` here carries an explicit registry: no default fallback.
    format!(
        "{}/{}",
        identifier.registry().to_ascii_lowercase(),
        identifier.first_path_segment()
    )
}

/// The `<registry>/<org>` set `lock`'s tools **claim**; it authorizes nothing
/// alone (see [`verified_sources`]).
#[must_use]
pub fn lock_sources(lock: &ProjectLock) -> BTreeSet<String> {
    lock.tools.iter().map(|tool| source_of(&tool.repository)).collect()
}

/// The source set the package store corroborates for `lock` on `platform`,
/// from `refs/origins/`; `None` if any tool lacks a host leaf, materialization
/// or origin, so deleting a package cannot hide the disqualifying half.
/// Residual: with layers cached, a registry pull naming any repository on the
/// registry mints its marker with no registry contact (ocx-sh/ocx#348);
/// `pull_local` and store hits mint none.
#[must_use]
pub fn verified_sources(
    lock: &ProjectLock,
    platform: &ocx_oci::Platform,
    store: &ocx_store::file_structure::PackageStore,
) -> Option<BTreeSet<String>> {
    let mut verified = BTreeSet::new();
    for tool in &lock.tools {
        let leaf = match crate::compose::host_leaf_identifier(tool, platform) {
            Ok(leaf) => leaf,
            Err(error) => {
                log::debug!(
                    "No host leaf for locked binding '{}' on {platform}; clause 2 cannot corroborate this lock: {error}",
                    tool.name
                );
                return None;
            }
        };
        let pinned = match ocx_oci::PinnedPackageRef::try_from(leaf) {
            Ok(pinned) => pinned,
            Err(error) => {
                log::debug!(
                    "Unpinned host leaf for locked binding '{}'; clause 2 cannot corroborate this lock: {error}",
                    tool.name
                );
                return None;
            }
        };
        let origins = store.package_dir(&pinned).recorded_origins();
        if origins.is_empty() {
            log::debug!(
                "No recorded pull origin for locked binding '{}'; clause 2 cannot corroborate this lock",
                tool.name
            );
            return None;
        }
        for origin in &origins {
            verified.insert(source_of_origin(origin)?);
        }
    }
    Some(verified)
}

/// The consent source of one recorded origin, through [`source_of`] so claim
/// and record normalize alike; a malformed marker yields `None`, failing closed.
fn source_of_origin(origin: &str) -> Option<String> {
    let (registry, repository) = origin.split_once('/')?;
    if registry.is_empty() || repository.is_empty() {
        return None;
    }
    Some(source_of(&PackageRef::new_registry(repository, registry)))
}

/// The one project identity: canonicalize the config file, then its parent.
/// That order, so a symlinked `ocx.toml` keys as its real directory: a granted
/// `/w/fake` linking into `/attacker` resolves to the ungranted `/attacker`.
///
/// # Errors
///
/// The canonicalization's I/O error, or `InvalidInput` for a parentless path.
pub fn canonical_project_dir(config_path: &Path) -> std::io::Result<PathBuf> {
    // `std` for the file, `dunce` for the parent: the project ledger keys on the
    // `dunce` form, which differs on Windows.
    let canonical_config = std::fs::canonicalize(config_path)?;
    let parent = canonical_config.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "canonical config path has no parent directory",
        )
    })?;
    dunce::canonicalize(parent)
}

/// Read the stamp for `key`, or `None` on any failure (logged at debug): an
/// unusable stamp is an absent one, and clauses 2 and 3 still evaluate.
#[must_use]
pub fn load(key: &str) -> Option<ConsentStamp> {
    load_from(state_store().as_ref()?, key)
}

/// What [`record`] did. Both are success; `ocx shell allow` refuses on the
/// ocx-home case, so they stay distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// A stamp was written for `project_dir`.
    Stamped,
    /// `project_dir` is the ocx home, always consented; nothing was written.
    OcxHomeNeedsNoStamp,
}

/// What [`revoke`] did; [`Revoked::Absent`] is not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revoked {
    /// A stamp existed and was removed.
    Removed,
    /// There was no stamp to remove.
    Absent,
}

/// Record consent for `project_dir` over `sources`. Only the seven
/// project-scoped commands (`add`, `remove`, `lock`, `update`, `pull`, `exec`,
/// `init`) may call it: a stamp from a shared loader would auto-grant consent.
/// `project_dir` must come from [`canonical_project_dir`], or the stamp keys as
/// another directory.
///
/// # Errors
///
/// The atomic write's I/O failure, or no resolvable OCX home.
pub fn record(project_dir: &Path, sources: &BTreeSet<String>) -> crate::Result<Recorded> {
    // The allowlist is enforced by the acceptance test. Callers resolve
    // `OCX_NO_CONSENT`: `ocx shell allow` is the human gesture it exempts.
    let store = state_store().ok_or_else(|| {
        io_error(
            project_dir,
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "could not determine the OCX home directory",
            ),
        )
    })?;
    record_in(&store, project_dir, sources)
}

/// The activation predicate: activate **iff any of** (1) a valid stamp exists and
/// the lock's claimed sources ⊆ the stamped set; (2) the **verified** source set
/// is present, **non-empty** and every source matches the namespace whitelist;
/// (3) the canonical `project_dir` is in the path whitelist. Else zero env change.
/// `lock_sources` is `None` for an unusable lock. `verified` must be
/// [`verified_sources`] over the **same** lock, or it grants on evidence this
/// lock never earned. The ocx home is absent by design: add nothing that puts the
/// global tier before this predicate (`adr_shell_env_overhaul.md` Decision 4).
#[must_use]
pub fn evaluate(
    project_dir: &Path,
    lock_sources: Option<&BTreeSet<String>>,
    verified: Option<&BTreeSet<String>>,
    whitelist: &ShellConsent,
) -> Decision {
    let key = ReferenceManager::name_for_path(project_dir);
    let stamp = load(&key);
    evaluate_with_stamp(project_dir, stamp.as_ref(), lock_sources, verified, whitelist)
}

/// [`evaluate`] over an already-read stamp; `None` covers absent and unusable alike.
#[must_use]
pub fn evaluate_with_stamp(
    project_dir: &Path,
    stamp: Option<&ConsentStamp>,
    lock_sources: Option<&BTreeSet<String>>,
    verified: Option<&BTreeSet<String>>,
    whitelist: &ShellConsent,
) -> Decision {
    // Clause 3 before the lock: a `paths` grant is the one clause that holds
    // when the lock is unreadable.
    if path_granted(project_dir, whitelist) {
        return Decision::Activate(Grant::Path);
    }

    let Some(sources) = lock_sources else {
        return Decision::Inert(Reason::LockUnavailable);
    };

    // Clause 1 before 2, since it also grants `[env]`; it checks the claim, which
    // is what the stamp recorded. The stamp's own `project_dir` is the identity:
    // one filed under this key for another directory is not consent here.
    if stamp.is_some_and(|stamp| stamp.project_dir == project_dir && sources.is_subset(&stamp.sources)) {
        return Decision::Activate(Grant::Stamp);
    }

    // Clause 2 reads only the store's record, never the claim an attacker writes.
    if verified.is_some_and(|verified| namespace_granted(verified, whitelist)) {
        return Decision::Activate(Grant::Namespace);
    }

    // The claim would have granted but the record did not corroborate it:
    // report that clause, ahead of stamp drift.
    if namespace_granted(sources, whitelist) {
        return Decision::Inert(Reason::UncorroboratedNamespace {
            claimed_sources: sources.clone(),
            verified_sources: verified.cloned(),
        });
    }

    match stamp.filter(|stamp| stamp.project_dir == project_dir) {
        Some(stamp) => Decision::Inert(Reason::SourceSetDrift {
            new_sources: sources.difference(&stamp.sources).cloned().collect(),
        }),
        None => Decision::Inert(Reason::NoStampNoGrant {
            derived_sources: sources.clone(),
            paths_tested: whitelist.paths.clone(),
            namespaces_tested: whitelist
                .namespaces
                .as_ref()
                .map(|spec| spec.include().to_vec())
                .unwrap_or_default(),
        }),
    }
}

/// Whether `[shell.consent] paths` names `project_dir` (clause 3). A
/// non-canonical dir with `..` matches nothing: `..` escapes the subtree it sits in.
fn path_granted(project_dir: &Path, whitelist: &ShellConsent) -> bool {
    whitelist
        .paths
        .iter()
        .any(|entry| consent_path_matches(entry, project_dir))
}

/// Clause 2: `sources` is non-empty and every one matches `namespaces`. The
/// caller decides whether it may grant (the record) or only word a refusal (the claim).
fn namespace_granted(sources: &BTreeSet<String>, whitelist: &ShellConsent) -> bool {
    let Some(namespaces) = whitelist.namespaces.as_ref() else {
        return false;
    };
    !sources.is_empty() && sources.iter().all(|source| namespaces.matches(source))
}

/// The state store consent stamps live under; `None` without an OCX home.
fn state_store() -> Option<StateStore> {
    let root = ocx_config::home::default_ocx_root()?;
    Some(ocx_store::file_structure::FileStructure::with_root(root).state)
}

/// [`load`] against an explicit store.
fn load_from(store: &StateStore, key: &str) -> Option<ConsentStamp> {
    load_at(&store.consent_stamp_file(key))
}

/// [`load`] against an explicit stamp file.
fn load_at(path: &Path) -> Option<ConsentStamp> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            // Absence is the common case (every unstamped prompt): debug, never WARN.
            log::debug!("No usable consent stamp at '{}': {e}", path.display());
            return None;
        }
    };
    let stamp: ConsentStamp = match serde_json::from_slice(&bytes) {
        Ok(stamp) => stamp,
        Err(e) => {
            log::debug!(
                "Consent stamp '{}' did not parse, treating as absent: {e}",
                path.display()
            );
            return None;
        }
    };
    if stamp.v != STAMP_VERSION {
        log::debug!(
            "Consent stamp '{}' is version {}, which this ocx does not recognise; treating as absent",
            path.display(),
            stamp.v
        );
        return None;
    }
    Some(stamp)
}

/// [`record`] against an explicit store.
fn record_in(store: &StateStore, project_dir: &Path, sources: &BTreeSet<String>) -> crate::Result<Recorded> {
    // `$OCX_HOME` never owns a stamp (`ocx --global lock` would write one);
    // guarded where every write routes. Device+inode identity; a probe failure proceeds.
    let ocx_home = store.root().parent().unwrap_or_else(|| store.root());
    if ocx_util::fs::same_dir(project_dir, ocx_home).unwrap_or(false) {
        return Ok(Recorded::OcxHomeNeedsNoStamp);
    }

    let key = ReferenceManager::name_for_path(project_dir);
    record_at(&store.consent_stamp_file(&key), project_dir, sources)?;
    Ok(Recorded::Stamped)
}

/// Delete the consent stamp for `project_dir`; the next prompt is inert unless
/// a `paths` or `namespaces` grant still covers it.
///
/// # Errors
///
/// The removal's I/O failure, or no resolvable OCX home.
pub fn revoke(project_dir: &Path) -> crate::Result<Revoked> {
    let store = state_store().ok_or_else(|| {
        io_error(
            project_dir,
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "could not determine the OCX home directory",
            ),
        )
    })?;
    revoke_in(&store, project_dir)
}

/// [`revoke`] against an explicit store.
fn revoke_in(store: &StateStore, project_dir: &Path) -> crate::Result<Revoked> {
    let key = ReferenceManager::name_for_path(project_dir);
    let target = store.consent_stamp_file(&key);
    match std::fs::remove_file(&target) {
        Ok(()) => Ok(Revoked::Removed),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Revoked::Absent),
        Err(e) => Err(io_error(&target, e)),
    }
}

/// [`record`] against an explicit stamp file.
fn record_at(target: &Path, project_dir: &Path, sources: &BTreeSet<String>) -> crate::Result<()> {
    let stamp = ConsentStamp {
        v: STAMP_VERSION,
        project_dir: project_dir.to_path_buf(),
        sources: sources.clone(),
        stamped_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let bytes = serde_json::to_vec_pretty(&stamp).map_err(|e| io_error(target, std::io::Error::other(e)))?;

    // `write_bytes_atomic` stages beside the target, so the directory must exist.
    let parent = target.parent().ok_or_else(|| {
        io_error(
            target,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "stamp path has no parent"),
        )
    })?;
    std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    ocx_util::fs::write_bytes_atomic(target, &bytes).map_err(|e| io_error(target, e))
}

/// Attach `path` context to an I/O failure on the stamp path.
fn io_error(path: &Path, source: std::io::Error) -> crate::Error {
    crate::Error::Project(ProjectError::new(path.to_path_buf(), ProjectErrorKind::Io(source)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_config::shell::ConsentScopeSpec;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// [`evaluate_with_stamp`] where the store's record corroborates exactly
    /// what the lock claims — the honest project, and the shape every test
    /// below is quantified over unless it is specifically about the gap between
    /// the two.
    ///
    /// The claim and the record are passed as the *same* set deliberately: a
    /// test that means "an honest project" must not be able to pass by
    /// accident because the claim alone was consulted.
    fn evaluate_corroborated(
        project_dir: &Path,
        stamp: Option<&ConsentStamp>,
        lock_sources: Option<&BTreeSet<String>>,
        whitelist: &ShellConsent,
    ) -> Decision {
        evaluate_with_stamp(project_dir, stamp, lock_sources, lock_sources, whitelist)
    }

    fn sources(entries: &[&str]) -> BTreeSet<String> {
        entries.iter().map(|entry| (*entry).to_string()).collect()
    }

    fn nothing_granted() -> ShellConsent {
        ShellConsent::default()
    }

    fn paths_grant(dir: &Path) -> ShellConsent {
        ShellConsent {
            paths: vec![dir.to_path_buf()],
            namespaces: None,
        }
    }

    /// One `config.toml` tier's `[shell.consent]`, through the **shipped**
    /// deserializer — so no fixture below can grant something the production
    /// parser would have refused.
    fn consent_tier(fragment: &str) -> ShellConsent {
        let parsed: ocx_config::shell::ShellConfig = toml::from_str(fragment).expect("grant fixture must parse");
        parsed.consent.expect("fixture declares [consent]")
    }

    fn namespaces_grant(patterns: &[&str]) -> ShellConsent {
        consent_tier(&format!(
            "[consent]\nnamespaces = {{ include = [{}] }}\n",
            patterns
                .iter()
                .map(|pattern| format!("\"{pattern}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    fn stamp(project_dir: &Path, entries: &[&str]) -> ConsentStamp {
        ConsentStamp {
            v: STAMP_VERSION,
            project_dir: project_dir.to_path_buf(),
            sources: sources(entries),
            stamped_at: "2026-08-25T00:00:00Z".to_string(),
        }
    }

    fn identifier(registry: &str, repository: &str) -> PackageRef {
        PackageRef::new_registry(repository, registry)
    }

    // ── clause-2 corroboration fixtures ──────────────────────────────────────

    /// The digest every corroboration fixture below locks. One value, so the
    /// store directory is genuinely shared across the repositories that claim
    /// it — which is the whole point of the borrow.
    const LEAF_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn leaf_digest() -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256(LEAF_HEX.to_string())
    }

    /// The host platform the corroboration fixtures resolve against.
    fn host() -> ocx_oci::Platform {
        "linux/amd64".parse().expect("valid host platform")
    }

    /// A one-tool lock claiming `repository`, shipping [`leaf_digest`] for the
    /// host platform.
    fn lock_claiming(repository: &str) -> ProjectLock {
        use std::collections::BTreeMap;

        use crate::lock::LockedTool;

        let (registry, path) = repository
            .split_once('/')
            .expect("fixture repository carries a registry");
        ProjectLock {
            metadata: crate::LockMetadata {
                lock_version: crate::LockVersion::V3,
                declaration_hash_version: crate::DECLARATION_HASH_VERSION,
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

    /// The package directory `lock_claiming`'s leaf resolves to — keyed by
    /// registry and digest only, so every fixture repository on one registry
    /// lands here.
    fn leaf_package_dir(
        store: &ocx_store::file_structure::PackageStore,
        registry: &str,
    ) -> ocx_store::file_structure::PackageDir {
        let pinned =
            ocx_oci::PinnedPackageRef::try_from(identifier(registry, "any/repo").clone_with_digest(leaf_digest()))
                .expect("a digest-bearing identifier is pinned");
        store.package_dir(&pinned)
    }

    /// Materialize the shared package directory and record `repository` as a
    /// logical repository it was fetched under — through the **production**
    /// writer, so no fixture can record a marker shape production would not.
    async fn materialize_from(store: &ocx_store::file_structure::PackageStore, repository: &str) {
        let (registry, path) = repository
            .split_once('/')
            .expect("fixture repository carries a registry");
        let pkg = leaf_package_dir(store, registry);
        std::fs::create_dir_all(pkg.content()).expect("materialize content/");
        ocx_store::file_structure::record_origin(&pkg, &identifier(registry, path))
            .await
            .expect("record the pull origin");
    }

    /// Whether `root` is absent or holds no entry at all.
    fn is_empty_tree(root: &Path) -> bool {
        match std::fs::read_dir(root) {
            Ok(mut entries) => entries.next().is_none(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => panic!("could not probe {}: {e}", root.display()),
        }
    }

    // ── C-026 — source normalization off the LOGICAL coordinate ──────────────

    /// C-026: `<registry>/<first path segment>`, host lowercased, port
    /// preserved, default registry spelled out, repository truncated to its
    /// first segment.
    ///
    /// **Fault injection 1 lands here**: build the source from a re-derived
    /// physical (mirror-routed) address and the `ghcr.io/acme` assertion below
    /// goes red.
    ///
    /// EC-CONSENT-010 — registry case, port, the never-elided default registry
    /// and a single-segment repository, in one place.
    #[test]
    fn c026_source_is_registry_and_first_path_segment_of_the_logical_coordinate() {
        assert_eq!(
            source_of(&identifier("ghcr.io", "acme/tools/cmake")),
            "ghcr.io/acme",
            "the repository truncates to its first segment"
        );
        assert_eq!(
            source_of(&identifier("ocx.sh", "cmake")),
            "ocx.sh/cmake",
            "the default registry is spelled explicitly and a single-segment repo is its own org"
        );
        assert_eq!(
            source_of(&identifier("GHCR.IO", "Acme/tool")),
            "ghcr.io/Acme",
            "the host lowercases; the repository path does not"
        );
        assert_eq!(
            source_of(&identifier("localhost:5000", "acme/tool")),
            "localhost:5000/acme",
            "the port is part of the source and is preserved"
        );
        assert_ne!(
            source_of(&identifier("localhost:5000", "acme/tool")),
            source_of(&identifier("localhost", "acme/tool")),
            "a ported registry is a distinct source from the bare host"
        );
    }

    /// C-026: `lock_sources` collapses a lock's tools to their distinct
    /// `<registry>/<org>` sources.
    ///
    /// EC-CONSENT-011 — the source set is derived from the **logical** coordinate
    /// the lock records and from no other field; that the lock records the logical
    /// one is `resolve.rs`'s `without_specifiers()`, not this function's.
    #[test]
    fn c026_lock_sources_collapses_tools_to_distinct_sources() {
        use std::collections::BTreeMap;

        use crate::lock::LockedTool;

        let lock = ProjectLock {
            metadata: crate::LockMetadata {
                lock_version: crate::LockVersion::V3,
                declaration_hash_version: crate::DECLARATION_HASH_VERSION,
                declaration_hash: String::new(),
                generated_by: String::new(),
                generated_at: String::new(),
            },
            tools: vec![
                LockedTool {
                    name: "cmake".into(),
                    group: "default".into(),
                    repository: identifier("ghcr.io", "acme/tools/cmake"),
                    platforms: BTreeMap::new(),
                },
                LockedTool {
                    name: "ninja".into(),
                    group: "default".into(),
                    repository: identifier("ghcr.io", "acme/ninja"),
                    platforms: BTreeMap::new(),
                },
                LockedTool {
                    name: "uv".into(),
                    group: "ci".into(),
                    repository: identifier("ocx.sh", "uv"),
                    platforms: BTreeMap::new(),
                },
            ],
        };

        assert_eq!(
            lock_sources(&lock),
            sources(&["ghcr.io/acme", "ocx.sh/uv"]),
            "two tools under one org contribute one source"
        );
    }

    // ── S-011 — a fresh clone is inert ───────────────────────────────────────

    /// S-011/C-025: a clone naming arbitrary registries, with no stamp and no
    /// grant, is inert and names what it tested.
    ///
    /// EC-CONSENT-001 — the fresh clone: lock present, no stamp, no grant, all
    /// three clauses fail, zero env change.
    #[test]
    fn s011_fresh_clone_with_no_stamp_and_no_grant_is_inert() {
        let project = Path::new("/w/clone");
        let derived = sources(&["ghcr.io/someone"]);

        let decision = evaluate_corroborated(project, None, Some(&derived), &nothing_granted());

        let Decision::Inert(Reason::NoStampNoGrant {
            derived_sources,
            paths_tested,
            namespaces_tested,
        }) = decision
        else {
            panic!("a fresh clone must be inert with NoStampNoGrant; got {decision:?}");
        };
        assert_eq!(derived_sources, derived, "the reason names the derived source set");
        assert!(paths_tested.is_empty(), "no paths were configured to test");
        assert!(namespaces_tested.is_empty(), "no namespaces were configured to test");
    }

    /// S-011/C-025 non-vacuity: an EMPTY source set — the `[env]`-only clone
    /// with no `ocx.lock` at all — never satisfies clause 2, for any
    /// whitelist. Deleting the `!sources.is_empty()` guard in
    /// `namespace_granted` flips this to `Activate`.
    ///
    /// EC-CONSENT-003 — the non-vacuous quantifier: a lock parsing cleanly with
    /// no tools never satisfies clause 2, so an `[env]`-only clone stays inert.
    #[test]
    fn s011_empty_source_set_never_satisfies_the_namespace_clause() {
        let project = Path::new("/w/env-only");
        let empty = BTreeSet::new();

        assert_eq!(
            evaluate_corroborated(project, None, Some(&empty), &namespaces_grant(&["ghcr.io/acme"])),
            Decision::Inert(Reason::NoStampNoGrant {
                derived_sources: BTreeSet::new(),
                paths_tested: Vec::new(),
                namespaces_tested: vec!["ghcr.io/acme".to_string()],
            }),
            "an empty source set vacuously 'matches every pattern' and must still be inert"
        );
    }

    /// S1 / CVE-2026-35533 — clause 2 authorizes the **package channel only**.
    ///
    /// A `namespaces` grant is satisfied by `ocx.lock` text the project itself
    /// authors: the named package need not exist, need not be pullable and need
    /// not be signed. That evidence can never authenticate anything, so it may
    /// not open the project-file `[env]` channel, which has no publisher at all
    /// and whose relative `type = "path"` values resolve against the project
    /// root. Clauses 1 and 3 are explicit gestures naming *this* directory and
    /// do open it.
    ///
    /// Red state: make [`Grant::authorizes_project_env`] return `true` for
    /// `Namespace` — the shipped behaviour before this fix — and the first
    /// assertion flips.
    #[test]
    fn s1_only_a_stamp_or_a_paths_grant_authorizes_the_project_env_channel() {
        assert!(
            !Grant::Namespace.authorizes_project_env(),
            "lock text the project authors cannot authorize the project's own [env]"
        );
        assert!(
            Grant::Stamp.authorizes_project_env(),
            "a stamp records an explicit command run in this directory"
        );
        assert!(
            Grant::Path.authorizes_project_env(),
            "a `paths` entry is an operator writing this directory down"
        );

        // The predicate really does report clause 2 for the attack shape: a
        // fresh clone, no stamp, no `paths` entry, one fabricated lock line
        // naming the granted namespace.
        let project = Path::new("/w/clone");
        assert_eq!(
            evaluate_corroborated(
                project,
                None,
                Some(&sources(&["ocx.sh/acme"])),
                &namespaces_grant(&["ocx.sh/acme"]),
            ),
            Decision::Activate(Grant::Namespace),
            "the attack shape must activate as clause 2 and be labelled as such - a mislabel here \
             would hand the `[env]` channel back through the gate above"
        );

        // Both clauses holding at once resolves to the stronger grant, so a
        // stamped project inside a granted namespace is not demoted.
        assert_eq!(
            evaluate_corroborated(
                project,
                Some(&stamp(project, &["ocx.sh/acme"])),
                Some(&sources(&["ocx.sh/acme"])),
                &namespaces_grant(&["ocx.sh/acme"]),
            ),
            Decision::Activate(Grant::Stamp),
            "clause 1 outranks clause 2 when both hold, because it grants more"
        );
    }

    // ── #344 — clause 2 quantifies over the RECORD, never over the claim ─────

    /// [ocx-sh/ocx#344](https://github.com/ocx-sh/ocx/issues/344): a lock that
    /// pairs a **granted org's name** with the digest of content that came from
    /// a **different repository** does not activate.
    ///
    /// This is the borrow the package store's own shape makes possible: it is
    /// addressed by `(registry, digest)` only, so composition finds the evil
    /// package by digest no matter what repository the lock names for it. The
    /// only thing that tells the two apart is `refs/origins/`, and clause 2 now
    /// quantifies over that.
    ///
    /// Red state: point clause 2 back at `sources` (the claim) — the lock's own
    /// text matches the whitelist, so it activates as `Grant::Namespace` and
    /// the attacker's `entrypoints/` reach `PATH`.
    #[tokio::test]
    async fn s344_a_lock_borrowing_a_granted_orgs_name_for_foreign_content_is_refused() {
        let home = tempfile::tempdir().expect("tempdir");
        let store = ocx_store::file_structure::PackageStore::new(home.path());
        let grant = namespaces_grant(&["ocx.sh/acme-corp"]);
        let project = Path::new("/w/clone");

        // The digest in the store genuinely came from an org nobody granted.
        materialize_from(&store, "ocx.sh/evil/x").await;

        // The attacker's lock renames it into the granted org.
        let lock = lock_claiming("ocx.sh/acme-corp/anything");
        let claimed = lock_sources(&lock);
        assert_eq!(
            claimed,
            sources(&["ocx.sh/acme-corp"]),
            "the claim must match the grant, or the refusal below proves nothing"
        );

        let verified = verified_sources(&lock, &host(), &store);
        assert_eq!(
            verified,
            Some(sources(&["ocx.sh/evil"])),
            "the store records where the bytes came from, not what the lock calls them"
        );

        assert_eq!(
            evaluate_with_stamp(project, None, Some(&claimed), verified.as_ref(), &grant),
            Decision::Inert(Reason::UncorroboratedNamespace {
                claimed_sources: sources(&["ocx.sh/acme-corp"]),
                verified_sources: Some(sources(&["ocx.sh/evil"])),
            }),
            "the refusal must name the gap between claim and record, not merely decline"
        );
    }

    /// The other half of #344: an **honest** project inside a granted namespace
    /// still activates, so the fix is a discrimination and not a blanket
    /// refusal.
    ///
    /// Without this the test above passes for a build where clause 2 never
    /// grants at all — which would silently retire the fleet auto-enabler.
    #[tokio::test]
    async fn s344_a_lock_the_store_corroborates_still_activates_as_clause_two() {
        let home = tempfile::tempdir().expect("tempdir");
        let store = ocx_store::file_structure::PackageStore::new(home.path());
        let grant = namespaces_grant(&["ocx.sh/acme-corp"]);
        let project = Path::new("/w/fleet");

        materialize_from(&store, "ocx.sh/acme-corp/cmake").await;

        let lock = lock_claiming("ocx.sh/acme-corp/cmake");
        let claimed = lock_sources(&lock);
        let verified = verified_sources(&lock, &host(), &store);

        assert_eq!(
            verified,
            Some(sources(&["ocx.sh/acme-corp"])),
            "the recorded origin truncates to the same source the whitelist matches"
        );
        assert_eq!(
            evaluate_with_stamp(project, None, Some(&claimed), verified.as_ref(), &grant),
            Decision::Activate(Grant::Namespace),
            "a corroborated lock inside the granted namespace is exactly what clause 2 is for"
        );
    }

    /// Fail closed on an absent record: a package that is materialized but
    /// carries no recorded origin corroborates nothing, so clause 2 refuses.
    ///
    /// This is also the whole backward-compatibility story — a store populated
    /// before origins were recorded looks exactly like this — and it is why the
    /// refusal's `verified_sources` is `Option`: `None` means "no record",
    /// which the renderer answers with `run ocx pull`, not with an accusation.
    ///
    /// Red state: make `verified_sources` fall back to `lock_sources` when the
    /// record is empty, or drop the `origins.is_empty()` return — either way an
    /// unrecorded package starts granting on the strength of its own lock again.
    #[tokio::test]
    async fn s344_a_materialized_package_with_no_recorded_origin_fails_closed() {
        let home = tempfile::tempdir().expect("tempdir");
        let store = ocx_store::file_structure::PackageStore::new(home.path());
        let grant = namespaces_grant(&["ocx.sh/acme-corp"]);
        let project = Path::new("/w/legacy");
        let lock = lock_claiming("ocx.sh/acme-corp/cmake");
        let claimed = lock_sources(&lock);

        // Materialized the pre-origins way: content, no `refs/origins/`.
        std::fs::create_dir_all(leaf_package_dir(&store, "ocx.sh").content()).expect("materialize content/");

        assert_eq!(
            verified_sources(&lock, &host(), &store),
            None,
            "one uncorroborated tool poisons the whole grant"
        );
        assert_eq!(
            evaluate_with_stamp(project, None, Some(&claimed), None, &grant),
            Decision::Inert(Reason::UncorroboratedNamespace {
                claimed_sources: sources(&["ocx.sh/acme-corp"]),
                verified_sources: None,
            }),
            "an absent record is a refusal that names itself as absent, never a grant"
        );

        // The positive control: recording the origin — what the next `ocx pull`
        // does — turns the same lock and the same grant into an activation.
        materialize_from(&store, "ocx.sh/acme-corp/cmake").await;
        assert_eq!(
            evaluate_with_stamp(
                project,
                None,
                Some(&claimed),
                verified_sources(&lock, &host(), &store).as_ref(),
                &grant,
            ),
            Decision::Activate(Grant::Namespace),
            "the inertness self-heals at the next pull, so this is a delay and not a regression"
        );
    }

    /// Clause 1 is untouched by an uncorroborated lock: a stamped project still
    /// activates, and with the **stronger** grant.
    ///
    /// The stamp is an explicit per-directory gesture, and its drift detection
    /// is deliberately claim-based — so tightening clause 2 must not quietly
    /// demote a stamped project to inert (which would revoke its `[env]`).
    #[tokio::test]
    async fn s344_a_stamp_still_activates_when_the_store_corroborates_nothing() {
        let home = tempfile::tempdir().expect("tempdir");
        let store = ocx_store::file_structure::PackageStore::new(home.path());
        let project = Path::new("/w/stamped");
        let lock = lock_claiming("ocx.sh/acme-corp/cmake");
        let claimed = lock_sources(&lock);

        assert_eq!(
            verified_sources(&lock, &host(), &store),
            None,
            "nothing is materialized, so the store corroborates nothing"
        );
        assert_eq!(
            evaluate_with_stamp(
                project,
                Some(&stamp(project, &["ocx.sh/acme-corp"])),
                Some(&claimed),
                None,
                &namespaces_grant(&["ocx.sh/acme-corp"]),
            ),
            Decision::Activate(Grant::Stamp),
            "clause 1 quantifies over the claim it stamped and must be unaffected"
        );
    }

    /// S-011/C-025: absent, unreadable and unparseable locks share one
    /// outcome — `LockUnavailable`, never activation.
    ///
    /// EC-CONSENT-002 — absent, unreadable and unparseable locks share this one
    /// outcome: all three reach the predicate as `None`, with no partial source
    /// set built from the tools that did parse.
    #[test]
    fn s011_unavailable_lock_is_inert_without_a_paths_grant() {
        assert_eq!(
            evaluate_corroborated(Path::new("/w/clone"), None, None, &namespaces_grant(&["ghcr.io/acme"])),
            Decision::Inert(Reason::LockUnavailable),
            "no lock means nothing for clause 2 to quantify over"
        );
    }

    // ── S-012 / C-027 / A-26 — a grant activates and writes no stamp ─────────

    /// S-012/C-027/A-26, arm 1: a `paths` grant activates unconditionally, and
    /// keeps activating after the lock gains an unconsented source. Making
    /// clause 3 conditional on clause 1 flips this to `Inert`.
    ///
    /// EC-CONSENT-013 — drift under a standing `paths` grant: clause 3 wins as
    /// written, unconditionally, and no stamp is rewritten (A-26).
    #[test]
    fn s012_paths_grant_activates_and_stays_active_through_source_set_drift() {
        let project = Path::new("/workspaces/acme-monorepo");
        let grant = paths_grant(project);

        assert_eq!(
            evaluate_corroborated(project, None, Some(&sources(&["ocx.sh/cmake"])), &grant),
            Decision::Activate(Grant::Path),
            "a paths grant activates on its own authority, with no stamp"
        );
        assert_eq!(
            evaluate_corroborated(project, None, Some(&sources(&["ocx.sh/cmake", "ghcr.io/evil"])), &grant),
            Decision::Activate(Grant::Path),
            "clause 3 is drift-blind: a new source never makes a path-granted project inert"
        );
        assert_eq!(
            evaluate_corroborated(project, None, None, &grant),
            Decision::Activate(Grant::Path),
            "clause 3 is the deliberate exception to an unavailable lock"
        );
    }

    /// S-012/C-027/A-26, arm 2 — **the load-bearing negative**: nothing on the
    /// activation path writes a stamp, so `state/projects/<key>/` stays absent
    /// and revoking the grant is immediately effective. Re-introducing an
    /// auto-stamp inside `evaluate_with_stamp` fails the emptiness assertion.
    ///
    /// The assertion is over the whole home tree rather than one derived path:
    /// "activation wrote nothing anywhere" cannot be satisfied by a stamp
    /// landing under a name this test guessed wrong.
    #[test]
    fn s012_activation_writes_no_stamp_and_revoking_the_grant_is_immediate() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = home.path().join("granted");
        std::fs::create_dir_all(&project).expect("project dir");

        // Positive control: this tree DOES gain an entry when something writes
        // a stamp into it, so the emptiness assertion below is reachable-red.
        let control = tempfile::tempdir().expect("tempdir");
        record_at(
            &control.path().join("projects").join("k").join("consent.json"),
            &project,
            &sources(&["ghcr.io/evil"]),
        )
        .expect("control write");
        assert!(
            !is_empty_tree(control.path()),
            "the control proves a write is visible here"
        );

        let granted = paths_grant(&project);
        assert_eq!(
            evaluate_corroborated(&project, None, Some(&sources(&["ghcr.io/evil"])), &granted),
            Decision::Activate(Grant::Path)
        );
        assert!(
            is_empty_tree(&home.path().join("state")),
            "a grant must never write under $OCX_HOME/state"
        );

        // Revocation: the same project, the same lock, an empty whitelist.
        assert!(
            matches!(
                evaluate_corroborated(&project, None, Some(&sources(&["ghcr.io/evil"])), &nothing_granted()),
                Decision::Inert(_)
            ),
            "with no stamp derived from it, revoking the grant is effective at the next prompt"
        );
    }

    /// S-012 edge/C-027: a `namespaces`-granted project is drift-sensitive
    /// without any stamp, because clause 2 re-quantifies every prompt.
    #[test]
    fn s012_namespace_grant_goes_inert_when_a_source_leaves_the_grant() {
        let project = Path::new("/w/fleet");
        let grant = namespaces_grant(&["ghcr.io/acme"]);

        assert_eq!(
            evaluate_corroborated(project, None, Some(&sources(&["ghcr.io/acme"])), &grant),
            Decision::Activate(Grant::Namespace)
        );
        assert!(
            matches!(
                evaluate_corroborated(project, None, Some(&sources(&["ghcr.io/acme", "ghcr.io/evil"])), &grant),
                Decision::Inert(Reason::NoStampNoGrant { .. })
            ),
            "one source outside the grant makes the whole project inert"
        );
    }

    // ── S-013 — drift re-confirms, for a stamped project only ────────────────

    /// S-013/C-025 clause 1: a same-cardinality source swap under a real stamp
    /// is inert and names the source that is new.
    ///
    /// EC-CONSENT-005 — a same-cardinality source swap re-confirms: cardinality
    /// is not the test, subset is.
    #[test]
    fn s013_source_set_drift_under_a_stamp_is_inert_and_names_the_new_source() {
        let project = Path::new("/w/stamped");
        let stamped = stamp(project, &["ghcr.io/acme"]);

        let decision = evaluate_corroborated(
            project,
            Some(&stamped),
            Some(&sources(&["ghcr.io/evil"])),
            &nothing_granted(),
        );

        assert_eq!(
            decision,
            Decision::Inert(Reason::SourceSetDrift {
                new_sources: sources(&["ghcr.io/evil"]),
            }),
            "the reason must name ghcr.io/evil, not merely report drift"
        );
    }

    /// S-013/C-025 clause 1: growth *inside* an already-stamped source does not
    /// re-confirm, and a strict subset still activates.
    ///
    /// EC-CONSENT-006 — a subset shrink activates silently and the stamp is not
    /// rewritten narrower by activation alone; the superset half is
    /// [`s013_source_set_drift_under_a_stamp_is_inert_and_names_the_new_source`].
    #[test]
    fn s013_growth_inside_stamped_sources_does_not_reconfirm() {
        let project = Path::new("/w/stamped");
        let stamped = stamp(project, &["ghcr.io/acme", "ocx.sh/cmake"]);

        assert_eq!(
            evaluate_corroborated(
                project,
                Some(&stamped),
                Some(&sources(&["ghcr.io/acme", "ocx.sh/cmake"])),
                &nothing_granted()
            ),
            Decision::Activate(Grant::Stamp)
        );
        assert_eq!(
            evaluate_corroborated(
                project,
                Some(&stamped),
                Some(&sources(&["ghcr.io/acme"])),
                &nothing_granted()
            ),
            Decision::Activate(Grant::Stamp),
            "a subset of the stamped set is still consented"
        );
    }

    /// C-025: the stamp's `project_dir` is the identity, not the key. A stamp
    /// filed under this key for another directory is not consent for this one.
    ///
    /// EC-CONSENT-007 — the 16-hex key is a lookup index and the path is the
    /// identity, so a stamp filed under this key for another directory (a copied
    /// `consent.json`, or a genuine key collision) is not consent for this one.
    #[test]
    fn c025_a_stamp_for_another_directory_is_not_consent() {
        let stamped_elsewhere = stamp(Path::new("/w/other"), &["ghcr.io/acme"]);

        assert!(
            matches!(
                evaluate_corroborated(
                    Path::new("/w/here"),
                    Some(&stamped_elsewhere),
                    Some(&sources(&["ghcr.io/acme"])),
                    &nothing_granted()
                ),
                Decision::Inert(Reason::NoStampNoGrant { .. })
            ),
            "a key collision must not carry another project's consent"
        );
    }

    /// C-025: a stamp with an EMPTY `sources` set is still consent — the
    /// emptiness that must never grant activation is the unstamped kind.
    ///
    /// EC-CONSENT-004 — the emptiness clause 1 must keep honouring: a stamp is
    /// the record of an explicit command in this directory, so the fix for
    /// EC-CONSENT-003 must not regress it.
    #[test]
    fn c025_an_empty_stamped_source_set_is_still_consent() {
        let project = Path::new("/w/env-only");
        let stamped = stamp(project, &[]);

        assert_eq!(
            evaluate_corroborated(project, Some(&stamped), Some(&BTreeSet::new()), &nothing_granted()),
            Decision::Activate(Grant::Stamp)
        );
    }

    // ── C-030/A-28 — the `paths` compare is literal ──────────────────────────

    /// C-030/A-28: separator and trailing-slash normalization only. A
    /// case-only mismatch is inert (and reported as a near-miss elsewhere),
    /// never silently granted.
    ///
    /// EC-GRANT-012 — a case-only difference between an entry and the canonical
    /// directory stays inert on every filesystem; folding it would merge `/a/B`
    /// and `/a/b` into one grant where the filesystem keeps them apart.
    #[test]
    fn c030_paths_compare_normalizes_separators_and_only_windows_folds_case() {
        let project = Path::new("/w/Repo");

        assert_eq!(
            evaluate_corroborated(
                project,
                None,
                Some(&sources(&["ocx.sh/cmake"])),
                &paths_grant(Path::new("/w/Repo/"))
            ),
            Decision::Activate(Grant::Path),
            "a trailing slash on the entry is normalized away"
        );
        let case_only = evaluate_corroborated(
            project,
            None,
            Some(&sources(&["ocx.sh/cmake"])),
            &paths_grant(Path::new("/w/repo")),
        );
        if cfg!(windows) {
            // Windows folds ASCII case in the compare because its filesystem
            // folds it too: `/w/Repo` and `/w/repo` are one directory there, so
            // refusing would leave an operator inert on the directory they named.
            assert_eq!(case_only, Decision::Activate(Grant::Path));
        } else {
            assert!(
                matches!(case_only, Decision::Inert(_)),
                "case folding would widen the grant onto a second directory an \
                 attacker can create, so a case-only mismatch stays inert"
            );
        }
    }

    /// EC-GRANT-020 — a `*`-suffixed entry grants the named directory and
    /// everything under it, bounded by path components.
    ///
    /// The sibling assertion is the whole reason the subtree form is spelled
    /// with an explicit `/*` and matched component-wise: a string prefix would
    /// let `/w/acme/*` cover an attacker-planted `/w/acme-evil`.
    ///
    /// Red state: swap `consent_path_matches`' `Path::starts_with` for a
    /// `str::starts_with` on the rendered prefix, and the sibling activates.
    #[test]
    fn c030_a_star_suffixed_entry_grants_the_subtree_but_never_a_sibling() {
        let grant = paths_grant(Path::new("/w/acme/*"));
        let locked = sources(&["ocx.sh/cmake"]);
        let verdict = |dir: &str| evaluate_corroborated(Path::new(dir), None, Some(&locked), &grant);

        assert_eq!(
            verdict("/w/acme"),
            Decision::Activate(Grant::Path),
            "the named directory is inside its own subtree"
        );
        assert_eq!(
            verdict("/w/acme/tools/inner"),
            Decision::Activate(Grant::Path),
            "any depth beneath the named directory is granted"
        );
        assert!(
            matches!(verdict("/w/acme-evil"), Decision::Inert(_)),
            "a sibling sharing a string prefix is not inside the subtree"
        );
        assert!(
            matches!(verdict("/w"), Decision::Inert(_)),
            "the parent of the granted directory is not granted"
        );
    }

    /// EC-GRANT-021 — a `*` that leaves no directory behind is not a subtree
    /// grant.
    ///
    /// git spells `safe.directory = *` "trust every repository on this
    /// machine"; OCX has no such token, for the same reason `namespaces` has no
    /// whole-registry one.
    ///
    /// Red state: drop `subtree_prefix`' "at least one `Normal` component"
    /// guard and both entries grant every directory on the machine.
    #[test]
    fn c030_a_star_naming_no_directory_grants_nothing() {
        let locked = sources(&["ocx.sh/cmake"]);

        // The positive control: the same directory, under an entry that does
        // name it. Without this the loop below is green for a build whose
        // clause 3 grants nothing at all, which is indistinguishable from one
        // that refuses these two entries specifically.
        assert_eq!(
            evaluate_corroborated(
                Path::new("/w/acme"),
                None,
                Some(&locked),
                &paths_grant(Path::new("/w/acme"))
            ),
            Decision::Activate(Grant::Path),
            "an entry naming the directory does grant it, so the refusals below are the rule and not a dead clause"
        );

        for entry in ["*", "/*"] {
            let grant = paths_grant(Path::new(entry));
            assert!(
                matches!(
                    evaluate_corroborated(Path::new("/w/acme"), None, Some(&locked), &grant),
                    Decision::Inert(_)
                ),
                "`{entry}` is a filesystem-wide grant, which is not expressible"
            );
        }
    }

    /// The `*` is a whole path component, never a suffix on one.
    ///
    /// `/w/acme*` is one legal directory name, so the entry is an *exact* grant
    /// for the directory spelled that way and a grant for nothing else. Read as
    /// a suffix it would instead name the subtree `/w`, which covers every
    /// sibling — including the attacker-planted `/w/acme-evil` the component
    /// bound exists to exclude.
    ///
    /// Red state: relax `subtree_prefix`' component equality to a suffix test
    /// (`last.as_os_str().to_string_lossy().ends_with('*')`) and all three
    /// refusals below activate as `Grant::Path`. The literal-directory control
    /// survives that mutation, which is what makes it a control.
    #[test]
    fn c030_a_star_grants_a_subtree_only_as_its_own_component() {
        let grant = paths_grant(Path::new("/w/acme*"));
        let locked = sources(&["ocx.sh/cmake"]);
        let verdict = |dir: &str| evaluate_corroborated(Path::new(dir), None, Some(&locked), &grant);

        for missed in ["/w/acme", "/w/acme-evil", "/w/other"] {
            assert!(
                matches!(verdict(missed), Decision::Inert(_)),
                "`/w/acme*` is a directory name, not a subtree over /w; '{missed}' must stay inert"
            );
        }
        assert_eq!(
            verdict("/w/acme*"),
            Decision::Activate(Grant::Path),
            "the control: a directory literally named `/w/acme*` is what the entry does grant"
        );
    }

    /// The `*` is the **last** component or it is not a wildcard at all.
    ///
    /// A `*` in the middle is not a grant over its own left-hand prefix: read
    /// that way, `/w/*/inner` would hand out all of `/w`. It is instead an
    /// exact entry for the one directory spelled with a literal `*` segment.
    ///
    /// Red state: replace `subtree_prefix`' `next_back()` equality with a scan
    /// (`components.any(|c| c == Component::Normal("*"))`, prefix taken up to
    /// the first `*`) and every refusal below activates as `Grant::Path`.
    #[test]
    fn c030_a_star_grants_a_subtree_only_as_the_last_component() {
        let grant = paths_grant(Path::new("/w/*/inner"));
        let locked = sources(&["ocx.sh/cmake"]);
        let verdict = |dir: &str| evaluate_corroborated(Path::new(dir), None, Some(&locked), &grant);

        for missed in ["/w", "/w/anything", "/w/anything/inner"] {
            assert!(
                matches!(verdict(missed), Decision::Inert(_)),
                "a mid-path `*` must not grant its left-hand prefix; '{missed}' must stay inert"
            );
        }
        assert_eq!(
            verdict("/w/*/inner"),
            Decision::Activate(Grant::Path),
            "the control: the entry is exact, and the directory it exactly names is granted"
        );
    }

    /// Two more spellings that look like a subtree grant and are not: a `..`
    /// segment before the `*`, and `**`.
    ///
    /// Both are inert rather than errors — an entry is never canonicalized and
    /// never re-parsed, so a spelling the grammar does not know simply matches
    /// no canonical directory.
    ///
    /// Red state, per row: for `/w/acme/../*`, lexically normalize the prefix
    /// in `subtree_prefix` (`/w/acme/..` → `/w`) and the `/w/…` rows activate;
    /// for `/w/acme/**`, relax the component equality to a suffix test and the
    /// `/w/acme` rows activate. The positive control fails with neither, and
    /// pins that the real subtree form still works.
    #[test]
    fn c030_near_miss_star_spellings_grant_nothing() {
        let locked = sources(&["ocx.sh/cmake"]);

        for (entry, missed) in [
            ("/w/acme/../*", "/w/acme"),
            ("/w/acme/../*", "/w"),
            ("/w/acme/../*", "/w/other"),
            ("/w/acme/**", "/w/acme"),
            ("/w/acme/**", "/w/acme/tools"),
        ] {
            let grant = paths_grant(Path::new(entry));
            assert!(
                matches!(
                    evaluate_corroborated(Path::new(missed), None, Some(&locked), &grant),
                    Decision::Inert(_)
                ),
                "'{entry}' is not the subtree spelling and must not grant '{missed}'"
            );
        }

        assert_eq!(
            evaluate_corroborated(
                Path::new("/w/acme/tools"),
                None,
                Some(&locked),
                &paths_grant(Path::new("/w/acme/*"))
            ),
            Decision::Activate(Grant::Path),
            "the control: the one spelling that IS a subtree grant still grants, so the rows above are \
             discriminating between spellings and not reporting a dead clause"
        );
    }

    /// S2 / CWE-41 — a Unix directory literally named `services\api` must not
    /// satisfy a `paths` grant for `services/api`.
    ///
    /// The attack the old unconditional `\` → `/` rewrite enabled: an operator
    /// grants a nested monorepo checkout, an attacker gets one directory added
    /// under that tree named with a literal backslash — a single legal Unix
    /// filename `git` checks out without complaint — and clause 3 fires for it,
    /// unconditionally, before the lock is even read.
    ///
    /// Windows is the mirror image and is asserted as such: there the two
    /// spellings genuinely name one directory, so the grant *must* hold.
    ///
    /// Red state: restore `normalize_consent_path`'s
    /// `to_string_lossy().replace('\\', "/")` body and the Unix arm activates.
    #[test]
    fn s2_a_literal_backslash_name_does_not_satisfy_a_separator_grant() {
        let granted = Path::new("/workspaces/mono/services/api");
        let impostor = Path::new(r"/workspaces/mono/services\api");
        let locked = sources(&["ghcr.io/evil"]);
        let grant = paths_grant(granted);

        assert_eq!(
            evaluate_corroborated(granted, None, Some(&locked), &grant),
            Decision::Activate(Grant::Path),
            "the positive control: the granted directory itself must activate, or the negative below \
             passes for a build that grants nothing at all"
        );

        let verdict = evaluate_corroborated(impostor, None, Some(&locked), &grant);
        if cfg!(windows) {
            assert_eq!(
                verdict,
                Decision::Activate(Grant::Path),
                "on Windows the backslash spelling is the same directory"
            );
        } else {
            assert!(
                matches!(verdict, Decision::Inert(_)),
                r"a directory named `services\api` is not the directory `services/api`; got {verdict:?}"
            );
        }
    }

    /// S2 — a `project_dir` that is not canonical grants **nothing**, in a
    /// release build exactly as in a debug one.
    ///
    /// `Path::starts_with` is a component compare, so `/w/acme/../../etc` sits
    /// inside `/w/acme/*` as far as the subtree arm can tell: a `..` escapes
    /// the tree the entry names while still satisfying the containment test.
    /// Round 1 held the premise with a `debug_assert!`, which is a guard the
    /// shipped binary does not carry — so the refusal is now a real one.
    ///
    /// EC-GRANT-023 — a `project_dir` carrying a `..` component grants
    /// nothing, in a release build exactly as in a debug one.
    ///
    /// Red state: delete the `Component::ParentDir` refusal at the top of
    /// [`consent_path_matches`] and the first escape activates as
    /// `Grant::Path`.
    #[test]
    fn s2_a_non_canonical_project_dir_grants_nothing() {
        let locked = sources(&["ocx.sh/cmake"]);
        let grant = paths_grant(Path::new("/w/acme/*"));
        let verdict = |dir: &str| evaluate_corroborated(Path::new(dir), None, Some(&locked), &grant);

        assert_eq!(
            verdict("/w/acme/tools"),
            Decision::Activate(Grant::Path),
            "the positive control: a canonical directory under the granted tree still activates, so the \
             refusals below are the guard and not a dead clause"
        );
        for escape in ["/w/acme/..", "/w/acme/../../etc", "/w/acme/tools/../../../etc"] {
            assert!(
                matches!(verdict(escape), Decision::Inert(_)),
                "'{escape}' satisfies the containment test only because `..` is just another component; it \
                 escapes the granted tree and must activate nothing"
            );
        }

        // The exact arm too. It only ever *missed* on a non-canonical
        // directory, so the refusal must not depend on which arm happened to
        // run — an entry spelled with the same `..` is still no grant.
        assert!(matches!(
            evaluate_corroborated(
                Path::new("/w/acme/.."),
                None,
                Some(&locked),
                &paths_grant(Path::new("/w/acme/.."))
            ),
            Decision::Inert(_)
        ));
    }

    // ── A-25 — any unusable stamp is an absent stamp ─────────────────────────

    /// A-25: an unknown `v`, a truncated file, an unknown field and an absent
    /// file all read as `None` — and clauses 2 and 3 still evaluate.
    ///
    /// The `deny_unknown_fields` + all-fields-required derives are load-bearing
    /// here: dropping them makes the unknown-`v` and truncated cases split.
    ///
    /// EC-CONSENT-008 — an unrecognised `v`, a truncated file and an unreadable
    /// one are all the absent stamp: inert, never a hard error and never a valid
    /// stamp with defaulted fields.
    #[test]
    fn a025_every_unusable_stamp_reads_as_absent() {
        let home = tempfile::tempdir().expect("tempdir");
        let target = home
            .path()
            .join("projects")
            .join("0123456789abcdef")
            .join("consent.json");
        std::fs::create_dir_all(target.parent().expect("parent")).expect("stamp dir");

        assert!(load_at(&target).is_none(), "an absent stamp is None");

        let usable = serde_json::to_vec(&stamp(Path::new("/w/p"), &["ocx.sh/cmake"])).expect("serialize");
        std::fs::write(&target, &usable).expect("write");
        assert!(
            load_at(&target).is_some(),
            "the positive control must load, or the negatives below prove nothing"
        );

        for (label, body) in [
            (
                "unknown version",
                r#"{"v":2,"project_dir":"/w/p","sources":[],"stamped_at":"2026-01-01T00:00:00Z"}"#,
            ),
            ("truncated", r#"{"v":1,"project_dir":"/w/p","#),
            (
                "missing sources",
                r#"{"v":1,"project_dir":"/w/p","stamped_at":"2026-01-01T00:00:00Z"}"#,
            ),
            (
                "unknown field",
                r#"{"v":1,"project_dir":"/w/p","sources":[],"stamped_at":"2026-01-01T00:00:00Z","extra":1}"#,
            ),
            ("not json", "}}}not json"),
        ] {
            std::fs::write(&target, body).expect("write");
            assert!(load_at(&target).is_none(), "{label} must read as an absent stamp");
        }
    }

    // ── C-024 — record round-trips through the atomic write seam ─────────────

    /// C-024: `record` writes `state/projects/<key>/consent.json` and `load`
    /// reads back exactly what was written.
    ///
    /// EC-IDENT-014 — stamp writes replace atomically and never edit in place, so
    /// a reader sees the old stamp or the new one and the parent is left with no
    /// staging litter. (A torn write needs a crash to stage and is not unit-
    /// observable; what is pinned here is replace-never-edit and atomic publish.)
    #[test]
    fn c024_record_writes_a_stamp_that_load_reads_back() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        let consented = sources(&["ghcr.io/acme", "ocx.sh/cmake"]);
        // The `projects/<key>/consent.json` shape `StateStore::consent_stamp_file`
        // owns, spelled out here so this test never depends on that accessor.
        let target = home
            .path()
            .join("state")
            .join("projects")
            .join(ReferenceManager::name_for_path(&project))
            .join("consent.json");

        record_at(&target, &project, &consented).expect("record must succeed");

        let loaded = load_at(&target).expect("the stamp just written must load");
        assert_eq!(loaded.v, STAMP_VERSION);
        assert_eq!(loaded.project_dir, project);
        assert_eq!(loaded.sources, consented);
        assert!(target.is_file(), "the parent directory is created on the way in");

        // Replace, never edit in place (C-022).
        let narrowed = sources(&["ocx.sh/cmake"]);
        record_at(&target, &project, &narrowed).expect("re-record must succeed");
        assert_eq!(
            load_at(&target).expect("re-read").sources,
            narrowed,
            "a second record replaces the stamp rather than merging into it"
        );

        // The atomic publish leaves nothing behind: a staging file surviving in
        // the parent is the tell that the write did not go through
        // `write_bytes_atomic`'s temp-then-rename, and the next reader would
        // then be racing a partial file.
        let siblings: Vec<_> = std::fs::read_dir(target.parent().expect("parent"))
            .expect("read the stamp directory")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(
            siblings,
            vec![std::ffi::OsString::from("consent.json")],
            "two records must leave exactly the stamp and no staging litter"
        );
    }

    /// A-44 — `$OCX_HOME` never owns a consent stamp, on any writer.
    ///
    /// The ocx home toolchain is always consented, so the ocx root is not a
    /// consent subject. Before the guard, `ocx --global lock` (and the other
    /// five writers under `--global`) reached `record_in` with
    /// `project_dir == $OCX_HOME` and wrote
    /// `state/projects/<key-for-$OCX_HOME>/consent.json` — reproduced against
    /// a real binary, and the direct contradiction of
    /// `adr_shell_env_overhaul.md`'s "nothing ever writes that directory",
    /// which Decision 2 uses to delete the global-tier sweep carve-out.
    ///
    /// Red state: delete the `same_dir` early return in `record_in` and the
    /// first assertion fails with the stamp present. The ordinary-project half
    /// is the discrimination — without it the test also passes for a
    /// `record_in` that writes nothing at all.
    ///
    /// The returned [`Recorded`] is asserted beside the filesystem state
    /// because it is what `ocx shell allow` refuses on: a caller that only
    /// checked for `Ok(..)` would report a stamp it never wrote.
    #[test]
    fn a44_the_ocx_home_never_owns_a_consent_stamp() {
        let home = tempfile::tempdir().expect("tempdir");
        // `StateStore`'s root is `$OCX_HOME/state`, so the ocx root is its parent
        // — the same derivation `record_in`'s guard makes.
        let store = StateStore::new(home.path().join("state"));
        let consented = sources(&["ocx.sh/cmake"]);

        assert_eq!(
            record_in(&store, home.path(), &consented).expect("recording for the ocx root must not error"),
            Recorded::OcxHomeNeedsNoStamp,
            "the writer must say it declined, or `ocx shell allow` cannot refuse"
        );
        assert!(
            !store
                .consent_stamp_file(&ReferenceManager::name_for_path(home.path()))
                .exists(),
            "the ocx home is always consented and must never be stamped"
        );

        // Discrimination: an ordinary project under the same store still stamps.
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        assert_eq!(
            record_in(&store, &project, &consented).expect("recording for a project must succeed"),
            Recorded::Stamped,
            "the guard must skip the ocx root only — an ordinary project still stamps"
        );
        assert!(
            store
                .consent_stamp_file(&ReferenceManager::name_for_path(&project))
                .is_file(),
            "the guard must skip the ocx root only — an ordinary project still stamps"
        );
    }

    /// `revoke` removes the stamp `record` wrote, and says so; a second
    /// revoke is [`Revoked::Absent`] rather than an error.
    ///
    /// The three assertions are one round trip on purpose: the removal is only
    /// meaningful if the file was there first, and the idempotence claim is
    /// only meaningful if the first call actually removed something.
    ///
    /// Red state: return `Revoked::Removed` unconditionally and the second
    /// call's assertion fails; drop the `NotFound` arm and it errors instead.
    #[test]
    fn revoke_removes_a_stamp_and_is_idempotent() {
        let home = tempfile::tempdir().expect("tempdir");
        let store = StateStore::new(home.path().join("state"));
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        let stamp = store.consent_stamp_file(&ReferenceManager::name_for_path(&project));

        assert_eq!(
            revoke_in(&store, &project).expect("revoking an unstamped project is not an error"),
            Revoked::Absent,
            "there is nothing to revoke before anything was recorded"
        );

        record_in(&store, &project, &sources(&["ocx.sh/cmake"])).expect("record");
        assert!(
            stamp.is_file(),
            "the stamp must exist, or the removal below proves nothing"
        );

        assert_eq!(revoke_in(&store, &project).expect("revoke"), Revoked::Removed);
        assert!(!stamp.exists(), "the stamp file is gone after a revoke");
        assert_eq!(
            revoke_in(&store, &project).expect("a second revoke is not an error"),
            Revoked::Absent,
            "revoking twice is idempotent, not a failure"
        );
    }

    /// C-024 + C-025 clause 1, end to end over a real stamp file: what
    /// `record` wrote is what `evaluate_with_stamp` consents to.
    #[test]
    fn c024_recorded_consent_satisfies_clause_one() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        let consented = sources(&["ghcr.io/acme"]);
        let target = home.path().join("projects").join("key").join("consent.json");
        record_at(&target, &project, &consented).expect("record");

        let loaded = load_at(&target).expect("stamp");

        assert_eq!(
            evaluate_corroborated(&project, Some(&loaded), Some(&consented), &nothing_granted()),
            Decision::Activate(Grant::Stamp)
        );
        assert!(matches!(
            evaluate_corroborated(
                &project,
                Some(&loaded),
                Some(&sources(&["ghcr.io/acme", "ghcr.io/evil"])),
                &nothing_granted()
            ),
            Decision::Inert(Reason::SourceSetDrift { .. })
        ));
    }

    // ── A-30 — canonicalize the FILE, then take its parent ───────────────────

    /// A-30: `OCX_PROJECT` naming a symlinked `ocx.toml` and the real one
    /// derive the identical project directory — and it is the symlink
    /// *target's* directory, which is the stricter outcome.
    ///
    /// EC-IDENT-002 — `OCX_PROJECT` naming a symlinked `ocx.toml`: the explicit
    /// tier must not fork the key space, and a symlink must not relocate a stamp.
    #[test]
    #[cfg(unix)]
    fn a030_symlinked_config_file_derives_the_targets_directory() {
        let home = tempfile::tempdir().expect("tempdir");
        let real = home.path().join("real");
        let fake = home.path().join("fake");
        std::fs::create_dir_all(&real).expect("real dir");
        std::fs::create_dir_all(&fake).expect("fake dir");
        std::fs::write(real.join("ocx.toml"), "").expect("real config");
        std::os::unix::fs::symlink(real.join("ocx.toml"), fake.join("ocx.toml")).expect("symlink");

        let via_real = canonical_project_dir(&real.join("ocx.toml")).expect("real derivation");
        let via_fake = canonical_project_dir(&fake.join("ocx.toml")).expect("fake derivation");

        assert_eq!(
            via_real, via_fake,
            "both spellings must reach one identity, so one stamp serves both"
        );
        assert_eq!(
            ReferenceManager::name_for_path(&via_real),
            ReferenceManager::name_for_path(&via_fake),
            "identical directories yield the identical 16-hex key"
        );
        assert_ne!(
            via_fake,
            dunce::canonicalize(&fake).expect("fake canonical"),
            "canonicalizing the DIRECTORY instead of the FILE would land in /fake — the widening direction"
        );
    }

    /// A-30: a bare relative config basename resolves against the CWD rather
    /// than canonicalizing the empty path `Path::parent` yields for it.
    #[test]
    fn a030_derivation_is_rooted_even_for_a_bare_relative_basename() {
        let home = tempfile::tempdir().expect("tempdir");
        let config = home.path().join("workspace.toml");
        std::fs::write(&config, "").expect("config");

        let derived = canonical_project_dir(&config).expect("derivation");
        assert_eq!(derived, dunce::canonicalize(home.path()).expect("home canonical"));
        assert!(derived.is_absolute(), "the derived identity is always absolute");
    }

    /// C-029 sanity: the namespace fixture used above really is the shipped
    /// `ConsentScopeSpec`, so these tests exercise the production matcher.
    #[test]
    fn namespace_fixture_uses_the_shipped_consent_scope_spec() {
        let grant = namespaces_grant(&["ghcr.io/acme"]);
        let spec: &ConsentScopeSpec = grant.namespaces.as_ref().expect("fixture declares namespaces");
        assert!(spec.matches("ghcr.io/acme"));
        assert!(!spec.matches("ghcr.io/acme-evil"));
    }

    // ── EC-GRANT — the two grants, at the decision level ─────────────────────

    /// EC-GRANT-001 — the ADR's own `<host>/<org>/*` spelling activates a
    /// normative two-segment source, and still refuses the sibling org.
    ///
    /// The trailing `/*` must be stripped at parse so matching takes
    /// `pattern_matches`' segment-bounded no-wildcard branch. Handing the
    /// unstripped pattern to `ScopeSpec::matches` takes the wildcard branch,
    /// where `"ocx.sh/acme".starts_with("ocx.sh/acme/")` is false — and a
    /// correctly-spelled fleet grant is silently inert.
    #[test]
    fn ec_grant_001_the_wildcard_spelling_activates_a_two_segment_source() {
        let project = Path::new("/w/acme");
        let grant = namespaces_grant(&["ocx.sh/acme/*"]);

        assert_eq!(
            evaluate_corroborated(project, None, Some(&sources(&["ocx.sh/acme"])), &grant),
            Decision::Activate(Grant::Namespace),
            "the trailing `/*` must strip, or a normative two-segment source never matches its own grant"
        );
        assert!(
            matches!(
                evaluate_corroborated(project, None, Some(&sources(&["ocx.sh/acme-evil"])), &grant),
                Decision::Inert(_)
            ),
            "the stripped pattern stays segment-bounded: the sibling org must not match"
        );
    }

    /// EC-GRANT-007 — `namespaces` is **one** spec, never a `Vec`: the TOML
    /// array spelling is a hard parse error, and the message names the two
    /// legal forms rather than leaving the user with `invalid type: sequence`
    /// alone. A list could only ever widen — "everything under `ocx.sh/acme/*`
    /// except the compromised org" would be unspellable.
    #[test]
    fn ec_grant_007_a_toml_array_of_patterns_is_refused_by_the_shipped_parser() {
        let error = toml::from_str::<ocx_config::shell::ShellConfig>(
            "[consent]\nnamespaces = [\"ocx.sh/a/*\", \"ocx.sh/b/*\"]\n",
        )
        .expect_err("a sequence must never deserialize into one ConsentScopeSpec");
        let message = error.to_string();
        assert!(
            message.contains("pattern string") && message.contains("include"),
            "the refusal must name the string form and the table form, got: {message}"
        );
    }

    /// EC-GRANT-009 — an `exclude` beats every `include`, contributed by any
    /// tier, and `specificity_for` is never consulted to break the tie: the
    /// third tier's include here is the **exact** source the second tier carves
    /// out, so any specificity rule would hand it the win.
    ///
    /// The register's own fixture spells the middle tier `{ exclude = [...] }`;
    /// that table is refused at parse (EC-GRANT-002), so it would contribute
    /// nothing and the case would evaporate. Written here with the legal
    /// spelling, which preserves the contract under test.
    #[test]
    fn ec_grant_009_an_exclusion_beats_every_include_regardless_of_tier() {
        let mut accumulated = consent_tier("[consent]\nnamespaces = { include = [\"ocx.sh/ok\", \"ocx.sh/evil\"] }\n");
        accumulated.merge(consent_tier(
            "[consent]\nnamespaces = { include = [\"ocx.sh/ok\"], exclude = [\"ocx.sh/evil\"] }\n",
        ));
        accumulated.merge(consent_tier(
            "[consent]\nnamespaces = { include = [\"ocx.sh/evil\", \"ocx.sh/ok\"] }\n",
        ));

        assert_eq!(
            evaluate_corroborated(Path::new("/w/ok"), None, Some(&sources(&["ocx.sh/ok"])), &accumulated),
            Decision::Activate(Grant::Namespace),
            "an unexcluded source stays granted across the accumulation"
        );
        assert!(
            matches!(
                evaluate_corroborated(
                    Path::new("/w/evil"),
                    None,
                    Some(&sources(&["ocx.sh/evil"])),
                    &accumulated
                ),
                Decision::Inert(_)
            ),
            "a carve-out is unconditional: the only thing one tier can do to another's grant is remove it"
        );
    }

    /// EC-GRANT-010 — an entry with no trailing `/*` is the byte-exact
    /// canonical directory and nothing else. A sibling, a nested child and the
    /// parent all miss. The subtree form opts into the children explicitly and
    /// is component-bounded, so the sibling never comes back either — see
    /// `c030_a_star_suffixed_entry_grants_the_subtree_but_never_a_sibling`.
    #[test]
    fn ec_grant_010_paths_matches_the_exact_directory_and_neither_kin_nor_parent() {
        let granted = Path::new("/w/acme");
        let locked = sources(&["ocx.sh/cmake"]);
        let grant = paths_grant(granted);

        assert_eq!(
            evaluate_corroborated(granted, None, Some(&locked), &grant),
            Decision::Activate(Grant::Path),
            "the byte-exact canonical directory is the one thing a paths entry matches"
        );
        for missed in ["/w/acme-evil", "/w/acme/inner"] {
            assert!(
                matches!(
                    evaluate_corroborated(Path::new(missed), None, Some(&locked), &grant),
                    Decision::Inert(_)
                ),
                "'{missed}' must not be covered by a grant naming /w/acme"
            );
        }
        assert!(
            matches!(
                evaluate_corroborated(granted, None, Some(&locked), &paths_grant(Path::new("/w"))),
                Decision::Inert(_)
            ),
            "a parent grant deliberately does not cover a child project"
        );
    }

    /// EC-GRANT-011 — entries are compared literally and are **never**
    /// canonicalized: a `..` spelling misses, and a grant naming a symlinked
    /// checkout misses the directory that checkout canonicalizes to.
    ///
    /// Canonicalizing entries at read time would let a symlink an attacker
    /// controls on the parent (`/workspaces/repo → /tmp/evil` needs only write
    /// access on `/workspaces`) redirect a grant. Inert is the deliberate
    /// fail-safe; an operator writing an exact path can write the real one.
    #[test]
    fn ec_grant_011_paths_entries_are_never_canonicalized() {
        let locked = sources(&["ocx.sh/cmake"]);

        assert!(
            matches!(
                evaluate_corroborated(
                    Path::new("/w/acme"),
                    None,
                    Some(&locked),
                    &paths_grant(Path::new("/w/acme/../acme")),
                ),
                Decision::Inert(_)
            ),
            "a `..` entry is not lexically normalized into a match"
        );

        #[cfg(unix)]
        {
            let home = tempfile::tempdir().expect("tempdir");
            let real = home.path().join("evil");
            std::fs::create_dir_all(&real).expect("target dir");
            std::fs::write(real.join("ocx.toml"), "").expect("config");
            let link = home.path().join("repo");
            std::os::unix::fs::symlink(&real, &link).expect("symlink the checkout");

            // What the walk actually hands the predicate: the canonical target.
            let canonical = canonical_project_dir(&link.join("ocx.toml")).expect("derivation");
            assert!(
                matches!(
                    evaluate_corroborated(&canonical, None, Some(&locked), &paths_grant(&link)),
                    Decision::Inert(_)
                ),
                "a grant naming the link must not follow it to {}",
                canonical.display()
            );
            assert_eq!(
                evaluate_corroborated(&canonical, None, Some(&locked), &paths_grant(&canonical)),
                Decision::Activate(Grant::Path),
                "the positive control: naming the real directory does grant it, so the miss above is the rule \
                 and not a broken fixture"
            );
        }
    }

    /// EC-GRANT-015 — one malformed token discards the **whole**
    /// `OCX_CONSENT_NAMESPACES` contribution, and the config tiers stand alone:
    /// `ghcr.io/ok` still activates, and `ocx.sh/acme` — named only by the
    /// discarded value — does not. Never partially parse; never a hard error on
    /// this channel, which would break every prompt.
    #[test]
    fn ec_grant_015_a_discarded_env_contribution_leaves_the_config_tiers_standing() {
        let mut consent = namespaces_grant(&["ghcr.io/ok"]);
        consent.merge(ocx_config::shell::env_channel(None, Some("ocx.sh/acme/*,acme-corp*")));

        assert_eq!(
            evaluate_corroborated(Path::new("/w/ok"), None, Some(&sources(&["ghcr.io/ok"])), &consent),
            Decision::Activate(Grant::Namespace),
            "the config tier's grant survives a discarded env contribution"
        );
        assert!(
            matches!(
                evaluate_corroborated(Path::new("/w/acme"), None, Some(&sources(&["ocx.sh/acme"])), &consent),
                Decision::Inert(_)
            ),
            "the valid half of a malformed value must not survive on its own"
        );
    }

    /// EC-GRANT-017 — the env channel is additive in one direction only: it
    /// folds into the accumulated `include` set, so it can widen a grant and
    /// can never lift a config tier's carve-out.
    #[test]
    fn ec_grant_017_the_env_channel_widens_but_never_lifts_a_carve_out() {
        let mut consent =
            consent_tier("[consent]\nnamespaces = { include = [\"ocx.sh/a/*\"], exclude = [\"ocx.sh/evil\"] }\n");
        consent.merge(ocx_config::shell::env_channel(None, Some("ocx.sh/evil")));

        assert!(
            matches!(
                evaluate_corroborated(Path::new("/w/evil"), None, Some(&sources(&["ocx.sh/evil"])), &consent),
                Decision::Inert(_)
            ),
            "an env token naming an excluded source must not re-admit it"
        );
        assert_eq!(
            evaluate_corroborated(Path::new("/w/a"), None, Some(&sources(&["ocx.sh/a"])), &consent),
            Decision::Activate(Grant::Namespace),
            "the positive control: the config tier's own grant still activates, so the refusal above is the \
             carve-out and not a collapsed spec"
        );
    }

    /// EC-GRANT-019 — the two grants are independent, OR'd, and neither
    /// constrains the other; an absent or non-matching grant grants **nothing**.
    ///
    /// (a) a `paths` hit with every source outside `namespaces`;
    /// (b) `namespaces` covering every source with the directory absent from
    ///     `paths`;
    /// (c) a `paths` hit with no `namespaces` key at all;
    /// (d) `[shell.consent]` present, `paths` naming another directory, no
    ///     `namespaces` — inert. Red state for (d): default `namespaces` to a
    ///     catch-all and `ghcr.io/evil` activates.
    #[test]
    fn ec_grant_019_the_grants_are_independent_and_an_absent_grant_grants_nothing() {
        let project = Path::new("/w/acme");
        let outside = sources(&["ghcr.io/evil"]);

        let mut both = paths_grant(project);
        both.merge(namespaces_grant(&["ocx.sh/tools"]));
        assert_eq!(
            evaluate_corroborated(project, None, Some(&outside), &both),
            Decision::Activate(Grant::Path),
            "(a) a paths hit is sufficient even with every source outside the namespace grant"
        );

        assert_eq!(
            evaluate_corroborated(project, None, Some(&outside), &namespaces_grant(&["ghcr.io/evil"])),
            Decision::Activate(Grant::Namespace),
            "(b) a namespace grant is sufficient with the directory absent from paths"
        );

        let paths_only = paths_grant(project);
        assert!(
            paths_only.namespaces.is_none(),
            "(c) the fixture declares no namespaces"
        );
        assert_eq!(
            evaluate_corroborated(project, None, Some(&outside), &paths_only),
            Decision::Activate(Grant::Path),
            "(c) requiring both grants would break the devcontainer case by construction"
        );

        assert!(
            matches!(
                evaluate_corroborated(project, None, Some(&outside), &paths_grant(Path::new("/w/other"))),
                Decision::Inert(_)
            ),
            "(d) an unset or non-matching grant is never read as 'everything allowed'"
        );
    }

    /// EC-CONSENT-014 — drift under a `namespaces` grant, inside versus
    /// outside it:
    ///
    /// (a) ordinary growth inside a consented namespace never re-confirms —
    ///     both clauses hold, and the answer is the **stronger** one
    ///     ([`Grant::Stamp`]), so the stamped project keeps its `[env]`
    ///     instead of being demoted to the tool-only namespace grant;
    /// (b) one source outside the grant fails clause 2, and clause 1 then
    ///     refuses with the drift reason that **names** the new source.
    #[test]
    fn ec_consent_014_drift_under_a_namespace_grant_reconfirms_only_outside_it() {
        let project = Path::new("/w/fleet");
        let grant = namespaces_grant(&["ghcr.io/acme"]);
        let stamped = stamp(project, &["ghcr.io/acme"]);

        assert_eq!(
            evaluate_corroborated(project, Some(&stamped), Some(&sources(&["ghcr.io/acme"])), &grant),
            Decision::Activate(Grant::Stamp),
            "a second tool from a consented org leaves the source set unchanged and must not re-confirm"
        );
        assert_eq!(
            evaluate_corroborated(
                project,
                Some(&stamped),
                Some(&sources(&["ghcr.io/acme", "ghcr.io/evil"])),
                &grant,
            ),
            Decision::Inert(Reason::SourceSetDrift {
                new_sources: sources(&["ghcr.io/evil"]),
            }),
            "clause 2 fails and clause 1 refuses, naming the source that is new"
        );
    }

    // ── EC-IDENT — the canonical directory is the identity ───────────────────

    /// EC-IDENT-001 — a symlinked project **directory** holding a regular
    /// `ocx.toml` reaches one identity by both routes, so one stamp serves
    /// both, and the derivation is byte-stable across repeated calls.
    ///
    /// Red state: key on the raw walked path. The `assert_ne!` below is what
    /// keeps the key assertion from passing vacuously — it proves the two raw
    /// paths really do hash apart, so their canonical forms agreeing is the
    /// canonicalization's doing and not a coincidence (the vscode#313681 fork).
    #[test]
    #[cfg(unix)]
    fn ec_ident_001_a_symlinked_project_directory_yields_one_identity_and_one_key() {
        let home = tempfile::tempdir().expect("tempdir");
        let real = home.path().join("real");
        std::fs::create_dir_all(&real).expect("real dir");
        std::fs::write(real.join("ocx.toml"), "").expect("config");
        let link = home.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink the directory");

        let via_real = canonical_project_dir(&real.join("ocx.toml")).expect("real route");
        let via_link = canonical_project_dir(&link.join("ocx.toml")).expect("link route");

        assert_eq!(via_real, via_link, "both routes must reach one identity");
        assert_eq!(
            canonical_project_dir(&link.join("ocx.toml")).expect("second link route"),
            via_link,
            "the derivation is byte-stable, so a key never rotates under an unchanged project"
        );
        assert_ne!(
            ReferenceManager::name_for_path(&link),
            ReferenceManager::name_for_path(&real),
            "the raw paths hash apart — without canonicalization the two routes are two projects"
        );
        assert_eq!(
            ReferenceManager::name_for_path(&via_link),
            ReferenceManager::name_for_path(&via_real),
            "one identity is one 16-hex key"
        );

        let stamped = stamp(&via_real, &["ghcr.io/acme"]);
        assert_eq!(
            evaluate_corroborated(
                &via_link,
                Some(&stamped),
                Some(&sources(&["ghcr.io/acme"])),
                &nothing_granted()
            ),
            Decision::Activate(Grant::Stamp),
            "a stamp written through the real route is consent when the same project is reached through the link"
        );
    }

    /// EC-IDENT-003 — the canonical path is the identity, and both accepted
    /// residuals are pinned rather than left to be rediscovered:
    ///
    /// (a) a moved directory is a **new** key carrying no stamp, so it is inert
    ///     until a write-seam command runs there;
    /// (b) a path reused by an unrelated repository activates **iff** the new
    ///     sources are a subset of the stamped ones — the superseded ADR's
    ///     tool-name-set guard is deliberately gone (ceremony fatigue).
    #[test]
    fn ec_ident_003_a_moved_directory_is_a_new_key_and_a_reused_path_keeps_its_stamp() {
        let old = Path::new("/w/old");
        let new = Path::new("/w/new");
        assert_ne!(
            ReferenceManager::name_for_path(old),
            ReferenceManager::name_for_path(new),
            "the key is derived from the path, so a move lands on a different key"
        );
        assert!(
            matches!(
                evaluate_corroborated(
                    new,
                    Some(&stamp(old, &["ghcr.io/acme"])),
                    Some(&sources(&["ghcr.io/acme"])),
                    &nothing_granted(),
                ),
                Decision::Inert(Reason::NoStampNoGrant { .. })
            ),
            "even reached under the old key, a stamp for the old path is not consent for the new one"
        );

        let reused = Path::new("/w/p");
        let stamped = stamp(reused, &["ghcr.io/acme"]);
        assert_eq!(
            evaluate_corroborated(
                reused,
                Some(&stamped),
                Some(&sources(&["ghcr.io/acme"])),
                &nothing_granted()
            ),
            Decision::Activate(Grant::Stamp),
            "accepted residual: an unrelated repository at the same path activates on a subset of the stamped sources"
        );
        assert!(
            matches!(
                evaluate_corroborated(
                    reused,
                    Some(&stamped),
                    Some(&sources(&["ghcr.io/other"])),
                    &nothing_granted()
                ),
                Decision::Inert(Reason::SourceSetDrift { .. })
            ),
            "a different namespace is refused by the source-set predicate"
        );
    }

    /// EC-IDENT-004 — a project nested inside another is a distinct identity:
    /// the outer's stamp is keyed to a different canonical directory and is not
    /// consent for the inner, and the outer's `paths` grant does not reach it
    /// either. The scope *switch* — `ConfigLoader::project_path` returning the
    /// **nearest** `ocx.toml` and stopping — is the loader's contract, not this
    /// predicate's.
    #[test]
    fn ec_ident_004_an_inner_project_inherits_neither_stamp_nor_grant() {
        let outer = Path::new("/w/outer");
        let inner = Path::new("/w/outer/inner");
        let locked = sources(&["ghcr.io/acme"]);

        assert_ne!(
            ReferenceManager::name_for_path(outer),
            ReferenceManager::name_for_path(inner),
            "nesting is a scope switch, never a stack: the two directories are two keys"
        );
        assert!(
            matches!(
                evaluate_corroborated(
                    inner,
                    Some(&stamp(outer, &["ghcr.io/acme"])),
                    Some(&locked),
                    &nothing_granted()
                ),
                Decision::Inert(Reason::NoStampNoGrant { .. })
            ),
            "the inner project is unstamped; a parent's consent never nests"
        );
        assert!(
            matches!(
                evaluate_corroborated(inner, None, Some(&locked), &paths_grant(outer)),
                Decision::Inert(_)
            ),
            "nor does the parent's paths grant cover it"
        );
    }
}
