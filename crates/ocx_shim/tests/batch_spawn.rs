// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Windows launches end to end. A vendored `.cmd` target: hostile arguments never run an injected command (BatBadBut,
//! CVE-2024-24576). An argument std can quote reaches the batch file (exit 0); one holding a line break is refused (exit 64).
//! A program whose path exceeds `MAX_PATH` starts, from a `direct` record and from a `package` record; one that cannot names the limit.
//! A launcher whose real path is past `MAX_PATH`, started through a directory junction, finds its record and starts a program.
//!
//! Runs on Windows CI only; the dispatch decision itself is a host-run unit in `direct.rs`.
#![cfg(windows)]
#![expect(
    clippy::disallowed_types,
    reason = "the test runs the launcher binary as a child process"
)]

use std::os::windows::ffi::OsStrExt;
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
            r#"{{"kind":"direct","schema_version":1,"vendor_id":"{VENDOR_ID}","self":"{SELF_PATH}","program":"@VENDOR_DIR@/{PROGRAM}","args":[],"unset":[],"env":[]}}"#
        );
        std::fs::write(root.join(format!("{SELF_PATH}.launch")), sidecar).unwrap();
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

/// std quotes every metacharacter for a batch file, so these reach the script inert and the launcher exits 0.
#[test]
fn a_hostile_argument_std_can_quote_runs_no_injected_command() {
    let injection = format!("echo INJECTED>{MARKER}");
    let quotable = [
        format!("x&{injection}"),
        format!("x\"&{injection}&\""),
        format!("x|{injection}"),
        format!("x^&{injection}"),
        "%PATH%".to_string(),
        "%CD%&%CD%".to_string(),
        "\"".to_string(),
    ];
    for argument in quotable {
        let vendor = Vendor::new("quotable");
        let output = vendor.run(&argument);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{argument:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!vendor.injected(), "{argument:?} ran an injected command");
    }
}

/// std refuses a line break, which cmd.exe would read as the end of the command: the launcher exits 64 naming the argument.
#[test]
fn an_argument_with_a_line_break_is_refused_with_exit_64() {
    let injection = format!("echo INJECTED>{MARKER}");
    for argument in [format!("x\n{injection}"), format!("x\r\n{injection}")] {
        let vendor = Vendor::new("refused");
        let output = vendor.run(&argument);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(64), "{argument:?}: {stderr}");
        assert!(stderr.starts_with("ocx-shim: "), "{argument:?}: {stderr:?}");
        assert!(stderr.contains("argument 1 "), "{argument:?}: {stderr:?}");
        assert!(!vendor.injected(), "{argument:?} ran an injected command");
    }
}

/// The deepest directory name of [`LongPath::program`]; the child names it when it reports its own path.
const DEEP_LEAF: &str = "leaf-past-max-path";

/// A temp dir holding a real executable, a copy of the launcher, under a directory deeper than `MAX_PATH`.
///
/// The copy has no sidecar of its own, so once started it exits 78 with `sidecar not found` naming its own path:
/// that exit and that path prove the child ran. A launcher that cannot start it exits 74 or 69 instead.
struct LongPath {
    base: PathBuf,
    relative: String,
}

impl LongPath {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let base = std::env::temp_dir().join(format!("ocx-long-{label}-{}-{nanos}", std::process::id()));
        let segments: Vec<String> = (0..5).map(|index| format!("{index}{}", "d".repeat(49))).collect();
        let relative = format!("packages/{}/{DEEP_LEAF}/probe.exe", segments.join("/"));
        let long_path = Self { base, relative };
        let program = long_path.program();
        assert!(
            program.as_os_str().encode_wide().count() >= 260,
            "the program path must exceed MAX_PATH: {}",
            program.display()
        );
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::copy(LAUNCHER_BINARY, &program).unwrap();
        long_path
    }

    fn vendor_root(&self) -> PathBuf {
        self.base.join("vendor")
    }

    /// Writes the `package` record a classic launcher in `bin` reads, naming the base directory as its root.
    fn write_package_record(&self, bin: &Path) {
        let package_root = self.base.display().to_string().replace('\\', "\\\\");
        let record = format!(r#"{{"kind":"package","schema_version":1,"package_root":"{package_root}"}}"#);
        std::fs::write(bin.join("tool.launch"), record).unwrap();
    }

    fn program(&self) -> PathBuf {
        self.vendor_root().join(self.relative.replace('/', "\\"))
    }

    fn assert_child_ran(output: &Output) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(78), "{stderr}");
        assert!(stderr.contains("sidecar not found"), "{stderr}");
        assert!(
            stderr.contains(DEEP_LEAF),
            "the child must report its own deep path: {stderr}"
        );
    }
}

impl Drop for LongPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[test]
fn a_direct_launcher_starts_a_program_past_max_path() {
    let long_path = LongPath::new("direct");
    let root = long_path.vendor_root();
    let launcher = root.join(format!("{SELF_PATH}.exe"));
    std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    std::fs::copy(LAUNCHER_BINARY, &launcher).unwrap();
    let sidecar = format!(
        r#"{{"kind":"direct","schema_version":1,"vendor_id":"{VENDOR_ID}","self":"{SELF_PATH}","program":"@VENDOR_DIR@/{}","args":[],"unset":[],"env":[]}}"#,
        long_path.relative
    );
    std::fs::write(root.join(format!("{SELF_PATH}.launch")), sidecar).unwrap();

    let output = Command::new(&launcher).current_dir(&long_path.base).output().unwrap();

    LongPath::assert_child_ran(&output);
}

/// The launcher image lies under nested directories that put its real path past `MAX_PATH`, and it is started through a
/// short directory junction onto that tree. The record beside the real image is read and the short batch target runs.
///
/// Guards the image lookup: with `GetMappedFileNameW` the launcher exits 78 with "cannot resolve the launcher image".
#[test]
fn a_launcher_whose_real_path_is_past_max_path_starts_through_a_junction() {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let base = std::env::temp_dir().join(format!("ocx-junction-{}-{nanos}", std::process::id()));
    let root = base.join("vendor");
    let nested: Vec<String> = (0..5).map(|index| format!("{index}{}", "d".repeat(49))).collect();
    let self_path = format!("{}/tool", nested.join("/"));
    let launcher = root.join(format!("{self_path}.exe").replace('/', "\\"));
    assert!(
        launcher.as_os_str().encode_wide().count() > 260,
        "the real image path must exceed MAX_PATH: {}",
        launcher.display()
    );
    std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    std::fs::copy(LAUNCHER_BINARY, &launcher).unwrap();
    // The child stays short: the launcher image is what must be long.
    let program = root.join(PROGRAM);
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    std::fs::write(&program, PROBE_SCRIPT).unwrap();
    let sidecar = format!(
        r#"{{"kind":"direct","schema_version":1,"vendor_id":"{VENDOR_ID}","self":"{self_path}","program":"@VENDOR_DIR@/{PROGRAM}","args":[],"unset":[],"env":[]}}"#
    );
    std::fs::write(root.join(format!("{self_path}.launch").replace('/', "\\")), sidecar).unwrap();

    // The junction target is short, so `mklink` takes it; the length is in the directories beneath it.
    let link = base.join("active");
    let made = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&root)
        .output()
        .unwrap();
    assert!(made.status.success(), "mklink /J: {made:?}");
    let attributes = std::fs::symlink_metadata(&link).unwrap().file_attributes();
    assert!(attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0, "{attributes:#x}");
    let through_junction = link.join(format!("{self_path}.exe").replace('/', "\\"));
    assert!(through_junction.is_file(), "{}", through_junction.display());

    let output = Command::new(&through_junction)
        .arg("hello")
        .current_dir(&base)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let _ = std::fs::remove_dir_all(&base);

    assert!(!stderr.contains("cannot resolve the launcher image"), "{stderr}");
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("ARG1=hello"), "the program did not run: {stdout:?}");
}

#[test]
fn a_classic_launcher_starts_a_pinned_ocx_past_max_path() {
    let long_path = LongPath::new("classic");
    let bin = long_path.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let launcher = bin.join("tool.exe");
    std::fs::copy(LAUNCHER_BINARY, &launcher).unwrap();
    long_path.write_package_record(&bin);

    let output = Command::new(&launcher)
        .env("OCX_BINARY_PIN", long_path.program())
        .env_remove("OCX_HOME")
        .current_dir(&long_path.base)
        .output()
        .unwrap();

    LongPath::assert_child_ran(&output);
}

/// A relative pin is refused as not absolute before any start, whatever its length: one stderr line, exit 78
/// (`adr_unified_launcher.md` § `ocx` resolution, rung 1).
#[test]
fn a_relative_pin_past_max_path_is_refused_as_not_absolute_on_the_one_stderr_line() {
    let long_path = LongPath::new("relative-pin");
    let bin = long_path.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let launcher = bin.join("tool.exe");
    std::fs::copy(LAUNCHER_BINARY, &launcher).unwrap();
    long_path.write_package_record(&bin);
    let missing = format!("{}\\absent\\ocx.exe", long_path.relative.replace('/', "\\"));
    assert!(
        missing.encode_utf16().count() >= 260,
        "the pin must reach MAX_PATH: {missing}"
    );

    let output = Command::new(&launcher)
        .env("OCX_BINARY_PIN", &missing)
        .env_remove("OCX_HOME")
        .current_dir(&long_path.base)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(78), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "one ocx-shim line: {stderr}");
    assert!(
        stderr.starts_with("ocx-shim: OCX_BINARY_PIN must be an absolute path: "),
        "{stderr}"
    );
}

/// A batch target whose path reaches `MAX_PATH` never runs: whether std refuses the start or cmd.exe exits non-zero,
/// the one stderr line carries the length-limit clause (the line's exact text is a host unit in `direct.rs`). The
/// launcher here lies under a short path, so this says nothing about the launcher image lookup.
#[test]
fn a_batch_target_past_max_path_names_the_length_limit_on_the_stderr_line() {
    let long_path = LongPath::new("batch");
    let relative = long_path.relative.replace("probe.exe", "probe.cmd");
    std::fs::write(long_path.vendor_root().join(relative.replace('/', "\\")), PROBE_SCRIPT).unwrap();
    let root = long_path.vendor_root();
    let launcher = root.join(format!("{SELF_PATH}.exe"));
    std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    std::fs::copy(LAUNCHER_BINARY, &launcher).unwrap();
    let sidecar = format!(
        r#"{{"kind":"direct","schema_version":1,"vendor_id":"{VENDOR_ID}","self":"{SELF_PATH}","program":"@VENDOR_DIR@/{relative}","args":[],"unset":[],"env":[]}}"#
    );
    std::fs::write(root.join(format!("{SELF_PATH}.launch")), sidecar).unwrap();

    let output = Command::new(&launcher).current_dir(&long_path.base).output().unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.starts_with("ocx-shim: "), "{stderr}");
    assert!(
        stderr.contains("and a batch file runs through cmd.exe, which has the same limit; move the directory"),
        "{stderr}"
    );
}
