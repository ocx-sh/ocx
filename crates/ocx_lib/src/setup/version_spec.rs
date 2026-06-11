// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Version specification for `ocx self setup [VERSION]`.
//!
//! A [`VersionSpec`] is an identifier *suffix* — a tag, a digest, or both —
//! applied to a base [`oci::Identifier`] via [`VersionSpec::apply`]. The base
//! identifier supplies the registry and repository; the spec supplies the
//! version constraint.
//!
//! # Grammar
//!
//! ```text
//! VERSION = tag | digest | tag "@" digest
//! tag     = <identifier tag chars, same charset as OCI tag>
//! digest  = "sha256:" <64 hex chars> | "sha384:" <96 hex chars> | "sha512:" <128 hex chars>
//! ```
//!
//! Examples: `1.2.3`, `sha256:<64hex>`, `1.2.3@sha256:<64hex>`.
//!
//! A digest-only spec is written bare (`sha256:<hex>`, no `@`): the OCI tag
//! charset has no `:`, so the form is unambiguous, matching `LayerRef` and
//! docker/oras conventions. `@` appears only as the `tag@digest` separator.
//!
//! # Application order (invariant)
//!
//! [`apply`](VersionSpec::apply) sets the tag first, then the digest. This is
//! mandatory because [`Identifier::clone_with_tag`] drops any prior digest — if
//! the digest were applied first it would be silently erased on the subsequent
//! tag call.

use std::fmt;
use std::str::FromStr;

use crate::oci::{self, Digest};
use crate::setup;

/// An identifier suffix that pins an `ocx self setup` run to a specific
/// version of `ocx.sh/ocx/cli`.
///
/// The three variants mirror the grammar exactly — an empty spec is
/// unrepresentable by construction. With [`TagAndDigest`](Self::TagAndDigest),
/// `setup` resolves the tag and cross-checks the resulting digest against the
/// specified one (fail-closed immutability assertion per plan D9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionSpec {
    /// Tag only (`1.2.3`) — resolved against the registry or local index.
    Tag(String),
    /// Digest only (`sha256:<hex>`, bare) — installs the exact content, no
    /// tag resolution.
    Digest(Digest),
    /// Tag plus digest — resolves the tag, then asserts the resolved digest
    /// equals the specified one (plan D9, exit 65 on mismatch).
    TagAndDigest { tag: String, digest: Digest },
}

impl VersionSpec {
    /// Apply this spec to a base identifier, producing a new identifier that
    /// carries the specified tag and/or digest.
    ///
    /// # Application order (mandatory)
    ///
    /// Tag is applied first via [`Identifier::clone_with_tag`] (which drops
    /// any prior digest on the base), then the digest is applied via
    /// [`Identifier::clone_with_digest`]. This order is required: reversing it
    /// would silently drop the digest.
    pub fn apply(&self, base: oci::Identifier) -> oci::Identifier {
        // ORDER INVARIANT: tag first (clone_with_tag drops any prior digest),
        // then digest. Reversing would silently erase the digest.
        match self {
            Self::Tag(tag) => base.clone_with_tag(tag.as_str()),
            Self::Digest(digest) => base.clone_with_digest(digest.clone()),
            Self::TagAndDigest { tag, digest } => base.clone_with_tag(tag.as_str()).clone_with_digest(digest.clone()),
        }
    }

    /// Returns `true` when this spec carries an explicit digest component.
    ///
    /// Used by `ensure_self_installed` to decide whether to run the
    /// `tag@digest` cross-check (plan D9).
    pub fn is_pinned_digest(&self) -> bool {
        matches!(self, Self::Digest(_) | Self::TagAndDigest { .. })
    }

    /// Returns the tag component, if any.
    pub fn tag(&self) -> Option<&str> {
        match self {
            Self::Tag(tag) | Self::TagAndDigest { tag, .. } => Some(tag),
            Self::Digest(_) => None,
        }
    }

    /// Returns the digest component, if any.
    pub fn digest(&self) -> Option<&Digest> {
        match self {
            Self::Digest(digest) | Self::TagAndDigest { digest, .. } => Some(digest),
            Self::Tag(_) => None,
        }
    }
}

impl FromStr for VersionSpec {
    type Err = setup::error::Error;

    /// Parse a version spec string.
    ///
    /// Accepted forms:
    /// - `"1.2.3"` — tag only
    /// - `"sha256:<64hex>"` — digest only (bare, no `@`)
    /// - `"1.2.3@sha256:<64hex>"` — tag and digest
    ///
    /// Errors (map to exit 64 `UsageError` via clap `value_parser`):
    /// - Empty string
    /// - Double `@` (`"a@b@c"`)
    /// - Trailing `@` (`"1.2.3@"`)
    /// - Leading `@` (`"@sha256:..."`) — digest-only pins are written bare
    /// - Malformed digest (short hex, uppercase hex, unknown algorithm)
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = |reason: &str| setup::error::Error::InvalidVersionSpec {
            input: s.to_string(),
            reason: reason.to_string(),
        };

        if s.is_empty() {
            return Err(invalid("empty version spec"));
        }

        match s.split_once('@') {
            // `@` present: strictly the `tag@digest` form.
            Some((tag_part, digest_part)) => {
                if digest_part.contains('@') {
                    return Err(invalid("version spec may contain at most one '@'"));
                }
                if tag_part.is_empty() {
                    return Err(invalid(
                        "'@' separates tag from digest; write a digest-only pin bare (`sha256:<hex>`, no '@')",
                    ));
                }
                Ok(Self::TagAndDigest {
                    tag: validate_tag(tag_part).map_err(|reason| invalid(&reason))?,
                    digest: parse_digest(digest_part).map_err(|reason| invalid(&reason))?,
                })
            }
            // No `@`: a `:` means digest (the OCI tag charset has no `:`),
            // otherwise tag. Fail-closed: `sha256:<bad-hex>` surfaces a digest
            // error, never falls back to tag interpretation.
            None => {
                if s.contains(':') {
                    Ok(Self::Digest(parse_digest(s).map_err(|reason| invalid(&reason))?))
                } else {
                    Ok(Self::Tag(validate_tag(s).map_err(|reason| invalid(&reason))?))
                }
            }
        }
    }
}

/// Parses and validates a digest string (`sha256:<64hex>` style).
///
/// OCI digest hex must be lowercase. `Digest::try_from` tolerates uppercase
/// hex (`is_ascii_hexdigit`), so reject it explicitly before constructing the
/// digest — a digest pin is an immutability assertion and must not silently
/// accept a case-variant of the canonical lowercase form.
fn parse_digest(digest_str: &str) -> Result<Digest, String> {
    if digest_str.chars().any(|c| c.is_ascii_uppercase()) {
        return Err("invalid digest: hex must be lowercase (`sha256:<64hex>` style)".to_string());
    }
    Digest::try_from(digest_str)
        .map_err(|_| "invalid digest: expected `sha256:<64hex>`, `sha384:<96hex>`, or `sha512:<128hex>`".to_string())
}

/// Validates a tag against the OCI tag charset and normalizes `+` to `_`
/// (mirroring [`oci::Identifier`] tag handling).
///
/// OCI tags match `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}`. A `+` in the input is
/// normalized to `_` before validation, matching the identifier parser's
/// `normalize_tag` behavior, so the same forms users already pass to
/// identifiers are accepted here.
fn validate_tag(tag: &str) -> Result<String, String> {
    let normalized = tag.replace('+', "_");
    if normalized.len() > 128 {
        return Err(format!("tag too long: {} chars (max 128)", normalized.len()));
    }
    let mut chars = normalized.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() || first == '_' => {}
        _ => return Err(format!("invalid tag {tag:?}: must start with [a-zA-Z0-9_]")),
    }
    for ch in chars {
        if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-') {
            return Err(format!("invalid tag {tag:?}: character {ch:?} not in [a-zA-Z0-9._-]"));
        }
    }
    Ok(normalized)
}

impl fmt::Display for VersionSpec {
    /// Renders the spec in its grammar form: `tag`, `digest`, or `tag@digest`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tag(tag) => f.write_str(tag),
            Self::Digest(digest) => write!(f, "{digest}"),
            Self::TagAndDigest { tag, digest } => write!(f, "{tag}@{digest}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::ocx_cli_identifier;

    // ── Helper: valid 64-char lowercase hex ─────────────────────────────────

    fn hex64() -> String {
        "a".repeat(64)
    }

    fn hex96() -> String {
        "b".repeat(96)
    }

    fn hex128() -> String {
        "c".repeat(128)
    }

    // ── from_str: valid forms ────────────────────────────────────────────────

    /// UX row: tag-only VERSION → `VersionSpec` with tag set, no digest.
    ///
    /// Contract: `"1.2.3"` parses to `VersionSpec { tag: Some("1.2.3"), digest: None }`.
    #[test]
    fn from_str_tag_only() {
        let spec: VersionSpec = "1.2.3".parse().expect("tag-only spec must parse");
        // is_pinned_digest false: no digest component
        assert!(!spec.is_pinned_digest(), "tag-only spec must not be pinned-digest");
        // Round-trip via Display must show the tag
        let rendered = spec.to_string();
        assert!(
            rendered.contains("1.2.3"),
            "Display must include the tag; got: {rendered}"
        );
    }

    /// UX row: digest-only VERSION → `VersionSpec` with digest set, no tag.
    ///
    /// Contract: bare `"sha256:<64hex>"` parses to `VersionSpec::Digest(...)`.
    #[test]
    fn from_str_digest_only_sha256() {
        let input = format!("sha256:{}", hex64());
        let spec: VersionSpec = input.parse().expect("digest-only spec must parse");
        assert!(spec.is_pinned_digest(), "digest-only spec must be pinned-digest");
        let rendered = spec.to_string();
        assert!(
            rendered.contains("sha256:"),
            "Display must include 'sha256:'; got: {rendered}"
        );
    }

    /// Contract: bare `"sha384:<96hex>"` parses correctly (SHA-384 algorithm).
    #[test]
    fn from_str_digest_only_sha384() {
        let input = format!("sha384:{}", hex96());
        let spec: VersionSpec = input.parse().expect("sha384 digest-only spec must parse");
        assert!(spec.is_pinned_digest());
        let rendered = spec.to_string();
        assert!(
            rendered.contains("sha384:"),
            "Display must include 'sha384:'; got: {rendered}"
        );
    }

    /// Contract: bare `"sha512:<128hex>"` parses correctly (SHA-512 algorithm).
    #[test]
    fn from_str_digest_only_sha512() {
        let input = format!("sha512:{}", hex128());
        let spec: VersionSpec = input.parse().expect("sha512 digest-only spec must parse");
        assert!(spec.is_pinned_digest());
    }

    /// UX row: `tag@digest` VERSION → `VersionSpec` with both tag and digest.
    ///
    /// Contract: `"1.2.3@sha256:<64hex>"` parses to both tag and digest set.
    #[test]
    fn from_str_tag_and_digest() {
        let input = format!("1.2.3@sha256:{}", hex64());
        let spec: VersionSpec = input.parse().expect("tag@digest spec must parse");
        assert!(spec.is_pinned_digest(), "tag+digest spec must be pinned-digest");
        let rendered = spec.to_string();
        assert!(
            rendered.contains("1.2.3"),
            "Display must include the tag; got: {rendered}"
        );
        assert!(
            rendered.contains("sha256:"),
            "Display must include 'sha256:'; got: {rendered}"
        );
    }

    // ── from_str: invalid forms (all → InvalidVersionSpec, exit 64) ─────────

    /// Error taxonomy: empty string → `InvalidVersionSpec`.
    #[test]
    fn from_str_empty_string_is_error() {
        let err = "".parse::<VersionSpec>().expect_err("empty string must be rejected");
        assert!(
            matches!(err, crate::setup::error::Error::InvalidVersionSpec { .. }),
            "empty string must produce InvalidVersionSpec; got: {err:?}"
        );
    }

    /// Error taxonomy: trailing `@` (e.g. `"1.2.3@"`) → `InvalidVersionSpec`.
    #[test]
    fn from_str_trailing_at_is_error() {
        let err = "1.2.3@"
            .parse::<VersionSpec>()
            .expect_err("trailing @ must be rejected");
        assert!(
            matches!(err, crate::setup::error::Error::InvalidVersionSpec { .. }),
            "trailing @ must produce InvalidVersionSpec; got: {err:?}"
        );
    }

    /// Error taxonomy: bare `@` with no tag and no valid digest → `InvalidVersionSpec`.
    #[test]
    fn from_str_bare_at_is_error() {
        let err = "@".parse::<VersionSpec>().expect_err("bare @ must be rejected");
        assert!(
            matches!(err, crate::setup::error::Error::InvalidVersionSpec { .. }),
            "bare @ must produce InvalidVersionSpec; got: {err:?}"
        );
    }

    /// Error taxonomy: leading `@` (`"@sha256:<hex>"`) → `InvalidVersionSpec`.
    ///
    /// Digest-only pins are written bare; `@` exists only as the `tag@digest`
    /// separator. The error message points at the bare form.
    #[test]
    fn from_str_leading_at_digest_is_error() {
        let input = format!("@sha256:{}", hex64());
        let err = input
            .parse::<VersionSpec>()
            .expect_err("leading @ before digest must be rejected");
        assert!(
            matches!(err, crate::setup::error::Error::InvalidVersionSpec { .. }),
            "leading @ must produce InvalidVersionSpec; got: {err:?}"
        );
        assert!(
            err.to_string().contains("bare"),
            "error must hint at the bare digest form; got: {err}"
        );
    }

    /// Round-trip: bare digest parse → Display → reparse is structurally equal.
    #[test]
    fn digest_only_round_trips_through_display() {
        let input = format!("sha256:{}", hex64());
        let spec: VersionSpec = input.parse().unwrap();
        assert_eq!(spec.to_string(), input, "Display must round-trip the bare form");
        let reparsed: VersionSpec = spec.to_string().parse().unwrap();
        assert_eq!(spec, reparsed);
    }

    /// Error taxonomy: double `@` (`"a@b@c"`) → `InvalidVersionSpec`.
    #[test]
    fn from_str_double_at_is_error() {
        let input = format!("1.2.3@sha256:{}@extra", hex64());
        let err = input.parse::<VersionSpec>().expect_err("double @ must be rejected");
        assert!(
            matches!(err, crate::setup::error::Error::InvalidVersionSpec { .. }),
            "double @ must produce InvalidVersionSpec; got: {err:?}"
        );
    }

    /// Error taxonomy: short hex (< 64 chars for sha256) → `InvalidVersionSpec`.
    #[test]
    fn from_str_short_hex_is_error() {
        let err = "sha256:abc123"
            .parse::<VersionSpec>()
            .expect_err("short hex must be rejected");
        assert!(
            matches!(err, crate::setup::error::Error::InvalidVersionSpec { .. }),
            "short hex must produce InvalidVersionSpec; got: {err:?}"
        );
    }

    /// Error taxonomy: uppercase hex → `InvalidVersionSpec`.
    ///
    /// OCI digest hex must be lowercase; uppercase is rejected by `Digest::try_from`.
    #[test]
    fn from_str_uppercase_hex_is_error() {
        let input = format!("sha256:{}", "A".repeat(64));
        let err = input
            .parse::<VersionSpec>()
            .expect_err("uppercase hex must be rejected");
        assert!(
            matches!(err, crate::setup::error::Error::InvalidVersionSpec { .. }),
            "uppercase hex must produce InvalidVersionSpec; got: {err:?}"
        );
    }

    /// Error taxonomy: unknown algorithm (`@sha999:...`) → `InvalidVersionSpec`.
    #[test]
    fn from_str_unknown_algorithm_is_error() {
        let input = format!("sha999:{}", hex64());
        let err = input
            .parse::<VersionSpec>()
            .expect_err("unknown algorithm must be rejected");
        assert!(
            matches!(err, crate::setup::error::Error::InvalidVersionSpec { .. }),
            "unknown algorithm must produce InvalidVersionSpec; got: {err:?}"
        );
    }

    /// Error taxonomy: whitespace input → `InvalidVersionSpec`.
    #[test]
    fn from_str_whitespace_is_error() {
        let err = "  "
            .parse::<VersionSpec>()
            .expect_err("whitespace input must be rejected");
        assert!(
            matches!(err, crate::setup::error::Error::InvalidVersionSpec { .. }),
            "whitespace must produce InvalidVersionSpec; got: {err:?}"
        );
    }

    // ── apply: composition order invariant ──────────────────────────────────

    /// Contract D2: tag-only spec → identifier carries the new tag; no digest.
    #[test]
    fn apply_tag_only_sets_tag_and_no_digest() {
        let spec: VersionSpec = "0.9.2".parse().expect("tag-only spec must parse");
        let base = ocx_cli_identifier();
        let result = spec.apply(base);
        assert_eq!(result.tag(), Some("0.9.2"), "apply must set the specified tag");
        assert!(result.digest().is_none(), "tag-only apply must not carry a digest");
    }

    /// Contract D2: digest-only spec → identifier carries the digest; base tag preserved.
    ///
    /// When only a digest is specified (no tag), `apply` must not call `clone_with_tag`
    /// (which would drop the base identifier's tag). The base `ocx_cli_identifier()` has
    /// no tag, so this asserts the digest was attached.
    #[test]
    fn apply_digest_only_sets_digest() {
        let hex = hex64();
        let input = format!("sha256:{hex}");
        let spec: VersionSpec = input.parse().expect("digest-only spec must parse");
        let base = ocx_cli_identifier();
        let result = spec.apply(base);
        let digest = result.digest().expect("digest-only apply must set digest on result");
        assert!(
            digest.to_string().contains(&hex),
            "applied digest must match the spec hex"
        );
    }

    /// Contract D2 ORDER INVARIANT: `clone_with_tag` is called BEFORE `clone_with_digest`.
    ///
    /// `clone_with_tag` drops any prior digest. If digest were applied first and tag
    /// second, the digest would be silently erased. This test verifies the output
    /// carries BOTH tag and digest after applying a `tag@digest` spec.
    #[test]
    fn apply_tag_and_digest_preserves_both_order_invariant() {
        let hex = hex64();
        let input = format!("0.9.2@sha256:{hex}");
        let spec: VersionSpec = input.parse().expect("tag@digest spec must parse");
        let base = ocx_cli_identifier();
        let result = spec.apply(base);

        // Both tag and digest must be present — proves tag-first-then-digest order.
        assert_eq!(result.tag(), Some("0.9.2"), "apply must carry the new tag");
        let digest = result
            .digest()
            .expect("apply must carry the digest after tag-first application");
        assert!(
            digest.to_string().contains(&hex),
            "apply must carry the correct digest; got: {digest}"
        );
    }

    // ── validate_tag: length guard ───────────────────────────────────────────

    /// A tag longer than 128 chars (after `+`→`_` normalization) must be rejected.
    ///
    /// OCI tag limit is 128 chars (post-normalization). `validate_tag` checks
    /// `normalized.len() > 128`, so a 129-char string (valid charset) fails.
    #[test]
    fn from_str_tag_too_long_is_error() {
        // 129 chars of valid tag characters (after normalization) → reject.
        let long_tag = "a".repeat(129);
        let err = long_tag
            .parse::<VersionSpec>()
            .expect_err("tag longer than 128 chars must be rejected");
        assert!(
            matches!(err, crate::setup::error::Error::InvalidVersionSpec { .. }),
            "tag too long must produce InvalidVersionSpec; got: {err:?}"
        );
    }

    // ── validate_tag: mid-string illegal character ────────────────────────────

    /// A tag with an illegal character in the middle (`!`, space, …) must be rejected.
    ///
    /// The existing whitespace test covers first-char rejection only (` 1.2.3` fails
    /// the `[a-zA-Z0-9_]` first-char check). This test exercises the inner loop that
    /// rejects illegal characters mid-string.
    #[test]
    fn from_str_tag_mid_illegal_char_is_error() {
        // `!` is not in `[a-zA-Z0-9._-]` — rejected by the inner-loop guard.
        let err = "1.2.3!"
            .parse::<VersionSpec>()
            .expect_err("tag with mid-string '!' must be rejected");
        assert!(
            matches!(err, crate::setup::error::Error::InvalidVersionSpec { .. }),
            "mid-string illegal char must produce InvalidVersionSpec; got: {err:?}"
        );
    }

    /// A tag with an embedded space mid-string is also rejected by the inner loop.
    #[test]
    fn from_str_tag_mid_space_is_error() {
        // "1.2.3 4" — first char `1` passes, but space in middle is caught by inner loop.
        let err = "1.2.3 4"
            .parse::<VersionSpec>()
            .expect_err("tag with embedded space must be rejected");
        assert!(
            matches!(err, crate::setup::error::Error::InvalidVersionSpec { .. }),
            "tag with embedded space must produce InvalidVersionSpec; got: {err:?}"
        );
    }

    // ── validate_tag: `+` → `_` normalization round-trip ─────────────────────

    /// `"1.2.3+build"` normalizes to `"1.2.3_build"` and round-trips through `Display`.
    ///
    /// Contract: the spec stores the normalized form (`_`), not the raw input (`+`).
    /// Both `tag()` and `Display` must reflect the normalized value.
    #[test]
    fn from_str_plus_normalized_to_underscore() {
        let spec: VersionSpec = "1.2.3+build".parse().expect("tag with '+' must parse (normalization)");
        let tag = spec.tag().expect("parsed spec must have a tag component");
        assert_eq!(
            tag, "1.2.3_build",
            "tag() must return the normalized form ('_' not '+')"
        );
        let rendered = spec.to_string();
        assert_eq!(
            rendered, "1.2.3_build",
            "Display must round-trip to the normalized form; got: {rendered}"
        );
    }

    // ── is_pinned_digest ─────────────────────────────────────────────────────

    /// Contract: `is_pinned_digest()` returns `true` iff the spec has a digest component.
    #[test]
    fn is_pinned_digest_true_when_digest_present() {
        let spec: VersionSpec = format!("sha256:{}", hex64()).parse().unwrap();
        assert!(spec.is_pinned_digest());
    }

    #[test]
    fn is_pinned_digest_false_when_tag_only() {
        let spec: VersionSpec = "1.0.0".parse().unwrap();
        assert!(!spec.is_pinned_digest());
    }

    #[test]
    fn is_pinned_digest_true_for_tag_and_digest() {
        let spec: VersionSpec = format!("1.0.0@sha256:{}", hex64()).parse().unwrap();
        assert!(spec.is_pinned_digest());
    }

    // ── Display: round-trip contract ─────────────────────────────────────────

    /// Display must render tag-only as just the tag string (no `@` suffix).
    #[test]
    fn display_tag_only_renders_tag() {
        let spec: VersionSpec = "0.9.2".parse().unwrap();
        let rendered = spec.to_string();
        // Must contain the tag; must NOT contain `@` (no digest to append).
        assert!(
            rendered.contains("0.9.2"),
            "Display of tag-only must include tag; got: {rendered}"
        );
        assert!(
            !rendered.contains('@'),
            "Display of tag-only must not contain '@'; got: {rendered}"
        );
    }

    /// Display must render digest-only as bare `sha256:<hex>` (no `@`).
    #[test]
    fn display_digest_only_renders_bare_digest() {
        let hex = hex64();
        let spec: VersionSpec = format!("sha256:{hex}").parse().unwrap();
        let rendered = spec.to_string();
        assert!(
            rendered.contains("sha256:"),
            "Display of digest-only must include algorithm prefix; got: {rendered}"
        );
        assert!(
            rendered.contains(&hex),
            "Display of digest-only must include the hex; got: {rendered}"
        );
        assert!(
            !rendered.contains('@'),
            "Display of digest-only must be bare (no '@'); got: {rendered}"
        );
    }

    /// Display of `tag@digest` must include both and use `@` as separator.
    #[test]
    fn display_tag_and_digest_renders_both() {
        let hex = hex64();
        let spec: VersionSpec = format!("0.9.2@sha256:{hex}").parse().unwrap();
        let rendered = spec.to_string();
        assert!(
            rendered.contains("0.9.2"),
            "Display of tag@digest must include tag; got: {rendered}"
        );
        assert!(
            rendered.contains('@'),
            "Display of tag@digest must include '@'; got: {rendered}"
        );
        assert!(
            rendered.contains("sha256:"),
            "Display of tag@digest must include algo prefix; got: {rendered}"
        );
    }
}
