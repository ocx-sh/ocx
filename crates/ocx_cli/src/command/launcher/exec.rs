// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Hidden `ocx launcher exec` subcommand — stable entry-point from generated launchers.
//!
//! Generated launchers call `ocx launcher exec '<pkg-root>' -- "$(basename "$0")" "$@"`. The two
//! subcommand names and that positional shape are the whole launcher ABI: installed launchers bake it.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::error::UsageError;
use clap::Parser;
use ocx_config::env;
use ocx_package::install_info::InstallInfo;
use ocx_package::launch::LaunchIdentities;
use ocx_package::metadata::Metadata;
use ocx_package::metadata::env::apply::{ChildEnv, EnvEntriesExt, forwarded_env, reconcile_list_separators};
use ocx_package::metadata::template::{TemplateResolver, Usage};
use ocx_package_manager::AdmittedClaims;
use ocx_package_manager::launch::{self, ExemptionReason, Launch};
use ocx_package_manager::record::{RecordInputs, Scope};
use ocx_store::file_structure::{PackageDir, read_digest_file};
use ocx_util::prelude::SerdeExt;

/// Entry point from generated launchers. Validates the package root, then
/// executes the resolved entrypoint with forced self-view and silent presentation.
#[derive(Parser)]
pub struct LauncherExec {
    /// Absolute path to the installed package root (the directory containing
    /// `metadata.json`). Baked into the launcher at install time.
    pkg_root: PathBuf,

    /// The launcher's own filename (argv0 passed after `--`), used to
    /// identify which entrypoint to dispatch.
    #[clap(last = true, required = true, num_args = 1..)]
    argv: Vec<String>,
}

impl LauncherExec {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let fs = context.file_structure();
        let [packages_root, package_test_root, patch_test_root] = launcher_pkg_root_allow_list(fs);
        // The scratch root the baked pkg-root sits under is the only exemption carrier that survives the
        // hop into this fresh process.
        let scratch_roots = [
            (package_test_root, ExemptionReason::PackageTest),
            (patch_test_root, ExemptionReason::PatchTest),
        ];
        let manager = context.manager();

        let (validated, exemption) = validate_launcher_pkg_root(&self.pkg_root, &packages_root, &scratch_roots).await?;
        // Folded before any work, so a malformed name template fails first as a config error; skipping
        // it on the exempt path would let a caller-supplied `$OCX_HOME/temp/…` pkg-root bypass a
        // `required = true` policy.
        let recording = Recording {
            exemption,
            policy: context.records(ocx_package_manager::record::RecordsOptions::default())?,
        };
        let package_dir = PackageDir::with_root(validated);

        // Read only under a `[patches]` tier, the one case a composing parent writes it. With no name
        // for this digest the launcher keeps a synthetic id, where only catch-all rules match.
        let identities = match context.config_view().patches {
            Some(_) => Some(
                LaunchIdentities::from_env()
                    .map_err(anyhow::Error::new)?
                    .unwrap_or_default(),
            ),
            None => None,
        };
        let names = match &identities {
            Some(identities) => {
                let digest = read_digest_file(&package_dir.digest_file())
                    .await
                    .map_err(ocx_package_manager::Error::from)?;
                let names = identities.identities_for(&digest);
                if names.is_empty() {
                    log::debug!("no launch identity forwarded for {digest}; only catch-all patch rules match");
                }
                names
            }
            None => Vec::new(),
        };
        // A launcher always composes its package's self view (public + private surface).
        let info = manager
            .install_info_from_package_root(package_dir.root(), &names)
            .await?;
        // Scoped to this re-entry, never grafted onto the global manager tier, so it cannot leak into
        // nested `ocx` commands. An exported environment carries its opt-out on the identities.
        let mut no_patches = ocx_config::patch::patches_from_env()
            .map_err(anyhow::Error::new)?
            .map(|forwarded| forwarded.no_patches)
            .unwrap_or_default();
        no_patches.extend(identities.iter().flat_map(LaunchIdentities::opted_out_repositories));
        // The parent's `[env]` and `--env` via `OCX_ENV`: without them the package's own entries would
        // silently revert the project's overrides. Fails closed on the whole payload, never one entry.
        let project_env = forwarded_env().map_err(anyhow::Error::new)?;
        let packages = [std::sync::Arc::new(info)];
        // The attribution-keeping variant: the record names who claimed each `PATH` executable and every
        // patch companion, and this call is the only place either exists.
        let (mut entries, _, _, admitted) = manager
            .resolve_env_with_attribution(
                &packages,
                true,
                ocx_package_manager::EnvScope::Project {
                    // Opt-out keys match the forwarded launch identities; a launch with none falls back
                    // to the synthetic id, where only `*` rules apply.
                    no_patches,
                    env: project_env.clone(),
                    // No link lane: a `<group>/<entry>` link could compose a different package than the
                    // digest-pinned one this trampoline was written for.
                    toolchain: None,
                },
                // The launcher runs on the host its package is materialized on.
                &ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any),
            )
            .await?;
        // As in `ocx exec`: without it the launcher folds a silently-chosen separator where `exec` exits
        // 65 for the same package.
        reconcile_list_separators(entries.iter_mut()).map_err(anyhow::Error::new)?;

        let (argv0, args) = self
            .argv
            .split_first()
            .expect("clap required=true guarantees at least one argv element");

        // Absent `command` leaves `argv0` unchanged, keeping resolve-name-on-PATH behaviour byte-for-byte.
        let metadata = Metadata::read_json(&package_dir.metadata()).await?;
        let command = metadata
            .entrypoints()
            .map_or(argv0.as_str(), |eps| eps.dispatch_command(argv0));

        let content_path = package_dir.content();
        let baked: &[String] = metadata
            .entrypoints()
            .and_then(|eps| eps.get(argv0))
            .map(|e| e.args())
            .unwrap_or(&[]);

        // One vector: the launch seam derives the child's args from the record's `argv`, so they cannot
        // disagree. Index 0 stays `argv0`, not `command`, so this record pairs with the outer one.
        let mut argv = Vec::with_capacity(1 + baked.len() + args.len());
        argv.push(argv0.clone());
        if !baked.is_empty() {
            // Empty dep contexts: the `Usage::EntryPointArgs` gate refuses any `${deps.*}` token before
            // substitution, which is what still refuses one in an already-published package.
            let dep_contexts = std::collections::HashMap::new();
            let resolver = TemplateResolver::new(&content_path, &dep_contexts).usage(Usage::EntryPointArgs);
            for baked_arg in baked {
                argv.push(resolver.resolve(baked_arg).map_err(|e| {
                    anyhow::Error::from(e).context(format!(
                        "failed to interpolate baked arg '{baked_arg}' for entrypoint '{argv0}'"
                    ))
                })?);
            }
        }
        argv.extend_from_slice(args);

        run_with_env(
            &context,
            Resolved {
                packages: &packages,
                admitted: &admitted,
            },
            ChildEnv {
                composed: &entries,
                forwarded: &project_env,
                identities: None,
            },
            command,
            &argv,
            recording,
        )
        .await
    }
}

/// What this frame's env composition resolved, as the record needs to name it.
///
/// Grouped so the launch seam cannot take slices from different resolutions.
struct Resolved<'a> {
    /// The package roots this frame composed — here, always exactly one.
    packages: &'a [std::sync::Arc<InstallInfo>],
    /// Which package claimed each executable name on `PATH`, and the patch companions overlaid.
    admitted: &'a AdmittedClaims,
}

/// What this launch records, decided once in [`LauncherExec::execute`].
///
/// The pkg-root only claims an exemption; the policy decides whether it is granted
/// ([`Launch::exempt`]).
struct Recording {
    /// Set when the pkg-root sits under a command-scratch root, so this launch
    /// claims that command's recording exclusion.
    exemption: Option<ExemptionReason>,
    /// The folded sink, filename template and fail posture for this frame.
    policy: ocx_package_manager::record::RecordingPolicy,
}

/// Run the resolved entrypoint with the given env.
///
/// Returns only when start-up fails or a `required` record could not be written: `launch::exec`
/// diverges on success. `command` is `argv[0]` remapped through the entrypoint table; the record
/// publishes both.
async fn run_with_env(
    context: &crate::app::Context,
    resolved: Resolved<'_>,
    child_env: ChildEnv<'_>,
    command: &str,
    argv: &[String],
    recording: Recording,
) -> anyhow::Result<ExitCode> {
    let Resolved { packages, admitted } = resolved;
    let mut process_env = env::Env::new();
    // Re-emits `OCX_ENV` after the package entries, or a nested launcher's package value beats the
    // project override at the second hop. No separator reconcile: the `OCX_ENV` decode gate refuses one.
    process_env.apply_child_env(child_env, context.config_view());
    // Resolved once for record and launch, or the record could name the wrong binary. A plain
    // `resolve_command` would let a host copy shadow the package's launcher. No PATHEXT edit: the
    // Windows launcher is a native `<name>.exe`.
    let executable = process_env.resolve_test_command(command)?;

    // A scratch pkg-root inherits its command's recording exclusion, since no collector can tell this
    // hop from a direct invocation. Placement alone forges the claim, so `required = true` refuses it.
    let Recording { exemption, policy } = recording;
    if let Some(reason) = exemption {
        let (_argv0, args) = argv
            .split_first()
            .expect("the caller pushes argv0 before any user argument");
        let launch = Launch::exempt(process_env, &executable, args, reason, &policy)?;
        return Err(anyhow::Error::from(launch::exec(launch).await));
    }

    let launch = Launch::recording(
        process_env,
        RecordInputs {
            packages,
            admitted,
            executable: &executable,
            store_root: context.file_structure().packages.root(),
            shim_root: context.file_structure().shims.root(),
            argv,
            config: context.config_view(),
            insecure_registries: context.insecure_hosts(),
            // Read and identity-gated once at `try_init`; no I/O on the exec path.
            managed_config_digest: context.managed_config_snapshot().map(|snapshot| &snapshot.digest),
            // Likewise read once at `try_init`, alongside the pins it describes.
            patch_snapshot_digest: context.patch_snapshot_digest(),
            // Handed a package root, not an identifier: an explicit null, never a guess from the host.
            platform: None,
            clean_env: false,
            auto_installed: &[],
            scope: Scope::Launcher,
        },
        &policy,
    )?;

    Err(anyhow::Error::from(launch::exec(launch).await))
}

/// Package roots a launcher's baked `pkg-root` may resolve inside: the install store and the
/// `ocx package test`/`ocx patch test` scratch roots. Explicit, since "anything under `temp/`"
/// would admit in-progress downloads.
///
/// `ocx_shim::core::pkg_root_allowed` restates these roots and only a test on each side binds
/// them: change both together.
fn launcher_pkg_root_allow_list(fs: &ocx_store::file_structure::FileStructure) -> [PathBuf; 3] {
    [
        fs.packages.root().to_path_buf(),
        fs.temp.package_test_root(),
        fs.temp.patch_test_root(),
    ]
}

/// Validate a launcher's package root: absolute, canonically inside `packages_root` or one of
/// `extra_roots`, and holding `metadata.json`.
///
/// Returns the matched scratch root's exemption: a re-entry cannot learn its spawning command
/// any other way.
async fn validate_launcher_pkg_root(
    dir: &std::path::Path,
    packages_root: &std::path::Path,
    extra_roots: &[(PathBuf, ExemptionReason)],
) -> Result<(PathBuf, Option<ExemptionReason>), UsageError> {
    if !dir.is_absolute() {
        return Err(UsageError::new(format!(
            "launcher exec: pkg-root must be absolute, got '{}'",
            dir.display()
        )));
    }

    // Canonicalized so symlinks and `..` cannot smuggle a path outside the allowed roots.
    let canonical_dir = tokio::fs::canonicalize(dir).await.map_err(|e| {
        UsageError::new(format!(
            "launcher exec: pkg-root '{}' cannot be resolved: {e}",
            dir.display()
        ))
    })?;

    // An absent store (fresh `OCX_HOME`) matches nothing, so `.ok()` keeps the boundary.
    let canonical_root = tokio::fs::canonicalize(packages_root).await.ok();

    // Scratch roots are created lazily; one that does not exist yet is skipped.
    let mut exemption = None;
    for (extra, reason) in extra_roots {
        if let Ok(canonical_extra) = tokio::fs::canonicalize(extra).await
            && canonical_dir.starts_with(&canonical_extra)
        {
            exemption = Some(*reason);
            break;
        }
    }

    let under_packages = canonical_root.as_ref().is_some_and(|r| canonical_dir.starts_with(r));

    if !under_packages && exemption.is_none() {
        let root_display = canonical_root.as_deref().unwrap_or(packages_root).display();
        return Err(UsageError::new(format!(
            "launcher exec: pkg-root must point inside {} (got {})",
            root_display,
            canonical_dir.display()
        )));
    }

    let metadata = canonical_dir.join("metadata.json");
    if !tokio::fs::try_exists(&metadata).await.unwrap_or(false) {
        return Err(UsageError::new(format!(
            "launcher exec: pkg-root is not a package root (missing metadata.json): {}",
            canonical_dir.display()
        )));
    }

    Ok((canonical_dir, exemption))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a directory tree `base/subdir/` with a `metadata.json`
    /// inside `subdir/` and return `(base, subdir)`.
    fn make_pkg_tree(tmp: &std::path::Path, base: &str, pkg: &str) -> (PathBuf, PathBuf) {
        let base_dir = tmp.join(base);
        let pkg_dir = base_dir.join(pkg);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("metadata.json"), b"{}").unwrap();
        (base_dir, pkg_dir)
    }

    // ── C-018: the pkg-root allow-list, bound across both crates ─────────────

    /// The CLI half of the allow-list golden.
    ///
    /// `ocx_lib` cannot depend on `ocx_shim` (a binary crate), so there is **no
    /// compiler link** between the two producers of this allow-list. Its peer
    /// is `ocx_shim::core::pkg_root_allowed` and the test that restates the
    /// same three roots from the shim side,
    /// `crates/ocx_shim/src/main.rs::pkg_root_allowed_matches_the_cli_allow_list`.
    /// Together they are the entire binding: a root added, removed or renamed
    /// on either side reds the other side's literal.
    ///
    /// The rows below exercise `validate_launcher_pkg_root` behaviourally over
    /// tempdirs and restate nothing, so before this test a fourth root added
    /// here reddened nothing on the shim side — a one-sided binding, which is
    /// exactly the doc-comment-only sync gap C-018 exists to close.
    ///
    /// Restating the literals rather than importing a shared constant is
    /// forced (no dependency edge) and is also the point: a shared constant
    /// would make the canary self-referential.
    #[test]
    fn launcher_pkg_root_allow_list_matches_the_shim_allow_list() {
        let home = tempfile::tempdir().unwrap();
        let fs = ocx_store::file_structure::FileStructure::with_root(home.path().to_path_buf());

        let expected: Vec<PathBuf> = ["packages", "temp/test", "temp/patch-test"]
            .iter()
            .map(|relative| home.path().join(relative))
            .collect();

        assert_eq!(
            launcher_pkg_root_allow_list(&fs).to_vec(),
            expected,
            "the launcher pkg-root allow-list is exactly $OCX_HOME/{{packages, \
             temp/test, temp/patch-test}}. `ocx_shim::core::pkg_root_allowed` \
             restates the same three; changing one side without the other \
             silently desynchronises the shim's E3 containment check from the \
             roots `launcher exec` actually accepts."
        );
    }

    // ── validate_launcher_pkg_root — key contract rows ───────────────────────

    /// The two command-scratch roots the real call site allow-lists, rooted at
    /// `tmp`, each paired with the exemption that root's command owns. Mirrors
    /// `TempStore::package_test_root` / `patch_test_root`.
    fn scratch_roots(tmp: &std::path::Path) -> [(PathBuf, ExemptionReason); 2] {
        [
            (tmp.join("temp/test"), ExemptionReason::PackageTest),
            (tmp.join("temp/patch-test"), ExemptionReason::PatchTest),
        ]
    }

    /// Pkg root inside packages_root → accepted (normal install path), and it
    /// records: an installed package is published, so nothing exempts it.
    #[tokio::test]
    async fn accepts_path_under_packages_root() {
        let tmp = tempfile::tempdir().unwrap();
        let (packages_root, pkg_dir) = make_pkg_tree(tmp.path(), "packages", "abc123");
        let result = validate_launcher_pkg_root(&pkg_dir, &packages_root, &scratch_roots(tmp.path())).await;
        assert!(
            matches!(result, Ok((_, None))),
            "expected Ok with no exemption; got {result:?}"
        );
    }

    /// Pkg root inside the package-test scratch root (temp/test/) → accepted.
    #[tokio::test]
    async fn accepts_path_under_extra_root() {
        let tmp = tempfile::tempdir().unwrap();
        let (temp_test_root, pkg_dir) = make_pkg_tree(tmp.path(), "temp/test", "test-XXXXX");
        // packages_root is a separate sibling that does NOT contain pkg_dir.
        let packages_root = tmp.path().join("packages");
        std::fs::create_dir_all(&packages_root).unwrap();
        let result = validate_launcher_pkg_root(
            &pkg_dir,
            &packages_root,
            &[(temp_test_root, ExemptionReason::PackageTest)],
        )
        .await;
        assert!(result.is_ok(), "expected Ok for temp/test path; got {result:?}");
    }

    /// A launcher baked with a scratch pkg-root inherits its command's
    /// exemption, and the two roots stay distinguishable.
    ///
    /// This is the only carrier that survives the launcher hop: `ocx package
    /// test` declares its exclusion at its own spawn site, but a package
    /// declaring an entrypoint re-enters `ocx launcher exec` as a fresh process
    /// that re-reads `[records]` from its own config chain. Without this, a
    /// maintainer preview writes a synthetic record into the operator's audit
    /// sink that no collector can filter out.
    #[tokio::test]
    async fn a_scratch_pkg_root_carries_its_command_exemption() {
        let tmp = tempfile::tempdir().unwrap();
        let packages_root = tmp.path().join("packages");
        std::fs::create_dir_all(&packages_root).unwrap();

        let (_, package_test_pkg) = make_pkg_tree(tmp.path(), "temp/test", "test-XXXXX");
        let (_, patch_test_pkg) = make_pkg_tree(tmp.path(), "temp/patch-test", "patch-test-XXXXX/packages/pkg");

        for (pkg_dir, expected) in [
            (package_test_pkg, ExemptionReason::PackageTest),
            (patch_test_pkg, ExemptionReason::PatchTest),
        ] {
            let (_, exemption) = validate_launcher_pkg_root(&pkg_dir, &packages_root, &scratch_roots(tmp.path()))
                .await
                .expect("a scratch pkg-root is accepted");
            assert_eq!(
                exemption,
                Some(expected),
                "{} must carry {expected:?}",
                pkg_dir.display()
            );
        }
    }

    /// Regression: pkg root inside the `ocx patch test` scratch root
    /// (temp/patch-test/) → accepted. That command composes into its own scratch
    /// root, so allow-listing only temp/test rejected every `ocx patch test`
    /// invocation naming a generated entrypoint with exit 64, before `--env` was
    /// ever consulted. The path here matches what `patch test` actually bakes:
    /// the scratch `FileStructure` puts packages under `<scratch>/packages/`.
    #[tokio::test]
    async fn accepts_path_under_patch_test_scratch_root() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, pkg_dir) = make_pkg_tree(tmp.path(), "temp/patch-test", "patch-test-XXXXX/packages/pkg");
        let packages_root = tmp.path().join("packages");
        std::fs::create_dir_all(&packages_root).unwrap();
        let result = validate_launcher_pkg_root(&pkg_dir, &packages_root, &scratch_roots(tmp.path())).await;
        assert!(result.is_ok(), "expected Ok for temp/patch-test path; got {result:?}");
    }

    /// Security boundary: the allow-list stays an enumeration of the two known
    /// scratch roots. `temp/` itself and an unrelated `temp/other` are NOT
    /// accepted — a guard widened to "anything under temp/" would admit the
    /// in-progress download directories that share that root.
    #[tokio::test]
    async fn rejects_temp_root_itself_and_unrelated_temp_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let packages_root = tmp.path().join("packages");
        std::fs::create_dir_all(&packages_root).unwrap();
        for (root, _) in scratch_roots(tmp.path()) {
            std::fs::create_dir_all(&root).unwrap();
        }

        // A package sitting directly in temp/ ...
        let (_, in_temp_root) = make_pkg_tree(tmp.path(), "temp", "loose-pkg");
        let result = validate_launcher_pkg_root(&in_temp_root, &packages_root, &scratch_roots(tmp.path())).await;
        assert!(result.is_err(), "temp/ itself must not be allow-listed; got Ok");

        // ... and one under an unrelated temp sibling.
        let (_, in_other) = make_pkg_tree(tmp.path(), "temp/other", "pkg");
        let result = validate_launcher_pkg_root(&in_other, &packages_root, &scratch_roots(tmp.path())).await;
        assert!(result.is_err(), "temp/other must not be allow-listed; got Ok");
    }

    /// Regression: pkg root inside extra_root when packages_root does NOT EXIST
    /// (fresh OCX_HOME, no packages ever installed). This was the actual bug:
    /// `canonicalize(packages_root)` hard-failed with ENOENT before the
    /// extra_root check was reached, causing exit 64 on every `ocx package test`
    /// invocation on a clean host for packages with entrypoint launchers.
    #[tokio::test]
    async fn accepts_path_under_extra_root_when_packages_root_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let (temp_test_root, pkg_dir) = make_pkg_tree(tmp.path(), "temp/test", "test-ABCDE");
        // packages_root intentionally NOT created — simulates fresh OCX_HOME
        // where no `ocx install` has ever been run.
        let packages_root = tmp.path().join("packages");
        assert!(!packages_root.exists(), "test setup: packages_root must be absent");
        let result = validate_launcher_pkg_root(
            &pkg_dir,
            &packages_root,
            &[(temp_test_root, ExemptionReason::PackageTest)],
        )
        .await;
        assert!(
            result.is_ok(),
            "expected Ok for temp/test path with absent packages_root; got {result:?}"
        );
    }

    /// Security boundary: path outside both allowed roots is rejected even when
    /// packages_root does not exist (absent packages_root must not widen the
    /// accepted set to "anything").
    #[tokio::test]
    async fn rejects_outside_path_when_packages_root_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let temp_test_root = tmp.path().join("temp/test");
        std::fs::create_dir_all(&temp_test_root).unwrap();
        // packages_root intentionally absent.
        let packages_root = tmp.path().join("packages");

        let outsider_dir = tmp.path().join("outsider/pkg");
        std::fs::create_dir_all(&outsider_dir).unwrap();
        std::fs::write(outsider_dir.join("metadata.json"), b"{}").unwrap();

        let result = validate_launcher_pkg_root(
            &outsider_dir,
            &packages_root,
            &[(temp_test_root, ExemptionReason::PackageTest)],
        )
        .await;
        assert!(
            result.is_err(),
            "expected Err for outsider with absent packages_root; got Ok"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("pkg-root must point inside"),
            "unexpected error message: {msg}"
        );
    }

    /// Pkg root outside both allowed roots → rejected.
    #[tokio::test]
    async fn rejects_path_outside_both_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let packages_root = tmp.path().join("packages");
        std::fs::create_dir_all(&packages_root).unwrap();
        let temp_test_root = tmp.path().join("temp/test");
        std::fs::create_dir_all(&temp_test_root).unwrap();

        // outsider is a sibling of both allowed roots.
        let outsider_dir = tmp.path().join("outsider/pkg");
        std::fs::create_dir_all(&outsider_dir).unwrap();
        std::fs::write(outsider_dir.join("metadata.json"), b"{}").unwrap();

        let result = validate_launcher_pkg_root(
            &outsider_dir,
            &packages_root,
            &[(temp_test_root, ExemptionReason::PackageTest)],
        )
        .await;
        assert!(result.is_err(), "expected Err for outsider path; got Ok");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("pkg-root must point inside"),
            "unexpected error message: {msg}"
        );
    }

    /// Non-absolute path → rejected with appropriate message.
    #[tokio::test]
    async fn rejects_relative_path() {
        let result = validate_launcher_pkg_root(
            std::path::Path::new("relative/path"),
            std::path::Path::new("/packages"),
            &[],
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be absolute"));
    }

    /// Path pointing to a directory that exists but has no metadata.json → rejected.
    #[tokio::test]
    async fn rejects_missing_metadata_json() {
        let tmp = tempfile::tempdir().unwrap();
        let packages_root = tmp.path().join("packages");
        let pkg_dir = packages_root.join("no-meta");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        // metadata.json intentionally absent.
        let result = validate_launcher_pkg_root(&pkg_dir, &packages_root, &[]).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("missing metadata.json"),
            "wrong rejection reason"
        );
    }
}
