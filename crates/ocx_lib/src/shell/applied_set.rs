// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fingerprint helpers for the `_OCX_APPLIED` env var written by `ocx hook-env`.
//!
//! The hook tracks the set of currently-applied tools as a stable fingerprint
//! so that successive prompt invocations can short-circuit when nothing
//! changed. Wire format: `v1:<sha256-hex>` where the hex is the SHA-256 of the
//! sorted, line-joined `(group, name, manifest_digest)` triples.
//!
//! # v1 trade-off (Phase 7)
//!
//! The env var only carries a fingerprint, not the previously-applied key
//! set. When the fingerprint changes, the diff path approximates "previously
//! applied keys" with "keys we are about to export" — `unset KEY` is emitted
//! for every key in the fresh export set before each new `export KEY=...`.
//! This guarantees the shell sees a clean re-set when a tool is *replaced*
//! but cannot unset variables for tools that have been *removed* outright
//! (old set had X, new set does not). v2 of the wire format may carry the
//! full key list to close that gap; v1 documents the limitation here.

use sha2::{Digest, Sha256};

/// Single entry for the fingerprint set used by `ocx hook-env`.
///
/// One element per exported tool — `(name, manifest_digest, group)`.
/// SHA-256 of the sorted, line-joined triples = the fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEntry {
    /// Binding name of the tool as seen by the user (e.g. `cmake`, `node`).
    pub name: String,
    /// Manifest digest pinned by `ocx.lock` for this tool.
    pub manifest_digest: String,
    /// Group the tool was selected from (`default` or a named group).
    pub group: String,
}

/// Compute the v1 `_OCX_APPLIED` fingerprint string (`v1:<sha256-hex>`)
/// over a slice of [`AppliedEntry`] values.
///
/// Stable across runs because inputs are sorted lexicographically by
/// `(group, name, manifest_digest)` and the joined wire form is byte-fixed.
/// Returns `"v1:"` followed by the lowercase hex SHA-256 digest.
///
/// Wire form per entry: `<group>\t<name>\t<manifest_digest>\n`. Tab-separated
/// to keep the three string fields distinguishable when any one of them
/// happens to contain (e.g.) a colon — concatenating without a separator
/// collapses `("alpha", "sha256:beta")` and `("alphasha256:beta", "")` to
/// the same bytes.
///
/// # Stability of `manifest_digest`
///
/// `AppliedEntry::manifest_digest` MUST be sourced from the lock-side
/// digest (`LockedTool::pinned.digest()`) — it is what the lock file
/// commits to and is stable across machines and platforms (image-index
/// digest for multi-platform packages). The post-resolution
/// platform-selected manifest digest diverges per host architecture and
/// would silently break `hook-env`'s unchanged-prompt fast path on
/// multi-platform installs.
///
/// Post-Codex P2 fix: there is exactly one production call site —
/// [`crate::project::hook::collect_applied`] — and the fast path in
/// `hook-env` no longer derives a parallel fingerprint from
/// `lock.tools` directly. The fingerprint always reflects the
/// *actually-installed* set (post-`find_plain`), so `ocx uninstall` /
/// `ocx clean` cause the next prompt to detect the change.
pub fn compute_fingerprint(entries: &[AppliedEntry]) -> String {
    let mut sorted: Vec<&AppliedEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| {
        (a.group.as_str(), a.name.as_str(), a.manifest_digest.as_str()).cmp(&(
            b.group.as_str(),
            b.name.as_str(),
            b.manifest_digest.as_str(),
        ))
    });

    let mut hasher = Sha256::new();
    for entry in sorted {
        hasher.update(entry.group.as_bytes());
        hasher.update(b"\t");
        hasher.update(entry.name.as_bytes());
        hasher.update(b"\t");
        hasher.update(entry.manifest_digest.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    format!("v1:{}", hex::encode(digest))
}

/// Parse a previously-emitted `_OCX_APPLIED` env-var value (e.g. set by
/// the previous `ocx hook-env` invocation) into the fingerprint hex string.
///
/// Strict parser: returns `Some(hex)` only when `value` starts with the
/// literal `"v1:"` prefix AND the suffix is exactly 64 lowercase hex chars
/// (`[0-9a-f]`). Anything else — `v2:…`, missing prefix, wrong length,
/// non-hex characters, uppercase hex — returns `None`.
///
/// **v2 payloads return `None` by design.** Callers treat the absence of a
/// match as "fingerprint mismatch ⇒ re-emit"; a future v2 wire format
/// therefore triggers a full re-export rather than being silently accepted
/// as v1. See module doc-comment for the wire-version compatibility rule.
pub fn parse_applied(value: &str) -> Option<&str> {
    let hex = value.strip_prefix("v1:")?;
    if hex.len() != 64 {
        return None;
    }
    if !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    Some(hex)
}

/// Render the env-var value to set after a fresh `ocx hook-env` run.
///
/// Inverse of [`parse_applied`] for the v1 wire format. The input is the hex
/// fingerprint (without `v1:` prefix); the output is the full `v1:<hex>`
/// string suitable for `export _OCX_APPLIED=...`.
///
/// The caller is expected to pass a 64-hex string already (typically from
/// a fresh [`compute_fingerprint`] result with the `v1:` prefix stripped);
/// the shape is not re-validated here — round-trip with [`parse_applied`]
/// is what enforces the contract.
pub fn render_applied(fingerprint: &str) -> String {
    format!("v1:{fingerprint}")
}

#[cfg(test)]
mod tests {
    //! Tests for the `_OCX_APPLIED` v1 wire format.
    //!
    //! Each test asserts the contract documented in plan §7 line 776 and
    //! ADR §5B (Decision 5B): deterministic fingerprint over sorted
    //! `(group, name, manifest_digest)` triples, strict v1 parser, and
    //! lossless `render → parse` round-trip.
    use super::*;

    /// Build a hex string of a specific length for the v1 wire format.
    fn hex(byte: u8, len: usize) -> String {
        let ch = format!("{:02x}", byte);
        ch.chars().cycle().take(len).collect()
    }

    fn entry(name: &str, manifest_digest: &str, group: &str) -> AppliedEntry {
        AppliedEntry {
            name: name.into(),
            manifest_digest: manifest_digest.into(),
            group: group.into(),
        }
    }

    fn sample_entries() -> Vec<AppliedEntry> {
        vec![
            entry("cmake", "sha256:aaaaaaaa", "default"),
            entry("node", "sha256:bbbbbbbb", "ci"),
            entry("ruff", "sha256:cccccccc", "lint"),
        ]
    }

    // ── compute_fingerprint ──────────────────────────────────────────

    #[test]
    fn compute_fingerprint_is_deterministic() {
        let entries = sample_entries();
        let first = compute_fingerprint(&entries);
        let second = compute_fingerprint(&entries);
        assert_eq!(first, second, "fingerprint must be deterministic");
    }

    #[test]
    fn compute_fingerprint_sorts_inputs() {
        // Three permutations of the same set — sorted by `(group, name,
        // manifest_digest)` they should all collapse to the same fingerprint.
        let a = sample_entries();
        let b = vec![a[2].clone(), a[0].clone(), a[1].clone()];
        let c = vec![a[1].clone(), a[2].clone(), a[0].clone()];
        let fa = compute_fingerprint(&a);
        let fb = compute_fingerprint(&b);
        let fc = compute_fingerprint(&c);
        assert_eq!(fa, fb, "fingerprint must be order-independent");
        assert_eq!(fa, fc, "fingerprint must be order-independent");
    }

    #[test]
    fn compute_fingerprint_starts_with_v1_prefix() {
        let fp = compute_fingerprint(&sample_entries());
        assert!(fp.starts_with("v1:"), "expected 'v1:' prefix, got {:?}", fp);
    }

    #[test]
    fn compute_fingerprint_is_64_hex_after_prefix() {
        let fp = compute_fingerprint(&sample_entries());
        let rest = fp.strip_prefix("v1:").expect("expected 'v1:' prefix");
        assert_eq!(rest.len(), 64, "expected 64 hex chars, got {}: {:?}", rest.len(), rest);
        assert!(
            rest.chars()
                .all(|c| c.is_ascii_hexdigit() && (c.is_ascii_digit() || c.is_ascii_lowercase())),
            "expected lowercase hex, got {:?}",
            rest
        );
    }

    #[test]
    fn compute_fingerprint_distinguishes_name_from_digest() {
        // Swapping name and manifest_digest values between two entries must
        // change the fingerprint — the wire format must not collapse the two
        // through naive concatenation.
        let a = vec![
            entry("alpha", "sha256:beta", "default"),
            entry("beta", "sha256:alpha", "default"),
        ];
        let b = vec![
            entry("alpha", "sha256:alpha", "default"),
            entry("beta", "sha256:beta", "default"),
        ];
        let fa = compute_fingerprint(&a);
        let fb = compute_fingerprint(&b);
        assert_ne!(fa, fb, "name and manifest_digest must not be conflatable");
    }

    #[test]
    fn compute_fingerprint_distinguishes_group() {
        let a = vec![
            entry("alpha", "sha256:dead", "default"),
            entry("beta", "sha256:beef", "ci"),
        ];
        let b = vec![
            entry("alpha", "sha256:dead", "ci"),
            entry("beta", "sha256:beef", "default"),
        ];
        let fa = compute_fingerprint(&a);
        let fb = compute_fingerprint(&b);
        assert_ne!(fa, fb, "swapping groups must change the fingerprint");
    }

    #[test]
    fn compute_fingerprint_empty_set() {
        let fp = compute_fingerprint(&[]);
        assert!(
            fp.starts_with("v1:"),
            "empty set must still produce v1: shape, got {:?}",
            fp
        );
        let rest = fp.strip_prefix("v1:").expect("expected 'v1:' prefix");
        assert_eq!(
            rest.len(),
            64,
            "empty-set fingerprint must be 64 hex chars, got {:?}",
            rest
        );
    }

    // ── parse_applied ────────────────────────────────────────────────

    #[test]
    fn parse_applied_v1_valid() {
        let body = hex(0xab, 64);
        let value = format!("v1:{}", body);
        let parsed = parse_applied(&value);
        assert_eq!(
            parsed,
            Some(body.as_str()),
            "valid v1:<64-hex> must round-trip to its hex body"
        );
    }

    #[test]
    fn parse_applied_rejects_non_v1_prefix() {
        let body = hex(0x12, 64);
        let value = format!("v2:{}", body);
        assert_eq!(
            parse_applied(&value),
            None,
            "v2 must NOT silently parse as v1; future versions must be a deliberate change"
        );
    }

    #[test]
    fn parse_applied_rejects_missing_prefix() {
        let body = hex(0x34, 64);
        assert_eq!(
            parse_applied(&body),
            None,
            "bare 64-hex without 'v1:' prefix must not parse"
        );
    }

    #[test]
    fn parse_applied_rejects_malformed_hex() {
        let value = "v1:not-hex-not-hex-not-hex-not-hex-not-hex-not-hex-not-hex-not-hex";
        assert_eq!(parse_applied(value), None, "non-hex characters after v1: must reject");
    }

    #[test]
    fn parse_applied_rejects_uppercase_hex() {
        // The wire format is locked to lowercase hex (`[0-9a-f]`). Accepting
        // uppercase would produce two distinct env-var values for the same
        // logical fingerprint, defeating the unchanged-prompt fast path.
        let value = format!("v1:{}", "AB".repeat(32));
        assert_eq!(
            parse_applied(&value),
            None,
            "uppercase hex must reject; v1 wire format is lowercase only"
        );
    }

    #[test]
    fn parse_applied_rejects_wrong_hex_length() {
        // Too short.
        let short = "v1:abc";
        assert_eq!(parse_applied(short), None, "short hex must reject");

        // 65-char hex (one over) must also reject.
        let long_body = hex(0xcd, 65);
        let long = format!("v1:{}", long_body);
        assert_eq!(parse_applied(&long), None, "65-char hex must reject");
    }

    #[test]
    fn parse_applied_empty() {
        assert_eq!(parse_applied(""), None, "empty string must reject");
    }

    // ── render_applied ───────────────────────────────────────────────

    #[test]
    fn render_applied_round_trips() {
        let body = hex(0xef, 64);
        let rendered = render_applied(&body);
        let parsed = parse_applied(&rendered);
        assert_eq!(
            parsed,
            Some(body.as_str()),
            "render → parse must round-trip the hex body"
        );
        assert!(rendered.starts_with("v1:"), "render output must carry 'v1:' prefix");
    }
}
