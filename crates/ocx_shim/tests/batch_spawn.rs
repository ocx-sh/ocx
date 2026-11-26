// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! A vendored `.cmd` target on Windows, end to end: hostile arguments never run an injected command (BatBadBut,
//! CVE-2024-24576). The launcher either hands the argument to the batch file or refuses it with exit 64.
//!
//! Runs on Windows CI only; the dispatch decision itself is a host-run unit in `direct.rs`.
#![cfg(windows)]
#![expect(
    clippy::disallowed_types,
    reason = "the test runs the launcher binary as a child process"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LAUNCHER_BINARY: &str = env!("CARGO_BIN_EXE_ocx-shim");
const SELF_PATH: &str = "selections/default/shells/default/bin/tool";
const PROGRAM: &str = "packages/p/content/bin/probe.cmd";
const VENDOR_ID: &str = "sha256:0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a";

/// Echoes its first argument with the quotes it arrived in, so a quoted `&` stays inert inside the batch file too.
const PROBE_SCRIPT: &str = "@echo off\r\necho ARG1=%1\r\n";

/// What an injected command writes into the working directory.
const MARKER: &str = "injected.marker";

/// A vendor directory with the launcher, a batch target and its sidecar, in a private temp dir.
struct Vendor {
    base: PathBuf,
}

impl Vendor {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let base = std::env::temp_dir().join(format!("ocx-batch-{label}-{}-{nanos}", std::process::id()));
        let root = base.join("vendor");
        let launcher = root.join(format!("{SELF_PATH}.exe"));
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::copy(LAUNCHER_BINARY, &launcher).unwrap();
        let program = root.join(PROGRAM);
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, PROBE_SCRIPT).unwrap();
        let sidecar = format!(
            r#"{{"schema_version":1,"vendor_id":"{VENDOR_ID}","self":"{SELF_PATH}","program":"@VENDOR_DIR@/{PROGRAM}","args":[],"unset":[],"env":[]}}"#
        );
        std::fs::write(root.join(format!("{SELF_PATH}.direct")), sidecar).unwrap();
        Self { base }
    }

    fn launcher(&self) -> PathBuf {
        self.base.join("vendor").join(format!("{SELF_PATH}.exe"))
    }

    /// Runs the launcher with `argument`, from a working directory an injected command would write into.
    fn run(&self, argument: &str) -> Output {
        Command::new(self.launcher())
            .arg(argument)
            .current_dir(&self.base)
            .output()
            .unwrap()
    }

    fn injected(&self) -> bool {
        Path::new(&self.base).join(MARKER).exists()
    }
}

impl Drop for Vendor {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[test]
fn a_plain_argument_reaches_the_batch_file_verbatim() {
    let vendor = Vendor::new("plain");
    for argument in ["plain", "with space"] {
        let output = vendor.run(argument);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains(argument), "{argument:?} not in {stdout:?}");
    }
}

#[test]
fn a_hostile_argument_never_runs_an_injected_command() {
    let injection = format!("echo INJECTED>{MARKER}");
    let hostile = [
        format!("x&{injection}"),
        format!("x\"&{injection}&\""),
        format!("x|{injection}"),
        format!("x^&{injection}"),
        format!("x\n{injection}"),
        format!("x\r\n{injection}"),
        "%PATH%".to_string(),
        "%CD%&%CD%".to_string(),
        "\"".to_string(),
    ];
    for argument in hostile {
        let vendor = Vendor::new("hostile");
        let output = vendor.run(&argument);
        let code = output.status.code();
        assert!(
            matches!(code, Some(0 | 64)),
            "{argument:?} exited {code:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if code == Some(64) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.starts_with("ocx-shim: "), "{argument:?}: {stderr:?}");
        }
        assert!(!vendor.injected(), "{argument:?} ran an injected command");
    }
}
