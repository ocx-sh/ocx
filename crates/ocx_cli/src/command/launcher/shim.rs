// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Hidden `ocx launcher shim` subcommand — the first-invocation entry point of a deferred tool.
//! Generated shims call `ocx launcher shim '<pinned-id>' -- "$(basename "$0")" "$@"`.
//!
//! Shim-store write access equals package-store write access, so nothing here authenticates the
//! caller; integrity rests on the full-digest fetch.

use std::collections::BTreeSet;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use ocx_config::env;
use ocx_oci::{PackageRef, PinnedPackageRef};
use ocx_package::metadata::BinaryName;
use ocx_package_manager::Arrival;
use ocx_package_manager::EnvScope;
use ocx_package_manager::error::{PackageErrorKind, ShimClaim};
use ocx_package_manager::launch;
use ocx_package_manager::launch::Launch;
use ocx_package_manager::record::{RecordInputs, Scope};
use ocx_project::ProjectConfig;
use ocx_project::lazy;

use crate::options::LazyReport;
use ocx_package::metadata::env::apply::{ChildEnv, EnvEntriesExt, reconcile_list_separators};

/// Entry point from a generated shim. Validates the invoked name, materializes
/// the package, then execs the resolved target.
#[derive(Parser)]
pub struct LauncherShim {
    #[clap(flatten)]
    lazy_report: LazyReport,

    /// Pinned identifier of the deferred tool, baked into the shim.
    ///
    /// Always fully qualified and always digest-bearing
    /// (`registry/repository[:tag]@sha256:...`), because ocx wrote it: the
    /// download it triggers is addressed by that digest, never by the tag.
    #[clap(value_name = "PINNED-ID", value_parser = parse_pinned_identifier)]
    identifier: PinnedPackageRef,

    /// The shim's own filename (argv0 passed after `--`), then the user's
    /// arguments. The filename selects which of the tool's declared names was
    /// invoked.
    #[clap(last = true, required = true, num_args = 1..)]
    argv: Vec<String>,
}

impl LauncherShim {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let manager = context.manager();

        // Both `argv0` legs before anything downloads: grammar stops a path separator bypassing `PATH`,
        // membership stops an unclaimed name triggering a download. Args stay unbound: the launch seam
        // derives them from the record's `argv`.
        let (argv0, _) = self
            .argv
            .split_first()
            .expect("clap `required = true, num_args = 1..` guarantees at least one argv element");
        let claimed = manager.claimed_shim_names(&self.identifier).await?;
        let name = validate_argv0(argv0, &self.identifier, &claimed).map_err(anyhow::Error::new)?;

        // Folded before the download, so a malformed name template fails before a large transfer.
        let records = context.records(ocx_package_manager::record::RecordsOptions::default())?;

        // `materialize_deferred` owns the read-only-view routing that keeps `index/` at zero bytes.
        let report = self.report(project_in_scope(&context).await.as_ref());
        let platform = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);
        let found = manager
            .materialize_deferred(&self.identifier, platform.clone(), report)
            .await?;
        // The one frame that downloads a lazy tool, so the only place `resolution.autoInstalled` can
        // truthfully report it.
        let auto_installed: Vec<PackageRef> = match found.arrival {
            Arrival::Pulled => vec![self.identifier.as_identifier().clone()],
            Arrival::Cached => Vec::new(),
        };
        let packages = [Arc::new(found.info)];

        // Consumer view, unlike `launcher exec`: this resolves a name the package publishes outward. The
        // attribution-keeping variant, since the record needs both and only this call has them.
        let (mut entries, _, patch_companions, admitted) = manager
            .resolve_env_with_attribution(&packages, false, EnvScope::package_tier(), &platform)
            .await?;
        // As in `ocx exec`: contributors disagreeing on a key's separator fail, not fold silently.
        reconcile_list_separators(entries.iter_mut()).map_err(anyhow::Error::new)?;

        let mut process_env = env::Env::new();
        // No forwarded payload: a shim runs from a bare `PATH` lookup, never under `ocx exec`.
        process_env.apply_child_env(
            ChildEnv {
                composed: &entries,
                forwarded: &[],
            },
            context.config_view(),
        );

        // Strip the shim `bin/` first, or an unshipped claimed name resolves back here and `execvp(2)`
        // re-enters forever at 100% CPU. `prepare_lazy` cannot catch that claim: no content exists yet.
        let shim_bin = context.file_structure().shims.shim_dir(&self.identifier).bin();
        if let Some(path) = process_env.get("PATH") {
            let pruned = ocx_util::path::remove_segment(path, shim_bin.as_os_str());
            process_env.set("PATH", pruned);
        }

        // Two signals, since `remove_segment` compares exactly: a name `PATH` lacks, and an answer that
        // resolves back inside the shim tree. Neither may reach `exec`.
        let resolved = resolve_claimed(&process_env, &self.identifier, name.clone())?;
        if resolves_inside(resolved.clone(), shim_bin.clone()).await? {
            return Err(shim_claim_unfulfilled(&self.identifier, name));
        }

        // Resolved once for record and launch, or the audit trail could name a binary that did not run.
        let launch = Launch::recording(
            process_env,
            RecordInputs {
                packages: &packages,
                admitted: &admitted,
                patch_companions: &patch_companions,
                executable: &resolved,
                store_root: context.file_structure().packages.root(),
                shim_root: context.file_structure().shims.root(),
                argv: &self.argv,
                config: context.config_view(),
                insecure_registries: context.insecure_hosts(),
                // Read and identity-gated once at `try_init`; no I/O here.
                managed_config_digest: context.managed_config_snapshot().map(|snapshot| &snapshot.digest),
                // Likewise read once at `try_init`.
                patch_snapshot_digest: context.patch_snapshot_digest(),
                platform: Some(&platform),
                // `Env::new()` inherits this process's environment, hence the `PATH` prune above.
                clean_env: false,
                auto_installed: &auto_installed,
                scope: Scope::LauncherShim {
                    requested: self.identifier.clone(),
                },
            },
            &records,
        )?;

        // Diverges on success on every platform; only start-up failures fall through.
        Err(anyhow::Error::from(launch::exec(launch).await))
    }

    /// Resolves `lazy-report`: `--lazy-report` ▸ `[package."<id>"]` ▸ toolchain ▸ `OCX_LAZY_REPORT`.
    ///
    /// Resolved where the download runs, so it reflects today's configuration. No `[group.<g>]`
    /// tier: nothing on the wire names the composing group, so it could be written but never read.
    fn report(&self, project: Option<&ProjectConfig>) -> lazy::LazyReport {
        lazy::LazyReportLadder {
            cli: self.lazy_report.mode(),
            package: project
                .and_then(|config| config.package_settings(self.identifier.as_identifier()))
                .and_then(|settings| settings.lazy_report),
            toolchain: project.and_then(|config| config.lazy_report),
            environment: lazy::LazyReport::from_env(),
        }
        .resolve()
    }
}

/// Resolves the claimed `name` on the pruned composed `PATH`, mapping **only** a total miss to the
/// claim refusal.
///
/// A bare `?` would keep exit 65 but lose the attribution naming the publisher. Any other kind,
/// including a refused trampoline, propagates as itself: the package did provide the name.
fn resolve_claimed(
    process_env: &env::Env,
    identifier: &PinnedPackageRef,
    name: BinaryName,
) -> anyhow::Result<std::path::PathBuf> {
    match process_env.resolve_command(name.as_str()) {
        Ok(resolved) => Ok(resolved),
        Err(env::CommandResolutionError::NotFound { .. }) => Err(shim_claim_unfulfilled(identifier, name)),
        Err(error) => Err(anyhow::Error::new(error)),
    }
}

/// The claim-unfulfilled refusal, built at both of the guard's signals.
///
/// One function so both sites emit a byte-identical error: the message is all that tells it from a
/// generic resolution failure with the same exit code.
fn shim_claim_unfulfilled(package: &PinnedPackageRef, name: BinaryName) -> anyhow::Error {
    anyhow::Error::new(PackageErrorKind::ShimClaimUnfulfilled(Box::new(ShimClaim {
        package: package.clone(),
        name,
    })))
}

/// Whether `resolved` lands inside `shim_bin` once both paths are canonicalized.
///
/// Catches what the string-compare strip cannot: a trailing slash, a symlink alias, a differently
/// spelled `$OCX_HOME`. **Fails closed**: an unresolvable path counts as inside, the only thing
/// between a false `binaries` claim and an endless `execve` loop. [`dunce::canonicalize`] avoids
/// Windows verbatim `\\?\` paths.
async fn resolves_inside(resolved: std::path::PathBuf, shim_bin: std::path::PathBuf) -> anyhow::Result<bool> {
    let inside = tokio::task::spawn_blocking(move || {
        let resolved = dunce::canonicalize(&resolved);
        let shim_bin = dunce::canonicalize(&shim_bin);
        match (resolved, shim_bin) {
            (Ok(resolved), Ok(shim_bin)) => resolved.starts_with(shim_bin),
            // Fail closed — see the doc comment above.
            _ => true,
        }
    })
    .await?;
    Ok(inside)
}

/// The `ocx.toml` this process stands in, if any — best effort.
///
/// Any failure is `None` with a debug log: the value only decides whether a progress bar renders,
/// never whether the tool runs.
async fn project_in_scope(context: &crate::app::Context) -> Option<ProjectConfig> {
    let (config_path, _lock_path) = crate::app::project_context::resolve_project_paths(context, None)
        .await
        .inspect_err(|error| log::debug!("No project in scope for the lazy-report ladder: {error}"))
        .ok()?;
    ProjectConfig::from_path(&config_path)
        .await
        .inspect_err(|error| {
            log::debug!(
                "Ignoring '{}' for the lazy-report ladder: {error}",
                config_path.display()
            );
        })
        .ok()
}

/// Parses the baked positional into a digest-bearing identifier.
///
/// [`PackageRef::parse`], not the default-registry form: a shim body must not depend on the ambient
/// default registry. A failure is a clap invalid-value error, exit 64.
fn parse_pinned_identifier(value: &str) -> Result<PinnedPackageRef, String> {
    let identifier = PackageRef::parse(value).map_err(|error| error.to_string())?;
    PinnedPackageRef::try_from(identifier).map_err(|error| error.to_string())
}

/// Validates the wire's `argv0` against both legs of the name contract.
///
/// `claimed` is the shim directory's own `bin/` listing: the generated launchers are that set, and
/// whoever can write the shim store controls body and name alike.
///
/// # Errors
///
/// - [`PackageErrorKind::ShimNameInvalid`] when `argv0` fails the [`BinaryName`] grammar.
/// - [`PackageErrorKind::ShimNameNotClaimed`] when `package` claims no such name.
// `PackageErrorKind` sits at clippy's 128-byte ceiling; boxing would give this cold path a shape no
// sibling refusal has.
#[allow(clippy::result_large_err)]
fn validate_argv0(
    argv0: &str,
    package: &PinnedPackageRef,
    claimed: &BTreeSet<BinaryName>,
) -> Result<BinaryName, PackageErrorKind> {
    // Grammar first: it is the security leg (`/` or `\` bypasses `PATH`), and membership first would
    // report an ill-formed name as merely unclaimed.
    let name = BinaryName::try_from(argv0).map_err(PackageErrorKind::ShimNameInvalid)?;
    if claimed.contains(&name) {
        return Ok(name);
    }
    Err(PackageErrorKind::ShimNameNotClaimed(Box::new(ShimClaim {
        package: package.clone(),
        name,
    })))
}

#[cfg(test)]
mod tests {
    //! Contract-first tests for C-010's wire consumer, C-011's two `argv0`
    //! legs and its exit-code rows, and the four-tier `lazy-report` ladder
    //! C-006 leaves this process to resolve.
    //!
    //! Written from the contract, never from the bodies
    //! below: everything the Implement phase still owns fails here with
    //! `unimplemented`, which is what says these tests describe the contract
    //! rather than restate the code.

    use clap::{CommandFactory as _, Parser as _};

    use super::*;

    /// A digest-bearing, tag-bearing identifier of the shape a generated shim
    /// bakes. C-010's amendment keeps the advisory tag, so every fixture that
    /// could distinguish "kept" from "stripped" carries one.
    const PINNED: &str =
        "ocx.sh/tool/cmake:3.28@sha256:0000000000000000000000000000000000000000000000000000000000000001";

    // ── Parser harness ───────────────────────────────────────────────────────

    /// Builds a full `ocx launcher shim` argv: the flags under test, then the
    /// pinned identifier, then `--` and the argv the wire carries.
    fn argv(flags: &[&str], positional: &[&str]) -> Vec<String> {
        ["ocx", "launcher", "shim"]
            .iter()
            .copied()
            .chain(flags.iter().copied())
            .chain(positional.iter().copied())
            .map(str::to_string)
            .collect()
    }

    /// Runs argv through the real `ocx` parser — the same `Cli` the binary
    /// builds, not a stand-in, so the assertions speak about the shipped
    /// grammar.
    fn parse(argv: &[String]) -> Result<crate::app::Cli, clap::Error> {
        crate::app::Cli::try_parse_from(argv)
    }

    /// Parses and digs the verb's own struct back out of the command tree.
    /// Panics with the clap diagnostic on a failure, so a grammar regression
    /// names itself instead of surfacing as a match failure.
    fn shim(flags: &[&str], positional: &[&str]) -> LauncherShim {
        let line = argv(flags, positional);
        let cli = parse(&line).unwrap_or_else(|error| panic!("`{}` must parse: {error}", line.join(" ")));
        match cli.command {
            Some(crate::command::Command::Launcher(super::super::Launcher::Shim(shim))) => shim,
            other => panic!("expected `launcher shim`, parsed {:?}", other.is_some()),
        }
    }

    /// The clap error a rejected invocation produced. `Cli` derives no
    /// `Debug`, so `expect_err` is unavailable and this is the idiom the
    /// sibling option tests already use.
    fn rejection(line: &[String], what: &str) -> clap::Error {
        parse(line)
            .err()
            .unwrap_or_else(|| panic!("`{}` must be refused ({what})", line.join(" ")))
    }

    /// Asserts a rejected invocation is one ocx reports as **exit 64**.
    ///
    /// The predicate is not a restatement of the number: `clap_parse::parse`
    /// routes every clap failure to `ExitCode::UsageError` (64) *except* the
    /// three display kinds, which it hands to clap's renderer and exit 0. So
    /// "kind is outside that set" is exactly the condition that produces 64,
    /// checked here rather than asserted as a literal.
    fn assert_exits_64(error: &clap::Error, what: &str) {
        use clap::error::ErrorKind;
        assert!(
            !matches!(
                error.kind(),
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ),
            "{what}: a display kind ({:?}) prints and exits 0; this invocation must map to EX_USAGE 64",
            error.kind()
        );
    }

    /// Descends `Cli::command()` to `launcher shim`. Panics when a segment is
    /// missing so a renamed verb reds here rather than making every assertion
    /// below vacuous.
    fn shim_command() -> clap::Command {
        crate::app::Cli::command()
            .find_subcommand("launcher")
            .expect("`ocx launcher` must exist")
            .find_subcommand("shim")
            .expect("`ocx launcher shim` must exist")
            .clone()
    }

    fn lazy_report_arg() -> clap::Arg {
        shim_command()
            .get_arguments()
            .find(|argument| argument.get_long() == Some("lazy-report"))
            .expect("`ocx launcher shim` must declare --lazy-report")
            .clone()
    }

    // ── C-010 consumer side: the baked identifier ────────────────────────────

    /// C-011: `<pinned-id>` keeps its advisory tag. The tag is what makes
    /// `ShimNameNotClaimed` / `ShimClaimUnfulfilled` name a package the user
    /// recognizes; the fetch is digest-addressed either way. A parser that
    /// canonicalized the wire value through `strip_advisory` would pass every
    /// other test in this file.
    #[test]
    fn the_baked_identifier_keeps_its_advisory_tag() {
        let parsed = shim(&[], &[PINNED, "--", "cmake"]);
        assert_eq!(
            parsed.identifier.to_string(),
            PINNED,
            "the pinned identifier must round-trip verbatim, advisory tag included"
        );
        assert_eq!(
            parsed.identifier.tag(),
            Some("3.28"),
            "the advisory tag must survive parsing"
        );
    }

    /// C-011 (F-8): a `<pinned-id>` carrying no digest exits 64. A shim body
    /// is written by ocx, so an unpinned value is a malformed *invocation*
    /// reachable only from a corrupted body — the same treatment
    /// `launcher exec` gives a bad `pkg-root`.
    #[test]
    fn a_pinned_id_without_a_digest_exits_64() {
        let line = argv(&[], &["ocx.sh/tool/cmake:3.28", "--", "cmake"]);
        assert_exits_64(&rejection(&line, "no digest"), "unpinned identifier");
        // Control on the same parser: the digest-bearing form does parse, so
        // the rejection cannot come from a verb that refuses everything.
        parse(&argv(&[], &[PINNED, "--", "cmake"])).expect("the digest-bearing form parses");
    }

    /// C-011 (F-8): a `<pinned-id>` that does not parse at all exits 64 too.
    /// The wire value is always fully qualified, so a bare repository is
    /// refused rather than completed from the ambient default registry — a
    /// shim body must not depend on whatever shell later runs it.
    #[test]
    fn a_pinned_id_that_does_not_parse_exits_64() {
        for malformed in ["cmake", "cmake:3.28", "not a reference"] {
            let line = argv(&[], &[malformed, "--", "cmake"]);
            assert_exits_64(&rejection(&line, "unparseable identifier"), malformed);
        }
    }

    // ── C-011: the positional shape ──────────────────────────────────────────

    /// The wire is `<pinned-id> -- <argv0> [args...]`. Without the `--` the
    /// invocation is refused: `argv0` is the shim's own filename and the
    /// separator is what keeps a user argument from ever being read as one.
    #[test]
    fn the_wire_requires_a_double_dash_before_argv0() {
        let line = argv(&[], &[PINNED, "cmake"]);
        assert_exits_64(&rejection(&line, "no `--`"), "missing `--` separator");
        parse(&argv(&[], &[PINNED, "--", "cmake"])).expect("control: the separated form parses");
    }

    /// `argv0` first, then the user's arguments, in order and unmodified. A
    /// leading-dash user argument must reach the child rather than be read as
    /// a flag of this verb.
    #[test]
    fn argv_carries_argv0_then_the_user_arguments_in_order() {
        let parsed = shim(&[], &[PINNED, "--", "cmake", "--version", "-DFOO=bar", "--lazy-report"]);
        assert_eq!(
            parsed.argv,
            vec!["cmake", "--version", "-DFOO=bar", "--lazy-report"],
            "everything after `--` is the invoked name then the user's args, verbatim"
        );
    }

    /// At least an `argv0` is required — a shim always knows the name it was
    /// invoked under, so an empty tail is a malformed invocation.
    #[test]
    fn an_empty_argv_tail_exits_64() {
        let line = argv(&[], &[PINNED, "--"]);
        assert_exits_64(&rejection(&line, "no argv0"), "empty argv tail");
        parse(&argv(&[], &[PINNED, "--", "cmake"])).expect("control: one argv0 parses");
    }

    // ── C-006 / C-007: the `--lazy-report` flag on this verb ─────────────────

    /// The published value set is exactly `silent`, `progress`, in that order,
    /// long form only.
    #[test]
    fn the_lazy_report_flag_offers_exactly_silent_and_progress() {
        let argument = lazy_report_arg();
        let values: Vec<String> = argument
            .get_possible_values()
            .iter()
            .map(|value| value.get_name().to_string())
            .collect();
        assert_eq!(values, ["silent", "progress"]);
        assert_eq!(argument.get_short(), None, "no short form");
        assert!(!argument.is_required_set(), "a user never types this flag");
        let value_names: Vec<String> = argument
            .get_value_names()
            .unwrap_or_default()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(value_names, ["MODE"]);
    }

    /// Space- and `=`-separated forms are one invocation; the value is
    /// case-sensitive and mandatory. Case folding belongs to the environment
    /// tier alone.
    #[test]
    fn the_lazy_report_value_is_case_sensitive_and_mandatory() {
        for form in [&["--lazy-report", "progress"][..], &["--lazy-report=progress"][..]] {
            let parsed = shim(form, &[PINNED, "--", "cmake"]);
            assert_eq!(parsed.lazy_report.mode(), Some(ocx_project::lazy::LazyReport::Progress));
        }
        for rejected in [&["--lazy-report", "Progress"][..], &["--lazy-report"][..]] {
            let line = argv(rejected, &[PINNED, "--", "cmake"]);
            assert_exits_64(&rejection(&line, "bad value"), "bad --lazy-report value");
        }
    }

    /// An omitted flag leaves the CLI tier absent so the config and
    /// environment tiers can speak — it never means `silent`.
    #[test]
    fn an_omitted_lazy_report_leaves_the_cli_tier_absent() {
        assert_eq!(
            shim(&[], &[PINNED, "--", "cmake"]).lazy_report.mode(),
            None,
            "absence means inherit, never the ladder floor"
        );
    }

    /// Flags before positional arguments, per the project convention.
    #[test]
    fn the_lazy_report_flag_is_declared_before_every_positional() {
        let command = shim_command();
        let arguments: Vec<&clap::Arg> = command.get_arguments().collect();
        let flag_at = arguments
            .iter()
            .position(|argument| argument.get_long() == Some("lazy-report"))
            .expect("--lazy-report must be declared");
        let positional_at = arguments
            .iter()
            .position(|argument| argument.is_positional())
            .expect("the verb takes positionals, or this guard measures nothing");
        assert!(
            flag_at < positional_at,
            "--lazy-report is declared after the first positional"
        );
    }

    /// The verb is internal: hidden from `ocx --help`, still listed under
    /// `ocx launcher --help` so it stays debuggable.
    #[test]
    fn the_shim_verb_is_hidden_from_root_help_and_listed_under_launcher() {
        let root = crate::app::Cli::command().render_long_help().to_string();
        assert!(
            !root.contains("launcher"),
            "the launcher group must not appear in `ocx --help`"
        );
        let group = crate::app::Cli::command()
            .find_subcommand("launcher")
            .expect("`ocx launcher` must exist")
            .clone()
            .render_long_help()
            .to_string();
        assert!(
            group.contains("shim"),
            "`ocx launcher --help` must list the shim verb: {group}"
        );
    }

    // ── C-011: the two `argv0` legs ──────────────────────────────────────────

    fn pinned() -> PinnedPackageRef {
        let identifier = PackageRef::parse(PINNED).expect("fixture parses");
        PinnedPackageRef::try_from(identifier).expect("fixture is digest-bearing")
    }

    fn names(values: &[&str]) -> BTreeSet<BinaryName> {
        values
            .iter()
            .map(|value| BinaryName::try_from(*value).expect("fixture is a valid binary name"))
            .collect()
    }

    /// The happy path: a name the package claims is admitted, and comes back
    /// as the typed `BinaryName` the caller resolves on `PATH`.
    #[test]
    fn validate_argv0_accepts_a_claimed_name() {
        let claimed = names(&["cmake", "cpack", "ctest"]);
        let admitted = validate_argv0("cmake", &pinned(), &claimed).expect("a claimed name must be admitted");
        assert_eq!(admitted.as_str(), "cmake");
    }

    /// Grammar leg. `BinaryName` forbids `/`, `\` and the Windows-reserved
    /// device names at construction — which is what stops a wire value
    /// containing a path separator from bypassing `PATH` resolution entirely.
    /// Each row is also a *claimed* name in the set, so a refusal here cannot
    /// be the membership leg answering.
    #[test]
    fn validate_argv0_rejects_a_name_outside_the_binary_grammar() {
        let package = pinned();
        for rejected in [
            "../../etc/passwd",
            "bin/cmake",
            "..\\windows\\system32\\cmd",
            "nul",
            "-rf",
            "",
        ] {
            let claimed = names(&["cmake"]);
            let error = validate_argv0(rejected, &package, &claimed)
                .expect_err(&format!("'{rejected}' is not a valid binary name"));
            assert!(
                matches!(error, PackageErrorKind::ShimNameInvalid(_)),
                "'{rejected}' must be refused as ShimNameInvalid, got {error:?}"
            );
        }
    }

    /// Membership leg. A well-formed name the package never claimed is
    /// refused *before* anything downloads — the whole point of the check is
    /// that an unclaimed name must not trigger a materialization.
    #[test]
    fn validate_argv0_rejects_a_well_formed_name_the_package_never_claimed() {
        let package = pinned();
        let claimed = names(&["cmake", "cpack"]);
        let error = validate_argv0("ctest", &package, &claimed).expect_err("'ctest' is not claimed");
        match error {
            PackageErrorKind::ShimNameNotClaimed(claim) => {
                assert_eq!(claim.name.as_str(), "ctest", "the refusal names the invoked name");
                assert_eq!(claim.package, package, "the refusal names the package");
            }
            other => panic!("expected ShimNameNotClaimed, got {other:?}"),
        }
    }

    /// The grammar leg runs first. `bin/cmake` is both ill-formed and
    /// unclaimed; it must be reported as `ShimNameInvalid`, because the
    /// grammar leg is the one closing the path-separator hole and a
    /// membership answer would describe the wrong defect.
    #[test]
    fn validate_argv0_checks_the_grammar_leg_before_the_membership_leg() {
        let claimed = names(&["cmake"]);
        let error = validate_argv0("bin/cmake", &pinned(), &claimed).expect_err("a path-bearing name is refused");
        assert!(
            matches!(error, PackageErrorKind::ShimNameInvalid(_)),
            "an ill-formed AND unclaimed name is reported by the grammar leg, got {error:?}"
        );
    }

    /// An empty claim set claims nothing, so every well-formed name is
    /// unclaimed. Reds on an implementation that reads an empty set as
    /// "unknown, admit everything".
    #[test]
    fn validate_argv0_refuses_every_name_when_the_claim_set_is_empty() {
        let error = validate_argv0("cmake", &pinned(), &BTreeSet::new()).expect_err("nothing is claimed");
        assert!(
            matches!(error, PackageErrorKind::ShimNameNotClaimed(_)),
            "an empty claim set must refuse, got {error:?}"
        );
    }

    // ── C-006: the four-tier `lazy-report` ladder ────────────────────────────
    //
    // Every row below expects `Progress`, which is never the ladder's floor,
    // and sets every tier BELOW the one under test to `Silent`. So a resolver
    // that consults the tiers in the wrong order - or consults none at all -
    // returns `Silent` and reds. Each row also leaves the environment tier
    // out-ranked, which is what makes these deterministic regardless of the
    // ambient `OCX_LAZY_REPORT` (see the report for why that tier is not
    // assertable here).

    fn project(toml: &str) -> ProjectConfig {
        ProjectConfig::from_toml_str(toml).expect("fixture config parses")
    }

    #[test]
    fn report_prefers_the_cli_flag_over_every_config_tier() {
        let config = project("lazy-report = \"silent\"\n\n[package.\"ocx.sh/tool/cmake\"]\nlazy-report = \"silent\"\n");
        let parsed = shim(&["--lazy-report", "progress"], &[PINNED, "--", "cmake"]);
        assert_eq!(parsed.report(Some(&config)), ocx_project::lazy::LazyReport::Progress);
    }

    #[test]
    fn report_prefers_the_package_entry_over_the_toolchain_tier() {
        let config =
            project("lazy-report = \"silent\"\n\n[package.\"ocx.sh/tool/cmake\"]\nlazy-report = \"progress\"\n");
        let parsed = shim(&[], &[PINNED, "--", "cmake"]);
        assert_eq!(parsed.report(Some(&config)), ocx_project::lazy::LazyReport::Progress);
    }

    /// The package entry is keyed on `registry/repository`, version-
    /// independent — the same rule `no-patches` already follows, and what the
    /// wire allows: the shim carries one pinned identifier and the config
    /// author cannot know its digest.
    #[test]
    fn report_matches_the_package_entry_without_its_tag() {
        let config = project("[package.\"ocx.sh/tool/cmake\"]\nlazy-report = \"progress\"\n");
        let parsed = shim(&[], &[PINNED, "--", "cmake"]);
        assert_eq!(
            parsed.report(Some(&config)),
            ocx_project::lazy::LazyReport::Progress,
            "a tagless package key must match a tag-and-digest-bearing identifier"
        );
    }

    /// A package entry for a *different* package must not answer. Without
    /// this the test above would also pass for an implementation that takes
    /// whatever single `[package.*]` entry it finds.
    #[test]
    fn report_ignores_a_package_entry_for_another_package() {
        let config =
            project("lazy-report = \"progress\"\n\n[package.\"ghcr.io/acme/other\"]\nlazy-report = \"silent\"\n");
        let parsed = shim(&[], &[PINNED, "--", "cmake"]);
        assert_eq!(
            parsed.report(Some(&config)),
            ocx_project::lazy::LazyReport::Progress,
            "an unrelated package entry must not outrank the toolchain tier"
        );
    }

    #[test]
    fn report_falls_back_to_the_toolchain_tier() {
        let config = project("lazy-report = \"progress\"\n\n[tools]\ncmake = \"ocx.sh/tool/cmake:3.28\"\n");
        let parsed = shim(&[], &[PINNED, "--", "cmake"]);
        assert_eq!(parsed.report(Some(&config)), ocx_project::lazy::LazyReport::Progress);
    }

    // ── C-057 / S-010: an unresolvable claim keeps its own message ─────────

    /// C-057: an unresolvable claimed name still produces the
    /// `ShimClaimUnfulfilled` message **naming the package and the claim**.
    ///
    /// Both this error and `CommandResolutionError::NotFound` classify to 65,
    /// so the exit code cannot catch a regression here — replacing the mapping
    /// with a bare `?` keeps the code and loses the attribution. The assertion
    /// is therefore on the rendered message text.
    #[test]
    fn an_unfulfilled_claim_renders_a_message_naming_the_package_and_the_name() {
        let name = BinaryName::try_from("cmake").expect("fixture is a valid binary name");
        let error = shim_claim_unfulfilled(&pinned(), name);
        let message = format!("{error:#}");

        assert!(
            message.contains("cmake"),
            "the message must name the claim, got: {message}"
        );
        assert!(
            message.contains(&pinned().to_string()),
            "the message must name the package that made the claim, got: {message}"
        );
        assert!(
            message.contains("claims the name"),
            "the message must attribute the failure to the publisher, got: {message}"
        );
        assert_eq!(
            crate::exit::classify_library_error(error.as_ref()),
            ocx_exit::ExitCode::DataError,
            "C-057: the refusal is 65, the same code a bare resolution failure carries"
        );
    }

    /// C-057, through the seam `execute` resolves on: a name the composed
    /// `PATH` does not provide keeps the `ShimClaimUnfulfilled` attribution
    /// rather than degrading to a generic resolution error.
    ///
    /// Both errors classify to 65, so only the message can catch a bare `?`
    /// here — which is why the assertion is on the rendered text.
    #[cfg(unix)]
    #[test]
    fn resolve_claimed_maps_a_total_miss_back_to_the_claim_refusal() {
        let empty = tempfile::tempdir().expect("tempdir");
        let mut process_env = env::Env::clean();
        process_env.set("PATH", empty.path());

        let name = BinaryName::try_from("cmake").expect("fixture is a valid binary name");
        let error = resolve_claimed(&process_env, &pinned(), name).expect_err("an empty PATH provides no claim");
        let message = format!("{error:#}");

        assert!(
            message.contains("claims the name"),
            "a total miss must stay attributed to the publisher, got: {message}"
        );
        assert!(
            message.contains("cmake") && message.contains(&pinned().to_string()),
            "the message must name both the claim and the package, got: {message}"
        );
    }

    /// C-069 through the same seam: a **trampoline** answer propagates as
    /// itself, never as an unfulfilled claim.
    ///
    /// The package did provide the name — an ocx trampoline answered for it —
    /// so blaming the publisher would misattribute a loop guard firing. This is
    /// the half a blanket `Err(_) => claim refusal` arm would swallow.
    #[cfg(unix)]
    #[test]
    fn resolve_claimed_propagates_a_trampoline_refusal_as_itself() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let trampoline = dir.path().join("cmake");
        std::fs::write(
            &trampoline,
            format!("#!/bin/sh\n{}\nexec ocx exec -- cmake \"$@\"\n", env::TRAMPOLINE_MARKER),
        )
        .expect("write the trampoline fixture");
        std::fs::set_permissions(&trampoline, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let mut process_env = env::Env::clean();
        process_env.set("PATH", dir.path());

        let name = BinaryName::try_from("cmake").expect("fixture is a valid binary name");
        let error = resolve_claimed(&process_env, &pinned(), name).expect_err("a trampoline answer is refused");

        assert!(
            matches!(
                error.downcast_ref::<env::CommandResolutionError>(),
                Some(env::CommandResolutionError::TrampolineRefused { .. })
            ),
            "C-069's refusal must reach the caller as itself, got: {error:#}"
        );
        assert!(
            !format!("{error:#}").contains("claims the name"),
            "a refused trampoline is not an unfulfilled claim"
        );
    }
}
