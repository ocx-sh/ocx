// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use crate::package::version;

#[derive(Debug, Clone)]
pub enum Tag {
    Latest,
    Canary,
    Version(version::Version),
    Canonical(String),
    Other(String),
}

const LATEST_STR: &str = "latest";
const CANARY_STR: &str = "canary";

impl From<String> for Tag {
    fn from(value: String) -> Self {
        use regex::Regex;
        use std::sync::LazyLock;

        static CANONICAL_TAG_REGEX: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^sha256:[a-z0-9]{64}$").expect("Invalid canonical tag regex!"));

        if value == LATEST_STR {
            Tag::Latest
        } else if value == CANARY_STR {
            Tag::Canary
        } else if let Some(version) = version::Version::parse(value.as_ref()) {
            Tag::Version(version)
        } else if CANONICAL_TAG_REGEX.is_match(value.as_ref()) {
            Tag::Canonical(value)
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
            Tag::Canary => CANARY_STR.to_string(),
            Tag::Version(version) => version.to_string(),
            Tag::Canonical(canonical) => canonical,
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

        let canary_tag = Tag::from("canary".to_string());
        assert!(matches!(canary_tag, Tag::Canary));
        assert_eq!(canary_tag.to_string(), "canary");

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
}
