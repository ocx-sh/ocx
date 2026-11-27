// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pure shim logic, split from the Win32 calls in [`crate::run`] so the Linux CI host can unit-test it.

use super::ShimError;
use super::direct::{fold, refuse, separators};
use ocx_env_apply::launch::{LaunchRecord, ToolchainTier, is_batch_target, is_fully_qualified, record_path_for};
use std::ffi::{OsStr, OsString};

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

    /// The tokens after the program, through `name`, in the frozen order of the wire ABI; the caller's arguments follow.
    ///
    /// The shape follows the variant, never a value: the home is a root flag's argument for `toolchain` (`--global`
    /// takes none, and a `""` there would make `ocx` read the verb as its argument) and the verb's first positional for
    /// the other two kinds.
    pub(super) fn arguments(&self, name: &str) -> Vec<String> {
        let subcommand = self.wire_subcommand().split(' ').map(str::to_string);
        let mut tokens: Vec<String> = match self {
            HomeLaunch::ToolchainHome(ToolchainHome::Project(root), _) => ["--project".to_string(), root.clone()]
                .into_iter()
                .chain(subcommand)
                .collect(),
            HomeLaunch::ToolchainHome(ToolchainHome::Global, _) => {
                std::iter::once("--global".to_string()).chain(subcommand).collect()
            }
            HomeLaunch::PackageRoot(value) | HomeLaunch::PinnedIdentifier(value) => {
                subcommand.chain([value.clone()]).collect()
            }
        };
        tokens.extend(["--".to_string(), name.to_string()]);
        tokens
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

/// The environment variable that counts home-launcher hops; the registry row is `__OCX_LAUNCHER_DEPTH` in `ocx_env`,
/// which this crate cannot link without breaking its size budget.
pub(super) const LAUNCHER_DEPTH_VAR: &str = "__OCX_LAUNCHER_DEPTH";

/// A home launcher that reads this depth or more exits 78 before it starts anything.
pub(super) const MAX_LAUNCHER_DEPTH: u32 = 32;

/// The value of `name` in `environment`; ASCII case folds on Windows, where variable names do.
pub(super) fn environment_value<'a>(
    environment: &'a [(OsString, OsString)],
    name: &str,
    windows: bool,
) -> Option<&'a OsStr> {
    environment
        .iter()
        .find(|(key, _)| {
            if windows {
                key.eq_ignore_ascii_case(name)
            } else {
                key == name
            }
        })
        .map(|(_, value)| value.as_os_str())
}

/// The child environment of a home launch: `inherited`, minus the tier selectors when `home` bakes the only one, and
/// with [`LAUNCHER_DEPTH_VAR`] one higher.
///
/// A depth that is absent or not a number counts as 0; digits past the `u32` range count as the limit. A depth at
/// [`MAX_LAUNCHER_DEPTH`] or more is a loop: two launchers whose resolved programs start each other would otherwise
/// run until the machine gives out.
pub(super) fn home_environment(
    inherited: Vec<(OsString, OsString)>,
    home: &HomeLaunch,
    windows: bool,
) -> Result<Vec<(OsString, OsString)>, ShimError> {
    let depth = environment_value(&inherited, LAUNCHER_DEPTH_VAR, windows)
        .and_then(OsStr::to_str)
        .map_or(0, |text| match text.parse::<u32>() {
            Ok(depth) => depth,
            Err(error) if *error.kind() == std::num::IntErrorKind::PosOverflow => MAX_LAUNCHER_DEPTH,
            Err(_) => 0,
        });
    if depth >= MAX_LAUNCHER_DEPTH {
        return Err(refuse(format!(
            "launcher loop: {LAUNCHER_DEPTH_VAR} is {depth}, the limit is {MAX_LAUNCHER_DEPTH}; two launchers start each other"
        )));
    }
    let strips = strips_tier_selectors(home);
    let dropped = |key: &OsStr| {
        let is = |name: &str| {
            if windows {
                key.eq_ignore_ascii_case(name)
            } else {
                key == name
            }
        };
        is(LAUNCHER_DEPTH_VAR) || (strips && (is("OCX_GLOBAL") || is("OCX_PROJECT")))
    };
    let mut environment: Vec<(OsString, OsString)> = inherited.into_iter().filter(|(key, _)| !dropped(key)).collect();
    environment.push((
        OsString::from(LAUNCHER_DEPTH_VAR),
        OsString::from((depth + 1).to_string()),
    ));
    Ok(environment)
}

/// The filesystem questions program resolution asks.
pub(super) trait ProgramFs {
    /// Whether `path` is a regular file the OS can start: executable on Unix, any regular file on Windows.
    fn is_program_file(&self, path: &str) -> bool;
    /// Whether `path` is a regular file.
    fn is_regular_file(&self, path: &str) -> bool;
}

/// What resolving `ocx` reads: the environment, the record and where the launcher lives.
pub(super) struct ProgramInputs<'a> {
    /// `OCX_BINARY_PIN`; empty counts as unset.
    pub pin: Option<&'a str>,
    /// The `toolchain` record's `ocx`.
    pub baked: Option<&'a str>,
    /// The launcher's own `PATH`.
    pub path: Option<&'a str>,
    /// The directory the launcher runs from; the `PATH` walk skips it.
    pub launcher_directory: &'a str,
    pub windows: bool,
}

/// Resolves the absolute `ocx` a home launch starts: a non-empty pin, then the record's `ocx`, then the launcher's own
/// `PATH` walk. Never the launcher's directory, the current directory or an OS search (CWE-427, CWE-428).
///
/// A relative pin or baked program, and a batch file on any rung, is a 78: `cmd.exe` would re-parse its arguments.
pub(super) fn resolve_program(inputs: &ProgramInputs<'_>, fs: &dyn ProgramFs) -> Result<String, ShimError> {
    let explicit = inputs
        .pin
        .filter(|pin| !pin.is_empty())
        .map(|pin| ("OCX_BINARY_PIN", pin))
        .or(inputs.baked.map(|baked| ("the record's ocx", baked)));
    let program = match explicit {
        Some((source, program)) => {
            if !is_fully_qualified(program, inputs.windows) {
                return Err(refuse(format!(
                    "{source} must be an absolute path: {}",
                    super::escape_controls(program)
                )));
            }
            program.to_string()
        }
        None => find_on_path(inputs, fs)?,
    };
    if is_batch_target(&program) {
        return Err(refuse(format!(
            "ocx resolved to the batch file {}, which the launcher does not start",
            super::escape_controls(&program)
        )));
    }
    Ok(program)
}

/// `directory` for a lexical comparison: no trailing separator, and on Windows ASCII case and `\` folded.
fn comparable_directory(directory: &str, windows: bool) -> String {
    fold(directory.trim_end_matches(separators(windows)), windows)
}

/// The first `ocx` on `inputs.path` that is not the launcher's own directory and not another launcher.
///
/// A candidate with a sibling `ocx.launch` is a launcher: starting it would loop back here, so it is skipped and the
/// walk continues to the real binary.
fn find_on_path(inputs: &ProgramInputs<'_>, fs: &dyn ProgramFs) -> Result<String, ShimError> {
    let windows = inputs.windows;
    let own = comparable_directory(inputs.launcher_directory, windows);
    let (separator, file_name) = if windows { (';', "ocx.exe") } else { (':', "ocx") };
    for entry in inputs.path.unwrap_or_default().split(separator) {
        // A quoted Windows entry may hold a `;`, which the split above cannot see; it is a miss, never a wrong hit.
        let entry = if windows { entry.trim_matches('"') } else { entry };
        if entry.is_empty() || !is_fully_qualified(entry, windows) || comparable_directory(entry, windows) == own {
            continue;
        }
        let candidate = format!(
            "{}{}{file_name}",
            entry.trim_end_matches(separators(windows)),
            if windows { '\\' } else { '/' }
        );
        // An entry of separators alone trims to nothing and would name the current drive's root.
        if is_fully_qualified(&candidate, windows)
            && fs.is_program_file(&candidate)
            && !fs.is_regular_file(&record_path_for(&candidate, windows))
        {
            return Ok(candidate);
        }
    }
    Err(refuse("no ocx found on PATH".to_string()))
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

/// Assembles the child command line of the frozen wire ABI: `program`, [`HomeLaunch::arguments`], then `argv`.
///
/// Every token passes through [`append_quoted_arg`], which quotes only what needs it.
pub(super) fn build_child_command_line(program: &str, launch: &HomeLaunch, stem: &str, argv: &[String]) -> String {
    // Every token goes through the `CommandLineToArgvW` quoter, never a hand-written `"…"`: the record is no trust
    // boundary, and a trailing `\` before a hand-written quote escapes it and collapses the argv boundary (CWE-88).
    let mut line = String::new();
    append_quoted_arg(&mut line, program);
    for token in launch.arguments(stem).iter().chain(argv) {
        line.push(' ');
        append_quoted_arg(&mut line, token);
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

#[cfg(test)]
mod tests {
    use super::{
        HomeLaunch, LAUNCHER_DEPTH_VAR, ProgramFs, ProgramInputs, ToolchainHome, home_environment, resolve_program,
    };
    use crate::ShimError;
    use std::collections::BTreeSet;
    use std::ffi::OsString;

    /// A filesystem of named programs and regular files; a program is also a regular file.
    #[derive(Default)]
    struct FakeFs {
        programs: BTreeSet<String>,
        files: BTreeSet<String>,
    }

    impl FakeFs {
        fn with_program(mut self, path: &str) -> Self {
            self.programs.insert(path.to_string());
            self
        }

        fn with_file(mut self, path: &str) -> Self {
            self.files.insert(path.to_string());
            self
        }
    }

    impl ProgramFs for FakeFs {
        fn is_program_file(&self, path: &str) -> bool {
            self.programs.contains(path)
        }

        fn is_regular_file(&self, path: &str) -> bool {
            self.files.contains(path) || self.programs.contains(path)
        }
    }

    fn unix<'a>(pin: Option<&'a str>, baked: Option<&'a str>, path: Option<&'a str>) -> ProgramInputs<'a> {
        ProgramInputs {
            pin,
            baked,
            path,
            launcher_directory: "/launcher/bin/",
            windows: false,
        }
    }

    fn windows<'a>(pin: Option<&'a str>, baked: Option<&'a str>, path: Option<&'a str>) -> ProgramInputs<'a> {
        ProgramInputs {
            pin,
            baked,
            path,
            launcher_directory: "C:\\Launcher\\bin\\",
            windows: true,
        }
    }

    /// The line a refusal prints, asserting it is a 78.
    fn refusal(result: Result<String, ShimError>) -> String {
        let error = result.expect_err("the resolution is refused");
        assert_eq!(error.exit_code(), 78, "a refused program is a configuration error");
        error.stderr_message()
    }

    fn package() -> HomeLaunch {
        HomeLaunch::PackageRoot("/o/packages/p".to_string())
    }

    fn deferred() -> HomeLaunch {
        HomeLaunch::PinnedIdentifier("ocx.sh/t:1@sha256:00".to_string())
    }

    fn project(ocx: Option<&str>) -> HomeLaunch {
        HomeLaunch::ToolchainHome(ToolchainHome::Project("/w/proj".to_string()), ocx.map(str::to_string))
    }

    fn global() -> HomeLaunch {
        HomeLaunch::ToolchainHome(ToolchainHome::Global, None)
    }

    fn environment(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect()
    }

    fn value_of(environment: &[(OsString, OsString)], key: &str) -> Option<String> {
        super::environment_value(environment, key, false).map(|value| value.to_string_lossy().into_owned())
    }

    // ── an empty pin is unset ─────────────────────────────────────────

    #[test]
    fn vl06_an_empty_pin_resolves_to_the_next_rung() {
        let fs = FakeFs::default().with_program("/usr/bin/ocx");
        assert_eq!(
            resolve_program(&unix(Some(""), Some("/baked/ocx"), None), &fs)
                .as_deref()
                .ok(),
            Some("/baked/ocx"),
            "an empty pin falls to the record's ocx, not to an empty program"
        );
        assert_eq!(
            resolve_program(&unix(Some(""), None, Some("/usr/bin")), &fs)
                .as_deref()
                .ok(),
            Some("/usr/bin/ocx"),
            "with no record ocx either, the PATH walk runs"
        );
    }

    #[test]
    fn vl06_control_a_non_empty_pin_outranks_the_record_and_the_path() {
        let fs = FakeFs::default().with_program("/usr/bin/ocx");
        assert_eq!(
            resolve_program(&unix(Some("/pin/ocx"), Some("/baked/ocx"), Some("/usr/bin")), &fs)
                .as_deref()
                .ok(),
            Some("/pin/ocx")
        );
        assert_eq!(
            resolve_program(&unix(None, Some("/baked/ocx"), Some("/usr/bin")), &fs)
                .as_deref()
                .ok(),
            Some("/baked/ocx"),
            "the record's ocx outranks the PATH walk"
        );
    }

    // ── the frozen argv, the selector strip and the depth ─────────────

    #[test]
    fn vl07_every_home_kind_builds_the_frozen_token_sequence() {
        let tokens = |home: HomeLaunch| home.arguments("cmake");
        assert_eq!(
            tokens(package()),
            ["launcher", "exec", "/o/packages/p", "--", "cmake"],
            "package: `launcher exec <package_root> -- NAME`"
        );
        assert_eq!(
            tokens(deferred()),
            ["launcher", "shim", "ocx.sh/t:1@sha256:00", "--", "cmake"],
            "deferred: `launcher shim <identifier> -- NAME`"
        );
        assert_eq!(
            tokens(project(None)),
            ["--project", "/w/proj", "exec", "--", "cmake"],
            "toolchain, project tier: the root flag comes before the verb"
        );
        assert_eq!(
            tokens(global()),
            ["--global", "exec", "--", "cmake"],
            "toolchain, global tier: the flag takes no value token"
        );
    }

    #[test]
    fn vl07_only_a_toolchain_launch_removes_the_tier_selectors() {
        let inherited = || environment(&[("OCX_GLOBAL", "1"), ("OCX_PROJECT", "/p"), ("KEEP", "yes")]);
        for home in [project(None), global()] {
            let child = home_environment(inherited(), &home, false).expect("depth 0 starts");
            assert_eq!(value_of(&child, "OCX_GLOBAL"), None, "{home:?}");
            assert_eq!(value_of(&child, "OCX_PROJECT"), None, "{home:?}");
            assert_eq!(value_of(&child, "KEEP").as_deref(), Some("yes"), "{home:?}");
        }
        for home in [package(), deferred()] {
            let child = home_environment(inherited(), &home, false).expect("depth 0 starts");
            assert_eq!(value_of(&child, "OCX_GLOBAL").as_deref(), Some("1"), "{home:?}");
            assert_eq!(value_of(&child, "OCX_PROJECT").as_deref(), Some("/p"), "{home:?}");
        }
    }

    #[test]
    fn vl07_the_selector_strip_folds_case_on_windows_only() {
        let inherited = || environment(&[("ocx_global", "1"), ("Ocx_Project", "/p")]);
        let windows = home_environment(inherited(), &project(None), true).expect("depth 0 starts");
        assert_eq!(windows.len(), 1, "only the depth entry is left: {windows:?}");
        let unix = home_environment(inherited(), &project(None), false).expect("depth 0 starts");
        assert_eq!(
            unix.len(),
            3,
            "a differently cased key is another variable on Unix: {unix:?}"
        );
    }

    #[test]
    fn vl07_every_home_kind_raises_the_depth_by_one() {
        for home in [package(), deferred(), project(None), global()] {
            let absent = home_environment(environment(&[]), &home, false).expect("starts");
            assert_eq!(value_of(&absent, LAUNCHER_DEPTH_VAR).as_deref(), Some("1"), "{home:?}");
            let counted = home_environment(environment(&[(LAUNCHER_DEPTH_VAR, "5")]), &home, false).expect("starts");
            assert_eq!(value_of(&counted, LAUNCHER_DEPTH_VAR).as_deref(), Some("6"), "{home:?}");
            assert_eq!(
                counted.iter().filter(|(key, _)| key == LAUNCHER_DEPTH_VAR).count(),
                1,
                "the old depth entry is replaced, never repeated: {counted:?}"
            );
        }
    }

    #[test]
    fn vl07_an_unparsable_depth_counts_as_zero() {
        for text in ["", "x", "-1", "1.5"] {
            let child =
                home_environment(environment(&[(LAUNCHER_DEPTH_VAR, text)]), &package(), false).expect("starts");
            assert_eq!(
                value_of(&child, LAUNCHER_DEPTH_VAR).as_deref(),
                Some("1"),
                "depth {text:?} counts as 0"
            );
        }
    }

    // ── the PATH walk ─────────────────────────────────────────────────

    #[test]
    fn vl10_the_walk_skips_empty_relative_own_and_other_launcher_entries() {
        let fs = FakeFs::default()
            .with_program("/launcher/bin/ocx")
            .with_program("rel/bin/ocx")
            .with_program("/other/ocx")
            .with_file("/other/ocx.launch")
            .with_program("/real/bin/ocx");
        let path = ":rel/bin::/launcher/bin:/other:/real/bin";
        assert_eq!(
            resolve_program(&unix(None, None, Some(path)), &fs).as_deref().ok(),
            Some("/real/bin/ocx"),
            "empty, relative, the launcher's own directory and a directory whose ocx is another launcher are skipped"
        );
    }

    #[test]
    fn vl10_a_path_with_the_launchers_directory_first_still_finds_the_real_ocx() {
        let fs = FakeFs::default()
            .with_program("/launcher/bin/ocx")
            .with_program("/real/bin/ocx");
        for own_entry in ["/launcher/bin", "/launcher/bin/"] {
            let path = format!("{own_entry}:/real/bin");
            assert_eq!(
                resolve_program(&unix(None, None, Some(&path)), &fs).as_deref().ok(),
                Some("/real/bin/ocx"),
                "own directory spelled {own_entry:?}"
            );
        }
    }

    #[test]
    fn vl10_a_candidate_that_is_not_an_executable_file_is_skipped() {
        let fs = FakeFs::default().with_file("/dead/ocx").with_program("/real/ocx");
        assert_eq!(
            resolve_program(&unix(None, None, Some("/dead:/real")), &fs)
                .as_deref()
                .ok(),
            Some("/real/ocx")
        );
    }

    #[test]
    fn vl10_no_ocx_on_the_path_is_a_78_naming_the_path() {
        let fs = FakeFs::default().with_program("/launcher/bin/ocx");
        for path in [None, Some(""), Some("/launcher/bin"), Some("rel")] {
            let line = refusal(resolve_program(&unix(None, None, path), &fs));
            assert!(line.contains("no ocx found on PATH"), "{path:?}: {line}");
        }
    }

    #[test]
    fn vl10_windows_walks_for_ocx_exe_and_folds_the_own_directory() {
        let fs = FakeFs::default()
            .with_program("C:\\Launcher\\bin\\ocx.exe")
            .with_program("D:\\real\\ocx.exe");
        let path = "c:/launcher/BIN;rel;;\"D:\\real\\\"";
        assert_eq!(
            resolve_program(&windows(None, None, Some(path)), &fs).as_deref().ok(),
            Some("D:\\real\\ocx.exe"),
            "the own directory matches across case and separators, the entry's quotes and trailing separator go"
        );
        let no_exe = FakeFs::default().with_program("D:\\real\\ocx");
        let line = refusal(resolve_program(&windows(None, None, Some("D:\\real")), &no_exe));
        assert!(
            line.contains("no ocx found on PATH"),
            "a Windows walk looks for ocx.exe only: {line}"
        );
    }

    // ── relative and batch programs are 78 on every rung ──────────────

    #[test]
    fn vl12_a_relative_pin_or_record_program_is_refused() {
        let fs = FakeFs::default();
        for (platform, relative) in [
            (false, "bin/ocx"),
            (false, "./ocx"),
            (true, "bin\\ocx.exe"),
            (true, "C:ocx.exe"),
        ] {
            let make = |pin, baked| {
                if platform {
                    windows(pin, baked, None)
                } else {
                    unix(pin, baked, None)
                }
            };
            let line = refusal(resolve_program(&make(Some(relative), None), &fs));
            assert!(
                line.contains("OCX_BINARY_PIN must be an absolute path"),
                "{relative}: {line}"
            );
            let line = refusal(resolve_program(&make(None, Some(relative)), &fs));
            assert!(line.contains("must be an absolute path"), "{relative}: {line}");
        }
    }

    #[test]
    fn vl12_a_batch_program_is_refused_on_every_rung() {
        let fs = FakeFs::default();
        for batch in ["C:\\t\\ocx.cmd", "C:\\t\\ocx.BAT", "C:\\t\\ocx.cmd. "] {
            let pin = refusal(resolve_program(&windows(Some(batch), None, None), &fs));
            assert!(pin.contains("batch file"), "pin {batch}: {pin}");
            let baked = refusal(resolve_program(&windows(None, Some(batch), None), &fs));
            assert!(baked.contains("batch file"), "baked {batch}: {baked}");
        }
        let both = refusal(resolve_program(
            &windows(Some("C:\\t\\ocx.cmd"), Some("C:\\t\\ocx.exe"), None),
            &fs,
        ));
        assert!(
            both.contains("batch file"),
            "an empty or ignored lower rung does not rescue a batch pin: {both}"
        );
    }

    /// A Windows path with a leading separator and no drive names a file on the current drive, which follows the
    /// working directory (CWE-427).
    #[test]
    fn a_windows_program_on_the_current_drive_is_refused_on_every_rung() {
        let fs = FakeFs::default()
            .with_program("/tools\\ocx.exe")
            .with_program("\\ocx.exe")
            .with_program("D:\\real\\ocx.exe");
        for driveless in ["/tools/ocx.exe", "\\tools\\ocx.exe"] {
            let pin = refusal(resolve_program(&windows(Some(driveless), None, None), &fs));
            assert!(pin.contains("must be an absolute path"), "pin {driveless}: {pin}");
            let baked = refusal(resolve_program(&windows(None, Some(driveless), None), &fs));
            assert!(baked.contains("must be an absolute path"), "baked {driveless}: {baked}");
        }
        assert_eq!(
            resolve_program(&windows(None, None, Some("/tools;\\\\;/;D:\\real")), &fs)
                .as_deref()
                .ok(),
            Some("D:\\real\\ocx.exe"),
            "a PATH entry that names the current drive is skipped"
        );
    }

    #[test]
    fn control_a_windows_drive_or_unc_program_is_accepted() {
        let fs = FakeFs::default().with_program("\\\\server\\share\\ocx.exe");
        for program in [
            "C:\\t\\ocx.exe",
            "C:/t/ocx.exe",
            "\\\\server\\share\\ocx.exe",
            "\\\\?\\C:\\t\\ocx.exe",
        ] {
            assert_eq!(
                resolve_program(&windows(Some(program), None, None), &fs)
                    .as_deref()
                    .ok(),
                Some(program)
            );
        }
        assert_eq!(
            resolve_program(&windows(None, None, Some("\\\\server\\share")), &fs)
                .as_deref()
                .ok(),
            Some("\\\\server\\share\\ocx.exe")
        );
    }

    #[test]
    fn vl12_control_an_absolute_exe_pin_is_accepted_without_checking_it_exists() {
        let fs = FakeFs::default();
        assert_eq!(
            resolve_program(&windows(Some("C:\\Program Files\\ocx\\ocx.exe"), None, None), &fs)
                .as_deref()
                .ok(),
            Some("C:\\Program Files\\ocx\\ocx.exe"),
            "a missing pinned program is the spawn's 69, not a resolution refusal"
        );
    }

    // ── the loop guard ────────────────────────────────────────────────

    #[test]
    fn vl14_a_depth_of_32_or_more_is_a_78_launcher_loop() {
        for depth in ["32", "33", "4000000000"] {
            for home in [package(), deferred(), project(None), global()] {
                let error = home_environment(environment(&[(LAUNCHER_DEPTH_VAR, depth)]), &home, false)
                    .expect_err("a loop stops before anything starts");
                assert_eq!(error.exit_code(), 78, "depth {depth}, {home:?}");
                assert!(
                    error.stderr_message().contains("launcher loop"),
                    "{}",
                    error.stderr_message()
                );
            }
        }
    }

    /// A count too large for the counter is still 32 or more.
    #[test]
    fn vl14_a_depth_past_the_counter_range_is_a_launcher_loop() {
        for depth in ["4294967296", "99999999999999999999"] {
            let error = home_environment(environment(&[(LAUNCHER_DEPTH_VAR, depth)]), &package(), false)
                .expect_err("a huge depth is a loop");
            assert!(error.stderr_message().contains("launcher loop"), "{depth}");
        }
    }

    #[test]
    fn vl14_control_depth_31_still_starts_and_hands_the_child_32() {
        let child =
            home_environment(environment(&[(LAUNCHER_DEPTH_VAR, "31")]), &package(), false).expect("depth 31 starts");
        assert_eq!(value_of(&child, LAUNCHER_DEPTH_VAR).as_deref(), Some("32"));
    }
}
