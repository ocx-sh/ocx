// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;

use serde::{Deserialize, Serialize};

use error::DigestError;

const DIGEST_SHORT_LEN: usize = 12;
/// Small wrapper of the OCI digest, with some extra convenience methods.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Digest {
    /// A SHA256 digest, which is the most common type of digest used in OCI.
    Sha256(String),
    /// A SHA384 digest, which is less common but still supported in OCI.
    Sha384(String),
    /// A SHA512 digest, which is the least common but supported in OCI.
    Sha512(String),
}

impl Digest {
    /// All supported digest algorithm names, matching the enum variants.
    ///
    /// This is the single source of truth for algorithm string identifiers.
    /// Used by CAS path validation, Display, and TryFrom implementations.
    pub const ALGORITHMS: &[&str] = &["sha256", "sha384", "sha512"];

    /// Creates a new digest from the given data, using the SHA256 algorithm.
    pub fn sha256(data: impl AsRef<[u8]>) -> Self {
        Self::Sha256(hex::encode(<sha2::Sha256 as sha2::Digest>::digest(data)))
    }

    /// Returns the algorithm prefix and hex string without allocating.
    ///
    /// Algorithm strings are sourced from [`Self::ALGORITHMS`] — the single
    /// source of truth for algorithm names.
    pub fn parts(&self) -> (&str, &str) {
        match self {
            Digest::Sha256(hex) => (Self::ALGORITHMS[0], hex),
            Digest::Sha384(hex) => (Self::ALGORITHMS[1], hex),
            Digest::Sha512(hex) => (Self::ALGORITHMS[2], hex),
        }
    }

    /// Algorithm prefix, e.g. `"sha256"`.
    pub fn algorithm(&self) -> &str {
        self.parts().0
    }

    /// Returns the hex string of the digest (without the algorithm prefix).
    pub fn hex(&self) -> &str {
        self.parts().1
    }

    /// Returns the first [`DIGEST_SHORT_LEN`] characters of the hex string for display purposes.
    pub fn short_hex(&self) -> &str {
        &self.hex()[..DIGEST_SHORT_LEN]
    }

    /// Returns a truncated digest string (`algorithm:short_hex`) of [`DIGEST_SHORT_LEN`] hex
    /// characters for display purposes.
    pub fn to_short_string(&self) -> String {
        let (alg, hex) = self.parts();
        format!("{}:{}", alg, &hex[..DIGEST_SHORT_LEN])
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (algorithm, hex) = self.parts();
        write!(f, "{algorithm}:{hex}")
    }
}

impl TryFrom<&str> for Digest {
    type Error = DigestError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        fn check_digest(value: &str, digest: &str, expected_len: usize) -> Result<(), DigestError> {
            if digest.len() == expected_len && digest.chars().all(|c| c.is_ascii_hexdigit()) {
                Ok(())
            } else {
                Err(DigestError::Invalid(value.to_owned()))
            }
        }

        if let Some(hex) = value
            .strip_prefix(Digest::ALGORITHMS[0])
            .and_then(|s| s.strip_prefix(':'))
        {
            check_digest(value, hex, 64)?;
            Ok(Digest::Sha256(hex.to_string()))
        } else if let Some(hex) = value
            .strip_prefix(Digest::ALGORITHMS[1])
            .and_then(|s| s.strip_prefix(':'))
        {
            check_digest(value, hex, 96)?;
            Ok(Digest::Sha384(hex.to_string()))
        } else if let Some(hex) = value
            .strip_prefix(Digest::ALGORITHMS[2])
            .and_then(|s| s.strip_prefix(':'))
        {
            check_digest(value, hex, 128)?;
            Ok(Digest::Sha512(hex.to_string()))
        } else {
            Err(DigestError::Invalid(value.to_owned()))
        }
    }
}

impl TryFrom<String> for Digest {
    type Error = DigestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl TryFrom<&String> for Digest {
    type Error = DigestError;

    fn try_from(value: &String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
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

impl schemars::JsonSchema for Digest {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Digest")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "OCI content-addressed digest (e.g. 'sha256:abcdef...').",
            "pattern": "^sha(256|384|512):[0-9a-f]+$"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_digest_parsing() {
        let digest_str = "sha256:43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
        let digest = Digest::try_from(digest_str).unwrap();
        assert!(matches!(digest, Digest::Sha256(_)));

        let digest_str =
            "sha384:43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9a1b2c3d4e5f678901234567890123456";
        let digest = Digest::try_from(digest_str).unwrap();
        assert!(matches!(digest, Digest::Sha384(_)));

        let digest_str = "sha512:43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d943567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
        let digest = Digest::try_from(digest_str).unwrap();
        assert!(matches!(digest, Digest::Sha512(_)));
    }

    #[test]
    fn algorithms_constant_covers_all_variants() {
        // If a new variant is added to Digest, this test will fail until
        // ALGORITHMS is updated to include the new algorithm string.
        // The round-trip (Display → TryFrom) ensures that `parts()`, `Display`,
        // and `TryFrom` all agree on the algorithm prefix strings.
        let all = [
            Digest::Sha256("a".repeat(64)),
            Digest::Sha384("b".repeat(96)),
            Digest::Sha512("c".repeat(128)),
        ];
        for d in &all {
            assert!(
                Digest::ALGORITHMS.contains(&d.algorithm()),
                "Digest::ALGORITHMS is missing '{}'",
                d.algorithm()
            );
            // Round-trip: parts() → Display → TryFrom. If any of the three
            // sites uses a different string, this breaks.
            let displayed = d.to_string();
            let parsed = Digest::try_from(displayed.as_str())
                .unwrap_or_else(|_| panic!("Display output '{}' failed to round-trip through TryFrom", displayed));
            assert_eq!(&parsed, d, "round-trip mismatch for {}", d.algorithm());
        }
        assert_eq!(
            Digest::ALGORITHMS.len(),
            all.len(),
            "Digest::ALGORITHMS has entries not covered by test variants"
        );
    }

    #[test]
    fn algorithm_returns_prefix() {
        let d256 = Digest::Sha256("a".repeat(64));
        let d384 = Digest::Sha384("b".repeat(96));
        let d512 = Digest::Sha512("c".repeat(128));
        assert_eq!(d256.algorithm(), "sha256");
        assert_eq!(d384.algorithm(), "sha384");
        assert_eq!(d512.algorithm(), "sha512");
    }

    #[test]
    fn hex_returns_raw_hex_without_prefix() {
        let hex = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
        let digest = Digest::Sha256(hex.to_string());
        assert_eq!(digest.hex(), hex);
    }

    #[test]
    fn short_hex_truncates_to_constant_length() {
        let hex = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
        let digest = Digest::Sha256(hex.to_string());
        assert_eq!(digest.short_hex(), &hex[..DIGEST_SHORT_LEN]);
        assert_eq!(digest.short_hex().len(), DIGEST_SHORT_LEN);
    }

    #[test]
    fn to_short_string_includes_algorithm_and_truncated_hex() {
        let hex = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
        let digest = Digest::Sha256(hex.to_string());
        assert_eq!(digest.to_short_string(), format!("sha256:{}", &hex[..DIGEST_SHORT_LEN]));
    }

    #[test]
    fn to_short_string_works_for_all_algorithms() {
        let d384 = Digest::Sha384("ab".repeat(48));
        assert!(d384.to_short_string().starts_with("sha384:"));
        assert_eq!(d384.to_short_string().len(), "sha384:".len() + DIGEST_SHORT_LEN);

        let d512 = Digest::Sha512("cd".repeat(64));
        assert!(d512.to_short_string().starts_with("sha512:"));
        assert_eq!(d512.to_short_string().len(), "sha512:".len() + DIGEST_SHORT_LEN);
    }
}
