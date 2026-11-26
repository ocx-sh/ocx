// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The launcher generation key `G` and the toolchain render fingerprint, as exact bytes.
//! See `adr_unified_launcher.md` § 4.1.

use std::collections::BTreeMap;

/// Bumped when a writer change must reach launcher directories already on disk.
pub const HOME_LAUNCHER_REVISION: u32 = 1;

const GENERATION_HEX_LEN: usize = 16;

/// 16 lowercase hex characters naming one launcher blob, record schema and writer revision.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LauncherGeneration(String);

impl LauncherGeneration {
    /// The first 16 hex characters of SHA-256 over the `ocx.launcher-generation.v1` lines.
    pub fn of(_blob: &[u8], _schema_version: u32, _revision: u32) -> Self {
        unimplemented!("LauncherGeneration::of")
    }

    /// `of(shim::host_blob()?, ocx_env_apply::launch::SCHEMA_VERSION, HOME_LAUNCHER_REVISION)`; `None` on a host with no blob.
    pub fn host() -> Option<Self> {
        unimplemented!("LauncherGeneration::host")
    }

    /// Reads a generation back from a directory name; `None` unless it is exactly 16 lowercase hex characters.
    pub fn from_hex(hex: &str) -> Option<Self> {
        let valid = hex.len() == GENERATION_HEX_LEN && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        valid.then(|| Self(hex.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// SHA-256 over the `ocx.toolchain-render.v1` input; 64 lowercase hex, stored in the render stamp.
pub fn render_fingerprint(_generation: &LauncherGeneration, _records: &BTreeMap<String, Vec<u8>>) -> String {
    unimplemented!("render_fingerprint")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_starts_at_one() {
        assert_eq!(HOME_LAUNCHER_REVISION, 1);
    }

    /// C-124, worked example 1 of the decision record.
    #[test]
    fn contract_generation_of_the_empty_blob_matches_the_golden() {
        assert_eq!(LauncherGeneration::of(b"", 1, 1).as_str(), "d7c9115eb03568da");
    }

    /// C-124, worked example 2 of the decision record.
    #[test]
    fn contract_render_fingerprint_matches_the_golden() {
        let generation = LauncherGeneration::from_hex("0123456789abcdef").unwrap();
        let record = b"{\"kind\":\"toolchain\",\"schema_version\":1,\"tier\":\"global\"}\n".to_vec();
        assert_eq!(record.len(), 56);
        let records = BTreeMap::from([("cmake".to_string(), record)]);

        assert_eq!(
            render_fingerprint(&generation, &records),
            "202755de66972decba3905023b7f5f3303c0472810eec3a7f0ffe02691de8f68"
        );
    }

    /// C-124: `host()` is the key of the host blob, and `None` exactly when the host has none.
    #[test]
    fn contract_host_follows_the_host_blob() {
        let expected = crate::shim::host_blob()
            .map(|blob| LauncherGeneration::of(blob, ocx_env_apply::launch::SCHEMA_VERSION, HOME_LAUNCHER_REVISION));
        assert_eq!(LauncherGeneration::host(), expected);
    }

    #[test]
    fn from_hex_accepts_only_sixteen_lowercase_hex() {
        assert_eq!(
            LauncherGeneration::from_hex("d7c9115eb03568da").unwrap().as_str(),
            "d7c9115eb03568da"
        );
        for bad in [
            "",
            "d7c9115eb03568d",
            "d7c9115eb03568daa",
            "D7C9115EB03568DA",
            "d7c9115eb03568dg",
        ] {
            assert_eq!(LauncherGeneration::from_hex(bad), None, "{bad:?}");
        }
    }
}
