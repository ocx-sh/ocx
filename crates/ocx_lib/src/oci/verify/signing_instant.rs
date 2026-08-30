// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The instant certificate validity is judged against, and where it came from.
//!
//! # Carried constraint from G0 — read before touching verification time
//!
//! Verification anchors certificate validity to the **signing-time proof**,
//! never to a wall-clock "is this certificate valid right now". A Fulcio
//! certificate is short-lived *by design*: G0's keyless golden fixture
//! (`test/tests/fixtures/golden/keyless_bundle.json`) carries one whose window
//! is `02:07:54Z .. 02:17:54Z` — ten minutes, long elapsed by the time anyone
//! verifies. A clock-reading check refuses that legitimately signed artifact,
//! and every keyless signature older than an hour with it. The transparency-log
//! timestamp is the only evidence that the signature happened while the
//! certificate was live, which is why it, and not the clock, is the anchor.
//!
//! [`SigningInstant`] exists so a later edit cannot lose that rule by passing
//! the wrong number into an `i64` parameter: the argument names its own
//! provenance, and the type offers **no constructor that reads a clock** — no
//! `Default`, no `From<SystemTime>`, nothing spelling "the present".
//!
//! # One variant, and that is the contract
//!
//! [`SigningInstant::TransparencyLog`] is a log entry's `integratedTime`,
//! SET-checked before it reaches the window check — on the bundle path by
//! [`super::pipeline`]'s `verify_rekor_set` (SET **and** inclusion proof), on
//! the cosign sidecar path by [`super::simplesigning_read`] over the
//! `dev.sigstore.cosign/bundle` annotation (SET only, which is all cosign's
//! offline bundle carries).
//!
//! It used to have a sibling, `CallerSupplied`, and the sibling is why this
//! paragraph exists. G1 froze it as the *legal* no-transparency-log shape:
//! [`super::simplesigning_read`] passed the leaf certificate's own `notBefore`,
//! reasoning that a sidecar carrying no log entry has no signing instant to
//! discriminate against. That is circular — it asks the certificate when it was
//! valid, then checks the certificate against its own answer, so the window
//! check can never fail — and it reached further than it looked, because the
//! synthesised entry handed to `sigstore` carried the same value and anchored
//! that library's chain build *and* its expiry check on it too. Net effect: a
//! Fulcio leaf valid for ten minutes a year ago verified for ever, and a
//! later-revoked identity was undetectable. The contract is reversed: a keyless
//! simplesigning sidecar verifies **only** with transparency-log evidence, and
//! the variant is deleted rather than left reachable-in-name-only.
//!
//! The one place a certificate's `notBefore` still reaches `sigstore` is under
//! the explicit `--allow-unlogged-signature` opt-out, where the library demands
//! *some* entry to hold a bundle together; it is a bare `i64` there, is never
//! spelled as a signing instant, and the window check this module guards is
//! **skipped** rather than fed a value nothing proved.
//!
//! The guard that consumes this lives in [`super::tlog`]; the invariant that
//! nothing on the verify path reads a clock for validity is pinned by
//! `tests::the_certificate_validity_path_reads_no_clock`.

/// The instant a certificate's validity window is judged against, tagged with
/// the evidence it came from.
///
/// Deliberately **not** constructible from the present. See the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SigningInstant {
    /// A transparency-log entry's `integratedTime`, in seconds since the Unix
    /// epoch. The SET over it is what makes it a *proof* of signing time.
    TransparencyLog(i64),
}

impl SigningInstant {
    /// Seconds since the Unix epoch, whichever evidence supplied them.
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

        let verify_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/oci/verify");

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
