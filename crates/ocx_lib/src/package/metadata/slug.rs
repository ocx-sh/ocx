// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared slug pattern used by dependency and entrypoint names plus the
//! `${deps.NAME.field}` template token regex.

use std::sync::LazyLock;

use regex::Regex;

pub const SLUG_PATTERN_STR: &str = r"^[a-z0-9][a-z0-9_-]*$";
pub const SLUG_MAX_LEN: usize = 64;

pub static SLUG_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(SLUG_PATTERN_STR).expect("valid slug regex"));

/// Regex matching `${deps.NAME.FIELD}` template tokens.
///
/// Capture group 1 = NAME (slug body, anchors stripped from `SLUG_PATTERN_STR` so
/// the accepted character class stays in sync with `DependencyName` validation).
/// Capture group 2 = FIELD (`[a-zA-Z]+`).
///
/// Built once and reused by `template::TemplateResolver`,
/// `validation::validate_env_tokens`, and `validation::validate_entrypoints` so
/// the three sites cannot drift out of sync.
pub static DEP_TOKEN_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    let slug_body = SLUG_PATTERN_STR.trim_start_matches('^').trim_end_matches('$');
    let pattern = format!(r"\$\{{deps\.({slug_body})\.([a-zA-Z]+)\}}");
    Regex::new(&pattern).expect("valid dep-token regex")
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_pattern_matches_valid_slugs() {
        assert!(SLUG_PATTERN.is_match("foo-bar_1"));
        assert!(SLUG_PATTERN.is_match("a"));
        assert!(SLUG_PATTERN.is_match("0abc"));
        assert!(SLUG_PATTERN.is_match("my-dep"));
        assert!(SLUG_PATTERN.is_match("dep_1"));
    }

    #[test]
    fn slug_pattern_rejects_invalid_slugs() {
        assert!(!SLUG_PATTERN.is_match("Bad"), "uppercase not allowed");
        assert!(!SLUG_PATTERN.is_match(""), "empty not allowed");
        assert!(!SLUG_PATTERN.is_match("_start"), "leading underscore not allowed");
        assert!(!SLUG_PATTERN.is_match("-start"), "leading dash not allowed");
        assert!(!SLUG_PATTERN.is_match("a/b"), "slash not allowed");
    }

    #[test]
    fn slug_max_len_boundary() {
        let at_cap = "a".repeat(SLUG_MAX_LEN);
        assert!(SLUG_PATTERN.is_match(&at_cap), "64-char slug must match");

        // Pattern itself has no length limit — max len is enforced by callers.
        // This test just confirms the constant value is 64.
        assert_eq!(SLUG_MAX_LEN, 64);
    }
}
