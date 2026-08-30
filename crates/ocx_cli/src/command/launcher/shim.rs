// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Hidden `ocx launcher shim` subcommand — the first-invocation entry point of
//! a deferred tool.
//!
//! Generated shims call:
//!   `ocx launcher shim '<pinned-id>' -- "$(basename "$0")" "$@"`
//!
//! The sibling of [`super::exec`], and the same two-token wire commitment
//! (`launcher` + `shim`, then the positional shape). It differs in what it is
//! handed and what it must do first: `launcher exec` receives a package root
//! that already exists and dispatches into it, while `launcher shim` receives a
//! pinned identifier whose package is deliberately *absent* and materializes it
//! before anything can be dispatched.
//!
//! Order of work, which is contract rather than convenience:
//!
//! 1. Validate `argv0` — it must parse as a
//!    [`BinaryName`](ocx_lib::package::metadata::BinaryName) and be a member of
//!    the deferred tool's composed name set. The grammar leg is what stops a
//!    wire value carrying a path separator from bypassing `PATH` resolution
//!    entirely; the membership leg is what stops a well-formed name the package
//!    never claimed from triggering a download.
//! 2. Materialize the package — the ordinary pull, by digest, through
//!    [`PackageManager::read_only_view`](ocx_lib::package_manager::PackageManager::read_only_view).
//!    The read-only view is not incidental: a deferred tool is composed from
//!    `ocx.lock`, so its materialization is the same index-free resolve the
//!    lock already promises. A writing view would let a lazily composed tool
//!    grow the local index where its eager twin does not — a `tag@digest`
//!    pull skips the tag pointer but still persists a dispatch object under
//!    `index/` — which breaks the byte-identical-to-eager property on the
//!    index axis, and would leave a `--frozen` first invocation writing there
//!    at all.
//! 3. Compose the tool's consumer-facing environment (`self_view = false`),
//!    drop this tool's own shim directory from the composed `PATH`, resolve
//!    `argv0` on what is left, and exec it. A name still absent at that point is
//!    reported as an unfulfilled claim naming the package, never as a bare
//!    `ENOENT`, so a wrong `binaries` claim is attributed to the publisher
//!    rather than read as a missing package.
//!
//!    **The strip is what makes step 3 terminate**, not a tidiness measure. The
//!    process inherits the `PATH` its launcher was found on and composed entries
//!    only prepend, so without it a claimed-but-unshipped name resolves back to
//!    the same launcher and `execvp` re-enters this process with no depth
//!    counter. The refusal below is unreachable in that state, which is how a
//!    guard written against a false premise ("`PATH` yields nothing") survived
//!    review — it was never once observed firing.
//!
//! **What this verb does not close.** Write access to the shim store is
//! equivalent to write access to the package store: whoever can rewrite a shim
//! body controls both the identifier it bakes and the name it passes, so no
//! check here authenticates the caller. Integrity rests where it already does —
//! the full-digest fetch and its content verification.

use std::collections::BTreeSet;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use ocx_lib::oci::{Identifier, PinnedIdentifier};
use ocx_lib::package::metadata::BinaryName;
use ocx_lib::package_manager::EnvScope;
use ocx_lib::package_manager::error::{PackageErrorKind, ShimClaim};
use ocx_lib::project::ProjectConfig;
use ocx_lib::utility::child_process;
use ocx_lib::{env, lazy, log, oci};

use crate::options::LazyReport;

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
    identifier: PinnedIdentifier,

    /// The shim's own filename (argv0 passed after `--`), then the user's
    /// arguments. The filename selects which of the tool's declared names was
    /// invoked.
    #[clap(last = true, required = true, num_args = 1..)]
    argv: Vec<String>,
}

impl LauncherShim {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let manager = context.manager();

        // Step 1 — both `argv0` legs, before anything downloads. An unclaimed
        // name must not be able to trigger a materialization, so the check runs
        // against the store's own claim set first.
        let (argv0, args) = self
            .argv
            .split_first()
            .expect("clap `required = true, num_args = 1..` guarantees at least one argv element");
        let claimed = manager.claimed_shim_names(&self.identifier).await?;
        let name = validate_argv0(argv0, &self.identifier, &claimed).map_err(anyhow::Error::new)?;

        // Step 2 — materialize. The report tier is resolved here, at download
        // time, from whatever project this process happens to stand in; the
        // library helper owns the read-only-view routing that keeps `index/` at
        // zero bytes.
        let report = self.report(project_in_scope(&context).await.as_ref());
        let platform = oci::Platform::current().unwrap_or_else(oci::Platform::any);
        let info = manager
            .materialize_deferred(&self.identifier, platform.clone(), report)
            .await?;

        // Step 3 — compose the tool's CONSUMER-facing environment. `self_view`
        // is false, unlike `launcher exec`: that verb dispatches a package's own
        // entrypoint from inside the package, while this one resolves a name the
        // package publishes to the outside world.
        let mut entries = manager
            .resolve_env(&[Arc::new(info)], false, EnvScope::package_tier(), &platform)
            .await?;
        // Same per-key list-separator agreement `ocx exec` and `ocx package exec`
        // settle before applying: this process composes the closure afresh, so
        // two contributors disagreeing on one key's separator has to fail here
        // too rather than fold with a silently chosen one.
        env::reconcile_list_separators(entries.iter_mut()).map_err(anyhow::Error::new)?;

        let mut process_env = env::Env::new();
        // No forwarded payload: a shim is invoked from a bare `PATH` lookup, not
        // from an `ocx exec` parent, so there is no project `[env]` to replay.
        process_env.apply_child_env(
            env::ChildEnv {
                composed: &entries,
                forwarded: &[],
            },
            context.config_view(),
        );

        // Drop the shim directory this process was invoked from before
        // resolving anything on the composed `PATH`.
        //
        // Without it the guard below cannot fire and the failure is not a bad
        // error message but an unbounded `execve` loop. `Env::new()` inherits
        // the caller's `PATH`, which necessarily contains this shim `bin/` —
        // that is the only reason the launcher ran at all — and composed
        // entries *prepend*, so it survives lower down. Materialization never
        // retires the shim tree. So a claimed name the package does not ship
        // walks past the now-real package directories and finds the same
        // launcher again: `resolved` is an absolute path, the bare-name
        // comparison is false, and `child_process::exec` is `execvp(2)` — no
        // return, no depth counter. One process at 100% CPU forever, from
        // nothing more than publishing a `binaries` claim naming an executable
        // the package does not contain. `prepare_lazy` cannot verify such a
        // claim by construction: no content exists yet.
        let shim_bin = context.file_structure().shims.shim_dir(&self.identifier).bin();
        if let Some(path) = process_env.get("PATH") {
            let pruned = ocx_lib::utility::path::remove_segment(path, shim_bin.as_os_str());
            process_env.set("PATH", pruned);
        }

        let resolved = process_env.resolve_command(name.as_str());
        // Two signals, because `remove_segment` compares segments exactly: the
        // bare-name fallback `resolve_command` returns when `PATH` yields
        // nothing, and — belt for a segment the strip could not recognize —
        // any answer that *resolves* back inside the shim tree. Either way the
        // claim went unfulfilled; neither may be allowed to reach `exec`.
        if resolved == std::path::Path::new(name.as_str())
            || resolves_inside(resolved.clone(), shim_bin.clone()).await?
        {
            // Reported as an unfulfilled claim rather than left to the exec's
            // bare `ENOENT`, so a wrong `binaries` claim is attributed to the
            // publisher instead of reading as a missing package.
            return Err(anyhow::Error::new(PackageErrorKind::ShimClaimUnfulfilled(Box::new(
                ShimClaim {
                    package: self.identifier.clone(),
                    name,
                },
            ))));
        }

        let error = child_process::exec(&resolved, args, process_env);
        Err(anyhow::Error::from(error).context(format!("failed to run '{}'", resolved.display())))
    }

    /// Resolves the `lazy-report` setting for this materialization.
    ///
    /// Four tiers, not five: `--lazy-report` ▸ `[package."<id>"]` ▸ toolchain
    /// ▸ `OCX_LAZY_REPORT`. Resolution happens *here* rather than in the
    /// command that composed the shim onto `PATH`, because this is the process
    /// that performs the download the setting describes — and because a value
    /// resolved now reflects what the user configured today, not what they
    /// configured when the shim was generated. The same fact is why there is
    /// no `[group.<g>]` tier: nothing on the wire says which group composed
    /// the tool, so that tier could only ever be written and never read.
    ///
    /// `project` is `None` when this process resolves no project, which is the
    /// ordinary case for a shim invoked from a directory outside one.
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

/// Whether `resolved` lands inside `shim_bin` once both paths are resolved.
///
/// The strip above compares `PATH` segments as strings, so every segment that
/// names the shim directory by a *different* string survives it: a trailing
/// slash, a symlink alias, `$OCX_HOME` spelled one way by the process that
/// composed the `PATH` and another way by this one (nothing canonicalizes it —
/// it is used as given). Resolving both sides is what collapses those spellings
/// onto one answer, which is why this covers what the strip structurally
/// cannot, rather than merely repeating it in another form.
///
/// **Fails closed.** A path that cannot be resolved counts as inside the tree:
/// the alternative is `exec`ing a path this process could not identify, and the
/// refusal it would skip is the only thing standing between a false `binaries`
/// claim and an `execve` loop with no depth counter.
///
/// [`dunce::canonicalize`] rather than [`std::fs::canonicalize`], per the
/// project's cross-platform path rule: the latter yields Windows verbatim
/// (`\\?\`) paths, and a prefix comparison is only as good as both sides
/// agreeing on a spelling. It blocks, hence the blocking pool — `execvp` being
/// two statements away does not make a stray `stat` on the reactor correct.
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
/// A shim is invoked as a bare `PATH` lookup from wherever the user happens to
/// be, so the ordinary precedence chain (`--global` ▸ `--project` ▸
/// `OCX_PROJECT` ▸ CWD walk) is the only sensible reading of "which project
/// configured this". No project, an unreadable one, or a malformed one all
/// resolve to `None` with a debug log rather than a failure: the value decides
/// nothing but whether a progress bar renders, and refusing to run a user's
/// tool over it would be absurd.
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
/// The wire value is always fully qualified, so it goes through
/// [`Identifier::parse`] rather than the default-registry form: a shim body is
/// written by ocx and must not depend on the ambient default registry of
/// whatever shell later runs it. A value that fails either step is a clap
/// invalid-value error, so a malformed shim body exits 64 and names the field.
fn parse_pinned_identifier(value: &str) -> Result<PinnedIdentifier, String> {
    let identifier = Identifier::parse(value).map_err(|error| error.to_string())?;
    PinnedIdentifier::try_from(identifier).map_err(|error| error.to_string())
}

/// Validates the wire's `argv0` against both legs of the name contract.
///
/// `claimed` is the deferred tool's composed name set, read from the shim
/// directory's own `bin/` listing: the generated launchers **are** that set,
/// one file per claimed name. Re-deriving it from the ref-linked config blobs
/// would cost a closure walk on every first invocation and buy nothing —
/// whoever can write the shim store controls the body and the name equally, so
/// the trust boundary this crosses is the one C-011 already concedes.
///
/// # Errors
///
/// - [`PackageErrorKind::ShimNameInvalid`] when `argv0` does not satisfy the
///   [`BinaryName`] grammar, which forbids `/`, `\` and the Windows-reserved
///   device names.
/// - [`PackageErrorKind::ShimNameNotClaimed`] when `argv0` is well-formed but
///   `package` claims no such name.
// `PackageErrorKind` is 128 bytes — exactly clippy's ceiling — and `BinaryName`
// is one `String`, so the `Err` variant dominates the `Ok` one and the lint
// fires where it does not for the manager's own `Result<InstallInfo, _>`
// returns. Boxing here would give this one signature a shape no sibling
// refusal has, to shrink a path taken only on a malformed shim invocation; the
// house answer is the same `allow` the seven cold error paths in `ocx_lib`
// already carry.
#[allow(clippy::result_large_err)]
fn validate_argv0(
    argv0: &str,
    package: &PinnedIdentifier,
    claimed: &BTreeSet<BinaryName>,
) -> Result<BinaryName, PackageErrorKind> {
    // Grammar first. It is the security leg — a value carrying `/` or `\` would
    // bypass `PATH` resolution entirely — and asking membership first would
    // report an ill-formed name as merely unclaimed, describing the wrong
    // defect. The type makes the order structural too: membership cannot be
    // asked until a `BinaryName` exists.
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
    //! Written from `plan_lazy_package_loading.md`, never from the bodies
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
    /// The predicate is not a restatement of the number: `cli::clap::parse`
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
            assert_eq!(parsed.lazy_report.mode(), Some(ocx_lib::lazy::LazyReport::Progress));
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

    fn pinned() -> PinnedIdentifier {
        let identifier = Identifier::parse(PINNED).expect("fixture parses");
        PinnedIdentifier::try_from(identifier).expect("fixture is digest-bearing")
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
        assert_eq!(parsed.report(Some(&config)), ocx_lib::lazy::LazyReport::Progress);
    }

    #[test]
    fn report_prefers_the_package_entry_over_the_toolchain_tier() {
        let config =
            project("lazy-report = \"silent\"\n\n[package.\"ocx.sh/tool/cmake\"]\nlazy-report = \"progress\"\n");
        let parsed = shim(&[], &[PINNED, "--", "cmake"]);
        assert_eq!(parsed.report(Some(&config)), ocx_lib::lazy::LazyReport::Progress);
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
            ocx_lib::lazy::LazyReport::Progress,
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
            ocx_lib::lazy::LazyReport::Progress,
            "an unrelated package entry must not outrank the toolchain tier"
        );
    }

    #[test]
    fn report_falls_back_to_the_toolchain_tier() {
        let config = project("lazy-report = \"progress\"\n\n[tools]\ncmake = \"ocx.sh/tool/cmake:3.28\"\n");
        let parsed = shim(&[], &[PINNED, "--", "cmake"]);
        assert_eq!(parsed.report(Some(&config)), ocx_lib::lazy::LazyReport::Progress);
    }
}
