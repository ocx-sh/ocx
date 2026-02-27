use serde::{Deserialize, Serialize};

use crate::{Error, Result, log};

use super::{Digest, native};

const OCX_SH_REGISTRY: &str = "ocx.sh";

pub const DEFAULT_REGISTRY: &str = OCX_SH_REGISTRY;

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Identifier {
    pub(crate) reference: native::Reference,
}

impl Identifier {
    pub fn new_registry(repository: impl Into<String>, registry: impl Into<String>) -> Self {
        let reference = format!("{}/{}", registry.into(), repository.into())
            .parse::<native::Reference>()
            .expect("Failed to parse reference with registry");
        Self { reference }
    }

    pub fn from_str_with_registry(s: &str, registry: &str) -> Result<Self> {
        let value = prepend_domain(s, registry);
        let reference = value.parse::<native::Reference>()?;
        Ok(Self { reference })
    }

    pub fn clone_with_tag(&self, tag: impl Into<String>) -> Self {
        let reference = native::Reference::with_tag(
            self.reference.registry().into(),
            self.reference.repository().into(),
            tag.into(),
        );
        Self { reference }
    }

    pub fn clone_with_digest(&self, digest: Digest) -> Self {
        let reference = self.reference.clone_with_digest(digest.to_string());
        Self { reference }
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

    /// Returns the tag of the identifier.
    pub fn tag(&self) -> Option<&str> {
        self.reference.tag()
    }

    /// Returns the tag of the identifier, or "latest" if no tag is specified.
    pub fn tag_or_latest(&self) -> &str {
        self.tag().unwrap_or("latest")
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
        let value = prepend_domain(value, DEFAULT_REGISTRY);
        let reference = value.parse::<native::Reference>()?;
        Ok(Self { reference })
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
        s.parse::<native::Reference>()
            .map(|reference| Self { reference })
            .map_err(serde::de::Error::custom)
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
