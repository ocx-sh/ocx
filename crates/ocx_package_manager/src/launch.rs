// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The policy seam every tool launch goes through: probe, emit, spawn.
//!
//! The spawn primitives stay in a private submodule so no launch can skip deciding what it records
//! (`adr_exec_resolution_record.md` § "Rationale from code: launch").

#[expect(
    clippy::disallowed_types,
    reason = "the seam: the one module that builds the Command a tool launch runs"
)]
mod child_process;

use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use chrono::Utc;

use crate::record::{self, ExecutionRecord, RecordInputs, RecordingPolicy, RecordsError, Scope};
use ocx_config::env::Env;

/// A tool launch that has decided what it records.
pub struct Launch<'a> {
    env: Env,
    executable: &'a Path,
    args: &'a [String],
    mode: Mode<'a>,
}

/// Whether this launch records, kept private so the choice cannot be made at a call site.
// ponytail: unboxed large variant; one `Launch` per process, consumed at once, so boxing buys nothing.
#[allow(clippy::large_enum_variant, reason = "one stack value per process, consumed at once")]
enum Mode<'a> {
    Recording {
        record: RecordInputs<'a>,
        policy: &'a RecordingPolicy,
    },
    Exempt(ExemptionReason),
}

/// Why a launch does not record; clippy bans [`Launch::exempt`], so each variant's call sites carry an `#[expect]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExemptionReason {
    /// `ocx package test` — a maintainer preview over local artifacts.
    PackageTest,
    /// `ocx patch test` — likewise.
    PatchTest,
}

impl ExemptionReason {
    fn command(self) -> &'static str {
        match self {
            Self::PackageTest => "ocx package test",
            Self::PatchTest => "ocx patch test",
        }
    }
}

impl<'a> Launch<'a> {
    /// A recording launch, running the executable and arguments `record` names.
    ///
    /// # Errors
    ///
    /// [`LaunchError::IncompleteRecordInputs`] when the frame carries no `argv`.
    // No separate executable/args parameters: they would let the launched process disagree with the record.
    pub fn recording(env: Env, record: RecordInputs<'a>, policy: &'a RecordingPolicy) -> Result<Self, LaunchError> {
        let args = validate_record_inputs(&record)?;
        Ok(Self {
            env,
            executable: record.executable,
            args,
            mode: Mode::Recording { record, policy },
        })
    }

    /// A sanctioned non-recording launch, bounded by the resolved policy.
    ///
    /// # Errors
    ///
    /// [`LaunchError::ExemptionRefused`] when the policy is `required` and has a sink to record to.
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

/// Whether `reason`'s exemption survives the resolved policy; `--script` frames call this before
/// the Starlark host spawns children of its own.
///
/// # Errors
///
/// [`LaunchError::ExemptionRefused`] when the policy is `required` and has a sink to record to.
pub fn exemption_allowed(policy: &RecordingPolicy, reason: ExemptionReason) -> Result<(), LaunchError> {
    // Not `required` alone: a SYSTEM lock with no `dir` records nothing, and refusing there turns every preview into exit 74.
    if policy.required() && policy.is_recording() {
        return Err(LaunchError::ExemptionRefused { reason });
    }
    Ok(())
}

/// Reject a recording frame that carries no `argv`, and return the child's arguments (its tail).
// An empty package set must stay valid: `ocx exec` in a project with no tools is a working command.
fn validate_record_inputs<'a>(record: &RecordInputs<'a>) -> Result<&'a [String], LaunchError> {
    let (_argv0, args) = record
        .argv
        .split_first()
        .ok_or_else(|| LaunchError::IncompleteRecordInputs {
            command: frame_name(&record.scope).to_string(),
        })?;
    Ok(args)
}

/// Name the launching command as a user typed it, for a diagnostic.
fn frame_name(scope: &Scope) -> &'static str {
    match scope {
        Scope::Project { .. } => "ocx exec",
        Scope::Package { .. } => "ocx package exec",
        Scope::Launcher => "ocx launcher exec",
        Scope::LauncherShim { .. } => "ocx launcher shim",
    }
}

/// Replace the current process with the launched tool; returns only on a start-up or required-record failure.
pub async fn exec(launch: Launch<'_>) -> LaunchError {
    let Launch {
        env,
        executable,
        args,
        mode,
    } = launch;
    match mode {
        Mode::Recording { record, policy } => {
            // Off Unix the record needs the child's pid, so only this probe gates the spawn under a fail-closed policy.
            #[cfg(not(unix))]
            if let Err(error) = probe_sink(policy).await {
                return error;
            }
            child_process::exec(executable, args, env, |pid| write_record(&record, policy, pid)).await
        }
        Mode::Exempt(reason) => {
            log::debug!("launch not recorded: {reason:?}");
            child_process::exec(executable, args, env, |_| std::future::ready(Ok(()))).await
        }
    }
}

/// Spawn the launched tool and wait, so the caller can clean up before propagating the exit status.
///
/// # Errors
///
/// [`LaunchError`] when the child cannot be started or a `required` record fails; a non-zero
/// child exit is the returned [`ExitStatus`].
pub async fn spawn_and_wait(launch: Launch<'_>) -> Result<ExitStatus, LaunchError> {
    let Launch {
        env,
        executable,
        args,
        mode,
    } = launch;
    match mode {
        Mode::Recording { record, policy } => {
            // The pid exists only post-spawn here, so the probe is the only pre-spawn gate.
            probe_sink(policy).await?;
            child_process::spawn_and_wait(executable, args, env, |pid| write_record(&record, policy, pid)).await
        }
        Mode::Exempt(reason) => {
            log::debug!("launch not recorded: {reason:?}");
            child_process::spawn_and_wait(executable, args, env, |_| std::future::ready(Ok(()))).await
        }
    }
}

/// Refuse an unwritable sink before the child starts.
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
    // `emit` refuses too, but only after building the record on every `ocx exec`.
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

/// Apply the policy's failure posture to an unwritable record: `required` aborts, otherwise warn and run.
fn apply_posture(error: RecordsError, policy: &RecordingPolicy) -> Result<(), LaunchError> {
    if policy.required() {
        return Err(LaunchError::Records(error));
    }
    // The whole chain: the head names only the sink, the cause says why.
    let chain = std::iter::successors(Some(&error as &dyn std::error::Error), |cause| cause.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");
    log::warn!("{chain}");
    Ok(())
}

/// A launch could not be constructed, recorded, or started.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum LaunchError {
    /// The tool could not be started.
    #[error("failed to run '{resolved}'", resolved = .resolved.display())]
    #[exit(
        chain,
        fallback(
            Failure,
            slug = "launch_spawn_failed",
            summary = "The resolved command could not be started"
        )
    )]
    Spawn {
        /// The resolved executable that could not be started.
        resolved: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// A recording frame supplied no `argv`, so it named nothing to run.
    #[error("recording frame '{command}' carries no argv")]
    #[exit(
        Failure,
        slug = "record_inputs_incomplete",
        summary = "An execution record frame carries no argv"
    )]
    IncompleteRecordInputs {
        /// The frame that failed validation.
        command: String,
    },

    /// A launch claimed a recording exemption under a fail-closed policy.
    #[error(
        "launch refused by [records] required = true in the resolved config chain: {command} does not record, and a fail-closed policy grants no exemption",
        command = .reason.command()
    )]
    #[exit(
        IoError,
        slug = "record_exemption_refused",
        summary = "The execution-record exemption was refused"
    )]
    ExemptionRefused {
        /// The preview command whose exemption was refused.
        reason: ExemptionReason,
    },

    /// A record could not be written and the policy is `required`.
    // The message names the policy: under the SYSTEM clamp `required` may appear in nobody's config.
    #[error("launch refused by [records] required = true in the resolved config chain")]
    #[exit(delegate = 0)]
    Records(#[from] crate::record::RecordsError),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::record::RecordsOptions;
    use crate::tasks::resolve::AdmittedClaims;
    use ocx_config::env::OcxConfigView;
    use ocx_oci::{Digest, PackageRef, PinnedPackageRef};
    use ocx_package::install_info::InstallInfo;
    use ocx_package::resolved_package::ResolvedPackage;
    use ocx_store::file_structure::PackageDir;

    const HEX: &str = "3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0";

    /// Owned fixture data, so a [`RecordInputs`] can borrow from one place.
    struct Frame {
        packages: Vec<Arc<InstallInfo>>,
        admitted: AdmittedClaims,
        executable: PathBuf,
        store_root: PathBuf,
        shim_root: PathBuf,
        argv: Vec<String>,
        config: OcxConfigView,
        auto_installed: Vec<PackageRef>,
    }

    impl Frame {
        fn new(packages: usize, argv: &[&str]) -> Self {
            let identifier = PinnedPackageRef::try_from(
                PackageRef::new_registry("ocx/cmake", "index.ocx.sh")
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
                admitted: AdmittedClaims::default(),
                executable: PathBuf::from("/store/cmake/content/bin/cmake"),
                store_root: PathBuf::from("/store"),
                shim_root: PathBuf::from("/shims"),
                argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
                config: OcxConfigView::new("/opt/ocx/bin/ocx"),
                auto_installed: Vec::new(),
            }
        }

        /// Point the frame at a real program, so a launch built from it runs
        /// something instead of failing to start.
        #[cfg(unix)]
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
                shim_root: &self.shim_root,
                argv: &self.argv,
                config: &self.config,
                insecure_registries: &[],
                managed_config_digest: None,
                patch_snapshot_digest: None,
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
    /// would break a working `ocx exec` for a policy nobody set.
    #[test]
    fn a_frame_that_composed_no_packages_still_launches() {
        let frame = Frame::new(0, &["env"]);
        let policy = policy(None, false);
        let launch = Launch::recording(Env::clean(), frame.inputs(), &policy)
            .expect("an empty toolchain is a legitimate launch, not a broken frame");

        assert!(launch.args.is_empty());
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
            #[expect(clippy::disallowed_methods, reason = "exercises the exemption bound")]
            let error = Launch::exempt(Env::clean(), &frame.executable, &[], reason, &policy)
                .err()
                .expect("a required policy grants no exemption");

            assert!(matches!(error, LaunchError::ExemptionRefused { reason: got } if got == reason));
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
            #[expect(clippy::disallowed_methods, reason = "exercises the exemption bound")]
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

        let _error = probe_sink(&policy)
            .await
            .expect_err("a sink that cannot be written refuses the launch before anything starts");
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

    // ── the recording wait path ──────────────────────────────────────────────
    //
    // `ocx package exec --rm` is the production caller of `spawn_and_wait`'s
    // recording arm: it must outlive the child to collect the package, and a
    // replaced process image cannot. The other spawn-and-wait caller
    // (`package test`) launches exempt.
    //
    // These two tests stay because that caller does not exercise the arm's
    // fail-closed half: the pre-spawn probe is the only gate on a non-Unix
    // recording launch, and no acceptance fixture drives an unwritable sink to
    // a refusal. Without them the probe would rot silently.

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

        let _error = spawn_and_wait(launch)
            .await
            .expect_err("an unwritable sink under a required policy refuses the launch");

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
}

#[cfg(test)]
mod firewall_tests {
    use std::path::{Path, PathBuf};

    // Clippy (`disallowed_types` / `disallowed_methods` in the root `clippy.toml`) holds the launch seam
    // for every file compiled for the host. This scan covers the rest: source clippy never compiles, and
    // spawns inside `#[cfg(windows)]` / `#[cfg(target_os = ...)]` code a Linux clippy pass cannot see.

    /// Tokens that mean "this file can start a process": every spelling that names a `Command`, including
    /// a braced or renamed import, and the `execvp` trait. Over-broad on purpose.
    const SPAWN_TOKENS: &[&str] = &[
        "process::Command",
        "process::{",
        "process::*",
        "process as ",
        "CommandExt",
    ];

    /// Source trees clippy does not lint as part of any crate: the SDK templates, and the golden copies of what
    /// they generate.
    const UNLINTED_ROOTS: &[&str] = &["ocx_sdkgen/templates/", "ocx_sdkgen/tests/golden/"];

    /// The files under [`UNLINTED_ROOTS`] allowed to spawn: the generated Rust SDK's runtime spawns the caller's
    /// `ocx`, never runs inside ocx, and resolves no package.
    const GENERATED_SDK_SPAWNERS: &[&str] = &[
        "ocx_sdkgen/templates/rust/spawn.rs",
        "ocx_sdkgen/tests/golden/rust/spawn.rs",
    ];

    /// The launch seam: the one module that owns spawning, and so needs no expectation.
    const LAUNCH_SEAM: &[&str] = &["ocx_package_manager/src/launch.rs", "ocx_package_manager/src/launch/"];

    /// What a sanctioned spawn site carries: an item-level `#[expect(clippy::disallowed_types, ...)]`, matched
    /// with whitespace removed so a rustfmt-wrapped attribute still counts.
    const EXPECTATION: &str = "#[expect(clippy::disallowed_types";

    /// Files the unlinted roots must yield: the three templates and the nine golden files today. A reader
    /// that stopped early would otherwise pass an empty tree.
    const SCAN_FLOOR: usize = 12;

    /// Files the workspace walk must yield (758 today). A walk that lost its root would otherwise pass.
    const WORKSPACE_FLOOR: usize = 400;

    /// Directory names holding build output or vendored code, never workspace source.
    fn is_skipped_dir(name: &str) -> bool {
        name == "target"
            || name == "external"
            || name == "node_modules"
            || name.starts_with("bazel-")
            || name.starts_with('.')
    }

    fn crates_root() -> PathBuf {
        // CARGO_MANIFEST_DIR = crates/ocx_package_manager → parent = crates/.
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
                if !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(is_skipped_dir)
                {
                    collect_rs(&path, out);
                }
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// Every `.rs` file under `crates/`, as `crates/`-relative slash-separated paths paired with their contents.
    fn workspace_sources() -> Vec<(String, String)> {
        let root = crates_root();
        let mut files = Vec::new();
        collect_rs(&root, &mut files);
        files
            .iter()
            .filter_map(|path| {
                let relative = path.strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
                Some((relative, std::fs::read_to_string(path).ok()?))
            })
            .collect()
    }

    fn is_unlinted(path: &str) -> bool {
        UNLINTED_ROOTS.iter().any(|root| path.starts_with(root))
    }

    /// The compiled files that spawn without the seam's blessing: outside the launch seam, with no
    /// [`EXPECTATION`] anywhere in the file to say why.
    fn unexpected_spawners(sources: &[(String, String)]) -> Vec<String> {
        let mut found: Vec<String> = sources
            .iter()
            .filter(|(path, _)| !is_unlinted(path) && !LAUNCH_SEAM.iter().any(|seam| path.starts_with(seam)))
            .filter(|(_, content)| spawns(content) && !claims(content))
            .map(|(path, _)| path.clone())
            .collect();
        found.sort();
        found
    }

    /// Whether `content` carries [`EXPECTATION`] outside a comment. A mention in prose or an `allow` does not
    /// count. Per file, not per item: a second spawn in a claimed file passes.
    fn claims(content: &str) -> bool {
        let code: String = content
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .flat_map(str::chars)
            .filter(|character| !character.is_whitespace())
            .collect();
        code.contains(EXPECTATION)
    }

    /// Whether `content` uses any of [`SPAWN_TOKENS`] outside a comment.
    fn spawns(content: &str) -> bool {
        content
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .any(|line| SPAWN_TOKENS.iter().any(|token| line.contains(token)))
    }

    /// The files that disagree with `sanctioned`: spawning without being listed, or listed without spawning.
    /// The second half keeps an entry from promising a hole that no longer exists.
    fn disagreements(sources: &[(String, String)], sanctioned: &[&str]) -> Vec<String> {
        let mut found: Vec<String> = sources
            .iter()
            .filter(|(path, content)| spawns(content) != sanctioned.contains(&path.as_str()))
            .map(|(path, _)| path.clone())
            .collect();
        found.extend(
            sanctioned
                .iter()
                .filter(|path| !sources.iter().any(|(source, _)| source == *path))
                .map(|path| (*path).to_owned()),
        );
        found.sort();
        found
    }

    #[test]
    fn no_process_spawn_outside_launch() {
        let workspace = workspace_sources();
        assert!(
            workspace.len() >= WORKSPACE_FLOOR,
            "the scan read {} files under crates/, below the floor of {WORKSPACE_FLOOR}",
            workspace.len()
        );
        assert!(
            workspace
                .iter()
                .any(|(path, content)| !is_unlinted(path) && spawns(content)),
            "no compiled file spawns a process: the scan is reading nothing it can judge"
        );
        let unexpected = unexpected_spawners(&workspace);
        assert!(
            unexpected.is_empty(),
            "a spawn primitive appears outside the launch seam with no `#[expect(clippy::disallowed_types, \
             reason = ...)]` in the file (clippy cannot see code behind another platform's `cfg`):\n{}\n\n\
             Route the launch through `ocx_package_manager::launch`, or claim it with the expectation.",
            unexpected.join("\n")
        );

        let sources: Vec<(String, String)> = workspace.into_iter().filter(|(path, _)| is_unlinted(path)).collect();
        assert!(
            sources.len() >= SCAN_FLOOR,
            "the scan read {} files under {UNLINTED_ROOTS:?}, below the floor of {SCAN_FLOOR}",
            sources.len()
        );

        let found = disagreements(&sources, GENERATED_SDK_SPAWNERS);
        assert!(
            found.is_empty(),
            "a spawn primitive appears outside the launch seam in a tree clippy does not lint, or a \
             sanctioned spawner no longer spawns:\n{}\n\nRoute the launch through \
             `ocx_package_manager::launch`, or amend `GENERATED_SDK_SPAWNERS`.",
            found.join("\n")
        );
    }

    /// The detector goes red on a stray spawn in every spelling, on a sanctioned file that stopped spawning,
    /// and stays quiet on prose and on an unrelated `process` import.
    #[test]
    fn the_unlinted_scan_goes_red_on_a_stray_spawn() {
        let sanctioned = ["sdk/spawn.rs"];
        let spelled = [
            "use tokio::process::{Command as RawCommand};\nRawCommand::new(\"sh\").spawn()",
            "use std::process::{Command, Stdio};\nCommand::new(\"sh\").status()",
            "use std::process as proc;\nproc::Command::new(\"sh\").output()",
            "std::process::Command::new(\"sh\").spawn()",
            "use std::os::unix::process::CommandExt as _;\ncmd.exec()",
            "use std::process::*;\nCommand::new(\"sh\").spawn()",
        ];
        for source in spelled {
            let sources = [
                ("sdk/spawn.rs".to_owned(), source.to_owned()),
                ("sdk/stray.rs".to_owned(), source.to_owned()),
            ];
            assert_eq!(
                disagreements(&sources, &sanctioned),
                ["sdk/stray.rs"],
                "a spawn spelled this way slips past the scan:\n{source}"
            );
        }

        let quiet = [
            ("sdk/spawn.rs".to_owned(), "// no spawn left here".to_owned()),
            (
                "sdk/prose.rs".to_owned(),
                "// builds a process::Command\nuse std::process::ExitCode;".to_owned(),
            ),
        ];
        assert_eq!(
            disagreements(&quiet, &sanctioned),
            ["sdk/spawn.rs"],
            "a stale sanctioned entry must be reported, prose and an unrelated import must not"
        );
    }

    /// A spawn behind another platform's `cfg` is reported unless the file carries the expectation or is the
    /// launch seam.
    #[test]
    fn the_compiled_scan_goes_red_on_an_unclaimed_spawn() {
        let spawn = "#[cfg(windows)]\nfn f() { std::process::Command::new(\"x\"); }";
        let claimed = format!("#[expect(clippy::disallowed_types, reason = \"why\")]\n{spawn}");
        let wrapped = format!("#[expect(\n    clippy::disallowed_types,\n    reason = \"why\"\n)]\n{spawn}");
        let commented = format!("// #[expect(clippy::disallowed_types, reason = \"why\")]\n{spawn}");
        let allowed = format!("#[allow(clippy::disallowed_types)]\n{spawn}");
        let sources = [
            ("ocx_a/src/stray.rs".to_owned(), spawn.to_owned()),
            ("ocx_a/src/claimed.rs".to_owned(), claimed),
            ("ocx_a/src/wrapped.rs".to_owned(), wrapped),
            ("ocx_a/src/commented.rs".to_owned(), commented),
            ("ocx_a/src/allowed.rs".to_owned(), allowed),
            (
                "ocx_package_manager/src/launch/child_process.rs".to_owned(),
                spawn.to_owned(),
            ),
            (
                "ocx_a/src/prose.rs".to_owned(),
                "/// builds a `std::process::Command`".to_owned(),
            ),
        ];
        assert_eq!(
            unexpected_spawners(&sources),
            ["ocx_a/src/allowed.rs", "ocx_a/src/commented.rs", "ocx_a/src/stray.rs"]
        );
    }
}
