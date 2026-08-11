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

/// A parsed sidecar: which one was read, and the single value it carries.
///
/// The two variants bind a wire verb to the payload that verb consumes, so a
/// pinned identifier can never be emitted under `launcher exec` (which assumes
/// the package already exists) and a package root can never be emitted under
/// `launcher shim` (which assumes it does not). That pairing is the reason this
/// is an enum rather than a `String` plus a separately-carried verb.
#[derive(Debug)]
pub(super) enum Sidecar {
    /// `<stem>.shim` — the absolute package root of an **installed** package.
    /// Dispatches [`WIRE_SUBCOMMAND`].
    PackageRoot(String),
    /// `<stem>.shimref` — the pinned identifier of a **deferred** tool whose
    /// package is deliberately absent. Dispatches [`WIRE_SUBCOMMAND_SHIM`],
    /// which materializes it before dispatching (C-017).
    PinnedIdentifier(String),
}

impl Sidecar {
    /// The wire subcommand this sidecar's value is passed to.
    pub(super) fn wire_subcommand(&self) -> &'static str {
        match self {
            Sidecar::PackageRoot(_) => WIRE_SUBCOMMAND,
            Sidecar::PinnedIdentifier(_) => WIRE_SUBCOMMAND_SHIM,
        }
    }

    /// The single value the sidecar carries, emitted as the first positional
    /// after the wire subcommand.
    pub(super) fn value(&self) -> &str {
        match self {
            Sidecar::PackageRoot(value) | Sidecar::PinnedIdentifier(value) => value,
        }
    }

    /// The filesystem path this sidecar subjects to the E3 containment check,
    /// or `None` when it names no path.
    ///
    /// `.shimref` returns `None` **by design**, not by omission: it carries a
    /// registry identifier, so there is no local path to canonicalize and
    /// nothing for [`pkg_root_allowed`] to compare against. The consequence is
    /// stated plainly because no test may claim otherwise (C-011 trust
    /// boundary): a deferred tool gets **no** E3 containment defense-in-depth.
    /// What stands in its place is that the materialization `launcher shim`
    /// performs is addressed by the digest baked into the identifier and is
    /// content-verified on fetch — the same integrity the sidecar's own write
    /// access already bounds.
    pub(super) fn containment_path(&self) -> Option<&str> {
        match self {
            Sidecar::PackageRoot(root) => Some(root),
            Sidecar::PinnedIdentifier(_) => None,
        }
    }
}

/// The sidecars a shim probes for, in precedence order, each paired with the
/// parser for its own grammar: `<stem>.shim` (an installed package) is tried
/// before `<stem>.shimref` (a deferred tool).
///
/// The order is a tie-break for a state that cannot arise from ocx's own
/// writes — the two sidecars are produced into different trees
/// (`entrypoints/` by the launcher generator, a shim tree's `bin/` by
/// `prepare_lazy`) and never into the same directory. If both are somehow
/// present, the installed package wins: it is the state that needs no
/// download, so preferring it is the fail-safe reading.
pub(super) const SIDECAR_PROBE_ORDER: [(&str, SidecarParser); 2] =
    [("shim", parse_shim_sidecar), ("shimref", parse_shimref_sidecar)];

/// The read side of one sidecar grammar: raw file bytes in, a [`Sidecar`] out,
/// or [`ShimError::MalformedSidecar`] (E2, exit 78).
type SidecarParser = fn(&[u8]) -> Result<Sidecar, ShimError>;

/// Hard upper bound on a sidecar file, applied by the read side of **both**
/// grammars before any further work. Defends against a corrupt/huge file.
///
/// Deliberately stricter than the write side, which imposes no length cap at
/// all (`launcher/body.rs`, `launcher/safety.rs`): the reader re-validates
/// independently rather than trusting what wrote the file.
const MAX_LEN: usize = 32 * 1024;

/// The shared read-side rules of every sidecar grammar, applied before the
/// per-grammar clause. Returns the single line with its terminator stripped.
///
/// Stated in full rather than by reference, because both grammars are frozen
/// on-disk contracts and neither inherits from the other by assumption
/// (C-017):
///
/// 1. Input larger than [`MAX_LEN`] (32 KiB) → [`ShimError::MalformedSidecar`].
/// 2. Exactly ONE trailing terminator is stripped: `\r\n`, `\n`, or none. A
///    second trailing newline is an *interior* newline and is rejected by (4).
/// 3. Empty after the strip → rejected.
/// 4. Any `0x00`, `0x0A` or `0x0D` remaining in the body → rejected.
/// 5. Not valid UTF-8 → rejected.
///
/// UTF-8 is required but a BOM is not stripped: the write side emits none, so
/// a leading `\u{feff}` reaches the per-grammar clause and fails there.
fn parse_one_line(raw: &[u8]) -> Result<&str, ShimError> {
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

    std::str::from_utf8(body).map_err(|_| ShimError::MalformedSidecar {
        reason: "not valid UTF-8".to_string(),
    })
}

/// Parses + validates the raw bytes of a `<stem>.shim` sidecar, returning the
/// contained absolute `pkg_root`.
///
/// Grammar: the five shared rules in [`parse_one_line`], then the clause that
/// is this grammar's own — the value must be an absolute path
/// ([`is_absolute_path`]). Anything rejected is
/// [`ShimError::MalformedSidecar`], exit 78 (E2).
pub(super) fn parse_shim_sidecar(raw: &[u8]) -> Result<Sidecar, ShimError> {
    let pkg_root = parse_one_line(raw)?;

    if !is_absolute_path(pkg_root) {
        return Err(ShimError::MalformedSidecar {
            reason: format!("pkg_root is not absolute: {pkg_root}"),
        });
    }

    Ok(Sidecar::PackageRoot(pkg_root.to_string()))
}

/// Parses + validates the raw bytes of a `<stem>.shimref` sidecar, returning
/// the contained pinned identifier (C-017).
///
/// # The grammar, written out rather than inherited
///
/// `.shimref` is a frozen on-disk contract in its own right. It shares the
/// five read-side rules of [`parse_one_line`] — 32 KiB cap, exactly one
/// stripped terminator, non-empty after the strip, no `0x00`/`0x0A`/`0x0D`,
/// valid UTF-8 — and substitutes a **pinned-identifier clause** where `.shim`
/// has an absolute-path clause. That substitution is the only intended
/// divergence, and it is [`is_pinned_identifier`].
///
/// Note one place the two sidecars are NOT symmetric with the write side: the
/// shared writer ([`LauncherSafeString`](../../ocx_lib/src/package_manager/launcher/safety.rs))
/// permits a space, because a Windows `pkg_root` routinely contains one. A
/// pinned identifier never does, so this read side rejects it. A reader that
/// re-derived its rules from the writer's would have missed that.
///
/// Anything rejected is [`ShimError::MalformedSidecar`], exit 78 (E2).
pub(super) fn parse_shimref_sidecar(raw: &[u8]) -> Result<Sidecar, ShimError> {
    let identifier = parse_one_line(raw)?;

    if !is_pinned_identifier(identifier) {
        return Err(ShimError::MalformedSidecar {
            reason: format!("not a pinned identifier: {identifier}"),
        });
    }

    Ok(Sidecar::PinnedIdentifier(identifier.to_string()))
}

/// The `.shimref` read side's substitution for `.shim`'s absolute-path clause:
/// a **structural admissibility check**, deliberately not an OCI reference
/// parser.
///
/// # What it checks
///
/// 1. **Digest-bearing.** Everything after the last `@` must be
///    `<algorithm>:<hex>` — `<algorithm>` one or more `[a-z0-9]`, `<hex>` one
///    or more `[0-9a-f]`, nothing after it — and the `@` must not be the first
///    byte. This is the clause with real value: ocx bakes only a
///    `PinnedIdentifier`, so the fetch it triggers is digest-addressed, and a
///    tampered sidecar therefore cannot *downgrade* the reference to a mutable
///    tag the registry side controls. No algorithm allow-list and no digest
///    length table: `sha384`/`sha512` are already `oci::Algorithm` variants,
///    and a reader that hard-coded `sha256` would reject a future pin it was
///    never meant to adjudicate.
/// 2. **No leading `-`.** Otherwise `ocx`'s own argument parser reads the
///    positional as a flag and the failure surfaces as a usage error (64) from
///    a process the user did not invoke, instead of E2 (78) naming the file.
/// 3. **Printable ASCII only** (`0x21..=0x7E`): no space, no DEL, no control
///    byte, nothing non-ASCII. Every part of an OCI reference — host,
///    repository, tag, digest — is drawn from restricted ASCII, so this costs
///    no legitimate value and keeps the identifier clear of anything the
///    `CommandLineToArgvW` quoter would otherwise have to neutralize.
///
/// # What it deliberately does NOT check
///
/// Registry-host validity, the repository-path grammar, tag grammar, the
/// digest's length or its algorithm's existence, and whether the digest names
/// anything real. Authority for all of that stays with
/// `ocx_lib::oci::PinnedIdentifier`, which re-parses this exact value on the
/// receiving end of the wire and exits 64 if it does not hold (C-011). This
/// narrowness is the point: a second, hand-rolled OCI reference parser living
/// in a dependency-free crate would be a wire-format parser owned in the wrong
/// place, and the two copies would drift.
fn is_pinned_identifier(value: &str) -> bool {
    // Clause 3 first: it is the cheapest and it bounds what the two clauses
    // below can see, so they only ever reason about printable ASCII.
    if !value.bytes().all(|byte| matches!(byte, 0x21..=0x7E)) {
        return false;
    }

    // Clause 2.
    if value.starts_with('-') {
        return false;
    }

    // Clause 1. The LAST `@`, not the first: a value carrying more than one is
    // not a realistic reference, but splitting at the first would measure the
    // digest from the wrong place and is the mistake worth being immune to.
    let Some(at) = value.rfind('@') else {
        return false;
    };
    if at == 0 {
        return false;
    }
    let Some((algorithm, hex)) = value[at + 1..].split_once(':') else {
        return false;
    };
    // `split_once` splits at the FIRST colon, so a second one lands in `hex`
    // and fails the hex class — which is the intended rejection.
    !algorithm.is_empty()
        && algorithm.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9'))
        && !hex.is_empty()
        && hex.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
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

/// Whether a canonicalized package root is inside the shim's E3 containment
/// boundary, given the canonicalized `OCX_HOME`.
///
/// Three roots are admitted, and they mirror `validate_launcher_pkg_root`'s
/// allow-list in `ocx_cli` exactly — `$OCX_HOME/packages` (installed
/// candidates), `$OCX_HOME/temp/test` (`ocx package test`) and
/// `$OCX_HOME/temp/patch-test` (`ocx patch test`). The two scratch roots are
/// enumerated rather than collapsed to `$OCX_HOME/temp`: `temp/` also holds
/// in-progress download directories, so admitting the whole subtree would let
/// a tampered sidecar aim the shim at un-assembled, mid-download content and
/// still clear E3.
///
/// Comparison is component-wise (`Path::starts_with`), so a sibling whose name
/// merely shares a prefix — `temp-evil`, `packages-old` — does not match.
/// Both arguments must already be canonicalized by the caller; this function
/// performs no I/O so it stays host-runnable on the Linux CI.
pub(crate) fn pkg_root_allowed(canon_home: &std::path::Path, canon_root: &std::path::Path) -> bool {
    let temp = canon_home.join("temp");
    [canon_home.join("packages"), temp.join("test"), temp.join("patch-test")]
        .iter()
        .any(|allowed| canon_root.starts_with(allowed))
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

/// The wire-ABI vocabulary the shim emits for a **deferred** tool:
/// `launcher shim "<pinned-id>" -- "<stem>" <argv...>` (C-017, C-018).
///
/// The second frozen wire token, and it has the same two-producer problem as
/// [`WIRE_SUBCOMMAND`]: `ocx_lib` cannot depend on this binary crate, so
/// nothing the compiler can see binds this literal to the `.sh` shim body in
/// `ocx_lib`'s `package_manager::launcher::body`. The binding is a PAIRED
/// GOLDEN — `body.rs`'s `launcher_shim_wire_token_is_bound_to_shim_producer`
/// restates it from the `.sh` side, and
/// [`super::tests::shim_ref_wire_token_matches_sh_shim_body`] restates it from
/// here. A change to this verb must touch both or one canary fails loudly
/// (`subsystem-package-manager.md` canary rule).
pub(super) const WIRE_SUBCOMMAND_SHIM: &str = "launcher shim";

/// Assembles the child command line reproducing the frozen wire ABI:
/// `<program> <verb> "<value>" -- "<stem>" <argv...>`, where the verb/value
/// pair comes from the parsed [`Sidecar`] — `launcher exec` with a package
/// root, or `launcher shim` with a pinned identifier.
///
/// SECURITY (B1/B2): `program`, the sidecar value, and `stem` are all routed
/// through the [`append_quoted_arg`] `CommandLineToArgvW` quoter — NOT
/// hand-written `"…"` wrapping. The sidecar is explicitly **not** a trust
/// boundary ([`parse_shim_sidecar`] tolerantly accepts `"` and a trailing `\`;
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
pub(super) fn build_child_command_line(program: &str, sidecar: &Sidecar, stem: &str, argv: &[String]) -> String {
    let (subcommand, value) = (sidecar.wire_subcommand(), sidecar.value());
    // Argv-aware capacity estimate: the shim is on every-invocation hot
    // path (one process per launcher call). `len()+3` per arg covers the
    // separating space plus a quote pair in the common quoted case.
    let argv_estimate: usize = argv.iter().map(|a| a.len() + 3).sum();
    let mut line =
        String::with_capacity(program.len() + subcommand.len() + value.len() + stem.len() + 12 + argv_estimate);
    append_quoted_arg(&mut line, program);
    line.push(' ');
    line.push_str(subcommand);
    line.push(' ');
    append_quoted_arg(&mut line, value);
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
