// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use crate::oci::Digest;

/// Number of hex characters used as the shard prefix directory name.
const CAS_SHARD_PREFIX_LEN: usize = 2;

/// Total hex characters encoded in the path (prefix + suffix).
/// The full digest is recovered from the `digest` file, not the path.
/// 32 hex chars = 128 bits — birthday-safe for any realistic store size.
const CAS_SHARD_TOTAL_LEN: usize = 32;

/// Number of hex characters in the suffix (remaining after prefix).
const CAS_SHARD_SUFFIX_LEN: usize = CAS_SHARD_TOTAL_LEN - CAS_SHARD_PREFIX_LEN;

/// Number of path components produced by [`cas_shard_path`]: `algorithm/prefix/suffix`.
/// Store walkers add their own prefix levels (e.g., registry) to this.
pub const CAS_SHARD_DEPTH: usize = 3;

/// Filename for the digest marker file inside a CAS directory.
/// Used by all three CAS stores' `digest_file()` accessors.
pub const DIGEST_FILENAME: &str = "digest";

/// CAS tier classification for GC and reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CasTier {
    /// A fully resolved and installed package.
    Package,
    /// An extracted OCI layer (shared across packages).
    Layer,
    /// A raw content-addressed blob.
    Blob,
}

/// Returns a 2-level sharded path for the given digest, truncated to
/// [`CAS_SHARD_TOTAL_LEN`] hex characters.
///
/// Layout: `{algorithm}/{hex[0..2]}/{hex[2..32]}`
///
/// The full digest is NOT recoverable from the path alone — use the sibling
/// `digest` file for that. The truncation keeps paths bounded for Windows
/// MAX_PATH compliance.
pub fn cas_shard_path(digest: &Digest) -> PathBuf {
    let (algorithm, hex) = digest.parts();
    let mut path = PathBuf::from(algorithm);
    path.push(&hex[..CAS_SHARD_PREFIX_LEN]);
    path.push(&hex[CAS_SHARD_PREFIX_LEN..CAS_SHARD_TOTAL_LEN]);
    path
}

/// Writes the full digest string (e.g., `sha256:43567c07...`) to the given path.
///
/// The caller provides the full path (typically from a store's `digest_file()` accessor).
pub async fn write_digest_file(path: &Path, digest: &Digest) -> crate::Result<()> {
    tokio::fs::write(path, digest.to_string())
        .await
        .map_err(|e| crate::Error::InternalFile(path.to_path_buf(), e))
}

/// Reads and parses a digest file at the given path.
///
/// The caller provides the full path (typically from a store's `digest_file()` accessor).
#[allow(dead_code)]
pub async fn read_digest_file(path: &Path) -> crate::Result<Digest> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| crate::Error::InternalFile(path.to_path_buf(), e))?;
    let digest = Digest::try_from(content.trim())?;
    Ok(digest)
}

/// Validates that a directory path ends with `{algorithm}/{2hex}/{remaining_hex}`.
///
/// Checks that the algorithm is known (`sha256`, `sha384`, `sha512`), the prefix
/// is exactly 2 hex characters, and the remaining segment is valid hex.
/// Derives a filesystem-safe reference name from a content digest.
///
/// Format: `{algorithm}_{32_hex}` — e.g. `sha256_4a3f...1b2c`. The
/// algorithm prefix makes the filename self-describing when browsed
/// in an IDE, and the 32-hex suffix mirrors [`CAS_SHARD_TOTAL_LEN`]
/// so a reader can recognize the identity at a glance.
///
/// Used for `refs/layers/`, `refs/blobs/`, and `refs/deps/`, where the
/// ref's identity is the content digest.
pub fn cas_ref_name(digest: &Digest) -> String {
    let hex = digest.hex();
    let total = CAS_SHARD_TOTAL_LEN.min(hex.len());
    format!("{}_{}", digest.algorithm(), &hex[..total])
}

pub fn is_valid_cas_path(dir: &Path) -> bool {
    // Collect last 3 components in reverse: remaining, prefix, algorithm
    let tail: Vec<&str> = dir
        .components()
        .rev()
        .take(3)
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    if tail.len() != 3 {
        return false;
    }
    // tail is reversed: [remaining, prefix, algorithm]
    let algorithm = tail[2];
    let prefix = tail[1];
    let remaining = tail[0];

    Digest::ALGORITHMS.contains(&algorithm)
        && prefix.len() == CAS_SHARD_PREFIX_LEN
        && prefix.bytes().all(|b| b.is_ascii_hexdigit())
        && remaining.len() == CAS_SHARD_SUFFIX_LEN
        && remaining.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::oci::Digest;

    // Hex strings for each algorithm.
    // SHA-256: 64 hex chars
    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
    // SHA-384: 96 hex chars
    const SHA384_HEX: &str =
        "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9a1b2c3d4e5f678901234567890123456";
    // SHA-512: 128 hex chars
    const SHA512_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d943567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn sha256() -> Digest {
        Digest::Sha256(SHA256_HEX.to_string())
    }

    fn sha384() -> Digest {
        Digest::Sha384(SHA384_HEX.to_string())
    }

    fn sha512() -> Digest {
        Digest::Sha512(SHA512_HEX.to_string())
    }

    // ── cas_shard_path ────────────────────────────────────────────────────────

    #[test]
    fn cas_shard_path_sha256_layout() {
        let p = cas_shard_path(&sha256());
        let components: Vec<_> = p.components().collect();
        // Layout: sha256 / {2hex} / {30hex} (truncated to 32 total)
        assert_eq!(components.len(), 3, "expected exactly 3 path components");
        assert_eq!(components[0].as_os_str(), "sha256");
        assert_eq!(components[1].as_os_str(), &SHA256_HEX[..CAS_SHARD_PREFIX_LEN]);
        assert_eq!(
            components[2].as_os_str(),
            &SHA256_HEX[CAS_SHARD_PREFIX_LEN..CAS_SHARD_TOTAL_LEN]
        );
    }

    #[test]
    fn cas_shard_path_truncates_to_32_hex_chars_total() {
        let p = cas_shard_path(&sha256());
        let mut comps = p.components().skip(1); // skip algorithm
        let prefix = comps.next().unwrap().as_os_str().to_string_lossy().into_owned();
        let suffix = comps.next().unwrap().as_os_str().to_string_lossy().into_owned();
        assert_eq!(prefix.len() + suffix.len(), CAS_SHARD_TOTAL_LEN);
        assert_eq!(prefix + &suffix, &SHA256_HEX[..CAS_SHARD_TOTAL_LEN]);
    }

    #[test]
    fn cas_shard_path_sha384_truncated_same_length() {
        let p = cas_shard_path(&sha384());
        let components: Vec<_> = p.components().collect();
        assert_eq!(components.len(), 3);
        assert_eq!(components[0].as_os_str(), "sha384");
        // SHA-384 has 96 hex chars, but path only uses first 32
        assert_eq!(components[1].as_os_str(), &SHA384_HEX[..CAS_SHARD_PREFIX_LEN]);
        assert_eq!(
            components[2].as_os_str(),
            &SHA384_HEX[CAS_SHARD_PREFIX_LEN..CAS_SHARD_TOTAL_LEN]
        );
    }

    #[test]
    fn cas_shard_path_sha512_truncated_same_length() {
        let p = cas_shard_path(&sha512());
        let components: Vec<_> = p.components().collect();
        assert_eq!(components.len(), 3);
        assert_eq!(components[0].as_os_str(), "sha512");
        // SHA-512 has 128 hex chars, but path only uses first 32
        assert_eq!(components[1].as_os_str(), &SHA512_HEX[..CAS_SHARD_PREFIX_LEN]);
        assert_eq!(
            components[2].as_os_str(),
            &SHA512_HEX[CAS_SHARD_PREFIX_LEN..CAS_SHARD_TOTAL_LEN]
        );
    }

    #[test]
    fn cas_shard_path_suffix_length_is_consistent_across_algorithms() {
        for digest in [sha256(), sha384(), sha512()] {
            let p = cas_shard_path(&digest);
            let suffix = p.components().nth(2).unwrap().as_os_str().to_string_lossy();
            assert_eq!(
                suffix.len(),
                CAS_SHARD_SUFFIX_LEN,
                "suffix must be {} hex chars for {}",
                CAS_SHARD_SUFFIX_LEN,
                digest.algorithm()
            );
        }
    }

    // ── write_digest_file / read_digest_file ──────────────────────────────────

    #[tokio::test]
    async fn round_trip_write_read_returns_same_digest() {
        let dir = tempfile::tempdir().unwrap();
        let digest = sha256();
        let path = dir.path().join(DIGEST_FILENAME);
        write_digest_file(&path, &digest).await.unwrap();
        let result = read_digest_file(&path).await.unwrap();
        assert_eq!(result, digest);
    }

    #[tokio::test]
    async fn round_trip_preserves_sha384_digest() {
        let dir = tempfile::tempdir().unwrap();
        let digest = sha384();
        let path = dir.path().join(DIGEST_FILENAME);
        write_digest_file(&path, &digest).await.unwrap();
        let result = read_digest_file(&path).await.unwrap();
        assert_eq!(result, digest);
    }

    #[tokio::test]
    async fn round_trip_preserves_sha512_digest() {
        let dir = tempfile::tempdir().unwrap();
        let digest = sha512();
        let path = dir.path().join(DIGEST_FILENAME);
        write_digest_file(&path, &digest).await.unwrap();
        let result = read_digest_file(&path).await.unwrap();
        assert_eq!(result, digest);
    }

    #[tokio::test]
    async fn written_file_content_matches_display_format() {
        let dir = tempfile::tempdir().unwrap();
        let digest = sha256();
        let path = dir.path().join(DIGEST_FILENAME);
        write_digest_file(&path, &digest).await.unwrap();
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content.trim(), digest.to_string());
    }

    #[tokio::test]
    async fn read_digest_file_from_nonexistent_path_returns_error() {
        let result = read_digest_file(Path::new("/nonexistent/path/digest")).await;
        assert!(result.is_err(), "expected error reading from nonexistent path");
    }

    #[tokio::test]
    async fn read_digest_file_absent_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DIGEST_FILENAME);
        let result = read_digest_file(&path).await;
        assert!(result.is_err(), "expected error when digest file is absent");
    }

    #[tokio::test]
    async fn read_digest_file_with_invalid_content_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DIGEST_FILENAME);
        tokio::fs::write(&path, b"not-a-valid-digest\n").await.unwrap();
        let result = read_digest_file(&path).await;
        assert!(result.is_err(), "expected error when digest file content is invalid");
    }

    // ── is_valid_cas_path ─────────────────────────────────────────────────────

    #[test]
    fn valid_sha256_path_returns_true() {
        let prefix = &SHA256_HEX[..CAS_SHARD_PREFIX_LEN];
        let suffix = &SHA256_HEX[CAS_SHARD_PREFIX_LEN..CAS_SHARD_TOTAL_LEN];
        let p = Path::new("/some/root/sha256").join(prefix).join(suffix);
        assert!(is_valid_cas_path(&p));
    }

    #[test]
    fn valid_sha384_path_returns_true() {
        let prefix = &SHA384_HEX[..CAS_SHARD_PREFIX_LEN];
        let suffix = &SHA384_HEX[CAS_SHARD_PREFIX_LEN..CAS_SHARD_TOTAL_LEN];
        let p = Path::new("/root/sha384").join(prefix).join(suffix);
        assert!(is_valid_cas_path(&p));
    }

    #[test]
    fn valid_sha512_path_returns_true() {
        let prefix = &SHA512_HEX[..CAS_SHARD_PREFIX_LEN];
        let suffix = &SHA512_HEX[CAS_SHARD_PREFIX_LEN..CAS_SHARD_TOTAL_LEN];
        let p = Path::new("/root/sha512").join(prefix).join(suffix);
        assert!(is_valid_cas_path(&p));
    }

    #[test]
    fn invalid_algorithm_returns_false() {
        let rest = &SHA256_HEX[2..];
        let p = Path::new("/root/md5/ab").join(rest);
        assert!(!is_valid_cas_path(&p));
    }

    #[test]
    fn non_hex_prefix_returns_false() {
        let rest = &SHA256_HEX[2..];
        let p = Path::new("/root/sha256/zz").join(rest);
        assert!(!is_valid_cas_path(&p));
    }

    #[test]
    fn non_hex_remaining_segment_returns_false() {
        let p = Path::new("/root/sha256/ab/not-valid-hex-string");
        assert!(!is_valid_cas_path(p));
    }

    #[test]
    fn too_few_components_returns_false() {
        // Only 2 components after root — needs 3 (algorithm + prefix + remaining)
        let p = Path::new("/sha256/ab");
        assert!(!is_valid_cas_path(p));
    }

    #[test]
    fn prefix_length_one_char_returns_false() {
        // 1-char prefix + 30-char suffix: prefix wrong length
        let suffix = &SHA256_HEX[1..CAS_SHARD_TOTAL_LEN];
        let p = Path::new("/root/sha256/a").join(suffix);
        assert!(!is_valid_cas_path(&p));
    }

    #[test]
    fn prefix_length_three_chars_returns_false() {
        // 3-char prefix + 30-char suffix: prefix wrong length
        let suffix = &SHA256_HEX[3..(3 + CAS_SHARD_SUFFIX_LEN)];
        let p = Path::new("/root/sha256/abc").join(suffix);
        assert!(!is_valid_cas_path(&p));
    }

    #[test]
    fn suffix_wrong_length_returns_false() {
        // Correct prefix but suffix too long (full hex, not truncated)
        let prefix = &SHA256_HEX[..CAS_SHARD_PREFIX_LEN];
        let full_rest = &SHA256_HEX[CAS_SHARD_PREFIX_LEN..]; // 62 chars, not 30
        let p = Path::new("/root/sha256").join(prefix).join(full_rest);
        assert!(!is_valid_cas_path(&p));
    }

    // ── cas_ref_name ─────────────────────────────────────────────────────────

    #[test]
    fn cas_ref_name_is_algorithm_prefixed_32_hex() {
        let digest = Digest::Sha256("4a3f1b2c5d6e7f8091a2b3c4d5e6f7081928374656473829101112131415161718".to_string());
        let name = cas_ref_name(&digest);
        assert_eq!(name, "sha256_4a3f1b2c5d6e7f8091a2b3c4d5e6f708");
        assert_eq!(name.len(), "sha256_".len() + 32);
    }

    #[test]
    fn cas_ref_name_differs_across_digests() {
        let a = Digest::Sha256("1111111111111111111111111111111122222222222222222222222222222222".to_string());
        let b = Digest::Sha256("3333333333333333333333333333333344444444444444444444444444444444".to_string());
        assert_ne!(cas_ref_name(&a), cas_ref_name(&b));
    }
}
