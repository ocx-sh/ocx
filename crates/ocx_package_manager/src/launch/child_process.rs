// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The spawn primitives, private to [`crate::launch`] so no command can skip [`Launch`](super::Launch).
//!
//! `on_started` receives the pid that will run the tool: ocx's own before `execvp(2)` on Unix,
//! the child's after the spawn elsewhere (`adr_exec_resolution_record.md` § "Rationale from code: launch").

use std::future::Future;
use std::path::Path;
use std::process::ExitStatus;

use super::LaunchError;
use ocx_config::env::Env;

/// Stop a child whose launch was refused after it started, and wait for it to be gone.
///
/// Neither failure propagates: the caller is already returning the refusal.
async fn stop_refused_child(child: &mut tokio::process::Child) {
    let kill = child.start_kill();
    match (kill, child.wait().await) {
        (Ok(()), Ok(_)) => {}
        (Err(kill_error), Ok(_)) => {
            log::debug!("the refused launch had already exited: {kill_error}");
        }
        (Ok(()), Err(wait_error)) => {
            log::warn!("failed to reap the refused launch: {wait_error}");
        }
        (Err(kill_error), Err(wait_error)) => {
            log::warn!("failed to stop the refused launch: {kill_error}; and to reap it: {wait_error}");
        }
    }
}

/// Exit with the child's status, skipping the drop chain.
#[cfg(not(unix))]
#[inline(never)] // ensure the diverging path is not inlined away in tests
fn propagate_exit_status(status: ExitStatus) -> ! {
    std::process::exit(ocx_util::child_process::exit_code_from_status(status));
}

/// Run `program` with `args` and exactly `env`, replacing the running process (`execvp(2)` on
/// Unix; spawn, wait and exit elsewhere). Returns only on failure.
pub async fn exec<F, Fut>(program: &Path, args: &[String], env: Env, on_started: F) -> LaunchError
where
    F: FnOnce(u32) -> Fut,
    Fut: Future<Output = Result<(), LaunchError>>,
{
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;

        if let Err(error) = on_started(std::process::id()).await {
            return error;
        }

        let mut cmd = std::process::Command::new(program);
        cmd.args(args).env_clear().envs(env);
        LaunchError::Spawn {
            resolved: program.to_path_buf(),
            source: cmd.exec(),
        }
    }
    #[cfg(not(unix))]
    {
        // Not `status()`: it yields no `Child`, so the record could never name the tool's pid.
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

        let pid = child
            .id()
            .expect("a just-spawned child that has not been awaited still holds its pid");

        if let Err(error) = on_started(pid).await {
            stop_refused_child(&mut child).await;
            return error;
        }

        match child.wait().await {
            Ok(status) => propagate_exit_status(status),
            Err(source) => LaunchError::Spawn {
                resolved: program.to_path_buf(),
                source,
            },
        }
    }
}

/// Spawn `program` with `args` and exactly `env`, wait, and return its [`ExitStatus`].
///
/// # Errors
///
/// [`LaunchError::Spawn`] when the child cannot be started or waited on, or `on_started`'s refusal.
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
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        // Or a cancelled task (timeout, abort) orphans the child process.
        .kill_on_drop(true)
        .spawn()
        .map_err(spawn_error)?;

    let pid = child
        .id()
        .expect("a just-spawned child that has not been awaited still holds its pid");
    if let Err(error) = on_started(pid).await {
        // Not `kill_on_drop`: it neither reports a failed kill nor waits for the process to go.
        stop_refused_child(&mut child).await;
        return Err(error);
    }

    // On Unix, SIGINT/SIGTERM kill the child; elsewhere `kill_on_drop` plus the default Ctrl-C handler cover it.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigint = signal(SignalKind::interrupt()).map_err(spawn_error)?;
        let mut sigterm = signal(SignalKind::terminate()).map_err(spawn_error)?;

        let status = loop {
            tokio::select! {
                status = child.wait() => break status.map_err(spawn_error)?,
                _ = sigint.recv() => {
                    // Best effort: the `wait` arm reports how the child ended.
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
        // `/usr/bin/true`, not `/bin/true`: macOS ships no `/bin/true`, while
        // both platforms carry the `/usr/bin` spelling. The sibling tests reach
        // for `/bin/sleep` and `/bin/mkdir`, which macOS does have.
        let mut child = tokio::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("/usr/bin/true is present on linux and macOS");
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
