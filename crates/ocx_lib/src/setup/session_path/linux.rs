// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The Linux session-PATH writer: `environment.d/ocx.conf`.
//!
//! **Co-primary with the `~/.profile` managed block, not a fallback for it.**
//! `environment.d` reaches only processes started under `systemd --user` —
//! confirmed for GNOME and KDE Plasma Wayland, and read by neither LightDM (by
//! default), SDDM, nor a non-systemd desktop. The profile block reaches login
//! shells and nothing else. Each covers what the other misses, so both are
//! written and neither is conditional on the other.
//!
//! The `~/.profile` half is **not** this module's: it is the managed RC block
//! `ocx self setup` already writes through [`crate::setup::rc_block`]. This
//! module owns exactly one file.
//!
//! `~/.pam_environment` is deliberately not written — deprecated since
//! pam_env 1.5.0.
//!
//! Two limits are stated rather than worked around: a desktop that is neither
//! a systemd-user session nor profile-sourcing (a bare i3/sway started outside
//! any Xsession wrapper) sees neither directory, and a Flatpak- or
//! Snap-sandboxed application takes its PATH from the sandbox rather than from
//! the session.

use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use super::SessionPathOutcome;
use super::{SessionPathError, SessionPathFormat};
use crate::setup::profiles::HomeEnv;

/// The file this writer owns, relative to the XDG config home.
pub const CONF_RELATIVE_PATH: &str = "environment.d/ocx.conf";

/// Characters an `environment.d` value cannot carry **in any position**,
/// checked in this order (the first match is the one the refusal names).
///
/// # Where this set comes from
///
/// Derived from systemd's own reader — `parse_env_file_internal` in
/// `src/basic/env-file.c`, whose `VALUE`/`VALUE_ESCAPE`/`PRE_VALUE` states
/// decide what survives — and then verified against the shipped
/// `30-systemd-environment-d-generator` binary, one candidate character per
/// run, on inputs where the losing cases and the surviving cases were both
/// observed. It is **not** derived from `environment.d(5)`, which describes
/// the format but does not enumerate the parser's escape and strip rules; the
/// first cut of this set was written from the manual page and got `\` wrong.
///
/// - `\n` and `\r` — the format is line-structured `KEY=VALUE`, so a value
///   spanning two lines declares a second, unrelated variable. `\r` is refused
///   beside `\n` because a lone `\r` ends a line for some readers and not
///   others, which is worse than either answer.
/// - `\` — the parser's `VALUE_ESCAPE` state consumes it and takes the next
///   character literally, so `/home/a\b` is read back as `/home/ab` and a
///   directory ending in `\` swallows the `:` that follows it, welding two
///   PATH entries into one. There is no way to spell a literal `\` that
///   `environment.d` and a POSIX path agree on, so it is refused rather than
///   doubled.
/// - `$` — `${FOO}` and `${FOO:-x}` expand at session-manager read time, so a
///   `$` in the directory names something else by the time it is read. The
///   trailing `:$PATH` in the line we write is *ours* and deliberate; a `$`
///   arriving from `$OCX_HOME` is not.
/// - `:` — the list delimiter. A segment containing one is two segments to
///   every reader.
/// - `\0` — cannot appear in a text configuration file at all.
///
/// `=` is admitted, and that is a decision rather than an omission: the format
/// splits a line on its **first** `=` only, so every later one is ordinary
/// value text. So are `%`, `'`, `"`, `&`, `<`, `>`, `#`, `;` and a space away
/// from the front — each confirmed to survive the generator unchanged.
pub const REFUSED: &[(char, &str)] = &[
    (
        '\n',
        "and a value that spans two lines declares a second, unrelated variable",
    ),
    ('\r', "which ends a line for some readers of this format and not others"),
    (
        '\\',
        "which this format consumes as an escape, dropping it and taking the next character literally",
    ),
    ('$', "which the session manager expands to something else at read time"),
    (':', "which is the PATH delimiter and would split the entry in two"),
    ('\0', "which a text configuration file cannot carry"),
];

/// Characters an `environment.d` value cannot carry **as its first character**,
/// checked in this order after [`REFUSED`] finds nothing.
///
/// Same derivation as [`REFUSED`]: systemd's `PRE_VALUE` state skips over
/// `WHITESPACE` before the value begins, and a quote in the first position
/// opens a quoted string that runs past the line ending. Confirmed against the
/// generator, which strips a leading space and a leading tab, and consumes a
/// leading `'` or `"` while pulling the newline into the value. Vertical tab
/// and form feed survive the same position untouched — they are not in
/// systemd's `WHITESPACE` — and so are not refused.
///
/// Refused for **every** directory, not only the one that happens to be
/// written first. Only the leading directory sits at the value's front, but
/// which directory leads is [`render_conf`]'s business; making [`encode`]'s
/// answer depend on a caller's ordering would be a contract that silently
/// changes when the order does.
pub const REFUSED_LEADING: &[(char, &str)] = &[
    (' ', "which this format strips when it leads a value"),
    ('\t', "which this format strips when it leads a value"),
    (
        '"',
        "which opens a quoted string when it leads a value and swallows the line ending",
    ),
    (
        '\'',
        "which opens a quoted string when it leads a value and swallows the line ending",
    ),
];

/// `$XDG_CONFIG_HOME/environment.d/ocx.conf`, or
/// `$HOME/.config/environment.d/ocx.conf` when `XDG_CONFIG_HOME` is unset.
///
/// The XDG fallback is systemd's own search rule for `environment.d`, and it
/// is spelled here from [`HomeEnv`]'s public fields rather than shared with
/// [`crate::setup::profiles`]'s private `config_home`, which applies the same
/// rule for the profile targets. Two spellings of one rule is a drift risk
/// worth one line of consolidation later; it is not worth widening another
/// package's private helper mid-wave.
pub fn conf_path(home: &HomeEnv) -> PathBuf {
    home.xdg_config_home
        .clone()
        .unwrap_or_else(|| home.home.join(".config"))
        .join(CONF_RELATIVE_PATH)
}

/// Refuse `directory` when it cannot be spelled in an `environment.d` value,
/// else yield its UTF-8 spelling.
///
/// # Errors
///
/// [`SessionPathError::NotUtf8`] for a non-UTF-8 directory;
/// [`SessionPathError::Unencodable`] naming the first character of [`REFUSED`]
/// the directory carries, or — when it carries none — the character of
/// [`REFUSED_LEADING`] it begins with.
pub fn encode(directory: &Path) -> Result<&str, SessionPathError> {
    let spelled = super::encodable_str(directory, SessionPathFormat::EnvironmentD)?;
    let offender = REFUSED
        .iter()
        .find(|(character, _)| spelled.contains(*character))
        .or_else(|| {
            let leading = spelled.chars().next()?;
            REFUSED_LEADING.iter().find(|(character, _)| *character == leading)
        });
    match offender {
        Some((character, reason)) => Err(SessionPathError::Unencodable {
            path: directory.to_path_buf(),
            format: SessionPathFormat::EnvironmentD,
            character: *character,
            reason,
        }),
        None => Ok(spelled),
    }
}

/// The whole content of `ocx.conf`: a single `PATH=…:$PATH` prepend carrying
/// `directories` in order.
///
/// A prepend, not an assignment: the session manager's own value follows, so
/// nothing another tool put on PATH is displaced. Every directory is refused
/// or encoded before it reaches the line, which is what makes a bare
/// `format!` safe here — there is no escaping layer because there is no
/// character left that would need one.
///
/// # Errors
///
/// [`SessionPathError`] from [`encode`], for the first directory that cannot
/// be encoded.
pub fn render_conf(directories: &[PathBuf]) -> Result<String, SessionPathError> {
    let mut segments = Vec::with_capacity(directories.len() + 1);
    for directory in directories {
        segments.push(encode(directory)?);
    }
    // `$PATH` last, and appended rather than interpolated into a `:`-prefixed
    // literal: with no directories the line is `PATH=$PATH`, never a leading
    // `:` — which every reader takes as the working directory.
    segments.push("$PATH");
    Ok(format!("PATH={}\n", segments.join(":")))
}

/// Write `ocx.conf`, creating `environment.d/` when absent.
///
/// Ensure-present rather than write-once: the file is rewritten when its
/// content differs and left untouched when it already matches, so a re-run is
/// [`SessionPathOutcome::Unchanged`] and does not disturb its mtime.
///
/// The write goes through [`crate::utility::fs::write_bytes_atomic`], which
/// publishes a temp file created `0o600` in the target's own parent — the
/// owner-only-write hygiene C-039 asks for (CWE-732), and the same primitive
/// the shipped shim and profile writes already use rather than a second
/// atomic-write path.
///
/// # Errors
///
/// [`SessionPathError`] from [`render_conf`]. An I/O failure is
/// [`SessionPathOutcome::Failed`], never an error.
#[cfg(target_os = "linux")]
pub(crate) fn register(
    home: &HomeEnv,
    directories: &[PathBuf],
    dry_run: bool,
) -> Result<(PathBuf, SessionPathOutcome), SessionPathError> {
    // The one `?` on this path, and it runs before a byte is read or written:
    // from here down every failure is an outcome (C-036).
    let content = render_conf(directories)?;
    let path = conf_path(home);

    if conf_is_current(&path, &content) {
        return Ok((path, SessionPathOutcome::Unchanged));
    }
    if dry_run {
        return Ok((path, SessionPathOutcome::Written));
    }
    let outcome = match write_conf(&path, &content) {
        Ok(()) => SessionPathOutcome::Written,
        Err(error) => {
            // Reported, not swallowed: the user-facing half is the caller's
            // warning, and this is the cause behind it.
            tracing::debug!(path = ?path, %error, "session-PATH conf write failed");
            SessionPathOutcome::Failed
        }
    };
    Ok((path, outcome))
}

/// Whether the conf on disk is already what this run would publish — **bytes
/// and permissions both**.
///
/// The mode belongs in the comparison for the same reason it does in the
/// macOS sibling's [`super::macos::plist_path`] check, and the failure it
/// stops is worse here: a group- or world-**writable** `ocx.conf` lets another
/// account prepend a directory to the victim's PATH in every `systemd --user`
/// session (CWE-732). A bytes-only comparison reports such a file `Unchanged`
/// forever, so the file `ocx self setup` is supposed to own would never be
/// rewritten and the hygiene this writer claims would be a claim only.
///
/// Only the *writable* bits are judged. `environment.d` files are ordinarily
/// world-readable, and rewriting a 0644 conf on every run to force 0600 would
/// churn a file for a property that is not a vulnerability.
#[cfg(target_os = "linux")]
fn conf_is_current(path: &Path, content: &str) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o022 == 0)
        && std::fs::read(path).is_ok_and(|current| current == content.as_bytes())
}

/// Create `environment.d/` and publish `content` into it.
///
/// The parent is created here because [`crate::utility::fs::write_bytes_atomic`]
/// stages its temp file *in* the target's parent and does not create it — so on
/// a fresh machine the very first `ocx self setup` would otherwise report
/// [`SessionPathOutcome::Failed`].
#[cfg(target_os = "linux")]
fn write_conf(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::utility::fs::write_bytes_atomic(path, content.as_bytes())
}

/// Delete `ocx.conf`.
///
/// The file carries nothing but our own prepend, so deleting it subtracts
/// exactly the two segments and touches no other variable and no other file —
/// in particular not `~/.profile`, whose managed block belongs to
/// [`crate::setup::rc_block`]. An absent file is
/// [`SessionPathOutcome::Unchanged`], not an error.
///
/// # Errors
///
/// [`SessionPathError`] from [`encode`] — the directories are still validated,
/// so a value that could never have been written is diagnosed as such rather
/// than reported as a successful removal.
#[cfg(target_os = "linux")]
pub(crate) fn deregister(
    home: &HomeEnv,
    directories: &[PathBuf],
    dry_run: bool,
) -> Result<(PathBuf, SessionPathOutcome), SessionPathError> {
    // The one `?` on this path, and it runs before the store is touched.
    for directory in directories {
        encode(directory)?;
    }
    let path = conf_path(home);

    // `symlink_metadata`, not `Path::exists`: a dangling symlink at the store
    // is present — it just does not resolve — and reporting it absent would
    // leave it in place while claiming there was nothing to remove.
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok((path, SessionPathOutcome::Unchanged));
    }
    if dry_run {
        return Ok((path, SessionPathOutcome::Removed));
    }
    let outcome = match std::fs::remove_file(&path) {
        Ok(()) => SessionPathOutcome::Removed,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => SessionPathOutcome::Unchanged,
        Err(error) => {
            tracing::debug!(path = ?path, %error, "session-PATH conf removal failed");
            SessionPathOutcome::Failed
        }
    };
    Ok((path, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home_env(home: &str, xdg: Option<&str>) -> HomeEnv {
        HomeEnv {
            home: PathBuf::from(home),
            zdotdir: None,
            xdg_config_home: xdg.map(PathBuf::from),
            xdg_data_home: None,
            ocx_home: PathBuf::from("/home/u/.ocx"),
            shell: None,
        }
    }

    /// C-039: the location is systemd's, and `XDG_CONFIG_HOME` decides it when
    /// set — the rule `environment.d` itself follows.
    #[test]
    fn the_conf_lands_under_the_xdg_config_home() {
        assert_eq!(
            conf_path(&home_env("/home/u", None)),
            PathBuf::from("/home/u/.config/environment.d/ocx.conf")
        );
        assert_eq!(
            conf_path(&home_env("/home/u", Some("/elsewhere/cfg"))),
            PathBuf::from("/elsewhere/cfg/environment.d/ocx.conf")
        );
    }

    // ── C-037 / C-039: the refusal set ───────────────────────────────────

    fn refusal(directory: &str) -> SessionPathError {
        encode(Path::new(directory)).expect_err("the directory must be refused")
    }

    fn refused_character(directory: &str) -> char {
        match refusal(directory) {
            SessionPathError::Unencodable {
                character,
                format: SessionPathFormat::EnvironmentD,
                ..
            } => character,
            other => panic!("expected an Unencodable refusal naming the environment.d format, got {other:?}"),
        }
    }

    /// C-039 / S-015 / item 5 (E-L8, E-L9): every character an `environment.d`
    /// value cannot carry is refused **before** the file is written.
    ///
    /// `\n` and `\r` because the format is line-structured `KEY=VALUE`, so a
    /// value spanning two lines declares a second, unrelated variable — the
    /// injection this refusal exists for. `$` because `${FOO}` and `${FOO:-x}`
    /// expand at session-manager **read** time, so a `$` arriving from
    /// `$OCX_HOME` names something else by the time the value is used. `\`
    /// because systemd's parser consumes it as an escape, so `a\b` is read
    /// back as `ab` and a trailing one eats the `:` after it. `:` and `\0` are
    /// the two widenings on the same ground.
    ///
    /// The last four rows are position-dependent ([`REFUSED_LEADING`]): a
    /// space or tab at the front of the value is stripped before the session
    /// manager reads it, and a quote there opens a string that runs past the
    /// line ending.
    #[test]
    fn environment_d_refuses_every_character_it_cannot_carry() {
        for (directory, offender) in [
            ("/home/u/a\nb/.ocx/bin", '\n'),
            ("/home/u/a\rb/.ocx/bin", '\r'),
            ("/home/u/a\\b/.ocx/bin", '\\'),
            ("/home/u/a$b/.ocx/bin", '$'),
            ("/home/u/a:b/.ocx/bin", ':'),
            ("/home/u/a\0b/.ocx/bin", '\0'),
            (" /home/u/.ocx/bin", ' '),
            ("\t/home/u/.ocx/bin", '\t'),
            ("\"/home/u/.ocx/bin", '"'),
            ("'/home/u/.ocx/bin", '\''),
        ] {
            assert_eq!(
                refused_character(directory),
                offender,
                "environment.d must refuse {offender:?} in {directory:?}"
            );
        }
    }

    /// S-015 / C-037: the refusal names the path **and** the format, and
    /// classifies as exit 78.
    #[test]
    fn an_environment_d_refusal_names_the_path_and_the_format() {
        use crate::cli::ClassifyExitCode as _;

        let error = refusal("/home/u/a$b/.ocx/bin");
        let rendered = error.to_string();
        assert!(
            rendered.contains("/home/u/a$b/.ocx/bin"),
            "the refusal must name the path: {rendered}"
        );
        assert!(
            rendered.contains(&SessionPathFormat::EnvironmentD.to_string()),
            "the refusal must name the format: {rendered}"
        );
        assert_eq!(error.classify(), Some(crate::cli::ExitCode::ConfigError));
    }

    /// The load-bearing **negative** (E-L10, E-M13's Linux sibling): an
    /// over-broad refusal set breaks ordinary home directories.
    ///
    /// A space away from the front is legal in a `KEY=VALUE` value; `%` is
    /// admissible in a store root by design; `'`, `"`, `&`, `<`, `>`, `#` and
    /// `;` are all inert to a systemd `environment.d` parser away from the
    /// first position, which is not a shell. `=` is legal **inside the value**
    /// because the format splits on the *first* `=` only. Vertical tab and
    /// form feed lead a value untouched — they are outside systemd's
    /// `WHITESPACE`, so the leading-whitespace refusal stops at space and tab
    /// rather than covering `char::is_whitespace`.
    ///
    /// Every row was observed surviving the real
    /// `30-systemd-environment-d-generator` unchanged, which is what keeps
    /// this list honest against a refusal set that grew by guessing.
    #[test]
    fn environment_d_accepts_a_directory_it_can_carry() {
        for directory in [
            "/home/u/.ocx/toolchain/bin",
            "/home/First Last/.ocx/toolchain/bin",
            "/home/u/100%real/.ocx/bin",
            "/home/u/a=b/.ocx/bin",
            "/home/u/a'b/.ocx/bin",
            "/home/u/a\"b/.ocx/bin",
            "/home/u/a&b/.ocx/bin",
            "/home/u/a<b>c/.ocx/bin",
            "/home/u/a#b;c/.ocx/bin",
            "/home/u/trail /.ocx/bin",
            "\u{b}/home/u/vtab/.ocx/bin",
            "\u{c}/home/u/formfeed/.ocx/bin",
            "/home/u/ünïcode/.ocx/bin",
        ] {
            assert_eq!(
                encode(Path::new(directory)).expect("a representable directory must be accepted"),
                directory,
                "{directory:?} is representable in an environment.d value"
            );
        }
    }

    /// E-L10, corrected: a **leading** space is refused, and an interior or
    /// trailing one is not.
    ///
    /// The distinction is the parser's, not ours. systemd's `PRE_VALUE` state
    /// skips `WHITESPACE` before the value starts, so a directory written with
    /// a leading space is read back without it — the session PATH would then
    /// name a directory that does not exist, silently, on every login. Away
    /// from the front the same space survives byte-for-byte, so refusing it
    /// there would break a legal `$OCX_HOME` for nothing.
    ///
    /// Pinned as a test rather than left to the [`REFUSED_LEADING`] doc alone
    /// because both halves are tempting to get wrong in opposite directions,
    /// and a doc line does not red.
    #[test]
    fn a_leading_space_is_refused_and_an_interior_one_is_not() {
        for directory in [
            " /home/u/.ocx/toolchain/bin",
            "  /home/u/lead/.ocx/bin",
            "\t/home/u/tab/.ocx/bin",
        ] {
            assert_eq!(
                refused_character(directory),
                directory.chars().next().expect("the fixture is not empty"),
                "{directory:?} leads with whitespace this format strips"
            );
        }
        for directory in ["/home/u/trail /.ocx/bin", "/home/First Last/.ocx/bin"] {
            assert_eq!(
                encode(Path::new(directory)).expect("a space away from the front is representable"),
                directory,
                "{directory:?} must survive verbatim, spaces included"
            );
        }
        assert_eq!(
            render_conf(&[PathBuf::from("/home/u/trail /.ocx/bin")])
                .expect("representable")
                .trim_end_matches('\n'),
            "PATH=/home/u/trail /.ocx/bin:$PATH",
            "the space reaches the file rather than being trimmed on the way"
        );
    }

    // ── C-039: the rendered file ─────────────────────────────────────────

    fn directories() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/home/u/.ocx/symlinks/abc/current/content/bin"),
            PathBuf::from("/home/u/.ocx/toolchain/bin"),
        ]
    }

    /// C-039 / C-060 / E-L12: the value is a **prepend**, and the install bin
    /// directory leads the toolchain bin directory.
    ///
    /// The prepend is the assertion that reds if the writer emits an append:
    /// `PATH=$PATH:…` puts every session directory ahead of OCX's, which is the
    /// difference between a toolchain that resolves and one that is shadowed by
    /// `/usr/bin`.
    #[test]
    fn the_conf_prepends_both_directories_ahead_of_the_session_value() {
        let rendered = render_conf(&directories()).expect("representable");
        assert_eq!(
            rendered.trim_end_matches('\n'),
            "PATH=/home/u/.ocx/symlinks/abc/current/content/bin:/home/u/.ocx/toolchain/bin:$PATH",
            "rendered: {rendered:?}"
        );
        assert!(
            !rendered.contains("PATH=$PATH"),
            "an append would put every session directory ahead of OCX's: {rendered:?}"
        );
    }

    /// C-039: the file is line-structured, so it carries exactly one
    /// assignment. A second line would declare a variable nobody asked for —
    /// the same defect the `\n` refusal guards from the value side.
    #[test]
    fn the_conf_carries_exactly_one_assignment() {
        let rendered = render_conf(&directories()).expect("representable");
        let lines: Vec<&str> = rendered.lines().filter(|line| !line.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "rendered: {rendered:?}");
        assert_eq!(
            rendered.matches('=').count(),
            1,
            "one `=`, so the split-on-first-`=` rule has one answer: {rendered:?}"
        );
    }

    /// C-037: a refused directory stops the render, so no partial text can
    /// reach the file.
    #[test]
    fn a_refused_directory_stops_the_render() {
        assert!(matches!(
            render_conf(&[PathBuf::from("/home/u/a$b/.ocx/bin")]),
            Err(SessionPathError::Unencodable { character: '$', .. })
        ));
    }

    // ── C-039 / C-036: the writer, on its own platform ───────────────────

    #[cfg(target_os = "linux")]
    fn tmp_home(root: &std::path::Path) -> HomeEnv {
        HomeEnv {
            home: root.join("home"),
            zdotdir: None,
            xdg_config_home: Some(root.join("xdg")),
            xdg_data_home: None,
            ocx_home: root.join("ocx"),
            shell: None,
        }
    }

    /// C-039 / E-L2: `environment.d/` is created when absent — the atomic write
    /// helper does **not** create the parent, so a fresh machine would
    /// otherwise report `Failed` on its very first `ocx self setup`.
    #[cfg(target_os = "linux")]
    #[test]
    fn register_creates_the_directory_and_writes_the_conf() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = tmp_home(root.path());
        let expected = conf_path(&home);
        assert!(!expected.parent().expect("has a parent").exists());

        let (location, outcome) = register(&home, &directories(), false).expect("no refusal");

        assert_eq!(location, expected, "the outcome is keyed by the store it wrote");
        assert_eq!(outcome, SessionPathOutcome::Written);
        assert_eq!(
            std::fs::read_to_string(&expected).expect("the conf exists"),
            render_conf(&directories()).expect("representable")
        );
    }

    /// C-039 / S-002 / E-L4, item 4: a re-run against byte-identical content is
    /// `Unchanged` and does not disturb the file — the ensure-present shape,
    /// not a write-every-time.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_re_run_against_identical_content_is_unchanged() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = tmp_home(root.path());

        let (_, first) = register(&home, &directories(), false).expect("no refusal");
        assert_eq!(first, SessionPathOutcome::Written);
        let bytes = std::fs::read(conf_path(&home)).expect("the conf exists");
        let mtime = std::fs::metadata(conf_path(&home))
            .expect("stat")
            .modified()
            .expect("mtime");

        let (_, second) = register(&home, &directories(), false).expect("no refusal");
        assert_eq!(second, SessionPathOutcome::Unchanged, "a second run must not rewrite");
        assert_eq!(std::fs::read(conf_path(&home)).expect("the conf exists"), bytes);
        assert_eq!(
            std::fs::metadata(conf_path(&home))
                .expect("stat")
                .modified()
                .expect("mtime"),
            mtime,
            "an unchanged store must not be republished"
        );
    }

    /// C-039 / E-L5: the file is entirely ocx-owned, so content that differs —
    /// `$OCX_HOME` moved between runs — is replaced wholesale rather than
    /// merged.
    #[cfg(target_os = "linux")]
    #[test]
    fn content_from_an_earlier_run_is_replaced_wholesale() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = tmp_home(root.path());
        std::fs::create_dir_all(conf_path(&home).parent().expect("has a parent")).expect("mkdir");
        std::fs::write(conf_path(&home), "PATH=/somewhere/else:$PATH\n").expect("seed a stale conf");

        let (_, outcome) = register(&home, &directories(), false).expect("no refusal");

        assert_eq!(outcome, SessionPathOutcome::Written);
        assert_eq!(
            std::fs::read_to_string(conf_path(&home)).expect("the conf exists"),
            render_conf(&directories()).expect("representable"),
            "a stale value must not be merged into, it must be replaced"
        );
    }

    /// C-039 / E-L11 / CWE-732: the published file is owner-write only. The
    /// mode is asserted on the **published** path, not on a temp file, because
    /// the publish is what a later reader sees.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_published_conf_is_owner_writable_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().expect("tempdir");
        let home = tmp_home(root.path());
        register(&home, &directories(), false).expect("no refusal");

        let mode = std::fs::metadata(conf_path(&home))
            .expect("published")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode & 0o022,
            0,
            "group- and world-writable bits must be clear, got {mode:o}"
        );
    }

    /// C-036's `--dry-run` rule: a dry run reports the outcome the write
    /// **would** have produced, and never [`SessionPathOutcome::Failed`],
    /// because nothing was attempted.
    ///
    /// Asserted against the very obstruction that makes a real run `Failed`,
    /// so the two answers are pinned against one another. A dry run asserted
    /// alone would pass for a writer that reported `Failed` in both modes on a
    /// healthy store — the mode would never be what the answer turned on.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_dry_run_never_reports_failed_even_where_a_real_run_would() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = tmp_home(root.path());
        let parent = conf_path(&home).parent().expect("has a parent").to_path_buf();
        std::fs::create_dir_all(parent.parent().expect("xdg root")).expect("mkdir");
        std::fs::write(&parent, "not a directory").expect("obstruct environment.d");

        let (_, predicted) = register(&home, &directories(), true).expect("no refusal");
        assert_eq!(
            predicted,
            SessionPathOutcome::Written,
            "the store does not carry the value yet, so the write would change it"
        );

        let (_, attempted) = register(&home, &directories(), false).expect("no refusal");
        assert_eq!(
            attempted,
            SessionPathOutcome::Failed,
            "the same obstruction fails a real run: the mode is what the answer turns on"
        );
    }

    /// **C-036's never-block rule at the writer** (E-L3): a store that cannot
    /// be written is [`SessionPathOutcome::Failed`] carried in the `Ok` arm —
    /// never an `Err`, and never a non-zero exit. Registering a session PATH is
    /// a convenience; failing the whole install over it is not.
    ///
    /// The obstruction is a **file** where `environment.d/` must be a
    /// directory, so `create_dir_all` fails for `root` as surely as for anyone
    /// else. A read-only parent would be bypassed by uid 0 and the test would
    /// then pass without ever having observed the failure it names.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_store_that_cannot_be_written_is_failed_and_not_an_error() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = tmp_home(root.path());
        let parent = conf_path(&home).parent().expect("has a parent").to_path_buf();
        std::fs::create_dir_all(parent.parent().expect("xdg root")).expect("mkdir");
        std::fs::write(&parent, "not a directory").expect("obstruct environment.d");

        let (location, outcome) = register(&home, &directories(), false)
            .expect("a write failure must not be an Err: C-036 makes it an outcome with exit 0");

        assert_eq!(outcome, SessionPathOutcome::Failed);
        assert_eq!(
            location,
            conf_path(&home),
            "a Failed still names the store it could not write"
        );
    }

    /// C-036: `--dry-run` computes the outcome and writes no byte.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_dry_run_writes_no_byte() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = tmp_home(root.path());

        let (location, _) = register(&home, &directories(), true).expect("no refusal");

        assert_eq!(location, conf_path(&home));
        assert!(!conf_path(&home).exists(), "a dry run must not create the conf");
        assert!(
            !conf_path(&home).parent().expect("has a parent").exists(),
            "a dry run must not create environment.d/ either"
        );
    }

    /// C-037 on the **remove** path: a directory that could never have been
    /// written is diagnosed as such rather than reported as a successful
    /// removal, and the store is left exactly as it was found.
    #[cfg(target_os = "linux")]
    #[test]
    fn deregistration_refuses_an_unencodable_directory_before_touching_the_store() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = tmp_home(root.path());
        register(&home, &directories(), false).expect("no refusal");
        let before = std::fs::read(conf_path(&home)).expect("the conf exists");

        let error = deregister(&home, &[PathBuf::from("/home/u/a$b/.ocx/bin")], false)
            .expect_err("an unencodable directory must be refused");

        assert!(matches!(error, SessionPathError::Unencodable { character: '$', .. }));
        assert_eq!(
            std::fs::read(conf_path(&home)).expect("the conf exists"),
            before,
            "a refused deregistration must leave the store byte-identical"
        );
    }

    /// **S-014, the survivor half.** Deregistration deletes the file this
    /// module owns and touches nothing else — not a sibling drop-in, not
    /// `~/.profile`, whose managed block belongs to `setup::rc_block`.
    ///
    /// The foreign drop-in is asserted **present afterwards**, by content.
    /// Asserting only that `ocx.conf` is gone would pass for an implementation
    /// that cleared the whole `environment.d/` directory.
    #[cfg(target_os = "linux")]
    #[test]
    fn deregistration_removes_only_our_own_conf() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = tmp_home(root.path());
        register(&home, &directories(), false).expect("no refusal");

        let foreign = conf_path(&home)
            .parent()
            .expect("has a parent")
            .join("50-someone-else.conf");
        std::fs::write(&foreign, "PATH=/opt/foreign/bin:$PATH\n").expect("plant a foreign drop-in");
        let profile = home.home.join(".profile");
        std::fs::create_dir_all(&home.home).expect("mkdir");
        std::fs::write(&profile, "# managed by ocx\n").expect("plant a profile");

        let (location, outcome) = deregister(&home, &directories(), false).expect("no refusal");

        assert_eq!(outcome, SessionPathOutcome::Removed);
        assert_eq!(location, conf_path(&home));
        assert!(!conf_path(&home).exists(), "our own conf must be gone");
        assert_eq!(
            std::fs::read_to_string(&foreign).expect("the foreign drop-in survives"),
            "PATH=/opt/foreign/bin:$PATH\n",
            "only OCX's own segments go; a foreign one planted beforehand survives"
        );
        assert!(
            profile.exists(),
            "the profile fence belongs to rc_block, not to this writer"
        );
    }

    /// S-014 / C-036: removal is a **no-op** when the store does not carry our
    /// segments. `Removed` is not the unconditional answer — absence is
    /// [`SessionPathOutcome::Unchanged`], and it is not an error either.
    #[cfg(target_os = "linux")]
    #[test]
    fn deregistration_is_unchanged_when_nothing_was_registered() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = tmp_home(root.path());

        let (_, outcome) = deregister(&home, &directories(), false).expect("an absent store is not an error");

        assert_eq!(outcome, SessionPathOutcome::Unchanged);
    }

    /// C-036: a dry-run deregistration reports without deleting.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_dry_run_deregistration_deletes_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = tmp_home(root.path());
        register(&home, &directories(), false).expect("no refusal");

        deregister(&home, &directories(), true).expect("no refusal");

        assert!(conf_path(&home).exists(), "a dry run must not delete the conf");
    }
}
