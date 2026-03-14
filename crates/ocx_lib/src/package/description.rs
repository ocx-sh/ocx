// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::{MEDIA_TYPE_PNG, MEDIA_TYPE_SVG, Result};

/// Prefix for OCX-internal tags that should be hidden from user-facing listings.
pub const INTERNAL_TAG_PREFIX: &str = "__ocx.";

/// The reserved OCI tag for description artifacts.
pub const DESCRIPTION_TAG: &str = "__ocx.desc";

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
pub fn logo_media_type(path: &Path) -> Result<&'static str> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => Ok(MEDIA_TYPE_PNG),
        Some("svg") => Ok(MEDIA_TYPE_SVG),
        other => Err(crate::Error::UndefinedWithMessage(format!(
            "unsupported logo format: {}",
            other.unwrap_or("<no extension>")
        ))),
    }
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
