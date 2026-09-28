// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The Windows session-PATH writer: a direct `HKCU\Environment\Path` write, never `setx`, which
//! truncates silently at 1024 characters.
//!
//! Registration always writes `REG_EXPAND_SZ`: `REG_SZ` breaks `%VAR%` expansion for every other entry.

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

#[cfg(windows)]
use super::SessionPathOutcome;
use super::{SessionPathError, SessionPathFormat};

/// The location this writer owns, reported as the outcome pair's key; a registry path, not a file.
pub const REGISTRY_LOCATION: &str = r"HKCU\Environment\Path";

/// Characters a `REG_EXPAND_SZ` PATH value cannot carry, in the order the refusal names them.
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

/// The spellings of `directory` that name the same directory but differ from it by a single trailing
/// separator.
///
/// The merge and subtraction drop these first: `move_to_front` is segment-exact, so the value would
/// otherwise grow by one segment per run. A drive root yields nothing, since `C:` is the drive's
/// current directory, not `C:\`.
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
#[cfg(windows)]
fn drop_trailing_separator_twins(existing: &str, directories: &[&str]) -> std::ffi::OsString {
    let mut value = std::ffi::OsString::from(existing);
    for directory in directories {
        for twin in trailing_separator_twins(directory) {
            value = ocx_util::path::remove_segment(&value, std::ffi::OsStr::new(&twin));
        }
    }
    value
}

/// Back to `String`; the lossy arm is unreachable, since every input was `&str`.
#[cfg(windows)]
fn into_value(composed: std::ffi::OsString) -> String {
    composed
        .into_string()
        .unwrap_or_else(|raw| raw.to_string_lossy().into_owned())
}

/// The merged value for `HKCU\Environment\Path`: `directories` in order, then every surviving segment
/// of `existing`.
// `move_to_front` is reused so the registry value and the in-process PATH cannot drift; `cfg(windows)`
// because its separator and case fold are the host's.
#[cfg(windows)]
pub fn merged_value(existing: &str, directories: &[&str]) -> String {
    let mut value = drop_trailing_separator_twins(existing, directories);
    // Reversed, so `directories[0]` ends up in front.
    for directory in directories.iter().rev() {
        value = ocx_util::path::move_to_front(&value, std::ffi::OsStr::new(directory));
    }
    into_value(value)
}

/// The value for `HKCU\Environment\Path` with `directories` subtracted and nothing else changed.
#[cfg(windows)]
pub fn subtracted_value(existing: &str, directories: &[&str]) -> String {
    let mut value = drop_trailing_separator_twins(existing, directories);
    for directory in directories {
        value = ocx_util::path::remove_segment(&value, std::ffi::OsStr::new(directory));
    }
    into_value(value)
}

/// The registry subkey holding the user PATH.
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
/// `Ok(None)` when the value does not exist (a fresh profile).
// `RegGetValueW`, never `RegQueryValueExW`, which does not guarantee a NUL-terminated buffer.
// `RRF_NOEXPAND`, or a foreign `%LOCALAPPDATA%` entry is flattened by our read-modify-write.
#[cfg(windows)]
fn read_user_path() -> std::io::Result<Option<(String, u32)>> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
    };

    let subkey = wide(ENVIRONMENT_SUBKEY);
    let name = wide(PATH_VALUE_NAME);
    // `RRF_RT_REG_SZ` too, or a `REG_SZ` PATH fails the read with `ERROR_UNSUPPORTED_TYPE`.
    let flags = RRF_RT_REG_EXPAND_SZ | RRF_RT_REG_SZ | RRF_NOEXPAND;
    let mut kind: u32 = 0;
    let mut bytes: u32 = 0;

    // SAFETY: `subkey` and `name` are NUL-terminated and outlive the call, the out-parameters are
    // live locals, and a null data pointer with zero size is the documented size probe.
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

    // `bytes` counts bytes, not `u16` units; sizing the buffer with it directly is off by half.
    let mut buffer: Vec<u16> = vec![0; (bytes as usize).div_ceil(2)];
    let mut written = bytes;
    // SAFETY: as above, and `buffer` holds at least `written` bytes of writable, `u16`-aligned storage.
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

    // An odd byte count drops the half unit; the terminator is trimmed without assuming one exists.
    let units = ((written as usize) / 2).min(buffer.len());
    let value = &buffer[..units];
    let value = match value.iter().rposition(|unit| *unit != 0) {
        Some(last) => &value[..=last],
        None => &[][..],
    };
    let text = String::from_utf16(value).map_err(|error| {
        // Not lossy: U+FFFD would be written back into another tool's segment.
        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
    })?;
    Ok(Some((text, kind)))
}

/// Write `value` to `HKCU\Environment\Path` under `kind`.
// Deregistration passes the type it read, or a surviving foreign `%` segment starts expanding.
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

    // SAFETY: `key` is open for the whole call, `name` and `data` are NUL-terminated and outlive it,
    // and `size` is `data`'s length in bytes.
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
/// Best-effort: a failed broadcast never downgrades the outcome.
// The timeout and `SMTO_ABORTIFHUNG` are required: a blocking broadcast wedges on any hung window
// and setup hangs silently.
#[cfg(windows)]
fn broadcast_environment_change() {
    use windows_sys::Win32::Foundation::{LPARAM, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };

    let subject = wide(ENVIRONMENT_SUBKEY);
    let mut ignored: usize = 0;
    // SAFETY: `subject` is NUL-terminated and outlives the call, and `ignored` is a live local.
    // The result is discarded: the registry write already landed.
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

/// Register `directories` in `HKCU\Environment\Path`; a refused run touches no registry.
///
/// # Errors
///
/// [`SessionPathError`] from [`encode`]; a registry failure is [`SessionPathOutcome::Failed`].
#[cfg(windows)]
pub(crate) fn register(
    directories: &[PathBuf],
    dry_run: bool,
) -> Result<(PathBuf, SessionPathOutcome), SessionPathError> {
    // The only `?`, before the key is opened: every later failure must be an outcome, never an error.
    let mut encoded = Vec::with_capacity(directories.len());
    for directory in directories {
        encoded.push(encode(directory)?);
    }
    let location = PathBuf::from(REGISTRY_LOCATION);
    let outcome = match merge_into_registry(&encoded, dry_run) {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::debug!(%error, "user PATH registry write failed");
            SessionPathOutcome::Failed
        }
    };
    Ok((location, outcome))
}

/// Read, merge, and write back: the body [`register`] maps to an outcome.
// ponytail: unlocked read-modify-write, so a concurrent `Path` edit is lost; the registry has no
// compare-and-swap, and rustup carries the same race.
#[cfg(windows)]
fn merge_into_registry(directories: &[&str], dry_run: bool) -> std::io::Result<SessionPathOutcome> {
    use windows_sys::Win32::System::Registry::REG_EXPAND_SZ;

    let existing = match read_user_path() {
        Ok(existing) => existing,
        // A dry run never reports `Failed`; an unreadable store is one the real run would rewrite.
        Err(error) if dry_run => {
            tracing::debug!(%error, "user PATH registry read failed during a dry run");
            return Ok(SessionPathOutcome::Written);
        }
        Err(error) => return Err(error),
    };
    let text = existing.as_ref().map_or("", |(text, _)| text.as_str());
    let composed = merged_value(text, directories);
    // A current-text `REG_SZ` is still rewritten, or `%VAR%` expansion stays broken for other entries.
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
    // The only `?`, before the key is opened.
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

/// Read, subtract, and write back: the body [`deregister`] maps to an outcome.
///
/// Keyed on content alone: a foreign `REG_SZ` value keeps the type its owner chose.
#[cfg(windows)]
fn subtract_from_registry(directories: &[&str], dry_run: bool) -> std::io::Result<SessionPathOutcome> {
    let current = match read_user_path() {
        Ok(current) => current,
        // A dry run never reports `Failed`; an unreadable store predicts nothing removed.
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
        merged_value(existing, &[TOOLCHAIN, BIN])
    }

    /// C-038 / E-W3, E-W4: an absent or empty value becomes exactly the two
    /// directories, in contract order and with no leading `;`.
    #[cfg(windows)]
    #[test]
    fn an_absent_or_empty_value_becomes_the_two_directories_alone() {
        for existing in ["", ";", ";;"] {
            assert_eq!(
                merged(existing),
                format!("{TOOLCHAIN};{BIN}"),
                "for existing {existing:?}"
            );
        }
    }

    /// C-038 / C-060 / E-X10: the toolchain bin directory leads, then the
    /// install bin directory, then every surviving foreign segment in its
    /// original order.
    #[cfg(windows)]
    #[test]
    fn the_two_directories_lead_and_foreign_segments_keep_their_order() {
        assert_eq!(
            merged(r"C:\Windows;C:\Windows\System32"),
            format!(r"{TOOLCHAIN};{BIN};C:\Windows;C:\Windows\System32")
        );
    }

    /// C-038 / E-W5, E-W6: empty segments — a trailing `;`, a doubled `;;` —
    /// are dropped, and the foreign segments around them survive in order.
    #[cfg(windows)]
    #[test]
    fn empty_segments_are_dropped_and_foreign_ones_survive() {
        let composed = merged(r"C:\Windows;;C:\Tools;");
        assert_eq!(composed, format!(r"{TOOLCHAIN};{BIN};C:\Windows;C:\Tools"));
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
        let existing = format!(r"{BIN};C:\Windows;{TOOLCHAIN}");
        assert_eq!(merged(&existing), format!(r"{TOOLCHAIN};{BIN};C:\Windows"));
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
        assert_eq!(composed, format!(r"{TOOLCHAIN};{BIN};C:\Windows"));
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
        assert_eq!(composed, format!(r"{TOOLCHAIN};{BIN};C:\Windows"));
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
