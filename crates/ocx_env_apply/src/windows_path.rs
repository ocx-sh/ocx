// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The Windows rules for one path segment: the one copy `ocx` and the direct launcher both check against.

/// Why a segment cannot name a file on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentViolation {
    /// `\` is a separator on Windows.
    Backslash,
    /// `:` names a drive or an alternate data stream.
    Colon,
    /// A control character (U+0000 to U+001F) or one of `<>"|?*`, which no Windows file name holds.
    ReservedCharacter,
    /// A reserved device name, with or without an extension.
    DeviceName,
    /// Windows drops a trailing `.` or space, so the name on disk differs.
    TrailingDotOrSpace,
}

/// Checks one segment of a `/`-separated relative path against the Windows file-name rules.
///
/// # Errors
///
/// The first rule the segment breaks, in the order of [`SegmentViolation`]'s variants.
pub fn check_segment(segment: &str) -> Result<(), SegmentViolation> {
    if segment.contains('\\') {
        return Err(SegmentViolation::Backslash);
    }
    if segment.contains(':') {
        return Err(SegmentViolation::Colon);
    }
    if segment.contains(|c: char| c < ' ' || "<>\"|?*".contains(c)) {
        return Err(SegmentViolation::ReservedCharacter);
    }
    if is_reserved_device_name(segment) {
        return Err(SegmentViolation::DeviceName);
    }
    if segment.ends_with(['.', ' ']) {
        return Err(SegmentViolation::TrailingDotOrSpace);
    }
    Ok(())
}

/// Whether `name` is a Windows device name: `CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9` and the
/// superscript `COM¹²³`/`LPT¹²³`, in any ASCII case, with any extension (`NUL.txt`, `com1.tar.gz`).
pub fn is_reserved_device_name(name: &str) -> bool {
    // Windows resolves `NUL.txt` and `NUL .txt` to the device too: only the part before the first dot counts.
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    // Byte-wise: a multi-byte character makes `stem[..3]` a non-boundary slice and panics.
    let bytes = stem.as_bytes();
    if ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|device| bytes.eq_ignore_ascii_case(device.as_bytes()))
    {
        return true;
    }
    let Some((prefix, number)) = bytes.split_at_checked(3) else {
        return false;
    };
    (prefix.eq_ignore_ascii_case(b"COM") || prefix.eq_ignore_ascii_case(b"LPT"))
        // `¹`, `²`, `³` in UTF-8.
        && matches!(number, [b'0'..=b'9'] | [0xC2, 0xB2 | 0xB3 | 0xB9])
}

/// `MAX_PATH`: the UTF-16 units a path may take, its terminating NUL included, where no extended-length form is used.
pub const MAX_PATH: usize = 260;

/// The `\\?\` extended-length form of an absolute Windows `path` that does not fit [`MAX_PATH`], or `None` when the
/// path fits, is relative, or is already in a `\\?\` or `\??\` form.
///
/// For `CreateProcessW`'s `lpApplicationName`: a plain absolute path at or past [`MAX_PATH`] fails there with
/// `ERROR_PATH_NOT_FOUND`. `C:\…` becomes `\\?\C:\…` and `\\server\share\…` becomes `\\?\UNC\server\share\…`.
/// A verbatim path reaches the system unparsed, so `/` becomes `\`, empty and `.` segments drop and `..` removes
/// the segment before it (never the drive or the share), as `GetFullPathNameW` would.
pub fn extended_length_path(path: &str) -> Option<String> {
    // Fits when the path plus the terminating NUL the caller appends stays within MAX_PATH.
    if path.encode_utf16().count() < MAX_PATH {
        return None;
    }
    let unified = path.replace('/', "\\");
    if unified.starts_with(r"\\?\") || unified.starts_with(r"\??\") || unified.starts_with(r"\\.\") {
        return None;
    }
    let (mut out, root_segments, rest) = if let Some(unc) = unified.strip_prefix(r"\\") {
        (String::from(r"\\?\UNC"), 2, unc)
    } else {
        let bytes = unified.as_bytes();
        if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
            return None;
        }
        (format!(r"\\?\{}", &unified[..2]), 0, &unified[3..])
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in rest.split('\\') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.len() > root_segments {
                    segments.pop();
                }
            }
            _ => segments.push(segment),
        }
    }
    if segments.len() < root_segments {
        return None;
    }
    for segment in segments {
        out.push('\\');
        out.push_str(segment);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{MAX_PATH, SegmentViolation, check_segment, extended_length_path, is_reserved_device_name};

    /// `C:\` plus 50-character directories until the whole path is `units` long, ending in `\tool.exe`.
    fn drive_path_of(units: usize) -> String {
        let tail = r"\tool.exe";
        let mut path = String::from("C:");
        while units - path.len() - tail.len() > 52 {
            path.push('\\');
            path.push_str(&"d".repeat(50));
        }
        let last = units - path.len() - tail.len() - 1;
        path.push('\\');
        path.push_str(&"e".repeat(last));
        path.push_str(tail);
        assert_eq!(path.len(), units);
        path
    }

    #[test]
    fn a_path_that_fits_max_path_is_left_alone() {
        assert_eq!(extended_length_path(&drive_path_of(MAX_PATH - 1)), None);
        assert_eq!(extended_length_path(r"C:\tools\tool.exe"), None);
    }

    /// The threshold counts UTF-16 units: `é` is one unit in two UTF-8 bytes, `😀` two units in one `char`.
    #[test]
    fn the_threshold_counts_utf16_units_not_bytes_or_chars() {
        let path = |middle: String| format!(r"C:\{middle}\tool.exe");
        let bmp_259 = path("é".repeat(247));
        let bmp_260 = path("é".repeat(248));
        let astral_259 = path(format!("{}é", "😀".repeat(123)));
        let astral_260 = path("😀".repeat(124));
        for (fits, units) in [(&bmp_259, 259), (&astral_259, 259)] {
            assert_eq!(fits.encode_utf16().count(), units);
            assert_eq!(extended_length_path(fits), None, "{units} units fit");
        }
        for (long, units) in [(&bmp_260, 260), (&astral_260, 260)] {
            assert_eq!(long.encode_utf16().count(), units);
            assert_eq!(
                extended_length_path(long),
                Some(format!(r"\\?\{long}")),
                "{units} units do not fit"
            );
        }
    }

    #[test]
    fn a_drive_path_past_max_path_gains_the_verbatim_prefix() {
        let long = drive_path_of(MAX_PATH);
        assert_eq!(extended_length_path(&long), Some(format!(r"\\?\{long}")));
        let longer = drive_path_of(267);
        assert_eq!(extended_length_path(&longer), Some(format!(r"\\?\{longer}")));
    }

    #[test]
    fn a_unc_path_past_max_path_becomes_verbatim_unc() {
        let long = format!(r"\\server\share\{}\tool.exe", "u".repeat(260));
        let expected = format!(r"\\?\UNC\server\share\{}\tool.exe", "u".repeat(260));
        assert_eq!(extended_length_path(&long), Some(expected));
    }

    #[test]
    fn separators_and_dot_segments_are_normalised_as_full_path_resolution_would() {
        let deep = "x".repeat(260);
        let mixed = format!(r"C:/runfiles\./{deep}/skipped/..\\bin/tool.exe");
        assert_eq!(
            extended_length_path(&mixed),
            Some(format!(r"\\?\C:\runfiles\{deep}\bin\tool.exe"))
        );
        // `..` never climbs above the drive or the share.
        let above = format!(r"C:\..\..\{deep}\tool.exe");
        assert_eq!(extended_length_path(&above), Some(format!(r"\\?\C:\{deep}\tool.exe")));
        let unc_above = format!(r"\\server\share\..\..\{deep}\tool.exe");
        assert_eq!(
            extended_length_path(&unc_above),
            Some(format!(r"\\?\UNC\server\share\{deep}\tool.exe"))
        );
    }

    #[test]
    fn relative_already_verbatim_and_device_paths_are_left_alone() {
        let deep = "r".repeat(300);
        for path in [
            format!(r"relative\{deep}\tool.exe"),
            format!(r"\rooted-without-drive\{deep}\tool.exe"),
            format!(r"C:drive-relative\{deep}\tool.exe"),
            format!(r"\\?\C:\{deep}\tool.exe"),
            format!(r"\??\C:\{deep}\tool.exe"),
            format!(r"\\.\C:\{deep}\tool.exe"),
            // A server with no share names no file.
            format!(r"\\{deep}"),
        ] {
            assert_eq!(extended_length_path(&path), None, "{path}");
        }
    }

    #[test]
    fn device_names_match_in_any_case_and_with_an_extension() {
        for name in [
            "CON",
            "prn",
            "Aux",
            "nul.txt",
            "NUL .txt",
            "COM0",
            "com1.tar.gz",
            "LPT9",
            "lpt0",
            "COM¹",
            "LPT³.log",
        ] {
            assert!(is_reserved_device_name(name), "{name}");
        }
    }

    #[test]
    fn near_device_names_are_ordinary() {
        for name in [
            "CONSOLE", "COM10", "LPT", "COMX", "NULL", "aux_", "c", "", "é", "COMé", "my.CON",
        ] {
            assert!(!is_reserved_device_name(name), "{name}");
        }
    }

    #[test]
    fn segments_report_the_first_rule_they_break() {
        assert_eq!(check_segment("a\\b"), Err(SegmentViolation::Backslash));
        assert_eq!(check_segment("C:"), Err(SegmentViolation::Colon));
        for reserved in [
            "a<b",
            "a>b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
            "a\u{1}b",
            "a\u{1b}[31m",
            "\u{1f}",
        ] {
            assert_eq!(
                check_segment(reserved),
                Err(SegmentViolation::ReservedCharacter),
                "{reserved:?}"
            );
        }
        assert_eq!(
            check_segment("a\u{7f}b"),
            Ok(()),
            "DEL is a legal Windows file-name character"
        );
        assert_eq!(check_segment("com1"), Err(SegmentViolation::DeviceName));
        assert_eq!(check_segment("name."), Err(SegmentViolation::TrailingDotOrSpace));
        assert_eq!(check_segment("name "), Err(SegmentViolation::TrailingDotOrSpace));
        assert_eq!(check_segment("cmake.exe"), Ok(()));
        assert_eq!(check_segment(".."), Err(SegmentViolation::TrailingDotOrSpace));
    }
}
