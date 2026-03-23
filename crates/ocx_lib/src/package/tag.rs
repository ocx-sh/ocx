// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use crate::{oci::Digest, package::version};

/// Prefix for OCX-internal tags that should be hidden from user-facing listings.
const INTERNAL_TAG_PREFIX: &str = "__ocx.";

/// Known OCX-internal tag types.
///
/// Internal tags are prefixed with `__ocx.` and used for metadata artifacts.
/// Unknown internal tags (from newer OCX versions) are preserved as
/// [`Unknown`](InternalTag::Unknown) rather than causing errors.
#[derive(Debug, Clone)]
pub enum InternalTag {
    /// Package description artifact (`__ocx.desc`).
    Description,
    /// An internal tag not recognized by this version of OCX.
    Unknown(String),
}

impl InternalTag {
    /// The OCI tag string for description artifacts.
    pub const DESCRIPTION_TAG: &str = "__ocx.desc";

    fn from_tag(value: &str) -> Self {
        match value {
            Self::DESCRIPTION_TAG => InternalTag::Description,
            _ => InternalTag::Unknown(value.to_string()),
        }
    }
}

impl std::fmt::Display for InternalTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InternalTag::Description => write!(f, "{}", Self::DESCRIPTION_TAG),
            InternalTag::Unknown(tag) => write!(f, "{}", tag),
        }
    }
}

/// Semantic classification of an OCI tag string.
///
/// Parsed from a raw tag string via `Tag::from(String)`. The parse order is:
/// 1. `"latest"` → [`Latest`](Tag::Latest)
/// 2. Internal OCX tag (`__ocx.*`) → [`Internal`](Tag::Internal)
/// 3. Version-parseable (digit-first or variant-prefixed) → [`Version`](Tag::Version)
/// 4. Canonical digest (`sha256:...`) → [`Canonical`](Tag::Canonical)
/// 5. Anything else → [`Other`](Tag::Other)
///
/// Bare variant names (e.g., `"debug"`, `"canary"`) fall into [`Other`](Tag::Other).
/// Variant semantics are determined at a higher layer (mirror spec, package
/// annotations) where declared variants are known — the `Tag` enum is purely
/// syntactic and does not guess intent.
#[derive(Debug, Clone)]
pub enum Tag {
    /// The literal `"latest"` tag — latest version of the default variant.
    Latest,
    /// An OCX-internal tag (prefixed with `__ocx.`). Used for metadata artifacts
    /// like package descriptions. Filtered from user-facing tag listings.
    Internal(InternalTag),
    /// A semantic version, optionally with a variant prefix.
    /// Examples: `"3.28.1"`, `"3.28.1-alpha_b1"`, `"debug-3.12.5"`.
    Version(version::Version),
    /// A content-addressable digest tag (e.g., `"sha256:abcdef..."`).
    Canonical(Digest),
    /// Any tag that doesn't match the above patterns.
    /// Includes bare variant names (`"debug"`) and arbitrary user-chosen tags (`"custom-tag"`).
    Other(String),
}

const LATEST_STR: &str = "latest";

impl Tag {
    /// Returns `true` if this is an OCX-internal tag (prefixed with `__ocx.`).
    pub fn is_internal(&self) -> bool {
        matches!(self, Tag::Internal(_))
    }

    /// Returns `true` if the raw tag string is an OCX-internal tag.
    ///
    /// Cheap `&str` check for use in filter pipelines where constructing
    /// a full `Tag` would be wasteful.
    pub fn is_internal_str(tag: &str) -> bool {
        tag.starts_with(INTERNAL_TAG_PREFIX)
    }
}

impl From<String> for Tag {
    fn from(value: String) -> Self {
        if value == LATEST_STR {
            Tag::Latest
        } else if value.starts_with(INTERNAL_TAG_PREFIX) {
            Tag::Internal(InternalTag::from_tag(&value))
        } else if let Some(version) = version::Version::parse(value.as_ref()) {
            Tag::Version(version)
        } else if let Ok(digest) = Digest::try_from(&value) {
            Tag::Canonical(digest)
        } else {
            Tag::Other(value)
        }
    }
}

impl std::fmt::Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s: String = self.clone().into();
        write!(f, "{}", s)
    }
}

impl From<Tag> for String {
    fn from(val: Tag) -> Self {
        match val {
            Tag::Latest => LATEST_STR.to_string(),
            Tag::Internal(internal) => internal.to_string(),
            Tag::Version(version) => version.to_string(),
            Tag::Canonical(canonical) => canonical.to_string(),
            Tag::Other(other) => other,
        }
    }
}

impl Serialize for Tag {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s: String = self.clone().into();
        serializer.serialize_str(&s)
    }
}

impl<'de> Deserialize<'de> for Tag {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Tag::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tag_parsing() {
        let latest_tag = Tag::from("latest".to_string());
        assert!(matches!(latest_tag, Tag::Latest));
        assert_eq!(latest_tag.to_string(), "latest");

        let version_tag = Tag::from("1.2.3-alpha".to_string());
        assert!(matches!(version_tag, Tag::Version(_)));
        assert_eq!(version_tag.to_string(), "1.2.3-alpha");

        let canonical_tag =
            Tag::from("sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string());
        assert!(matches!(canonical_tag, Tag::Canonical(_)));
        assert_eq!(
            canonical_tag.to_string(),
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );

        let other_tag = Tag::from("custom-tag".to_string());
        assert!(matches!(other_tag, Tag::Other(_)));
        assert_eq!(other_tag.to_string(), "custom-tag");
    }

    #[test]
    fn tag_internal_description() {
        let tag = Tag::from("__ocx.desc".to_string());
        assert!(tag.is_internal());
        assert!(matches!(tag, Tag::Internal(InternalTag::Description)));
        assert_eq!(tag.to_string(), "__ocx.desc");
    }

    #[test]
    fn tag_internal_unknown_forward_compat() {
        let tag = Tag::from("__ocx.sbom".to_string());
        assert!(tag.is_internal());
        assert!(matches!(tag, Tag::Internal(InternalTag::Unknown(_))));
        assert_eq!(tag.to_string(), "__ocx.sbom");
    }

    #[test]
    fn tag_is_internal_str() {
        assert!(Tag::is_internal_str("__ocx.desc"));
        assert!(Tag::is_internal_str("__ocx.future"));
        assert!(!Tag::is_internal_str("latest"));
        assert!(!Tag::is_internal_str("3.28.1"));
        assert!(!Tag::is_internal_str("debug"));
    }

    #[test]
    fn tag_non_internal() {
        for name in ["latest", "3.28.1", "debug-3.12", "custom-tag", "debug"] {
            let tag = Tag::from(name.to_string());
            assert!(!tag.is_internal(), "'{name}' should not be internal");
        }
    }

    // ── Variant tag parsing tests ─────────────────────────────────

    #[test]
    fn tag_variant_prefixed_version() {
        let tag = Tag::from("debug-3.12".to_string());
        assert!(matches!(tag, Tag::Version(_)));
        if let Tag::Version(v) = &tag {
            assert_eq!(v.variant(), Some("debug"));
            assert_eq!(v.major(), 3);
            assert_eq!(v.minor(), Some(12));
        }
        assert_eq!(tag.to_string(), "debug-3.12");
    }

    #[test]
    fn tag_variant_prefixed_with_build() {
        let tag = Tag::from("pgo.lto-3.12.5_b1".to_string());
        assert!(matches!(tag, Tag::Version(_)));
        if let Tag::Version(v) = &tag {
            assert_eq!(v.variant(), Some("pgo.lto"));
        }
        assert_eq!(tag.to_string(), "pgo.lto-3.12.5_b1");
    }

    #[test]
    fn tag_bare_variant_is_other() {
        for name in ["debug", "pgo.lto", "slim", "canary"] {
            let tag = Tag::from(name.to_string());
            assert!(matches!(tag, Tag::Other(_)), "'{name}' should be Tag::Other");
            assert_eq!(tag.to_string(), name);
        }
    }

    #[test]
    fn tag_custom_tag_still_other() {
        let tag = Tag::from("custom-tag".to_string());
        assert!(matches!(tag, Tag::Other(_)));

        let tag = Tag::from("my-custom-thing".to_string());
        assert!(matches!(tag, Tag::Other(_)));
    }

    #[test]
    fn tag_backward_compat_existing_formats() {
        assert!(matches!(Tag::from("latest".to_string()), Tag::Latest));
        assert!(matches!(Tag::from("3.28.1".to_string()), Tag::Version(_)));
        assert!(matches!(Tag::from("3.28.1-alpha".to_string()), Tag::Version(_)));
        assert!(matches!(Tag::from("3.28.1_b1".to_string()), Tag::Version(_)));
        assert!(matches!(Tag::from("3.28.1-alpha_b1".to_string()), Tag::Version(_)));
        assert!(matches!(
            Tag::from("sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string()),
            Tag::Canonical(_)
        ));
        assert!(matches!(Tag::from("custom-tag".to_string()), Tag::Other(_)));
        assert!(matches!(Tag::from("__ocx.desc".to_string()), Tag::Internal(_)));
    }
}
