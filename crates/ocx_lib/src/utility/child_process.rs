// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Generic boundary between OCX and the child processes it spawns. Platform
//! conditionals live inside each helper so callers stay platform-blind.
//!
//! [`exec`] runs a command with a fully-controlled environment and diverges
//! on success on every platform. On Unix it `execvp(2)`s, so the running ocx
//! is replaced by the child (same PID, no extra process-tree entry); on
//! Windows it spawns the child, waits for it, and calls
//! [`std::process::exit`] via [`propagate_exit_status`] with the child's exit
//! status. Either way no Drop / cleanup runs after the child finishes — the
//! function's return type only describes the start-up failure path.
//!
//! [`spawn_and_wait`] is the non-diverging variant: it spawns the child,
//! awaits its exit, and returns an [`ExitStatus`] so the caller can perform
//! cleanup (e.g. drop a tempdir) before propagating the exit code. Use this
//! instead of [`exec`] when RAII guards must run after the child exits.
//!
//! [`propagate_exit_code`] converts a child [`ExitStatus`] into a
//! [`std::process::ExitCode`] using platform-appropriate semantics.
//! Both [`propagate_exit_status`] and [`propagate_exit_code`] delegate to the
//! private `exit_code_from_status` helper, which is the single source of truth
//! for the `128 + signum` (Unix) and full-i32 passthrough (Windows) mapping.
//!
//! Launcher-specific spawn helpers (e.g. `PATHEXT` injection) live in
//! [`crate::package_manager::launcher::pathext`] — they belong with the
//! launcher concept, not with this generic helper.

use std::path::Path;
use std::process::ExitStatus;

use crate::env::Env;

/// Single source of truth for the "child [`ExitStatus`] → `i32` exit code"
/// mapping shared by [`propagate_exit_status`] and [`propagate_exit_code`].
///
/// * **Unix** — signal-aware: when `code()` is `None` the child was killed by
///   a signal; returns `128 + signum` (POSIX convention) so `$?` and scripts
///   see a meaningful code. Falls back to `1` when no signal number is
///   available.
/// * **non-Unix** — the full `i32` exit code is forwarded. Windows preserves
///   32-bit `STATUS_*` codes in single-process pipelines; truncating to 8 bits
///   would silently lose information (e.g. `STATUS_ACCESS_VIOLATION =
///   0xC0000005`). Callers that must map to `u8` (e.g. `propagate_exit_code`)
///   saturate after calling this function.
#[cfg(unix)]
fn exit_code_from_status(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        code
    } else {
        // Child killed by signal — follow 128 + signum convention.
        status.signal().map(|s| 128 + s).unwrap_or(1)
    }
}

#[cfg(not(unix))]
fn exit_code_from_status(status: ExitStatus) -> i32 {
    // Windows preserves the full 32-bit exit code; pass it through so
    // PowerShell's `$LastExitCode` and cmd's `%ERRORLEVEL%` see what
    // the child actually returned.
    status.code().unwrap_or(1)
}

/// Platform-aware exit-status propagation that diverges via `process::exit`.
///
/// Delegates to [`exit_code_from_status`] for the exit-code computation and
/// then calls [`std::process::exit`]. Both [`exec`] (Windows branch) and
/// [`propagate_exit_code`] share that helper so the `128 + signum` convention
/// and Windows passthrough semantics are a single source of truth.
// On Unix this function is only referenced from the cfg(not(unix)) branch; allow
// the dead_code lint so Unix builds stay clean.
#[cfg_attr(unix, allow(dead_code))]
#[inline(never)] // ensure the diverging path is not inlined away in tests
fn propagate_exit_status(status: ExitStatus) -> ! {
    std::process::exit(exit_code_from_status(status));
}

/// Run `program` with `args` and the exact `env` provided, replacing the
/// running process when the platform supports it and otherwise faking
/// replacement by spawning + waiting + exiting.
///
/// On Unix this calls `execvp(2)`: the child inherits the current PID,
/// no fork happens, and the function only returns when exec itself
/// fails. On Windows there is no exec syscall; we spawn the child
/// synchronously, wait for it, then call [`propagate_exit_status`] so no
/// Drop chain runs after the child finishes — keeping behaviour symmetrical
/// with the Unix branch from the caller's point of view.
///
/// The child's environment is fully controlled — `env_clear` is called
/// before applying `env`, so no parent-shell variable can leak past the
/// authoritative env the caller built up (this is what makes
/// [`Env::apply_ocx_config`]'s "remove" branches load-bearing and what
/// gives `--clean` real effect).
///
/// Returns only on start-up failure (exec syscall error, or spawn /
/// wait failure on Windows). The return type encodes that:
/// [`std::io::Error`] is the start-up error; success diverges.
pub fn exec(program: &Path, args: &[String], env: Env) -> std::io::Error {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).env_clear().envs(env);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // `exec` only returns when the syscall fails — on success the
        // running image is already gone.
        cmd.exec()
    }
    #[cfg(not(unix))]
    {
        match cmd.status() {
            // Windows has no exec syscall; emulate the "no cleanup after child"
            // property by skipping the Drop chain via `process::exit`.
            // `propagate_exit_status` is the shared helper so exit-code semantics
            // are identical to the `spawn_and_wait` path.
            Ok(status) => propagate_exit_status(status),
            Err(err) => err,
        }
    }
}

/// Spawn `program` with `args` in the exact `env` provided, wait for it to
/// finish, and return its [`ExitStatus`].
///
/// Unlike [`exec`] this function always returns — it does not replace the
/// running process image. The caller is responsible for propagating the exit
/// status (via [`propagate_exit_code`]) and for any cleanup that must happen
/// after the child finishes (e.g., dropping a tempdir guard).
///
/// The child's environment is fully controlled — `env_clear()` is called
/// before applying `env`, so no parent-shell variable can leak (same
/// semantics as [`exec`]).
///
/// Stdin, stdout and stderr all inherit from the current process.
///
/// Returns `Err` only when spawning fails (program not found, permission
/// denied, etc.) or when `.wait()` returns an OS error. Child failures
/// (non-zero exit code) are reported via the returned `ExitStatus`, not as
/// an `Err`.
pub async fn spawn_and_wait(program: &Path, args: &[String], env: Env) -> std::io::Result<ExitStatus> {
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
        .spawn()?;

    // On Unix, forward SIGINT and SIGTERM to the child so that Ctrl-C from the
    // terminal and shell job-control signals reach the child correctly.
    // On Windows, kill_on_drop + the default Ctrl-C handler suffices for the
    // interactive case; the `select!` below compiles to `child.wait().await`
    // only (the cfg blocks out the signal branches).
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigint = signal(SignalKind::interrupt())?;
        let mut sigterm = signal(SignalKind::terminate())?;

        let status = loop {
            tokio::select! {
                status = child.wait() => break status?,
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
        child.wait().await
    }
}

/// Convert a child [`ExitStatus`] into a [`std::process::ExitCode`].
///
/// On Unix: signal-aware — `128 + signum` when the child was killed by a
/// signal (POSIX convention). When `code()` is `Some(n)` the raw value is
/// passed through, saturated to `u8` (0–255 range) for compatibility with the
/// [`std::process::ExitCode`] type.
///
/// On Windows: the raw numeric exit code is forwarded via `u8` saturation
/// (Windows `STATUS_*` values are 32-bit; values > 255 saturate to 255).
/// The diverging [`propagate_exit_status`] (used by the [`exec`] Windows
/// branch) passes the full `i32` directly; this non-diverging variant must
/// truncate to `u8` because [`std::process::ExitCode::from`] only accepts
/// `u8`. The contract for callers is documented: codes ≤ 255 are preserved
/// cross-platform; Windows-only high codes use the [`exec`] path instead.
pub fn propagate_exit_code(status: ExitStatus) -> std::process::ExitCode {
    // Delegate to exit_code_from_status for the platform-aware i32 computation
    // (128 + signum on Unix, full i32 passthrough on Windows), then saturate to
    // u8 because std::process::ExitCode::from only accepts u8 (0–255 range).
    // Codes above 255 saturate to 255. Callers that need the full i32 (e.g.,
    // Windows STATUS_* codes) should use the exec path instead.
    let code = exit_code_from_status(status);
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(255))
}
