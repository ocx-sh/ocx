// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Live-bash proof of the MSYS activation emit (ocx-sh/ocx#753).
//!
//! The helpers take the bash executable and a [`Dialect`], so the Linux test drives the same code
//! with `/bin/bash` and [`Dialect::Native`]; the Windows test is a thin caller that resolves Git for
//! Windows' bash and passes [`Dialect::host`]. `bash` on a Windows `PATH` may be the WSL launcher,
//! so it is never looked up there.

use std::path::{Path, PathBuf};

use ocx_util::path::path_element_eq;

use crate::shell::{Dialect, Shell, hook, msys};

#[expect(
    clippy::disallowed_types,
    reason = "test-only live-shell harness sourcing generated export lines in a real shell"
)]
use std::process::Command;

const PROBE: &str = "probe-ocx-msys";

/// Bash to drive on this host, or `None` when it is not installed.
#[cfg(unix)]
pub(crate) fn find_bash() -> Option<PathBuf> {
    ["/bin/bash", "/usr/bin/bash"]
        .into_iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
}

/// Git for Windows' bash, by absolute path: derived from `git --exec-path`, then the standard install
/// roots. A candidate under `System32` is the WSL launcher and is rejected.
#[cfg(windows)]
#[expect(
    clippy::disallowed_types,
    reason = "test-only lookup of Git for Windows: `git` is not a program ocx launches for a user"
)]
pub(crate) fn find_bash() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(output) = Command::new("git").arg("--exec-path").output()
        && output.status.success()
    {
        let exec_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        roots.extend(exec_path.ancestors().map(Path::to_path_buf));
    }
    for var in [&ocx_env::PROGRAM_FILES, &ocx_env::PROGRAM_FILES_X86] {
        if let Some(base) = var.get_os() {
            roots.push(PathBuf::from(base).join("Git"));
        }
    }
    roots
        .iter()
        .flat_map(|root| {
            [
                root.join("bin").join("bash.exe"),
                root.join("usr").join("bin").join("bash.exe"),
            ]
        })
        .find(|candidate| {
            candidate.is_file()
                && !candidate
                    .components()
                    .any(|part| part.as_os_str().eq_ignore_ascii_case("system32"))
        })
}

/// Run `script` under `bash -c`, returning stdout; any failure panics with the script and stderr.
#[expect(
    clippy::disallowed_types,
    reason = "test-only live-shell harness sourcing generated export lines in a real shell"
)]
fn bash_stdout(bash: &Path, script: &str) -> String {
    let output = Command::new(bash)
        .args(["-c", script])
        .output()
        .unwrap_or_else(|error| panic!("failed to spawn {}: {error}", bash.display()));
    assert!(
        output.status.success(),
        "{} exited {} on:\n{script}\nstderr: {}",
        bash.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).replace('\r', "")
}

/// The value printed after the `@@name` marker line (one line).
fn section<'a>(stdout: &'a str, name: &str) -> &'a str {
    let marker = format!("@@{name}");
    let mut lines = stdout.lines();
    lines.find(|line| *line == marker);
    lines
        .next()
        .unwrap_or_else(|| panic!("no `{marker}` section in:\n{stdout}"))
}

/// What one bash session saw while a PATH entry was added and removed.
struct PathSession {
    version: String,
    before_export: String,
    after_export: String,
    probe: String,
    native_child: Option<String>,
    after_remove: String,
}

/// Export `dir` through the emit, resolve a probe script in it, optionally run `native_child_cmd`
/// (a command line whose last stdout line is the native child's `PATH`), then remove `dir` again.
fn path_session(bash: &Path, dialect: Dialect, dir: &Path, native_child_cmd: Option<&str>) -> PathSession {
    let dir_str = dir.to_string_lossy();
    let export = Shell::Bash
        .export_path_as(dialect, "PATH", &dir_str)
        .expect("export line");
    let remove = Shell::Bash
        .remove_list_element_as(dialect, "PATH", &dir_str, None)
        .expect("remove line");
    let native = native_child_cmd
        .map(|cmd| format!("echo @@native; {cmd} | tail -n 1\n"))
        .unwrap_or_default();
    let script = format!(
        "echo @@version; echo \"$BASH_VERSION\"\n\
         echo @@before; printf '%s\\n' \"$PATH\"\n\
         {export}\n\
         echo @@path; printf '%s\\n' \"$PATH\"\n\
         echo @@probe; command -v {PROBE} || echo missing\n\
         {native}\
         {remove}\n\
         echo @@removed; printf '%s\\n' \"$PATH\"\n"
    );
    let stdout = bash_stdout(bash, &script);
    PathSession {
        version: section(&stdout, "version").to_string(),
        before_export: section(&stdout, "before").to_string(),
        after_export: section(&stdout, "path").to_string(),
        probe: section(&stdout, "probe").to_string(),
        native_child: native_child_cmd.map(|_| section(&stdout, "native").to_string()),
        after_remove: section(&stdout, "removed").to_string(),
    }
}

/// Parts a, c and (when `native_child_cmd` is given) b of the contract: the entry leads `$PATH` in the
/// dialect's spelling, bash resolves a command placed there, a native child sees the directory under
/// the in-process comparison rule, and the removal line takes the entry out again.
fn check_path_entry(bash: &Path, dialect: Dialect, native_child_cmd: Option<&str>) {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let dir = temp.path();
    let probe = dir.join(PROBE);
    std::fs::write(&probe, "#!/bin/sh\nexit 0\n").expect("write probe");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).expect("chmod probe");
    }

    let native = dir.to_string_lossy();
    let expected = match dialect {
        Dialect::Msys { drive_prefix } => msys::list_element(&native, drive_prefix).into_owned(),
        Dialect::Native => native.clone().into_owned(),
    };
    let session = path_session(bash, dialect, dir, native_child_cmd);

    let front: Vec<&str> = session.after_export.split(':').take(3).collect();
    eprintln!("# ocx: bash {} PATH front {front:?}", session.version);
    if let Some(child) = &session.native_child {
        eprintln!("# ocx: native child PATH {child}");
    }

    let elements: Vec<&str> = session.after_export.split(':').collect();
    assert_eq!(
        elements.first(),
        Some(&expected.as_str()),
        "PATH: {}",
        session.after_export
    );
    // Only what the line added: the inherited PATH is the host's and may hold anything.
    let inherited: Vec<&str> = session.before_export.split(':').collect();
    for element in elements
        .iter()
        .filter(|element| !element.is_empty() && !inherited.contains(element))
    {
        assert!(
            element.starts_with('/'),
            "relative element {element:?} in {}",
            session.after_export
        );
        assert!(
            element.len() > 1,
            "bare element {element:?} in {}",
            session.after_export
        );
    }
    assert!(
        session.probe.starts_with(&format!("{expected}/{PROBE}")),
        "bash resolved {PROBE} to {:?}, expected under {expected}",
        session.probe
    );

    if let Some(child) = &session.native_child {
        assert!(
            child.split(';').any(|element| path_element_eq(
                std::ffi::OsStr::new(element),
                std::ffi::OsStr::new(&*native),
                true
            )),
            "the native child's PATH lacks {native}: {child}"
        );
    }

    let restored: Vec<&str> = session
        .after_remove
        .split(':')
        .filter(|element| !element.is_empty())
        .collect();
    let before: Vec<&str> = inherited
        .iter()
        .copied()
        .filter(|element| !element.is_empty())
        .collect();
    assert_eq!(
        restored, before,
        "the removal line must leave the PATH as it was before the export"
    );
}

/// Part d: `exe` — a runnable binary in a directory whose name holds a space — works as a command
/// word, `[ -x … ]` operand and wrapped function target.
fn check_command_word_and_wrapper(bash: &Path, dialect: Dialect, exe: &Path) {
    let word = Shell::Bash.command_word_as(dialect, exe);
    assert!(!word.contains('\''), "fixture path must be quote-free: {word}");
    let wrapper = hook::wrapper_as(dialect, Shell::Bash, exe).expect("bash wrapper");
    let script = format!(
        "[ -x '{word}' ] && echo exec-ok\n\
         '{word}' >/dev/null; echo status=$?\n\
         {wrapper}\n\
         ocx >/dev/null; echo wrapper-status=$?\n"
    );
    let stdout = bash_stdout(bash, &script);
    for expected in ["exec-ok", "status=0", "wrapper-status=0"] {
        assert!(
            stdout.lines().any(|line| line == expected),
            "no `{expected}` in:\n{stdout}\nword {word}"
        );
    }
}

/// The skip-and-require convention: absent bash fails where the environment promises it.
pub(crate) fn bash_absent(context: &str) {
    assert!(
        hook::absence_may_skip("bash"),
        "bash is absent, so {context} proved nothing (__OCX_TESTING_REQUIRE_LIVE_SHELLS names it); install it"
    );
    eprintln!("# ocx: UNPROVEN — bash is not found, so {context} asserted nothing");
}

#[cfg(unix)]
#[test]
fn the_native_dialect_round_trips_through_live_bash() {
    let Some(bash) = find_bash() else {
        bash_absent("the PATH round trip");
        return;
    };
    check_path_entry(&bash, Dialect::Native, None);

    let temp = tempfile::TempDir::new().expect("tempdir");
    let dir = temp.path().join("with space");
    std::fs::create_dir(&dir).expect("mkdir");
    let exe = dir.join("ocx-probe");
    let source = ["/bin/true", "/usr/bin/true"]
        .into_iter()
        .map(Path::new)
        .find(|candidate| candidate.is_file())
        .expect("a `true` binary");
    std::fs::copy(source, &exe).expect("copy binary");
    check_command_word_and_wrapper(&bash, Dialect::Native, &exe);
}

#[cfg(windows)]
#[test]
fn the_msys_dialect_round_trips_through_live_git_bash() {
    let Some(bash) = find_bash() else {
        bash_absent("the Git Bash round trip");
        return;
    };
    check_path_entry(&bash, Dialect::host(), Some(r#"cmd.exe //c "echo %PATH%""#));

    let temp = tempfile::TempDir::new().expect("tempdir");
    let dir = temp.path().join("with space");
    std::fs::create_dir(&dir).expect("mkdir");
    let system_root = ocx_env::SYSTEM_ROOT.get_os().unwrap_or_else(|| r"C:\Windows".into());
    let exe = dir.join("whoami.exe");
    std::fs::copy(PathBuf::from(system_root).join("System32").join("whoami.exe"), &exe).expect("copy whoami");
    check_command_word_and_wrapper(&bash, Dialect::host(), &exe);
}
