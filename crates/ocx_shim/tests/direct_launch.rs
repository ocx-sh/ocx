// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The vendored launcher on Unix, end to end: the real `ocx-shim` binary, a hand-written `.direct` sidecar and a
//! vendor directory laid out the way `ocx vendor` writes one.
#![cfg(unix)]
#![expect(
    clippy::disallowed_types,
    reason = "the test runs the launcher binary as a child process"
)]

use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LAUNCHER_BINARY: &str = env!("CARGO_BIN_EXE_ocx-shim");
const SELF_PATH: &str = "selections/default/shells/default/bin/tool";
const PROGRAM: &str = "packages/p/content/bin/probe";
const VENDOR_ID: &str = "sha256:0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a";

/// Prints `$0`, then each argument, then the environment, so a test reads back exactly what the child received.
const PROBE_SCRIPT: &str = "#!/bin/sh\nprintf 'ARG:%s\\n' \"$0\" \"$@\"\nenv | sed 's/^/ENV:/'\n";

/// A vendor directory in a private temp dir, removed (permissions restored first) on drop.
struct Vendor {
    base: PathBuf,
    root: PathBuf,
}

impl Vendor {
    fn new(label: &str) -> Self {
        Self::with_root(label, "vendor")
    }

    /// A vendor directory at `relative_root` under the private temp dir.
    fn with_root(label: &str, relative_root: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let base = std::env::temp_dir().join(format!("ocx-direct-{label}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let root = base.join(relative_root);
        Self { base, root }
    }

    fn root(&self) -> PathBuf {
        self.root.clone()
    }

    fn launcher(&self) -> PathBuf {
        self.root().join(SELF_PATH)
    }

    fn write(&self, relative: &str, bytes: &[u8], mode: u32) {
        let path = self.root().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Installs the launcher, the probe program and a sidecar whose `args` and `env` are the given JSON fragments.
    fn install(&self, args: &str, unset: &str, env: &str) {
        std::fs::create_dir_all(self.launcher().parent().unwrap()).unwrap();
        std::fs::copy(LAUNCHER_BINARY, self.launcher()).unwrap();
        self.write(PROGRAM, PROBE_SCRIPT.as_bytes(), 0o755);
        let sidecar = format!(
            r#"{{"schema_version":1,"vendor_id":"{VENDOR_ID}","self":"{SELF_PATH}","program":"@VENDOR_DIR@/{PROGRAM}","args":{args},"unset":{unset},"env":{env}}}"#
        );
        self.write(&format!("{SELF_PATH}.direct"), sidecar.as_bytes(), 0o644);
    }

    /// The launcher with a cleared environment holding only `env`, so a test never depends on the host's.
    fn run(&self, caller_args: &[&str], env: &[(&str, &str)]) -> Output {
        run_launcher(&self.launcher(), caller_args, env)
    }
}

/// Runs `launcher` with a cleared environment holding only `env`.
///
/// A thread of this test binary that forks while another holds a just-written executable open makes the kernel
/// refuse the exec with `ETXTBSY`, either here or inside the launcher (exit 74); the file is released within
/// milliseconds, so the run is repeated.
fn run_launcher(launcher: &Path, caller_args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut attempts = 0;
    loop {
        let result = Command::new(launcher)
            .args(caller_args)
            .env_clear()
            .envs(env.iter().copied())
            .output();
        attempts += 1;
        let busy = match &result {
            Ok(output) => output.status.code() == Some(74) && stderr_of(output).contains("Text file busy"),
            Err(error) => error.kind() == std::io::ErrorKind::ExecutableFileBusy,
        };
        if !busy || attempts >= 100 {
            return result.unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

impl Drop for Vendor {
    fn drop(&mut self) {
        make_writable(&self.base);
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn make_writable(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                make_writable(&entry.path());
            }
        }
    }
}

/// The probe's report: the argument lines in order, and the environment as a map.
fn report(output: &Output) -> (Vec<String>, BTreeMap<String, String>) {
    let text = String::from_utf8(output.stdout.clone()).unwrap();
    let mut args = Vec::new();
    let mut env = BTreeMap::new();
    for line in text.lines() {
        if let Some(arg) = line.strip_prefix("ARG:") {
            args.push(arg.to_string());
        } else if let Some(pair) = line.strip_prefix("ENV:")
            && let Some((key, value)) = pair.split_once('=')
        {
            env.insert(key.to_string(), value.to_string());
        }
    }
    (args, env)
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn the_child_sees_the_baked_arguments_then_the_callers_unchanged() {
    let vendor = Vendor::new("argv");
    vendor.install(r#"["--baked","@VENDOR_DIR@/share"]"#, "[]", "[]");
    let output = vendor.run(&["a b", "--flag", "", "$HOME"], &[]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    let (args, _) = report(&output);
    let root = vendor.root();
    assert_eq!(
        args,
        [
            format!("{}/{PROGRAM}", root.display()),
            "--baked".to_string(),
            format!("{}/share", root.display()),
            "a b".to_string(),
            "--flag".to_string(),
            String::new(),
            "$HOME".to_string(),
        ]
    );
}

#[test]
fn the_child_sees_the_environment_the_records_build() {
    let vendor = Vendor::new("env");
    let root = vendor.root();
    let env_records = r#"[
        {"key":"PATH","value":"@VENDOR_DIR@/packages/p/content/bin","kind":"path"},
        {"key":"TOOL_HOME","value":"@VENDOR_DIR@/packages/p/content","kind":"constant"},
        {"key":"CLOSED","value":"replaced","kind":"constant"},
        {"key":"FLAGS","value":"-b","kind":"list","separator":" "}]"#;
    vendor.install("[]", r#"["DROPPED","CLOSED_UNSET"]"#, env_records);
    let output = vendor.run(
        &[],
        &[
            ("PATH", "/usr/bin:/bin"),
            ("DROPPED", "secret"),
            ("CLOSED_UNSET", "x"),
            ("CLOSED", "original"),
            ("FLAGS", "-a"),
            ("PASSTHROUGH", "kept"),
        ],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    let (_, env) = report(&output);
    assert_eq!(
        env.get("PATH").map(String::as_str),
        Some(format!("{}/packages/p/content/bin:/usr/bin:/bin", root.display()).as_str())
    );
    assert_eq!(
        env.get("TOOL_HOME").map(String::as_str),
        Some(format!("{}/packages/p/content", root.display()).as_str())
    );
    assert_eq!(env.get("CLOSED").map(String::as_str), Some("replaced"));
    assert_eq!(env.get("FLAGS").map(String::as_str), Some("-a -b"));
    assert_eq!(env.get("PASSTHROUGH").map(String::as_str), Some("kept"));
}

#[test]
fn an_unset_key_is_gone_from_the_child() {
    let vendor = Vendor::new("unset");
    vendor.install("[]", r#"["OCX_SIGNING_KEY","__OCX_ENV_STATE"]"#, "[]");
    let output = vendor.run(
        &[],
        &[
            ("OCX_SIGNING_KEY", "secret"),
            ("__OCX_ENV_STATE", "state"),
            ("OCX_OTHER", "visible"),
        ],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    let (_, env) = report(&output);
    assert!(!env.contains_key("OCX_SIGNING_KEY"), "{env:?}");
    assert!(!env.contains_key("__OCX_ENV_STATE"), "{env:?}");
    assert_eq!(env.get("OCX_OTHER").map(String::as_str), Some("visible"));
}

#[test]
fn the_child_replaces_the_launcher_and_its_exit_status_is_the_launchers() {
    let vendor = Vendor::new("status");
    vendor.install("[]", "[]", "[]");
    vendor.write(PROGRAM, b"#!/bin/sh\nexit 7\n", 0o755);
    assert_eq!(vendor.run(&[], &[]).status.code(), Some(7));
}

/// Every node of the tree with what a write would change: contents, mode and mtime.
fn snapshot(path: &Path, into: &mut BTreeMap<PathBuf, (Vec<u8>, u32, i64, i64)>) {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    let bytes = if metadata.is_file() {
        std::fs::read(path).unwrap()
    } else {
        Vec::new()
    };
    into.insert(
        path.to_path_buf(),
        (bytes, metadata.mode(), metadata.mtime(), metadata.mtime_nsec()),
    );
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            snapshot(&entry.unwrap().path(), into);
        }
    }
}

fn lock_down(path: &Path) {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            lock_down(&entry.unwrap().path());
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555)).unwrap();
    } else if metadata.is_file() {
        let mode = if metadata.mode() & 0o111 == 0 { 0o444 } else { 0o555 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
}

#[test]
fn the_launcher_starts_from_a_read_only_directory_and_changes_nothing() {
    let vendor = Vendor::new("readonly");
    vendor.install("[]", "[]", "[]");
    lock_down(&vendor.root());

    let mut before = BTreeMap::new();
    snapshot(&vendor.root(), &mut before);
    let output = vendor.run(&["--version"], &[]);
    let mut after = BTreeMap::new();
    snapshot(&vendor.root(), &mut after);

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(before, after);
    assert!(
        before.len() >= 8,
        "the snapshot must have read the whole tree: {}",
        before.len()
    );
}

#[test]
fn a_launcher_without_its_sidecar_exits_78_naming_the_path() {
    let vendor = Vendor::new("nosidecar");
    vendor.install("[]", "[]", "[]");
    std::fs::remove_file(vendor.root().join(format!("{SELF_PATH}.direct"))).unwrap();
    let output = vendor.run(&[], &[]);
    assert_eq!(output.status.code(), Some(78), "{}", stderr_of(&output));
    assert!(stderr_of(&output).contains("tool.direct"), "{}", stderr_of(&output));
}

#[test]
fn a_program_without_the_execute_bit_exits_77() {
    let vendor = Vendor::new("not-executable");
    vendor.install("[]", "[]", "[]");
    vendor.write(PROGRAM, PROBE_SCRIPT.as_bytes(), 0o644);
    let output = vendor.run(&[], &[]);
    assert_eq!(output.status.code(), Some(77), "{}", stderr_of(&output));
}

#[test]
fn a_missing_program_exits_74_naming_it() {
    let vendor = Vendor::new("no-program");
    vendor.install("[]", "[]", "[]");
    std::fs::remove_file(vendor.root().join(PROGRAM)).unwrap();
    let output = vendor.run(&[], &[]);
    assert_eq!(output.status.code(), Some(74), "{}", stderr_of(&output));
    assert!(stderr_of(&output).contains(PROGRAM), "{}", stderr_of(&output));
}

#[test]
fn the_root_is_found_through_the_runfiles_tree_of_the_started_path() {
    // The vendor tree sits under `<launcher>.runfiles/<entry>/`, beside a launcher copy outside it, as a Bazel
    // output holds them.
    let vendor = Vendor::with_root("runfiles", "out/bin/tool.runfiles/pkg");
    vendor.install("[]", "[]", "[]");
    let outside = vendor.base.join("out/bin");
    std::fs::create_dir_all(vendor.base.join("out/bin/tool.runfiles/_main")).unwrap();
    std::fs::copy(LAUNCHER_BINARY, outside.join("tool")).unwrap();
    std::fs::copy(
        vendor.root().join(format!("{SELF_PATH}.direct")),
        outside.join("tool.direct"),
    )
    .unwrap();

    let output = run_launcher(&outside.join("tool"), &[], &[]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    let (args, _) = report(&output);
    assert_eq!(args[0], format!("{}/{PROGRAM}", vendor.root().display()));
}

/// V-66, measure only: the start-up cost the launcher adds when 1000 runfiles entries precede the matching one.
///
/// Run with `cargo test -p ocx_shim --test direct_launch -- --ignored --nocapture`; it prints the numbers and
/// asserts nothing.
#[test]
#[ignore = "timing measurement, run on request"]
fn v66_launcher_overhead_with_one_thousand_runfiles_entries() {
    const ENTRIES: usize = 1000;
    const RUNS: u32 = 50;

    // `zz_match` sorts after every `eNNNN`, so the lookup examines every entry first.
    let vendor = Vendor::with_root("timing", "out/bin/tool.runfiles/zz_match");
    vendor.install("[]", "[]", "[]");
    let outside = vendor.base.join("out/bin");
    let runfiles = outside.join("tool.runfiles");
    for entry in 0..ENTRIES {
        std::fs::create_dir_all(runfiles.join(format!("e{entry:04}"))).unwrap();
    }
    let launcher = outside.join("tool");
    std::fs::copy(LAUNCHER_BINARY, &launcher).unwrap();
    std::fs::copy(
        vendor.root().join(format!("{SELF_PATH}.direct")),
        outside.join("tool.direct"),
    )
    .unwrap();
    let program = vendor.root().join(PROGRAM);

    let time = |command: &Path| {
        let started = std::time::Instant::now();
        for _ in 0..RUNS {
            assert!(run_launcher(command, &[], &[]).status.success());
        }
        started.elapsed() / RUNS
    };
    let direct = time(&program);
    let through_launcher = time(&launcher);
    println!(
        "V-66: {ENTRIES} runfiles entries: probe alone {direct:?}, through the launcher {through_launcher:?}, \
         overhead {:?}",
        through_launcher.saturating_sub(direct)
    );
}
