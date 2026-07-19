// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx patch test` — compose a patch descriptor onto a base, without publishing.
//!
//! Reads a descriptor JSON file and composes its matched companions onto the
//! given base identifier in a scratch store, then either runs a Starlark test
//! script, runs a trailing command in the composed environment, or prints the
//! composed environment. Lets a maintainer verify a descriptor before publishing
//! it. Required companion packages must be resolvable (installed locally or
//! pulled from the registry); an unresolvable required companion fails.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Args;
use ocx_lib::utility::child_process;
use ocx_lib::{cli::UsageError, env, oci, package, publisher::LayerRef};

use crate::{conventions, options};

/// Arguments for `ocx patch test`.
#[derive(Args)]
pub struct PatchTestArgs {
    /// Path to the patch descriptor JSON file to compose.
    #[clap(long = "descriptor", required = true)]
    descriptor: PathBuf,

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

    /// Patch registry to compose against, as `HOST/PATH`
    /// (e.g. registry.corp.example/ocx-patches).
    ///
    /// Overrides the configured `[patches]` tier for this command, so you can
    /// preview a descriptor against a new patch registry without first adding a
    /// `[patches]` config block. Defaults to the configured registry.
    #[clap(long = "registry", value_name = "HOST/PATH")]
    registry: Option<String>,

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

impl PatchTestArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        run_patch_test(self, context).await
    }
}

/// Build a scratch `PackageManager` over a temporary `FileStructure` that shares
/// the running `Context`'s remote sources (index + client) and patch tier.
///
/// Companion pulls and seeded descriptor blobs land in the scratch CAS so the
/// real `$OCX_HOME`'s package/blob store is never mutated by `ocx patch test`.
/// The local index is a separate, self-contained store outside `FileStructure`
/// (`adr_index_indirection.md` Decision A) and is deliberately NOT
/// scratch-isolated: it reuses `context.local_index()`, so a companion pull
/// benefits from (and grows) the same resolved index home every other
/// command shares.
fn build_scratch_manager(
    context: &crate::app::Context,
    scratch_root: &std::path::Path,
    patches: &ocx_lib::ResolvedPatchConfig,
) -> ocx_lib::package_manager::PackageManager {
    use ocx_lib::oci::index::{ChainMode, Index};

    let file_structure = ocx_lib::file_structure::FileStructure::with_root(scratch_root.to_path_buf());
    // Reuse the running context's already-resolved local index — same home
    // precedence `Context::try_init` applies (`--index` ▸ `OCX_INDEX` ▸
    // `$OCX_HOME/index`) — instead of constructing a fresh one bound to the
    // scratch root's own `index/` subdir. The scratch-bound construction
    // always started empty and silently ignored any `--index`/`OCX_INDEX`
    // override the rest of this same invocation honours.
    let local_index = context.local_index().clone();

    // Share the running context's remote sources so companions can be pulled
    // into the scratch CAS. Offline → no source (the scratch manager is offline
    // too, and required-companion resolution fails closed).
    let (mode, sources, client): (ChainMode, Vec<Index>, Option<oci::Client>) = match context.oci_index() {
        Ok(remote) => (
            ChainMode::Default,
            vec![Index::from_remote(remote.clone())],
            context.remote_client().ok().cloned(),
        ),
        Err(_) => (ChainMode::Offline, Vec::new(), None),
    };
    let index = Index::from_chained_with_content_store(local_index, sources, mode, file_structure.blobs.clone());

    ocx_lib::package_manager::PackageManager::new(file_structure, index, client, context.default_registry())
        .with_patches(Some(patches.clone()))
        // Route the guaranteed-local companion / site-patch lookups
        // (`effective_index_store`) through the SAME index home the reused
        // `local_index` (and thus `pull`) writes tag pointers to. Without this the
        // manager falls back to the scratch root's empty `index/`, so a
        // registry-pulled companion's tag pointer — committed to the context's
        // real home — is invisible to `find_companion_local`, which then reports
        // the required companion as not found (exit 79).
        .with_index(context.local_index().index_store().clone())
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
    // ── Step 0: Resolve the effective patch tier (honours --registry). ──
    let patches = crate::command::patch_common::effective_patches(args.registry.as_deref(), &context)?;

    let platform = args
        .platform
        .clone()
        .unwrap_or_else(|| oci::Platform::current().unwrap_or_else(oci::Platform::any));
    let base_id = args.base.with_domain(context.default_registry())?;

    // ── Step 1: Read + validate the descriptor file. ──
    let descriptor_bytes = tokio::fs::read(&args.descriptor)
        .await
        .with_context(|| format!("reading descriptor file {}", args.descriptor.display()))?;
    let descriptor = ocx_lib::patch::PatchDescriptor::from_json_bytes(&descriptor_bytes)
        .with_context(|| format!("validating patch descriptor file {}", args.descriptor.display()))?;

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

    let manager = build_scratch_manager(&context, &scratch_root, &patches);

    // ── Step 3: Materialize the base into the scratch store. ──
    //
    // `pull` materializes the base (and its transitive deps) WITHOUT triggering
    // patch discovery — discovery would hit the real patch registry and clobber
    // the seeded descriptor. The maintainer is testing online, so the base is
    // pulled from the shared registry source; offline → fail-closed.
    let base_info = manager
        .pull(&base_id, platform.clone())
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
        // Read the script source and provision the engine sandbox, then delegate
        // to the shared script runner (also used by `ocx package test`).
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
        let package_root = base_arc.dir().root();

        crate::command::script_runner::run_script_in_env(
            &context,
            &source,
            &label,
            package_root,
            &engine_scratch,
            &platform,
            process_env,
        )
        .await
    } else if !args.command.is_empty() {
        let (command, command_args) = args.command.split_first().expect("non-empty command checked above");
        let resolved = process_env.resolve_command(command);
        let status = child_process::spawn_and_wait(&resolved, command_args, process_env)
            .await
            .map_err(|e| anyhow::Error::from(e).context(format!("failed to run '{}'", resolved.display())))?;
        Ok(child_process::propagate_exit_code(status))
    } else {
        // Print the composed companion env entries, annotating each overlay entry
        // (index `>= patch_start`) with its provenance — the rule glob + companion
        // that produced it — so the maintainer can trace every patched var.
        let companion_strings: Vec<String> = composition.matched_companions.iter().map(ToString::to_string).collect();
        let patch_start = composition.patch_start;
        let entries: Vec<crate::api::data::patch_test::PatchTestEntry> = composition
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let source = if i >= patch_start {
                    let provenance = &composition.provenance[i - patch_start];
                    Some(crate::api::data::env::EntrySource::Patch {
                        rule: provenance.rule_match.clone(),
                        companion: provenance.companion.to_string(),
                    })
                } else {
                    None
                };
                crate::api::data::patch_test::PatchTestEntry {
                    key: entry.key.clone(),
                    value: entry.value.clone(),
                    kind: entry.kind.clone(),
                    source,
                }
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
        let layer = LayerRef::File {
            path: archive.clone(),
            layout: oci::LayerLayoutSpec::default(),
        };
        let metadata_path = conventions::resolve_metadata_path(std::slice::from_ref(&layer), None)
            .with_context(|| format!("locating metadata sibling for companion archive {}", archive.display()))?;
        // Read the metadata sibling once; parse both the `Metadata` struct and
        // the maintainer-authored `identifier` field from the same bytes.
        let metadata_bytes = tokio::fs::read(&metadata_path)
            .await
            .map_err(|error| ocx_lib::error::file_error(&metadata_path, error))
            .with_context(|| format!("reading companion metadata from {}", metadata_path.display()))?;
        let metadata = package::metadata::ValidMetadata::try_from(
            serde_json::from_slice::<package::metadata::Metadata>(&metadata_bytes)
                .with_context(|| format!("parsing companion metadata from {}", metadata_path.display()))?,
        )?;
        let identifier = ocx_lib::oci::Identifier::parse_with_default_registry(
            metadata_identifier_or_error(&metadata_path, &metadata_bytes)?.as_str(),
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
            .pull(&companion.identifier, platform.clone())
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

/// Extract the `identifier` field a maintainer set in a companion metadata file.
///
/// The package-test convention requires the identifier on the CLI; for patch
/// test the companion archive carries it in its metadata sibling under an
/// `identifier` key. Absent → a clear usage error. Takes the already-read
/// metadata bytes so the sibling is read only once per companion.
fn metadata_identifier_or_error(metadata_path: &std::path::Path, metadata_bytes: &[u8]) -> anyhow::Result<String> {
    let value: serde_json::Value = serde_json::from_slice(metadata_bytes)
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- Clap surface: --descriptor rename (C9) ---

    /// `--descriptor` parses and is threaded through to the args struct.
    #[test]
    fn descriptor_flag_parses() {
        use clap::Parser as _;

        let cli = crate::app::Cli::try_parse_from(["ocx", "patch", "test", "--descriptor", "p.json", "cmake:1.0"])
            .expect("--descriptor must parse");
        let Some(crate::command::Command::Patch(crate::command::patch::PatchGroup::Test(args))) = cli.command else {
            panic!("expected Patch(Test(..)) subcommand");
        };
        assert_eq!(args.descriptor, PathBuf::from("p.json"));
    }

    /// `--registry` overrides the target patch registry and threads through to
    /// the args struct — the ad-hoc bootstrap/preview path.
    #[test]
    fn registry_flag_parses() {
        use clap::Parser as _;

        let cli = crate::app::Cli::try_parse_from([
            "ocx",
            "patch",
            "test",
            "--descriptor",
            "p.json",
            "--registry",
            "registry.corp.example/ocx-patches",
            "cmake:1.0",
        ])
        .expect("--registry must parse");
        let Some(crate::command::Command::Patch(crate::command::patch::PatchGroup::Test(args))) = cli.command else {
            panic!("expected Patch(Test(..)) subcommand");
        };
        assert_eq!(args.registry.as_deref(), Some("registry.corp.example/ocx-patches"));
    }

    /// `--descriptor-file` is the OLD flag name — it must be an unknown flag
    /// now that `patch test` uses `--descriptor` (C9).
    #[test]
    fn descriptor_file_flag_is_rejected() {
        use clap::Parser as _;

        let result =
            crate::app::Cli::try_parse_from(["ocx", "patch", "test", "--descriptor-file", "p.json", "cmake:1.0"]);
        assert!(
            result.is_err(),
            "--descriptor-file must be rejected; the flag was renamed to --descriptor"
        );
    }

    /// `--companion-archive` is unchanged by the C7/C9 rename (C10) — still parses.
    #[test]
    fn companion_archive_flag_still_parses() {
        use clap::Parser as _;

        crate::app::Cli::try_parse_from([
            "ocx",
            "patch",
            "test",
            "--descriptor",
            "p.json",
            "--companion-archive",
            "companion.tar.gz",
            "cmake:1.0",
        ])
        .expect("--companion-archive must still parse unchanged");
    }
}
