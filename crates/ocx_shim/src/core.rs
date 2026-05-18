// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pure (host-runnable) shim logic — split from the Win32 syscalls so the
//! wire-ABI assembler, sidecar parser, stem derivation, and program
//! resolution can be unit-tested on the Linux CI host (system_design §8
//! mandates the pure/Win32 split). The functions here take plain values
//! (no `GetModuleFileNameW`, `CreateProcessW`, or job object) and are NOT
//! gated behind `#[cfg(windows)]`. The Win32 orchestration in
//! [`crate::run`] calls into these.

use super::ShimError;
use std::ffi::OsStr;

/// Derives the entrypoint stem from the shim's own module path: the file
/// name with exactly one trailing `.exe` stripped (case-insensitive).
/// `cmake.exe` → `cmake`; `clang-format.exe` → `clang-format`; a name
/// without a trailing `.exe` is returned unchanged.
///
/// Pure: the Win32 `GetModuleFileNameW` lookup that produces
/// `module_path` stays in [`super::run`]; this only does the
/// string/extension transform.
pub(super) fn derive_stem(module_path: &OsStr) -> Result<String, ShimError> {
    // The module path is always a Windows path (`GetModuleFileNameW`),
    // even when this pure function is exercised host-side on Linux CI.
    // `std::path::Path::file_name` is platform-conditional on the
    // separator, so split on BOTH `\` and `/` explicitly to stay
    // host-runnable (system_design §8 pure/Win32 split).
    let full = module_path.to_str().ok_or(ShimError::SelfPathFailure)?;
    let file_name = full.rsplit(['\\', '/']).next().unwrap_or(full);
    if file_name.is_empty() {
        return Err(ShimError::SelfPathFailure);
    }

    // Strip exactly ONE trailing `.exe` (case-insensitive). Interior dots
    // are preserved: `clang-format.exe` → `clang-format`,
    // `tool.exe.exe` → `tool.exe`. A name without a trailing `.exe` is
    // returned unchanged.
    let stem = match file_name.len().checked_sub(4) {
        Some(cut) if file_name[cut..].eq_ignore_ascii_case(".exe") => &file_name[..cut],
        _ => file_name,
    };
    if stem.is_empty() {
        return Err(ShimError::SelfPathFailure);
    }
    Ok(stem.to_string())
}

/// Parses + validates the raw bytes of a `<stem>.shim` sidecar, returning
/// the contained absolute `pkg_root` with a single trailing `\r?\n`
/// terminator stripped.
///
/// Accepts: trailing `\n`, trailing `\r\n`, or no terminator.
/// Rejects (→ [`ShimError::MalformedSidecar`]): empty after strip, any
/// `0x00`, any interior `0x0A`/`0x0D` before the terminator, invalid
/// UTF-8, non-absolute path, or input larger than 32 KiB.
pub(super) fn parse_sidecar(raw: &[u8]) -> Result<String, ShimError> {
    // Defends against a corrupt/huge file before any further work.
    const MAX_LEN: usize = 32 * 1024;
    if raw.len() > MAX_LEN {
        return Err(ShimError::MalformedSidecar {
            reason: format!("sidecar larger than {MAX_LEN} bytes"),
        });
    }

    // Strip a single trailing terminator: `\r\n`, `\n`, or none. Only ONE
    // terminator is stripped — a second trailing newline is an interior
    // newline and rejected below.
    let body = if let Some(stripped) = raw.strip_suffix(b"\r\n") {
        stripped
    } else if let Some(stripped) = raw.strip_suffix(b"\n") {
        stripped
    } else {
        raw
    };

    if body.is_empty() {
        return Err(ShimError::MalformedSidecar {
            reason: "empty after stripping the terminator".to_string(),
        });
    }

    // No NUL, and no interior CR/LF before the terminator (the terminator
    // was already stripped, so any remaining `\r`/`\n` is interior).
    for &byte in body {
        match byte {
            0x00 => {
                return Err(ShimError::MalformedSidecar {
                    reason: "embedded NUL byte".to_string(),
                });
            }
            b'\n' => {
                return Err(ShimError::MalformedSidecar {
                    reason: "embedded newline".to_string(),
                });
            }
            b'\r' => {
                return Err(ShimError::MalformedSidecar {
                    reason: "embedded carriage return".to_string(),
                });
            }
            _ => {}
        }
    }

    let pkg_root = std::str::from_utf8(body).map_err(|_| ShimError::MalformedSidecar {
        reason: "not valid UTF-8".to_string(),
    })?;

    if !is_absolute_path(pkg_root) {
        return Err(ShimError::MalformedSidecar {
            reason: format!("pkg_root is not absolute: {pkg_root}"),
        });
    }

    Ok(pkg_root.to_string())
}

/// Absolute-path check for a sidecar `pkg_root`. Recognises Windows
/// absolute forms (`C:\...`, `\\server\share`, `\\?\...`) and a leading
/// `/` so the parser is host-runnable on the Linux CI without depending on
/// `std::path::Path::is_absolute`'s platform-conditional behaviour.
fn is_absolute_path(p: &str) -> bool {
    let bytes = p.as_bytes();
    // UNC / device path: `\\server\share`, `\\?\C:\...`.
    if p.starts_with("\\\\") {
        return true;
    }
    // Drive-absolute: `C:\` or `C:/`.
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    // POSIX-absolute (defensive; OCX_HOME is normally a drive path on
    // Windows but tests and exotic layouts may use `/`).
    p.starts_with('/')
}

/// Resolves the program to spawn, applying the Windows
/// `IF DEFINED OCX_BINARY_PIN` semantics the ADR §Error Taxonomy E5/E6
/// mandates: if `OCX_BINARY_PIN` is **defined at all** (present, even as
/// an empty string) → that value; **only when unset** → the literal
/// `"ocx"` (PATH lookup).
///
/// `pin` models the env lookup result: `None` = unset, `Some(value)` =
/// defined (value may be empty).
pub(super) fn resolve_program(pin: Option<&str>) -> String {
    // `IF DEFINED OCX_BINARY_PIN` semantics (ADR §Error Taxonomy E5/E6):
    // *defined at all* (even empty) → use its value; *only unset* →
    // literal `ocx`. Empty must NOT collapse to `ocx` (that is the Unix
    // `${VAR:-ocx}` behaviour, deliberately out of scope).
    match pin {
        Some(value) => value.to_string(),
        None => "ocx".to_string(),
    }
}

/// The wire-ABI vocabulary the shim emits between the program token and
/// the forwarded argv: `launcher exec "<pkg_root>" -- "<stem>"`. This pair
/// of subcommand tokens is the frozen wire surface shared with the `.sh`
/// launcher body (`body.rs`). The cross-producer canary
/// [`super::tests::shim_wire_token_matches_sh_body`] fails if this drifts
/// from the `.sh` body, keeping the shim bound as the 2nd wire-ABI
/// reproducer (`.sh` ⇄ shim; the `.cmd` producer was removed in the
/// Axis C cutover; `subsystem-package-manager.md` canary rule).
pub(super) const WIRE_SUBCOMMAND: &str = "launcher exec";

/// Assembles the child command line reproducing the frozen wire ABI:
/// `<program> launcher exec "<pkg_root>" -- "<stem>" <argv...>`.
///
/// SECURITY (B1/B2): `program`, `pkg_root`, and `stem` are all routed
/// through the [`append_quoted_arg`] `CommandLineToArgvW` quoter — NOT
/// hand-written `"…"` wrapping. The sidecar is explicitly **not** a trust
/// boundary ([`parse_sidecar`] tolerantly accepts `"` and a trailing `\`;
/// `LauncherSafeString` ran at install time on a *different* machine), so
/// the runtime shim must neutralise an embedded `"` and a trailing `\`
/// (a trailing backslash before a hand-written closing quote escapes it →
/// argv-boundary collapse, CWE-88). Forwarded argv uses the same quoter.
/// The shim NEVER routes through `cmd.exe`.
///
/// `program` is emitted as the leading command-line token ONLY for the
/// unset-`OCX_BINARY_PIN` → literal `ocx` PATH-search case (see
/// [`spawn_application_name`]); a pinned program is passed to
/// `CreateProcessW` via `lpApplicationName` and is NOT parsed from this
/// string (CWE-428). It is still quoted here so the leading token is
/// well-formed when it IS used.
pub(super) fn build_child_command_line(program: &str, pkg_root: &str, stem: &str, argv: &[String]) -> String {
    // Argv-aware capacity estimate: the shim is on every-invocation hot
    // path (one process per launcher call). `len()+3` per arg covers the
    // separating space plus a quote pair in the common quoted case.
    let argv_estimate: usize = argv.iter().map(|a| a.len() + 3).sum();
    let mut line =
        String::with_capacity(program.len() + WIRE_SUBCOMMAND.len() + pkg_root.len() + stem.len() + 12 + argv_estimate);
    append_quoted_arg(&mut line, program);
    line.push(' ');
    line.push_str(WIRE_SUBCOMMAND);
    line.push(' ');
    append_quoted_arg(&mut line, pkg_root);
    line.push_str(" -- ");
    append_quoted_arg(&mut line, stem);
    for arg in argv {
        line.push(' ');
        append_quoted_arg(&mut line, arg);
    }
    line
}

/// Decides what `CreateProcessW` receives as `lpApplicationName`.
///
/// SECURITY (B2 / CWE-428): a pinned `OCX_BINARY_PIN` (a real filesystem
/// path that may contain spaces, e.g. `C:\Program Files\…\ocx.cmd`) MUST
/// be passed as an explicit, NUL-terminated `lpApplicationName` so
/// `CreateProcessW` performs **no** command-line program-name parsing
/// (otherwise `C:\Program Files\…` mis-resolves to `C:\Program.exe`).
///
/// `lpApplicationName = NULL` (command-line program search) is acceptable
/// **only** for the unset-`OCX_BINARY_PIN` → literal `"ocx"` case, which
/// legitimately needs a PATH/`PATHEXT` search that `lpApplicationName`
/// does not perform. `pin_defined` is `true` when `OCX_BINARY_PIN` is
/// present in the environment (even empty) — `IF DEFINED` semantics
/// (ADR §Error Taxonomy E5/E6): a defined-but-empty pin still takes the
/// pin branch and resolves explicitly (an empty `lpApplicationName` then
/// fails the spawn deterministically rather than silently parsing the
/// command line).
///
/// Returns `Some(program)` to pass explicitly via `lpApplicationName`,
/// or `None` to leave `lpApplicationName = NULL` (literal `ocx` PATH
/// search only).
pub(super) fn spawn_application_name(program: &str, pin_defined: bool) -> Option<&str> {
    if pin_defined { Some(program) } else { None }
}

/// Whether `STARTF_USESTDHANDLES` may be set: `true` **only** when all three
/// std handles (stdin, stdout, stderr) are real, valid handles.
///
/// SECURITY/CORRECTNESS (no-console regression vs the removed `.cmd` path):
/// a parent without a console (detached process, GUI subsystem, Windows
/// service) yields `NULL`/`INVALID_HANDLE_VALUE` for one or more std handles.
/// Setting `STARTF_USESTDHANDLES` while wiring an invalid handle as a child
/// std stream makes `CreateProcessW` hand the child a broken stream instead
/// of letting the OS provide a default one. The shim MUST still launch the
/// child in that case, so the flag is set only when every handle is valid;
/// otherwise the caller leaves `hStd*` zeroed and the OS supplies default
/// streams.
///
/// Pure (no Win32): the `GetStdHandle` + validity probe stays in
/// [`super::run`]; this only encodes the all-three-valid policy so it is
/// host-testable on the Linux CI (system_design §8 pure/Win32 split).
pub(super) fn use_std_handles(stdin_valid: bool, stdout_valid: bool, stderr_valid: bool) -> bool {
    stdin_valid && stdout_valid && stderr_valid
}

/// Appends `arg` to `line` using the Win32 `CommandLineToArgvW` quoting
/// rules (the same algorithm Rust's `std::process::Command` uses to build
/// a command line). An argument is wrapped in double quotes when it is
/// empty, or contains a space, tab, double quote, **or any ASCII control
/// byte**; backslashes that immediately precede a double quote (or the
/// closing quote) are doubled; an embedded `"` is escaped as `\"`.
///
/// The predicate is deliberately widened to *all* ASCII control bytes (not
/// just `\t`): a forwarded argv carrying an embedded newline/CR would
/// otherwise be mis-split by a generic command-line consumer
/// (design record "Review-Fix amendments" §3).
fn append_quoted_arg(line: &mut String, arg: &str) {
    let needs_quotes = arg.is_empty()
        || arg
            .bytes()
            .any(|b| b == b' ' || b == b'\t' || b == b'"' || b.is_ascii_control());
    if !needs_quotes {
        line.push_str(arg);
        return;
    }
    line.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => {
                backslashes += 1;
            }
            '"' => {
                // Double the run of backslashes, then escape the quote.
                for _ in 0..backslashes * 2 + 1 {
                    line.push('\\');
                }
                backslashes = 0;
                line.push('"');
            }
            _ => {
                for _ in 0..backslashes {
                    line.push('\\');
                }
                backslashes = 0;
                line.push(ch);
            }
        }
    }
    // Trailing backslashes precede the closing quote — double them so the
    // quote is not escaped by them.
    for _ in 0..backslashes * 2 {
        line.push('\\');
    }
    line.push('"');
}
