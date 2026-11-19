// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test harness that runs a real interpreter, and says so when it cannot.

use std::ffi::OsStr;
use std::path::Path;

/// What an interpreter run left behind.
pub(crate) struct Ran {
    pub stdout: String,
    pub stderr: String,
}

/// Whether `__OCX_TESTING_REQUIRE_LIVE_SHELLS` makes a missing `shell` a failure.
fn required(shell: &str) -> bool {
    let raw = ocx_env::__OCX_TESTING_REQUIRE_LIVE_SHELLS.get().unwrap_or_default();
    let raw = raw.trim();
    raw == "1" || raw.eq_ignore_ascii_case("all") || raw.split(',').any(|name| name.trim() == shell)
}

/// Run `shell args…` with exactly `env` and null stdin. `None` when `shell` is not installed, which
/// fails the test when the environment says it must be live and is otherwise reported as unproven.
#[expect(
    clippy::disallowed_types,
    reason = "test-only live-shell harness sourcing generated text in a real shell"
)]
pub(crate) fn run(shell: &str, args: &[&OsStr], env: &[(&str, &OsStr)], cwd: &Path) -> Option<Ran> {
    let mut command = std::process::Command::new(shell);
    command
        .args(args)
        .env_clear()
        .current_dir(cwd)
        .stdin(std::process::Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    match command.output() {
        Ok(output) => {
            let ran = Ran {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            };
            assert!(
                output.status.success(),
                "{shell} exited {:?}: {}",
                output.status,
                ran.stderr
            );
            Some(ran)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            assert!(!required(shell), "{shell} is required live here but is not installed");
            eprintln!("# ocx: UNPROVEN — {shell} is not installed, this live test ran nothing under it");
            None
        }
        Err(err) => panic!("{shell} failed to start: {err}"),
    }
}
