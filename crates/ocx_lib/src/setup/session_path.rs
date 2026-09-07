// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Session-level PATH registration for `ocx self setup`, per platform.
//!
//! `ocx self setup` puts two directories on the PATH of every process started
//! afterwards: the ocx install `bin` directory and `$OCX_HOME/toolchain/bin`,
//! in that order. Only the global tier is registered — a project's
//! `.ocx/toolchain/bin` never reaches a session PATH by ocx's own hand.
//!
//! Three properties shape every writer here, and each is a contract rather
//! than an implementation detail:
//!
//! 1. **A write failure is an outcome, never an error.** A read-only home, a
//!    foreign-owned plist, a registry key another process holds — each yields
//!    [`SessionPathOutcome::Failed`], a warning advising a re-run of
//!    `ocx self setup`, and exit 0. Registering a session PATH is a
//!    convenience; failing the whole install over it is not.
//! 2. **What cannot be encoded is refused before anything is written.** Each
//!    of the three formats has characters it cannot represent and does not
//!    escape, and `$OCX_HOME` is user-chosen. A refusal is a
//!    [`SessionPathError`] naming the directory and the format, and exits 78.
//!    That is deliberately the *opposite* rule from (1): a failed write leaves
//!    the machine as it was, whereas a silently mangled value leaves it with a
//!    wrong PATH nobody can diagnose.
//! 3. **Removal is subtractive on every platform.** [`deregister_session_path`]
//!    removes exactly the segments its sibling added and is a no-op when
//!    neither is present. It never clears, resets or unsets a whole variable —
//!    a foreign segment that was on PATH before `ocx self setup` ran is still
//!    there afterwards.
//!
//! # Host split
//!
//! The three platform modules are compiled on **every** host. Only the
//! functions that touch the registry, the filesystem or `launchctl` carry a
//! `cfg`; every refusal predicate and every renderer is pure and host
//! independent, so the rule each one encodes has a reachable red state on any
//! CI leg rather than only on its own platform.
//!
//! The two exceptions are named at their definitions: the Windows merge and
//! subtraction delegate to [`crate::utility::path`], whose separator and
//! case-folding are keyed on `cfg!(windows)` at compile time, so those two are
//! `cfg(windows)` and are exercised by the Windows test leg
//! (`.github/workflows/verify-deep.yml`, `.github/workflows/verify-basic.yml`).

use std::fmt;
use std::path::{Path, PathBuf};

use crate::setup::profiles::HomeEnv;

pub mod linux;
pub mod macos;
pub mod windows;

/// What one session-PATH store did during a register or deregister run.
///
/// One value per **store** — the location a platform owns
/// (`HKCU\Environment\Path`, `~/.config/environment.d/ocx.conf`,
/// `~/Library/LaunchAgents/sh.ocx.path.plist`) — not one per directory. The
/// directories are written together or not at all, so a per-directory outcome
/// could never disagree with its sibling, while a per-store one is what the
/// run summary and the manual-removal recipe are both keyed by. Same shape as
/// the shipped [`crate::setup::ProfileOutcome`], which is also per file.
///
/// # `--dry-run`
///
/// A dry run reports **the outcome the write would have produced**:
/// [`Self::Written`] when the stored value would change, [`Self::Unchanged`]
/// when it would not, [`Self::Removed`] when a deregistration would take
/// something away, [`Self::SkippedOptOut`] when C-043 suppressed the arm, and
/// [`Self::SkippedUnsupported`] on a host with no facility.
///
/// It **never** reports [`Self::Failed`], because nothing was attempted — a
/// dry run that predicted a failure would be predicting an I/O error it never
/// issued. There is deliberately no `Would*` variant either: the enum answers
/// "what is the state of this store", and `--dry-run`'s own distinguishing
/// evidence is that not one byte moved, which the caller reports and the tests
/// assert directly rather than through a second spelling of every value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPathOutcome {
    /// The store was written: it gained the directories, or its existing value
    /// was rewritten so they lead it.
    Written,
    /// The store already carried the directories in the required order;
    /// nothing was written. Re-running `ocx self setup` lands here.
    Unchanged,
    /// The store carried the directories and they were subtracted from it.
    /// A store that never carried them reports [`Self::Unchanged`] instead —
    /// deregistration is a no-op when neither segment is present.
    Removed,
    /// `--no-modify-path`, or a truthy `OCX_NO_MODIFY_PATH`, suppressed the
    /// whole session-PATH arm. Reported per store so the run summary can name
    /// what was *not* touched; the writers are never called.
    SkippedOptOut,
    /// This host has no session-PATH facility ocx writes. Reported for a
    /// target that is neither Windows, Linux nor macOS.
    SkippedUnsupported,
    /// The store could not be written. Warn, advise re-running
    /// `ocx self setup`, exit 0 — never an error.
    Failed,
}

/// The three session-PATH wire formats, named in an encoding refusal.
///
/// Carried by [`SessionPathError`] rather than inferred from the store path,
/// because the diagnostic's whole job is to tell a user *which grammar* their
/// `$OCX_HOME` cannot be spelled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPathFormat {
    /// `HKCU\Environment\Path`, written as `REG_EXPAND_SZ`: a `;`-delimited
    /// list in which `%…%` pairs expand at read time and have no escape.
    WindowsRegistry,
    /// `~/.config/environment.d/ocx.conf`: line-structured `KEY=VALUE`, with
    /// `${FOO}` / `${FOO:-x}` expansion, backslash escapes, and leading-
    /// whitespace stripping all applied at session-manager read time.
    EnvironmentD,
    /// `~/Library/LaunchAgents/sh.ocx.path.plist`: XML 1.0 carrying a
    /// single-quoted `/bin/sh -c` merge script.
    LaunchAgentPlist,
}

impl fmt::Display for SessionPathFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WindowsRegistry => write!(f, "the Windows user PATH registry value (REG_EXPAND_SZ)"),
            Self::EnvironmentD => write!(f, "an environment.d configuration file"),
            Self::LaunchAgentPlist => write!(f, "a launchd LaunchAgent property list"),
        }
    }
}

/// A directory that cannot be encoded for a session-PATH format.
///
/// **This type is only ever the C-037 pre-write refusal.** No I/O error may
/// travel this arm: from the first byte written onward every `io::Error` is
/// mapped to [`SessionPathOutcome::Failed`] at the platform-writer boundary and
/// carried in the `Ok`. There is deliberately no `From<io::Error>` impl, so a
/// stray `?` on a write cannot reach here even by accident — the never-block
/// rule is enforced by the type, not by review.
///
/// Raised **before** anything is written, so a refused run leaves the machine
/// byte-identical. Classified as [`ExitCode::ConfigError`](crate::cli::ExitCode::ConfigError)
/// (78): `$OCX_HOME` is a configured location, and the remedy is to choose a
/// different one. It reaches `argv` through
/// [`crate::setup::error::Error::SessionPath`], whose `classify` arm delegates
/// straight back here.
///
/// Every message interpolates the offending path with `{path:?}`, never
/// `Path::display`. A refusal case *is* a path that may carry a newline, so
/// rendering it raw would forge log lines (CWE-117) at exactly the moment the
/// value is known hostile.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum SessionPathError {
    /// The directory carries a character the format cannot represent and does
    /// not escape.
    #[error("{path:?} cannot be registered in {format}: it contains {character:?}, {reason}")]
    Unencodable {
        /// The directory that was to be registered.
        path: PathBuf,
        /// The format that cannot represent it.
        format: SessionPathFormat,
        /// The first offending character, in the order the refusal set is
        /// checked. One character is named rather than all of them because the
        /// remedy — move `$OCX_HOME` — is the same for every one of them.
        character: char,
        /// Why that character is unrepresentable in this format, as a phrase
        /// completing the message (e.g. `"and REG_EXPAND_SZ has no escape for
        /// it"`).
        reason: &'static str,
    },

    /// The directory is not valid UTF-8.
    ///
    /// All three formats are text. `to_string_lossy` would substitute U+FFFD
    /// and register a directory that does not exist, which fails later as a
    /// bare "command not found" naming nothing — strictly worse than a refusal
    /// that names the path.
    #[error("{path:?} is not valid UTF-8 and cannot be registered in {format}")]
    NotUtf8 {
        /// The directory that was to be registered.
        path: PathBuf,
        /// The format it was to be registered in.
        format: SessionPathFormat,
    },

    /// The directory is relative.
    ///
    /// A relative segment on a session PATH resolves against **each process's
    /// own working directory** (CWE-426), so `ocx self setup` with a relative
    /// `$OCX_HOME` would put a per-directory, attacker-plantable lookup on the
    /// PATH of every process the user starts from then on. Nothing upstream
    /// refuses it — `file_structure::default_ocx_root` takes `$OCX_HOME`
    /// verbatim — and the three formats all store the value as written.
    ///
    /// Refused here rather than made absolute against the working directory:
    /// the directory that happened to be current during `ocx self setup` is
    /// not a location the user asked for, and baking it in silently would be
    /// the same defect with a longer fuse.
    #[error("{path:?} is relative and cannot be registered in {format}: a session PATH entry must be absolute")]
    NotAbsolute {
        /// The directory that was to be registered.
        path: PathBuf,
        /// The format it was to be registered in.
        format: SessionPathFormat,
    },
}

impl crate::cli::ClassifyExitCode for SessionPathError {
    /// An exhaustive match with no wildcard arm, copying
    /// [`crate::env::CommandResolutionError`]'s shape: under a blanket arm a
    /// variant added later is silently classified and no test can catch it.
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        match self {
            Self::Unencodable { .. } | Self::NotUtf8 { .. } | Self::NotAbsolute { .. } => {
                Some(crate::cli::ExitCode::ConfigError)
            }
        }
    }
}

/// The session-PATH stores this host owns, in write order.
///
/// Empty on a host with no session-PATH facility. The caller needs this to
/// report [`SessionPathOutcome::SkippedOptOut`] per store when
/// `--no-modify-path` suppresses the arm — the writers are not called in that
/// case, so they cannot name their own locations — and it is the same list the
/// manual-removal recipe in the user guide enumerates.
///
/// `ocx_home` is the store root; it reaches [`HomeEnv`] as the home-directory
/// fallback for a process whose `$HOME` is unset.
///
/// The Windows entry is a registry location rather than a filesystem path.
/// `PathBuf` carries it because the pair's key is a *location* — the same role
/// [`crate::setup::SetupOutcome::profiles`] gives it — and no other type in
/// this signature would be shared by the three platforms.
pub fn session_path_stores(ocx_home: &Path) -> Vec<PathBuf> {
    let home = super::home_env_from_environment(ocx_home);
    stores_for(&home)
}

/// [`session_path_stores`] over an injectable [`HomeEnv`], so the layout is
/// unit-testable without mutating the process environment out from under every
/// concurrent test — the seam [`crate::setup::profiles::detect_targets`]
/// already uses for the same reason.
pub fn stores_for(home: &HomeEnv) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let _ = home;
        vec![PathBuf::from(windows::REGISTRY_LOCATION)]
    }
    #[cfg(target_os = "linux")]
    {
        vec![linux::conf_path(home)]
    }
    #[cfg(target_os = "macos")]
    {
        vec![macos::plist_path(home)]
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = home;
        Vec::new()
    }
}

/// Register `directories` at session level, front-first.
///
/// `directories[0]` ends up nearest the front of the resulting PATH. Today's
/// caller passes exactly two — the ocx install `bin` directory, then
/// `$OCX_HOME/toolchain/bin` — but nothing here depends on the count.
///
/// `ocx_home` is the store root the two directories belong to; it reaches
/// [`HomeEnv`] as the home-directory fallback when `$HOME` is unset.
///
/// `dry_run` computes the same outcome and writes no byte. It still performs
/// the encoding refusal: a `--dry-run` that reported `Written` for a value the
/// real run would refuse would be worse than useless.
///
/// # Errors
///
/// [`SessionPathError`] when a directory cannot be encoded for this host's
/// format (exit 78). Raised before any write, so a refused run changes
/// nothing. A **write** failure is not an error — it is
/// [`SessionPathOutcome::Failed`] with exit 0.
pub fn register_session_path(
    ocx_home: &Path,
    directories: &[PathBuf],
    dry_run: bool,
) -> Result<Vec<(PathBuf, SessionPathOutcome)>, SessionPathError> {
    let home = super::home_env_from_environment(ocx_home);
    register_in(&home, directories, dry_run)
}

/// Apply every C-037 refusal to `directories` without reading or writing a
/// store — the value rule ([`refuse_relative`]) and this host's grammar rule
/// (its platform `encode`).
///
/// Exists so a caller can run the refusal *before* it starts writing. Property
/// (2) of this module's contract says an unencodable directory is refused
/// before anything is written; that is true of the session-PATH stores by
/// construction, but [`crate::setup::run`] writes four other things first
/// (bootstrap, managed config, shims, profiles), so the promise a user is
/// given — a refused run leaves the machine as it was — needs the check to
/// happen ahead of all of them. [`crate::setup::run`] calls this as its first
/// act; [`register_in`] and [`deregister_in`] keep validating on their own, so
/// a caller that skips the preflight is refused just the same.
///
/// # Errors
///
/// [`SessionPathError`] naming the first directory this host's format cannot
/// spell, and the format.
pub fn refuse_unencodable(directories: &[PathBuf]) -> Result<(), SessionPathError> {
    refuse_relative(directories)?;
    for directory in directories {
        #[cfg(windows)]
        windows::encode(directory)?;
        #[cfg(target_os = "linux")]
        linux::encode(directory)?;
        #[cfg(target_os = "macos")]
        macos::encode(directory)?;
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        let _ = directory;
    }
    Ok(())
}

/// [`register_session_path`] over an injectable [`HomeEnv`] (see
/// [`stores_for`]).
///
/// # Errors
///
/// Same as [`register_session_path`].
pub fn register_in(
    home: &HomeEnv,
    directories: &[PathBuf],
    dry_run: bool,
) -> Result<Vec<(PathBuf, SessionPathOutcome)>, SessionPathError> {
    // Before the cfg dispatch, so the one rule that is about the *value*
    // rather than the grammar is applied identically on every host, and
    // before any store is read or written.
    refuse_relative(directories)?;
    #[cfg(windows)]
    {
        let _ = home;
        Ok(vec![windows::register(directories, dry_run)?])
    }
    #[cfg(target_os = "linux")]
    {
        Ok(vec![linux::register(home, directories, dry_run)?])
    }
    #[cfg(target_os = "macos")]
    {
        Ok(vec![macos::register(home, directories, dry_run)?])
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (home, directories, dry_run);
        Ok(unsupported())
    }
}

/// Subtract `directories` from this host's session-PATH store.
///
/// Removes exactly the segments [`register_session_path`] added, leaves every
/// other segment in place, and is a no-op ([`SessionPathOutcome::Unchanged`])
/// when neither is present. It never clears, resets or unsets a whole
/// variable.
///
/// # Errors
///
/// Same as [`register_session_path`]: an encoding refusal only. A directory
/// that cannot be encoded also cannot have been written, so the refusal is
/// reached before the store is read.
pub fn deregister_session_path(
    ocx_home: &Path,
    directories: &[PathBuf],
    dry_run: bool,
) -> Result<Vec<(PathBuf, SessionPathOutcome)>, SessionPathError> {
    let home = super::home_env_from_environment(ocx_home);
    deregister_in(&home, directories, dry_run)
}

/// [`deregister_session_path`] over an injectable [`HomeEnv`] (see
/// [`stores_for`]).
///
/// # Errors
///
/// Same as [`deregister_session_path`].
pub fn deregister_in(
    home: &HomeEnv,
    directories: &[PathBuf],
    dry_run: bool,
) -> Result<Vec<(PathBuf, SessionPathOutcome)>, SessionPathError> {
    // Before the cfg dispatch, so the one rule that is about the *value*
    // rather than the grammar is applied identically on every host, and
    // before any store is read or written.
    refuse_relative(directories)?;
    #[cfg(windows)]
    {
        let _ = home;
        Ok(vec![windows::deregister(directories, dry_run)?])
    }
    #[cfg(target_os = "linux")]
    {
        Ok(vec![linux::deregister(home, directories, dry_run)?])
    }
    #[cfg(target_os = "macos")]
    {
        Ok(vec![macos::deregister(home, directories, dry_run)?])
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (home, directories, dry_run);
        Ok(unsupported())
    }
}

/// The one pair a host with no session-PATH facility reports.
///
/// Keyed by [`UNSUPPORTED_LOCATION`] rather than by an empty path: the pair's
/// key is rendered in the run summary, and an empty one would print as a blank
/// column that reads like a bug.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn unsupported() -> Vec<(PathBuf, SessionPathOutcome)> {
    vec![(
        PathBuf::from(UNSUPPORTED_LOCATION),
        SessionPathOutcome::SkippedUnsupported,
    )]
}

/// The location key reported on a host with no session-PATH facility.
///
/// Not a path. Deliberately unspellable as one — angle brackets are not a
/// filesystem convention on any of the three supported targets — so nobody
/// reads the run summary as naming a file they could edit.
pub const UNSUPPORTED_LOCATION: &str = "<no session PATH facility on this platform>";

/// The UTF-8 spelling of `directory`, or a [`SessionPathError::NotUtf8`]
/// refusal naming `format`.
///
/// The one place a session-PATH directory crosses from `Path` to `str`; every
/// refusal predicate and every renderer takes the `&str` this returns, so none
/// of them can reach for `to_string_lossy` on its own.
///
/// It deliberately does **not** check absoluteness. That is a property of the
/// value, not of the grammar, and the three `encode` functions are pure
/// grammar so their red state is reachable on every CI leg rather than only on
/// the host whose format they describe. [`refuse_relative`] owns the value
/// rule, at the dispatch boundary.
///
/// # Errors
///
/// [`SessionPathError::NotUtf8`] when `directory` is not valid UTF-8.
pub fn encodable_str(directory: &Path, format: SessionPathFormat) -> Result<&str, SessionPathError> {
    directory.to_str().ok_or_else(|| SessionPathError::NotUtf8 {
        path: directory.to_path_buf(),
        format,
    })
}

/// The wire format this host's session-PATH store uses, or `None` where there
/// is no store — the [`stores_for`] split, answered as a format rather than a
/// location.
fn host_format() -> Option<SessionPathFormat> {
    #[cfg(windows)]
    {
        Some(SessionPathFormat::WindowsRegistry)
    }
    #[cfg(target_os = "linux")]
    {
        Some(SessionPathFormat::EnvironmentD)
    }
    #[cfg(target_os = "macos")]
    {
        Some(SessionPathFormat::LaunchAgentPlist)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Refuse a relative directory before any store is read or written.
///
/// A relative segment on a session PATH resolves against **each process's own
/// working directory** (CWE-426), so registering one puts a per-directory,
/// attacker-plantable lookup on the PATH of every process the user starts from
/// then on. Nothing upstream refuses it —
/// [`crate::file_structure::default_ocx_root`] takes `$OCX_HOME` verbatim —
/// and all three formats store the value exactly as written.
///
/// Refused rather than made absolute against the working directory: whichever
/// directory happened to be current during `ocx self setup` is not a location
/// the user asked for, and baking it in silently is the same defect with a
/// longer fuse.
///
/// `Path::is_absolute` is host-relative on purpose. A driveless `/opt/bin` has
/// a root but no drive prefix, so Windows answers `false` — and that is the
/// wanted answer, because such a path resolves against the *current drive*,
/// which is the same per-process ambiguity in a second spelling.
///
/// # Errors
///
/// [`SessionPathError::NotAbsolute`] naming the first relative directory.
fn refuse_relative(directories: &[PathBuf]) -> Result<(), SessionPathError> {
    let Some(format) = host_format() else {
        return Ok(());
    };
    match directories.iter().find(|directory| !directory.is_absolute()) {
        Some(directory) => Err(SessionPathError::NotAbsolute {
            path: directory.clone(),
            format,
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute directory this host's PATH format spells without complaint.
    ///
    /// `Path::is_absolute` is host-relative, so a POSIX literal would be
    /// *relative* on Windows and the acceptance arm below would pass through
    /// [`refuse_relative`] instead of the grammar rule it is aimed at.
    fn plain_absolute_directory() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\ocx\bin")
        } else {
            PathBuf::from("/opt/ocx/bin")
        }
    }

    /// The first character **this host's** format cannot spell, from the
    /// platform module [`refuse_unencodable`] dispatches to.
    fn host_refused_character() -> char {
        #[cfg(windows)]
        {
            windows::REFUSED[0].0
        }
        #[cfg(target_os = "linux")]
        {
            linux::REFUSED[0].0
        }
        #[cfg(target_os = "macos")]
        {
            macos::REFUSED[0].0
        }
    }

    /// C-037 — the preflight is wired to *this host's* grammar, and says yes to
    /// a directory the host can spell.
    ///
    /// Both halves are the test. A `cfg` dispatch that reached no arm — the
    /// failure mode of a function whose whole body is behind `cfg`, and the one
    /// that would quietly make `setup::run`'s phase 0 a no-op — returns `Ok`
    /// for every input, so the refusal half fails. A preflight hard-wired to
    /// refuse would pass that half while failing the acceptance half, which is
    /// why the plain path is asserted too.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn the_preflight_applies_this_hosts_grammar() {
        let plain = vec![plain_absolute_directory()];
        assert!(
            refuse_unencodable(&plain).is_ok(),
            "a directory this host can spell must not be refused: {plain:?}"
        );

        let hostile = vec![plain_absolute_directory().join(format!("a{}b", host_refused_character()))];
        assert!(
            matches!(refuse_unencodable(&hostile), Err(SessionPathError::Unencodable { .. })),
            "the preflight did not apply this host's own grammar rule to {hostile:?}"
        );

        assert!(
            matches!(
                refuse_unencodable(&[PathBuf::from("relative/bin")]),
                Err(SessionPathError::NotAbsolute { .. })
            ),
            "the preflight must carry the value rule as well as the grammar rule"
        );
    }

    /// The refusal is a configuration fault, exit 78 — the code the ADR's
    /// error table gives "`OCX_HOME` cannot be encoded for a session-PATH
    /// format".
    #[test]
    fn an_encoding_refusal_classifies_as_a_configuration_error() {
        use crate::cli::ClassifyExitCode as _;

        let refusal = SessionPathError::Unencodable {
            path: PathBuf::from("/opt/100%real"),
            format: SessionPathFormat::WindowsRegistry,
            character: '%',
            reason: "and REG_EXPAND_SZ has no escape for it",
        };
        assert_eq!(refusal.classify(), Some(crate::cli::ExitCode::ConfigError));
        assert_eq!(
            SessionPathError::NotUtf8 {
                path: PathBuf::from("/opt"),
                format: SessionPathFormat::EnvironmentD,
            }
            .classify(),
            Some(crate::cli::ExitCode::ConfigError)
        );

        // The hop that makes 78 reachable from `argv` rather than merely
        // classifiable: `SetupError` is what `cli::classify` downcasts, and its
        // `SessionPath` arm delegates back to the impl above. Without the
        // wrapper variant this type would be unreachable from every command.
        assert_eq!(
            crate::setup::error::Error::from(refusal).classify(),
            Some(crate::cli::ExitCode::ConfigError)
        );
    }

    /// A relative directory is refused by **both** entry points, before any
    /// store is read or written (CWE-426).
    ///
    /// Nothing upstream refuses one: `default_ocx_root` takes `$OCX_HOME`
    /// verbatim, so `OCX_HOME=ocxhome ocx self setup` would otherwise put the
    /// relative `ocxhome/toolchain/bin` into the registry value, the
    /// `environment.d` line and the LaunchAgent alike — a lookup directory
    /// every later process resolves against *its own* working directory.
    ///
    /// Both directions are asserted, and this is why the assertion is on the
    /// **home-taking** entry points rather than on the three `encode`
    /// functions: absoluteness is a property of the value, and putting it in an
    /// `encode` would have made those deliberately host-independent grammar
    /// tests unrunnable off their own platform (a `C:\…` fixture is relative to
    /// Linux, a `/home/…` one is relative to Windows).
    ///
    /// The `dry_run` argument is `false` on purpose: the refusal has to precede
    /// the write on the **real** path, and a test that only ever asked for a
    /// dry run could not tell a pre-write refusal from a writer that happened
    /// to be inert. Nothing is written either way — the `?` fires first, which
    /// is the property under test.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_relative_directory_is_refused_before_any_store_is_touched() {
        // An absolute, certainly-nonexistent root, built the way the host
        // spells one so the fixture is not itself a relative path on Windows.
        let root = std::env::temp_dir().join("ocx-session-path-refusal-fixture");
        let home = HomeEnv {
            home: root.clone(),
            zdotdir: None,
            xdg_config_home: Some(root.join(".config")),
            xdg_data_home: None,
            ocx_home: root.join(".ocx"),
            shell: None,
        };
        let relative = vec![PathBuf::from("ocxhome/toolchain/bin")];

        for (label, result) in [
            ("register_in", register_in(&home, &relative, false)),
            ("deregister_in", deregister_in(&home, &relative, false)),
        ] {
            match result {
                Err(SessionPathError::NotAbsolute { path, .. }) => {
                    assert_eq!(path, relative[0], "{label} must name the offending directory");
                }
                other => panic!("{label} must refuse a relative directory, got {other:?}"),
            }
        }

        // The negative control: the same call with an absolute directory gets
        // past the refusal. Asserted as "not NotAbsolute" rather than as `Ok`,
        // because what happens after the refusal is the platform writer's
        // business and its store is not writable from here.
        let absolute = vec![root.join("toolchain").join("bin")];
        assert!(absolute[0].is_absolute(), "the control fixture must be absolute");
        assert!(
            !matches!(
                register_in(&home, &absolute, true),
                Err(SessionPathError::NotAbsolute { .. })
            ),
            "an absolute directory must clear the refusal, or the check is refusing everything"
        );
    }

    /// CWE-117: a refused path is exactly the one that may carry a newline, so
    /// the message must escape it rather than render it raw.
    #[test]
    fn a_refusal_message_escapes_the_path_it_names() {
        let rendered = SessionPathError::Unencodable {
            path: PathBuf::from("/opt/a\nb"),
            format: SessionPathFormat::EnvironmentD,
            character: '\n',
            reason: "and a value that spans two lines declares a second variable",
        }
        .to_string();
        assert!(
            !rendered.contains('\n'),
            "the message must not carry a raw newline: {rendered:?}"
        );
        assert!(
            rendered.contains("\\n"),
            "the path must be escaped, not dropped: {rendered:?}"
        );
    }

    /// The whole-variable destructors, and the second `toml_edit` site, that
    /// must never appear in this subsystem.
    ///
    /// Each needle names an accident with a stated blast radius rather than a
    /// style preference:
    ///
    /// - `launchctl unsetenv` deletes the entire session-wide `PATH`, stripping
    ///   every other tool's segment from every GUI application launched
    ///   afterwards. Deregistration subtracts two segments and re-`setenv`s the
    ///   remainder.
    /// - `RegDeleteValue` deletes `HKCU\Environment\Path` outright. The value is
    ///   **edited**, never removed.
    /// - `toml_edit` here would be C-042's forbidden second write site. The
    ///   `activate` key goes through `project::mutate`, under the project lock,
    ///   and nowhere else.
    ///
    /// A tripwire for the likely accident, not the contract — S-014's
    /// foreign-segment survivor is the behavioural half.
    ///
    /// Two things are cut before the scan, and both were **observed** failing
    /// this guard against itself rather than anticipated. Comment lines: the
    /// doc comments above quote every needle, which is the right thing for a
    /// doc comment to do. And this `#[cfg(test)]` module: `FORBIDDEN` below is
    /// a literal in the very file being scanned, so a detector that reads its
    /// own needle list reports the same answer in every state. The subject is
    /// the shipped code, which is what the cut leaves.
    #[test]
    fn no_writer_here_destroys_a_whole_variable_or_opens_a_second_toml_site() {
        const SOURCES: &[(&str, &str)] = &[
            ("session_path.rs", include_str!("session_path.rs")),
            ("windows.rs", include_str!("session_path/windows.rs")),
            ("linux.rs", include_str!("session_path/linux.rs")),
            ("macos.rs", include_str!("session_path/macos.rs")),
        ];
        const FORBIDDEN: &[&str] = &["launchctl unsetenv", "RegDeleteValue", "toml_edit"];

        for (name, source) in SOURCES {
            let shipped = source.split("#[cfg(test)]").next().unwrap_or(source);
            let code: String = shipped
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");

            // Non-vacuity: a needle that stops matching, a file that stops
            // being scanned, or a cut that swallows the whole file would each
            // leave this test green while measuring nothing.
            assert!(
                code.contains("fn ") && code.len() > 200,
                "{name}: the scanned text is not this module's code any more"
            );
            assert!(
                !code.contains("mod tests"),
                "{name}: the test module must be cut, or this guard reads its own needle list"
            );
            for needle in FORBIDDEN {
                assert!(!code.contains(needle), "{name} must not reach for {needle:?}");
            }
        }
    }

    /// A non-UTF-8 directory is refused rather than lossily converted.
    #[test]
    fn a_non_utf8_directory_is_refused() {
        assert!(encodable_str(Path::new("/opt/bin"), SessionPathFormat::EnvironmentD).is_ok());

        #[cfg(unix)]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt as _;

            let invalid = Path::new(OsStr::from_bytes(b"/opt/\xff"));
            assert!(matches!(
                encodable_str(invalid, SessionPathFormat::EnvironmentD),
                Err(SessionPathError::NotUtf8 { .. })
            ));
        }
    }

    /// Shipped code only: the `#[cfg(test)]` tail, `//` comment lines and
    /// `unimplemented!` placeholder messages are cut.
    ///
    /// All three cuts were needed rather than anticipated. Comments, because a
    /// denylist that quotes what it forbids matches its own documentation. The
    /// test module, because a needle list is a literal in the file being
    /// scanned, so a detector that reads its own list answers the same in every
    /// state. And `unimplemented!` bodies, because a stub's placeholder message
    /// names the very API the guard is asking for — which would make a positive
    /// assertion green before a single line of the implementation existed.
    fn shipped_code(source: &str) -> String {
        source
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(source)
            .lines()
            .filter(|line| !line.trim_start().starts_with("//") && !line.contains("unimplemented!"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// **C-036's never-block rule, from `setup::run` down** — the half no
    /// writer-level test can reach.
    ///
    /// A `Failed` computed and discarded is a green indistinguishable from the
    /// arm never having run, so three things have to be true of `setup.rs` and
    /// each one reds on its own:
    ///
    /// 1. `run` calls the registrar. Without this the whole subsystem is dead
    ///    code no argv route reaches.
    /// 2. [`SetupOutcome`](crate::setup::SetupOutcome) carries the outcome
    ///    vector, so a `Failed` has somewhere to be warned from and somewhere
    ///    to appear in `--format json`.
    /// 3. `SkippedOptOut` is produced **here**, by the caller, not by a writer:
    ///    C-043 suppresses the arm entirely, so the writers are never called
    ///    and cannot name their own stores. That is what
    ///    [`session_path_stores`] exists for.
    ///
    /// The `Err` arm of the registrar is a different rule and is deliberately
    /// **not** forbidden here: C-037's pre-write encoding refusal *is* an
    /// error, exit 78, and `?`-propagating it is correct. What must never reach
    /// that arm is an `io::Error` — enforced by the type, which has no
    /// `From<io::Error>` impl (see
    /// [`no_io_error_channel_exists_into_the_refusal_type`]).
    #[test]
    fn the_session_path_arm_is_wired_into_setup_run() {
        let code = shipped_code(include_str!("../setup.rs"));

        assert!(
            code.contains("register_session_path(") || code.contains("session_path::register"),
            "C-036: `setup::run` must call the session-PATH registrar, or the whole arm is unreachable"
        );
        assert!(
            code.contains("pub session_path:"),
            "C-036: `SetupOutcome` must carry the session-PATH outcomes, or a `Failed` is computed and discarded"
        );
        assert!(
            code.contains("SkippedOptOut"),
            "C-043: the opt-out is reported per store by the caller, which is the only party that knows the \
             writers were never called"
        );
        assert!(
            code.contains("session_path_stores(") || code.contains("session_path::stores_for"),
            "C-043: the suppressed run still names the stores it did not touch"
        );
    }

    /// C-036's never-block rule, enforced by the type rather than by review.
    ///
    /// A `From<io::Error>` impl on the refusal type would let a stray `?` on a
    /// write turn a `Failed` into a non-zero exit from any of the four files
    /// here, silently — which is exactly the defect the rule exists to prevent,
    /// and the shape all four surrounding phases of `setup::run` already model.
    ///
    /// A tripwire for the likely accident, not the contract: a denylist cannot
    /// enumerate every way to write the forbidden shape. The behavioural half
    /// is the platform writer's own "a store that cannot be written is `Failed`
    /// and not an error".
    #[test]
    fn no_io_error_channel_exists_into_the_refusal_type() {
        for (name, source) in SOURCES {
            let code = shipped_code(source);
            for forbidden in ["From<std::io::Error>", "From<io::Error>", "#[from] std::io::Error"] {
                assert!(
                    !code.contains(forbidden),
                    "{name}: a write failure is `SessionPathOutcome::Failed`, never a `SessionPathError` — \
                     {forbidden:?} would make a stray `?` convert one into the other"
                );
            }
        }
    }

    /// The four files this subsystem owns.
    const SOURCES: &[(&str, &str)] = &[
        ("session_path.rs", include_str!("session_path.rs")),
        ("windows.rs", include_str!("session_path/windows.rs")),
        ("linux.rs", include_str!("session_path/linux.rs")),
        ("macos.rs", include_str!("session_path/macos.rs")),
    ];

    /// Verbs that destroy more than this subsystem owns, or that name a
    /// mechanism its contract rejects. Each has a stated blast radius:
    ///
    /// - `launchctl unload` is deprecated; `bootout` of the **service** target
    ///   replaces it (D-V26).
    /// - `unsetenv` deletes the entire session-wide `PATH`.
    /// - `setx` truncates silently at 1024 characters.
    /// - `RegDeleteKey` removes the key rather than editing the value.
    /// - `pam_environment` is deprecated since pam_env 1.5.0 and the ADR names
    ///   it as a rejected alternative — a negative control that catches a
    ///   well-meaning re-addition.
    ///
    /// Complements the shipped denylist rather than replacing it; both are
    /// tripwires for the likely accident, and S-014's foreign-segment survivor
    /// is the behavioural half.
    #[test]
    fn no_writer_reaches_for_a_deprecated_or_whole_variable_verb() {
        const FORBIDDEN: &[&str] = &[
            "launchctl unload",
            "unsetenv",
            "setx",
            "RegDeleteKey",
            "pam_environment",
        ];

        for (name, source) in SOURCES {
            let code = shipped_code(source);
            assert!(
                code.contains("fn ") && code.len() > 200,
                "{name}: the scanned text is not this module's code any more"
            );
            for needle in FORBIDDEN {
                assert!(!code.contains(needle), "{name} must not reach for {needle:?}");
            }
        }
    }

    /// E-X8 / C-036: a host that is neither Windows, Linux nor macOS reports
    /// [`SessionPathOutcome::SkippedUnsupported`] and writes nothing.
    ///
    /// The arm is reachable only by `cfg`, so what is asserted is that it
    /// **exists** — on all four functions that need it — and that its location
    /// key is deliberately unspellable as a filesystem path, so nobody reads
    /// the run summary as naming a file they could edit.
    #[test]
    fn the_unsupported_host_arm_exists_and_is_keyed_by_a_non_path() {
        let code = shipped_code(include_str!("session_path.rs"));
        let arms = code
            .matches(r#"#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]"#)
            .count();
        assert!(
            arms >= 4,
            "`stores_for`, `register_in`, `deregister_in` and the `unsupported` pair each need the fallback arm; \
             found {arms}"
        );
        assert!(
            UNSUPPORTED_LOCATION.starts_with('<') && UNSUPPORTED_LOCATION.ends_with('>'),
            "the key must not read as a file a user could edit: {UNSUPPORTED_LOCATION:?}"
        );
    }

    /// C-036 / E-X8: this host owns exactly one session-PATH store, and it is
    /// the one its platform module names. Empty only where no facility exists.
    #[test]
    fn this_host_owns_exactly_one_session_path_store() {
        let home = HomeEnv {
            home: PathBuf::from("/home/u"),
            zdotdir: None,
            xdg_config_home: Some(PathBuf::from("/home/u/xdg")),
            xdg_data_home: None,
            ocx_home: PathBuf::from("/home/u/.ocx"),
            shell: None,
        };
        let stores = stores_for(&home);

        #[cfg(windows)]
        assert_eq!(stores, vec![PathBuf::from(windows::REGISTRY_LOCATION)]);
        #[cfg(target_os = "linux")]
        assert_eq!(stores, vec![linux::conf_path(&home)]);
        #[cfg(target_os = "macos")]
        assert_eq!(stores, vec![macos::plist_path(&home)]);
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        assert!(stores.is_empty());
    }

    /// C-037 / S-015, on whichever host runs it: a directory carrying a NUL is
    /// unrepresentable in **all three** formats, so it is refused before
    /// anything is written and the store is left untouched.
    ///
    /// NUL is the one character in every platform's refusal set, which is what
    /// makes this the cross-platform half of the per-format tests. It is
    /// reachable rather than theoretical: a `PathBuf` can carry one on Unix.
    #[test]
    fn every_host_refuses_a_directory_no_format_can_carry() {
        let home = HomeEnv {
            home: PathBuf::from("/home/u"),
            zdotdir: None,
            xdg_config_home: Some(PathBuf::from("/home/u/xdg")),
            xdg_data_home: None,
            ocx_home: PathBuf::from("/home/u/.ocx"),
            shell: None,
        };
        // Absolute *to this host's parser*. `register_in` runs `refuse_relative`
        // ahead of the cfg dispatch, and a driveless `/home/u/…` has a root but
        // no drive prefix on Windows — so a POSIX literal is refused as
        // `NotAbsolute` there and the encoding rule under test never runs.
        let hostile = vec![PathBuf::from(if cfg!(windows) {
            "C:\\home\\u\\a\u{0}b\\.ocx\\bin"
        } else {
            "/home/u/a\u{0}b/.ocx/bin"
        })];

        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
        {
            let error = register_in(&home, &hostile, false).expect_err("a NUL cannot be encoded for any format");
            assert!(matches!(
                error,
                SessionPathError::Unencodable { character: '\u{0}', .. }
            ));

            let error = deregister_in(&home, &hostile, false).expect_err("the remove path validates too");
            assert!(matches!(
                error,
                SessionPathError::Unencodable { character: '\u{0}', .. }
            ));
        }
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        {
            assert_eq!(
                register_in(&home, &hostile, false).expect("an unsupported host writes nothing")[0].1,
                SessionPathOutcome::SkippedUnsupported
            );
        }
    }
}
