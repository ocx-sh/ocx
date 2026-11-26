// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pure shim logic, split from the Win32 calls in [`crate::run`] so the Linux CI host can unit-test it.

use super::ShimError;
use ocx_env_apply::launch::{LaunchRecord, ToolchainTier};
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

/// What a home-kind record starts; the variant decides the wire verb, so no payload can be emitted under another
/// kind's verb.
#[derive(Debug)]
pub(super) enum HomeLaunch {
    /// Kind `package`: an installed package's absolute root. Dispatches [`WIRE_SUBCOMMAND`].
    PackageRoot(String),
    /// Kind `deferred`: a deferred tool's pinned identifier. Dispatches [`WIRE_SUBCOMMAND_SHIM`], which materializes
    /// the package first.
    PinnedIdentifier(String),
    /// Kind `toolchain`: a toolchain home plus the baked absolute `ocx` (`None` when the renderer could not resolve
    /// its own path). Dispatches [`WIRE_SUBCOMMAND_EXEC`] behind a root flag.
    ToolchainHome(ToolchainHome, Option<String>),
}

/// The home a `toolchain` record names; typed because `--global` takes no value token.
#[derive(Debug)]
pub(super) enum ToolchainHome {
    /// An absolute project root, emitted as `--project "<root>"`.
    Project(String),
    /// The global home, emitted as a valueless `--global`.
    Global,
}

impl HomeLaunch {
    /// The home launch a decoded record names; `None` for kind `direct`, which has its own lookup, and for a
    /// toolchain tier and `project_root` that disagree (decoding refuses that already).
    pub(super) fn from_record(record: LaunchRecord) -> Option<Self> {
        match record {
            LaunchRecord::Direct(_) => None,
            LaunchRecord::Package(record) => Some(Self::PackageRoot(record.package_root)),
            LaunchRecord::Deferred(record) => Some(Self::PinnedIdentifier(record.identifier)),
            LaunchRecord::Toolchain(record) => {
                let home = match (record.tier, record.project_root) {
                    (ToolchainTier::Project, Some(root)) => ToolchainHome::Project(root),
                    (ToolchainTier::Global, None) => ToolchainHome::Global,
                    _ => return None,
                };
                Some(Self::ToolchainHome(home, record.ocx))
            }
        }
    }

    /// The wire subcommand this record's value is passed to.
    pub(super) fn wire_subcommand(&self) -> &'static str {
        match self {
            HomeLaunch::PackageRoot(_) => WIRE_SUBCOMMAND,
            HomeLaunch::PinnedIdentifier(_) => WIRE_SUBCOMMAND_SHIM,
            HomeLaunch::ToolchainHome(..) => WIRE_SUBCOMMAND_EXEC,
        }
    }

    /// The absolute `ocx` a `toolchain` record baked; `None` otherwise, and the caller falls back to the literal `ocx`.
    pub(super) fn baked_ocx(&self) -> Option<&str> {
        match self {
            HomeLaunch::ToolchainHome(_, ocx) => ocx.as_deref(),
            HomeLaunch::PackageRoot(_) | HomeLaunch::PinnedIdentifier(_) => None,
        }
    }

    /// The value the record carries; for `toolchain`, the project root, which [`build_child_command_line`] places
    /// before the verb, and empty for the global home, which has none.
    pub(super) fn value(&self) -> &str {
        match self {
            HomeLaunch::PackageRoot(value) | HomeLaunch::PinnedIdentifier(value) => value,
            HomeLaunch::ToolchainHome(ToolchainHome::Project(root), _) => root,
            HomeLaunch::ToolchainHome(ToolchainHome::Global, _) => "",
        }
    }

    /// The path subject to the containment check, or `None` when the record names no package root.
    ///
    /// Kind `deferred` gets no containment check: it names a registry identifier, whose fetch is digest-verified instead.
    pub(super) fn containment_path(&self) -> Option<&str> {
        match self {
            HomeLaunch::PackageRoot(root) => Some(root),
            // `toolchain` names a project directory, which `pkg_root_allowed`'s roots exclude, so a check would refuse
            // every trampoline.
            HomeLaunch::PinnedIdentifier(_) | HomeLaunch::ToolchainHome(..) => None,
        }
    }
}

/// Whether dispatching `home` must clear `OCX_GLOBAL` and `OCX_PROJECT` from the child's environment.
///
/// Only kind `toolchain`: its baked selector is the only selector, so an exported `OCX_GLOBAL` would make every
/// trampoline exit 64 from `check_global_project_exclusivity`. Stripping for the positional grammars would change what
/// `launcher exec` / `launcher shim` resolve for a caller who deliberately exported a tier.
pub(super) fn strips_tier_selectors(home: &HomeLaunch) -> bool {
    matches!(home, HomeLaunch::ToolchainHome(..))
}

/// Whether a canonicalized package root lies inside the containment roots, given the canonicalized `OCX_HOME`.
///
/// Must mirror `validate_launcher_pkg_root`'s allow-list in `ocx_cli`: `packages`, `temp/test`, `temp/patch-test`.
/// Keep the scratch roots enumerated: admitting all of `temp/` lets a tampered record aim at mid-download content.
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

/// Resolves the spawned program: `OCX_BINARY_PIN`, then the `toolchain` record's baked `ocx` ([`HomeLaunch::baked_ocx`]), then the
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

/// Assembles the child command line of the frozen wire ABI; the shape follows the [`HomeLaunch`] variant, never a value.
///
/// Every token passes through [`append_quoted_arg`], which quotes only what needs it.
pub(super) fn build_child_command_line(program: &str, launch: &HomeLaunch, stem: &str, argv: &[String]) -> String {
    // Every token goes through the `CommandLineToArgvW` quoter, never a hand-written `"…"`: the record is no trust
    // boundary, and a trailing `\` before a hand-written quote escapes it and collapses the argv boundary (CWE-88).
    let (subcommand, value) = (launch.wire_subcommand(), launch.value());
    let argv_estimate: usize = argv.iter().map(|a| a.len() + 3).sum();
    let mut line =
        String::with_capacity(program.len() + subcommand.len() + value.len() + stem.len() + 22 + argv_estimate);
    append_quoted_arg(&mut line, program);
    match launch {
        // `--global` emits no value token; a `""` there would make `ocx` read the verb as the flag's argument.
        HomeLaunch::ToolchainHome(home, _) => {
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
        HomeLaunch::PackageRoot(_) | HomeLaunch::PinnedIdentifier(_) => {
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
pub(super) fn append_quoted_arg(line: &mut String, arg: &str) {
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
