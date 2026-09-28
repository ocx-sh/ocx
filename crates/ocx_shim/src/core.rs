// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pure shim logic, split from the Win32 calls in [`crate::run`] so the Linux CI host can unit-test it.

use super::ShimError;
use std::ffi::OsStr;

/// Derives the entrypoint stem: the module path's file name with exactly one trailing `.exe` stripped
/// (case-insensitive); [`ShimError::SelfPathFailure`] on a non-UTF-8 path or an empty stem.
pub(super) fn derive_stem(module_path: &OsStr) -> Result<String, ShimError> {
    // Split on both `\` and `/`: `Path::file_name` uses the host separator, so Linux CI would keep the whole path.
    let full = module_path.to_str().ok_or(ShimError::SelfPathFailure)?;
    let file_name = full.rsplit(['\\', '/']).next().unwrap_or(full);
    if file_name.is_empty() {
        return Err(ShimError::SelfPathFailure);
    }

    let stem = match file_name.len().checked_sub(4) {
        Some(cut) if file_name[cut..].eq_ignore_ascii_case(".exe") => &file_name[..cut],
        _ => file_name,
    };
    if stem.is_empty() {
        return Err(ShimError::SelfPathFailure);
    }
    Ok(stem.to_string())
}

/// A parsed sidecar; the variant decides the wire verb, so no payload can be emitted under another sidecar's verb.
#[derive(Debug)]
pub(super) enum Sidecar {
    /// `<stem>.shim`: an installed package's absolute root. Dispatches [`WIRE_SUBCOMMAND`].
    PackageRoot(String),
    /// `<stem>.shimref`: a deferred tool's pinned identifier. Dispatches [`WIRE_SUBCOMMAND_SHIM`], which
    /// materializes the package first.
    PinnedIdentifier(String),
    /// `<stem>.exec`: a trampoline's toolchain home plus the baked absolute `ocx` (`None` for the one-line form).
    /// Dispatches [`WIRE_SUBCOMMAND_EXEC`] behind a root flag.
    ToolchainHome(ToolchainHome, Option<String>),
}

/// The home a `.exec` sidecar names; typed because `--global` takes no value token.
#[derive(Debug)]
pub(super) enum ToolchainHome {
    /// An absolute project root, emitted as `--project "<root>"`.
    Project(String),
    /// The global home, emitted as a valueless `--global`.
    Global,
}

impl ToolchainHome {
    /// The `.exec` literal for the global home, matched by byte equality.
    ///
    /// Must equal `EXEC_SIDECAR_GLOBAL` in `ocx_package_manager`'s `launcher::body`, which writes it; only a paired
    /// golden binds the two.
    pub(super) const GLOBAL: &'static str = "global";

    /// The sidecar line this home was parsed from.
    pub(super) fn as_line(&self) -> &str {
        match self {
            ToolchainHome::Project(root) => root,
            ToolchainHome::Global => Self::GLOBAL,
        }
    }
}

impl Sidecar {
    /// The wire subcommand this sidecar's value is passed to.
    pub(super) fn wire_subcommand(&self) -> &'static str {
        match self {
            Sidecar::PackageRoot(_) => WIRE_SUBCOMMAND,
            Sidecar::PinnedIdentifier(_) => WIRE_SUBCOMMAND_SHIM,
            Sidecar::ToolchainHome(..) => WIRE_SUBCOMMAND_EXEC,
        }
    }

    /// The absolute `ocx` a `.exec` sidecar baked; `None` otherwise, and the caller falls back to the literal `ocx`.
    pub(super) fn baked_ocx(&self) -> Option<&str> {
        match self {
            Sidecar::ToolchainHome(_, ocx) => ocx.as_deref(),
            Sidecar::PackageRoot(_) | Sidecar::PinnedIdentifier(_) => None,
        }
    }

    /// The single value the sidecar carries; for `.exec`, the home line, which [`build_child_command_line`] places
    /// before the verb.
    pub(super) fn value(&self) -> &str {
        match self {
            Sidecar::PackageRoot(value) | Sidecar::PinnedIdentifier(value) => value,
            Sidecar::ToolchainHome(home, _) => home.as_line(),
        }
    }

    /// The path subject to the containment check, or `None` when the sidecar names no package root.
    ///
    /// `.shimref` gets no containment check: it names a registry identifier, whose fetch is digest-verified instead.
    pub(super) fn containment_path(&self) -> Option<&str> {
        match self {
            Sidecar::PackageRoot(root) => Some(root),
            // `.exec` names a project directory, which `pkg_root_allowed`'s roots exclude, so a check would refuse
            // every trampoline.
            Sidecar::PinnedIdentifier(_) | Sidecar::ToolchainHome(..) => None,
        }
    }
}

/// Whether dispatching `sidecar` must clear `OCX_GLOBAL` and `OCX_PROJECT` from the child's environment.
///
/// Only `.exec`: its baked selector is the only selector, so an exported `OCX_GLOBAL` would make every trampoline
/// exit 64 from `check_global_project_exclusivity`. Stripping for the positional grammars would change what
/// `launcher exec` / `launcher shim` resolve for a caller who deliberately exported a tier.
pub(super) fn strips_tier_selectors(sidecar: &Sidecar) -> bool {
    matches!(sidecar, Sidecar::ToolchainHome(..))
}

/// The sidecars a shim probes, in precedence order (installed, deferred, trampoline), each with its grammar's parser.
pub(super) const SIDECAR_PROBE_ORDER: [(&str, SidecarParser); 3] = [
    ("shim", parse_shim_sidecar),
    ("shimref", parse_shimref_sidecar),
    ("exec", parse_exec_sidecar),
];

/// Raw sidecar bytes in, a [`Sidecar`] or [`ShimError::MalformedSidecar`] (exit 78) out.
type SidecarParser = fn(&[u8]) -> Result<Sidecar, ShimError>;

/// Upper bound on a sidecar file, checked by all three grammars; the write side caps nothing, so the reader must not
/// borrow its rules.
const MAX_LEN: usize = 32 * 1024;

/// The read-side rules every sidecar grammar shares: at most [`MAX_LEN`] bytes, one trailing `\r\n` or `\n`
/// stripped, non-empty, no NUL/CR/LF left, valid UTF-8. A BOM is not stripped, so the per-grammar clause refuses it.
fn parse_one_line(raw: &[u8]) -> Result<&str, ShimError> {
    if raw.len() > MAX_LEN {
        return Err(ShimError::MalformedSidecar {
            reason: format!("sidecar larger than {MAX_LEN} bytes"),
        });
    }

    // Only one terminator: a second trailing newline stays and is refused as interior.
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

/// Parses a `<stem>.shim` sidecar: [`parse_one_line`]'s rules, then an absolute package root
/// ([`is_absolute_path`]), else [`ShimError::MalformedSidecar`].
pub(super) fn parse_shim_sidecar(raw: &[u8]) -> Result<Sidecar, ShimError> {
    let pkg_root = parse_one_line(raw)?;

    if !is_absolute_path(pkg_root) {
        return Err(ShimError::MalformedSidecar {
            reason: format!("pkg_root is not absolute: {pkg_root}"),
        });
    }

    Ok(Sidecar::PackageRoot(pkg_root.to_string()))
}

/// Parses a `<stem>.shimref` sidecar: [`parse_one_line`]'s rules, then [`is_pinned_identifier`], else
/// [`ShimError::MalformedSidecar`].
///
/// Refuses a space although the shared writer (`LauncherSafeString`) permits one: a pinned identifier never holds one.
pub(super) fn parse_shimref_sidecar(raw: &[u8]) -> Result<Sidecar, ShimError> {
    let identifier = parse_one_line(raw)?;

    if !is_pinned_identifier(identifier) {
        return Err(ShimError::MalformedSidecar {
            reason: format!("not a pinned identifier: {identifier}"),
        });
    }

    Ok(Sidecar::PinnedIdentifier(identifier.to_string()))
}

/// Parses a `<stem>.exec` sidecar: line one is [`ToolchainHome::GLOBAL`] or an absolute path, the optional line two
/// an absolute baked `ocx`, and a third line is refused; anything rejected is [`ShimError::MalformedSidecar`].
///
/// Each line passes [`parse_one_line`], but [`MAX_LEN`] caps the whole file, so two lines cannot buy more than one.
/// Line two stays optional: an ocx that cannot resolve its own path must still render.
pub(super) fn parse_exec_sidecar(raw: &[u8]) -> Result<Sidecar, ShimError> {
    if raw.len() > MAX_LEN {
        return Err(ShimError::MalformedSidecar {
            reason: format!("sidecar larger than {MAX_LEN} bytes"),
        });
    }

    // One terminator off the file, so a second trailing newline stays an empty line two and is refused.
    let body = raw
        .strip_suffix(b"\r\n")
        .or_else(|| raw.strip_suffix(b"\n"))
        .unwrap_or(raw);

    // Split at the FIRST newline only: everything after it is line two, so a third line fails as an interior newline there.
    let (home_line, baked_line) = match body.iter().position(|&byte| byte == b'\n') {
        // Strip line one's CRLF `\r` only on a split, so a lone trailing CR on a one-line sidecar stays refused.
        Some(at) => (
            body[..at].strip_suffix(b"\r").unwrap_or(&body[..at]),
            Some(&body[at + 1..]),
        ),
        None => (body, None),
    };

    let home = parse_one_line(home_line)?;
    // Byte equality, never trimmed or case-folded: the writer emits one spelling.
    let home = if home == ToolchainHome::GLOBAL {
        ToolchainHome::Global
    } else if is_absolute_path(home) {
        ToolchainHome::Project(home.to_string())
    } else {
        return Err(ShimError::MalformedSidecar {
            reason: format!(
                "toolchain home is neither an absolute path nor `{}`: {home}",
                ToolchainHome::GLOBAL
            ),
        });
    };

    // Line two spares the literal-`ocx` search, which starts in `<home>/toolchain/active/bin`, where a package may ship
    // its own `ocx.exe` and capture every trampoline's spawn.
    let baked = match baked_line {
        Some(line) => {
            let ocx = parse_one_line(line)?;
            if !is_absolute_path(ocx) {
                return Err(ShimError::MalformedSidecar {
                    reason: format!("baked ocx is not absolute: {ocx}"),
                });
            }
            // Not containment-checked: bounded by write access to `<home>/toolchain/active/bin`, owner-only at create.
            Some(ocx.to_string())
        }
        None => None,
    };

    Ok(Sidecar::ToolchainHome(home, baked))
}

/// Structural admissibility check for a `.shimref` value, deliberately not an OCI reference parser: printable ASCII,
/// no leading `-`, and `<algorithm>:<hex>` after the last `@`.
///
/// Everything else is left to `ocx_oci::PinnedIdentifier`, which re-parses the value on the receiving end and exits 64.
fn is_pinned_identifier(value: &str) -> bool {
    if !value.bytes().all(|byte| matches!(byte, 0x21..=0x7E)) {
        return false;
    }

    // A leading `-` would parse as an `ocx` flag and exit 64 instead of 78 naming the file.
    if value.starts_with('-') {
        return false;
    }

    // Split at the last `@`; the first would measure the digest from the wrong place.
    // Requiring a digest stops a tampered sidecar downgrading the fetch to a registry-controlled tag.
    // No algorithm allow-list: a hard-coded `sha256` would reject a `sha384`/`sha512` pin.
    let Some(at) = value.rfind('@') else {
        return false;
    };
    if at == 0 {
        return false;
    }
    let Some((algorithm, hex)) = value[at + 1..].split_once(':') else {
        return false;
    };
    // `split_once` splits at the first colon, so a second one lands in `hex` and fails the hex class.
    !algorithm.is_empty()
        && algorithm.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9'))
        && !hex.is_empty()
        && hex.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Host-independent absolute-path check (`C:\`, `C:/`, `\\server\share`, `\\?\`, or a leading `/`) for a `.shim`
/// root or `.exec` home; `Path::is_absolute` would reject `C:\` on the Linux CI host.
fn is_absolute_path(p: &str) -> bool {
    let bytes = p.as_bytes();
    if p.starts_with("\\\\") {
        return true;
    }
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    p.starts_with('/')
}

/// Whether a canonicalized package root lies inside the containment roots, given the canonicalized `OCX_HOME`.
///
/// Must mirror `validate_launcher_pkg_root`'s allow-list in `ocx_cli`: `packages`, `temp/test`, `temp/patch-test`.
/// Keep the scratch roots enumerated: admitting all of `temp/` lets a tampered sidecar aim at mid-download content.
/// Comparison is `Path::starts_with`, never a string prefix, or `packages-old` would match.
pub(crate) fn pkg_root_allowed(canon_home: &std::path::Path, canon_root: &std::path::Path) -> bool {
    let temp = canon_home.join("temp");
    [canon_home.join("packages"), temp.join("test"), temp.join("patch-test")]
        .iter()
        .any(|allowed| canon_root.starts_with(allowed))
}

/// The program a shim spawns and whether `CreateProcessW` receives it explicitly.
///
/// One value, never a token plus a flag recomputed at the spawn site, which is `#[cfg(windows)]` and beyond any test.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ResolvedProgram {
    /// The leading token of the child command line.
    pub(super) token: String,
    /// Whether `token` is also passed as an explicit `lpApplicationName`.
    pub(super) explicit: bool,
}

impl ResolvedProgram {
    /// `lpApplicationName` for `CreateProcessW`: `Some` to resolve explicitly, `None` for the command-line search.
    ///
    /// A path with spaces left to the command line mis-resolves to `C:\Program.exe` (CWE-428).
    /// `None` searches the calling image's directory before `PATH`, where a co-resident `bin\ocx.exe` wins, so only
    /// [`resolve_program`]'s last rung may produce it.
    pub(super) fn application_name(&self) -> Option<&str> {
        self.explicit.then_some(self.token.as_str())
    }
}

/// Resolves the spawned program: `OCX_BINARY_PIN`, then the `.exec` baked `ocx` ([`Sidecar::baked_ocx`]), then the
/// literal `ocx`, the only rung with a NULL `lpApplicationName`. `pin`: `None` = unset, `Some` = defined (maybe empty).
pub(super) fn resolve_program(pin: Option<&str>, baked: Option<&str>) -> ResolvedProgram {
    // A defined pin wins even when empty (`IF DEFINED`): collapsing it to `ocx` would PATH-search instead of failing.
    // The pin sits above the baked `ocx`, so the operator override wins, as in the POSIX body.
    match pin.or(baked) {
        Some(value) => ResolvedProgram {
            token: value.to_string(),
            explicit: true,
        },
        None => ResolvedProgram {
            token: "ocx".to_string(),
            explicit: false,
        },
    }
}

/// Wire verb for an installed package: `launcher exec "<pkg_root>" -- "<stem>" <argv...>`.
///
/// Frozen and shared with the `.sh` launcher body; [`super::tests::shim_wire_token_matches_sh_body`] fails on drift.
pub(super) const WIRE_SUBCOMMAND: &str = "launcher exec";

/// Wire verb for a deferred tool: `launcher shim "<pinned-id>" -- "<stem>" <argv...>`.
///
/// Frozen; bound to the `.sh` shim body only by the paired goldens `launcher_shim_wire_token_is_bound_to_shim_producer`
/// (`body.rs`) and [`super::tests::shim_ref_wire_token_matches_sh_shim_body`], so a change must touch both.
pub(super) const WIRE_SUBCOMMAND_SHIM: &str = "launcher shim";

/// Wire verb for a trampoline: `<root flag> [<root>] exec -- "<stem>" <argv...>`; the selector comes first because
/// `--project` / `--global` are root flags of `ocx`, not arguments of `exec`.
///
/// Frozen; bound by the paired goldens `trampoline_wire_tokens_are_bound_to_shim_producer` (`body.rs`) and
/// [`super::tests::trampoline_wire_tokens_match_the_sh_trampoline_body`].
pub(super) const WIRE_SUBCOMMAND_EXEC: &str = "exec";

/// Assembles the child command line of the frozen wire ABI; the shape follows the [`Sidecar`] variant, never a value.
///
/// Every token passes through [`append_quoted_arg`], which quotes only what needs it.
pub(super) fn build_child_command_line(program: &str, sidecar: &Sidecar, stem: &str, argv: &[String]) -> String {
    // Every token goes through the `CommandLineToArgvW` quoter, never a hand-written `"…"`: the sidecar is no trust
    // boundary, and a trailing `\` before a hand-written quote escapes it and collapses the argv boundary (CWE-88).
    let (subcommand, value) = (sidecar.wire_subcommand(), sidecar.value());
    let argv_estimate: usize = argv.iter().map(|a| a.len() + 3).sum();
    let mut line =
        String::with_capacity(program.len() + subcommand.len() + value.len() + stem.len() + 22 + argv_estimate);
    append_quoted_arg(&mut line, program);
    match sidecar {
        // `--global` emits no value token; a `""` there would make `ocx` read the verb as the flag's argument.
        Sidecar::ToolchainHome(home, _) => {
            match home {
                ToolchainHome::Project(root) => {
                    line.push_str(" --project ");
                    append_quoted_arg(&mut line, root);
                }
                ToolchainHome::Global => line.push_str(" --global"),
            }
            line.push(' ');
            line.push_str(subcommand);
        }
        Sidecar::PackageRoot(_) | Sidecar::PinnedIdentifier(_) => {
            line.push(' ');
            line.push_str(subcommand);
            line.push(' ');
            append_quoted_arg(&mut line, value);
        }
    }
    line.push_str(" -- ");
    append_quoted_arg(&mut line, stem);
    for arg in argv {
        line.push(' ');
        append_quoted_arg(&mut line, arg);
    }
    line
}

/// Whether `STARTF_USESTDHANDLES` may be set: only when all three std handles are valid.
///
/// Wiring an invalid handle makes `CreateProcessW` hand the child a broken stream instead of the OS default.
pub(super) fn use_std_handles(stdin_valid: bool, stdout_valid: bool, stderr_valid: bool) -> bool {
    stdin_valid && stdout_valid && stderr_valid
}

/// Appends `arg` quoted per the Win32 `CommandLineToArgvW` rules, as `std::process::Command` does.
///
/// Quotes on any ASCII control byte, not just `\t`, or an embedded newline/CR is mis-split by a command-line consumer.
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
    // Double trailing backslashes, or they escape the closing quote.
    for _ in 0..backslashes * 2 {
        line.push('\\');
    }
    line.push('"');
}
