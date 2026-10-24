// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Windows rejects a path of 260 UTF-16 units or more in a call that lacks the `\\?\` prefix.
//!
//! `std::fs` adds the prefix itself, but `tempfile` hands both paths of a publish to `MoveFileExW` as given.

#[cfg(windows)]
use std::path::{Path, PathBuf};

#[cfg(windows)]
use super::Replace;

/// `MAX_PATH`: Windows rejects a non-verbatim path of this many UTF-16 units or more.
#[cfg(any(windows, test))]
const MAX_PATH_UNITS: usize = 260;

/// The `\\?\` spelling of `path`, which must be absolute, `\`-separated and free of `.` and `..`.
///
/// `None` below [`MAX_PATH_UNITS`], for a path already in the verbatim or device namespace, and for a
/// relative or drive-relative one, which the prefix would turn into a different path.
#[cfg(any(windows, test))]
fn verbatim_form(path: &[u16]) -> Option<Vec<u16>> {
    const SEPARATOR: u16 = b'\\' as u16;
    const QUESTION: u16 = b'?' as u16;
    const DOT: u16 = b'.' as u16;
    const COLON: u16 = b':' as u16;

    if path.len() < MAX_PATH_UNITS {
        return None;
    }
    let spell = |text: &str| text.encode_utf16().collect::<Vec<u16>>();
    match path {
        [SEPARATOR, SEPARATOR, QUESTION | DOT, SEPARATOR, ..] => None,
        [SEPARATOR, SEPARATOR, share @ ..] => Some([spell(r"\\?\UNC\").as_slice(), share].concat()),
        [drive, COLON, SEPARATOR, ..] if u8::try_from(*drive).is_ok_and(|byte| byte.is_ascii_alphabetic()) => {
            Some([spell(r"\\?\").as_slice(), path].concat())
        }
        _ => None,
    }
}

/// `path` as an absolute path, and whether the Windows API needs it in its `\\?\` spelling.
///
/// Absolute first, because the limit applies to the resolved path and `\\?\` switches `/` and `..` handling off.
#[cfg(windows)]
fn api_spelling(path: &Path) -> (PathBuf, bool) {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

    let Ok(absolute) = std::path::absolute(path) else {
        return (path.to_path_buf(), false);
    };
    let wide: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    match verbatim_form(&wide) {
        Some(verbatim) => (PathBuf::from(OsString::from_wide(&verbatim)), true),
        None => (absolute, false),
    }
}

/// `old` and `new` spelled for the Windows API, or `None` when neither path needs the long form.
#[cfg(windows)]
pub(super) fn api_pair(old: &Path, new: &Path) -> Option<(PathBuf, PathBuf)> {
    let (old, old_long) = api_spelling(old);
    let (new, new_long) = api_spelling(new);
    (old_long || new_long).then_some((old, new))
}

/// `tempfile`'s publish, with the paths of [`api_pair`]: it clears the temporary attribute, then moves.
///
/// A failure leaves `tmp` unpublished in the caller's hands, so its retry loop can run again. The error is a plain
/// `io::Error`, so that `tempfile::PersistError` stays in `persist_at` and crosses no `pub` signature.
#[cfg(windows)]
pub(super) fn persist(
    tmp: &mut tempfile::NamedTempFile,
    (old, new): (PathBuf, PathBuf),
    replace: Replace,
) -> std::io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_TEMPORARY, MOVEFILE_REPLACE_EXISTING, MoveFileExW, SetFileAttributesW,
    };

    let nul_terminated = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (old, new) = (nul_terminated(&old), nul_terminated(&new));

    // A file still marked temporary must not be reported published: it lacks a published file's write-back guarantee.
    // SAFETY: `old` is NUL-terminated and outlives the call.
    if unsafe { SetFileAttributesW(old.as_ptr(), FILE_ATTRIBUTE_NORMAL) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let flags = match replace {
        Replace::Existing => MOVEFILE_REPLACE_EXISTING,
        Replace::Never => 0,
    };
    // SAFETY: `old` and `new` are NUL-terminated and outlive the call.
    if unsafe { MoveFileExW(old.as_ptr(), new.as_ptr(), flags) } == 0 {
        let error = std::io::Error::last_os_error();
        // Best effort: the file is still usable without the attribute, and the move's error is the one to report.
        // SAFETY: `old` is NUL-terminated and outlives the call.
        let _ = unsafe { SetFileAttributesW(old.as_ptr(), FILE_ATTRIBUTE_TEMPORARY) };
        return Err(error);
    }
    // The old name is gone; a drop that deleted it would remove whatever has since taken the name.
    tmp.disable_cleanup(true);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MAX_PATH_UNITS, verbatim_form};

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// `prefix` padded with `a` to exactly `units` UTF-16 units.
    fn padded(prefix: &str, units: usize) -> Vec<u16> {
        let mut path = wide(prefix);
        path.resize(units, u16::from(b'a'));
        path
    }

    fn spelled(path: &[u16]) -> Option<String> {
        verbatim_form(path).map(|verbatim| String::from_utf16(&verbatim).unwrap())
    }

    /// The boundary is `MAX_PATH` itself: 259 units still fit, 260 do not.
    #[test]
    fn verbatim_form_applies_the_prefix_from_260_units_and_not_below() {
        assert_eq!(spelled(&padded(r"C:\dir\", MAX_PATH_UNITS - 1)), None);

        let at_limit = padded(r"C:\dir\", MAX_PATH_UNITS);
        let expected = format!(r"\\?\{}", String::from_utf16(&at_limit).unwrap());
        assert_eq!(spelled(&at_limit), Some(expected));
    }

    /// A UNC share becomes `\\?\UNC\server\share\…`, never `\\?\\\server\…`, which names no share.
    #[test]
    fn verbatim_form_rewrites_a_unc_path_to_the_verbatim_unc_form() {
        let unc = padded(r"\\server\share\dir\", MAX_PATH_UNITS);
        let expected = format!(r"\\?\UNC\{}", &String::from_utf16(&unc).unwrap()[2..]);
        assert_eq!(spelled(&unc), Some(expected));
        assert!(spelled(&unc).unwrap().starts_with(r"\\?\UNC\server\share\dir\"));
    }

    /// A path already in the verbatim or device namespace is final: a second prefix would break it.
    #[test]
    fn verbatim_form_leaves_a_verbatim_or_device_path_alone() {
        for prefix in [r"\\?\C:\dir\", r"\\?\UNC\server\share\", r"\\.\C:\dir\"] {
            assert_eq!(spelled(&padded(prefix, MAX_PATH_UNITS + 40)), None, "{prefix}");
        }
    }

    /// The prefix turns off relative-path handling, so a relative or drive-relative path must not receive it.
    #[test]
    fn verbatim_form_leaves_a_relative_path_alone() {
        for prefix in [r"dir\", r"C:dir\", r"\rooted\", r"..\dir\"] {
            assert_eq!(spelled(&padded(prefix, MAX_PATH_UNITS + 40)), None, "{prefix}");
        }
    }

    /// A lone surrogate is a valid Windows path unit and must pass through byte-for-byte.
    #[test]
    fn verbatim_form_keeps_an_unpaired_surrogate_unit() {
        let mut path = padded(r"C:\dir\", MAX_PATH_UNITS);
        path[10] = 0xD800;

        let verbatim = verbatim_form(&path).unwrap();

        assert_eq!(&verbatim[4..], path.as_slice());
    }
}
