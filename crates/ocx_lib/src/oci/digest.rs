// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;

use serde::{Deserialize, Serialize};

use error::DigestError;

const DIGEST_SHORT_LEN: usize = 12;

/// Supported digest hash algorithms. The single source of truth for the
/// algorithm concept: every place that hashes bytes or a file goes through
/// one of [`Algorithm::hash`], [`Algorithm::hash_file`], or
/// [`Algorithm::hash_file_read`], and every runtime dispatch on the
/// algorithm identity flows through one of these methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Algorithm {
    Sha256,
    Sha384,
    Sha512,
}

impl Algorithm {
    /// Every supported algorithm, in parse/Display-prefix order.
    pub const ALL: &'static [Self] = &[Self::Sha256, Self::Sha384, Self::Sha512];

    /// OCI-spec algorithm prefix, e.g. `"sha256"`.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha384 => "sha384",
            Self::Sha512 => "sha512",
        }
    }

    /// Expected hex-string length for this algorithm's digest output.
    pub const fn hex_len(self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Sha384 => 96,
            Self::Sha512 => 128,
        }
    }

    /// Hashes `bytes` in memory with this algorithm.
    pub fn hash(self, bytes: impl AsRef<[u8]>) -> Digest {
        match self {
            Self::Sha256 => Digest::Sha256(hex::encode(<sha2::Sha256 as sha2::Digest>::digest(bytes))),
            Self::Sha384 => Digest::Sha384(hex::encode(<sha2::Sha384 as sha2::Digest>::digest(bytes))),
            Self::Sha512 => Digest::Sha512(hex::encode(<sha2::Sha512 as sha2::Digest>::digest(bytes))),
        }
    }

    /// Streams the file at `path` through this algorithm without loading
    /// it into memory. The read-and-hash loop runs on
    /// [`tokio::task::spawn_blocking`] so neither synchronous disk I/O
    /// nor the hash state update stalls the async executor.
    pub async fn hash_file(self, path: &std::path::Path) -> std::io::Result<Digest> {
        let hex = match self {
            Self::Sha256 => hash_file_hex::<sha2::Sha256>(path.to_path_buf()).await?,
            Self::Sha384 => hash_file_hex::<sha2::Sha384>(path.to_path_buf()).await?,
            Self::Sha512 => hash_file_hex::<sha2::Sha512>(path.to_path_buf()).await?,
        };
        Ok(match self {
            Self::Sha256 => Digest::Sha256(hex),
            Self::Sha384 => Digest::Sha384(hex),
            Self::Sha512 => Digest::Sha512(hex),
        })
    }

    /// Reads a file into memory and hashes it in a single disk pass,
    /// returning `(bytes, digest)`. Used by the push path where the
    /// blob body must be held in memory for upload and must also be
    /// digested.
    pub async fn hash_file_read(self, path: &std::path::Path) -> std::io::Result<(Vec<u8>, Digest)> {
        let (bytes, hex) = match self {
            Self::Sha256 => hash_file_read_hex::<sha2::Sha256>(path.to_path_buf()).await?,
            Self::Sha384 => hash_file_read_hex::<sha2::Sha384>(path.to_path_buf()).await?,
            Self::Sha512 => hash_file_read_hex::<sha2::Sha512>(path.to_path_buf()).await?,
        };
        let digest = match self {
            Self::Sha256 => Digest::Sha256(hex),
            Self::Sha384 => Digest::Sha384(hex),
            Self::Sha512 => Digest::Sha512(hex),
        };
        Ok((bytes, digest))
    }
}

impl std::fmt::Display for Algorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.prefix())
    }
}

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
    /// Returns the algorithm prefix and hex string without allocating.
    pub fn parts(&self) -> (&str, &str) {
        (self.algorithm().prefix(), self.hex())
    }

    /// The algorithm that produced this digest.
    pub fn algorithm(&self) -> Algorithm {
        match self {
            Digest::Sha256(_) => Algorithm::Sha256,
            Digest::Sha384(_) => Algorithm::Sha384,
            Digest::Sha512(_) => Algorithm::Sha512,
        }
    }

    /// Returns the hex string of the digest (without the algorithm prefix).
    pub fn hex(&self) -> &str {
        match self {
            Digest::Sha256(hex) | Digest::Sha384(hex) | Digest::Sha512(hex) => hex,
        }
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

/// Streams a file through any `sha2::Digest` hasher in 64 KiB chunks on a
/// blocking thread, returning the lowercase hex output. Shared across the
/// [`Algorithm::hash_file`] variants so the read-and-hash loop is not
/// triplicated.
async fn hash_file_hex<H>(path: std::path::PathBuf) -> std::io::Result<String>
where
    H: sha2::Digest + Send + 'static,
    sha2::digest::Output<H>: AsRef<[u8]>,
{
    tokio::task::spawn_blocking(move || {
        use std::io::Read;

        let mut file = std::fs::File::open(&path)?;
        let mut hasher = H::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            sha2::Digest::update(&mut hasher, &buf[..n]);
        }
        Ok(hex::encode(hasher.finalize()))
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Like [`hash_file_hex`], but also returns the file bytes. Each 64 KiB
/// chunk is hashed as it is read, so there is no second pass over the
/// buffer after I/O completes.
async fn hash_file_read_hex<H>(path: std::path::PathBuf) -> std::io::Result<(Vec<u8>, String)>
where
    H: sha2::Digest + Send + 'static,
    sha2::digest::Output<H>: AsRef<[u8]>,
{
    tokio::task::spawn_blocking(move || {
        use std::io::Read;

        let mut file = std::fs::File::open(&path)?;
        let expected_len = file.metadata().map(|m| m.len() as usize).unwrap_or(0);
        let mut bytes = Vec::with_capacity(expected_len);
        let mut hasher = H::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            sha2::Digest::update(&mut hasher, &buf[..n]);
            bytes.extend_from_slice(&buf[..n]);
        }
        Ok((bytes, hex::encode(hasher.finalize())))
    })
    .await
    .map_err(std::io::Error::other)?
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
        for algorithm in Algorithm::ALL {
            if let Some(hex) = value.strip_prefix(algorithm.prefix()).and_then(|s| s.strip_prefix(':')) {
                if hex.len() != algorithm.hex_len() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(DigestError::Invalid(value.to_owned()));
                }
                return Ok(match algorithm {
                    Algorithm::Sha256 => Digest::Sha256(hex.to_string()),
                    Algorithm::Sha384 => Digest::Sha384(hex.to_string()),
                    Algorithm::Sha512 => Digest::Sha512(hex.to_string()),
                });
            }
        }
        Err(DigestError::Invalid(value.to_owned()))
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
    fn algorithm_all_covers_all_variants() {
        // If a new variant is added to Digest, this test will fail until
        // Algorithm::ALL is updated to include the new algorithm.
        // The round-trip (Display → TryFrom) ensures that `parts()`, `Display`,
        // and `TryFrom` all agree on the algorithm prefix strings.
        let all = [
            Digest::Sha256("a".repeat(64)),
            Digest::Sha384("b".repeat(96)),
            Digest::Sha512("c".repeat(128)),
        ];
        for d in &all {
            assert!(
                Algorithm::ALL.contains(&d.algorithm()),
                "Algorithm::ALL is missing '{}'",
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
            Algorithm::ALL.len(),
            all.len(),
            "Algorithm::ALL has entries not covered by test variants"
        );
    }

    #[test]
    fn algorithm_returns_prefix() {
        let d256 = Digest::Sha256("a".repeat(64));
        let d384 = Digest::Sha384("b".repeat(96));
        let d512 = Digest::Sha512("c".repeat(128));
        assert_eq!(d256.algorithm(), Algorithm::Sha256);
        assert_eq!(d384.algorithm(), Algorithm::Sha384);
        assert_eq!(d512.algorithm(), Algorithm::Sha512);
        assert_eq!(d256.algorithm().to_string(), "sha256");
        assert_eq!(d384.algorithm().to_string(), "sha384");
        assert_eq!(d512.algorithm().to_string(), "sha512");
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

    #[tokio::test]
    async fn hash_file_read_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        tokio::fs::write(&path, b"").await.unwrap();

        let (bytes, digest) = Algorithm::Sha256.hash_file_read(&path).await.unwrap();
        assert!(bytes.is_empty());
        assert_eq!(digest, Algorithm::Sha256.hash(&[] as &[u8]));
    }

    #[tokio::test]
    async fn hash_file_read_small_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small");
        let data = b"hello world".to_vec();
        tokio::fs::write(&path, &data).await.unwrap();

        let (bytes, digest) = Algorithm::Sha256.hash_file_read(&path).await.unwrap();
        assert_eq!(bytes, data);
        assert_eq!(digest, Algorithm::Sha256.hash(&data));
    }

    #[tokio::test]
    async fn hash_file_read_larger_than_chunk() {
        // Exceeds the 64 KiB internal read buffer so the loop runs multiple iterations.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big");
        let data: Vec<u8> = (0..(64 * 1024 * 3 + 7)).map(|i| (i % 251) as u8).collect();
        tokio::fs::write(&path, &data).await.unwrap();

        let (bytes, digest) = Algorithm::Sha256.hash_file_read(&path).await.unwrap();
        assert_eq!(bytes, data);
        assert_eq!(digest, Algorithm::Sha256.hash(&data));
    }

    #[tokio::test]
    async fn hash_file_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        tokio::fs::write(&path, b"").await.unwrap();

        let digest = Algorithm::Sha256.hash_file(&path).await.unwrap();
        assert_eq!(digest, Algorithm::Sha256.hash(&[] as &[u8]));
    }

    #[tokio::test]
    async fn hash_file_small() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small");
        let data = b"hello world".to_vec();
        tokio::fs::write(&path, &data).await.unwrap();

        let digest = Algorithm::Sha256.hash_file(&path).await.unwrap();
        assert_eq!(digest, Algorithm::Sha256.hash(&data));
    }

    #[tokio::test]
    async fn hash_file_larger_than_chunk() {
        // Exceeds the 64 KiB internal read buffer so the loop runs multiple iterations.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big");
        let data: Vec<u8> = (0..(64 * 1024 * 3 + 7)).map(|i| (i % 251) as u8).collect();
        tokio::fs::write(&path, &data).await.unwrap();

        let digest = Algorithm::Sha256.hash_file(&path).await.unwrap();
        assert_eq!(digest, Algorithm::Sha256.hash(&data));
    }

    #[tokio::test]
    async fn hash_file_sha384_matches_direct_sha2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        let data = b"hello world".to_vec();
        tokio::fs::write(&path, &data).await.unwrap();

        let digest = Algorithm::Sha384.hash_file(&path).await.unwrap();
        let expected = Digest::Sha384(hex::encode(<sha2::Sha384 as sha2::Digest>::digest(&data)));
        assert_eq!(digest, expected);
    }

    #[tokio::test]
    async fn hash_file_sha512_matches_direct_sha2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        let data: Vec<u8> = (0..(64 * 1024 * 2 + 13)).map(|i| (i % 251) as u8).collect();
        tokio::fs::write(&path, &data).await.unwrap();

        let digest = Algorithm::Sha512.hash_file(&path).await.unwrap();
        let expected = Digest::Sha512(hex::encode(<sha2::Sha512 as sha2::Digest>::digest(&data)));
        assert_eq!(digest, expected);
    }

    #[tokio::test]
    async fn algorithm_dispatch_selects_correct_hasher() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        let data = b"algorithm dispatch".to_vec();
        tokio::fs::write(&path, &data).await.unwrap();

        // Hash the same file under each algorithm via the Digest's algorithm()
        // accessor — exercises the round-trip that verify paths rely on.
        let sha256_seed = Digest::Sha256("0".repeat(64));
        let sha384_seed = Digest::Sha384("0".repeat(96));
        let sha512_seed = Digest::Sha512("0".repeat(128));

        let sha256_actual = sha256_seed.algorithm().hash_file(&path).await.unwrap();
        let sha384_actual = sha384_seed.algorithm().hash_file(&path).await.unwrap();
        let sha512_actual = sha512_seed.algorithm().hash_file(&path).await.unwrap();

        assert!(matches!(sha256_actual, Digest::Sha256(_)));
        assert!(matches!(sha384_actual, Digest::Sha384(_)));
        assert!(matches!(sha512_actual, Digest::Sha512(_)));
        assert_eq!(sha256_actual, Algorithm::Sha256.hash_file(&path).await.unwrap());
        assert_eq!(sha384_actual, Algorithm::Sha384.hash_file(&path).await.unwrap());
        assert_eq!(sha512_actual, Algorithm::Sha512.hash_file(&path).await.unwrap());
    }
}
