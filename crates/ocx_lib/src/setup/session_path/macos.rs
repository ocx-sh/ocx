// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The macOS session-PATH writer: a `RunAtLoad` LaunchAgent.
//!
//! The agent's `ProgramArguments` are `/bin/sh -c <merge script>`, **not**
//! `launchctl setenv PATH <literal>`. launchd runs no shell of its own, so a
//! plist argument reaches `execve` verbatim and a `$PATH` inside it is four
//! literal characters. Baking a composed literal at `ocx self setup` time
//! would freeze whatever PATH existed *that day* and replay it at every
//! subsequent login, silently reverting anything another tool changed in
//! between. The `/bin/sh -c` form makes the value a function of the
//! **then-current** session instead: the script reads
//! `launchctl getenv PATH` at load, removes any prior occurrence of the two
//! directories, and re-inserts them in front.
//!
//! **Each directory appears exactly once in the script, single-quoted, and
//! every later use is a `"$var"` expansion.** That binding is the whole safety
//! argument: inside `'…'` nothing expands, and `"$b"` expands the variable
//! without re-scanning its value, so a `$`, a backtick or a `\` in `$OCX_HOME`
//! is inert at both sites. Interpolating a directory directly into the final
//! double-quoted word would re-open the injection in a second place, because a
//! double-quoted word *does* expand `$` and backtick.
//!
//! # The plist is a template, not a serializer
//!
//! Per D-V2/C-041 the plist is a **constant template with two validated
//! interpolations and no escaper**, and the refusal set widens accordingly to
//! `&`, `<`, `>` and every XML-1.0-forbidden control character.
//! `quality-core.md` rates hand-emitting an external wire format Block-tier —
//! and it is right to, which is why nothing here *emits* XML: the document has
//! zero dynamic structure, and the only variability is two text runs whose
//! character set is refused down to one that needs no escaping. Stated
//! honestly: unlike the sibling writers, which refuse the *unrepresentable*,
//! this refuses the *representable*. It is a deliberate narrowing, and
//! launchd's failure mode is what justifies it — a malformed plist is a
//! **silent** non-load, so a refusal naming the directory is the only
//! diagnosable outcome available.
//!
//! Two limits are stated rather than worked around: `launchctl setenv` reaches
//! processes started afterwards, never a GUI application already running; and
//! another tool that calls `launchctl setenv PATH` later in the same session
//! simply wins by running last.

use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use super::SessionPathOutcome;
use super::{SessionPathError, SessionPathFormat};
use crate::setup::profiles::HomeEnv;

/// The LaunchAgent's label, and the stem of its plist.
pub const AGENT_LABEL: &str = "sh.ocx.path";

/// The plist this writer owns, relative to the user's home directory.
pub const PLIST_RELATIVE_PATH: &str = "Library/LaunchAgents/sh.ocx.path.plist";

/// The mode the plist must carry.
///
/// launchd refuses a LaunchAgent with "dubious permissions" and its failure
/// mode is a **silent non-load**, so this is load-bearing rather than hygiene
/// and needs its own test rather than a code review. Group- and
/// world-*writable* is what launchd objects to; 0644 is the mode it accepts.
pub const PLIST_MODE: u32 = 0o644;

/// Characters the plist and its embedded script cannot carry, checked in this
/// order (the first match is the one the refusal names).
///
/// - `'` — the script single-quotes each directory, and a single-quoted shell
///   word has no escape for a `'` inside it.
/// - `"` — refused with `'` so the two quoting characters have one answer
///   between them.
/// - `&`, `<`, `>` — XML markup. Under D-V2 they are refused rather than
///   escaped, because escaping them would mean owning an XML emitter.
/// - `\n`, `\r` — a newline ends the script line; `\r` is additionally
///   rewritten to `\n` by XML 1.0 line-end normalisation (§2.11), so a
///   directory containing one would be *silently renamed* by any conforming
///   parser rather than merely refused.
/// - `:` — the PATH delimiter. A segment containing one is two segments to
///   every reader.
/// - `\0` — forbidden by XML 1.0 and unrepresentable in a `sh` argument.
///
/// Every character XML 1.0 forbids outright is refused too — see
/// [`xml_forbids`], which covers the control bytes this table does not name
/// individually.
pub const REFUSED: &[(char, &str)] = &[
    ('\'', "and a single-quoted shell word has no escape for it"),
    ('"', "which the plist's quoting cannot carry"),
    ('&', "which is XML markup this writer escapes nothing for"),
    ('<', "which is XML markup this writer escapes nothing for"),
    ('>', "which is XML markup this writer escapes nothing for"),
    ('\n', "which ends the line the merge script occupies"),
    (
        '\r',
        "which XML line-end normalisation would silently rewrite to a newline",
    ),
    (':', "which is the PATH delimiter and would split the entry in two"),
    ('\0', "which XML 1.0 forbids outright"),
];

/// `~/Library/LaunchAgents/sh.ocx.path.plist`.
pub fn plist_path(home: &HomeEnv) -> PathBuf {
    home.home.join(PLIST_RELATIVE_PATH)
}

/// Whether XML 1.0 forbids `character` in document content.
///
/// The `Char` production, verbatim: `#x9 | #xA | #xD | [#x20-#xD7FF] |
/// [#xE000-#xFFFD] | [#x10000-#x10FFFF]`. Everything outside it is forbidden —
/// which is every C0 control except tab, newline and carriage return, plus
/// U+FFFE and U+FFFF.
///
/// Two boundaries are worth stating because they are the ones that get guessed
/// wrong. **U+007F is allowed**: XML 1.0 permits the whole `#x20-#xD7FF` range,
/// so DEL and the C1 controls are legal here (XML *1.1* is the one that
/// restricts them, and a plist is XML 1.0). And the D800–DFFF surrogate gap
/// needs no arm at all: a Rust `char` can never hold one, so a branch for it
/// would be unreachable rather than defensive.
pub fn xml_forbids(character: char) -> bool {
    !matches!(
        character,
        '\u{9}' | '\u{a}' | '\u{d}' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}'
    )
}

/// Refuse `directory` when the plist or its script cannot carry it, else yield
/// its UTF-8 spelling.
///
/// # Errors
///
/// [`SessionPathError::NotUtf8`] for a non-UTF-8 directory;
/// [`SessionPathError::Unencodable`] naming the first character of [`REFUSED`]
/// the directory carries, or the first character [`xml_forbids`] rejects.
pub fn encode(directory: &Path) -> Result<&str, SessionPathError> {
    let spelled = super::encodable_str(directory, SessionPathFormat::LaunchAgentPlist)?;
    let refusal = |character: char, reason: &'static str| SessionPathError::Unencodable {
        path: directory.to_path_buf(),
        format: SessionPathFormat::LaunchAgentPlist,
        character,
        reason,
    };

    if let Some((character, reason)) = REFUSED.iter().find(|(character, _)| spelled.contains(*character)) {
        return Err(refusal(*character, reason));
    }
    // The table names the characters a *reader* would guess at; this catches
    // every remaining one XML 1.0 has no representation for, so a control byte
    // nobody thought to list is refused rather than written into a document
    // launchd then declines to load without saying why.
    match spelled.chars().find(|character| xml_forbids(*character)) {
        Some(character) => Err(refusal(character, "which XML 1.0 forbids outright")),
        None => Ok(spelled),
    }
}

/// The `/bin/sh -c` merge script the agent runs at every load.
///
/// Shape, with each directory appearing exactly once, single-quoted:
///
/// ```sh
/// __OCX_TESTING_LAUNCHCTL=${__OCX_TESTING_LAUNCHCTL:-/bin/launchctl}
/// launchctl() { "$__OCX_TESTING_LAUNCHCTL" "$@"; }
/// d0='<dir0>'
/// d1='<dir1>'
/// cur=$(launchctl getenv PATH)
/// [ -n "$cur" ] || cur=/usr/bin:/bin:/usr/sbin:/sbin
/// out=$(printf '%s\n' "$cur" | tr ':' '\n' | grep -vxF -e "$d0" | grep -vxF -e "$d1" | paste -sd: -)
/// launchctl setenv PATH "$d0:$d1${out:+:$out}"
/// ```
///
/// The remove-then-prepend is what makes a second load a no-op instead of an
/// accumulating prefix, and everything the pipeline does not match is copied
/// through unchanged — so a tool that appended to the GUI PATH after the agent
/// was installed keeps its entry across the next login.
///
/// # The `__OCX_TESTING_LAUNCHCTL` seam
///
/// The first two lines are a **testability seam, and nothing else**: the
/// default is the absolute `/bin/launchctl` the agent runs under, so on a real
/// login the script behaves exactly as if the path were written inline. It
/// exists because ADR item 6 — that the composed PATH is a function of the
/// *then-current* session value rather than a setup-time snapshot — is only
/// provable by running this script, and the only other way to run it off macOS
/// is to rewrite a literal in the generated text, which tests the rewriting
/// rather than the script. launchd sets no `__OCX_TESTING_LAUNCHCTL` in an agent's
/// environment, so the default is what loads.
///
/// The `__OCX_TESTING_` prefix is this repository's convention for a seam that
/// is not product surface (D-V27), and it matters more here than it usually
/// does: this is an **environment-controlled program path in a script launchd
/// runs at every login**, so a plausible-looking name like `LAUNCHCTL` is one
/// `launchctl setenv` away from redirecting the binary the login agent
/// executes. The prefix makes the variable read as non-product to anyone
/// auditing the plist, and makes it something nobody sets by accident.
///
/// The indirection through a function keeps the two invocations spelled
/// `launchctl getenv PATH` and `launchctl setenv PATH` — the vocabulary the
/// contract and the manual-removal recipe both use, and what a user reading
/// the plist expects to find.
///
/// # Errors
///
/// [`SessionPathError`] from [`encode`], for the first directory that cannot
/// be encoded.
pub fn merge_script(directories: &[PathBuf]) -> Result<String, SessionPathError> {
    let mut assignments = String::new();
    let mut filters = String::new();
    let mut prefix = String::new();

    for (index, directory) in directories.iter().enumerate() {
        let spelled = encode(directory)?;
        // Single-quoted, exactly once. Every later use is `"$dN"`, which
        // expands the variable without re-scanning its value — so a `$`, a
        // backtick or a `\` in `$OCX_HOME` is inert at both sites.
        assignments.push_str(&format!("d{index}='{spelled}'\n"));
        // `-e` before the operand: without it a directory beginning with `-`
        // is parsed by `grep` as an option rather than a pattern, so the
        // dedup filters the wrong segment (or, for `-f…`, reads a file) and
        // the remove-then-prepend accumulates a duplicate at every login.
        // `encodable_str` already refuses a relative directory, which covers
        // this case today — this keeps the emitted script correct on its own
        // terms rather than by a guard three modules away.
        filters.push_str(&format!(" | grep -vxF -e \"$d{index}\""));
        if index > 0 {
            prefix.push(':');
        }
        prefix.push_str(&format!("$d{index}"));
    }

    // With no directories there is no prefix to prepend, and emitting the
    // `"<prefix>${out:+:$out}"` form anyway would set PATH to a value with a
    // leading `:` — the working directory, to every reader.
    let composed = if prefix.is_empty() {
        "\"$out\"".to_owned()
    } else {
        format!("\"{prefix}${{out:+:$out}}\"")
    };

    Ok(format!(
        "__OCX_TESTING_LAUNCHCTL=${{__OCX_TESTING_LAUNCHCTL:-{LAUNCHCTL_BINARY}}}\n\
         launchctl() {{ \"$__OCX_TESTING_LAUNCHCTL\" \"$@\"; }}\n\
         {assignments}\
         cur=$(launchctl getenv PATH)\n\
         [ -n \"$cur\" ] || cur={SYSTEM_DEFAULT_PATH}\n\
         out=$(printf '%s\\n' \"$cur\" | tr ':' '\\n'{filters} | paste -sd: -)\n\
         launchctl setenv PATH {composed}\n"
    ))
}

/// The `launchctl` the agent runs, and the default of the script's seam.
pub const LAUNCHCTL_BINARY: &str = "/bin/launchctl";

/// What the merge script composes onto when `launchctl getenv PATH` is unset.
///
/// Without it a fresh login would get a PATH holding OCX's two directories and
/// nothing else — no `sh`, no `ls` — for every GUI application launched
/// afterwards.
pub const SYSTEM_DEFAULT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// The whole plist: a constant template with exactly two interpolations —
/// [`AGENT_LABEL`] and the [`merge_script`] output — and no escaper (D-V2).
///
/// # Errors
///
/// [`SessionPathError`] from [`merge_script`].
pub fn render_plist(directories: &[PathBuf]) -> Result<String, SessionPathError> {
    let script = merge_script(directories)?;
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{AGENT_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/sh</string>
        <string>-c</string>
        <string>{script}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#
    ))
}

/// Publish `contents` to `path` and leave it at [`PLIST_MODE`].
///
/// **The mode is the whole reason this function exists.** Every shipped setup
/// writer publishes through [`crate::utility::fs::write_bytes_atomic`], which
/// creates its temp file `0o600` — a private-file contract its own doc states
/// explicitly and which is right for every other file `ocx self setup` writes.
/// A LaunchAgent at 0600 is what launchd calls "dubious permissions", and it
/// refuses to load it **silently**: no error, no log the user will find, just a
/// PATH that never changes. So the atomic publish is kept — a plain
/// `std::fs::write` would follow a symlink at the target and truncate it — and
/// the mode is set on the **temp file, before the rename**.
///
/// Ordering, not style: a `set_permissions` *after* the publish leaves the
/// plist briefly live at its final path at 0600, and `set_permissions`
/// follows symlinks — so a same-uid process replacing the file in that window
/// gets an arbitrary file chmod'd 0644. Same uid throughout, so no privilege
/// boundary is crossed, but the window has no reason to exist: `tempfile`
/// takes the mode at creation, and after that the rename publishes a file
/// that was never at the wrong mode.
///
/// `cfg(unix)` rather than `cfg(target_os = "macos")`: nothing here is
/// macOS-specific, and gating it to macOS would make the one rule whose failure
/// mode is silence observable only on the platform where it is hardest to test.
/// On Linux this runs, and its test reds there.
///
/// # Errors
///
/// Any I/O failure from the publish or the mode change. The caller maps it to
/// [`SessionPathOutcome::Failed`]; it never becomes a
/// [`SessionPathError`].
#[cfg(unix)]
pub fn publish_plist(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "plist path has no parent"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    // `File::set_permissions` on the open handle, not `Builder::permissions`:
    // the builder passes the mode to `open(2)`, where the umask clips it, so a
    // user at `umask 077` would publish a 0600 plist launchd silently refuses
    // — and `plist_is_current` would then agree with it forever. An `fchmod`
    // on the fd is neither umask-masked nor symlink-following.
    tmp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(PLIST_MODE))?;
    tmp.write_all(contents.as_bytes())?;
    crate::utility::fs::persist_temp_file(tmp, path)
}

/// Run `launchctl` with `arguments` and hand back its whole result.
///
/// The one place this module starts a process. Its callers decide what a
/// non-zero exit means, because that differs per subcommand: a failed
/// `setenv` is a genuine failure, while a `bootout` of an agent that was never
/// loaded is the ordinary state after a manual removal.
#[cfg(target_os = "macos")]
fn launchctl<I, S>(arguments: I) -> std::io::Result<std::process::Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    std::process::Command::new(LAUNCHCTL_BINARY).args(arguments).output()
}

/// The current session-wide `PATH`, as `launchctl getenv PATH` reports it.
///
/// `Ok(None)` when there is nothing to compose against — the variable is
/// unset, or `launchctl` itself declined the read. The two are deliberately
/// **not** distinguished: `getenv` of an unset variable is not a documented
/// exit status, so treating a non-zero exit as an error would make the
/// ordinary fresh-login state a failure on some OS versions. The only caller
/// subtracts from what comes back, and an empty answer means it subtracts
/// nothing — never that it clears anything.
///
/// An `OsString` rather than a `String`: a PATH that is not valid UTF-8 is
/// still a PATH, and `to_string_lossy` here would substitute U+FFFD into
/// somebody else's segment and then write the corrupted value back.
#[cfg(target_os = "macos")]
fn launchctl_getenv_path() -> std::io::Result<Option<std::ffi::OsString>> {
    use std::os::unix::ffi::OsStringExt as _;

    let output = launchctl(["getenv", "PATH"])?;
    if !output.status.success() {
        return Ok(None);
    }
    let mut bytes = output.stdout;
    while bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    Ok((!bytes.is_empty()).then(|| std::ffi::OsString::from_vec(bytes)))
}

/// Set the session-wide `PATH` to `value`.
///
/// **`launchctl unsetenv PATH` is forbidden** and has no wrapper here on
/// purpose: it deletes the entire session-wide value rather than ocx's
/// contribution to it, stripping every other tool's segment from every GUI
/// application launched afterwards. A deregistration whose remainder is empty
/// writes nothing at all rather than reaching for it.
#[cfg(target_os = "macos")]
fn launchctl_setenv_path(value: &std::ffi::OsStr) -> std::io::Result<()> {
    let output = launchctl([std::ffi::OsStr::new("setenv"), std::ffi::OsStr::new("PATH"), value])?;
    if output.status.success() {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "launchctl setenv PATH exited with {}",
        output.status
    )))
}

/// Boot the agent out of the current GUI domain.
///
/// `launchctl bootout gui/<uid>/sh.ocx.path` — a **service** target, not the
/// bare domain target `gui/<uid>`. The plan's C-040 text spells the latter;
/// that spelling boots out the user's entire GUI domain, which ends their
/// login session. The service target is what removes this one agent, and it is
/// what this writer uses.
///
/// `bootout` rather than the deprecated `launchctl unload`. An agent that is
/// not loaded is not an error — bootout of an absent service is the ordinary
/// state after a manual removal — so this reports the exit status rather than
/// judging it, and every caller ignores it deliberately.
#[cfg(target_os = "macos")]
fn boot_agent_out(uid: u32) -> std::io::Result<std::process::Output> {
    launchctl(["bootout".to_owned(), format!("gui/{uid}/{AGENT_LABEL}")])
}

/// The uid naming the GUI domain this process can address.
///
/// `launchctl`'s `gui/<uid>` is the caller's own login session, so the answer
/// comes from the process's own credentials rather than from `$HOME` or
/// `$USER` — the same reason `record::environment::user_id` reaches for
/// `geteuid` instead of an environment variable.
#[cfg(target_os = "macos")]
fn gui_domain_uid() -> u32 {
    // SAFETY: `getuid` reads the calling process's own credentials. It takes no
    // arguments, touches no memory, and is documented as always succeeding.
    unsafe { libc::getuid() }
}

/// Write the LaunchAgent plist at [`PLIST_MODE`], creating `LaunchAgents/`
/// when absent.
///
/// Ensure-present rather than write-once: rewritten when the rendered bytes
/// differ, left untouched when they already match
/// ([`SessionPathOutcome::Unchanged`]).
///
/// The publish itself is [`publish_plist`], which owns the 0644 rule.
///
/// # Errors
///
/// [`SessionPathError`] from [`render_plist`]. An I/O or `launchctl` failure
/// is [`SessionPathOutcome::Failed`], never an error.
#[cfg(target_os = "macos")]
pub(crate) fn register(
    home: &HomeEnv,
    directories: &[PathBuf],
    dry_run: bool,
) -> Result<(PathBuf, SessionPathOutcome), SessionPathError> {
    // The one `?` on this path, and it runs before a byte is read or written:
    // from here down every failure is an outcome (C-036).
    let contents = render_plist(directories)?;
    let path = plist_path(home);

    if plist_is_current(&path, &contents) {
        return Ok((path, SessionPathOutcome::Unchanged));
    }
    if dry_run {
        return Ok((path, SessionPathOutcome::Written));
    }
    let outcome = match write_and_load(&path, &contents) {
        Ok(()) => SessionPathOutcome::Written,
        Err(error) => {
            // Reported, not swallowed: the user-facing half is the caller's
            // warning, and this is the cause behind it.
            tracing::debug!(path = ?path, %error, "LaunchAgent publish failed");
            SessionPathOutcome::Failed
        }
    };
    Ok((path, outcome))
}

/// Whether the plist on disk is already what this run would publish — **bytes
/// and mode both**.
///
/// The mode belongs in the comparison because launchd's objection to a
/// non-0644 agent is a silent non-load: a plist whose content is right and
/// whose mode is wrong is a broken installation that a content-only check
/// would report as `Unchanged` forever.
#[cfg(target_os = "macos")]
fn plist_is_current(path: &Path, contents: &str) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o777 == PLIST_MODE)
        && std::fs::read(path).is_ok_and(|bytes| bytes == contents.as_bytes())
}

/// Publish the plist and load it into the current GUI domain.
///
/// The load is what makes `ocx self setup` take effect in this login session
/// rather than only at the next one. It is a boot-out-then-bootstrap pair
/// because `bootstrap` refuses a service that is already loaded, and the
/// boot-out of an agent that was never loaded is not a failure (E-M15).
#[cfg(target_os = "macos")]
fn write_and_load(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    publish_plist(path, contents)?;

    let uid = gui_domain_uid();
    // Deliberately ignored: an agent that is not loaded is the ordinary state
    // on a first run, and `bootout` reports that as a non-zero exit.
    let _ = boot_agent_out(uid);

    let domain = std::ffi::OsString::from(format!("gui/{uid}"));
    let loaded = launchctl([std::ffi::OsStr::new("bootstrap"), &domain, path.as_os_str()])?;
    if !loaded.status.success() {
        // Not a failure of the publish: `RunAtLoad` still composes the PATH at
        // the next login, so the store is correct either way. Recorded because
        // it is the difference between "takes effect now" and "takes effect
        // after you log in again".
        tracing::debug!(status = %loaded.status, "launchctl bootstrap of the LaunchAgent did not load it now");
    }
    Ok(())
}

/// Boot the agent out, delete the plist, then subtract the two directories
/// from the live session value.
///
/// Order matters: the agent is booted out first so it cannot re-add the
/// directories between the delete and the `setenv`. The `setenv` writes the
/// remainder — never `unsetenv` — and is skipped entirely when
/// `launchctl getenv PATH` comes back empty.
///
/// # Errors
///
/// [`SessionPathError`] from [`encode`] — the directories are still validated,
/// so a value that could never have been written is diagnosed as such rather
/// than reported as a successful removal.
#[cfg(target_os = "macos")]
pub(crate) fn deregister(
    home: &HomeEnv,
    directories: &[PathBuf],
    dry_run: bool,
) -> Result<(PathBuf, SessionPathOutcome), SessionPathError> {
    // The one `?` on this path, and it runs before the store is touched.
    let mut encoded = Vec::with_capacity(directories.len());
    for directory in directories {
        encoded.push(encode(directory)?);
    }
    let path = plist_path(home);

    if dry_run {
        // Both pieces of evidence the real run acts on, so the prediction
        // cannot disagree with it: the plist, and the live session value.
        // `symlink_metadata`, not `Path::exists` — a dangling symlink at the
        // store is present, it just does not resolve, and reporting it absent
        // would claim there was nothing to remove.
        let present = std::fs::symlink_metadata(&path).is_ok() || session_carries_any(&encoded);
        let outcome = if present {
            SessionPathOutcome::Removed
        } else {
            SessionPathOutcome::Unchanged
        };
        return Ok((path, outcome));
    }

    let outcome = match remove_agent(&path, &encoded) {
        Ok(true) => SessionPathOutcome::Removed,
        Ok(false) => SessionPathOutcome::Unchanged,
        Err(error) => {
            tracing::debug!(path = ?path, %error, "LaunchAgent removal failed");
            SessionPathOutcome::Failed
        }
    };
    Ok((path, outcome))
}

/// Boot the agent out, delete its plist, and subtract `directories` from the
/// live session value. `Ok(false)` when none of the three had anything to do.
///
/// The subtraction goes through [`crate::utility::path::remove_segment`], the
/// shipped inverse of the move-to-front the merge script performs, so the
/// segments this removes are exactly the ones registration added. What it does
/// **not** reach for is `launchctl` with a whole-variable verb: the remainder
/// is written back, and an empty read writes nothing at all.
///
/// **Only the plist can fail this function.** The store is the plist — the
/// live `launchctl` value is this session's copy of what the agent composed,
/// and it is gone at the next login either way. So a `launchctl` fault after
/// the plist has been deleted is a debug line, not an error: returning one
/// would report [`SessionPathOutcome::Failed`] for a store that *was* removed,
/// and the advisory behind that outcome tells the user to re-run
/// `ocx self setup` — a **registration** — as the remedy for a deregistration.
#[cfg(target_os = "macos")]
fn remove_agent(path: &Path, directories: &[&str]) -> std::io::Result<bool> {
    let uid = gui_domain_uid();
    // Deliberately ignored: an agent that is not loaded is the ordinary state
    // after a manual removal, and `bootout` reports that as a non-zero exit.
    let _ = boot_agent_out(uid);

    let changed = match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };

    let session_changed = match subtract_from_session(directories) {
        Ok(subtracted) => subtracted,
        Err(error) => {
            tracing::debug!(%error, "subtracting the ocx directories from the live session PATH failed");
            false
        }
    };
    Ok(changed || session_changed)
}

/// Subtract `directories` from the live `launchctl` PATH, writing back the
/// remainder and nothing else. `Ok(false)` when there was nothing to take away.
#[cfg(target_os = "macos")]
fn subtract_from_session(directories: &[&str]) -> std::io::Result<bool> {
    let Some(current) = launchctl_getenv_path()? else {
        return Ok(false);
    };
    let mut remainder = current.clone();
    for directory in directories {
        remainder = crate::utility::path::remove_segment(&remainder, std::ffi::OsStr::new(directory));
    }
    if remainder == current {
        return Ok(false);
    }
    launchctl_setenv_path(&remainder)?;
    Ok(true)
}

/// Whether the live session value still carries any of `directories`.
///
/// The half of the removal a `--dry-run` can observe without writing, so the
/// dry run predicts from the same two pieces of evidence the real run acts on
/// — the plist and the session value — rather than from the plist alone.
#[cfg(target_os = "macos")]
fn session_carries_any(directories: &[&str]) -> bool {
    let Ok(Some(current)) = launchctl_getenv_path() else {
        return false;
    };
    directories
        .iter()
        .any(|directory| crate::utility::path::remove_segment(&current, std::ffi::OsStr::new(directory)) != current)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C-040: the location and the label are the contract, and the plist stem
    /// is the label — so a reader who knows one knows the other.
    #[test]
    fn the_plist_is_the_label_under_launch_agents() {
        let home = HomeEnv {
            home: PathBuf::from("/Users/u"),
            zdotdir: None,
            xdg_config_home: None,
            xdg_data_home: None,
            ocx_home: PathBuf::from("/Users/u/.ocx"),
            shell: None,
        };
        assert_eq!(
            plist_path(&home),
            PathBuf::from("/Users/u/Library/LaunchAgents/sh.ocx.path.plist")
        );
        assert_eq!(
            PLIST_RELATIVE_PATH,
            format!("Library/LaunchAgents/{AGENT_LABEL}.plist"),
            "the plist stem is the agent label; two spellings would let them drift"
        );
    }

    /// C-040, the silent-failure rule: launchd refuses a LaunchAgent that is
    /// not 0644 by **not loading it**, so this asserts the published bits
    /// rather than the file's existence.
    ///
    /// The bits are what reds: the house atomic-write helper publishes 0600,
    /// which is right for every other file `ocx self setup` writes and wrong
    /// for exactly this one. Deleting the `set_permissions` line leaves the
    /// file present, the content correct, and the agent dead.
    #[cfg(unix)]
    #[test]
    fn the_published_plist_carries_the_mode_launchd_will_load() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sh.ocx.path.plist");
        publish_plist(&path, "<plist/>").expect("publish succeeds");

        let mode = std::fs::metadata(&path).expect("published file").permissions().mode() & 0o777;
        assert_eq!(
            mode, PLIST_MODE,
            "launchd silently refuses to load a plist that is not {PLIST_MODE:o}; got {mode:o}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("published file"),
            "<plist/>",
            "the mode change must not disturb the published bytes"
        );
    }

    // ── C-041 / D-V2: the refusal set ────────────────────────────────────

    fn refusal(directory: &str) -> SessionPathError {
        encode(Path::new(directory)).expect_err("the directory must be refused")
    }

    fn refused_character(directory: &str) -> char {
        match refusal(directory) {
            SessionPathError::Unencodable {
                character,
                format: SessionPathFormat::LaunchAgentPlist,
                ..
            } => character,
            other => panic!("expected an Unencodable refusal naming the plist format, got {other:?}"),
        }
    }

    /// C-041 / D-V2, items 5 and E-M7–E-M11: every character the plist and its
    /// single-quoted script cannot carry is refused **before** anything is
    /// written, and the refusal names that character.
    ///
    /// `&`, `<` and `>` are the deliberate divergence: the ADR (§ *Session
    /// PATH*, encoding table) XML-**escapes** them, and **D-V2 refuses them
    /// instead**, because escaping would mean owning an XML emitter that
    /// `quality-core.md` § *Don't Own Non-Domain Code* rates Block-tier for a
    /// wire format. **OQ-1 carries the reversal** — if a future reader "fixes"
    /// this back to the ADR's text, they are reopening OQ-1, not correcting a
    /// bug.
    ///
    /// `\r` is refused beside `\n` for a reason specific to XML rather than to
    /// `sh`: XML 1.0 §2.11 line-end normalisation rewrites a literal `\r` to
    /// `\n`, so a conforming parser would **silently rename** the directory
    /// rather than reject it.
    #[test]
    fn the_plist_refuses_every_character_it_cannot_carry() {
        for (directory, offender) in [
            ("/Users/o'brien/.ocx/bin", '\''),
            ("/Users/say\"what/.ocx/bin", '"'),
            ("/Users/a&b/.ocx/bin", '&'),
            ("/Users/a<b/.ocx/bin", '<'),
            ("/Users/a>b/.ocx/bin", '>'),
            ("/Users/a\nb/.ocx/bin", '\n'),
            ("/Users/a\rb/.ocx/bin", '\r'),
            ("/Users/a:b/.ocx/bin", ':'),
            ("/Users/a\0b/.ocx/bin", '\0'),
        ] {
            assert_eq!(
                refused_character(directory),
                offender,
                "the plist writer must refuse {offender:?} in {directory:?}"
            );
        }
    }

    /// S-015 / C-037: the refusal names **both** the offending path and the
    /// format, because the diagnostic's whole job is to tell the user which
    /// grammar their `$OCX_HOME` cannot be spelled in. Exit 78.
    #[test]
    fn a_plist_refusal_names_the_path_and_the_format() {
        use crate::cli::ClassifyExitCode as _;

        let error = refusal("/Users/a&b/.ocx/bin");
        let rendered = error.to_string();
        assert!(
            rendered.contains("/Users/a&b/.ocx/bin"),
            "the refusal must name the path: {rendered}"
        );
        assert!(
            rendered.contains(&SessionPathFormat::LaunchAgentPlist.to_string()),
            "the refusal must name the format: {rendered}"
        );
        assert_eq!(error.classify(), Some(crate::cli::ExitCode::ConfigError));
    }

    /// E-M13 / E-M14, the load-bearing **negative**: an over-broad refusal set
    /// breaks `/Users/First Last/.ocx`, which is an ordinary macOS home.
    ///
    /// `$`, a backtick and `\` are admitted deliberately (ADR § *Session
    /// PATH*): each directory appears exactly once in the script, single-quoted,
    /// and every later use is a `"$var"` expansion, so none of the three is ever
    /// re-scanned. `%` is admissible by design (`safety.rs`'s `100%real` test),
    /// and U+007F plus the C1 controls are legal in **XML 1.0** — XML 1.1 is
    /// the version that restricts them, and a plist is 1.0.
    #[test]
    fn the_plist_accepts_a_directory_it_can_carry() {
        for directory in [
            "/Users/u/.ocx/toolchain/bin",
            "/Users/First Last/.ocx/toolchain/bin",
            "/Users/a$b/.ocx/bin",
            "/Users/a`b/.ocx/bin",
            "/Users/a\\b/.ocx/bin",
            "/Users/100%real/.ocx/bin",
            "/Users/a\u{7f}b/.ocx/bin",
            "/Users/a\u{85}b/.ocx/bin",
            "/Users/ünïcode/.ocx/bin",
            "/Users/a\tb/.ocx/bin",
        ] {
            assert_eq!(
                encode(Path::new(directory)).expect("a representable directory must be accepted"),
                directory,
                "{directory:?} is representable in a single-quoted shell word inside XML 1.0"
            );
        }
    }

    /// E-M12: the XML 1.0 `Char` production, swept over the **whole** low range
    /// rather than a hand-listed subset — `quality-core.md`'s worked example is
    /// a hand-written emitter whose escape boundary was `> 0x7F` instead of
    /// `>= 0x7F`, with the unit test and the doc comment both affirming the
    /// wrong rule.
    ///
    /// The boundary pairs `#x8`/`#x9` and `#xD`/`#xE` are the ones that get
    /// guessed wrong, so they are asserted a second time by name below.
    #[test]
    fn xml_1_0_forbids_exactly_the_control_characters_outside_its_char_production() {
        for code in 0x00_u32..=0x20 {
            let character = char::from_u32(code).expect("a valid scalar value");
            let expected = !matches!(code, 0x9 | 0xA | 0xD | 0x20);
            assert_eq!(
                xml_forbids(character),
                expected,
                "U+{code:04X}: XML 1.0 permits #x9, #xA, #xD and #x20..#xD7FF and nothing below"
            );
        }

        assert!(!xml_forbids('\u{9}'), "#x9 is the first permitted control");
        assert!(xml_forbids('\u{8}'), "#x8 is the last forbidden one below it");
        assert!(!xml_forbids('\u{d}'), "#xD is permitted");
        assert!(xml_forbids('\u{e}'), "#xE is forbidden");

        assert!(
            !xml_forbids('\u{7f}'),
            "XML 1.0 permits the whole #x20-#xD7FF range, so U+007F is legal; XML 1.1 is the one that restricts it"
        );
        assert!(!xml_forbids('\u{85}'), "the C1 controls are legal in XML 1.0");
        assert!(!xml_forbids('\u{d7ff}'), "#xD7FF is the top of the first permitted run");
        assert!(!xml_forbids('\u{e000}'), "#xE000 opens the second permitted run");
        assert!(xml_forbids('\u{fffe}'), "#xFFFE is outside the Char production");
        assert!(xml_forbids('\u{ffff}'), "#xFFFF is outside the Char production");
        assert!(!xml_forbids('\u{10000}'), "the astral planes are permitted");
        assert!(
            !xml_forbids('\u{10ffff}'),
            "#x10FFFF is the top of the last permitted run"
        );
    }

    /// E-M12: a control character the [`REFUSED`] table does not name
    /// individually is still refused, through [`xml_forbids`]. Without this the
    /// table would be the whole rule and the sweep above would guard nothing
    /// that reaches a user.
    #[test]
    fn a_control_character_the_table_does_not_name_is_still_refused() {
        for offender in ['\u{1}', '\u{8}', '\u{b}', '\u{c}', '\u{e}', '\u{1f}'] {
            let directory = format!("/Users/a{offender}b/.ocx/bin");
            assert_eq!(
                refused_character(&directory),
                offender,
                "U+{:04X} is outside the XML 1.0 Char production and must be refused",
                u32::from(offender)
            );
        }
    }

    // ── C-040: the load-time merge script ────────────────────────────────

    fn directories() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/Users/u/.ocx/symlinks/abc/current/content/bin"),
            PathBuf::from("/Users/u/.ocx/toolchain/bin"),
        ]
    }

    /// C-040 / ADR § *Session PATH*: **each directory appears exactly once in
    /// the script, single-quoted, and every later use is a `"$var"`
    /// expansion.** That binding is the whole safety argument — inside `'…'`
    /// nothing expands, and `"$b"` expands the variable without re-scanning its
    /// value, so a `$`, a backtick or a `\` in `$OCX_HOME` is inert at both
    /// sites.
    ///
    /// A second occurrence — in particular inside the final double-quoted
    /// `setenv` word — is the exact regression an earlier ADR draft carried,
    /// and it re-opens command substitution on a directory the user chose.
    #[test]
    fn each_directory_appears_exactly_once_in_the_script_and_is_single_quoted() {
        let dirs = vec![
            PathBuf::from("/Users/a$b/.ocx/bin"),
            PathBuf::from("/Users/a`b/.ocx/tc/bin"),
        ];
        let script = merge_script(&dirs).expect("both directories are representable");

        for directory in &dirs {
            let spelled = directory.to_str().expect("UTF-8 fixture");
            assert_eq!(
                script.matches(spelled).count(),
                1,
                "{spelled:?} must appear exactly once in the script:\n{script}"
            );
            assert!(
                script.contains(&format!("'{spelled}'")),
                "{spelled:?} must be single-quoted where it appears:\n{script}"
            );
        }
    }

    /// C-040, item 6: the script reads the session value **at load**, so the
    /// composed PATH is a function of the then-current `launchctl getenv PATH`
    /// rather than a snapshot taken the day `ocx self setup` ran.
    #[test]
    fn the_script_reads_the_session_value_rather_than_baking_one() {
        let script = merge_script(&directories()).expect("representable");
        assert!(
            script.contains("launchctl getenv PATH"),
            "the script must read the live value:\n{script}"
        );
        assert!(
            script.contains("launchctl setenv PATH"),
            "the script must write the composed value back:\n{script}"
        );
        assert!(
            !script.contains("launchctl unsetenv"),
            "`launchctl unsetenv PATH` deletes the entire session-wide value:\n{script}"
        );
    }

    /// Install a fake `launchctl` and run the emitted script through `/bin/sh`.
    ///
    /// The script runs **verbatim** — the same `tr`/`grep -vxF`/`paste`
    /// pipeline the agent runs at load — with the fake supplied through the
    /// script's own `__OCX_TESTING_LAUNCHCTL` seam. That is the whole point of the seam: a
    /// harness that rewrote a literal in the generated text would be asserting
    /// on text it had just edited, which measures the rewriting rather than the
    /// script.
    ///
    /// Returns the value the script passed to `launchctl setenv PATH`, or
    /// `None` when it called `setenv` not at all.
    ///
    /// The launch firewall (`crate::launch`'s `no_process_spawn_outside_launch`)
    /// refuses a spawn primitive outside the launch seam, and it is right to —
    /// this module carries an entry in that test's `SPAWN_ALLOWED`, alongside
    /// the live-shell harness for `self_group/activate.rs`.
    #[cfg(unix)]
    fn run_merge_script(script: &str, current: Option<&str>) -> Option<String> {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let fake = dir.path().join("launchctl");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             d=$(dirname \"$0\")\n\
             case \"$1\" in\n\
             getenv) [ -f \"$d/current\" ] && cat \"$d/current\" ;;\n\
             setenv) shift; shift; printf '%s' \"$1\" > \"$d/result\" ;;\n\
             esac\n",
        )
        .expect("write the fake launchctl");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
        if let Some(value) = current {
            std::fs::write(dir.path().join("current"), value).expect("seed the session value");
        }

        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .env("__OCX_TESTING_LAUNCHCTL", &fake)
            .status()
            .expect("run the merge script");
        assert!(status.success(), "the merge script must exit 0:\n{script}");

        std::fs::read_to_string(dir.path().join("result")).ok()
    }

    /// The `__OCX_TESTING_LAUNCHCTL` seam defaults to the absolute `launchctl` launchd runs
    /// the agent under, so a real login behaves exactly as if the path were
    /// written inline — the seam changes nothing at load time.
    ///
    /// Pinned because a seam whose default drifted would make every test above
    /// pass against a script that does nothing on a real machine.
    #[test]
    fn the_launchctl_seam_defaults_to_the_binary_launchd_runs() {
        let script = merge_script(&directories()).expect("representable");
        assert_eq!(LAUNCHCTL_BINARY, "/bin/launchctl");
        assert!(
            script.contains(&format!(
                "__OCX_TESTING_LAUNCHCTL=${{__OCX_TESTING_LAUNCHCTL:-{LAUNCHCTL_BINARY}}}"
            )),
            "the seam must default to the absolute binary:\n{script}"
        );
    }

    /// C-040, item 6 — **this is the item's whole content**: run the emitted
    /// script against two different session values and assert two different
    /// results, each leading with the two OCX directories in contract order.
    ///
    /// A snapshot baked at setup time would produce the *same* output for both,
    /// which is precisely the rustup-era defect the `/bin/sh -c` form exists to
    /// avoid.
    #[cfg(unix)]
    #[test]
    fn the_composed_value_differs_with_the_session_value_it_was_loaded_against() {
        let dirs = directories();
        let script = merge_script(&dirs).expect("representable");
        let (bin, toolchain) = (dirs[0].display().to_string(), dirs[1].display().to_string());

        let first = run_merge_script(&script, Some("/usr/bin:/bin")).expect("setenv was called");
        let second = run_merge_script(&script, Some("/opt/homebrew/bin:/usr/bin:/bin")).expect("setenv was called");

        assert_ne!(
            first, second,
            "the composed value must be a function of the then-current session value"
        );
        for composed in [&first, &second] {
            assert!(
                composed.starts_with(&format!("{bin}:{toolchain}:")),
                "C-060 front-to-back order: the install bin directory leads, then the toolchain bin: {composed}"
            );
        }
        assert!(
            second.contains("/opt/homebrew/bin"),
            "a segment another tool put on the session PATH must survive: {second}"
        );
    }

    /// E-M5 / ADR § *Session PATH*: an empty `launchctl getenv PATH` falls back
    /// to `/usr/bin:/bin:/usr/sbin:/sbin` rather than composing a PATH holding
    /// only OCX's two directories — which would strip `sh`, `ls` and every
    /// system tool from every GUI application launched afterwards.
    #[cfg(unix)]
    #[test]
    fn an_empty_session_value_falls_back_to_the_system_default() {
        let dirs = directories();
        let script = merge_script(&dirs).expect("representable");
        let (bin, toolchain) = (dirs[0].display().to_string(), dirs[1].display().to_string());

        for current in [None, Some("")] {
            let composed = run_merge_script(&script, current).expect("setenv was called");
            assert_eq!(
                composed,
                format!("{bin}:{toolchain}:/usr/bin:/bin:/usr/sbin:/sbin"),
                "an empty read must fall back to the system default, not to an OCX-only PATH"
            );
        }
    }

    /// S-014 / E-M17 and C-040's remove-then-prepend rule, in one run: a
    /// foreign segment planted before the agent loaded survives, and loading a
    /// second time neither duplicates OCX's segments nor grows the value.
    ///
    /// The survivor is asserted **explicitly**. Asserting only that the two OCX
    /// directories lead would pass for a script that replaced the whole
    /// variable with them.
    #[cfg(unix)]
    #[test]
    fn a_second_load_is_a_no_op_and_a_foreign_segment_survives_it() {
        let dirs = directories();
        let script = merge_script(&dirs).expect("representable");

        let first = run_merge_script(&script, Some("/opt/foreign/bin:/usr/bin:/bin")).expect("setenv was called");
        let second = run_merge_script(&script, Some(&first)).expect("setenv was called");

        assert_eq!(first, second, "a second load must not grow or reorder the value");
        assert!(
            first.contains("/opt/foreign/bin"),
            "a foreign segment must be copied through unchanged: {first}"
        );
        let bin = dirs[0].display().to_string();
        assert_eq!(
            first.split(':').filter(|segment| *segment == bin).count(),
            1,
            "the install bin directory must appear once, not accumulate: {first}"
        );
    }

    /// C-037: a directory the plist cannot carry is refused **before** the
    /// script or the document is rendered, so a refused run produces no text at
    /// all rather than a mangled one.
    #[test]
    fn a_refused_directory_stops_the_render_before_any_text_exists() {
        let hostile = vec![PathBuf::from("/Users/a&b/.ocx/bin")];
        assert!(matches!(
            merge_script(&hostile),
            Err(SessionPathError::Unencodable { .. })
        ));
        assert!(matches!(
            render_plist(&hostile),
            Err(SessionPathError::Unencodable { .. })
        ));
    }

    /// C-041: the plist is a constant template with exactly two
    /// interpolations — the label and the merge script — and no escaper.
    #[test]
    fn the_plist_is_the_template_carrying_the_label_and_the_script() {
        let dirs = directories();
        let plist = render_plist(&dirs).expect("representable");
        let script = merge_script(&dirs).expect("representable");

        assert!(plist.starts_with("<?xml"), "a plist is an XML 1.0 document: {plist}");
        assert!(plist.contains("<plist") && plist.contains("</plist>"), "{plist}");
        assert!(
            plist.contains(&format!("<string>{AGENT_LABEL}</string>")),
            "the Label is the agent label: {plist}"
        );
        assert!(
            plist.contains("<key>RunAtLoad</key>"),
            "the agent must run at load, or the PATH is never composed: {plist}"
        );
        assert!(
            plist.contains("<string>/bin/sh</string>") && plist.contains("<string>-c</string>"),
            "ProgramArguments are `/bin/sh -c <script>`, never `launchctl setenv PATH <literal>`: {plist}"
        );
        assert!(plist.contains(&script), "the script is interpolated verbatim: {plist}");
    }

    /// E-M2 / E-M3, the two-sided half of the 0644 rule: a plist already on
    /// disk at a mode launchd refuses is **re-published** at 0644, not left as
    /// it was found. A one-value assertion on a freshly created file passes for
    /// an implementation that only sets the mode on create.
    #[cfg(unix)]
    #[test]
    fn a_plist_found_at_a_mode_launchd_refuses_is_republished_at_0644() {
        use std::os::unix::fs::PermissionsExt as _;

        for wrong in [0o600, 0o755, 0o666] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("sh.ocx.path.plist");
            std::fs::write(&path, "<plist>stale</plist>").expect("seed the stale plist");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(wrong)).expect("seed the wrong mode");

            publish_plist(&path, "<plist/>").expect("publish succeeds");

            let mode = std::fs::metadata(&path).expect("published").permissions().mode() & 0o777;
            assert_eq!(
                mode, PLIST_MODE,
                "a plist found at {wrong:o} must be republished at {PLIST_MODE:o}, got {mode:o}"
            );
        }
    }

    /// D-V26: deregistration targets the **service**,
    /// `launchctl bootout gui/<uid>/sh.ocx.path`, never the bare domain
    /// `gui/<uid>` — which boots out the user's entire GUI login domain and
    /// ends their graphical session. The plan and the ADR both carried the
    /// domain spelling; this asserts the correction rather than the text.
    ///
    /// Structural because `bootout` is a `launchctl` invocation with no return
    /// value to observe off macOS. Comment lines and `unimplemented!` bodies
    /// are cut first: the doc comments above quote both spellings, which is the
    /// right thing for a doc comment to do, and a stub's placeholder message is
    /// not the implementation. The positive assertion is what reds — a
    /// denylist alone would be green on a file with no `bootout` at all.
    #[test]
    fn deregistration_boots_out_the_service_and_never_the_gui_domain() {
        let code = shipped_code(include_str!("macos.rs"));

        let bootout_lines: Vec<&str> = code.lines().filter(|line| line.contains("bootout")).collect();
        assert!(
            !bootout_lines.is_empty(),
            "the deregistration path must reach for `launchctl bootout`; found none in the shipped code"
        );
        for line in &bootout_lines {
            assert!(
                line.contains("AGENT_LABEL") || line.contains(AGENT_LABEL),
                "every `bootout` must name the service `gui/<uid>/{AGENT_LABEL}`, not the domain `gui/<uid>`: {line}"
            );
        }
        assert!(
            !code.contains("launchctl unload") && !code.contains("\"unload\""),
            "`launchctl unload` is deprecated; `bootout` replaces it"
        );
    }

    /// Shipped code only: the `#[cfg(test)]` tail, `//` comment lines and
    /// `unimplemented!` placeholder messages are cut, so a structural guard
    /// cannot match its own needle list, its own documentation, or a stub's
    /// TODO string.
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
}
