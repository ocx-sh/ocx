// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx [--global] patch` — patch overlay management commands.
//!
//! Sub-commands:
//! - **freeze**: write a `patches.snapshot.json` to pin companion digests for
//!   reproducible builds.
//! - **sync**: refresh patch descriptors and companions from the registry for
//!   installed packages.
//! - **publish**: push a patch descriptor to the patch registry (maintainer).
//! - **test**: dry-run compose a descriptor onto a base without publishing
//!   (maintainer).
//!
//! # Design
//!
//! The freeze and sync commands are toolchain-tier: they operate on an
//! `ocx.toml` / `ocx.lock` project (or `$OCX_HOME` under `--global`). The
//! snapshot file is written as a sibling of `ocx.lock`. The publish and test
//! commands are maintainer commands operating against the configured `[patches]`
//! registry tier.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context as _;
use clap::{Args, Subcommand};
use ocx_lib::utility::child_process;
use ocx_lib::{
    cli,
    cli::UsageError,
    env, oci, package,
    package_manager::tasks::patch_discovery::{global_descriptor_id, patch_descriptor_id},
    patch::{PATCH_SNAPSHOT_FILE, PatchSnapshot},
    prelude::*,
    publisher::LayerRef,
};

use crate::{conventions, conventions::platforms_or_default, options};

/// Manage patch overlays for a project.
///
/// Commands here read or write `patches.snapshot.json`, which pins companion
/// package digests for reproducible builds. Use `patch freeze` to write the
/// snapshot and `patch sync` to refresh patch descriptors and companions from
/// the registry.
#[derive(Subcommand)]
pub enum PatchGroup {
    /// Freeze companion package digests to a snapshot for reproducible builds.
    ///
    /// Resolves every companion and descriptor digest in the active patch
    /// overlay and writes a `patches.snapshot.json` file beside `ocx.lock`.
    /// Once frozen, setting `OCX_PATCH_SNAPSHOT` to that path makes all env
    /// composition prefer the pinned digests over live tag lookups.
    ///
    /// Works offline: only the local object store is consulted.
    Freeze(PatchFreezeArgs),

    /// Refresh patch descriptors and companion packages from the registry.
    ///
    /// Re-fetches every patch descriptor for all installed packages and the
    /// global descriptor. Installs any newly-referenced companion packages.
    /// Requires network access.
    ///
    /// This command also picks up patches for packages installed before patch
    /// configuration was added. All states are re-checked regardless of what
    /// was previously recorded.
    Sync(PatchSyncArgs),

    /// Publish a patch descriptor to the patch registry.
    ///
    /// Reads a descriptor JSON file, validates it, and pushes it to the
    /// configured patch registry under either the reserved global repository
    /// (`--global`) or the package-specific sub-path for a given base
    /// identifier.
    ///
    /// The descriptor only references companion packages by identifier. Publish
    /// the companion packages separately with `ocx package push`. Requires
    /// network access; fails in offline mode.
    Publish(PatchPublishArgs),

    /// Compose a patch descriptor onto a base locally, without publishing.
    ///
    /// Reads a descriptor JSON file and composes its matched companions onto the
    /// given base identifier in a scratch store, then either runs a test script,
    /// runs a trailing command in the composed environment, or prints the
    /// composed environment. Lets a maintainer verify a descriptor before
    /// publishing it.
    ///
    /// Required companion packages must be resolvable (installed locally or
    /// pulled from the registry); an unresolvable required companion fails the
    /// command.
    Test(PatchTestArgs),
}

/// Arguments for `ocx [--global] patch freeze`.
#[derive(Args)]
pub struct PatchFreezeArgs {
    // No positional arguments — freeze always targets the in-scope project
    // (or `$OCX_HOME` under `--global`).
}

/// Arguments for `ocx [--global] patch sync`.
#[derive(Args)]
pub struct PatchSyncArgs {
    #[clap(flatten)]
    platforms: options::Platforms,
}

/// Arguments for `ocx [--global] patch publish`.
#[derive(Args)]
pub struct PatchPublishArgs {
    /// Path to the patch descriptor JSON file to publish.
    #[clap(long = "descriptor-file", required = true)]
    descriptor_file: PathBuf,

    /// Publish the descriptor as the global descriptor so it applies to every
    /// base. Stored at the reserved `global` repository in the patch registry.
    /// Mutually exclusive with a base identifier.
    #[clap(long = "global", conflicts_with = "base")]
    global: bool,

    /// Base identifier whose package-specific patch path receives the
    /// descriptor. Omit with `--global` for the global descriptor.
    #[clap(value_name = "BASE-ID", required_unless_present = "global")]
    base: Option<options::Identifier>,

    /// Target platform for resolving the patch path template. Defaults to the
    /// host platform.
    #[clap(short, long)]
    platform: Option<oci::Platform>,
}

/// Arguments for `ocx patch test`.
#[derive(Args)]
pub struct PatchTestArgs {
    /// Path to the patch descriptor JSON file to compose.
    #[clap(long = "descriptor-file", required = true)]
    descriptor_file: PathBuf,

    /// Target platform for composing the environment. Defaults to the host
    /// platform.
    #[clap(short, long)]
    platform: Option<oci::Platform>,

    /// Path to a local archive for a companion package, allowing the companion
    /// to be materialized without a registry round-trip. Repeatable.
    #[clap(long = "companion-archive", value_name = "PATH")]
    companion_archives: Vec<PathBuf>,

    /// Base identifier to compose the descriptor onto.
    #[clap(value_name = "BASE-ID", required = true)]
    base: options::Identifier,

    /// Path to a Starlark test script to run in the composed environment.
    /// Mutually exclusive with a trailing command.
    #[clap(long, conflicts_with = "command")]
    script: Option<PathBuf>,

    /// Command to run in the composed environment, after `--`. Mutually
    /// exclusive with `--script`. When neither is given, the composed
    /// environment is printed.
    #[clap(allow_hyphen_values = true, last = true, num_args = 1..)]
    command: Vec<String>,
}

impl PatchGroup {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            PatchGroup::Freeze(args) => args.execute(context).await,
            PatchGroup::Sync(args) => args.execute(context).await,
            PatchGroup::Publish(args) => args.execute(context).await,
            PatchGroup::Test(args) => args.execute(context).await,
        }
    }
}

impl PatchFreezeArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Step 1: Resolve the directory where the snapshot will be written. ──
        //
        // The snapshot lives beside `ocx.lock` (or `$OCX_HOME` under --global).
        // For the project tier, we locate the project via the standard resolution
        // chain (--global / --project / OCX_PROJECT / CWD walk). For the global
        // tier, the directory is always `$OCX_HOME`.
        let snapshot_dir = resolve_snapshot_dir(&context).await?;
        let snapshot_path = snapshot_dir.join(PATCH_SNAPSHOT_FILE);

        // ── Step 2: Resolve site-patch roots (local only, no network). ──
        //
        // Uses the manager's current `patches` config (from OCX_PATCHES /
        // `[patches]` config tier). When no patch tier is configured, roots is
        // empty and the snapshot records zero companions / descriptors.
        let host = oci::Platform::current().unwrap_or_else(oci::Platform::any);
        let roots = context
            .manager()
            .resolve_site_patch_roots(&[host])
            .await
            .map_err(anyhow::Error::new)?;

        let companion_count = roots.companions.len();
        let descriptor_count = roots.descriptors.len();

        // ── Step 3: Build and write the snapshot. ──
        let snapshot = PatchSnapshot::from_roots(&roots);
        snapshot.write(&snapshot_path).await.map_err(anyhow::Error::new)?;

        // ── Step 4: Report. ──
        context
            .api()
            .report(&crate::api::data::patch_freeze::PatchFreezeReport::new(
                companion_count,
                descriptor_count,
                snapshot_path,
            ))?;

        Ok(ExitCode::SUCCESS)
    }
}

impl PatchSyncArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Step 1: Resolve the platform set to sync. ──
        let platforms = platforms_or_default(self.platforms.as_slice());

        // ── Step 2: Run the sync. ──
        let report = context
            .manager()
            .sync_patches(&platforms)
            .await
            .map_err(anyhow::Error::new)?;

        // ── Step 3: Report. ──
        context
            .api()
            .report(&crate::api::data::patch_sync::PatchSyncReport::new(report))?;

        Ok(ExitCode::SUCCESS)
    }
}

impl PatchPublishArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Step 1: The patch tier must be configured. ──
        let patches = context
            .manager()
            .patches()
            .ok_or_else(|| {
                UsageError::new(
                    "no patch registry configured; set a [patches] config tier or OCX_PATCHES before publishing",
                )
            })?
            .clone();

        // ── Step 2: Read + validate the descriptor JSON file. ──
        let descriptor_bytes = tokio::fs::read(&self.descriptor_file)
            .await
            .with_context(|| format!("reading descriptor file {}", self.descriptor_file.display()))?;
        // Validate up front for a clear error before any network work.
        ocx_lib::patch::PatchDescriptor::from_json_bytes(&descriptor_bytes)
            .with_context(|| format!("validating patch descriptor file {}", self.descriptor_file.display()))?;

        // ── Step 3: Compute the target patch repo identifier. ──
        //
        // `base` is guaranteed present by `required_unless_present = "global"`
        // when `--global` is absent; resolve its default registry here so the
        // selection helper stays a pure function over already-resolved identifiers.
        let base_id = match &self.base {
            Some(base_raw) => Some(base_raw.with_domain(context.default_registry())?),
            None => None,
        };
        let patch_repo_id = select_publish_target(&patches, self.global, base_id.as_ref());

        // ── Step 4: Publish via the lib orchestration method. ──
        let report = context
            .manager()
            .publish_patch_descriptor(&patch_repo_id, &descriptor_bytes)
            .await
            .map_err(anyhow::Error::new)?;

        // ── Step 5: Report. ──
        context
            .api()
            .report(&crate::api::data::patch_publish::PatchPublishReport::new(report))?;

        Ok(ExitCode::SUCCESS)
    }
}

impl PatchTestArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        run_patch_test(self, context).await
    }
}

/// Select the patch repo identifier `ocx patch publish` targets.
///
/// `--global` targets the reserved global descriptor repository (applies to
/// every base); otherwise the descriptor lands at the package-specific sub-path
/// for `base_id`. `base_id` is `Some` whenever `global` is false (clap's
/// `required_unless_present` guarantees it); when both are absent the global
/// descriptor is used as a safe fallback so the function stays total.
fn select_publish_target(
    patches: &ocx_lib::ResolvedPatchConfig,
    global: bool,
    base_id: Option<&oci::Identifier>,
) -> oci::Identifier {
    match (global, base_id) {
        (false, Some(base)) => patch_descriptor_id(patches, base),
        // `--global`, or (defensively) no base supplied → the global descriptor.
        _ => global_descriptor_id(patches),
    }
}

/// Resolve the directory that will contain `patches.snapshot.json`.
///
/// Under `--global` (`context.global()` is true): `$OCX_HOME`.
/// Under project tier: the parent directory of the resolved `ocx.lock` (i.e.,
/// the same directory as `ocx.toml`). Returns an error if no project can be
/// found.
async fn resolve_snapshot_dir(context: &crate::app::Context) -> anyhow::Result<std::path::PathBuf> {
    if context.global() {
        // Global tier: snapshot beside $OCX_HOME/ocx.lock.
        Ok(context.file_structure().root().to_path_buf())
    } else {
        // Project tier: use the same project-resolution chain as pull/env/run.
        use crate::app::project_context::load_project_with_lock;
        let ctx = load_project_with_lock(context).await?;
        // The lock path is always <project_dir>/ocx.lock, so its parent is the
        // project directory. The snapshot goes beside it. If `parent()` returns
        // `None` (a bare filename with no directory component), fall back to the
        // current working directory rather than treating the lock file itself as
        // a directory, which would produce a path under a file and confuse the
        // error message on write failure.
        let dir = match ctx.lock_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            // Both branches failed: the lock path has no directory component AND
            // the CWD is unreadable. Fall back to the CWD-relative ".", never the
            // lock file path itself (which would yield a path *under* a file).
            _ => std::path::PathBuf::from("."),
        };
        Ok(dir)
    }
}

/// Build a scratch `PackageManager` over a temporary `FileStructure` that shares
/// the running `Context`'s remote sources (index + client) and patch tier.
///
/// Companion pulls and seeded descriptor blobs land in the scratch CAS so the
/// real `$OCX_HOME` is never mutated by `ocx patch test`.
fn build_scratch_manager(
    context: &crate::app::Context,
    scratch_root: &std::path::Path,
) -> ocx_lib::package_manager::PackageManager {
    use ocx_lib::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};

    let file_structure = ocx_lib::file_structure::FileStructure::with_root(scratch_root.to_path_buf());
    let local_index = LocalIndex::new(LocalConfig {
        tag_store: ocx_lib::file_structure::TagStore::new(scratch_root.join("tags")),
        blob_store: ocx_lib::file_structure::BlobStore::new(scratch_root.join("blobs")),
    });

    // Share the running context's remote sources so companions can be pulled
    // into the scratch CAS. Offline → no source (the scratch manager is offline
    // too, and required-companion resolution fails closed).
    let (mode, sources, client): (ChainMode, Vec<Index>, Option<oci::Client>) = match context.remote_index() {
        Ok(remote) => (
            ChainMode::Default,
            vec![Index::from_remote(remote.clone())],
            context.remote_client().ok().cloned(),
        ),
        Err(_) => (ChainMode::Offline, Vec::new(), None),
    };
    let index = Index::from_chained(local_index, sources, mode);

    ocx_lib::package_manager::PackageManager::new(file_structure, index, client, context.default_registry())
        .with_patches(context.manager().patches().cloned())
}

/// Implementation of `ocx patch test`.
///
/// Materializes the base (and the descriptor's matched companions) into a
/// scratch store, then delegates to
/// [`ocx_lib::package_manager::PackageManager::seed_and_compose_patch_test`] to
/// seed the local descriptor and compose the companion overlay onto the base.
/// Finally either runs a Starlark script, runs a trailing command, or prints the
/// composed environment.
async fn run_patch_test(args: &PatchTestArgs, context: crate::app::Context) -> anyhow::Result<ExitCode> {
    // ── Step 0: Patch tier must be configured. ──
    let patches = context
        .manager()
        .patches()
        .ok_or_else(|| {
            UsageError::new("no patch registry configured; set a [patches] config tier or OCX_PATCHES before testing")
        })?
        .clone();

    let platform = args
        .platform
        .clone()
        .unwrap_or_else(|| oci::Platform::current().unwrap_or_else(oci::Platform::any));
    let base_id = args.base.with_domain(context.default_registry())?;

    // ── Step 1: Read + validate the descriptor file. ──
    let descriptor_bytes = tokio::fs::read(&args.descriptor_file)
        .await
        .with_context(|| format!("reading descriptor file {}", args.descriptor_file.display()))?;
    let descriptor = ocx_lib::patch::PatchDescriptor::from_json_bytes(&descriptor_bytes)
        .with_context(|| format!("validating patch descriptor file {}", args.descriptor_file.display()))?;

    // ── Step 2: Provision a scratch FileStructure (tempdir). ──
    let temp_root = context.file_structure().temp.root().join("patch-test");
    tokio::fs::create_dir_all(&temp_root)
        .await
        .map_err(|e| ocx_lib::error::file_error(&temp_root, e))?;
    let scratch = tempfile::Builder::new()
        .prefix("patch-test-")
        .tempdir_in(&temp_root)
        .map_err(|e| ocx_lib::error::file_error(&temp_root, e))?;
    let scratch_root = scratch.path().to_path_buf();

    let manager = build_scratch_manager(&context, &scratch_root);

    // ── Step 3: Materialize the base into the scratch store. ──
    //
    // `pull` materializes the base (and its transitive deps) WITHOUT triggering
    // patch discovery — discovery would hit the real patch registry and clobber
    // the seeded descriptor. The maintainer is testing online, so the base is
    // pulled from the shared registry source; offline → fail-closed.
    let base_info = manager
        .pull(&base_id, vec![platform.clone()])
        .await
        .map_err(ocx_lib::Error::from)
        .with_context(|| format!("materializing base '{base_id}' into the scratch store"))?;
    let base_arc = Arc::new(base_info);

    // ── Step 4: Materialize the descriptor's matched companions into the scratch
    // store so the compose step finds them. Companions named for BASE-ID come from
    // a local archive (`--companion-archive`) when supplied, else are pulled from
    // the registry. Required companions fail closed via the compose step (C7). ──
    let companions = descriptor.collect_companions(&base_id, patches.required);
    materialize_companions(&context, &manager, &companions, &args.companion_archives, &platform).await?;

    // ── Step 5: Seed the local descriptor and compose the overlay onto the base. ──
    let composition = manager
        .seed_and_compose_patch_test(&base_arc, &descriptor_bytes, &patches)
        .await
        .map_err(ocx_lib::Error::from)?;

    // ── Step 6: Build the composed process env (mirrors package_test.rs). ──
    let mut process_env = env::Env::new();
    process_env.apply_entries(&composition.entries);
    process_env.apply_ocx_config(context.config_view());

    // ── Step 7: Dispatch on --script / trailing command / print env. ──
    if let Some(script_path) = &args.script {
        run_patch_test_script(&context, script_path, &base_arc, &scratch_root, &platform, process_env).await
    } else if !args.command.is_empty() {
        let (command, command_args) = args.command.split_first().expect("non-empty command checked above");
        let resolved = process_env.resolve_command(command);
        let status = child_process::spawn_and_wait(&resolved, command_args, process_env)
            .await
            .map_err(|e| anyhow::Error::from(e).context(format!("failed to run '{}'", resolved.display())))?;
        Ok(child_process::propagate_exit_code(status))
    } else {
        // Print the composed companion env entries.
        let companion_strings: Vec<String> = composition.matched_companions.iter().map(ToString::to_string).collect();
        let entries: Vec<crate::api::data::patch_test::PatchTestEntry> = composition
            .entries
            .iter()
            .map(|entry| crate::api::data::patch_test::PatchTestEntry {
                key: entry.key.clone(),
                value: entry.value.clone(),
                kind: entry.kind.clone(),
            })
            .collect();
        let report =
            crate::api::data::patch_test::PatchTestReport::new(base_id.to_string(), companion_strings, entries);
        context.api().report(&report)?;
        Ok(ExitCode::SUCCESS)
    }
}

/// Materialize the descriptor's matched companions into the scratch store.
///
/// For each `--companion-archive`, materialize via `pull_local` (no registry
/// round-trip) using the archive's sibling metadata file, following the same
/// convention as `ocx package test`. Remaining companions are pulled from the
/// shared registry source.
///
/// Fail posture mirrors production composition (`build_site_patch_set`):
/// - A **required** companion (effective `required == true`) that cannot be
///   pulled is surfaced directly so the maintainer sees the unresolvable
///   companion (the fail-closed compose step would also catch it via C7).
/// - An **optional** companion (effective `required == false`) that cannot be
///   pulled is warned-and-skipped, leaving the subsequent compose step to
///   exercise the real fail-open path — so the dry-run preview matches what
///   production would do when an optional companion is not yet published.
async fn materialize_companions(
    context: &crate::app::Context,
    manager: &ocx_lib::package_manager::PackageManager,
    companions: &[ocx_lib::patch::CompanionEntry],
    companion_archives: &[PathBuf],
    platform: &oci::Platform,
) -> anyhow::Result<()> {
    // Materialize each supplied local archive via the package-test local path.
    // The archive's sibling metadata file names the identifier the maintainer
    // authored, so the local companion lands under its own identifier AND its
    // tag → digest is registered in the scratch tag store (so companion
    // resolution finds it without a registry round-trip). The returned
    // `registry/repository` key lets the registry-pull loop below skip it — an
    // UNPUBLISHED local companion must not trigger a failing registry pull.
    let mut local_companion_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for archive in companion_archives {
        let layer = LayerRef::File(archive.clone());
        let metadata_path = conventions::resolve_metadata_path(std::slice::from_ref(&layer), None)
            .with_context(|| format!("locating metadata sibling for companion archive {}", archive.display()))?;
        let metadata = package::metadata::ValidMetadata::try_from(
            package::metadata::Metadata::read_json(&metadata_path)
                .await
                .with_context(|| format!("reading companion metadata from {}", metadata_path.display()))?,
        )?;
        let identifier = ocx_lib::oci::Identifier::parse_with_default_registry(
            metadata_identifier_or_error(&metadata_path)?.as_str(),
            manager.default_registry(),
        )
        .with_context(|| format!("parsing companion identifier from {}", metadata_path.display()))?;
        let info = package::info::Info {
            identifier,
            metadata: metadata.into(),
            platform: platform.clone(),
        };
        let key = manager
            .materialize_test_companion(info, std::slice::from_ref(&layer))
            .await
            .map_err(ocx_lib::Error::from)
            .with_context(|| format!("materializing companion archive {}", archive.display()))?;
        local_companion_keys.insert(key);
    }

    // Pull every descriptor-named companion from the registry, EXCEPT those
    // already materialized from a local archive above (skipped by their
    // `registry/repository` key). An unresolvable REQUIRED companion fails here so
    // the maintainer sees it; an unresolvable OPTIONAL companion is
    // warned-and-skipped so the compose step exercises the real fail-open path
    // (production parity).
    for companion in companions {
        let repo_key = format!(
            "{}/{}",
            companion.identifier.registry(),
            companion.identifier.repository()
        );
        if local_companion_keys.contains(&repo_key) {
            continue;
        }
        let pull_result = manager
            .pull(&companion.identifier, vec![platform.clone()])
            .await
            .map_err(ocx_lib::Error::from);
        match pull_result {
            Ok(_) => {}
            Err(error) if !companion.required => {
                context.ui().warn(format!(
                    "skipping optional companion '{}' that could not be resolved: {error:#}",
                    companion.identifier
                ));
            }
            Err(error) => {
                return Err(
                    anyhow::Error::from(error).context(format!("resolving companion '{}'", companion.identifier))
                );
            }
        }
    }
    Ok(())
}

/// Read the `identifier` field a maintainer set in a companion metadata file.
///
/// The package-test convention requires the identifier on the CLI; for patch
/// test the companion archive carries it in its metadata sibling under an
/// `identifier` key. Absent → a clear usage error.
fn metadata_identifier_or_error(metadata_path: &std::path::Path) -> anyhow::Result<String> {
    let raw = std::fs::read_to_string(metadata_path).map_err(|e| ocx_lib::error::file_error(metadata_path, e))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing companion metadata {}", metadata_path.display()))?;
    value
        .get("identifier")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow::Error::from(UsageError::new(format!(
                "companion metadata {} must carry an \"identifier\" field naming the companion package",
                metadata_path.display()
            )))
        })
}

/// Run a Starlark test script in the composed patch-test environment.
///
/// Mirrors `ocx package test`'s script branch: reads the script source, provides
/// a scratch sandbox sibling to the base package root, runs the engine on the
/// multi-thread runtime, reports the structured outcome, and maps it to an exit
/// code (bypassing `classify_error`).
async fn run_patch_test_script(
    context: &crate::app::Context,
    script_path: &std::path::Path,
    base: &Arc<package::install_info::InstallInfo>,
    scratch_root: &std::path::Path,
    platform: &oci::Platform,
    process_env: env::Env,
) -> anyhow::Result<ExitCode> {
    debug_assert!(
        matches!(
            tokio::runtime::Handle::current().runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        ),
        "run_script requires the multi-thread Tokio runtime (Handle::block_on in block_in_place)"
    );

    let source = tokio::fs::read_to_string(script_path).await.map_err(|e| {
        anyhow::Error::from(UsageError::new(format!(
            "cannot read script '{}': {e}",
            script_path.display()
        )))
    })?;
    let label = script_path.display().to_string();

    // Engine sandbox: a sub-directory of the scratch root, distinct from the
    // base package root so the script cannot mutate the materialized package.
    let engine_scratch = scratch_root.join("script-scratch");
    tokio::fs::create_dir_all(&engine_scratch)
        .await
        .map_err(|e| ocx_lib::error::file_error(&engine_scratch, e))?;
    let package_root = base.dir().root();

    let limits = ocx_lib::script::ScriptLimits {
        max_callstack_size: 50,
        wall_clock: std::time::Duration::from_secs(300),
    };

    let outcome_res = tokio::task::block_in_place(|| {
        ocx_lib::script::run_script(
            &source,
            &label,
            package_root,
            &engine_scratch,
            platform,
            process_env,
            limits,
        )
    });

    let outcome = match outcome_res {
        Ok(outcome) => outcome,
        Err(error) => {
            let report = crate::api::data::script_run::ScriptRunReport::new(
                crate::api::data::script_run::ScriptStatus::Failed,
                Some(crate::api::data::script_run::AssertionRecord {
                    kind: "other".to_string(),
                    message: format!("script host failure: {error}"),
                }),
                None,
            );
            context.api().report(&report)?;
            return Ok(cli::ExitCode::Failure.into());
        }
    };

    let run_summary = ocx_lib::script::last_run_summary();
    let report = crate::api::data::script_run::ScriptRunReport::from_outcome(&outcome, run_summary);
    context.api().report(&report)?;
    Ok(map_patch_test_outcome_to_exit_code(outcome).into())
}

/// Map a script [`ScriptOutcome`] to the process exit code.
///
/// Same mapping as `package_test.rs::map_outcome_to_exit_code`: a passing script
/// is `Success`, a failing assertion is `Failure`, and engine/usage/data/IO
/// outcomes map to their `sysexits`-aligned codes.
fn map_patch_test_outcome_to_exit_code(outcome: ocx_lib::script::ScriptOutcome) -> cli::ExitCode {
    use ocx_lib::script::ScriptOutcomeKind;
    match outcome.kind {
        ScriptOutcomeKind::Passed => cli::ExitCode::Success,
        ScriptOutcomeKind::Failed { .. } => cli::ExitCode::Failure,
        ScriptOutcomeKind::Usage { .. } => cli::ExitCode::UsageError,
        ScriptOutcomeKind::ScriptError { .. } => cli::ExitCode::DataError,
        ScriptOutcomeKind::Io { .. } => cli::ExitCode::IoError,
        ScriptOutcomeKind::Timeout => cli::ExitCode::Failure,
        _ => cli::ExitCode::Failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_lib::ResolvedPatchConfig;

    fn patches() -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            system_required: false,
            registry: "patches.corp.com".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        }
    }

    /// `--global` selects the reserved `global` repository descriptor.
    #[test]
    fn select_publish_target_global_is_reserved_global_repository() {
        let patches = patches();
        let target = select_publish_target(&patches, true, None);
        // Global descriptor: rooted at the patch registry, reserved single-segment
        // `global` repository, PATCH tag.
        assert_eq!(target.registry(), "patches.corp.com");
        assert_eq!(
            target.repository(),
            "global",
            "global target must use the reserved `global` repository; got '{}'",
            target.repository()
        );
        assert_eq!(
            target,
            global_descriptor_id(&patches),
            "global selection must equal global_descriptor_id"
        );
    }

    /// Without `--global`, a base identifier selects its package-specific
    /// sub-path target — NOT the reserved global repository.
    #[test]
    fn select_publish_target_base_is_package_specific_sub_path() {
        let patches = patches();
        let base = oci::Identifier::parse("ocx.sh/cmake:3.28").expect("valid identifier");
        let target = select_publish_target(&patches, false, Some(&base));
        assert_eq!(target.registry(), "patches.corp.com");
        assert!(
            target.repository().contains("cmake"),
            "package-specific target must embed the base repository 'cmake'; got '{}'",
            target.repository()
        );
        assert_eq!(
            target,
            patch_descriptor_id(&patches, &base),
            "per-base selection must equal patch_descriptor_id"
        );
        assert_ne!(
            target.repository(),
            global_descriptor_id(&patches).repository(),
            "package-specific target must differ from the global descriptor"
        );
    }
}
