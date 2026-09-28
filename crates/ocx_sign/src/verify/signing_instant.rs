// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The instant certificate validity is judged against, tagged with the evidence it came from.
//!
//! Add no constructor that reads a clock (`Default`, `From<SystemTime>`): a Fulcio certificate lives ten minutes,
//! so a wall-clock check refuses every keyless signature older than that
//! (`tests::the_certificate_validity_path_reads_no_clock`).
//! Add no `notBefore`-derived variant: judging the leaf against its own `notBefore` never fails, so an expired leaf
//! verifies forever.

/// The instant a certificate's validity window is judged against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SigningInstant {
    /// A SET-checked transparency-log `integratedTime`, in Unix seconds.
    TransparencyLog(i64),
}

impl SigningInstant {
    /// Seconds since the Unix epoch.
    pub(super) const fn epoch_seconds(self) -> i64 {
        match self {
            Self::TransparencyLog(seconds) => seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_logged_instant_is_the_seconds_it_was_built_from() {
        assert_eq!(
            SigningInstant::TransparencyLog(1_787_969_275).epoch_seconds(),
            1_787_969_275
        );
        assert_eq!(SigningInstant::TransparencyLog(-1).epoch_seconds(), -1);
    }

    /// Nothing under `oci/verify/` may read a clock to decide certificate
    /// validity — the G0 constraint in the module doc, pinned as a source scan
    /// rather than as a convention, because a convention is what the next edit
    /// does not know about. Same shape as the source-scanning allow-list test
    /// in `oci/client.rs` (named there; not spelled here, because that test
    /// scans for its own subject and a mention would trip it).
    ///
    /// The needles are assembled with `concat!` so this file does not contain
    /// the strings it looks for: a scanner whose needle is a literal in the set
    /// it scans matches itself in every state and therefore measures nothing.
    #[test]
    fn the_certificate_validity_path_reads_no_clock() {
        use std::fs;
        use std::path::{Path, PathBuf};

        const NEEDLES: &[&str] = &[
            concat!("SystemTime", "::now"),
            concat!("Utc", "::now"),
            concat!("Instant", "::now"),
        ];

        // Allow-list: files whose clock reads decide trust-material *cache
        // freshness* (a 24h TTL), never a certificate validity window. File
        // names, relative to this directory.
        const ALLOWED: &[&str] = &["trust_cache.rs", "trust_resolve.rs"];

        // `src/verify`, not `src/oci/verify`: WP-31 took this subtree out of
        // `ocx_lib`, and `CARGO_MANIFEST_DIR` followed it. The corpus floor
        // below is what said so — it read 0 files at the old join and refused
        // to report a verdict, which is the whole reason it is there.
        let verify_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/verify");

        fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_rs_files(&path, out);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    out.push(path);
                }
            }
        }

        let mut sources = Vec::new();
        collect_rs_files(&verify_dir, &mut sources);
        // A scan that found nothing is indistinguishable from a scan that
        // passed, so assert the corpus before asserting anything about it.
        assert!(
            sources.len() >= 5,
            "source scanner found only {} .rs files under {}",
            sources.len(),
            verify_dir.display()
        );
        assert!(
            sources
                .iter()
                .any(|p| p.file_name().and_then(|n| n.to_str()) == Some("tlog.rs")),
            "source scanner did not reach tlog.rs under {}",
            verify_dir.display()
        );

        let mut offenders: Vec<String> = Vec::new();
        for path in &sources {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if ALLOWED.contains(&name) {
                continue;
            }
            let content = fs::read_to_string(path).unwrap_or_default();
            for needle in NEEDLES {
                if content.contains(needle) {
                    offenders.push(format!("{name} contains {needle}"));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "certificate validity anchors to the signing-time proof, never to a clock: {offenders:?}"
        );
    }
}
