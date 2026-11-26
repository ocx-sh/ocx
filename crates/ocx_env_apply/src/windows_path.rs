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

#[cfg(test)]
mod tests {
    use super::{SegmentViolation, check_segment, is_reserved_device_name};

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
        assert_eq!(check_segment("com1"), Err(SegmentViolation::DeviceName));
        assert_eq!(check_segment("name."), Err(SegmentViolation::TrailingDotOrSpace));
        assert_eq!(check_segment("name "), Err(SegmentViolation::TrailingDotOrSpace));
        assert_eq!(check_segment("cmake.exe"), Ok(()));
        assert_eq!(check_segment(".."), Err(SegmentViolation::TrailingDotOrSpace));
    }
}
