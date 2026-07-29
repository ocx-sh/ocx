// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The policy seam every tool launch goes through: probe, emit, spawn.
//!
//! Recording used to be three near-identical call sites, which meant a future
//! exec-ish command could simply forget it — a hazard already realised twice,
//! since five spawn sites exist today. Routing a launch through [`Launch`] turns
//! forgetting into a compile error rather than a test finding: a launch cannot
//! be constructed without deciding what it records.
//!
//! Three properties keep a new command *on* the seam, and all three are
//! load-bearing:
//!
//! - [`Launch`]'s fields are private and it is built through constructors, so
//!   there is no struct literal to slip a default into.
//! - Recording state is not caller-chosen. It arrives as a
//!   [`RecordingPolicy`](crate::record::RecordingPolicy), which only
//!   `record::policy::resolve_records` can mint — a caller cannot fabricate
//!   "recording off", only configuration can. The sanctioned exemptions are
//!   bounded by the same value: [`exemption_allowed`] refuses one under a
//!   fail-closed posture over a live sink, so a frame that *can* claim an
//!   exemption still cannot take one the operator's policy contradicts.
//! - The spawn primitives live in a **private** submodule of this one, so the
//!   raw `execvp`/spawn calls are unreachable outside the seam. Without that a
//!   new command could skip [`Launch`] entirely and still compile.
//!
//! A recording launch is also derived *from* its record rather than described
//! beside it: the executable and arguments come out of
//! [`RecordInputs`](crate::record::RecordInputs), so the record cannot name one
//! binary while ocx runs another. Resolution stays at the call site, which
//! resolves once and hands the result to both.
//!
//! Two escapes types cannot close each get a structural test:
//! `no_process_spawn_outside_launch` catches a new command reaching a spawn
//! primitive directly, and `every_launch_exemption_is_enumerated` catches one
//! reusing an existing [`ExemptionReason`] — which adds no variant and would
//! otherwise compile clean.
//!
//! Those two are **searches over source text**, not proofs. Privacy is what
//! actually holds: `launch::child_process` is unreachable from outside this
//! module, so no other file can call the primitives it wraps. What the searches
//! add is catching a file that builds its *own* `std`/`tokio` `Command` — for
//! the spellings `SPAWN_TOKENS` recognises. A spelling nobody anticipated would
//! pass them, so read the claim as "the primitives are private and the common
//! escapes are caught", never as "spawning outside the seam is impossible".

mod child_process;

use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use chrono::Utc;

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::env::Env;
use crate::record::{self, ExecutionRecord, RecordInputs, RecordingPolicy, RecordsError, Scope};

/// A tool launch that has decided what it records.
pub struct Launch<'a> {
    env: Env,
    executable: &'a Path,
    args: &'a [String],
    mode: Mode<'a>,
}

/// Whether this launch records, kept private so the choice cannot be made at a
/// call site.
// ponytail: one `Launch` exists per process, on the stack, and is consumed
// immediately before the spawn. Boxing the recording variant to even out 255
// bytes would trade a real allocation on the exec path for a size difference
// nothing ever pays for.
#[allow(clippy::large_enum_variant, reason = "one stack value per process, consumed at once")]
enum Mode<'a> {
    Recording {
        record: RecordInputs<'a>,
        policy: &'a RecordingPolicy,
    },
    Exempt(ExemptionReason),
}

/// Why a launch does not record.
///
/// Closed by design: adding a sanctioned exclusion is a visible diff, and
/// reusing a variant for an unrelated command is what
/// `every_launch_exemption_is_enumerated` exists to catch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExemptionReason {
    /// `ocx package test` — a maintainer preview over local artifacts.
    PackageTest,
    /// `ocx patch test` — likewise.
    PatchTest,
}

impl ExemptionReason {
    /// The command a user typed, for a diagnostic that has to name it.
    fn command(self) -> &'static str {
        match self {
            Self::PackageTest => "ocx package test",
            Self::PatchTest => "ocx patch test",
        }
    }
}

impl<'a> Launch<'a> {
    /// A recording launch.
    ///
    /// The executable and arguments are taken from `record` rather than passed
    /// separately: `executable` is the one value the record actually publishes,
    /// and `args` is derived from the same `argv` input, so a second parameter
    /// for either would let the launched process disagree with what the record
    /// says ran. `argv` itself is never published — see
    /// [`RecordInputs::argv`](crate::record::RecordInputs::argv).
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::IncompleteRecordInputs`] when the frame carries no
    /// `argv` — an invariant `RecordInputs` cannot express in its types, so it is
    /// checked here rather than left to the sink.
    pub fn recording(env: Env, record: RecordInputs<'a>, policy: &'a RecordingPolicy) -> Result<Self, LaunchError> {
        let args = validate_record_inputs(&record)?;
        Ok(Self {
            env,
            executable: record.executable,
            args,
            mode: Mode::Recording { record, policy },
        })
    }

    /// A sanctioned non-recording launch, **bounded by the resolved posture**.
    ///
    /// Every call site is enumerated by `every_launch_exemption_is_enumerated`.
    ///
    /// The policy is a parameter rather than a call-site decision for the same
    /// reason [`RecordingPolicy`] has no public constructor: an exemption a
    /// frame could grant itself is an exemption anyone who can place a
    /// directory can grant themselves. The launcher's exemption is inherited
    /// from a caller-supplied pkg-root under `$OCX_HOME/temp/…`, which is
    /// inside the invoking user's own home — forgeable by construction, and a
    /// capability token cannot fix that, since parent and forger run as the
    /// same uid. What the operator *can* decide is the posture, so that is what
    /// bounds the exemption.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::ExemptionRefused`] when the resolved policy both
    /// requires recording **and** has somewhere to record: a fail-closed posture
    /// over a live sink and an exemption are a contradiction, and the
    /// contradiction is resolved in the operator's favour.
    ///
    /// `required` alone is not the test. A SYSTEM-locked `[records]` block that
    /// names no `dir` resolves to `required = true` with recording off — the
    /// operator locking recording *off* for the host — and
    /// [`resolve_records`](crate::record::resolve_records) preserves that state
    /// deliberately. Refusing on the posture alone would turn every preview on
    /// such a host into exit 74 while nothing was ever going to be recorded.
    pub fn exempt(
        env: Env,
        executable: &'a Path,
        args: &'a [String],
        reason: ExemptionReason,
        policy: &RecordingPolicy,
    ) -> Result<Self, LaunchError> {
        exemption_allowed(policy, reason)?;
        Ok(Self {
            env,
            executable,
            args,
            mode: Mode::Exempt(reason),
        })
    }
}

/// Whether `reason`'s exemption survives the resolved policy.
///
/// Split out of [`Launch::exempt`] because two of the three exempt frames never
/// reach a `Launch` at all: `ocx package test --script` and `ocx patch test
/// --script` hand control to the Starlark host, whose `ocx.run` spawns children
/// of its own (`script/ocx_module.rs`, allowlisted in
/// `no_process_spawn_outside_launch`). Those frames call this directly, before
/// the interpreter starts, so the bound holds for every preview and not merely
/// for the ones that happen to end in a `Launch`.
///
/// # Errors
///
/// Returns [`LaunchError::ExemptionRefused`] when the policy requires recording
/// and has a sink to record to. See [`Launch::exempt`] for why both halves are
/// tested rather than the posture alone.
pub fn exemption_allowed(policy: &RecordingPolicy, reason: ExemptionReason) -> Result<(), LaunchError> {
    if policy.required() && policy.is_recording() {
        return Err(LaunchError::ExemptionRefused { reason });
    }
    Ok(())
}

/// Reject a recording frame that carries no `argv`, and return the arguments the
/// child is launched with.
///
/// `RecordInputs` cannot express the invariant as a type, so it is checked at
/// construction rather than deferred to the sink: `argv` must have its `argv[0]`
/// for the remainder to be the child's arguments.
///
/// An **empty package set is not** a validation failure. `ocx run` in a project
/// whose selected scope declares no tools is a legitimate launch, and its record
/// is the truthful statement that this invocation composed nothing and reached
/// its executable from the ambient `PATH` — which is precisely the fact the
/// `sh.ocx.provenance` field exists to surface. Refusing the launch over it would
/// break a working command for a policy nobody set.
///
/// # Errors
///
/// Returns [`LaunchError::IncompleteRecordInputs`] naming the offending frame.
fn validate_record_inputs<'a>(record: &RecordInputs<'a>) -> Result<&'a [String], LaunchError> {
    // `argv` is `argv[0]` plus the arguments as invoked, so the child's
    // arguments are its tail — and an empty `argv` is a frame that resolved
    // nothing to run.
    let (_argv0, args) = record
        .argv
        .split_first()
        .ok_or_else(|| LaunchError::IncompleteRecordInputs {
            command: frame_name(&record.scope).to_string(),
        })?;
    Ok(args)
}

/// Name the launching command for a diagnostic.
///
/// Mirrors `record::execution_record`'s frame derivation, which owns the wire
/// spelling; this one is prose for an error message and deliberately reads as
/// the command a user typed.
fn frame_name(scope: &Scope) -> &'static str {
    match scope {
        Scope::Project { .. } => "ocx run",
        Scope::Package { .. } => "ocx package exec",
        Scope::Launcher => "ocx launcher exec",
    }
}

/// Replace the current process with the launched tool.
///
/// Diverges on success: on Unix via `execvp`, on Windows by spawning, waiting,
/// and exiting without running the drop chain. Only a start-up failure, or a
/// record the policy required and could not write, returns.
pub async fn exec(launch: Launch<'_>) -> LaunchError {
    let Launch {
        env,
        executable,
        args,
        mode,
    } = launch;
    match mode {
        Mode::Recording { record, policy } => {
            // On Unix the record is written before `execvp`, so the write is its
            // own gate and a probe would only duplicate it. Everywhere else the
            // pid does not exist until the child does, and the probe is what
            // keeps the fail-closed posture honest without it.
            #[cfg(not(unix))]
            if let Err(error) = probe_sink(policy).await {
                return error;
            }
            child_process::exec(executable, args, env, |pid| write_record(&record, policy, pid)).await
        }
        Mode::Exempt(reason) => {
            crate::log::debug!("launch not recorded: {reason:?}");
            child_process::exec(executable, args, env, |_| std::future::ready(Ok(()))).await
        }
    }
}

/// Spawn the launched tool and wait, so the caller can run cleanup (dropping a
/// tempdir guard, say) before propagating the exit status.
///
/// # Errors
///
/// Returns [`LaunchError`] when the child cannot be started, or when recording
/// fails under a `required` policy. A non-zero child exit is reported through
/// the returned [`ExitStatus`], not as an error.
pub async fn spawn_and_wait(launch: Launch<'_>) -> Result<ExitStatus, LaunchError> {
    let Launch {
        env,
        executable,
        args,
        mode,
    } = launch;
    match mode {
        Mode::Recording { record, policy } => {
            // This path never replaces the ocx image, so the tool's pid is
            // post-spawn on every platform and the probe is the only pre-spawn
            // gate available.
            probe_sink(policy).await?;
            child_process::spawn_and_wait(executable, args, env, |pid| write_record(&record, policy, pid)).await
        }
        Mode::Exempt(reason) => {
            crate::log::debug!("launch not recorded: {reason:?}");
            child_process::spawn_and_wait(executable, args, env, |_| std::future::ready(Ok(()))).await
        }
    }
}

/// Refuse an unwritable sink before the child starts.
///
/// Costs one create/unlink and is skipped entirely when recording is configured
/// off.
async fn probe_sink(policy: &RecordingPolicy) -> Result<(), LaunchError> {
    let Some(dir) = policy.dir() else {
        return Ok(());
    };
    match record::probe_writable(dir).await {
        Ok(()) => Ok(()),
        Err(error) => apply_posture(error, policy),
    }
}

#[cfg(test)]
thread_local! {
    /// Records [`write_record`] actually built on this thread.
    ///
    /// The guard below saves work without changing semantics — `emit` returns
    /// `Ok(None)` for a configured-off policy either way — so nothing observable
    /// separates "never built the record" from "built it and threw it away".
    /// This is that observation. Thread-local because `#[tokio::test]` drives
    /// each test's future on its own thread, and a process-wide counter would
    /// race with the tests that do record.
    static RECORDS_BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Build and publish this launch's record.
async fn write_record(inputs: &RecordInputs<'_>, policy: &RecordingPolicy, pid: u32) -> Result<(), LaunchError> {
    // The common case is no `[records] dir` anywhere in the config chain, and
    // everything below it is work whose only consumer is a record nobody asked
    // for: hostname, getcwd, getppid, the dependency-closure walk, purl
    // construction, the annotation maps. `emit` refuses a configured-off policy
    // too, but only after all of that has been paid for — twice per entrypoint
    // invocation, on every `ocx run`.
    if !policy.is_recording() {
        return Ok(());
    }

    let record = ExecutionRecord::build(inputs, Utc::now(), pid);
    #[cfg(test)]
    RECORDS_BUILT.with(|built| built.set(built.get() + 1));

    match record::emit(&record, policy).await {
        Ok(_) => Ok(()),
        Err(error) => apply_posture(error, policy),
    }
}

/// Apply the policy's failure posture to a record that could not be written.
///
/// `required` aborts the launch with exit 74; otherwise the operator gets a
/// warning and the tool still runs — a developer who fat-fingered
/// `--records-dir` once should not have their build die for a policy nobody
/// set.
///
/// The refusal names the policy rather than only the sink, because nobody may
/// have written `required = true` anywhere: it is the SYSTEM clamp's default.
/// Without the name, a dead build reads as an unexplained permission error.
fn apply_posture(error: RecordsError, policy: &RecordingPolicy) -> Result<(), LaunchError> {
    if policy.required() {
        return Err(LaunchError::Records(error));
    }
    // The variant's own `Display` names the sink but not why it failed, and the
    // cause ("permission denied") is the actionable half — so the whole chain is
    // rendered rather than just its head.
    let chain = std::iter::successors(Some(&error as &dyn std::error::Error), |cause| cause.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");
    crate::log::warn!("{chain}");
    Ok(())
}

/// A launch could not be constructed, recorded, or started.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LaunchError {
    /// The tool could not be started.
    ///
    /// Replaces the identical `failed to run '{}'` context string the three exec
    /// sites each carried.
    #[error("failed to run '{resolved}'", resolved = .resolved.display())]
    Spawn {
        /// The resolved executable that could not be started.
        resolved: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// A recording frame supplied no `argv`, so it named nothing to run.
    #[error("recording frame '{command}' carries no argv")]
    IncompleteRecordInputs {
        /// The frame that failed validation.
        command: String,
    },

    /// A launch claimed a recording exemption under a fail-closed policy.
    ///
    /// Names the policy for the same reason [`Self::Records`] does — nobody may
    /// have written `required = true` anywhere, since it is the SYSTEM clamp's
    /// default — and names the command, because the only remediation is to run
    /// a different one or change the policy.
    #[error(
        "launch refused by [records] required = true in the resolved config chain: {command} does not record, and a fail-closed policy grants no exemption",
        command = .reason.command()
    )]
    ExemptionRefused {
        /// The preview command whose exemption was refused.
        reason: ExemptionReason,
    },

    /// A record could not be written and the policy is `required`.
    ///
    /// Names the policy, because the wrapped error names only the sink — and
    /// under the SYSTEM clamp `required` defaults to `true`, so the setting that
    /// killed the build may appear in nobody's config file.
    #[error("launch refused by [records] required = true in the resolved config chain")]
    Records(#[from] crate::record::RecordsError),
}

impl ClassifyExitCode for LaunchError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Defer to the wrapped `io::Error` so a permission-denied spawn
            // still reaches 77 and everything else falls through to the generic
            // failure — the behaviour the three exec sites already had before
            // they were folded into this seam.
            Self::Spawn { .. } => None,
            // A frame reached the seam with nothing to record. That is a wiring
            // fault in ocx, not something an operator can configure their way
            // out of, so it takes the generic code rather than a diagnostic one
            // that would send them looking at their config.
            Self::IncompleteRecordInputs { .. } => Some(ExitCode::Failure),
            // The same 74 an unwritable sink takes under the same posture: from
            // a wrapper script's side both are one condition — this invocation
            // could not be recorded and the operator said not to run it.
            Self::ExemptionRefused { .. } => Some(ExitCode::IoError),
            // The wrapped error owns the split between 74 (unwritable sink) and
            // 78 (malformed template), so the classification is delegated to it
            // rather than restated here and left to drift.
            Self::Records(e) => e.classify(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::env::OcxConfigView;
    use crate::file_structure::PackageDir;
    use crate::oci::{Digest, Identifier, PinnedIdentifier};
    use crate::package::install_info::InstallInfo;
    use crate::package::resolved_package::ResolvedPackage;
    use crate::package_manager::tasks::resolve::AdmittedBinaries;
    use crate::record::RecordsOptions;

    const HEX: &str = "3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0";

    /// Owned fixture data, so a [`RecordInputs`] can borrow from one place.
    struct Frame {
        packages: Vec<Arc<InstallInfo>>,
        admitted: AdmittedBinaries,
        executable: PathBuf,
        store_root: PathBuf,
        argv: Vec<String>,
        config: OcxConfigView,
        auto_installed: Vec<Identifier>,
    }

    impl Frame {
        fn new(packages: usize, argv: &[&str]) -> Self {
            let identifier = PinnedIdentifier::try_from(
                Identifier::new_registry("ocx/cmake", "index.ocx.sh")
                    .clone_with_digest(Digest::Sha256(HEX.to_string())),
            )
            .expect("digest present");
            let install = InstallInfo::new(
                identifier,
                serde_json::from_str(r#"{"type":"bundle","version":1,"env":[]}"#).expect("bundle metadata"),
                ResolvedPackage {
                    dependencies: Vec::new(),
                },
                PackageDir::with_root(PathBuf::from("/store/cmake")),
            );
            Self {
                packages: std::iter::repeat_n(Arc::new(install), packages).collect(),
                admitted: AdmittedBinaries::default(),
                executable: PathBuf::from("/store/cmake/content/bin/cmake"),
                store_root: PathBuf::from("/store"),
                argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
                config: OcxConfigView::new("/opt/ocx/bin/ocx"),
                auto_installed: Vec::new(),
            }
        }

        /// Point the frame at a real program, so a launch built from it runs
        /// something instead of failing to start.
        fn running(mut self, executable: &str) -> Self {
            self.executable = PathBuf::from(executable);
            self
        }

        fn inputs(&self) -> RecordInputs<'_> {
            RecordInputs {
                packages: &self.packages,
                admitted: &self.admitted,
                executable: &self.executable,
                store_root: &self.store_root,
                argv: &self.argv,
                config: &self.config,
                managed_config_digest: None,
                platform: None,
                clean_env: false,
                auto_installed: &self.auto_installed,
                scope: Scope::Launcher,
            }
        }
    }

    fn policy(dir: Option<PathBuf>, required: bool) -> RecordingPolicy {
        record::resolve_records(
            RecordsOptions {
                dir,
                required: Some(required),
                ..RecordsOptions::default()
            },
            RecordsOptions::default(),
            RecordsOptions::default(),
        )
        .expect("the default name template parses")
    }

    /// The fail-closed posture with no sink to write to.
    ///
    /// Minted through the SYSTEM lock, because writing `required = true` beside
    /// no `dir` is a configuration error now
    /// ([`RecordsError::RequiredWithoutSink`]) — it is how an operator asks for
    /// recording and would otherwise get none. A locked block with no `dir` is
    /// the shape that still resolves, and the real fleet one: an operator
    /// locking recording *off* for the host.
    fn required_policy_without_a_sink() -> RecordingPolicy {
        let mut config = RecordsOptions::default();
        config.lock_as_system();
        record::resolve_records(config, RecordsOptions::default(), RecordsOptions::default())
            .expect("a locked block with no sink resolves to recording off")
    }

    // ── the record and the launch cannot disagree ────────────────────────────

    #[test]
    fn a_recording_launch_runs_what_its_record_publishes() {
        let frame = Frame::new(1, &["cmake", "--build", "build"]);
        let policy = policy(None, false);
        let launch = Launch::recording(Env::clean(), frame.inputs(), &policy).expect("a complete frame");

        assert_eq!(launch.executable, frame.executable);
        assert_eq!(launch.args, ["--build".to_string(), "build".to_string()]);
    }

    /// A project whose selected scope declares no tools still launches. The
    /// record is then the truthful statement that this invocation composed
    /// nothing and reached its executable from the ambient `PATH` — refusing it
    /// would break a working `ocx run` for a policy nobody set.
    #[test]
    fn a_frame_that_composed_no_packages_still_launches() {
        let frame = Frame::new(0, &["env"]);
        let policy = policy(None, false);
        let launch = Launch::recording(Env::clean(), frame.inputs(), &policy)
            .expect("an empty toolchain is a legitimate launch, not a broken frame");

        assert!(launch.args.is_empty());
    }

    #[test]
    fn an_incomplete_frame_takes_the_generic_exit_code() {
        let error = LaunchError::IncompleteRecordInputs {
            command: "ocx run".to_string(),
        };

        // A wiring fault in ocx, not something an operator can configure their
        // way out of, so it must not take a diagnostic code that would send them
        // looking at their config.
        assert_eq!(error.classify(), Some(ExitCode::Failure));
    }

    #[test]
    fn a_frame_with_no_argv_is_refused() {
        let frame = Frame::new(1, &[]);
        let policy = policy(None, false);
        let error = Launch::recording(Env::clean(), frame.inputs(), &policy)
            .err()
            .expect("a frame with no argv resolved nothing to run");

        assert!(matches!(error, LaunchError::IncompleteRecordInputs { .. }));
    }

    // ── an exemption is bounded by the posture ───────────────────────────────

    /// A fail-closed posture and an exemption are a contradiction, and it is
    /// resolved in the operator's favour.
    ///
    /// The exemption is not a secret: `ocx launcher exec` grants it for a
    /// caller-supplied pkg-root under `$OCX_HOME/temp/test/`, which sits in the
    /// invoking user's own home — copy an installed package tree there and the
    /// launch is exempt. A capability token cannot close that (parent and forger
    /// are the same uid), so what bounds it is the one thing the caller does not
    /// control: the resolved policy.
    #[test]
    fn an_exemption_is_refused_under_a_required_policy() {
        let frame = Frame::new(1, &["hello"]);
        let policy = policy(Some(PathBuf::from("/var/log/ocx/records")), true);

        for reason in [ExemptionReason::PackageTest, ExemptionReason::PatchTest] {
            let error = Launch::exempt(Env::clean(), &frame.executable, &[], reason, &policy)
                .err()
                .expect("a required policy grants no exemption");

            assert!(matches!(error, LaunchError::ExemptionRefused { reason: got } if got == reason));
            assert_eq!(
                error.classify(),
                Some(ExitCode::IoError),
                "the same code an unwritable sink takes under the same posture"
            );
            let rendered = error.to_string();
            assert!(
                rendered.contains("[records] required = true"),
                "the refusal must name the policy that refused it, not just a condition: {rendered}"
            );
            assert!(
                rendered.contains(reason.command()),
                "and the command whose exemption was refused: {rendered}"
            );
        }
    }

    /// The discriminator: without a live fail-closed sink the exemption stands,
    /// so the rule above bounds it rather than deleting it.
    ///
    /// Three shapes a preview meets in practice — a sink configured warn-only,
    /// no sink at all, and the one that makes `required` alone the wrong test: a
    /// SYSTEM-locked `[records]` block naming no `dir`, which resolves to
    /// `required = true` with recording off. That is an operator locking
    /// recording *off* for the host, and a preview there must still run.
    #[test]
    fn an_exemption_stands_when_the_policy_does_not_require_recording() {
        let frame = Frame::new(1, &["hello"]);
        for policy in [
            policy(Some(PathBuf::from("/var/log/ocx/records")), false),
            policy(None, false),
            required_policy_without_a_sink(),
        ] {
            Launch::exempt(
                Env::clean(),
                &frame.executable,
                &[],
                ExemptionReason::PackageTest,
                &policy,
            )
            .expect("a maintainer preview still runs unrecorded when no policy demands otherwise");
        }
    }

    // ── failure posture follows the policy, not the call site ────────────────

    /// A sink path under a fresh tempdir that was never created.
    ///
    /// Canonicalized so the path the test names is the path the policy pins:
    /// macOS puts `$TMPDIR` behind a symlink (`/var` → `/private/var`), and a
    /// raw tempdir path would not compare equal to the designated sink there.
    fn absent_sink(root: &tempfile::TempDir) -> PathBuf {
        dunce::canonicalize(root.path())
            .expect("a just-created tempdir resolves")
            .join("never-created")
    }

    #[tokio::test]
    async fn an_unwritable_sink_refuses_a_required_launch() {
        let root = tempfile::tempdir().expect("tempdir");
        let policy = policy(Some(absent_sink(&root)), true);

        let error = probe_sink(&policy)
            .await
            .expect_err("a sink that cannot be written refuses the launch before anything starts");
        assert_eq!(error.classify(), Some(ExitCode::IoError));
    }

    #[tokio::test]
    async fn an_unwritable_sink_only_warns_when_recording_is_not_required() {
        let root = tempfile::tempdir().expect("tempdir");
        let policy = policy(Some(absent_sink(&root)), false);

        probe_sink(&policy)
            .await
            .expect("without an operator policy a bad sink warns and the tool still runs");
    }

    /// A sink **substituted after the operator designated it** must be refused
    /// where the record is actually written, not only in the pre-spawn probe:
    /// that probe is `#[cfg(not(unix))]` on the `exec` path, so on Linux and
    /// macOS — every production launch — swapping the designated directory for a
    /// symlink would silently relocate the whole audit trail with exit 0 and no
    /// warning.
    ///
    /// A path that is *already* a symlink when the operator names it is their
    /// choice, not an attack; it is pinned as designated. Substitution is the
    /// only thing this refuses, so the sink here is a real directory at
    /// designation time and only becomes a link afterwards.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sink_substituted_after_designation_is_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(root.path()).expect("a just-created tempdir resolves");
        let sink = root.join("sink");
        std::fs::create_dir(&sink).expect("create the designated sink");

        let frame = Frame::new(1, &["cmake", "--version"]);
        let policy = policy(Some(sink.clone()), true);
        let pinned = policy.dir().expect("a designated sink").to_path_buf();

        // The swap: the designated directory is replaced by a link elsewhere,
        // which is where an unrefused record would end up.
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("create the substituted target");
        std::fs::remove_dir(&sink).expect("remove the designated sink");
        std::os::unix::fs::symlink(&elsewhere, &sink).expect("substitute the sink");

        let error = write_record(&frame.inputs(), &policy, 4711)
            .await
            .expect_err("a sink substituted after designation must refuse the record");
        assert!(
            matches!(&error, LaunchError::Records(RecordsError::SinkSymlink { path }) if *path == pinned),
            "expected a refusal naming the pinned sink ({}), got: {error:?}",
            pinned.display()
        );
        assert!(
            std::fs::read_dir(&elsewhere)
                .expect("read the substituted target")
                .next()
                .is_none(),
            "no record may be written through the substituted link"
        );
    }

    /// Recording off must cost nothing, not "cost everything and then discard
    /// it": the whole record — hostname, getcwd, the closure walk, purls — used
    /// to be assembled before `emit` looked at the sink.
    #[tokio::test]
    async fn recording_configured_off_builds_no_record() {
        let frame = Frame::new(1, &["cmake", "--version"]);
        // `required` is deliberately true: the guard must not turn "nothing to
        // do" into a refusal.
        let policy = required_policy_without_a_sink();

        let before = RECORDS_BUILT.with(|built| built.get());
        write_record(&frame.inputs(), &policy, 4711)
            .await
            .expect("a configured-off policy has nothing to write and nothing to fail");

        assert_eq!(
            RECORDS_BUILT.with(|built| built.get()),
            before,
            "no sink means the record must never be assembled in the first place"
        );
    }

    /// A refused launch has to name the setting that refused it. The wrapped
    /// error names the sink and the OS cause; neither points at a policy, and
    /// under the SYSTEM clamp nobody wrote `required = true` anywhere.
    #[test]
    fn a_required_refusal_names_the_policy_that_refused_it() {
        let error = LaunchError::Records(RecordsError::Io {
            path: PathBuf::from("/var/log/ocx/records"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        });

        let rendered = error.to_string();
        assert!(
            rendered.contains("[records] required = true"),
            "the refusal must name the setting a developer has to go and find: {rendered}"
        );
        assert!(
            rendered.contains("config chain"),
            "and where it comes from, since it may be in no file they own: {rendered}"
        );
        // Naming the policy must not cost the fail-closed exit code, which the
        // wrapped error still owns.
        assert_eq!(error.classify(), Some(ExitCode::IoError));
    }

    #[tokio::test]
    async fn recording_configured_off_probes_nothing() {
        // `required` is deliberately true: with no sink there is nothing to
        // fail, so the fail-closed posture must not manufacture a failure.
        let policy = required_policy_without_a_sink();

        probe_sink(&policy)
            .await
            .expect("nothing to probe when recording is off");
    }

    // ── the recording wait path, which has no caller yet ─────────────────────
    //
    // `spawn_and_wait`'s recording arm is reachable only from these two tests:
    // both production callers launch exempt. It carries the pre-spawn probe the
    // non-Unix fail-closed posture depends on, so without them it would rot
    // silently and the seam would re-grow wrong when a caller does appear.

    /// A real program with an observable side effect, so "did the tool run?" is
    /// a filesystem question rather than an inference from an exit status.
    #[cfg(unix)]
    const MKDIR: &str = "/bin/mkdir";

    #[cfg(unix)]
    #[tokio::test]
    async fn a_required_launch_is_refused_before_the_tool_starts() {
        let root = tempfile::tempdir().expect("tempdir");
        let marker = root.path().join("ran");
        let frame = Frame::new(1, &["mkdir", &marker.to_string_lossy()]).running(MKDIR);
        let policy = policy(Some(absent_sink(&root)), true);
        let launch = Launch::recording(Env::clean(), frame.inputs(), &policy).expect("a complete frame");

        let error = spawn_and_wait(launch)
            .await
            .expect_err("an unwritable sink under a required policy refuses the launch");

        assert_eq!(error.classify(), Some(ExitCode::IoError));
        assert!(
            !marker.exists(),
            "the probe is the only pre-spawn gate on this path — the tool must not have run"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_recording_launch_publishes_one_record_and_returns_the_child_status() {
        let root = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(root.path()).expect("a just-created tempdir resolves");
        let sink = root.join("sink");
        std::fs::create_dir(&sink).expect("create the sink");
        let marker = root.join("ran");

        let frame = Frame::new(1, &["mkdir", &marker.to_string_lossy()]).running(MKDIR);
        let policy = policy(Some(sink.clone()), true);
        let launch = Launch::recording(Env::clean(), frame.inputs(), &policy).expect("a complete frame");

        let status = spawn_and_wait(launch).await.expect("the launch is not refused");

        assert!(status.success(), "the child's status is returned, not swallowed");
        assert!(marker.exists(), "the tool ran with the arguments its record publishes");
        assert_eq!(
            std::fs::read_dir(&sink).expect("read the sink").count(),
            1,
            "exactly one record per launch"
        );
    }

    #[test]
    fn a_spawn_failure_defers_its_exit_code_to_the_io_error() {
        let error = LaunchError::Spawn {
            resolved: PathBuf::from("/store/cmake/content/bin/cmake"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };

        // `None` hands the decision to the chain walker, which reaches the
        // `io::Error` and its 77 — the behaviour the three exec sites had before
        // they were folded into this seam.
        assert_eq!(error.classify(), None);
    }
}

#[cfg(test)]
mod firewall_tests {
    use std::path::{Path, PathBuf};

    // ── Structural firewalls around the launch seam ──────────────────────────
    //
    // Two escapes the type system cannot close, and one test cannot do both:
    // reaching a spawn primitive without going through `launch`, and reusing an
    // existing `ExemptionReason` for a command it was not sanctioned for.
    // Modelled on `script.rs`'s `no_starlark_import_outside_firewall`.

    /// Tokens that mean "this file can start a process".
    ///
    /// A `Command` has to be named to be built, and these cover every spelling
    /// that names one: `process::Command` for the qualified constructor and the
    /// plain import, `process::{` for a braced or aliased one
    /// (`use tokio::process::{Command as RawCommand}` — the form that matches
    /// neither of the others), `process as ` for a renamed module, and
    /// `CommandExt` for the `execvp` trait. Deliberately over-broad: a file
    /// importing anything at all from a `process` module through a brace list
    /// matches, and paying for that with one honest allowlist line is cheaper
    /// than a token set that a rename slips past.
    const SPAWN_TOKENS: &[&str] = &["process::Command", "process::{", "process as ", "CommandExt"];

    /// Files sanctioned to spawn a process without going through `launch`.
    ///
    /// The firewall's subject is a **tool launch**: running a program that this
    /// invocation resolved out of a package and composed an environment for.
    /// That is what an execution record describes, and it is the only thing
    /// `launch` owns. Every entry below spawns a program *ocx itself* chose for
    /// its own purposes, so none of them is a tool launch — but each is listed
    /// by name, because a blanket allowlist would be worse than no firewall.
    const SPAWN_ALLOWED: &[(&str, &str)] = &[
        (
            "ocx_cli/src/app/plugin_dispatch.rs",
            "git-style `ocx-<name>` plugin dispatch — an ocx extension, not a resolved package tool, \
             and not one of the three recording frames (adr_cli_plugin_pattern.md)",
        ),
        (
            "ocx_cli/tests/linux_self_contained.rs",
            "integration test running `ldd` against the built binary",
        ),
        (
            "ocx_cli/tests/macos_self_contained.rs",
            "integration test running `otool` against the built binary",
        ),
        (
            "ocx_lib/src/codesign.rs",
            "fixed macOS system utilities (`codesign`, `xattr`, `cc` in tests) during extraction",
        ),
        (
            "ocx_lib/src/oci/host_capabilities.rs",
            "libc detection — runs a discovered loader with `--version` to classify its banner",
        ),
        (
            "ocx_lib/src/package_manager/tasks/update_check.rs",
            "hermetic self re-entry (`ocx --format json version`) to read the installed version",
        ),
        (
            "ocx_lib/src/script/ocx_module.rs",
            "the Starlark host's `ocx.run`, reachable only from `package test` / `patch test`. Its \
             exemption is inherited from those frames, not independent — and since it never reaches \
             a `Launch`, both call sites apply the bound themselves via `launch::exemption_allowed` \
             before starting the interpreter",
        ),
        (
            "ocx_lib/src/setup/profiles.rs",
            "shell detection — asks a candidate shell what it is",
        ),
        (
            "ocx_lib/src/shell.rs",
            "test-only round-trip harness that sources generated export lines in a real shell",
        ),
    ];

    /// Tokens that mean "this file claims a recording exemption".
    const EXEMPTION_TOKENS: &[&str] = &["Launch::exempt", "ExemptionReason::"];

    /// The commands sanctioned not to record, one file each.
    ///
    /// `package test` and `patch test` are maintainer previews over local
    /// unpublished artifacts: a record from them would describe something that
    /// was never published.
    ///
    /// Checked as an **equality**, not a subset: a new command quietly reusing a
    /// variant adds no variant and compiles clean, and a sanctioned site that
    /// silently stops claiming its exemption leaves an entry here promising a
    /// hole that no longer exists. Both directions are drift.
    const EXEMPTION_ALLOWED: &[&str] = &[
        // The launcher re-entry claims no exemption of its own: it INHERITS one
        // from the pkg-root it was baked with, and only from the two scratch
        // roots the commands below own. A fresh process re-reads `[records]`
        // from its own config chain, so an exemption declared at those commands'
        // spawn sites does not survive the hop — which is exactly the hole this
        // entry closes rather than opens.
        "ocx_cli/src/command/launcher/exec.rs",
        "ocx_cli/src/command/package_test.rs",
        "ocx_cli/src/command/patch_test.rs",
        // Declares the enum and this test's own allowlist.
        "ocx_lib/src/launch.rs",
    ];

    fn crates_root() -> PathBuf {
        // CARGO_MANIFEST_DIR = crates/ocx_lib → parent = crates/.
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate manifest dir has a parent (crates/)")
            .to_path_buf()
    }

    fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().and_then(|name| name.to_str()) == Some("target") {
                    continue;
                }
                collect_rs(&path, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// Every `.rs` file under `crates/`, as `crates/`-relative slash-separated
    /// paths paired with their contents.
    fn sources() -> Vec<(String, String)> {
        let root = crates_root();
        let mut files = Vec::new();
        collect_rs(&root, &mut files);
        assert!(!files.is_empty(), "expected to find Rust sources under {root:?}");

        files
            .iter()
            .filter_map(|path| {
                let relative = path.strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
                Some((relative, std::fs::read_to_string(path).ok()?))
            })
            .collect()
    }

    /// Whether `content` uses any of `tokens` outside a comment.
    ///
    /// Comment lines are skipped so the allowlists name real call sites only —
    /// several modules mention these primitives in prose.
    fn mentions(content: &str, tokens: &[&str]) -> bool {
        content
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .any(|line| tokens.iter().any(|token| line.contains(token)))
    }

    /// The seam itself: the module that owns the primitives, and this file,
    /// whose allowlist quotes the tokens it searches for.
    fn is_seam(path: &str) -> bool {
        path == "ocx_lib/src/launch.rs" || path.starts_with("ocx_lib/src/launch/")
    }

    #[test]
    fn every_allowlisted_file_still_exists() {
        let present: Vec<String> = sources().into_iter().map(|(path, _)| path).collect();
        let mut stale: Vec<&str> = SPAWN_ALLOWED
            .iter()
            .map(|(path, _)| *path)
            .chain(EXEMPTION_ALLOWED.iter().copied())
            .filter(|allowed| !present.iter().any(|path| path == allowed))
            .collect();
        stale.sort_unstable();

        assert!(
            stale.is_empty(),
            "an allowlist entry names a file that no longer exists, so the firewall it opened is \
             wider than anyone reading it would think:\n{}",
            stale.join("\n")
        );
    }

    /// The bypass the token set was widened for: a braced or renamed import
    /// spells the constructor in a way `process::Command` never sees, so a new
    /// command could spawn, compile, and pass the firewall.
    ///
    /// Fixtures rather than real files, because the point is the *detector* —
    /// the real-source sweep below can only ever prove that nothing in the tree
    /// spells it that way today.
    #[test]
    fn a_renamed_command_import_is_still_caught() {
        let bypasses = [
            "use tokio::process::{Command as RawCommand};\nRawCommand::new(\"sh\").spawn()",
            "use std::process::{Command, Stdio};\nCommand::new(\"sh\").status()",
            "use std::process as proc;\nproc::Command::new(\"sh\").output()",
            "use tokio::process::Command as RawCommand;\nRawCommand::new(\"sh\").spawn()",
            "std::process::Command::new(\"sh\").spawn()",
        ];
        for source in bypasses {
            assert!(
                mentions(source, SPAWN_TOKENS),
                "a spawn spelled this way slips past the firewall:\n{source}"
            );
        }

        // The discriminator: neither a comment about spawning nor an unrelated
        // `process` import may trip it, or the allowlist fills with noise and
        // stops naming anything.
        for benign in [
            "// this module never builds a process::Command",
            "use std::process::ExitCode;\nExitCode::SUCCESS",
            "let status = response.status();",
        ] {
            assert!(
                !mentions(benign, SPAWN_TOKENS),
                "the firewall must not fire on:\n{benign}"
            );
        }
    }

    #[test]
    fn no_process_spawn_outside_launch() {
        let mut violations: Vec<String> = sources()
            .into_iter()
            .filter(|(path, _)| !is_seam(path))
            .filter(|(path, _)| !SPAWN_ALLOWED.iter().any(|(allowed, _)| allowed == path))
            .filter(|(_, content)| mentions(content, SPAWN_TOKENS))
            .map(|(path, _)| path)
            .collect();
        violations.sort();

        assert!(
            violations.is_empty(),
            "a spawn primitive is reachable outside the launch seam:\n{}\n\n\
             Route the launch through `ocx_lib::launch` so it decides what it records. If this \
             genuinely is not a tool launch, add it to `SPAWN_ALLOWED` with the reason.",
            violations.join("\n")
        );
    }

    #[test]
    fn every_launch_exemption_is_enumerated() {
        let mut claimants: Vec<String> = sources()
            .into_iter()
            .filter(|(_, content)| mentions(content, EXEMPTION_TOKENS))
            .map(|(path, _)| path)
            .collect();
        claimants.sort();

        let mut unsanctioned: Vec<&str> = claimants
            .iter()
            .map(String::as_str)
            .filter(|path| !EXEMPTION_ALLOWED.contains(path))
            .collect();
        unsanctioned.sort_unstable();
        assert!(
            unsanctioned.is_empty(),
            "a recording exemption is claimed outside the sanctioned commands:\n{}\n\n\
             Reusing an existing `ExemptionReason` adds no variant and compiles clean, which is \
             what this test exists to catch. A new exclusion needs its own variant and its own \
             entry in `EXEMPTION_ALLOWED`.",
            unsanctioned.join("\n")
        );

        let mut silent: Vec<&str> = EXEMPTION_ALLOWED
            .iter()
            .copied()
            .filter(|allowed| !claimants.iter().any(|path| path == allowed))
            .collect();
        silent.sort_unstable();
        assert!(
            silent.is_empty(),
            "a sanctioned exclusion no longer claims one:\n{}\n\n\
             Either the command now records — in which case drop its entry — or it reached a \
             spawn primitive some other way, which is the hole this allowlist claims does not \
             exist.",
            silent.join("\n")
        );
    }
}
