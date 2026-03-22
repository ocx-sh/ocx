// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;

use serde::{Deserialize, Serialize};

use error::DigestError;

/// Small wrapper of the OCI digest, with some extra convenience methods.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Digest {
    /// A SHA256 digest, which is the most common type of digest used in OCI.
    Sha256(String),
    /// A SHA384 digest, which is less common but still supported in OCI.
    Sha384(String),
    /// A SHA512 digest, which is the least common but supported in OCI.
    Sha512(String),
}

impl Digest {
    /// Creates a new digest from the given data, using the SHA256 algorithm.
    pub fn sha256(data: impl AsRef<[u8]>) -> Self {
        Self::Sha256(hex::encode(<sha2::Sha256 as sha2::Digest>::digest(data)))
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Digest::Sha256(digest) => write!(f, "sha256:{}", digest),
            Digest::Sha384(digest) => write!(f, "sha384:{}", digest),
            Digest::Sha512(digest) => write!(f, "sha512:{}", digest),
        }
    }
}

impl TryFrom<String> for Digest {
    type Error = DigestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        fn check_digest(value: &str, digest: &str, expected_len: usize) -> Result<(), DigestError> {
            if digest.len() == expected_len && digest.chars().all(|c| c.is_ascii_hexdigit()) {
                Ok(())
            } else {
                Err(DigestError::Invalid(value.to_owned()))
            }
        }

        if let Some(digest) = value.strip_prefix("sha256:") {
            check_digest(&value, digest, 64)?;
            Ok(Digest::Sha256(digest.to_string()))
        } else if let Some(digest) = value.strip_prefix("sha384:") {
            check_digest(&value, digest, 96)?;
            Ok(Digest::Sha384(digest.to_string()))
        } else if let Some(digest) = value.strip_prefix("sha512:") {
            check_digest(&value, digest, 128)?;
            Ok(Digest::Sha512(digest.to_string()))
        } else {
            Err(DigestError::Invalid(value))
        }
    }
}

impl Serialize for Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Digest::try_from(s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_digest_parsing() {
        let digest_str = "sha256:43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
        let digest = Digest::try_from(digest_str.to_string()).unwrap();
        assert!(matches!(digest, Digest::Sha256(_)));

        let digest_str =
            "sha384:43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9a1b2c3d4e5f678901234567890123456";
        let digest = Digest::try_from(digest_str.to_string()).unwrap();
        assert!(matches!(digest, Digest::Sha384(_)));

        let digest_str = "sha512:43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d943567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
        let digest = Digest::try_from(digest_str.to_string()).unwrap();
        assert!(matches!(digest, Digest::Sha512(_)));
    }
}
