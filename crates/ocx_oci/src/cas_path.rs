// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Digest sharding shared by the on-disk store and any crate that must predict a blob's path.

use std::path::PathBuf;

use crate::Digest;

pub const CAS_SHARD_PREFIX_LEN: usize = 2;

/// 128 bits: birthday-safe for any realistic store; the full digest lives in the `digest` file.
pub const CAS_SHARD_TOTAL_LEN: usize = 32;

pub const CAS_SHARD_SUFFIX_LEN: usize = CAS_SHARD_TOTAL_LEN - CAS_SHARD_PREFIX_LEN;

/// Returns `{algorithm}/{hex[0..2]}/{hex[2..32]}`, truncated to stay under Windows `MAX_PATH`; the digest is not recoverable from it.
pub fn cas_shard_path(digest: &Digest) -> PathBuf {
    let (algorithm, hex) = digest.parts();
    let mut path = PathBuf::from(algorithm);
    path.push(&hex[..CAS_SHARD_PREFIX_LEN]);
    path.push(&hex[CAS_SHARD_PREFIX_LEN..CAS_SHARD_TOTAL_LEN]);
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
    const SHA384_HEX: &str =
        "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9a1b2c3d4e5f678901234567890123456";
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
}
