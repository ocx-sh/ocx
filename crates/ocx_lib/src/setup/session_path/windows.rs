// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The Windows session-PATH writer: a direct `HKCU\Environment\Path` write.
//!
//! **Never `setx`.** It truncates silently at 1024 characters and has
//! corrupted real users' PATH in shipped installers
//! ([desktop/desktop#18176](https://github.com/desktop/desktop/issues/18176)).
//!
//! **Registration** writes `REG_EXPAND_SZ` **unconditionally**; the existing
//! type is read only to decide how to merge. rustup shipped `REG_SZ` here and
//! broke `%VAR%` expansion for every *other* entry already on PATH
//! ([rust-lang/rustup#261](https://github.com/rust-lang/rustup/issues/261)).
//! **Deregistration keeps the type it read**, because converting a value on
//! the way out changes the machine in a way removal never promised: a
//! surviving foreign segment holding a literal `%` would start expanding the
//! moment ocx took its own entries away.
//!
//! Neither directory reaches an already-open terminal or IDE, and neither can
//! win against an entry on the **System** PATH — Windows resolves System
//! before User and no write order changes that. Both limits are stated to the
//! user rather than worked around.
//!
//! # The read-modify-write race, and why it is left open
//!
//! Both writers below read the value, compose a new one, and write it back
//! with no lock and no revision check. Another installer editing `Path` inside
//! that window has its edit erased. That is a real defect, not an oversight,
//! and it is left open deliberately:
//!
//! - **Windows offers no compare-and-swap for a registry value.**
//!   `RegSetValueExW` overwrites unconditionally. The only primitive that would
//!   make the sequence atomic is the Kernel Transaction Manager
//!   (`RegCreateKeyTransacted`), which Microsoft deprecated and advises against
//!   building on. There is no correct version of this function to write.
//! - **A re-read-and-retry would narrow the window, not close it.** Writing,
//!   reading back, and recomposing on a mismatch moves the loss window from
//!   read→write to write→verify; a concurrent writer landing in the new window
//!   is erased exactly as before. That is a genuine reduction, but it reads
//!   like a fix, and a mitigation mistaken for a fix is worse than a documented
//!   hazard.
//! - **It cannot be shown to work.** This module is `#[cfg(windows)]`, the
//!   racing half is the syscall wrapper (`read_user_path` / `write_user_path`),
//!   which has no injectable seam, and no test leg can drive two writers at it.
//!   A retry loop here could only ever be shipped green without having been
//!   seen red.
//!
//! Scale for the reader who has to weigh it: the window is one string
//! composition wide (tens of microseconds), it opens only during `ocx self
//! setup` and the post-`self update` refresh, and rustup carries the identical
//! race. Revisit if a supported atomic primitive appears, or if a seam is
//! introduced for another reason and makes the retry testable.

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

#[cfg(windows)]
use super::SessionPathOutcome;
use super::{SessionPathError, SessionPathFormat};

/// The location this writer owns, reported as the outcome pair's key.
///
/// A registry path, not a filesystem path — see
/// [`super::session_path_stores`] for why the pair is keyed by `PathBuf`
/// anyway.
pub const REGISTRY_LOCATION: &str = r"HKCU\Environment\Path";

/// Characters `REG_EXPAND_SZ` cannot carry in a PATH value, checked in this
/// order (the first match is the one the refusal names).
///
/// - `%` — `REG_EXPAND_SZ` expands `%…%` pairs at read time and has **no
///   escape**, so `C:\100%real\bin` cannot be represented at all.
/// - `;` — the list delimiter. A segment containing one is two segments to
///   every reader, including this module's own merge.
/// - `\0` — the value is written as a NUL-terminated wide string, so an
///   embedded NUL would silently truncate the whole PATH.
///
/// `%` is the contracted refusal; the other two are widenings on the same
/// ground (unrepresentable in the target grammar, no escape available). Every
/// character here is legal in a Windows path except `\0`, so none of the three
/// is a hypothetical.
pub const REFUSED: &[(char, &str)] = &[
    ('%', "and REG_EXPAND_SZ has no escape for it"),
    (';', "which is the PATH delimiter and would split the entry in two"),
    ('\0', "which would truncate the registry value"),
];

/// Refuse `directory` when it cannot be spelled in a `REG_EXPAND_SZ` PATH
/// value, else yield its UTF-8 spelling.
///
/// # Errors
///
/// [`SessionPathError::NotUtf8`] for a non-UTF-8 directory;
/// [`SessionPathError::Unencodable`] naming the first character of [`REFUSED`]
/// the directory carries.
pub fn encode(directory: &Path) -> Result<&str, SessionPathError> {
    let spelled = super::encodable_str(directory, SessionPathFormat::WindowsRegistry)?;
    match REFUSED.iter().find(|(character, _)| spelled.contains(*character)) {
        Some((character, reason)) => Err(SessionPathError::Unencodable {
            path: directory.to_path_buf(),
            format: SessionPathFormat::WindowsRegistry,
            character: *character,
            reason,
        }),
        None => Ok(spelled),
    }
}

/// The spellings of `directory` that name the same directory but differ from it
/// by a single trailing separator (E-W10).
///
/// `C:\foo\` and `C:\foo` are one directory to Windows and two segments to
/// [`crate::utility::path::move_to_front`], which is segment-exact by contract
/// — deliberately, so that it agrees byte for byte with the shell snippets
/// `Shell::export_path` emits. Rather than loosen that shared rule for every
/// caller, the merge and the subtraction here drop the twins first, so a user
/// whose PATH already carries the other spelling ends with one entry rather
/// than two, and the value stops growing by one segment per run.
///
/// A drive root yields nothing: `C:\` is not the trailing-separator spelling of
/// `C:`, which names the drive's *current directory* instead.
///
/// Pure and host-independent on purpose — the rule is a property of the
/// grammar, so its red state is reachable on every CI leg rather than only on
/// the Windows one.
pub fn trailing_separator_twins(directory: &str) -> Vec<String> {
    let bare = directory.strip_suffix(['\\', '/']).unwrap_or(directory);
    if bare.is_empty() || bare.ends_with(':') {
        return Vec::new();
    }
    [format!("{bare}\\"), format!("{bare}/"), bare.to_owned()]
        .into_iter()
        .filter(|twin| twin != directory)
        .collect()
}

/// Drop every trailing-separator twin of `directories` from `existing`.
///
/// The shared first step of [`merged_value`] and [`subtracted_value`], so the
/// two cannot disagree about which spellings name the same directory.
#[cfg(windows)]
fn drop_trailing_separator_twins(existing: &str, directories: &[&str]) -> std::ffi::OsString {
    let mut value = std::ffi::OsString::from(existing);
    for directory in directories {
        for twin in trailing_separator_twins(directory) {
            value = crate::utility::path::remove_segment(&value, std::ffi::OsStr::new(&twin));
        }
    }
    value
}

/// A registry PATH value assembled from `&str` inputs is UTF-8 by construction;
/// the lossy arm exists so this function is total rather than because it can be
/// reached.
#[cfg(windows)]
fn into_value(composed: std::ffi::OsString) -> String {
    composed
        .into_string()
        .unwrap_or_else(|raw| raw.to_string_lossy().into_owned())
}

/// The merged value for `HKCU\Environment\Path`: `directories` in order,
/// then every surviving segment of `existing`.
///
/// Idempotent by presence test, never by append — split on `;`, drop empty
/// segments, drop any existing occurrence of either directory
/// (ASCII-case-insensitively, as Windows paths are), then prepend the two in
/// order and rejoin. A second `ocx self setup` is a no-op and the value does
/// not grow.
///
/// Implemented by folding [`crate::utility::path::move_to_front`] over
/// `directories` **in reverse**, so the last one moved to the front is
/// `directories[0]`. That function is the shipped implementation of exactly
/// this rule — same empty-segment drop, same segment-exact comparison, same
/// ASCII case fold on Windows — and reusing it is what keeps the registry
/// value and the in-process PATH from drifting apart. Do not re-roll it here.
///
/// `cfg(windows)` because `move_to_front` keys its separator and its case fold
/// on `cfg!(windows)` at compile time: off Windows it would apply the POSIX
/// rule to a Windows value, and a unit test of that would assert the wrong
/// contract. The Windows test leg is where this is exercised.
#[cfg(windows)]
pub fn merged_value(existing: &str, directories: &[&str]) -> String {
    let mut value = drop_trailing_separator_twins(existing, directories);
    // In reverse, so the last one moved to the front is `directories[0]` —
    // C-060's front-to-back order.
    for directory in directories.iter().rev() {
        value = crate::utility::path::move_to_front(&value, std::ffi::OsStr::new(directory));
    }
    into_value(value)
}

/// The value for `HKCU\Environment\Path` with `directories` subtracted and
/// nothing else changed.
///
/// The same remove-then-rejoin arithmetic [`merged_value`] uses, run without
/// the prepend — [`crate::utility::path::remove_segment`] folded over
/// `directories`. A foreign segment planted before `ocx self setup` ran
/// survives; the variable is never cleared.
#[cfg(windows)]
pub fn subtracted_value(existing: &str, directories: &[&str]) -> String {
    let mut value = drop_trailing_separator_twins(existing, directories);
    for directory in directories {
        value = crate::utility::path::remove_segment(&value, std::ffi::OsStr::new(directory));
    }
    into_value(value)
}

/// The registry key and value this writer owns.
///
/// Spelled apart from [`REGISTRY_LOCATION`] because the API wants the subkey
/// and the value name separately, and one `HKCU\Environment\Path` literal cut
/// into pieces at the call site is a spelling that can drift from the one the
/// run summary prints.
#[cfg(windows)]
const ENVIRONMENT_SUBKEY: &str = "Environment";
/// The value under [`ENVIRONMENT_SUBKEY`] that holds the user PATH.
#[cfg(windows)]
const PATH_VALUE_NAME: &str = "Path";

/// A NUL-terminated UTF-16 copy of `value`, for the `W` entry points.
#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read `HKCU\Environment\Path` unexpanded, with its type.
///
/// Uses `RegGetValueW` with `RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND`, never
/// `RegQueryValueExW`: the latter does not guarantee the returned buffer is
/// NUL-terminated, and its `lpcbData` is a **byte** count that reads as a
/// wchar count to anyone who does not check — two ways to build a value that
/// is wrong by one character and looks right in a debugger. `RRF_NOEXPAND` is
/// what keeps `%LOCALAPPDATA%` in a *foreign* entry from being flattened into
/// its expanded spelling by our own read-modify-write.
///
/// `RRF_RT_REG_SZ` rides along with the expand form because the restriction
/// mask decides whether the read *succeeds at all*: rustup shipped `REG_SZ`
/// here and those values are still on real machines, and a read that refused
/// them would report the PATH absent and then overwrite it. The type comes
/// back so the caller can rewrite a `REG_SZ` value as `REG_EXPAND_SZ` even
/// when its text already merges to itself.
///
/// `Ok(None)` when the value does not exist — a fresh user profile — which is
/// an empty PATH to merge into, not a failure.
#[cfg(windows)]
fn read_user_path() -> std::io::Result<Option<(String, u32)>> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
    };

    let subkey = wide(ENVIRONMENT_SUBKEY);
    let name = wide(PATH_VALUE_NAME);
    // `RRF_RT_REG_SZ` beside the expand type, not instead of it: the mask
    // decides whether the read succeeds at all, and a stock or rustup-era
    // `REG_SZ` PATH answers `ERROR_UNSUPPORTED_TYPE` under the expand-only
    // mask. `RRF_NOEXPAND` still applies, so a foreign `%LOCALAPPDATA%` comes
    // back unflattened.
    let flags = RRF_RT_REG_EXPAND_SZ | RRF_RT_REG_SZ | RRF_NOEXPAND;
    let mut kind: u32 = 0;
    let mut bytes: u32 = 0;

    // SAFETY: `subkey` and `name` are NUL-terminated UTF-16 buffers that
    // outlive the call; the two out-parameters point at live locals; a null
    // data pointer with a zero size is the documented size-probe form.
    let probed = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            name.as_ptr(),
            flags,
            &raw mut kind,
            std::ptr::null_mut(),
            &raw mut bytes,
        )
    };
    if probed == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if probed != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(probed as i32));
    }

    // `bytes`, not code units: the API counts bytes, and sizing a `u16` buffer
    // with that number directly is the off-by-half this comment exists to stop.
    let mut buffer: Vec<u16> = vec![0; (bytes as usize).div_ceil(2)];
    let mut written = bytes;
    // SAFETY: as above, plus `buffer` is at least `written` bytes of writable,
    // `u16`-aligned storage for the whole call.
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            name.as_ptr(),
            flags,
            &raw mut kind,
            buffer.as_mut_ptr().cast(),
            &raw mut written,
        )
    };
    if read == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if read != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(read as i32));
    }

    // Truncate at the code-unit boundary — an odd byte count must drop the
    // half unit rather than slice into one — and trim the terminator without
    // relying on one being there.
    let units = ((written as usize) / 2).min(buffer.len());
    let value = &buffer[..units];
    let value = match value.iter().rposition(|unit| *unit != 0) {
        Some(last) => &value[..=last],
        None => &[][..],
    };
    let text = String::from_utf16(value).map_err(|error| {
        // Not lossy: `to_string_lossy` would substitute U+FFFD into somebody
        // else's PATH segment and then write the corrupted value back.
        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
    })?;
    Ok(Some((text, kind)))
}

/// Write `value` to `HKCU\Environment\Path` under `kind`.
///
/// The register path passes `REG_EXPAND_SZ` unconditionally (C-038); the
/// subtract path passes the type it read back, because converting a value on
/// the way *out* would leave the machine changed in a way deregistration never
/// promised — a surviving foreign segment holding a literal `%` starts
/// expanding the moment ocx removes itself, which is the opposite of S-014's
/// "only OCX's two segments are gone".
#[cfg(windows)]
fn write_user_path(value: &str, kind: u32) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, RegCloseKey, RegOpenKeyExW, RegSetValueExW,
    };

    let subkey = wide(ENVIRONMENT_SUBKEY);
    let name = wide(PATH_VALUE_NAME);
    let data = wide(value);
    let size = u32::try_from(std::mem::size_of_val(data.as_slice()))
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "PATH value exceeds a registry value"))?;

    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: `subkey` is a NUL-terminated UTF-16 buffer that outlives the
    // call, and `key` is a live local the call fills in on success.
    let opened = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_SET_VALUE, &raw mut key) };
    if opened != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(opened as i32));
    }

    // SAFETY: `key` is open for the whole call; `name` and `data` are
    // NUL-terminated UTF-16 buffers that outlive it; `size` is `data`'s length
    // in bytes.
    let written = unsafe { RegSetValueExW(key, name.as_ptr(), 0, kind, data.as_ptr().cast(), size) };
    // SAFETY: `key` was opened above and is not used afterwards.
    unsafe { RegCloseKey(key) };

    if written != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(written as i32));
    }
    Ok(())
}

/// Tell already-running processes that the environment block changed.
///
/// `SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, 0, "Environment",
/// SMTO_ABORTIFHUNG, 5000, …)`. The timeout flag is **not optional**: a
/// blocking `SendMessage` to `HWND_BROADCAST` wedges on any hung top-level
/// window on the desktop, and `ocx self setup` would hang with no output.
///
/// Best-effort by contract — a failed broadcast leaves a correct registry
/// value that new processes pick up anyway, so it never downgrades the
/// outcome.
#[cfg(windows)]
fn broadcast_environment_change() {
    use windows_sys::Win32::Foundation::{LPARAM, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };

    let subject = wide(ENVIRONMENT_SUBKEY);
    let mut ignored: usize = 0;
    // SAFETY: `subject` is a NUL-terminated UTF-16 buffer that outlives the
    // call, `ignored` is a live local, and the timeout plus `SMTO_ABORTIFHUNG`
    // bound the call rather than letting a hung window hold it forever. The
    // result is deliberately discarded: the registry write already landed, so
    // a failed broadcast costs a user nothing but a new terminal.
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0 as WPARAM,
            subject.as_ptr() as LPARAM,
            SMTO_ABORTIFHUNG,
            5000,
            &raw mut ignored,
        )
    };
}

/// Register `directories` in `HKCU\Environment\Path`.
///
/// Every directory is refused-or-encoded before the key is read, so a refused
/// run performs no registry access at all.
///
/// # Errors
///
/// [`SessionPathError`] from [`encode`]. A registry failure is
/// [`SessionPathOutcome::Failed`], never an error.
#[cfg(windows)]
pub(crate) fn register(
    directories: &[PathBuf],
    dry_run: bool,
) -> Result<(PathBuf, SessionPathOutcome), SessionPathError> {
    // The one `?` on this path, and it runs before the key is opened: from
    // here down every failure is an outcome (C-036).
    let mut encoded = Vec::with_capacity(directories.len());
    for directory in directories {
        encoded.push(encode(directory)?);
    }
    let location = PathBuf::from(REGISTRY_LOCATION);
    let outcome = match merge_into_registry(&encoded, dry_run) {
        Ok(outcome) => outcome,
        Err(error) => {
            // Reported, not swallowed: the user-facing half is the caller's
            // warning, and this is the cause behind it.
            tracing::debug!(%error, "user PATH registry write failed");
            SessionPathOutcome::Failed
        }
    };
    Ok((location, outcome))
}

/// Read, merge, and write back — the body [`register`] maps to an outcome.
#[cfg(windows)]
fn merge_into_registry(directories: &[&str], dry_run: bool) -> std::io::Result<SessionPathOutcome> {
    use windows_sys::Win32::System::Registry::REG_EXPAND_SZ;

    let existing = match read_user_path() {
        Ok(existing) => existing,
        // A dry run never reports `Failed` — nothing was attempted. A store it
        // could not even read is one the real run would have to rewrite, which
        // is the prediction its siblings make too: `linux::register` and
        // `macos::register` both treat an unreadable store as not-current.
        Err(error) if dry_run => {
            tracing::debug!(%error, "user PATH registry read failed during a dry run");
            return Ok(SessionPathOutcome::Written);
        }
        Err(error) => return Err(error),
    };
    let text = existing.as_ref().map_or("", |(text, _)| text.as_str());
    let composed = merged_value(text, directories);
    // A `REG_SZ` value whose text already merges to itself still has to be
    // rewritten: the type is the half rustup got wrong, and leaving it breaks
    // `%VAR%` expansion for every *other* entry on the PATH.
    let type_is_current = existing.as_ref().is_none_or(|(_, kind)| *kind == REG_EXPAND_SZ);
    if composed == text && type_is_current {
        return Ok(SessionPathOutcome::Unchanged);
    }
    if dry_run {
        return Ok(SessionPathOutcome::Written);
    }
    write_user_path(&composed, REG_EXPAND_SZ)?;
    broadcast_environment_change();
    Ok(SessionPathOutcome::Written)
}

/// Subtract `directories` from `HKCU\Environment\Path`, leaving every other
/// segment in place.
///
/// # Errors
///
/// Same as [`register`].
#[cfg(windows)]
pub(crate) fn deregister(
    directories: &[PathBuf],
    dry_run: bool,
) -> Result<(PathBuf, SessionPathOutcome), SessionPathError> {
    // The one `?` on this path, and it runs before the key is opened.
    let mut encoded = Vec::with_capacity(directories.len());
    for directory in directories {
        encoded.push(encode(directory)?);
    }
    let location = PathBuf::from(REGISTRY_LOCATION);
    let outcome = match subtract_from_registry(&encoded, dry_run) {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::debug!(%error, "user PATH registry subtraction failed");
            SessionPathOutcome::Failed
        }
    };
    Ok((location, outcome))
}

/// Read, subtract, and write back — the body [`deregister`] maps to an outcome.
///
/// Keyed on content alone, unlike [`merge_into_registry`]: a foreign `REG_SZ`
/// value we are not otherwise touching is left at the type its owner chose.
#[cfg(windows)]
fn subtract_from_registry(directories: &[&str], dry_run: bool) -> std::io::Result<SessionPathOutcome> {
    let current = match read_user_path() {
        Ok(current) => current,
        // A dry run never reports `Failed` — nothing was attempted. A store it
        // could not even read is one it cannot claim it would take anything
        // away from, so the prediction is `Unchanged`.
        Err(error) if dry_run => {
            tracing::debug!(%error, "user PATH registry read failed during a dry run");
            return Ok(SessionPathOutcome::Unchanged);
        }
        Err(error) => return Err(error),
    };
    let Some((existing, kind)) = current else {
        return Ok(SessionPathOutcome::Unchanged);
    };
    let composed = subtracted_value(&existing, directories);
    if composed == existing {
        return Ok(SessionPathOutcome::Unchanged);
    }
    if dry_run {
        return Ok(SessionPathOutcome::Removed);
    }
    write_user_path(&composed, kind)?;
    broadcast_environment_change();
    Ok(SessionPathOutcome::Removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(directory: &str) -> SessionPathError {
        encode(Path::new(directory)).expect_err("the directory must be refused")
    }

    fn refused_character(directory: &str) -> char {
        match refusal(directory) {
            SessionPathError::Unencodable {
                character,
                format: SessionPathFormat::WindowsRegistry,
                ..
            } => character,
            other => panic!("expected an Unencodable refusal naming the registry format, got {other:?}"),
        }
    }

    /// C-037 / C-038 / S-015 / item 5 (E-W12): `REG_EXPAND_SZ` expands `%…%`
    /// pairs at read time and has **no escape**, so `C:\100%real\bin` cannot be
    /// represented at all and is refused before the key is touched. `;` and
    /// `\0` are the two widenings on the same ground — the list delimiter, and
    /// a byte that would truncate the whole value.
    ///
    /// Host-independent on purpose: the rule is a property of the wire format,
    /// not of the host, so its red state is reachable on every CI leg rather
    /// than only on the Windows one.
    #[test]
    fn the_registry_refuses_every_character_reg_expand_sz_cannot_carry() {
        for (directory, offender) in [
            (r"C:\100%real\.ocx\bin", '%'),
            (r"C:\a;b\.ocx\bin", ';'),
            ("C:\\a\0b\\.ocx\\bin", '\0'),
        ] {
            assert_eq!(
                refused_character(directory),
                offender,
                "REG_EXPAND_SZ must refuse {offender:?} in {directory:?}"
            );
        }
    }

    /// S-015 / C-037: the refusal names the path **and** the format, and
    /// classifies as exit 78 — the code the ADR's error table gives
    /// "`OCX_HOME` cannot be encoded for a session-PATH format".
    #[test]
    fn a_registry_refusal_names_the_path_and_the_format() {
        use crate::cli::ClassifyExitCode as _;

        let error = refusal(r"C:\100%real\.ocx\bin");
        let rendered = error.to_string();
        assert!(
            rendered.contains(r"100%real"),
            "the refusal must name the path: {rendered}"
        );
        assert!(
            rendered.contains(&SessionPathFormat::WindowsRegistry.to_string()),
            "the refusal must name the format: {rendered}"
        );
        assert_eq!(error.classify(), Some(crate::cli::ExitCode::ConfigError));
    }

    /// The load-bearing **negative**: an over-broad refusal set breaks ordinary
    /// Windows homes. A space is legal and common (`C:\Users\First Last`), and
    /// `$`, a backtick, `&`, `'` and `=` are all inert to `REG_EXPAND_SZ`,
    /// which is not a shell.
    #[test]
    fn the_registry_accepts_a_directory_it_can_carry() {
        for directory in [
            r"C:\Users\u\.ocx\toolchain\bin",
            r"C:\Users\First Last\.ocx\toolchain\bin",
            r"C:\Users\a$b\.ocx\bin",
            r"C:\Users\a&b\.ocx\bin",
            r"C:\Users\a'b\.ocx\bin",
            r"C:\Users\a=b\.ocx\bin",
            r"C:\Users\ünïcode\.ocx\bin",
        ] {
            assert_eq!(
                encode(Path::new(directory)).expect("a representable directory must be accepted"),
                directory,
                "{directory:?} is representable in a REG_EXPAND_SZ PATH value"
            );
        }
    }

    /// C-038, and the structural half of item 3 / E-W11 / E-W15.
    ///
    /// Every needle is a clause of C-038 with a named, shipped failure behind
    /// it, so this is the contract's own vocabulary rather than a style rule:
    ///
    /// - `setx` truncates silently at 1024 characters and has corrupted real
    ///   users' PATH in shipped installers (desktop/desktop#18176).
    /// - `RegQueryValueEx` does not guarantee NUL-termination and reports a
    ///   **byte** count in `lpcbData`; `RegGetValueW` is what C-038 names.
    /// - `RRF_NOEXPAND` is what keeps a *foreign* `%LOCALAPPDATA%` from being
    ///   flattened into its expanded spelling by our own read-modify-write —
    ///   the rustup/#261 defect (E-W13).
    /// - `RRF_RT_REG_SZ` is required **beside** `RRF_RT_REG_EXPAND_SZ` on the
    ///   read. E-W1 says the value is written back as `REG_EXPAND_SZ` whatever
    ///   type it had, which is only reachable if the read accepts the plain
    ///   `REG_SZ` a stock Windows profile or another installer left behind:
    ///   with the expand-only flag `RegGetValueW` answers
    ///   `ERROR_UNSUPPORTED_TYPE`, the existing PATH reads as absent, and the
    ///   merge overwrites it with our two entries alone. C-038's prose names
    ///   only the expand type because it is describing the *write*; that
    ///   imprecision is the reason this needle is spelled out here rather than
    ///   left implied.
    /// - `REG_EXPAND_SZ` is written **unconditionally**, whatever the existing
    ///   type was (E-W1, E-W2).
    /// - `SMTO_ABORTIFHUNG` with a 5000 ms timeout is not optional: a blocking
    ///   `SendMessage` to `HWND_BROADCAST` wedges `ocx self setup` on any hung
    ///   top-level window (E-W15).
    ///
    /// Structural because none of it has a return value observable off a real
    /// `HKCU` hive, and a unit test that wrote the developer's own user PATH
    /// would not be isolated. The **positive** assertions are what red; the
    /// denylist alone would be green on a file that implemented nothing.
    #[test]
    fn the_registry_writer_names_the_api_c_038_pins() {
        let code = shipped_code(include_str!("windows.rs"));

        for required in [
            "RegGetValueW",
            "RRF_RT_REG_EXPAND_SZ",
            "RRF_RT_REG_SZ",
            "RRF_NOEXPAND",
            "REG_EXPAND_SZ",
            "SendMessageTimeoutW",
            "HWND_BROADCAST",
            "SMTO_ABORTIFHUNG",
            "5000",
        ] {
            assert!(
                code.contains(required),
                "C-038 pins {required:?}; the shipped code does not name it"
            );
        }
        for forbidden in ["setx", "RegQueryValueEx", "RegDeleteValue", "RegDeleteKey"] {
            assert!(
                !code.contains(forbidden),
                "C-038 forbids {forbidden:?}: the value is edited, never truncated or deleted"
            );
        }

        // The three needles above are satisfied by the `use` list alone, which
        // is how the expand-only mask shipped past this test once already: the
        // import was there and the *expression* was not. This pins the mask
        // itself, so dropping a flag from the read reds here rather than only
        // on a machine whose PATH happens to be `REG_SZ`.
        assert!(
            code.contains("RRF_RT_REG_EXPAND_SZ | RRF_RT_REG_SZ | RRF_NOEXPAND"),
            "C-038's read mask must name all three flags in the `RegGetValueW` call, \
             not merely import them"
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

    /// E-W10: `C:\foo\` and `C:\foo` name one directory, so the merge's
    /// presence test has to see through a single trailing separator — else a
    /// user who once typed the other spelling ends with two entries and the
    /// value grows by one segment on every run.
    ///
    /// Host-independent because the rule is a property of the grammar, so its
    /// red state is reachable on every CI leg rather than only on the Windows
    /// one. The behavioural half is the `merged_value` test below.
    #[test]
    fn a_single_trailing_separator_names_the_same_directory() {
        assert_eq!(
            trailing_separator_twins(r"C:\Users\u\.ocx\bin"),
            vec![r"C:\Users\u\.ocx\bin\".to_owned(), r"C:\Users\u\.ocx\bin/".to_owned()],
            "the bare spelling is the input itself, so only the two separator forms are twins"
        );
        assert_eq!(
            trailing_separator_twins(r"C:\Users\u\.ocx\bin\"),
            vec![r"C:\Users\u\.ocx\bin/".to_owned(), r"C:\Users\u\.ocx\bin".to_owned()],
            "given the separator spelling, the bare one is a twin"
        );
        assert!(
            trailing_separator_twins(r"C:\").is_empty(),
            r"a drive root is not the trailing-separator spelling of `C:`, which names its current directory"
        );
        assert!(trailing_separator_twins("").is_empty());
    }

    // ── C-038: the merge and subtraction arithmetic (Windows leg) ────────
    //
    // `merged_value` and `subtracted_value` are `cfg(windows)` because the
    // shipped `utility::path` helpers they delegate to key their separator and
    // their ASCII case fold on `cfg!(windows)` at compile time. These run on
    // the Windows test leg — `.github/workflows/verify-deep.yml` runs the whole
    // workspace under `--target=x86_64-pc-windows-msvc`, and
    // `verify-basic.yml` carries a dedicated Windows test job — so their red is
    // reachable on every pull request.

    #[cfg(windows)]
    const BIN: &str = r"C:\Users\u\.ocx\symlinks\abc\current\content\bin";
    #[cfg(windows)]
    const TOOLCHAIN: &str = r"C:\Users\u\.ocx\toolchain\bin";

    #[cfg(windows)]
    fn merged(existing: &str) -> String {
        merged_value(existing, &[BIN, TOOLCHAIN])
    }

    /// C-038 / E-W3, E-W4: an absent or empty value becomes exactly the two
    /// directories, in contract order and with no leading `;`.
    #[cfg(windows)]
    #[test]
    fn an_absent_or_empty_value_becomes_the_two_directories_alone() {
        for existing in ["", ";", ";;"] {
            assert_eq!(
                merged(existing),
                format!("{BIN};{TOOLCHAIN}"),
                "for existing {existing:?}"
            );
        }
    }

    /// C-038 / C-060 / E-X10: the install bin directory leads, then the
    /// toolchain bin directory, then every surviving foreign segment in its
    /// original order.
    #[cfg(windows)]
    #[test]
    fn the_two_directories_lead_and_foreign_segments_keep_their_order() {
        assert_eq!(
            merged(r"C:\Windows;C:\Windows\System32"),
            format!(r"{BIN};{TOOLCHAIN};C:\Windows;C:\Windows\System32")
        );
    }

    /// C-038 / E-W5, E-W6: empty segments — a trailing `;`, a doubled `;;` —
    /// are dropped, and the foreign segments around them survive in order.
    #[cfg(windows)]
    #[test]
    fn empty_segments_are_dropped_and_foreign_ones_survive() {
        let composed = merged(r"C:\Windows;;C:\Tools;");
        assert_eq!(composed, format!(r"{BIN};{TOOLCHAIN};C:\Windows;C:\Tools"));
        assert!(!composed.ends_with(';'), "no trailing delimiter: {composed}");
        assert!(!composed.contains(";;"), "no empty segment: {composed}");
    }

    /// C-038 / item 4 / E-W8: a second `ocx self setup` is a no-op — the value
    /// is byte-identical and does **not** grow. This is idempotency by presence
    /// test rather than by append, which is the whole reason the merge exists.
    #[cfg(windows)]
    #[test]
    fn a_second_merge_is_byte_identical_and_the_value_does_not_grow() {
        let first = merged(r"C:\Windows;C:\Tools");
        let second = merged(&first);
        assert_eq!(first, second);
        assert_eq!(
            second.split(';').filter(|segment| *segment == BIN).count(),
            1,
            "the install bin directory must appear once, not accumulate: {second}"
        );
    }

    /// C-038 / item 4 / E-W7: the two directories already present but in the
    /// **wrong order** are both dropped and re-prepended in contract order — a
    /// rewrite, not a no-op, because the order is the contract.
    #[cfg(windows)]
    #[test]
    fn a_wrong_order_pair_is_re_prepended_in_contract_order() {
        let existing = format!(r"{TOOLCHAIN};C:\Windows;{BIN}");
        assert_eq!(merged(&existing), format!(r"{BIN};{TOOLCHAIN};C:\Windows"));
    }

    /// C-038 / item 4 / E-W10, behavioural half: an existing occurrence that
    /// differs only by a trailing backslash is the **same** directory, so it is
    /// dropped rather than left beside the freshly prepended spelling.
    ///
    /// Without it the value gains one segment per run for anyone whose PATH was
    /// first written by a tool that spells directories with a trailing
    /// separator — `move_to_front` is segment-exact by contract and never
    /// collapses the two.
    #[cfg(windows)]
    #[test]
    fn a_trailing_separator_spelling_is_not_left_behind_as_a_duplicate() {
        let existing = format!(r"{BIN}\;C:\Windows;{TOOLCHAIN}/");
        let composed = merged(&existing);
        assert_eq!(composed, format!(r"{BIN};{TOOLCHAIN};C:\Windows"));
        assert_eq!(
            composed.split(';').count(),
            3,
            "the trailing-separator spellings must not survive as extra segments: {composed}"
        );
        assert_eq!(
            subtracted_value(&existing, &[BIN, TOOLCHAIN]),
            r"C:\Windows",
            "the subtraction sees through the same spelling, or deregistration leaves them forever"
        );
    }

    /// C-038 / item 4 / E-W9: Windows paths are case-insensitive, so an
    /// existing occurrence spelled in another case is the **same** segment and
    /// is dropped rather than duplicated. Without the fold the value grows by
    /// two segments on every run for a user whose profile path was spelled
    /// differently by whatever wrote it first.
    #[cfg(windows)]
    #[test]
    fn an_existing_occurrence_is_matched_case_insensitively() {
        let existing = format!("{};C:\\Windows", BIN.to_lowercase());
        let composed = merged(&existing);
        assert_eq!(composed, format!(r"{BIN};{TOOLCHAIN};C:\Windows"));
        assert_eq!(
            composed.split(';').count(),
            3,
            "the differently-cased occurrence must not survive as a fourth segment: {composed}"
        );
    }

    /// C-038 / E-W11 — **the positive control that proves `setx` is not on this
    /// path.** `setx` truncates at 1024 characters; a direct registry write
    /// does not. The merge must carry a value well past that limit through
    /// byte-for-byte.
    #[cfg(windows)]
    #[test]
    fn a_value_longer_than_setx_survives_the_merge_intact() {
        let foreign: Vec<String> = (0..40)
            .map(|index| format!(r"C:\Program Files\vendor-{index:02}\bin"))
            .collect();
        let existing = foreign.join(";");
        assert!(existing.len() > 1024, "the fixture must exceed the setx limit");

        let composed = merged(&existing);

        assert!(composed.len() > existing.len(), "the merge must not shrink the value");
        for segment in &foreign {
            assert!(
                composed.split(';').any(|candidate| candidate == segment),
                "{segment} was truncated away: {composed}"
            );
        }
    }

    /// C-038 / E-W13: `RRF_NOEXPAND` means the read returns the literal, so a
    /// foreign `%LOCALAPPDATA%` survives the read-modify-write verbatim. Baking
    /// its expansion into the rewritten value is the rustup/#261 defect — it
    /// breaks `%VAR%` expansion for every *other* entry already on PATH.
    #[cfg(windows)]
    #[test]
    fn an_unexpanded_variable_in_a_foreign_segment_survives_verbatim() {
        let composed = merged(r"%LOCALAPPDATA%\Microsoft\WindowsApps;C:\Windows");
        assert!(
            composed.contains(r"%LOCALAPPDATA%\Microsoft\WindowsApps"),
            "a foreign unexpanded variable must survive: {composed}"
        );
    }

    /// **S-014, the survivor half, on Windows.** Subtraction removes exactly
    /// the two segments its sibling added; a foreign segment planted before
    /// `ocx self setup` ran is still there afterwards, and the value is never
    /// cleared.
    ///
    /// The survivor is asserted explicitly: asserting only that OCX's segments
    /// are gone would pass for an implementation that emptied the whole value.
    #[cfg(windows)]
    #[test]
    fn subtraction_removes_only_our_segments_and_keeps_the_foreign_ones() {
        let existing = format!(r"{BIN};C:\Program Files\foreign\bin;{TOOLCHAIN};C:\Windows");
        let composed = subtracted_value(&existing, &[BIN, TOOLCHAIN]);
        assert_eq!(composed, r"C:\Program Files\foreign\bin;C:\Windows");
        assert!(!composed.is_empty(), "the variable is never cleared: {composed}");
    }

    /// S-014 / C-036: removal is a no-op when neither segment is present — the
    /// value comes back with every foreign segment intact and nothing else
    /// changed.
    #[cfg(windows)]
    #[test]
    fn subtraction_is_a_no_op_when_neither_segment_is_present() {
        let existing = r"C:\Windows;C:\Windows\System32";
        assert_eq!(subtracted_value(existing, &[BIN, TOOLCHAIN]), existing);
    }

    /// S-014 / E-W9: the subtraction folds case exactly as the merge does, or a
    /// differently-cased entry the merge would have de-duplicated survives
    /// deregistration forever.
    #[cfg(windows)]
    #[test]
    fn subtraction_folds_case_the_same_way_the_merge_does() {
        let existing = format!("{};C:\\Windows", BIN.to_lowercase());
        assert_eq!(subtracted_value(&existing, &[BIN, TOOLCHAIN]), r"C:\Windows");
    }
}
