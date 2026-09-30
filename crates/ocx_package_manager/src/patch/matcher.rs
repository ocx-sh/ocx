// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Unified flat glob matcher for patch descriptor `match` patterns.
//!
//! Unlike [`glob::Pattern`], `*` spans **any** run of characters including `/`,
//! `:` and `@`, because the target is a canonical OCI identifier
//! (`ghcr.io/acme/cli:v1@sha256:…`) and `ghcr.io/*` must match a whole registry.
//! `?` matches one character, `[set]` / `[!set]` a bracket class, anything else
//! itself; matching is case-sensitive, `**` behaves as `*`, and the empty pattern
//! matches only the empty string. An untagged identifier matches `*:*` only via
//! a digest's `sha256:` colon or an explicit registry port (`localhost:5000/repo`).

/// Returns `true` if `text` matches the flat glob `pattern` (`O(n·m)` worst case).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: &[u8] = pattern.as_bytes();
    let text: &[u8] = text.as_bytes();

    // `star_pi`: pattern index after the last `*`; `star_ti`: text index where that star's match began.
    let mut pi = 0usize;
    let mut ti = 0usize;
    let mut star_pi: Option<usize> = None;
    let mut star_ti: usize = 0;

    while ti < text.len() {
        match pattern.get(pi) {
            Some(&b'*') => {
                star_pi = Some(pi + 1);
                star_ti = ti;
                pi += 1;
            }
            Some(&b'?') => {
                pi += 1;
                ti += 1;
            }
            Some(&b'[') => {
                let (matched, consumed) = match_bracket(&pattern[pi..], text[ti]);
                if matched {
                    pi += consumed;
                    ti += 1;
                } else if let Some(spi) = star_pi {
                    star_ti += 1;
                    ti = star_ti;
                    pi = spi;
                } else {
                    return false;
                }
            }
            Some(&pc) => {
                if pc == text[ti] {
                    pi += 1;
                    ti += 1;
                } else if let Some(spi) = star_pi {
                    star_ti += 1;
                    ti = star_ti;
                    pi = spi;
                } else {
                    return false;
                }
            }
            None => {
                if let Some(spi) = star_pi {
                    star_ti += 1;
                    ti = star_ti;
                    pi = spi;
                } else {
                    return false;
                }
            }
        }
    }

    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }

    pi == pattern.len()
}

/// Parses a `[set]` or `[!set]` bracket expression starting at `pattern[0]`
/// (which must be `b'['`) and tests whether `ch` is matched.
///
/// Returns `(matched, bytes_consumed)`, brackets included; a bracket with no
/// closing `]` matches a literal `[` and consumes 1 byte.
fn match_bracket(pattern: &[u8], ch: u8) -> (bool, usize) {
    debug_assert_eq!(pattern[0], b'[');
    let negate = pattern.get(1) == Some(&b'!');
    let start = if negate { 2 } else { 1 };
    let mut i = start;
    let mut inner_match = false;

    while i < pattern.len() && pattern[i] != b']' {
        if i + 2 < pattern.len() && pattern[i + 1] == b'-' && pattern[i + 2] != b']' {
            if ch >= pattern[i] && ch <= pattern[i + 2] {
                inner_match = true;
            }
            i += 3;
        } else {
            if pattern[i] == ch {
                inner_match = true;
            }
            i += 1;
        }
    }

    if i < pattern.len() && pattern[i] == b']' {
        let consumed = i + 1; // +1 for the closing `]`
        let matched = if negate { !inner_match } else { inner_match };
        (matched, consumed)
    } else {
        (pattern[0] == ch, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    // ── ADR table cases ───────────────────────────────────────────────────────

    #[test]
    fn star_matches_everything() {
        assert!(glob_match("*", "ghcr.io/acme/cli:v1"));
        assert!(glob_match("*", "ocx.sh/java:21"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn ghcr_io_star_matches_images_on_ghcr() {
        assert!(glob_match("ghcr.io/*", "ghcr.io/acme/cli:v1"));
        assert!(!glob_match("ghcr.io/*", "ocx.sh/java:21"));
    }

    #[test]
    fn star_slash_java_star_matches_any_java() {
        assert!(glob_match("*/java:*", "ocx.sh/java:21"));
        assert!(glob_match("*/java:*", "ghcr.io/java:latest"));
        assert!(!glob_match("*/java:*", "ghcr.io/cmake:3.28"));
    }

    #[test]
    fn star_colon_21_matches_tag_21() {
        assert!(glob_match("*:21", "ocx.sh/java:21"));
        assert!(glob_match("*:21", "ghcr.io/jdk:21"));
        assert!(!glob_match("*:21", "ocx.sh/java:3.28"));
    }

    #[test]
    fn zscaler_substring_match() {
        assert!(glob_match(
            "*zscaler*",
            "internal.company.com/certs/zscaler-root:latest"
        ));
        assert!(!glob_match("*zscaler*", "ocx.sh/java:21"));
    }

    // ── Edge cases ────────────────────────────────────────────────────────────

    #[test]
    fn registry_port_colon_is_literal() {
        // `:` in a port must not special-case; `localhost:5000/repo:tag` must match
        // `localhost:5000/*` (the second `:` is inside the wildcard span).
        assert!(glob_match("localhost:5000/*", "localhost:5000/x:1"));
        assert!(!glob_match("localhost:5000/*", "ghcr.io/x:1"));
    }

    #[test]
    fn digest_colon_at_sign_are_literals() {
        // `@sha256:` inside a digest reference should match literally.
        assert!(glob_match(
            "*@sha256:*",
            "ghcr.io/cmake:3.28@sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"
        ));
        // No digest → does NOT match `*@sha256:*`.
        assert!(!glob_match("*@sha256:*", "ghcr.io/cmake:3.28"));
    }

    #[test]
    fn untagged_does_not_match_star_colon_star() {
        // An untagged identifier has no `:` after the repo; `*:*` should NOT match it.
        assert!(!glob_match("*:*", "ghcr.io/cmake"));
    }

    #[test]
    fn question_mark_matches_one_char() {
        assert!(glob_match("ocx.sh/java:?1", "ocx.sh/java:21"));
        assert!(!glob_match("ocx.sh/java:?1", "ocx.sh/java:211"));
        assert!(!glob_match("ocx.sh/java:?1", "ocx.sh/java:1"));
    }

    #[test]
    fn bracket_class() {
        assert!(glob_match("[abc]*", "a-tool:1.0"));
        assert!(glob_match("[abc]*", "b-tool:1.0"));
        assert!(!glob_match("[abc]*", "d-tool:1.0"));
    }

    #[test]
    fn negated_bracket_class() {
        assert!(glob_match("[!abc]*", "d-tool:1.0"));
        assert!(!glob_match("[!abc]*", "a-tool:1.0"));
    }

    #[test]
    fn range_bracket() {
        assert!(glob_match("[a-z]*", "ghcr.io/tool:1"));
        assert!(!glob_match("[A-Z]*", "ghcr.io/tool:1"));
    }

    #[test]
    fn empty_pattern_matches_empty_text() {
        assert!(glob_match("", ""));
    }

    #[test]
    fn empty_pattern_does_not_match_nonempty_text() {
        assert!(!glob_match("", "ocx.sh/java:21"));
    }

    #[test]
    fn trailing_stars() {
        assert!(glob_match("ocx.sh/**", "ocx.sh/java:21"));
    }

    // ── No drift: global == package-specific match semantics ─────────────────
    //
    // The ADR §"One match semantic — no global/package-specific drift" mandates
    // that the SAME `match` pattern behaves identically whether it appears in the
    // root (global) descriptor or a package-specific sub-path descriptor.
    //
    // The invariant is structural: `glob_match` is a pure, stateless function
    // with no "scope" parameter — it cannot drift between global and
    // package-specific contexts by construction. Asserting `f(x) == f(x)` would
    // be a tautology and test nothing. Instead, these tests document the
    // invariant through the actual expected outcomes (which identifiers match and
    // which do not), covering the same cases regardless of how the pattern is
    // "scoped" by the caller.

    /// The `*` pattern matches the same identifiers regardless of descriptor scope.
    #[test]
    fn no_drift_star_global_equals_package_specific() {
        // The "no drift" invariant is structural: glob_match carries no scope
        // state. These assertions document the expected outcomes for the ADR
        // table cases — same results in any calling context.
        let pattern = "*";
        let should_match = [
            "ocx.sh/java:21",
            "ghcr.io/acme/cli:v1",
            "internal.company.com/certs/zscaler-root:latest",
            "", // empty
        ];
        for id in &should_match {
            assert!(
                glob_match(pattern, id),
                "pattern '*' must match '{id}' (no-drift: same result in any descriptor scope)"
            );
        }
    }

    /// The `ghcr.io/*` pattern is scope-agnostic — same result in global and
    /// package-specific descriptor positions.
    #[test]
    fn no_drift_registry_scoped_pattern_global_equals_package_specific() {
        let pattern = "ghcr.io/*";
        let should_match = ["ghcr.io/acme/cli:v1", "ghcr.io/java:latest"];
        let should_not_match = ["ocx.sh/java:21", "docker.io/library/ubuntu:22.04"];

        for id in &should_match {
            assert!(glob_match(pattern, id), "'{pattern}' must match '{id}' (no drift)");
        }
        for id in &should_not_match {
            assert!(!glob_match(pattern, id), "'{pattern}' must NOT match '{id}' (no drift)");
        }
    }

    /// The `*:21` pattern — same result regardless of which descriptor scope.
    #[test]
    fn no_drift_tag_scoped_pattern_global_equals_package_specific() {
        let pattern = "*:21";
        assert!(glob_match(pattern, "ocx.sh/java:21"), "must match :21");
        assert!(!glob_match(pattern, "ocx.sh/java:3.28"), "must not match :3.28");
    }

    /// Port and digest edge cases produce consistent results across scope framing.
    #[test]
    fn no_drift_edge_cases_port_and_digest() {
        // Port edge case — localhost:5000
        let port_pattern = "localhost:5000/*";
        let port_text = "localhost:5000/x:1";
        assert!(glob_match(port_pattern, port_text));

        // Digest edge case — *@sha256:*
        let digest_pattern = "*@sha256:*";
        let digest_text = "ghcr.io/cmake:3.28@sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        assert!(glob_match(digest_pattern, digest_text));
    }
}
