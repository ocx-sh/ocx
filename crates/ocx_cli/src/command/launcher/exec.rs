// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Hidden `ocx launcher exec` subcommand — stable entry-point from generated launchers.
//!
//! Generated launcher scripts call:
//!   `ocx launcher exec '<pkg-root>' -- "$(basename "$0")" "$@"`
//!
//! This subcommand is the sole path from an installed launcher into the OCX runtime.
//! It hides all presentation flags, self-view selection, and binary pinning behind
//! the stable `launcher exec` name pair, reducing the launcher ABI surface from
//! 8 wire commitments to 2 (the `launcher` + `exec` subcommand names and positional shape).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli::UsageError;
use ocx_lib::env;
use ocx_lib::file_structure::PackageDir;
use ocx_lib::launch::{self, ExemptionReason, Launch};
use ocx_lib::package::install_info::InstallInfo;
use ocx_lib::package::metadata::Metadata;
use ocx_lib::package::metadata::template::{TemplateResolver, Usage};
use ocx_lib::package_manager::AdmittedBinaries;
use ocx_lib::prelude::SerdeExt;
use ocx_lib::record::{RecordInputs, Scope};

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
        let packages_root = fs.packages.root();
        // Also allow launchers materialised under the two command-scratch roots:
        // `ocx package test` ($OCX_HOME/temp/test/) and `ocx patch test`
        // ($OCX_HOME/temp/patch-test/) both place packages there, and the
        // launchers bake that path as their pkg-root. An explicit two-entry
        // allow-list on purpose — widening the guard to "anything under temp/"
        // would defeat it, since temp/ also holds in-progress download dirs.
        //
        // Each root is paired with the recording exemption of the command that
        // owns it, and that pairing is the whole mechanism: those commands
        // declare `Launch::exempt` at their own spawn sites, but a package that
        // declares an entrypoint re-enters HERE through a generated launcher,
        // and this is a fresh process that reads `[records]` from its own config
        // chain — so not forwarding the sink would not stop it recording. The
        // exemption has to be inherited from the pkg-root the launcher was baked
        // with, which is the one carrier that survives the hop. The roots stay
        // separate entries so the two reasons stay distinguishable.
        let scratch_roots = [
            (fs.temp.package_test_root(), ExemptionReason::PackageTest),
            (fs.temp.patch_test_root(), ExemptionReason::PatchTest),
        ];
        let manager = context.manager();

        // Validate: pkg_root must be absolute, under $OCX_HOME/packages/ or one
        // of the scratch roots above, and contain metadata.json. Errors surface
        // as UsageError (exit 64). A matched scratch root also names the
        // exemption this launch inherits.
        let (validated, exemption) = validate_launcher_pkg_root(&self.pkg_root, packages_root, &scratch_roots).await?;
        // Fold `[records]` before the metadata load and the env composition
        // below — the same point its two siblings (`ocx run`, `ocx package
        // exec`) fold at, and for the same reason: a malformed name template is
        // a configuration error, and the operator should hear about it before
        // the work rather than after. This frame has no flag tier — `launcher
        // exec` is a wire ABI with two positionals and no options — so the sink
        // comes from the config and the environment the outer frame forwarded.
        //
        // The fold runs on the exempt path too, and that is load-bearing: the
        // exemption is granted on the strength of a caller-supplied pkg-root
        // sitting under `$OCX_HOME/temp/…`, which is inside the invoking user's
        // own home. Skipping the fold meant an operator's `required = true`
        // policy was never even consulted for a frame that any caller could put
        // itself into. Folding it lets `Launch::exempt` refuse.
        let recording = Recording {
            exemption,
            policy: context.records(ocx_lib::record::RecordsOptions::default())?,
        };
        // Wrap the validated package root in a PackageDir so every per-package
        // path (`content/`, `metadata.json`, ...) comes from the file-structure
        // layout accessors — the single source of truth for the package layout —
        // instead of hand-rolled `.join("content")` / `.join("metadata.json")`.
        let package_dir = PackageDir::with_root(validated);

        // Resolve env with self_view=true — the launcher always runs in the
        // package's own env (public + private surface). This is equivalent to
        // the former `--self` flag that was baked into every launcher template.
        let info = manager.install_info_from_package_root(package_dir.root()).await?;
        // Thread the project `no-patches` opt-out forwarded over `OCX_PATCHES`
        // into env resolution. `ocx run` injects the project opt-out into the
        // forwarded patch tier; here — the launcher re-entry — we decode that
        // opt-out DIRECTLY from the env (the same decoder `Context` uses for the
        // tier). Decoding at the consumption site keeps the opt-out scoped to
        // THIS launcher re-entry: it is never grafted onto the global manager
        // tier, so it cannot leak as ambient inherited state into unrelated
        // nested `ocx` commands (which each compute their own opt-out).
        //
        // AF1: `install_info_from_package_root` mints a synthetic
        // `file-url-mode/<content-digest>` identifier here — packages are
        // content-shared and carry no root registry/repository (see
        // `ResolvedPackage`) — so a repo-key never matches this base. `ocx run`
        // additionally forwards each opted-out base's content digest, and the
        // resolver's opt-out check (`resolve.rs`) matches on repo-key OR digest,
        // so the digest leg is what suppresses a re-injected companion here.
        //
        // A DIRECT launcher invocation (`ocx package exec` → launcher) has no
        // forwarded `OCX_PATCHES` → `patches_from_env()` is `None` → `Project(empty)`,
        // byte-identical to the former project-free scope. A system-required
        // tier still overlays regardless (the resolver enforces C7).
        let no_patches = ocx_lib::patches_from_env()
            .map_err(anyhow::Error::new)?
            .map(|forwarded| forwarded.no_patches)
            .unwrap_or_default();
        // Decode the project/group `[env]` (+ `ocx run --env`) the parent
        // composed, forwarded over `OCX_ENV` for the same reason as the
        // `no-patches` opt-out: this process has no `ProjectConfig` and cannot
        // re-derive them. Without the forward, `Env::new()` below inherits the
        // parent's values and then `apply_entries` re-applies the package's own
        // entries ON TOP, silently reverting the project's overrides — the
        // failure R1 exists to close, and the primary path for any package that
        // declares entrypoints.
        //
        // Untrusted input: `forwarded_env` fails closed on the whole payload
        // (reserved key, invalid key, unrecognized modifier kind), never
        // filtering the bad entry and keeping the rest. A direct launcher
        // invocation with no `ocx run` parent has no payload and gets an empty
        // vector — identical to the pre-forwarding behaviour.
        let project_env = ocx_lib::env::forwarded_env().map_err(anyhow::Error::new)?;
        // `info` is retained rather than moved into the `resolve_env` call: it is
        // the only package this frame resolved, and the record names it.
        let packages = [std::sync::Arc::new(info)];
        // `resolve_env_with_attribution` rather than the attribution-dropping
        // wrapper: the record names which package claimed each executable on
        // `PATH`, and that derivation already exists here.
        let (entries, _, _, admitted) = manager
            .resolve_env_with_attribution(
                &packages,
                true,
                ocx_lib::package_manager::EnvScope::Project {
                    no_patches,
                    env: project_env.clone(),
                },
            )
            .await?;

        // argv[0] is the launcher's own filename — the invocable entrypoint
        // name. argv[1..] are the user args.
        let (argv0, args) = self
            .argv
            .split_first()
            .expect("clap required=true guarantees at least one argv element");

        // Map the invocable name to its dispatch command. Absent `command`
        // (the common case) leaves `argv0` unchanged, so packages that do not
        // declare a divergent command keep the existing resolve-name-on-PATH
        // behaviour byte-for-byte.
        let metadata = Metadata::read_json(&package_dir.metadata()).await?;
        let command = metadata
            .entrypoints()
            .map_or(argv0.as_str(), |eps| eps.dispatch_command(argv0));

        // Resolve baked args (if any declared for this entrypoint) and prepend
        // them before user-supplied args. The content path comes from the
        // PackageDir layout accessor, not a hand-rolled join.
        let content_path = package_dir.content();
        let baked: &[String] = metadata
            .entrypoints()
            .and_then(|eps| eps.get(argv0))
            .map(|e| e.args())
            .unwrap_or(&[]);

        // `argv` as invoked: the launcher's own filename, then what the child
        // receives — baked args first, then the user's. Built as one vector
        // because the launch seam derives the child's arguments from the record's
        // own `argv`, so the two cannot disagree.
        //
        // Index 0 is `argv0`, not `command`: the dispatch remap is internal, and
        // keeping the invoked name here is what lets the outer `ocx run` record
        // and this re-entry's record read as a pair. `process.executable` names
        // what actually ran.
        let mut argv = Vec::with_capacity(1 + baked.len() + args.len());
        argv.push(argv0.clone());
        if !baked.is_empty() {
            // ADR D6: pass an empty dep_contexts map — the Usage::EntryPointArgs
            // capability gate rejects any ${deps.*} token before the dep regex
            // fires. The gate is the safety mechanism; publish-time
            // validate_entrypoint_args already rejected such args before publish.
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
            &packages,
            &admitted,
            env::ChildEnv {
                composed: &entries,
                forwarded: &project_env,
            },
            command,
            &argv,
            recording,
        )
        .await
    }
}

/// What this launch records, decided once in [`LauncherExec::execute`].
///
/// One value rather than two parameters that could disagree. It carries both
/// halves because they are no longer exclusive: an exemption is *claimed* from
/// the pkg-root, but whether it is granted is the resolved policy's call — a
/// fail-closed posture grants none (see [`Launch::exempt`]).
struct Recording {
    /// Set when the pkg-root sits under a command-scratch root, so this launch
    /// claims that command's recording exclusion.
    exemption: Option<ExemptionReason>,
    /// The folded sink, filename template and fail posture for this frame.
    policy: ocx_lib::record::RecordingPolicy,
}

/// Run the resolved entrypoint with the given env.
///
/// `child_env` pairs the composed entries with the validated payload decoded
/// from `OCX_ENV` — the project/group `[env]` (+ `ocx run --env`) already folded
/// into the composed entries as stages 4-6. The payload is re-emitted onto the
/// child env so a *nested* launcher (an entrypoint that itself invokes a
/// generated launcher) can re-apply it after its own package entries, instead of
/// letting a package value beat the project override at the second hop.
///
/// Presentation flags are forced here (not baked in the launcher template):
/// - log_level=off, color=never, format=plain were previously baked into the
///   launcher script; now they are applied on the *inner* ocx invocation from
///   within this subcommand (i.e. if this subcommand itself spawns child ocx,
///   which it does not — it execs the entrypoint binary directly).
///
/// `launch::exec` diverges on success on every platform — Unix `execvp(2)`s,
/// Windows spawns + waits + `process::exit`s — so this function only returns
/// when start-up itself fails, or when a record the policy marked `required`
/// could not be written.
/// `command` is the dispatch command to resolve on `PATH`, which is `argv[0]`
/// remapped through the package's entrypoint table; `argv` is the invocation as
/// the launcher received it. They differ only for a package that declares a
/// divergent `command`, and the record publishes both.
///
/// `recording` was decided by the caller, before any of this frame's work, so a
/// malformed forwarded `OCX_RECORDS_NAME` fails the invocation up front.
async fn run_with_env(
    context: &crate::app::Context,
    packages: &[std::sync::Arc<InstallInfo>],
    admitted: &AdmittedBinaries,
    child_env: env::ChildEnv<'_>,
    command: &str,
    argv: &[String],
    recording: Recording,
) -> anyhow::Result<ExitCode> {
    let mut process_env = env::Env::new();
    // Composed entries + forwarded ocx config (for any grandchild ocx) +
    // the re-emitted payload, in the one order that is correct — see
    // `Env::apply_child_env`. Re-emitting matters here because an entrypoint
    // may itself invoke another generated launcher: without the payload at
    // that second hop the package value would beat the project override
    // stage 4-6 precedence promises.
    process_env.apply_child_env(child_env, context.config_view());
    // No PATHEXT manipulation: the Windows launcher is now a native
    // `<name>.exe` shim resolved via the default Windows PATHEXT.

    // Resolved once, then handed to both the record and the launch: a second
    // resolution could disagree with the first and make the audit trail name a
    // binary other than the one that ran.
    let executable = process_env.resolve_command(command);

    // A launcher baked with a command-scratch pkg-root inherits its command's
    // exclusion. `ocx package test` and `ocx patch test` preview local
    // unpublished artifacts, so a record from this hop would describe something
    // that was never published — and a collector could not filter it out, since
    // it is indistinguishable from a legitimate direct-launcher invocation.
    //
    // The pkg-root is caller-supplied and both scratch roots live in the
    // invoking user's own `$OCX_HOME`, so the claim is forgeable by placement
    // alone. `Launch::exempt` is where that is bounded: under `required = true`
    // it refuses rather than granting an exemption the operator's policy
    // contradicts.
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
            argv,
            config: context.config_view(),
            // Already in memory: the snapshot is read once at `try_init` and
            // identity-gated there, so naming it here costs no I/O on the exec
            // path.
            managed_config_digest: context.managed_config_snapshot().map(|snapshot| &snapshot.digest),
            // A launcher re-entry has no platform context of its own: it was
            // handed a package root, not an identifier to resolve. The record
            // states that as an explicit null rather than guessing from the host.
            platform: None,
            clean_env: false,
            auto_installed: &[],
            scope: Scope::Launcher,
        },
        &policy,
    )?;

    Err(anyhow::Error::from(launch::exec(launch).await))
}

/// Validate a package root path for use from a launcher.
///
/// The path must:
/// - Be absolute
/// - Canonicalize to a location inside `packages_root` OR one of `extra_roots`
/// - Contain `metadata.json`
///
/// `extra_roots` carries the command-scratch materialization paths
/// (`$OCX_HOME/temp/test/` for `ocx package test`, `$OCX_HOME/temp/patch-test/`
/// for `ocx patch test`): launchers baked into a package materialized there
/// carry the scratch path as their pkg-root, which is equally OCX-controlled and
/// equally safe to allow. It is a short explicit list, never a rule like
/// "anything under `temp/`" — the guard's value is exactly that the accepted set
/// is enumerated.
///
/// Each entry is paired with the recording exemption of the command that owns
/// that root, and the matched one comes back with the validated path: a launcher
/// re-entry cannot recover which command spawned it from anything else, and its
/// pkg-root is exactly that fact.
///
/// This mirrors the former `validate_package_root` from `options/package_ref.rs`,
/// now inlined here (its only remaining caller) with error messages updated to
/// reference `launcher exec` instead of `file://`.
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

    // Canonicalize both sides so symlinks and `..` components cannot smuggle
    // a path outside the allowed roots.
    let canonical_dir = tokio::fs::canonicalize(dir).await.map_err(|e| {
        UsageError::new(format!(
            "launcher exec: pkg-root '{}' cannot be resolved: {e}",
            dir.display()
        ))
    })?;

    // Use `.ok()` for packages_root so that a non-existent store (fresh
    // OCX_HOME with no packages/ dir yet, as in `ocx package test` on a clean
    // host) does not hard-fail here. When packages_root is absent it simply
    // cannot match as a prefix — the extra_root check below covers the
    // package-test case. Security boundary unchanged: an absent root matches
    // nothing.
    let canonical_root = tokio::fs::canonicalize(packages_root).await.ok();

    // Canonicalize each extra root; `.ok()` so that a non-existent one (the
    // scratch roots are created lazily by their command) is simply skipped and
    // cannot match as a prefix of canonical_dir.
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
        // Build a display path for the error. When packages_root does not
        // exist yet (fresh OCX_HOME), fall back to the raw path so the error
        // message still names a useful location.
        let root_display = canonical_root.as_deref().unwrap_or(packages_root).display();
        return Err(UsageError::new(format!(
            "launcher exec: pkg-root must point inside {} (got {})",
            root_display,
            canonical_dir.display()
        )));
    }

    // Existence check on metadata.json — canonical signal that this is a package root.
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
