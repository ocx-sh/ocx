// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Generic boundary between OCX and the child processes it spawns. Platform
//! conditionals live inside each helper so callers stay platform-blind.
//!
//! [`exec`] runs a command with a fully-controlled environment and diverges
//! on success on every platform. On Unix it `execvp(2)`s, so the running ocx
//! is replaced by the child (same PID, no extra process-tree entry); on
//! Windows it spawns the child, waits for it, and calls
//! [`std::process::exit`] with the child's raw exit code. Either way no Drop
//! / cleanup runs after the child finishes — the function's return type only
//! describes the start-up failure path.
//!
//! Launcher-specific spawn helpers (e.g. `PATHEXT` injection) live in
//! [`crate::package_manager::launcher::pathext`] — they belong with the
//! launcher concept, not with this generic helper.

use std::path::Path;

use crate::env::Env;

/// Run `program` with `args` and the exact `env` provided, replacing the
/// running process when the platform supports it and otherwise faking
/// replacement by spawning + waiting + exiting.
///
/// On Unix this calls `execvp(2)`: the child inherits the current PID,
/// no fork happens, and the function only returns when exec itself
/// fails. On Windows there is no exec syscall; we spawn the child
/// synchronously, wait for it, then [`std::process::exit`] with the
/// child's raw exit code so no Drop chain runs after the child
/// finishes — keeping behaviour symmetrical with the Unix branch from
/// the caller's point of view. Signal-killed children (`code()` =
/// `None`) surface as `1`.
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
            // Windows has no exec syscall; emulate the "no cleanup
            // after child" property by skipping the Drop chain via
            // `process::exit`. `code()` is the raw `ExitCode` /
            // `STATUS_*` value the child reported — pass it through
            // unmodified so PowerShell's `$LastExitCode` and cmd's
            // `%ERRORLEVEL%` see what the child actually returned.
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(err) => err,
        }
    }
}
