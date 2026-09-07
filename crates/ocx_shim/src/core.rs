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
/// The three variants bind a wire verb to the payload that verb consumes, so a
/// pinned identifier can never be emitted under `launcher exec` (which assumes
/// the package already exists), a package root can never be emitted under
/// `launcher shim` (which assumes it does not), and a toolchain home — the only
/// payload that precedes its verb, as a root flag — can never be emitted as
/// either. That pairing is the reason this is an enum rather than a `String`
/// plus a separately-carried verb.
#[derive(Debug)]
pub(super) enum Sidecar {
    /// `<stem>.shim` — the absolute package root of an **installed** package.
    /// Dispatches [`WIRE_SUBCOMMAND`].
    PackageRoot(String),
    /// `<stem>.shimref` — the pinned identifier of a **deferred** tool whose
    /// package is deliberately absent. Dispatches [`WIRE_SUBCOMMAND_SHIM`],
    /// which materializes it before dispatching (C-017).
    PinnedIdentifier(String),
    /// `<stem>.exec` — the toolchain home a rendered **trampoline** re-enters
    /// ocx for, plus the absolute `ocx` the render resolved. Dispatches
    /// [`WIRE_SUBCOMMAND_EXEC`] behind a **root flag**, the one grammar here
    /// whose selector precedes the verb (C-031, C-032).
    ///
    /// The second field is the optional baked program (V-9) and it is `None`
    /// for the one-line form, which stays valid — see [`parse_exec_sidecar`].
    ToolchainHome(ToolchainHome, Option<String>),
}

/// The two homes a `<stem>.exec` sidecar can name, and the reason that sidecar
/// parses into a typed value rather than a bare `String`: `--global` takes **no
/// value token**, so a string carrying `"global"` would have to be
/// re-discriminated at the emit site — the same string-vs-verb coupling
/// [`Sidecar`]'s own shape exists to prevent.
#[derive(Debug)]
pub(super) enum ToolchainHome {
    /// An absolute project root, emitted as `--project "<root>"`.
    Project(String),
    /// The global home, emitted as a valueless `--global`.
    Global,
}

impl ToolchainHome {
    /// The literal a `.exec` sidecar carries for the global home, matched by
    /// **byte equality** (C-031).
    ///
    /// Paired with `ocx_lib`'s `package_manager::launcher::body`
    /// `EXEC_SIDECAR_GLOBAL`, which writes it; the two are bound only by the
    /// C-034 paired golden, since `ocx_lib` cannot depend on this crate.
    pub(super) const GLOBAL: &'static str = "global";

    /// The sidecar line this home was parsed from — the inverse of
    /// [`parse_exec_sidecar`]'s clause, so [`Sidecar::value`] stays total
    /// without any variant having to invent a value it does not carry.
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

    /// The absolute `ocx` a rendered trampoline baked, when it has one (V-9).
    ///
    /// Only `.exec` can carry it: the two positional grammars are read by a
    /// shim in an *installed package's* tree, where the co-resident-`ocx.exe`
    /// hazard this exists to close does not arise — nothing renders an `ocx`
    /// beside them. `None` for the one-line `.exec` form, and the caller then
    /// falls back to the literal `ocx`, which is the accepted narrow risk the
    /// POSIX body records at `launcher/generate.rs`'s rung ladder.
    pub(super) fn baked_ocx(&self) -> Option<&str> {
        match self {
            Sidecar::ToolchainHome(_, ocx) => ocx.as_deref(),
            Sidecar::PackageRoot(_) | Sidecar::PinnedIdentifier(_) => None,
        }
    }

    /// The single value the sidecar carries.
    ///
    /// For the two positional grammars this is the first token after the wire
    /// subcommand. For `.exec` it is the sidecar **line** — which the emitter
    /// places *before* the verb, as the root flag's argument, and omits
    /// entirely for [`ToolchainHome::Global`]. Position is
    /// [`build_child_command_line`]'s to decide; this only reports the value.
    pub(super) fn value(&self) -> &str {
        match self {
            Sidecar::PackageRoot(value) | Sidecar::PinnedIdentifier(value) => value,
            Sidecar::ToolchainHome(home, _) => home.as_line(),
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
    ///
    /// `.exec` returns `None` for a **different** reason, and conflating the
    /// two would be a mistake (C-031): its value IS a local absolute path, but
    /// it is a project *selector*, not a package root. `pkg_root_allowed`'s
    /// allow-list is `$OCX_HOME/packages` plus two scratch roots, and a project
    /// directory is under none of them — a containment check here would refuse
    /// every legitimate trampoline. What stands in its place is that `ocx`
    /// re-resolves the selector through the ordinary project chain, which
    /// validates the home on its own terms.
    pub(super) fn containment_path(&self) -> Option<&str> {
        match self {
            Sidecar::PackageRoot(root) => Some(root),
            Sidecar::PinnedIdentifier(_) | Sidecar::ToolchainHome(..) => None,
        }
    }
}

/// Whether dispatching `sidecar` must clear `OCX_GLOBAL` and `OCX_PROJECT`
/// from the child's environment (C-033, S-009) — the Windows counterpart of the
/// POSIX trampoline body's `unset OCX_GLOBAL OCX_PROJECT`.
///
/// True for `.exec` and **only** for `.exec` (RUL-14 / D-V20). A trampoline's
/// baked selector is the only selector, so one exported `OCX_GLOBAL` would
/// otherwise make every trampoline on that `PATH` exit 64 from
/// `check_global_project_exclusivity`, for a flag nobody typed. The two
/// positional grammars bake no tier selector at all, so they have nothing to
/// shadow — and stripping there would change what `launcher exec` /
/// `launcher shim` resolve for a caller who deliberately exported a tier.
///
/// Extracted rather than written inline at the spawn site because that site is
/// `#[cfg(windows)]`: the scope decision is the whole of RUL-14, and inside the
/// Win32 arm no test on this host can observe it (RUL-39).
pub(super) fn strips_tier_selectors(sidecar: &Sidecar) -> bool {
    matches!(sidecar, Sidecar::ToolchainHome(..))
}

/// The sidecars a shim probes for, in precedence order, each paired with the
/// parser for its own grammar: `<stem>.shim` (an installed package), then
/// `<stem>.shimref` (a deferred tool), then `<stem>.exec` (a toolchain
/// trampoline).
///
/// The order is a tie-break for a state that cannot arise from ocx's own
/// writes — the three sidecars are produced into different trees
/// (`entrypoints/` by the launcher generator, a shim tree's `bin/` by
/// `prepare_lazy`, `<home>/toolchain/bin/` by the toolchain renderer) and never
/// into the same directory. If more than one is somehow present, the installed
/// package wins: it is the state that needs no download and no re-resolution,
/// so preferring it is the fail-safe reading. `.exec` is last for the same
/// reason it is the most indirect: it does not name what to run, it names a
/// home ocx must compose first.
pub(super) const SIDECAR_PROBE_ORDER: [(&str, SidecarParser); 3] = [
    ("shim", parse_shim_sidecar),
    ("shimref", parse_shimref_sidecar),
    ("exec", parse_exec_sidecar),
];

/// The read side of one sidecar grammar: raw file bytes in, a [`Sidecar`] out,
/// or [`ShimError::MalformedSidecar`] (E2, exit 78).
type SidecarParser = fn(&[u8]) -> Result<Sidecar, ShimError>;

/// Hard upper bound on a sidecar file, applied by the read side of **all
/// three** grammars before any further work — `.shim`, `.shimref` and `.exec`,
/// through the one [`parse_one_line`] they share. Defends against a
/// corrupt/huge file.
///
/// Deliberately stricter than the write side, which imposes no length cap at
/// all (`launcher/body.rs`, `launcher/safety.rs`): the reader re-validates
/// independently rather than trusting what wrote the file.
const MAX_LEN: usize = 32 * 1024;

/// The shared read-side rules of every sidecar grammar, applied before the
/// per-grammar clause. Returns the single line with its terminator stripped.
///
/// Stated in full rather than by reference, because all three grammars —
/// `.shim`, `.shimref`, `.exec` — are frozen on-disk contracts and none
/// inherits from another by assumption (C-017, C-031):
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

/// Parses + validates the raw bytes of a `<stem>.exec` sidecar, returning the
/// toolchain home a trampoline re-enters ocx for and the absolute `ocx` the
/// render baked (C-031, V-9).
///
/// # The grammar, written out rather than inherited
///
/// `.exec` is a frozen on-disk contract in its own right, so its rules are
/// stated here rather than referred to. It is the only one of the three that
/// is **not** one line:
///
/// 1. **Line one — the home clause.** Byte equality with
///    [`ToolchainHome::GLOBAL`], **or** [`is_absolute_path`]. Byte equality,
///    not a case-insensitive or trimmed compare: the writer emits one
///    spelling, and a tolerant reader here would be a second grammar nobody
///    specified. The two clauses cannot collide — `global` is not an absolute
///    path, and `/global` is — so the order they are tried in carries no
///    meaning.
/// 2. **Line two — the baked `ocx`, OPTIONAL.** [`is_absolute_path`], with no
///    `global` alternative: it names a program, not a home. Absent is the
///    degraded arm and stays valid, because an ocx that cannot resolve its own
///    path still has to render (`launcher/generate.rs`'s rung ladder).
///
/// A third line is refused: the split takes the FIRST newline only, so
/// everything after it is line two, and line two carrying an interior newline
/// fails rule 4 below.
///
/// Each line is then put through the five read-side rules of
/// [`parse_one_line`] **verbatim** — 32 KiB cap, exactly one stripped
/// terminator, non-empty after the strip, no `0x00`/`0x0A`/`0x0D`, valid UTF-8.
/// The cap is applied to the whole file first, so two lines cannot buy more
/// than one.
///
/// # Why the second line exists at all (V-9)
///
/// Without it the shim resolves the literal `ocx` and spawns with
/// `lpApplicationName = NULL` — and that search begins at the directory the
/// calling image loaded from, i.e. `<home>/toolchain/bin` itself. A package
/// claiming the name `ocx` is admitted by design (ADR D-4 removed
/// `ShimNameShadowsOcx`), so `bin\ocx.exe` lands beside every other
/// trampoline and captures all of their spawns. This line is the Windows half
/// of the absolute `__ocx_binary` the POSIX trampoline body has always baked.
///
/// # No containment, and that is not an omission
///
/// The home is a project **selector**, not a package root, so
/// [`Sidecar::containment_path`] returns `None` for it and the E3 allow-list
/// never sees it. See that method for why applying the allow-list here would
/// refuse every legitimate trampoline. The baked `ocx` is not containment-
/// checked either, and for a stronger reason: it is the program the sidecar
/// exists to name, so a check would only re-ask the question its own writer
/// answered. What bounds it is the same thing that bounds every other byte
/// here — write access to `<home>/toolchain/bin`, which is owner-only at
/// create time.
///
/// Anything rejected is [`ShimError::MalformedSidecar`], exit 78 (E2).
pub(super) fn parse_exec_sidecar(raw: &[u8]) -> Result<Sidecar, ShimError> {
    if raw.len() > MAX_LEN {
        return Err(ShimError::MalformedSidecar {
            reason: format!("sidecar larger than {MAX_LEN} bytes"),
        });
    }

    // One trailing terminator off the FILE, exactly as `parse_one_line` takes
    // one off a line — so `<home>\n<ocx>\n` and `<home>\n<ocx>` are the same
    // document, and a second trailing newline stays an empty line two rather
    // than becoming invisible.
    let body = raw
        .strip_suffix(b"\r\n")
        .or_else(|| raw.strip_suffix(b"\n"))
        .unwrap_or(raw);

    let (home_line, baked_line) = match body.iter().position(|&byte| byte == b'\n') {
        // The `\r` belongs to line one's CRLF terminator; stripping it is what
        // `parse_one_line` would have done had the line arrived alone. Done
        // ONLY on a split, so a lone trailing CR on a one-line sidecar stays
        // the interior byte the shared rules refuse.
        Some(at) => (
            body[..at].strip_suffix(b"\r").unwrap_or(&body[..at]),
            Some(&body[at + 1..]),
        ),
        None => (body, None),
    };

    let home = parse_one_line(home_line)?;
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

    let baked = match baked_line {
        Some(line) => {
            let ocx = parse_one_line(line)?;
            if !is_absolute_path(ocx) {
                return Err(ShimError::MalformedSidecar {
                    reason: format!("baked ocx is not absolute: {ocx}"),
                });
            }
            Some(ocx.to_string())
        }
        None => None,
    };

    Ok(Sidecar::ToolchainHome(home, baked))
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

/// Absolute-path check for the two sidecar payloads that name a directory: a
/// `.shim` package root, and a `.exec` project home (the clause that tells one
/// from [`ToolchainHome::GLOBAL`]). Recognises Windows absolute forms
/// (`C:\...`, `C:/...`, `\\server\share`, `\\?\...`) and a leading `/` so the
/// parser is host-runnable on the Linux CI without depending on
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

/// The program a shim will spawn, and how `CreateProcessW` must receive it.
///
/// One value rather than a token plus a loose boolean, because the two answers
/// are one decision: every rung that produces a real filesystem path must be
/// passed explicitly, and the single rung that produces a bare name must not
/// be. Splitting them let the second half be recomputed at the spawn site,
/// which is `#[cfg(windows)]` and therefore unreachable to any test on the
/// Linux CI host (RUL-39).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ResolvedProgram {
    /// The leading token of the child command line.
    pub(super) token: String,
    /// Whether `token` is also passed as an explicit `lpApplicationName`.
    pub(super) explicit: bool,
}

impl ResolvedProgram {
    /// What `CreateProcessW` receives as `lpApplicationName`: `Some(program)`
    /// to resolve it explicitly, or `None` for the command-line program search.
    ///
    /// SECURITY (B2 / CWE-428): a real filesystem path may contain spaces
    /// (`C:\Program Files\…\ocx.exe`), so it MUST be passed as its own
    /// NUL-terminated buffer — otherwise `CreateProcessW` parses the command
    /// line and mis-resolves it to `C:\Program.exe`.
    ///
    /// SECURITY (V-9): `None` is also the *dangerous* answer, which is why
    /// only [`resolve_program`]'s last rung produces it. With a NULL
    /// `lpApplicationName`, `CreateProcessW` searches **the directory the
    /// calling image loaded from first** — `<home>/toolchain/bin` for every
    /// rendered trampoline — then the working directory, the system
    /// directories, and only then `PATH`. A co-resident `bin\ocx.exe`
    /// therefore wins before `PATH` is consulted at all.
    pub(super) fn application_name(&self) -> Option<&str> {
        self.explicit.then_some(self.token.as_str())
    }
}

/// Resolves the program a shim spawns, over three rungs.
///
/// 1. **`OCX_BINARY_PIN`**, applying the Windows `IF DEFINED` semantics the
///    ADR §Error Taxonomy E5/E6 mandates: defined **at all** (present, even as
///    an empty string) → that value. Empty must NOT collapse to `ocx` — that
///    is the Unix `${VAR:-ocx}` behaviour, deliberately out of scope, and an
///    empty `lpApplicationName` then fails the spawn deterministically rather
///    than silently parsing the command line.
/// 2. **The baked `ocx`** from a `.exec` sidecar's second line (V-9) — the
///    Windows counterpart of the absolute `__ocx_binary` the POSIX trampoline
///    body bakes. Below the pin for the same reason POSIX puts it there:
///    `exec "${OCX_BINARY_PIN:-${__ocx_binary}}"` — the pin is the operator's
///    override and must win on both platforms.
/// 3. **The literal `ocx`**, spawned with `lpApplicationName = NULL`.
///
/// Rung 3 is the accepted narrow risk, not the ordinary case: it is reached
/// only by the two positional grammars (whose trees never hold a rendered
/// `ocx`) and by a one-line `.exec` — written when `trampoline_ocx_binary`'s
/// own ladder could resolve no absolute, existing `ocx` at render time.
/// `launcher/generate.rs` records that acceptance from the POSIX side and it
/// is the same population here.
///
/// `pin` models the env lookup result: `None` = unset, `Some(value)` =
/// defined (value may be empty). `baked` is [`Sidecar::baked_ocx`].
pub(super) fn resolve_program(pin: Option<&str>, baked: Option<&str>) -> ResolvedProgram {
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

/// The wire-ABI vocabulary the shim emits for a **toolchain trampoline**:
/// `<root flag> [<root>] exec -- "<stem>" <argv...>` (C-032).
///
/// The third frozen wire token, and the one that breaks the shape of the other
/// two: the selector is a **root flag** and it comes FIRST, before the verb,
/// because `--project` / `--global` are root-level flags on `ocx` itself rather
/// than arguments of the subcommand. `exec [-- ] <name>` is then the ordinary
/// project-tier verb; the trampoline adds nothing to it.
///
/// Same two-producer problem as its siblings, and the same remedy: `ocx_lib`
/// cannot depend on this binary crate, so the binding to
/// `package_manager::launcher::body::unix_trampoline_body` is a PAIRED GOLDEN —
/// `body.rs`'s `trampoline_wire_tokens_are_bound_to_shim_producer` restates the
/// token, the two root flags and the `global` sidecar literal from the `.sh`
/// side, and [`super::tests::trampoline_wire_tokens_match_the_sh_trampoline_body`]
/// restates them from here (`subsystem-package-manager.md` canary rule).
pub(super) const WIRE_SUBCOMMAND_EXEC: &str = "exec";

/// Assembles the child command line reproducing the frozen wire ABI.
///
/// Two shapes, decided by the parsed [`Sidecar`] and never by inspecting a
/// value:
///
/// - `<program> <verb> "<value>" -- "<stem>" <argv...>` — `launcher exec` with
///   a package root, or `launcher shim` with a pinned identifier.
/// - `<program> --project "<root>" exec -- "<stem>" <argv...>`, and
///   `<program> --global exec -- "<stem>" <argv...>` with **no value token**,
///   for a `.exec` toolchain home (C-032). The root flag precedes the verb
///   because it is a root-level flag on `ocx`, not an argument of `exec`.
///
/// The quotes in both shapes above denote **argument boundaries, not emitted
/// bytes** — the house convention the two shipped grammars already use.
/// [`append_quoted_arg`] quotes only what needs it (empty, or carrying a space,
/// tab, `"` or an ASCII control byte), so a root without a space renders bare:
/// `--project C:\w\proj exec -- cmake`. What is contractual is that every token
/// passes *through* the quoter, not that every token comes back wrapped.
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
/// `program` is *resolved from* the leading command-line token ONLY for
/// [`resolve_program`]'s last rung — the literal `ocx`, spawned with a NULL
/// `lpApplicationName` (see [`ResolvedProgram::application_name`]). A pinned
/// or baked program is passed to `CreateProcessW` via `lpApplicationName` and
/// is NOT parsed from this string (CWE-428). It is still quoted here so the
/// leading token is well-formed in every case — `CreateProcessW` hands the
/// whole command line to the child either way.
pub(super) fn build_child_command_line(program: &str, sidecar: &Sidecar, stem: &str, argv: &[String]) -> String {
    let (subcommand, value) = (sidecar.wire_subcommand(), sidecar.value());
    // Argv-aware capacity estimate: the shim is on every-invocation hot
    // path (one process per launcher call). `len()+3` per arg covers the
    // separating space plus a quote pair in the common quoted case.
    let argv_estimate: usize = argv.iter().map(|a| a.len() + 3).sum();
    let mut line =
        String::with_capacity(program.len() + subcommand.len() + value.len() + stem.len() + 22 + argv_estimate);
    append_quoted_arg(&mut line, program);
    match sidecar {
        // Root-flag-first arm (C-032). The flag belongs to `ocx`, not to
        // `exec`, so it precedes the verb; `--global` emits no value token at
        // all, and a `""` in its place would make `ocx` read the verb as the
        // flag's argument.
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
