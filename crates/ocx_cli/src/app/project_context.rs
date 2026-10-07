// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared project-tier resolution prologue for `pull.rs` and `toolchain_exec.rs`.
//!
//! The loaders never register the project in `ProjectRegistry`, or every `ocx exec`/`ocx pull`
//! pays a flock, JSON read and rename; registration lives at the lock-write sites that already
//! hold the flock, `ProjectLock::save` and `MutationGuard::commit` (superseded design:
//! `adr_clean_project_backlinks.md`).

use std::path::{Path, PathBuf};

use ocx_project::{
    MutationGuard, Origin, ProjectConfig, ProjectLock, SelectedTool, acquire_project_lock_for_file, lock::lock_path_for,
};

/// Result of resolving the project tier: owned paths, parsed config, parsed lock.
pub struct ProjectContext {
    /// Absolute path to the `ocx.toml` file that was loaded.
    pub config_path: PathBuf,
    /// Absolute path to the sibling `ocx.lock` file that was loaded.
    pub lock_path: PathBuf,
    /// Parsed project configuration from `ocx.toml`.
    pub config: ProjectConfig,
    /// Parsed project lock from `ocx.lock`.
    pub lock: ProjectLock,
}

/// Failure modes of the project-tier loaders.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[non_exhaustive]
pub enum ProjectContextError {
    /// No `ocx.toml` in `cwd` or any parent, and no explicit selection in effect.
    #[error("no ocx.toml found in {cwd} or any parent; run `ocx init` to create one")]
    #[exit(
        UsageError,
        slug = "no_project",
        summary = "No ocx.toml exists in the working directory or any parent"
    )]
    NoProject { cwd: PathBuf },

    /// `--project` / `OCX_PROJECT` named `dir`, and it holds no `ocx.toml`.
    /// Not `NoProject`, whose wording names the CWD and claims a parent walk that never happened.
    #[error("no ocx.toml in {dir} (check --project or OCX_PROJECT); run `ocx init` in that directory to create one")]
    #[exit(
        UsageError,
        slug = "no_project_in",
        summary = "The explicitly selected project directory holds no ocx.toml"
    )]
    NoProjectIn { dir: PathBuf },

    /// `ocx.lock` is absent or no longer describes `ocx.toml`.
    /// Wraps [`ocx_project::LockCurrency`] rather than restating it, so the wording matches
    /// `ocx_package_manager::activation::SessionError`'s for the same two states.
    #[error("{0}")]
    #[exit(delegate)]
    Lock(#[from] ocx_project::LockCurrency),

    /// A project-tier library error from `ocx_project`.
    // `{0}`, not `transparent`: transparent's `source()` skips the inner error, which then goes unclassified.
    #[error("{0}")]
    #[exit(
        chain,
        fallback(
            Failure,
            slug = "project_load_failed",
            summary = "Loading the project failed with an unclassified cause"
        )
    )]
    Project(#[from] ocx_project::Error),

    /// A config-tier error from the loader, e.g. an explicit `--project` path that is absent or unreadable.
    // `{0}`, not `transparent`, for the same reason as `Project`.
    #[error("{0}")]
    #[exit(
        chain,
        fallback(
            Failure,
            slug = "project_load_failed",
            summary = "Loading the project failed with an unclassified cause"
        )
    )]
    Config(#[from] ocx_config::error::Error),
}

/// Creates `$OCX_HOME/ocx.toml` under `--global` when it is absent; a no-op otherwise.
/// The one sanctioned auto-init, since `--global` is an explicit opt-in. Idempotent under races: a
/// losing `init_project` sees `ConfigAlreadyExists`, and concurrent writers write one fixed scaffold.
///
/// # Errors
///
/// `ProjectContextError::Project` for an I/O failure writing the scaffold.
pub async fn ensure_global_project_initialized(context: &crate::app::Context) -> Result<(), ProjectContextError> {
    use ocx_project::error::ProjectErrorKind;

    if !context.global() {
        return Ok(());
    }

    let home = context.file_structure().root().to_path_buf();
    let config_path = home.join("ocx.toml");

    // Fast path only; `init_project`'s own `symlink_metadata` check is the authoritative one.
    if tokio::fs::symlink_metadata(&config_path).await.is_ok() {
        return Ok(());
    }

    let init_path = config_path.clone();
    let result = tokio::task::spawn_blocking(move || ocx_project::init_project(&init_path))
        .await
        .map_err(|e| {
            ProjectContextError::Project(ocx_project::Error::from(ocx_project::error::ProjectError::new(
                config_path.clone(),
                ProjectErrorKind::Io(std::io::Error::other(e)),
            )))
        })?;

    match result {
        Ok(_) => Ok(()),
        // Another mutator created the file after the fast-path probe; it exists, so succeed.
        Err(ocx_project::Error::Project(pe)) if matches!(pe.kind, ProjectErrorKind::ConfigAlreadyExists { .. }) => {
            Ok(())
        }
        Err(e) => Err(ProjectContextError::Project(e)),
    }
}

/// The error for a resolution that answered `None`: the explicit selection's
/// directory when one was in effect, else the directory the walk started at.
fn no_project(context: &crate::app::Context, walked_from: PathBuf) -> ProjectContextError {
    match ocx_config::loader::ConfigLoader::explicit_project(context.project_path()) {
        Some(dir) => ProjectContextError::NoProjectIn { dir },
        None => ProjectContextError::NoProject { cwd: walked_from },
    }
}

/// Resolves the in-scope `ocx.toml` and its sibling `ocx.lock` path (whether or not it exists)
/// without loading either or applying [`load_project_with_lock`]'s lock gates, which `ocx status` reports on.
/// `walk_from` replaces the CWD as the walk start (`ocx shell allow`/`revoke`'s `PATH`), so a
/// consent gesture lands on the project the prompt activates; a selector still outranks it.
///
/// # Errors
///
/// [`ProjectContextError::NoProject`] when no `ocx.toml` is reachable through
/// the precedence chain, or [`ProjectContextError::Config`] for a resolution
/// I/O failure.
pub async fn resolve_project_paths(
    context: &crate::app::Context,
    walk_from: Option<&Path>,
) -> Result<(PathBuf, PathBuf), ProjectContextError> {
    use ocx_project::error::{ProjectError, ProjectErrorKind};

    let start = match walk_from {
        Some(path) => path.to_path_buf(),
        None => ocx_env::current_dir().map_err(|e| {
            ProjectContextError::Project(ocx_project::Error::from(ProjectError::new(
                std::path::PathBuf::new(),
                ProjectErrorKind::Io(e),
            )))
        })?,
    };
    let home = context.file_structure().root().to_path_buf();
    let resolved = ProjectConfig::resolve(Some(&start), context.project_path(), Some(&home), context.global()).await?;

    resolved.ok_or_else(|| no_project(context, start))
}

/// Loads `ocx.toml` and its sibling `ocx.lock` and applies the staleness gate; never registers (module doc).
///
/// # Errors
///
/// `NoProject`/`NoProjectIn` when no `ocx.toml` is reachable, `Lock` when `ocx.lock`
/// is absent or stale, `Project`/`Config` for parse or I/O errors.
pub async fn load_project_with_lock(context: &crate::app::Context) -> Result<ProjectContext, ProjectContextError> {
    use ocx_project::error::{ProjectError, ProjectErrorKind};

    let cwd = ocx_env::current_dir().map_err(|e| {
        ProjectContextError::Project(ocx_project::Error::from(ProjectError::new(
            std::path::PathBuf::new(),
            ProjectErrorKind::Io(e),
        )))
    })?;
    let home = context.file_structure().root().to_path_buf();
    let resolved = ProjectConfig::resolve(Some(&cwd), context.project_path(), Some(&home), context.global()).await?;

    let (config_path, lock_path) = match resolved {
        Some(pair) => pair,
        None => return Err(no_project(context, cwd)),
    };

    let config = ProjectConfig::from_path(&config_path).await?;

    let lock = match ProjectLock::from_path(&lock_path).await? {
        Some(l) => l,
        None => {
            return Err(ocx_project::LockCurrency::Missing { path: lock_path }.into());
        }
    };

    // Deliberately inert: registration happens only at lock-write sites (module doc).
    let _ = context.file_structure().root();

    if !lock.is_current(&config) {
        return Err(ocx_project::LockCurrency::Stale { lock_path }.into());
    }

    Ok(ProjectContext {
        config_path,
        lock_path,
        config,
        lock,
    })
}

/// The toolchain tree a project-tier composing emitter may follow.
///
/// `groups` is the caller's already-expanded `-g` set, passed verbatim, or `ocx exec`, `ocx env`,
/// `ocx direnv export` and the `env`-mode hook disagree and a `ci` selection answers for the default group.
/// `pinned_cli` is `--pinned`/`--no-pinned` where the command has them, `None` otherwise.
///
/// # Errors
///
/// The scope derivation's own I/O failure, or the home canonicalisation's.
pub async fn toolchain_links(
    context: &crate::app::Context,
    config_path: &Path,
    config: &ProjectConfig,
    lock: &ocx_project::ProjectLock,
    groups: &[String],
    pinned_cli: Option<bool>,
) -> anyhow::Result<ocx_package_manager::ToolchainLinks> {
    let scope = context.toolchain_render_scope(config_path).await?;
    let home = context.manager().toolchain_home(&scope, context.toolchain_root())?;
    Ok(ocx_package_manager::ToolchainLinks {
        pinned: ocx_package_manager::pinned_for_project(pinned_cli, config),
        home,
        scope,
        lock: lock.clone(),
        groups: groups.to_vec(),
    })
}

/// [`load_project_with_lock`], then records shell-activation consent for the project.
///
/// `consent` is `--consent`/`--no-consent` or `None`, resolved in [`record_activation_consent`].
///
/// # Errors
///
/// Exactly [`load_project_with_lock`]'s; a failed recording never fails the command.
pub async fn load_project_with_lock_consenting(
    context: &crate::app::Context,
    consent: Option<bool>,
) -> Result<ProjectContext, ProjectContextError> {
    let project = load_project_with_lock(context).await?;
    record_activation_consent(&project.config_path, &project.lock, consent).await;
    Ok(project)
}

/// Records shell-activation consent for the project at `config_path` over the sources `lock` resolves from.
///
/// Called per command from a closed allowlist, never from a shared loader: stamping in
/// `load_project_with_lock` would grant consent on its read-only callers. Mutators call it after
/// `commit`, so the stamp records the post-mutation set. A failed stamp is logged at WARN and swallowed.
pub async fn record_activation_consent(config_path: &Path, lock: &ocx_project::ProjectLock, consent: Option<bool>) {
    record_activation_consent_over(config_path, ocx_project::consent::lock_sources(lock), consent).await;
}

/// [`record_activation_consent`] over a source set the caller already holds (`ocx init` stamps the empty set).
///
/// `OCX_NO_CONSENT` is read only here, or an ambient read further up outranks an answered flag.
/// `ocx shell allow` calls [`ocx_project::consent::record`] directly because it is the explicit
/// gesture this variable exempts; gating `record` instead would disable that command too.
pub async fn record_activation_consent_over(
    config_path: &Path,
    sources: std::collections::BTreeSet<String>,
    consent: Option<bool>,
) {
    // An invalid value suppresses: context init already refused it, and a typo must never stamp consent.
    if !consent.unwrap_or_else(|| !ocx_env::OCX_NO_CONSENT.bool_or(false).unwrap_or(true)) {
        log::debug!(
            "Shell-activation consent was not recorded for '{}': suppressed by --no-consent or OCX_NO_CONSENT",
            config_path.display()
        );
        return;
    }

    let config_path = config_path.to_path_buf();

    let joined = tokio::task::spawn_blocking(move || {
        let project_dir = ocx_project::consent::canonical_project_dir(&config_path)
            .map_err(|e| format!("canonicalize of config path '{}' failed: {e}", config_path.display()))?;
        ocx_project::consent::record(&project_dir, &sources).map_err(|e| e.to_string())
    })
    .await;

    let outcome = match joined {
        Ok(outcome) => outcome,
        Err(e) => Err(format!("the consent-stamp task panicked or was cancelled: {e}")),
    };
    if let Err(reason) = outcome {
        log::warn!("Shell-activation consent was not recorded (non-fatal): {reason}");
    }
}

/// Pulls every binding in `lock` for `platform` into the object store; a no-op unless `eager`.
///
/// Each binding carries its `config` tag while `lock` binds to `config`, and none once stale.
/// Never touches `symlinks/`, which would add a redundant GC root. An unshipped platform
/// surfaces `NoHostLeaf` (exit 78), and a failure does not roll back the manifest or lock.
///
/// # Errors
///
/// When `eager`: a failed pull, or a fatal patch discovery failure (`DiscoverFailed`) naming the base.
pub async fn materialize_lock(
    context: &crate::app::Context,
    lock: &ocx_project::ProjectLock,
    config: &ProjectConfig,
    eager: bool,
    platform: ocx_oci::Platform,
) -> anyhow::Result<()> {
    if !eager {
        return Ok(());
    }
    let host = lock
        .lenient_host_identifiers(Some(config), &platform)
        .into_iter()
        .map(|(tool, identifier)| identifier.map(|identifier| (tool, identifier)))
        .collect::<Result<Vec<_>, _>>()?;
    // One pull per distinct content; a second tag on the same leaf rides the first.
    let identifiers: Vec<ocx_oci::PackageRef> = ocx_project::first_per_content(host)
        .into_iter()
        .map(|(_, identifier)| identifier.into())
        .collect();
    context
        .manager()
        .pull_all(&identifiers, platform, context.concurrency(), false)
        .await?;
    Ok(())
}

/// Rejects an empty comma segment in `--group` (`-g ci,,lint`) as a usage error (exit 64).
pub(crate) fn ensure_group_segments_nonempty(groups: &[String]) -> anyhow::Result<()> {
    if groups.iter().any(String::is_empty) {
        return Err(
            crate::error::UsageError::new("empty group segment in --group value; check for stray commas").into(),
        );
    }
    Ok(())
}

/// Rejects a `--group` name absent from `[group.*]` (exit 64); `default` and `all` are always valid.
pub(crate) fn ensure_groups_known(groups: &[String], config: &ProjectConfig) -> anyhow::Result<()> {
    for raw in groups {
        if raw == ocx_project::DEFAULT_GROUP || raw == ocx_project::ALL_GROUP {
            continue;
        }
        if !config.groups.contains_key(raw) {
            return Err(crate::error::UsageError::new(format!("unknown group '{raw}' in --group filter")).into());
        }
    }
    Ok(())
}

/// Narrows a [`select_tool_set`](ocx_project::select_tool_set) result to the requested binding names.
///
/// Runs before host-leaf resolution, or an unnamed sibling with no leaf for this host aborts the command.
/// Run [`check_duplicate_selection`](ocx_project::check_duplicate_selection) on the result before resolving.
/// Empty `names` returns `selected` unchanged; otherwise the output follows `names`' order, duplicates dropped.
///
/// # Errors
///
/// Exit 64 ([`crate::error::UsageError`]) when a requested name matches
/// nothing in the selected groups, or matches entries in two or more
/// selected groups that resolve it differently -- narrow with `-g <group>`.
pub(crate) fn filter_by_names(selected: Vec<SelectedTool>, names: &[String]) -> anyhow::Result<Vec<SelectedTool>> {
    if names.is_empty() {
        return Ok(selected);
    }

    let mut hits_by_binding: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::with_capacity(selected.len());
    for (position, tool) in selected.iter().enumerate() {
        hits_by_binding.entry(tool.binding.as_str()).or_default().push(position);
    }

    let mut out = Vec::with_capacity(names.len());
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for name in names {
        if !seen.insert(name.as_str()) {
            continue;
        }
        let hits = hits_by_binding.get(name.as_str()).map(Vec::as_slice).unwrap_or(&[]);
        match hits {
            [] => {
                return Err(
                    crate::error::UsageError::new(format!("binding '{name}' not found in selected groups")).into(),
                );
            }
            [single] => out.push(selected[*single].clone()),
            // Reachable: selection keeps both conflicting entries so a name filter can narrow past them.
            [_, _, ..] => {
                let groups: Vec<String> = hits
                    .iter()
                    .filter_map(|&position| match &selected[position].origin {
                        Origin::Group(group) => Some(group.clone()),
                        Origin::Explicit => None,
                    })
                    .collect();
                let groups_str = groups.join(", ");
                return Err(crate::error::UsageError::new(format!(
                    "binding '{name}' exists in multiple selected groups: [{groups_str}]; pass `-g <group>` to narrow scope"
                ))
                .into());
            }
        }
    }

    Ok(out)
}

/// Mutation-side counterpart to [`load_project_with_lock`]: takes the mutation lock and returns a
/// [`MutationGuard`] over the config and optional predecessor lock, held until commit, rollback or drop.
///
/// Skips the staleness gate, since `add`/`remove`/`lock`/`update` are the commands that fix a stale lock.
/// An absent `ocx.lock` makes [`MutationGuard::previous_lock`] return `None`.
///
/// # Errors
///
/// Returns the same `ProjectContextError` variants as
/// [`load_project_with_lock`] when the project cannot be resolved or its
/// files cannot be loaded. Surfaces `ProjectErrorKind::Locked` (wrapped in
/// `ProjectContextError::Project`) when another writer holds the mutation
/// lock.
pub async fn load_project_for_mutate(context: &crate::app::Context) -> Result<MutationGuard, ProjectContextError> {
    use ocx_project::error::{ProjectError, ProjectErrorKind};

    let cwd = ocx_env::current_dir().map_err(|e| {
        ProjectContextError::Project(ocx_project::Error::from(ProjectError::new(
            std::path::PathBuf::new(),
            ProjectErrorKind::Io(e),
        )))
    })?;
    let home = context.file_structure().root().to_path_buf();
    let resolved = ProjectConfig::resolve(Some(&cwd), context.project_path(), Some(&home), context.global()).await?;
    let (config_path, lock_path) = match resolved {
        Some(pair) => pair,
        None => return Err(no_project(context, cwd)),
    };

    // Locked under `$OCX_HOME/locks`, never on `ocx.toml`: its publish-by-rename would strand the lock.
    debug_assert_eq!(
        lock_path,
        lock_path_for(&config_path),
        "lock_path must be derived from config_path"
    );
    // Acquired before the snapshot read, or a concurrent writer races between read and commit.
    let mutate_lock = acquire_project_lock_for_file(&config_path, &context.file_structure().locks).await?;

    let manifest = ocx_project::mutate::read_manifest_snapshot(&config_path).await?;

    // Raw bytes kept so rollback restores the predecessor byte-for-byte; re-serializing the parsed lock may not.
    let previous_lock = ProjectLock::from_path(&lock_path).await?;
    let previous_lock_bytes = match &previous_lock {
        Some(_) => match tokio::fs::read(&lock_path).await {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(
                    ocx_project::Error::from(ProjectError::new(lock_path.clone(), ProjectErrorKind::Io(e))).into(),
                );
            }
        },
        None => None,
    };

    Ok(MutationGuard::from_parts(
        mutate_lock,
        config_path,
        lock_path,
        home,
        manifest,
        previous_lock,
        previous_lock_bytes,
    ))
}

#[cfg(test)]
mod tests {
    use ocx_oci::{Digest, PackageRef, PinnedPackageRef};
    use ocx_project::ToolSource;

    use super::*;

    /// Reds on: a project-context slug, delegated or walked, naming another cause than the exit
    /// code does.
    #[test]
    fn project_context_details_name_the_cause_that_decides_the_code() {
        use crate::exit::tests::assert_detail;

        let no_project = ProjectContextError::NoProject {
            cwd: PathBuf::from("/work"),
        };
        assert_detail(&no_project, "no_project");
        let stale = ProjectContextError::from(ocx_project::LockCurrency::Stale {
            lock_path: PathBuf::from("/work/proj/ocx.lock"),
        });
        assert_detail(&stale, "lock_stale");
        let diverged = ProjectContextError::Project(ocx_project::Error::from(ocx_project::ProjectError::new(
            PathBuf::from("/work/proj/ocx.toml"),
            ocx_project::error::ProjectErrorKind::ManifestEditDiverged,
        )));
        assert_detail(&diverged, "project_manifest_edit_diverged");
    }

    /// Finding 11 — the two lock states are one contract, and this enum must
    /// *delegate* to it rather than carry a second copy of the mapping.
    ///
    /// `ProjectContextError` and `ocx_package_manager::activation::SessionError` used to spell the
    /// same two `#[error]` strings and the same 78/65 twice over. They now both
    /// wrap [`ocx_project::LockCurrency`], so one mutation to that type's
    /// `classify` must red this **and** `app.rs`'s
    /// `c343_the_session_refusals_keep_their_exit_codes_through_anyhow` — which
    /// is what proves the two enums share one mapping rather than agreeing by
    /// coincidence.
    ///
    /// Red state: swap the two arms in `LockCurrency::classify`.
    #[test]
    fn f011_the_lock_states_classify_through_the_shared_currency_type() {
        use ocx_exit::ClassifyExitCode as _;
        use ocx_exit::ExitCode;
        use ocx_project::LockCurrency;

        let missing = ProjectContextError::from(LockCurrency::Missing {
            path: PathBuf::from("/work/proj/ocx.lock"),
        });
        assert_eq!(
            missing.classify(),
            Some(ExitCode::ConfigError),
            "an absent lock is a configuration gap (78)"
        );

        let stale = ProjectContextError::from(LockCurrency::Stale {
            lock_path: PathBuf::from("/work/proj/ocx.lock"),
        });
        assert_eq!(
            stale.classify(),
            Some(ExitCode::DataError),
            "a stale lock is stale on-disk data (65)"
        );

        // The wording is the other half of the contract: a user who meets this
        // from `ocx pull` and from a prompt must read one sentence, not two
        // that happen to match today.
        assert_eq!(
            missing.to_string(),
            ocx_package_manager::activation::SessionError::from(LockCurrency::Missing {
                path: PathBuf::from("/work/proj/ocx.lock"),
            })
            .to_string(),
        );
    }

    // ── the write seam is a closed allowlist of eight commands ─────────────

    /// The seam call every consent writer routes through. A caller that stamps
    /// names one of these two; a caller that does not, names neither.
    const SEAM_CALLS: [&str; 3] = [
        "record_activation_consent(",
        "record_activation_consent_over(",
        "load_project_with_lock_consenting(",
    ];

    /// Strip `//`-prefixed lines so a guard never matches the comments that
    /// document the very shape it polices.
    fn code_only(source: &str) -> String {
        source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether `source` calls the consent seam at all.
    fn stamps(source: &str) -> bool {
        let code = code_only(source);
        SEAM_CALLS.iter().any(|call| code.contains(call))
    }

    /// The body of the top-level `fn` named `name`, from its signature to the
    /// first line that is a bare `}` at column zero.
    fn function_body(source: &str, name: &str) -> String {
        let needle = format!("fn {name}(");
        let start = source
            .find(&needle)
            .unwrap_or_else(|| panic!("`fn {name}` must exist in project_context.rs"));
        let rest = &source[start..];
        let end = rest
            .find("\n}\n")
            .unwrap_or_else(|| panic!("`fn {name}` must end with a `}}` at column zero"));
        rest[..end].to_string()
    }

    /// Exactly the eight explicit project-scoped commands stamp consent, and
    /// no other command file does.
    ///
    /// Both halves are asserted. The positive half is what keeps this from
    /// passing vacuously if the seam is ever renamed out from under
    /// [`SEAM_CALLS`]; the negative half is the security contract — a stamp
    /// written from `ocx inspect` or `ocx env` would consent to a project the
    /// user only asked to look at.
    #[test]
    fn a029_exactly_eight_commands_write_a_consent_stamp() {
        let members: [(&str, &str); 8] = [
            // `init` is the one member that creates the project it consents to,
            // through the sources-taking half of the seam: it has no lock to
            // derive a source set from (ocx-sh/ocx#397).
            ("init", include_str!("../command/init.rs")),
            ("add", include_str!("../command/add.rs")),
            ("remove", include_str!("../command/remove.rs")),
            ("lock", include_str!("../command/lock.rs")),
            ("update", include_str!("../command/update.rs")),
            ("upgrade", include_str!("../command/upgrade.rs")),
            ("pull", include_str!("../command/pull.rs")),
            ("exec", include_str!("../command/toolchain_exec.rs")),
        ];
        // The commands that reach a project loader but must never consent —
        // `inspect`, `patch freeze` and `ocx env` share `load_project_with_lock`
        // with `run`/`pull`; `status`, `direnv export` and `shell state` are the
        // read-only surfaces the allowlist explicitly excludes.
        let non_members: [(&str, &str); 6] = [
            ("inspect", include_str!("../command/inspect.rs")),
            ("patch freeze", include_str!("../command/patch_freeze.rs")),
            ("env", include_str!("../command/toolchain_env.rs")),
            ("status", include_str!("../command/status.rs")),
            ("direnv export", include_str!("../command/direnv_export.rs")),
            ("shell state", include_str!("../command/shell_state.rs")),
        ];

        for (name, source) in members {
            assert!(
                stamps(source),
                "`ocx {name}` is a consent writer and must call the seam"
            );
        }
        for (name, source) in non_members {
            assert!(
                !stamps(source),
                "`ocx {name}` must not write a consent stamp; it calls the seam"
            );
        }
    }

    /// The structural half the file-set guard above cannot see: the
    /// **shared** loader stamps nothing.
    ///
    /// `lock --check` calls `load_project_with_lock` from inside `lock.rs`,
    /// which the file-set guard above counts as a member. Only this assertion
    /// catches a stamp moved into the shared loader, where it would fire for
    /// `lock --check`, `inspect`, `patch freeze` and `ocx env` alike.
    #[test]
    fn a029_the_shared_loaders_never_stamp() {
        let source = include_str!("project_context.rs");

        for name in ["load_project_with_lock", "load_project_for_mutate"] {
            let body = code_only(&function_body(source, name));
            assert!(
                body.contains("ProjectConfig::resolve"),
                "`fn {name}`'s body did not extract — the guard is watching nothing"
            );
            for call in SEAM_CALLS {
                assert!(
                    !body.contains(call),
                    "`fn {name}` is shared with non-consenting callers and must not call `{call}`"
                );
            }
        }

        let opt_in = code_only(&function_body(source, "load_project_with_lock_consenting"));
        assert!(
            opt_in.contains("record_activation_consent("),
            "the opt-in loader must be the one that stamps, or the guard above is vacuous"
        );
    }

    /// ocx-sh/ocx#400 — `OCX_NO_CONSENT` is read at exactly one depth, and it
    /// is the deepest one.
    ///
    /// The ladder is flag, then env, then stamp. A second ambient read in
    /// either wrapper above the seam would fire for a caller whose
    /// `--consent` had already answered, so `OCX_NO_CONSENT=1 ocx pull
    /// --consent` would record nothing — a flag losing to an env var, which
    /// inverts the house precedence everywhere else in this CLI. That defect
    /// is invisible in a diff of either function alone, which is why the
    /// property is asserted over all three at once.
    ///
    /// Red state: move the `ocx_env::OCX_NO_CONSENT.bool_or` call from
    /// `record_activation_consent_over` up into `record_activation_consent`.
    #[test]
    fn c400_the_consent_env_var_is_read_only_at_the_seam() {
        let source = include_str!("project_context.rs");

        let seam = code_only(&function_body(source, "record_activation_consent_over"));
        // The `bool_or(` half matters: the debug line below the gate names the
        // variable too, so a guard that only looked for the spelling would stay
        // green with the read itself deleted.
        assert!(
            seam.contains("ocx_env::OCX_NO_CONSENT.bool_or("),
            "the seam must be the function that reads the env var, or the guard below is vacuous"
        );

        // The extraction-sanity marker must be a call each body makes, never a
        // substring of its own signature: `function_body` slices from
        // `fn <name>(` inclusive, so a signature-only extraction still contains
        // `consent` for both of these and the guard below would pass watching
        // nothing.
        for (name, marker) in [
            ("record_activation_consent", "record_activation_consent_over("),
            ("load_project_with_lock_consenting", "load_project_with_lock("),
        ] {
            let body = code_only(&function_body(source, name));
            assert!(
                body.contains(marker),
                "`fn {name}`'s body did not extract — the guard is watching nothing"
            );
            assert!(
                !body.contains("OCX_NO_CONSENT"),
                "`fn {name}` forwards the caller's tri-state and must not read the env itself; \
                 a second read here outranks the `--consent` flag"
            );
        }
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    fn pin(repository: &str, marker: char) -> PinnedPackageRef {
        let digest = Digest::Sha256(std::iter::repeat_n(marker, 64).collect());
        let identifier = PackageRef::new_registry(repository, "ocx.sh").clone_with_digest(digest);
        PinnedPackageRef::try_from(identifier).expect("digest present")
    }

    fn tool(binding: &str, marker: char, group: &str) -> SelectedTool {
        SelectedTool {
            binding: binding.into(),
            origin: Origin::Group(group.into()),
            source: ToolSource::Explicit(pin(binding, marker).into()),
        }
    }

    // ── filter_by_names ──────────────────────────────────────────────────────

    /// An empty name list means "every binding in scope", not "nothing".
    #[test]
    fn filter_empty_names_returns_full_set() {
        let selected = vec![tool("cmake", 'a', "default"), tool("ninja", 'b', "default")];
        let result = filter_by_names(selected.clone(), &[]).expect("empty names must succeed");
        assert_eq!(result.len(), selected.len());
        assert!(result.iter().any(|entry| entry.binding == "cmake"));
        assert!(result.iter().any(|entry| entry.binding == "ninja"));
    }

    /// A name with no match in the selected groups is a usage error.
    #[test]
    fn filter_unknown_name_errors() {
        let selected = vec![tool("cmake", 'a', "default")];
        let error = filter_by_names(selected, &["does-not-exist".into()]).expect_err("unknown name must fail");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("binding 'does-not-exist' not found in selected groups"),
            "unexpected message: {rendered}"
        );
    }

    /// Two selected groups resolving one binding differently is ambiguous only
    /// when the user actually names it — and the message lists both groups so
    /// the `-g` remedy is actionable.
    #[test]
    fn filter_ambiguous_name_errors_with_groups_listed() {
        let selected = vec![tool("tool", 'a', "ci"), tool("tool", 'b', "release")];
        let error = filter_by_names(selected, &["tool".into()]).expect_err("ambiguous name must fail");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("binding 'tool' exists in multiple selected groups"),
            "unexpected message: {rendered}"
        );
        assert!(rendered.contains("ci"), "groups must name 'ci': {rendered}");
        assert!(rendered.contains("release"), "groups must name 'release': {rendered}");
    }

    /// Happy path: exactly one matching entry survives.
    #[test]
    fn filter_unique_name_picks_single_entry() {
        let selected = vec![tool("cmake", 'a', "default"), tool("ninja", 'b', "default")];
        let result = filter_by_names(selected, &["cmake".into()]).expect("ok");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].binding, "cmake");
    }

    /// User-supplied name order wins over selection order.
    #[test]
    fn filter_preserves_user_name_order_not_selection_order() {
        let selected = vec![
            tool("a", 'a', "default"),
            tool("b", 'b', "default"),
            tool("c", 'c', "default"),
        ];
        let names: Vec<String> = vec!["b".into(), "a".into()];
        let result = filter_by_names(selected, &names).expect("ok");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].binding, "b", "user-order: b must come first");
        assert_eq!(result[1].binding, "a", "user-order: a must come second");
    }

    /// Naming one binding twice is a typo, not a usage error.
    #[test]
    fn filter_duplicate_names_deduplicated_silently() {
        let selected = vec![tool("cmake", 'a', "default")];
        let names: Vec<String> = vec!["cmake".into(), "cmake".into()];
        let result = filter_by_names(selected, &names).expect("dedup must succeed");
        assert_eq!(result.len(), 1, "duplicate name must be silently deduped to one entry");
        assert_eq!(result[0].binding, "cmake");
    }

    /// The whole point of deferring the duplicate check: a conflict between two
    /// selected groups is dropped by the filter when the user named something
    /// else, so the surviving set validates clean.
    #[test]
    fn filter_drops_an_unnamed_conflict_so_the_check_passes() {
        let selected = vec![
            tool("shellcheck", 'a', "ci"),
            tool("shellcheck", 'b', "lint"),
            tool("cmake", 'c', "ci"),
        ];
        let result = filter_by_names(selected, &["cmake".into()]).expect("naming an unrelated binding must succeed");
        ocx_project::check_duplicate_selection(&result).expect("the surviving set carries no conflict");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].binding, "cmake");
    }
}
