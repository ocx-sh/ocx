// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use crate::{Error, Result, log};

use super::{Digest, native};

const OCX_SH_REGISTRY: &str = "ocx.sh";

pub const DEFAULT_REGISTRY: &str = OCX_SH_REGISTRY;

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Identifier {
    pub(crate) reference: native::Reference,
    explicit_tag: bool,
}

impl Identifier {
    pub fn new_registry(repository: impl Into<String>, registry: impl Into<String>) -> Self {
        let reference = format!("{}/{}", registry.into(), repository.into())
            .parse::<native::Reference>()
            .expect("Failed to parse reference with registry");
        Self {
            reference,
            explicit_tag: false,
        }
    }

    pub fn from_str_with_registry(s: &str, registry: &str) -> Result<Self> {
        let explicit_tag = has_explicit_tag(s);
        let value = prepend_domain(s, registry);
        let reference = value.parse::<native::Reference>()?;
        Ok(Self {
            reference,
            explicit_tag,
        })
    }

    pub fn clone_with_tag(&self, tag: impl Into<String>) -> Self {
        let reference = native::Reference::with_tag(
            self.reference.registry().into(),
            self.reference.repository().into(),
            tag.into(),
        );
        Self {
            reference,
            explicit_tag: true,
        }
    }

    pub fn clone_with_digest(&self, digest: Digest) -> Self {
        let reference = self.reference.clone_with_digest(digest.to_string());
        Self {
            reference,
            explicit_tag: self.explicit_tag,
        }
    }

    pub fn registry(&self) -> &str {
        self.reference.registry()
    }

    /// The path within the registry, e.g. "library/ubuntu".
    /// This includes the name of the package.
    pub fn repository(&self) -> &str {
        self.reference.repository()
    }

    /// Returns the name of the identifier, which is the last segment of the repository.
    pub fn name(&self) -> Option<String> {
        self.repository().split('/').next_back().map(|s| s.to_string())
    }

    /// Returns the tag of the identifier, or `None` if no tag was specified.
    ///
    /// The underlying OCI reference parser (`oci-spec`) automatically fills in
    /// `"latest"` when neither a tag nor a digest is present.  This method
    /// corrects for that: it returns `None` when the caller did not supply
    /// a tag in the input string.
    pub fn tag(&self) -> Option<&str> {
        if self.explicit_tag { self.reference.tag() } else { None }
    }

    /// Returns the tag of the identifier, or `"latest"` if no tag was specified.
    pub fn tag_or_latest(&self) -> &str {
        self.reference.tag().unwrap_or("latest")
    }

    /// Returns the digest of the identifier, if any.
    pub fn digest(&self) -> Option<Digest> {
        match self.reference.digest() {
            Some(digest) => match Digest::try_from(digest.to_string()) {
                Ok(digest) => Some(digest),
                Err(e) => {
                    log::warn!("{}", e);
                    None
                }
            },
            None => None,
        }
    }
}

impl std::fmt::Display for Identifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reference)
    }
}

impl std::str::FromStr for Identifier {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let explicit_tag = has_explicit_tag(value);
        let value = prepend_domain(value, DEFAULT_REGISTRY);
        let reference = value.parse::<native::Reference>()?;
        Ok(Self {
            reference,
            explicit_tag,
        })
    }
}

impl Serialize for Identifier {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.reference.to_string())
    }
}

impl<'de> Deserialize<'de> for Identifier {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let explicit_tag = has_explicit_tag(&s);
        s.parse::<native::Reference>()
            .map(|reference| Self {
                reference,
                explicit_tag,
            })
            .map_err(serde::de::Error::custom)
    }
}

/// Detects whether a raw input string contains an explicit tag (`:tag`).
///
/// Examines the name portion (after the last `/`) for a `:` separator,
/// excluding any digest (`@sha256:...`).
fn has_explicit_tag(raw: &str) -> bool {
    let without_digest = raw.split('@').next().unwrap_or(raw);
    let name_part = without_digest.rsplit('/').next().unwrap_or(without_digest);
    name_part.contains(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_explicit_tag_detection() {
        // Bare names — no tag
        assert!(!has_explicit_tag("python"));
        assert!(!has_explicit_tag("localhost:5000/python"));
        assert!(!has_explicit_tag("python@sha256:abc123"));

        // Explicit tags
        assert!(has_explicit_tag("python:3.12"));
        assert!(has_explicit_tag("python:latest"));
        assert!(has_explicit_tag("localhost:5000/python:3.12"));
        assert!(has_explicit_tag("python:3.12@sha256:abc123"));
    }

    #[test]
    fn tag_reflects_explicit_input() {
        let bare: Identifier = "python".parse().unwrap();
        assert_eq!(bare.tag(), None);
        assert_eq!(bare.tag_or_latest(), "latest");

        let tagged: Identifier = "python:3.12".parse().unwrap();
        assert_eq!(tagged.tag(), Some("3.12"));
        assert_eq!(tagged.tag_or_latest(), "3.12");

        let explicit_latest: Identifier = "python:latest".parse().unwrap();
        assert_eq!(explicit_latest.tag(), Some("latest"));
        assert_eq!(explicit_latest.tag_or_latest(), "latest");
    }

    #[test]
    fn from_str_with_registry_preserves_tag_presence() {
        let bare = Identifier::from_str_with_registry("python", "localhost:5000").unwrap();
        assert_eq!(bare.tag(), None);
        assert_eq!(bare.tag_or_latest(), "latest");

        let tagged = Identifier::from_str_with_registry("python:3.12", "localhost:5000").unwrap();
        assert_eq!(tagged.tag(), Some("3.12"));
    }

    #[test]
    fn clone_with_tag_always_explicit() {
        let bare: Identifier = "python".parse().unwrap();
        assert_eq!(bare.tag(), None);

        let tagged = bare.clone_with_tag("3.12");
        assert_eq!(tagged.tag(), Some("3.12"));
    }
}

fn prepend_domain(name: &str, domain: &str) -> String {
    match name.split_once('/') {
        None => format!("{domain}/{name}"),
        Some((left, _)) => {
            if !(left.contains('.') || left.contains(':')) && left != "localhost" {
                format!("{domain}/{name}")
            } else {
                name.into()
            }
        }
    }
}
