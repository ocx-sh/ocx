// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Runs one `ocx` process: a scrubbed environment, no shell, bounded output and cancellation.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::runtime::{CancelToken, Error, Limits};
use super::wire::Invocation;

/// How often a running child is checked for exit, cancellation and over-long output.
const POLL: Duration = Duration::from_millis(5);
/// The least time the pipes get to drain after ocx exited, so a zero `cancel_grace` cannot fail a finished run.
const DRAIN_FLOOR: Duration = Duration::from_secs(1);

/// What one run produced.
#[derive(Debug)]
pub struct Output {
    /// The exit status; `None` when a signal ended the process.
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// The one read of this process's environment; everything else works from the snapshot it returns.
#[expect(
    clippy::disallowed_methods,
    reason = "the SDK is the boundary that reads the caller's environment"
)]
pub fn parent_environment() -> Vec<(OsString, OsString)> {
    std::env::vars_os().collect()
}

/// `parent` without any `OCX_*` or `__OCX_*` variable, then `caller`'s pairs, then `OCX_BINARY_PIN` naming `binary`.
///
/// A variable of the caller's own `ocx` setup (credentials, trust, home) must not steer a child it did not ask
/// about, so the inherited ones go and only the explicit ones stay.
pub fn child_environment(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
    caller: &[(OsString, OsString)],
    binary: &Path,
) -> Vec<(OsString, OsString)> {
    let mut environment: Vec<_> = parent.into_iter().filter(|(key, _)| !is_ocx_variable(key)).collect();
    for (key, value) in caller {
        environment.retain(|(existing, _)| existing != key);
        environment.push((key.clone(), value.clone()));
    }
    environment.retain(|(key, _)| key != "OCX_BINARY_PIN");
    environment.push((OsString::from("OCX_BINARY_PIN"), binary.as_os_str().to_owned()));
    environment
}

fn is_ocx_variable(key: &OsStr) -> bool {
    key.to_str().is_some_and(|name| {
        let name = name.to_ascii_uppercase();
        name.starts_with("OCX_") || name.starts_with("__OCX_")
    })
}

/// Runs `binary` with `invocation` and the given environment, waiting for it under `limits` and `cancel`.
///
/// # Errors
///
/// [`Error::Spawn`] when the process cannot start or be waited on, [`Error::OutputTooLarge`] when stdout or stderr
/// pass their limit, [`Error::Cancelled`] when `cancel` fires. The child is killed in the first two cases; in the
/// last it is interrupted and killed only when it outlasts [`Limits::cancel_grace`]. The child itself is never left
/// running; a process it started that still holds the output is not killed.
pub fn run(
    binary: &Path,
    invocation: &Invocation,
    environment: Vec<(OsString, OsString)>,
    limits: &Limits,
    cancel: Option<&CancelToken>,
) -> Result<Output, Error> {
    let mut command = Command::new(binary);
    command
        .args(&invocation.arguments)
        .env_clear()
        .envs(environment)
        .stdin(if invocation.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(Error::Spawn)?;

    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout = child
        .stdout
        .take()
        .map(|pipe| read_capped(pipe, limits.max_stdout, &exceeded));
    let stderr = child
        .stderr
        .take()
        .map(|pipe| read_capped(pipe, limits.max_stderr, &exceeded));
    let writer = child
        .stdin
        .take()
        .zip(invocation.stdin.clone())
        .map(|(mut pipe, secret)| {
            thread::spawn(move || {
                // The child may exit without reading its stdin; the exit status is what reports that.
                let _ = pipe.write_all(secret.expose());
            })
        });

    let status = loop {
        if exceeded.load(Ordering::Relaxed) {
            terminate(&mut child);
            return Err(Error::OutputTooLarge);
        }
        if cancel.is_some_and(CancelToken::is_cancelled) {
            interrupt_then_terminate(&mut child, limits.cancel_grace);
            return Err(Error::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(POLL),
            Err(error) => {
                terminate(&mut child);
                return Err(Error::Spawn(error));
            }
        }
    };

    await_readers(
        stdout.as_ref(),
        stderr.as_ref(),
        writer.as_ref(),
        cancel,
        limits.cancel_grace,
    )?;
    let stdout = collect(stdout);
    let stderr = collect(stderr);
    if let Some(writer) = writer {
        // A panic in the writer thread has nothing to report: it only writes a buffer.
        let _ = writer.join();
    }
    if exceeded.load(Ordering::Relaxed) {
        return Err(Error::OutputTooLarge);
    }
    Ok(Output {
        status: status.code(),
        stdout,
        stderr,
    })
}

/// Reads `pipe` up to `cap` bytes; one byte more flags `exceeded` and stops.
fn read_capped(mut pipe: impl Read + Send + 'static, cap: usize, exceeded: &Arc<AtomicBool>) -> JoinHandle<Vec<u8>> {
    let exceeded = Arc::clone(exceeded);
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 8192];
        while let Ok(read) = pipe.read(&mut chunk) {
            if read == 0 {
                break;
            }
            if buffer.len() + read > cap {
                exceeded.store(true, Ordering::Relaxed);
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        buffer
    })
}

/// Waits until the pipe threads are done, which they are once every holder of the pipes has closed them.
///
/// A daemonised grandchild can keep the pipes open long after the child exited, so the wait ends at `bound`
/// ([`Error::Spawn`]) or on cancel ([`Error::Cancelled`]) instead of joining without end.
// ponytail: only the direct child is ever killed, never its descendants (no process-group kill without a libc
// dependency); a grandchild that outlives the call keeps running and its reader threads stay parked until it exits.
fn await_readers(
    stdout: Option<&JoinHandle<Vec<u8>>>,
    stderr: Option<&JoinHandle<Vec<u8>>>,
    writer: Option<&JoinHandle<()>>,
    cancel: Option<&CancelToken>,
    bound: Duration,
) -> Result<(), Error> {
    let deadline = Instant::now() + bound.max(DRAIN_FLOOR);
    let running = || {
        stdout.is_some_and(|handle| !handle.is_finished())
            || stderr.is_some_and(|handle| !handle.is_finished())
            || writer.is_some_and(|handle| !handle.is_finished())
    };
    while running() {
        if cancel.is_some_and(CancelToken::is_cancelled) {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(Error::Spawn(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the output stayed open after ocx exited",
            )));
        }
        thread::sleep(POLL);
    }
    Ok(())
}

fn collect(reader: Option<JoinHandle<Vec<u8>>>) -> Vec<u8> {
    reader.and_then(|handle| handle.join().ok()).unwrap_or_default()
}

fn terminate(child: &mut Child) {
    // `kill` fails when the child already exited, which is the state wanted; `wait` reaps it either way.
    let _ = child.kill();
    let _ = child.wait();
}

/// Asks the child to stop the way a terminal's Ctrl-C does and gives it `grace` to exit before killing it.
///
/// There is no interrupt to send on Windows, so the child is killed at once there.
fn interrupt_then_terminate(child: &mut Child, grace: Duration) {
    if send_interrupt(child) {
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(POLL);
        }
    }
    terminate(child);
}

/// Whether the interrupt was delivered. The POSIX shell's `kill` builtin sends it, which keeps `unsafe` and a libc
/// dependency out of the SDK; the pid is an argument, never part of the script text.
#[cfg(unix)]
fn send_interrupt(child: &Child) -> bool {
    Command::new("/bin/sh")
        .args(["-c", "kill -INT \"$0\"", &child.id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(unix))]
fn send_interrupt(_child: &Child) -> bool {
    false
}
