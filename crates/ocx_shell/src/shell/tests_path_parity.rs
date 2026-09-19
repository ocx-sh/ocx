// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! A-19 / C-021 — the one PATH-element comparison rule, pinned across the two
//! halves that implement it.
//!
//! The in-process appliers ([`ocx_util::path::move_to_front`] /
//! [`remove_segment`](ocx_util::path::remove_segment)) and the emitted
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

use ocx_util::env::PATH_SEPARATOR as SEP;
use ocx_util::path::{move_to_front, remove_segment};

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
