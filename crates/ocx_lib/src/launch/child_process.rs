// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The spawn primitives, reachable only from [`crate::launch`].
//!
//! This module is declared `mod child_process;` — private — inside `launch.rs`.
//! That nesting is what makes the seam's compile-time claim true at all: while
//! a raw spawn primitive is publicly callable, a new command can skip
//! [`Launch`](super::Launch) entirely and still compile. Visibility comes from
//! module nesting rather than `pub(crate)`, per `arch-principles.md`.
//!
//! Both primitives take an `on_started` callback invoked with **the pid of the
//! process that will run the tool**, at the one moment each platform can name
//! it:
//!
//! - [`exec`] on Unix — before `execvp(2)`, with ocx's own pid, because the
//!   syscall replaces this image in place and that pid *becomes* the tool.
//! - [`exec`] elsewhere and [`spawn_and_wait`] everywhere — after the spawn,
//!   with the child's pid, which does not exist until then.
//!
//! The callback is where the launch seam writes its record, so the ordering
//! above is the record's pre-exec guarantee on Unix and the reason the seam
//! probes the sink before spawning everywhere else.
//!
//! **That guarantee is Unix-only, and the difference is not cosmetic.** Where
//! the pid only exists after the spawn, the pre-spawn probe writes zero bytes
//! and the real record is written with the child already running: a mount that
//! disappears, a full disk or a permission change between the two leaves a
//! `required` launch returning exit 74 *after* the tool has begun work. The
//! child is then stopped and reaped ([`stop_refused_child`]), but it ran. So
//! fail-closed off Unix means "refused before the spawn if the sink was
//! unwritable at probe time", not "no unrecorded child can ever have run".
//! Suspending the child across the write would close it, and is rejected in the
//! design record: it needs the child's *thread* handle to resume, which
//! `std::process::Child` does not expose.
//!
//! Neither primitive knows what the callback does. `env` is applied after
//! `env_clear()` on both paths, so no parent-shell variable can leak past the
//! authoritative env the caller built (this is what makes `--clean` and
//! [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config)'s remove
//! branches load-bearing).

use std::future::Future;
use std::path::Path;
use std::process::ExitStatus;

use super::LaunchError;
use crate::env::Env;

/// Stop a child whose launch was refused after it had already started, and wait
/// for it to be gone.
///
/// Both halves matter and neither is automatic: `start_kill` can fail (the
/// process may have exited already, or the signal may be refused), and without
/// the `wait` the caller returns while the child is still running — so
/// "the refused launch left nothing behind" would be an assumption rather than a
/// fact. Neither failure is propagated: the caller is already returning the
/// refusal, which is the more useful error.
///
/// The kill is attempted first but judged last, because its failure alone says
/// nothing. A child that exited between the refusal and the signal refuses the
/// kill and is still perfectly gone; what decides is whether the `wait` could
/// account for it.
async fn stop_refused_child(child: &mut tokio::process::Child) {
    let kill = child.start_kill();
    match (kill, child.wait().await) {
        (Ok(()), Ok(_)) => {}
        (Err(kill_error), Ok(_)) => {
            // Almost always "no such process". The wait reaped it regardless,
            // which is the property the caller needs.
            crate::log::debug!("the refused launch had already exited: {kill_error}");
        }
        (Ok(()), Err(wait_error)) => {
            crate::log::warn!("failed to reap the refused launch: {wait_error}");
        }
        // The signal was refused *and* the child could not be accounted for, so
        // nothing establishes that it stopped — the one case where a tool the
        // policy rejected may still be running.
        (Err(kill_error), Err(wait_error)) => {
            crate::log::warn!("failed to stop the refused launch: {kill_error}; and to reap it: {wait_error}");
        }
    }
}

/// Platform-aware exit-status propagation that diverges via `process::exit`.
///
/// Delegates to [`crate::utility::child_process::exit_code_from_status`] for
/// the exit-code computation, which `propagate_exit_code` shares, so the
/// `128 + signum` convention and the Windows passthrough stay a single source
/// of truth across the diverging and non-diverging paths.
#[cfg(not(unix))]
#[inline(never)] // ensure the diverging path is not inlined away in tests
fn propagate_exit_status(status: ExitStatus) -> ! {
    std::process::exit(crate::utility::child_process::exit_code_from_status(status));
}

/// Run `program` with `args` and the exact `env` provided, replacing the
/// running process when the platform supports it and otherwise faking
/// replacement by spawning + waiting + exiting.
///
/// On Unix this calls `execvp(2)`: the child inherits the current PID, no fork
/// happens, and the function only returns when exec itself fails. On Windows
/// there is no exec syscall; the child is spawned, waited on, and the process
/// exits with its status, so no Drop chain runs after the child finishes —
/// keeping behaviour symmetrical with the Unix branch from the caller's point
/// of view.
///
/// Returns only when the launch fails to reach the child: a start-up error, or
/// an `on_started` that refused the launch. Success diverges, which is why the
/// return type is a bare [`LaunchError`] and not a `Result`.
pub async fn exec<F, Fut>(program: &Path, args: &[String], env: Env, on_started: F) -> LaunchError
where
    F: FnOnce(u32) -> Fut,
    Fut: Future<Output = Result<(), LaunchError>>,
{
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;

        // `execvp` replaces this image in place, so ocx's own pid is the pid
        // the tool will run under — known before anything starts, which is what
        // lets the callback run strictly pre-exec.
        if let Err(error) = on_started(std::process::id()).await {
            return error;
        }

        let mut cmd = std::process::Command::new(program);
        cmd.args(args).env_clear().envs(env);
        // `exec` only returns when the syscall fails — on success the running
        // image is already gone.
        LaunchError::Spawn {
            resolved: program.to_path_buf(),
            source: cmd.exec(),
        }
    }
    #[cfg(not(unix))]
    {
        // Spawn and wait are split rather than fused into `status()`: the fused
        // form yields no `Child`, so the pid of the process that runs the tool
        // never exists while that process is alive and the record could not
        // name it.
        let mut child = match tokio::process::Command::new(program)
            .args(args)
            .env_clear()
            .envs(env)
            .spawn()
        {
            Ok(child) => child,
            Err(source) => {
                return LaunchError::Spawn {
                    resolved: program.to_path_buf(),
                    source,
                };
            }
        };

        // `id()` is `None` only once the child has been reaped, and nothing has
        // awaited it between the spawn above and this line.
        let pid = child
            .id()
            .expect("a just-spawned child that has not been awaited still holds its pid");

        if let Err(error) = on_started(pid).await {
            // The launch was refused after the child existed — the narrow
            // window the seam's pre-spawn sink probe cannot close. Stop the
            // child rather than let a refused launch keep running.
            stop_refused_child(&mut child).await;
            return error;
        }

        match child.wait().await {
            // Emulate the Unix "no cleanup after the child" property by
            // skipping the Drop chain via `process::exit`.
            Ok(status) => propagate_exit_status(status),
            Err(source) => LaunchError::Spawn {
                resolved: program.to_path_buf(),
                source,
            },
        }
    }
}

/// Spawn `program` with `args` in the exact `env` provided, wait for it to
/// finish, and return its [`ExitStatus`].
///
/// Unlike [`exec`] this function always returns — it does not replace the
/// running process image. The caller is responsible for propagating the exit
/// status (via [`crate::utility::child_process::propagate_exit_code`]) and for
/// any cleanup that must happen after the child finishes (e.g. dropping a
/// tempdir guard).
///
/// Stdin, stdout and stderr all inherit from the current process.
///
/// # Errors
///
/// Returns [`LaunchError::Spawn`] only when the child cannot be started
/// (program not found, permission denied) or `.wait()` returns an OS error, and
/// whatever `on_started` returned when it refused the launch. Child failures
/// (non-zero exit code) are reported via the returned [`ExitStatus`], not as an
/// error.
pub async fn spawn_and_wait<F, Fut>(
    program: &Path,
    args: &[String],
    env: Env,
    on_started: F,
) -> Result<ExitStatus, LaunchError>
where
    F: FnOnce(u32) -> Fut,
    Fut: Future<Output = Result<(), LaunchError>>,
{
    let spawn_error = |source: std::io::Error| LaunchError::Spawn {
        resolved: program.to_path_buf(),
        source,
    };

    let mut child = tokio::process::Command::new(program)
        .args(args)
        .env_clear()
        .envs(env)
        // Inherit stdio so the child's output reaches the terminal directly.
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        // kill_on_drop: if the async task holding this child is cancelled (e.g.
        // via tokio::time::timeout or explicit abort), the OS process is killed
        // automatically rather than becoming an orphan.
        .kill_on_drop(true)
        .spawn()
        .map_err(spawn_error)?;

    // This path never replaces the ocx image, so the tool runs in the spawned
    // child on every platform and its pid is only knowable here.
    let pid = child
        .id()
        .expect("a just-spawned child that has not been awaited still holds its pid");
    if let Err(error) = on_started(pid).await {
        // `kill_on_drop` would fire on the way out, but it neither reports a
        // failed kill nor waits for the process to actually go — so a refused
        // launch would only *probably* leave no running tool behind. Stop it
        // explicitly instead, for the same reason the `exec` path does.
        stop_refused_child(&mut child).await;
        return Err(error);
    }

    // On Unix, forward SIGINT and SIGTERM to the child so that Ctrl-C from the
    // terminal and shell job-control signals reach the child correctly.
    // On Windows, kill_on_drop + the default Ctrl-C handler suffices for the
    // interactive case; the `select!` below compiles to `child.wait().await`
    // only (the cfg blocks out the signal branches).
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigint = signal(SignalKind::interrupt()).map_err(spawn_error)?;
        let mut sigterm = signal(SignalKind::terminate()).map_err(spawn_error)?;

        let status = loop {
            tokio::select! {
                status = child.wait() => break status.map_err(spawn_error)?,
                _ = sigint.recv() => {
                    // Forward SIGINT to child; re-raise for the process group.
                    let _ = child.start_kill();
                }
                _ = sigterm.recv() => {
                    let _ = child.start_kill();
                }
            }
        };
        Ok(status)
    }

    #[cfg(not(unix))]
    {
        child.wait().await.map_err(spawn_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal a launch hands back; its identity is all these tests read.
    fn refusal() -> LaunchError {
        LaunchError::IncompleteRecordInputs {
            command: "ocx exec".to_string(),
        }
    }

    /// A live child is reaped, not merely signalled — `Child::id` clears only
    /// after a successful wait, so it is the one observable that separates the
    /// two.
    #[cfg(unix)]
    #[tokio::test]
    async fn stopping_a_refused_child_waits_for_it_to_be_gone() {
        let mut child = tokio::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("/bin/sleep is present on every unix");

        stop_refused_child(&mut child).await;

        assert!(
            child.id().is_none(),
            "a refused launch must leave nothing running, and only the wait establishes that"
        );
    }

    /// The branch that used to return early: a child already reaped refuses the
    /// kill, and the wait must still run rather than be skipped on the strength
    /// of that failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_kill_that_fails_because_the_child_is_gone_is_not_an_escalation() {
        let mut child = tokio::process::Command::new("/bin/true")
            .spawn()
            .expect("/bin/true is present on every unix");
        child.wait().await.expect("the child exits immediately");

        // `start_kill` now fails; this must still return rather than hang on a
        // second wait for a process that no longer exists.
        stop_refused_child(&mut child).await;

        assert!(child.id().is_none());
    }

    /// On Unix the refusal lands strictly before `execvp`, and reaching the
    /// assertions at all is half the proof: had the callback moved after the
    /// syscall, this test process would have been *replaced* by the child and
    /// the run would end here.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_launch_never_reaches_the_tool() {
        let root = tempfile::tempdir().expect("tempdir");
        let marker = root.path().join("ran");
        let args = vec![marker.to_string_lossy().into_owned()];

        let error = exec(Path::new("/bin/mkdir"), &args, Env::clean(), |_| async {
            Err(refusal())
        })
        .await;

        assert!(
            matches!(error, LaunchError::IncompleteRecordInputs { .. }),
            "got: {error:?}"
        );
        assert!(!marker.exists(), "a refused launch must not have run the tool");
    }

    /// The same refusal where the child exists before the callback can run:
    /// `exec` must stop it and hand the refusal back rather than wait out a tool
    /// the policy already rejected.
    ///
    /// **Unexercised on this repo's Linux CI** — it is compiled only under
    /// `cfg(not(unix))`, so a Windows runner is the first thing that will ever
    /// build or run it.
    #[cfg(not(unix))]
    #[tokio::test]
    async fn a_refused_launch_stops_the_child_it_already_spawned() {
        let program = std::env::var_os("COMSPEC").map_or_else(
            || std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            std::path::PathBuf::from,
        );
        // The conventional Windows sleep: long enough that the refusal, not the
        // child finishing on its own, is what ends the launch.
        let args = vec!["/C".to_string(), "ping -n 30 127.0.0.1 >NUL".to_string()];

        let error = exec(&program, &args, Env::clean(), |_| async { Err(refusal()) }).await;

        assert!(
            matches!(error, LaunchError::IncompleteRecordInputs { .. }),
            "got: {error:?}"
        );
    }
}
