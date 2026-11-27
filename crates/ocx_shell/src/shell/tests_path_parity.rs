// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! A-19 / C-021 — the one PATH-element comparison rule, pinned across the two
//! halves that implement it.
//!
//! The in-process appliers ([`ocx_env_apply::move_to_front`] /
//! [`remove_segment`](ocx_env_apply::remove_segment)) and the emitted
//! shell snippets ([`Shell::export_path`](crate::shell::Shell::export_path) /
//! [`remove_list_element`](crate::shell::Shell::remove_list_element)) must
//! decide "same element?" identically, because the reconciler's
//! `C == L.applied` guard compares exactly those two products. Each test here
//! pins the two halves to one another, so a change to either one alone goes
//! red.
//!
//! **Why they live under `shell/` rather than beside the appliers they also
//! exercise** (plan C-042, spec D-013): they name `crate::shell`, and
//! `utility` becomes `ocx_util`, the bottom tier, which may name nothing above
//! itself — the boundary `utility_imports_nothing` holds that. `shell` is the
//! upper of the two halves, so the pairing test belongs on its side of the
//! edge. The in-process half's own tests stay in `utility/path.rs`.

use std::ffi::{OsStr, OsString};

use ocx_env_apply::PATH_SEPARATOR as SEP;
use ocx_env_apply::{move_to_front, remove_segment};

/// Build a separator-joined path string for the host platform so the
/// assertions read naturally on both Unix (`:`) and Windows (`;`).
fn join(parts: &[&str]) -> OsString {
    OsString::from(parts.join(SEP))
}

fn mtf(existing: &OsStr, value: &str) -> OsString {
    move_to_front(existing, OsStr::new(value))
}

fn rm(existing: &OsStr, value: &str) -> OsString {
    remove_segment(existing, OsStr::new(value))
}

/// A case-differing `(ambient, value)` pair spelled for the host platform.
///
/// Spelling matters: `split_paths` splits on `:` off Windows, so a
/// `C:\Opt\Bin` fixture would be torn into two segments there and the
/// comparison under test would never run on the whole element.
#[cfg(windows)]
const CASE_PAIR: (&str, &str) = (r"C:\Opt\Bin", r"C:\opt\bin");
#[cfg(not(windows))]
const CASE_PAIR: (&str, &str) = ("/opt/Bin", "/opt/bin");

/// The comparison folds ASCII case exactly where `Shell::export_path`'s
/// emit does — on Windows, and only there.
///
/// Red state: leave `move_to_front` case-sensitive on Windows (EC-PATH-008),
/// or revert the PowerShell arm to the case-insensitive `-ne`.
#[test]
fn move_to_front_folds_case_exactly_where_the_emit_does() {
    let (ambient, value) = CASE_PAIR;
    let ambient = OsString::from(ambient);
    let folded = mtf(&ambient, value) == *value;

    let emit = crate::shell::Shell::PowerShell.export_path("PATH", value).unwrap();
    assert_eq!(
        folded,
        emit.contains("OrdinalIgnoreCase"),
        "the in-process fold and the emitted comparison must agree; emit: {emit}"
    );
    assert_eq!(
        folded,
        cfg!(windows),
        "A-19: PATH elements fold ASCII case on Windows and nowhere else"
    );
}

/// The same rule in the removal direction, against the same emitted arm.
///
/// Red state: fold in one of the two functions only — the composer would
/// then add a slot the reconciler cannot retire.
#[test]
fn remove_segment_folds_case_exactly_where_the_emit_does() {
    let (ambient, value) = CASE_PAIR;
    let ambient = join(&[ambient, "/keep"]);
    let removed = rm(&ambient, value) == *"/keep";

    let emit = crate::shell::Shell::PowerShell
        .remove_list_element("PATH", value, None)
        .unwrap();
    assert_eq!(
        removed,
        emit.contains("OrdinalIgnoreCase"),
        "the in-process fold and the emitted comparison must agree; emit: {emit}"
    );
    assert_eq!(removed, cfg!(windows), "A-19: the two directions share one rule");
}

/// The removal operand carries one surrounding pair of `"` off, exactly as
/// `Shell::remove_list_element`'s path-kind arm does: the operand is
/// enumerated from the live environment, which spells a space-bearing
/// Windows segment either way.
///
/// Red state: drop the strip on either side (EC-PATH-010).
#[test]
fn remove_segment_strips_one_quote_pair_from_the_operand_as_the_emit_does() {
    let ambient = join(&["/opt/bin", "/x"]);
    assert_eq!(
        rm(&ambient, "\"/opt/bin\""),
        rm(&ambient, "/opt/bin"),
        "a quoted operand must normalise to the bare one, as the emitted arm's does"
    );

    let shell = crate::shell::Shell::Bash;
    assert_eq!(
        shell.remove_list_element("PATH", "\"/opt/bin\"", None),
        shell.remove_list_element("PATH", "/opt/bin", None),
        "the emitted half must carry the same normalisation"
    );
}

/// The in-process comparison folds `/` into `\` exactly where the emitted PowerShell arms do: the
/// emit carries the slash fold on Windows and nowhere else.
///
/// Red state: drop the fold from one arm, or from `path_element_eq`.
#[test]
fn slash_fold_is_emitted_exactly_where_the_in_process_compare_folds() {
    use crate::shell::Shell;
    let folds = |line: &str| line.contains("-replace '/','\\'");
    let export = Shell::PowerShell.export_path("PATH", "/opt/bin").unwrap();
    let remove = Shell::PowerShell.remove_list_element("PATH", "/opt/bin", None).unwrap();
    assert_eq!(folds(&export), cfg!(windows), "export arm: {export}");
    assert_eq!(folds(&remove), cfg!(windows), "remove arm: {remove}");
    // Host spelling: off Windows `split_paths` would tear a drive letter at its `:`.
    let (stored, wanted) = if cfg!(windows) {
        (r"C:\a\b", r"C:\a/b")
    } else {
        (r"/a\b", "/a/b")
    };
    assert_eq!(
        rm(&join(&[stored, "/keep"]), wanted) == *"/keep",
        cfg!(windows),
        "in-process removal folds slashes on Windows only"
    );
}

/// A POSIX shell sourcing the MSYS-form emit for a Windows-form input ends up with the `/c/...`
/// element first and no relative element anywhere in `PATH`.
///
/// Red state: emit the native `C:\...` value under `Dialect::Msys`; bash then splits it at `:`.
#[cfg(unix)]
#[test]
#[expect(
    clippy::disallowed_types,
    reason = "test-only live-shell harness sourcing a generated export line in a real shell"
)]
fn msys_dialect_line_puts_the_converted_element_first_in_live_bash() {
    use crate::shell::{Dialect, Shell};
    use std::process::Command;

    let line = Shell::Bash
        .export_path_as(
            Dialect::Msys {
                drive_prefix: crate::shell::msys::MSYS_DRIVE_PREFIX,
            },
            "PATH",
            r"C:\Users\x\bin",
        )
        .unwrap();
    let script = format!("export PATH=/usr/bin:/bin; {line}; printf '%s' \"$PATH\"");
    // Never `bash` from PATH: on Windows that can be the WSL launcher.
    let Some(bash) = crate::shell::tests_git_bash::find_bash() else {
        crate::shell::tests_git_bash::bash_absent("the MSYS-dialect live PATH check");
        return;
    };
    let output = Command::new(bash).args(["-c", &script]).output().expect("spawn bash");
    assert!(output.status.success(), "bash failed on:\n{script}");
    let path = String::from_utf8_lossy(&output.stdout).into_owned();
    let elements: Vec<&str> = path.split(':').collect();
    assert_eq!(elements.first(), Some(&"/c/Users/x/bin"), "PATH: {path}");
    assert!(
        elements.iter().all(|e| e.starts_with('/')),
        "relative element in {path}"
    );
}

/// fish reads the MSYS-form element for each runtime's drive prefix: it leads `$PATH` and no element is relative.
///
/// Red state: leave fish out of the converted arms; fish then keeps `C:\Users\x\bin` as a relative element.
#[cfg(unix)]
#[test]
#[expect(
    clippy::disallowed_types,
    reason = "test-only live-shell harness sourcing a generated export line in a real shell"
)]
fn msys_dialect_line_puts_the_converted_element_first_in_live_fish() {
    use crate::shell::{Dialect, Shell};
    use std::process::Command;

    for (drive_prefix, expected) in [
        (crate::shell::msys::MSYS_DRIVE_PREFIX, "/c/Users/x/bin"),
        (crate::shell::msys::CYGWIN_DRIVE_PREFIX, "/proc/cygdrive/c/Users/x/bin"),
    ] {
        let line = Shell::Fish
            .export_path_as(Dialect::Msys { drive_prefix }, "PATH", r"C:\Users\x\bin")
            .unwrap();
        let script = format!("set -gx PATH /usr/bin /bin; {line}; string join : $PATH");
        let output = match Command::new("fish").args(["-c", &script]).output() {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                assert!(
                    crate::shell::hook::absence_may_skip("fish"),
                    "fish is absent, so the MSYS-dialect live PATH check proved nothing (__OCX_TESTING_REQUIRE_LIVE_SHELLS names it); install it"
                );
                eprintln!("# ocx: UNPROVEN — fish is not found, so the MSYS-dialect live PATH check asserted nothing");
                return;
            }
            Err(error) => panic!("failed to spawn fish: {error}"),
        };
        assert!(output.status.success(), "fish failed on:\n{script}");
        let path = String::from_utf8_lossy(&output.stdout).trim_end().to_string();
        let elements: Vec<&str> = path.split(':').collect();
        assert_eq!(elements.first(), Some(&expected), "PATH: {path}");
        assert!(
            elements.iter().all(|e| e.starts_with('/')),
            "relative element in {path}"
        );
    }
}
