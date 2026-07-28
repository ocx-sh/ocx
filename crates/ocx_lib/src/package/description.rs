// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::{MEDIA_TYPE_PNG, MEDIA_TYPE_SVG, Result};

/// Repository-level description containing a README, optional logo,
/// and manifest-level annotations (title, summary, keywords, etc.).
pub struct Description {
    pub readme: String,
    pub logo: Option<Logo>,
    pub annotations: BTreeMap<String, String>,
}

/// A logo image with its raw bytes and media type.
pub struct Logo {
    pub data: Vec<u8>,
    pub media_type: &'static str,
}

/// Returns the media type for a logo file based on its extension.
fn logo_media_type(path: &Path) -> Result<&'static str> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => Ok(MEDIA_TYPE_PNG),
        Some("svg") => Ok(MEDIA_TYPE_SVG),
        other => Err(super::error::Error::UnsupportedLogoFormat(other.unwrap_or("<no extension>").to_string()).into()),
    }
}

/// The 8-byte PNG signature every PNG file starts with (PNG spec, clause 5.2).
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Reads a logo file and verifies its bytes are the format its extension claims.
///
/// The verification exists because an unchecked `--logo` silently overwrites a
/// published logo with whatever is on disk — a Git LFS pointer left by a checkout
/// without `lfs: true`, an empty file, an HTML error page — and the catalog then
/// renders nothing. Failing here turns that into a loud publish failure.
///
/// # Errors
///
/// - [`Error::UnsupportedLogoFormat`](super::error::Error::UnsupportedLogoFormat)
///   when the extension is neither `png` nor `svg`.
/// - An I/O error carrying the path when the file cannot be read.
/// - [`Error::InvalidLogoContent`](super::error::Error::InvalidLogoContent) when the
///   bytes are not the claimed format.
pub fn load_logo(path: &Path) -> Result<Logo> {
    let media_type = logo_media_type(path)?;
    let data = std::fs::read(path).map_err(|e| crate::error::file_error(path, e))?;
    verify_logo_bytes(path, media_type, &data)?;
    Ok(Logo { data, media_type })
}

/// Verifies logo bytes against the media type its extension claimed.
///
/// PNG is an exact signature check. SVG is UTF-8 text carrying an `<svg` element —
/// a presence check, not a parse: it rejects every non-SVG payload seen in practice
/// (LFS pointers, empty files, HTML, binaries) without owning an XML parser. A text
/// file that merely mentions `<svg` passes; the fix for that is a real XML parse,
/// which no failure so far justifies.
fn verify_logo_bytes(path: &Path, media_type: &'static str, data: &[u8]) -> Result<()> {
    let is_png = media_type == MEDIA_TYPE_PNG;
    let valid = if is_png {
        data.starts_with(&PNG_SIGNATURE)
    } else {
        std::str::from_utf8(data).is_ok_and(|text| text.contains("<svg"))
    };
    if valid {
        return Ok(());
    }
    Err(super::error::Error::InvalidLogoContent {
        path: path.to_path_buf(),
        expected: if is_png { "PNG" } else { "SVG" },
    }
    .into())
}

/// YAML frontmatter extracted from a README.
#[derive(Debug, Default, Deserialize)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub description: Option<String>,
    pub keywords: Option<Keywords>,
}

/// Keywords can be specified as a comma-separated string or a YAML list.
/// Both forms normalize to a comma-separated string.
#[derive(Debug, Clone)]
pub struct Keywords(pub String);

impl<'de> Deserialize<'de> for Keywords {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            String(String),
            List(Vec<String>),
        }

        match Raw::deserialize(deserializer)? {
            Raw::String(s) => Ok(Keywords(s)),
            Raw::List(v) => Ok(Keywords(v.join(","))),
        }
    }
}

/// A README with its frontmatter extracted and body stripped.
pub struct ParsedReadme {
    pub frontmatter: Frontmatter,
    pub body: String,
}

/// Parse YAML frontmatter from a README string.
///
/// Frontmatter must start at line 1 with `---` and end with a matching `---` fence.
/// If parsing fails, a warning is logged and the full content is returned as the body.
pub fn parse_readme(raw: &str) -> ParsedReadme {
    let fence = "---";

    // Must start with `---` followed by a newline.
    let after_open = if let Some(rest) = raw.strip_prefix("---\r\n") {
        rest
    } else if let Some(rest) = raw.strip_prefix("---\n") {
        rest
    } else {
        return ParsedReadme {
            frontmatter: Frontmatter::default(),
            body: raw.to_string(),
        };
    };

    // Find the closing fence.
    let close_pos = after_open
        .find("\n---\n")
        .map(|p| (p, p + "\n---\n".len()))
        .or_else(|| after_open.find("\n---\r\n").map(|p| (p, p + "\n---\r\n".len())))
        .or_else(|| {
            // Closing fence at end of file with no trailing newline.
            if after_open.ends_with("\n---") {
                let p = after_open.len() - fence.len();
                Some((p, after_open.len()))
            } else {
                None
            }
        });

    let Some((yaml_end, body_start)) = close_pos else {
        // No closing fence — treat as no frontmatter.
        return ParsedReadme {
            frontmatter: Frontmatter::default(),
            body: raw.to_string(),
        };
    };

    let yaml_str = &after_open[..yaml_end];
    let body = after_open[body_start..].trim_start_matches('\n').to_string();

    match serde_yaml_ng::from_str::<Frontmatter>(yaml_str) {
        Ok(fm) => ParsedReadme { frontmatter: fm, body },
        Err(e) => {
            tracing::warn!("failed to parse README frontmatter: {e}");
            ParsedReadme {
                frontmatter: Frontmatter::default(),
                body: raw.to_string(),
            }
        }
    }
}

#[cfg(test)]
mod logo_tests {
    use super::*;

    /// What a checkout without `lfs: true` leaves at `assets/logo.png`. This exact
    /// shape was published as a logo and blanked a live catalog entry.
    const LFS_POINTER: &[u8] = b"version https://git-lfs.github.com/spec/v1\noid sha256:c95693dc\nsize 596109\n";

    fn png() -> Vec<u8> {
        let mut data = PNG_SIGNATURE.to_vec();
        data.extend_from_slice(b"\x00\x00\x00\rIHDR");
        data
    }

    fn verify(name: &str, data: &[u8]) -> Result<()> {
        let path = Path::new(name);
        verify_logo_bytes(path, logo_media_type(path)?, data)
    }

    /// `Logo` holds raw bytes and deliberately has no `Debug`, so `unwrap_err` is
    /// unavailable here.
    fn load_error(path: &str) -> String {
        match load_logo(Path::new(path)) {
            Ok(_) => panic!("expected '{path}' to be refused"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn real_png_bytes_pass() {
        assert!(verify("logo.png", &png()).is_ok());
    }

    #[test]
    fn lfs_pointer_named_png_is_rejected() {
        let error = verify("logo.png", LFS_POINTER).unwrap_err().to_string();
        assert!(error.contains("is not a PNG image"), "{error}");
    }

    #[test]
    fn lfs_pointer_named_svg_is_rejected() {
        let error = verify("logo.svg", LFS_POINTER).unwrap_err().to_string();
        assert!(error.contains("is not a SVG image"), "{error}");
    }

    #[test]
    fn empty_file_is_rejected_for_both_formats() {
        assert!(verify("logo.png", b"").is_err());
        assert!(verify("logo.svg", b"").is_err());
    }

    #[test]
    fn svg_passes_with_or_without_an_xml_declaration() {
        assert!(verify("logo.svg", br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#).is_ok());
        assert!(verify("logo.svg", b"<?xml version=\"1.0\"?>\n<svg viewBox=\"0 0 1 1\"></svg>").is_ok());
    }

    #[test]
    fn html_error_page_named_svg_is_rejected() {
        assert!(verify("logo.svg", b"<!DOCTYPE html><html><body>404</body></html>").is_err());
    }

    #[test]
    fn png_bytes_named_svg_are_rejected() {
        // Swapped extensions are caught in both directions.
        assert!(verify("logo.svg", &png()).is_err());
        assert!(verify("logo.png", br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#).is_err());
    }

    #[test]
    fn unsupported_extension_is_still_rejected_before_any_read() {
        let error = load_error("logo.gif");
        assert!(error.contains("unsupported logo format: gif"), "{error}");
    }

    #[test]
    fn missing_file_reports_the_path() {
        let error = load_error("no/such/logo.png");
        assert!(error.contains("no/such/logo.png"), "{error}");
    }

    #[test]
    fn load_logo_returns_the_bytes_and_media_type_it_verified() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logo.png");
        std::fs::write(&path, png()).unwrap();

        let logo = load_logo(&path).unwrap();
        assert_eq!(logo.media_type, MEDIA_TYPE_PNG);
        assert_eq!(logo.data, png());
    }

    #[test]
    fn load_logo_refuses_a_file_whose_bytes_lie() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logo.png");
        std::fs::write(&path, LFS_POINTER).unwrap();

        assert!(load_logo(&path).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_frontmatter_all_keys() {
        let raw = "---\ntitle: CMake\ndescription: Build system\nkeywords: cmake,build,cpp\n---\n# Hello\n";
        let parsed = parse_readme(raw);
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("CMake"));
        assert_eq!(parsed.frontmatter.description.as_deref(), Some("Build system"));
        assert_eq!(
            parsed.frontmatter.keywords.as_ref().map(|k| k.0.as_str()),
            Some("cmake,build,cpp")
        );
        assert_eq!(parsed.body, "# Hello\n");
    }

    #[test]
    fn partial_frontmatter() {
        let raw = "---\ntitle: Only Title\n---\nBody text\n";
        let parsed = parse_readme(raw);
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("Only Title"));
        assert!(parsed.frontmatter.description.is_none());
        assert!(parsed.frontmatter.keywords.is_none());
        assert_eq!(parsed.body, "Body text\n");
    }

    #[test]
    fn no_frontmatter() {
        let raw = "# Just a heading\n\nSome content.\n";
        let parsed = parse_readme(raw);
        assert!(parsed.frontmatter.title.is_none());
        assert_eq!(parsed.body, raw);
    }

    #[test]
    fn unknown_keys_ignored() {
        let raw = "---\ntitle: Tool\nauthor: Someone\nsource: https://example.com\n---\nBody\n";
        let parsed = parse_readme(raw);
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("Tool"));
        assert_eq!(parsed.body, "Body\n");
    }

    #[test]
    fn malformed_yaml_treated_as_no_frontmatter() {
        let raw = "---\ntitle: [unclosed\n---\nBody\n";
        let parsed = parse_readme(raw);
        // Should fall back to treating entire content as body.
        assert!(parsed.frontmatter.title.is_none());
        assert_eq!(parsed.body, raw);
    }

    #[test]
    fn keywords_as_yaml_list() {
        let raw = "---\nkeywords:\n  - cmake\n  - build\n  - cpp\n---\nBody\n";
        let parsed = parse_readme(raw);
        assert_eq!(
            parsed.frontmatter.keywords.as_ref().map(|k| k.0.as_str()),
            Some("cmake,build,cpp")
        );
    }

    #[test]
    fn keywords_as_string() {
        let raw = "---\nkeywords: cmake,build,cpp\n---\nBody\n";
        let parsed = parse_readme(raw);
        assert_eq!(
            parsed.frontmatter.keywords.as_ref().map(|k| k.0.as_str()),
            Some("cmake,build,cpp")
        );
    }

    #[test]
    fn empty_body_after_frontmatter() {
        let raw = "---\ntitle: Empty\n---\n";
        let parsed = parse_readme(raw);
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("Empty"));
        assert_eq!(parsed.body, "");
    }

    #[test]
    fn horizontal_rule_not_treated_as_frontmatter() {
        let raw = "# Heading\n\n---\n\nContent after rule.\n";
        let parsed = parse_readme(raw);
        assert!(parsed.frontmatter.title.is_none());
        assert_eq!(parsed.body, raw);
    }

    #[test]
    fn blank_line_after_frontmatter_stripped() {
        let raw = "---\ntitle: CMake\n---\n\n# CMake\n";
        let parsed = parse_readme(raw);
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("CMake"));
        assert_eq!(parsed.body, "# CMake\n");
    }

    #[test]
    fn crlf_line_endings() {
        let raw = "---\r\ntitle: CRLF\r\n---\r\nBody\r\n";
        let parsed = parse_readme(raw);
        assert_eq!(parsed.frontmatter.title.as_deref(), Some("CRLF"));
        assert_eq!(parsed.body, "Body\r\n");
    }
}
