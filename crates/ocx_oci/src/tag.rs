// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The OCI-side tag vocabulary: names a registry holds that are not package versions.
//!
//! Reservedness is wire-visible, so the rule may move but never change
//! (`crates/ocx_package/tests/tag_verdicts.rs`).

use super::{Algorithm, Digest};

const RESERVED_INTERNAL_PREFIX: &str = "__ocx";

/// Known OCX-internal tag types; one from a newer OCX is [`Unknown`](InternalTag::Unknown), never an error.
#[derive(Debug, Clone)]
pub enum InternalTag {
    Description,
    Patch,
    /// `__ocx.keep.<algorithm>-<hex>`: keeps a platform manifest a lock pins
    /// reachable through registry GC (`adr_index_indirection.md` Decision E).
    Keep {
        algorithm: Algorithm,
        /// Verbatim as tagged, either case.
        hex: String,
    },
    Unknown(String),
}

impl InternalTag {
    pub const DESCRIPTION_TAG: &str = "__ocx.desc";

    pub const PATCH_TAG: &str = "__ocx.patch";

    pub const KEEP_TAG_PREFIX: &str = "__ocx.keep.";

    /// Classify a tag already known to sit in the `__ocx` namespace.
    pub fn from_tag(value: &str) -> Self {
        match value {
            Self::DESCRIPTION_TAG => InternalTag::Description,
            Self::PATCH_TAG => InternalTag::Patch,
            _ => value
                .strip_prefix(Self::KEEP_TAG_PREFIX)
                .and_then(|body| parse_keep(body, '-'))
                .map_or_else(
                    || InternalTag::Unknown(value.to_string()),
                    |(algorithm, hex)| InternalTag::Keep {
                        algorithm,
                        hex: hex.to_string(),
                    },
                ),
        }
    }
}

impl std::fmt::Display for InternalTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InternalTag::Description => write!(f, "{}", Self::DESCRIPTION_TAG),
            InternalTag::Patch => write!(f, "{}", Self::PATCH_TAG),
            InternalTag::Keep { algorithm, hex } => {
                write!(f, "{}{}-{}", Self::KEEP_TAG_PREFIX, algorithm.prefix(), hex)
            }
            InternalTag::Unknown(tag) => write!(f, "{}", tag),
        }
    }
}

/// Whether `tag` names the OCX-internal `__ocx` namespace (case-insensitive prefix, so `__ocxfoo` matches).
pub fn is_internal_namespace(tag: &str) -> bool {
    tag.get(..RESERVED_INTERNAL_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RESERVED_INTERNAL_PREFIX))
}

/// Matches a keep-tag digest body `<algorithm><separator><hex>` over every
/// supported algorithm: `'.'` for the frozen legacy form, `'-'` for [`InternalTag::Keep`].
pub fn parse_keep(value: &str, separator: char) -> Option<(Algorithm, &str)> {
    Algorithm::ALL.iter().find_map(|algorithm| {
        let hex = value.strip_prefix(algorithm.prefix())?.strip_prefix(separator)?;
        (hex.len() == algorithm.hex_len() && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some((*algorithm, hex))
    })
}

/// The distribution spec truncates a fallback tag's encoded section to 64
/// characters; sha384/sha512 subjects sharing a prefix share one tag.
const REFERRER_FALLBACK_ENCODED_LEN: usize = 64;

/// The OCI Referrers tag-schema fallback tag naming `digest`'s referrers index.
pub fn referrer_fallback_tag(digest: &Digest) -> String {
    let (algorithm, hex) = digest.parts();
    // `chars`, not a byte slice: an in-crate `Digest` can bypass validation, and a byte slice can panic mid-char.
    let encoded: String = hex.chars().take(REFERRER_FALLBACK_ENCODED_LEN).collect();
    format!("{algorithm}-{encoded}")
}

/// Cosign sidecar tag suffixes, shared by the classifier and the readers so the two cannot drift.
pub const SIG_SIDECAR_SUFFIX: &str = ".sig";
pub const ATT_SIDECAR_SUFFIX: &str = ".att";
pub(crate) const SBOM_SIDECAR_SUFFIX: &str = ".sbom";

/// Every cosign sidecar suffix, in a fixed sweep order.
pub(crate) const SIDECAR_SUFFIXES: [&str; 3] = [SIG_SIDECAR_SUFFIX, ATT_SIDECAR_SUFFIX, SBOM_SIDECAR_SUFFIX];

/// `<algorithm>-<hex><suffix>` — the cosign sidecar tag naming an attachment of `subject`.
pub fn sidecar_tag(subject: &Digest, suffix: &str) -> String {
    let mut tag = referrer_fallback_tag(subject);
    tag.push_str(suffix);
    tag
}

/// The cosign `sha256-<hex>.sbom` sidecar tag; a second `cosign attach sbom` replaces its manifest.
pub fn sbom_sidecar_tag(subject: &Digest) -> String {
    sidecar_tag(subject, SBOM_SIDECAR_SUFFIX)
}

/// Matches the OCI Referrers fallback tag `<algorithm>-<hex>` and its cosign suffixes.
///
/// The set only grows: narrowing it accepts as a version a tag once refused as one.
pub fn is_referrer_fallback_tag(value: &str) -> bool {
    let base = value
        .strip_suffix(SIG_SIDECAR_SUFFIX)
        .or_else(|| value.strip_suffix(ATT_SIDECAR_SUFFIX))
        .or_else(|| value.strip_suffix(SBOM_SIDECAR_SUFFIX))
        .unwrap_or(value);
    Algorithm::ALL.iter().any(|algorithm| {
        base.strip_prefix(algorithm.prefix())
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|hex| {
                (hex.len() == REFERRER_FALLBACK_ENCODED_LEN || hex.len() == algorithm.hex_len())
                    && hex.chars().all(|c| c.is_ascii_hexdigit())
            })
    })
}

/// Whether `tag` names something the registry holds that is not a package version.
///
/// Never parses `tag` as a version; `crates/ocx_package/tests/tag_verdicts.rs`
/// pins it equal to `Tag::is_reserved_str`.
#[must_use]
pub fn is_reserved_tag(tag: &str) -> bool {
    is_internal_namespace(tag) || parse_keep(tag, '.').is_some() || is_referrer_fallback_tag(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(len: usize) -> String {
        "a".repeat(len)
    }

    #[test]
    fn internal_namespace_matches_the_whole_prefix_case_insensitively() {
        assert!(is_internal_namespace("__ocx.desc"));
        assert!(is_internal_namespace("__ocx.future"));
        assert!(is_internal_namespace("__ocx"));
        assert!(is_internal_namespace("__ocxfoo"));
        assert!(is_internal_namespace("__OCX.desc"));
        assert!(!is_internal_namespace("latest"));
        assert!(!is_internal_namespace("3.28.1"));
        assert!(!is_internal_namespace("debug"));
        assert!(!is_internal_namespace("__oc"));
        assert!(!is_internal_namespace("x__ocx"));
    }

    /// The spec truncates the encoded section to 64 characters for **every**
    /// algorithm, so sha384 and sha512 subjects land on a 64-character tag and
    /// two subjects sharing that prefix share one referrers index.
    #[test]
    fn referrer_fallback_tag_truncates_the_encoded_section_to_64() {
        let cases = [
            (Digest::Sha256("a".repeat(64)), "sha256", 64),
            (Digest::Sha384("b".repeat(96)), "sha384", 64),
            (Digest::Sha512("c".repeat(128)), "sha512", 64),
        ];
        for (digest, prefix, encoded_len) in cases {
            let tag = referrer_fallback_tag(&digest);
            let body = tag
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('-'))
                .unwrap_or_else(|| panic!("{tag} must be '{prefix}-<encoded>'"));
            assert_eq!(
                body.len(),
                encoded_len,
                "{tag}: the encoded section is truncated to 64 for every algorithm"
            );
            assert!(
                digest.hex().starts_with(body),
                "{tag} must be a prefix of the digest body"
            );
        }
    }

    /// `"latest"` is the one string the version-free formula could have been
    /// expected to name explicitly, and does not: each of the three predicates
    /// declines it on its own, so the formula answers `false` without a second
    /// copy of the package layer's literal living here.
    ///
    /// Asserted through the three predicates as well as the verdict, because
    /// the verdict alone would stay green if one of them started matching while
    /// another stopped.
    #[test]
    fn latest_needs_no_special_case() {
        assert!(!is_internal_namespace("latest"));
        assert!(parse_keep("latest", '.').is_none());
        assert!(!is_referrer_fallback_tag("latest"));
        assert!(!is_reserved_tag("latest"));
    }

    /// Every shape this module recognises, stated as a verdict rather than
    /// compared against the implementation. The cross-check against
    /// `Tag::is_reserved` lives in `tests/tag_verdicts.rs`; this table
    /// is what `is_reserved_tag` answers on its own.
    #[test]
    fn is_reserved_tag_verdict_table() {
        let cases = [
            ("__ocx.desc".to_string(), true),
            ("__ocx.patch".to_string(), true),
            ("__ocx".to_string(), true),
            ("__ocxfoo".to_string(), true),
            ("__OCX.desc".to_string(), true),
            (format!("__ocx.keep.sha256-{}", hex(64)), true),
            (format!("__ocx.keep.sha256-{}", hex(63)), true),
            (format!("sha256.{}", hex(64)), true),
            (format!("sha384.{}", hex(96)), true),
            (format!("sha512.{}", hex(128)), true),
            (format!("sha256-{}", hex(64)), true),
            (format!("sha256-{}.sig", hex(64)), true),
            (format!("sha256-{}.att", hex(64)), true),
            (format!("sha256-{}.sbom", hex(64)), true),
            (format!("sha384-{}", hex(64)), true),
            (format!("sha512-{}", hex(128)), true),
            ("latest".to_string(), false),
            ("3.28.1".to_string(), false),
            ("debug-3.12".to_string(), false),
            ("custom-tag".to_string(), false),
            ("__oc".to_string(), false),
            ("x__ocx".to_string(), false),
            (format!("sha256-{}", hex(63)), false),
            (format!("sha256:{}", hex(64)), false),
            (format!("sha256.{}", hex(63)), false),
            (format!("sha384.{}", hex(64)), false),
            (format!("sha256.{}", "z".repeat(64)), false),
            (format!("sha256{}", hex(64)), false),
        ];
        for (raw, expected) in cases {
            assert_eq!(is_reserved_tag(&raw), expected, "is_reserved_tag('{raw}')");
        }
    }

    /// The written keep-tag form round-trips verbatim through `Display`, and a
    /// malformed digest body lands on `Unknown` rather than escaping the
    /// namespace.
    #[test]
    fn internal_tag_classifies_and_round_trips_the_keep_form() {
        for (algorithm, len) in [
            (Algorithm::Sha256, 64usize),
            (Algorithm::Sha384, 96),
            (Algorithm::Sha512, 128),
        ] {
            let body = hex(len);
            let raw = format!("__ocx.keep.{}-{}", algorithm.prefix(), body);
            let tag = InternalTag::from_tag(&raw);
            assert!(
                matches!(&tag, InternalTag::Keep { algorithm: a, hex: h } if *a == algorithm && h == &body),
                "{raw}: {tag:?}"
            );
            assert_eq!(tag.to_string(), raw, "Display must round-trip '{raw}'");
        }

        for raw in [
            format!("__ocx.keep.sha256-{}", hex(63)),
            format!("__ocx.keep.sha256-{}", "z".repeat(64)),
            format!("__ocx.keep.sha256.{}", hex(64)),
            format!("__ocx.keep.sha384-{}", hex(64)),
        ] {
            let tag = InternalTag::from_tag(&raw);
            assert!(
                matches!(tag, InternalTag::Unknown(_)),
                "'{raw}' must not classify as Keep, got {tag:?}"
            );
            assert_eq!(tag.to_string(), raw);
        }
    }
}
