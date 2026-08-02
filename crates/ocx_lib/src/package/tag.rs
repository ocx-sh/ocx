// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use crate::{oci::Algorithm, package::version};

/// The OCX-internal tag namespace. The prefix *is* the namespace, so the whole
/// of it is reserved: no separator is required after it and the match is
/// case-insensitive.
const RESERVED_INTERNAL_PREFIX: &str = "__ocx";

/// Known OCX-internal tag types.
///
/// Internal tags live in the `__ocx` namespace and name
/// metadata artifacts. Unknown internal tags (from newer OCX versions) are
/// preserved as [`Unknown`](InternalTag::Unknown) rather than causing errors.
#[derive(Debug, Clone)]
pub enum InternalTag {
    /// Package description artifact (`__ocx.desc`).
    Description,
    /// Infrastructure patch descriptor artifact (`__ocx.patch`).
    Patch,
    /// An internal tag not recognized by this version of OCX.
    Unknown(String),
}

impl InternalTag {
    /// The OCI tag string for description artifacts.
    pub const DESCRIPTION_TAG: &str = "__ocx.desc";

    /// The OCI tag string for patch descriptor artifacts.
    ///
    /// It sits in the `__ocx` namespace, so [`Tag::is_reserved`] returns `true`
    /// for it and it is excluded from user-facing tag listings without any
    /// additional filtering.
    pub const PATCH_TAG: &str = "__ocx.patch";

    fn from_tag(value: &str) -> Self {
        match value {
            Self::DESCRIPTION_TAG => InternalTag::Description,
            Self::PATCH_TAG => InternalTag::Patch,
            _ => InternalTag::Unknown(value.to_string()),
        }
    }
}

impl std::fmt::Display for InternalTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InternalTag::Description => write!(f, "{}", Self::DESCRIPTION_TAG),
            InternalTag::Patch => write!(f, "{}", Self::PATCH_TAG),
            InternalTag::Unknown(tag) => write!(f, "{}", tag),
        }
    }
}

/// Semantic classification of an OCI tag string.
///
/// Parsed from a raw tag string via `Tag::from(String)`. The parse order is:
/// 1. `"latest"` → [`Latest`](Tag::Latest)
/// 2. The `__ocx` namespace → [`Internal`](Tag::Internal)
/// 3. Version-parseable (digit-first or variant-prefixed) → [`Version`](Tag::Version)
/// 4. Digest-alias tag (`sha256.<hex>`) → [`Canonical`](Tag::Canonical)
/// 5. Anything else → [`Other`](Tag::Other)
///
/// Bare variant names (e.g., `"debug"`, `"canary"`) fall into [`Other`](Tag::Other).
/// Variant semantics are determined at a higher layer (mirror spec, package
/// annotations) where declared variants are known — the `Tag` enum is purely
/// syntactic and does not guess intent.
#[derive(Debug, Clone)]
pub enum Tag {
    /// The literal `"latest"` tag — latest version of the default variant.
    Latest,
    /// An OCX-internal tag in the `__ocx` namespace. Used for metadata artifacts
    /// like package descriptions. Excluded from user-facing tag listings.
    Internal(InternalTag),
    /// A semantic version, optionally with a variant prefix.
    /// Examples: `"3.28.1"`, `"3.28.1-alpha_b1"`, `"debug-3.12.5"`.
    Version(version::Version),
    /// A digest-alias tag naming a platform manifest by its own digest, e.g.
    /// `"sha256.abcdef…"`.
    ///
    /// The parts are carried separately rather than as an [`crate::oci::Digest`]
    /// because a tag spells them `<algorithm>.<hex>` — OCI forbids `:` in a tag,
    /// which is the separator `Digest`'s `Display` emits.
    Canonical {
        /// The digest algorithm the tag names.
        algorithm: Algorithm,
        /// The lower- or upper-case hex digest body, verbatim as tagged.
        hex: String,
    },
    /// Any tag that doesn't match the above patterns.
    /// Includes bare variant names (`"debug"`) and arbitrary user-chosen tags (`"custom-tag"`).
    Other(String),
}

const LATEST_STR: &str = "latest";

/// Matches the digest-alias tag form `<algorithm>.<hex>` over every supported
/// algorithm. Deliberately wider than the `sha256` tags `push_canonical_tag`
/// writes today: reserving a name costs nothing, and a `sha384.…` tag would be
/// no more a version than a `sha256.…` one.
fn parse_canonical(value: &str) -> Option<(Algorithm, &str)> {
    Algorithm::ALL.iter().find_map(|algorithm| {
        let hex = value.strip_prefix(algorithm.prefix())?.strip_prefix('.')?;
        (hex.len() == algorithm.hex_len() && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some((*algorithm, hex))
    })
}

/// Matches the OCI Referrers tag-schema fallback shape `<algorithm>-<hex>`
/// and its legacy `cosign` artifact suffixes `<algorithm>-<hex>.sig` /
/// `<algorithm>-<hex>.att` — the dash-separated digest tags a registry
/// without native Referrers-API support (or an older `cosign`) parks
/// signature/attestation manifests under. OCX never writes these
/// (`ocx package sign` fails closed with `ExitCode::ReferrersUnsupported`
/// instead of falling back), but a third-party repo signed elsewhere still
/// carries them, and they name a signature or attestation manifest, never a
/// package version — same rule as the dot-form [`parse_canonical`] alias,
/// spelled with a dash because that is the tag-schema convention.
fn is_referrer_fallback_tag(value: &str) -> bool {
    let base = value
        .strip_suffix(".sig")
        .or_else(|| value.strip_suffix(".att"))
        .unwrap_or(value);
    Algorithm::ALL.iter().any(|algorithm| {
        base.strip_prefix(algorithm.prefix())
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|hex| hex.len() == algorithm.hex_len() && hex.chars().all(|c| c.is_ascii_hexdigit()))
    })
}

impl Tag {
    /// Returns `true` if this tag is not a version pointer: the OCX-internal
    /// namespace, a digest-alias tag naming a platform manifest by its own
    /// digest, or an OCI Referrers fallback / `cosign` signature-artifact tag
    /// ([`is_referrer_fallback_tag`]). None of these may appear as a version
    /// in the index.
    pub fn is_reserved(&self) -> bool {
        match self {
            Tag::Internal(_) | Tag::Canonical { .. } => true,
            Tag::Other(value) => is_referrer_fallback_tag(value),
            _ => false,
        }
    }

    /// `&str` convenience wrapper over [`Tag::is_reserved`] for listing filters.
    ///
    /// One allocation and one full classification per listed tag, deliberately:
    /// there is exactly one implementation of the rule, and it is [`Tag::from`].
    /// Filtering a listing while also needing the parsed tag should build one
    /// [`Tag`] and reuse it instead.
    pub fn is_reserved_str(tag: &str) -> bool {
        Tag::from(tag.to_string()).is_reserved()
    }
}

/// Whether `tag` names the OCX-internal `__ocx` namespace. Case-insensitive and
/// prefix-based, so `__ocx`, `__ocxfoo` and `__OCX.desc` all match. Private:
/// [`Tag::from`] is the one classifier, and [`Tag::is_reserved`] is the one
/// verdict callers ask for.
fn is_internal_namespace(tag: &str) -> bool {
    tag.get(..RESERVED_INTERNAL_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RESERVED_INTERNAL_PREFIX))
}

impl From<String> for Tag {
    fn from(value: String) -> Self {
        if value == LATEST_STR {
            Tag::Latest
        } else if is_internal_namespace(&value) {
            Tag::Internal(InternalTag::from_tag(&value))
        } else if let Some(version) = version::Version::parse(value.as_ref()) {
            Tag::Version(version)
        } else if let Some((algorithm, hex)) = parse_canonical(&value) {
            Tag::Canonical {
                algorithm,
                hex: hex.to_string(),
            }
        } else {
            Tag::Other(value)
        }
    }
}

impl std::fmt::Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s: String = self.clone().into();
        write!(f, "{}", s)
    }
}

impl From<Tag> for String {
    fn from(val: Tag) -> Self {
        match val {
            Tag::Latest => LATEST_STR.to_string(),
            Tag::Internal(internal) => internal.to_string(),
            Tag::Version(version) => version.to_string(),
            Tag::Canonical { algorithm, hex } => format!("{}.{}", algorithm.prefix(), hex),
            Tag::Other(other) => other,
        }
    }
}

impl Serialize for Tag {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s: String = self.clone().into();
        serializer.serialize_str(&s)
    }
}

impl<'de> Deserialize<'de> for Tag {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Tag::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(len: usize) -> String {
        "a".repeat(len)
    }

    #[test]
    fn test_tag_parsing() {
        let latest_tag = Tag::from("latest".to_string());
        assert!(matches!(latest_tag, Tag::Latest));
        assert_eq!(latest_tag.to_string(), "latest");

        let version_tag = Tag::from("1.2.3-alpha".to_string());
        assert!(matches!(version_tag, Tag::Version(_)));
        assert_eq!(version_tag.to_string(), "1.2.3-alpha");

        let canonical_tag = Tag::from(format!("sha256.{}", hex(64)));
        assert!(matches!(canonical_tag, Tag::Canonical { .. }));
        assert_eq!(canonical_tag.to_string(), format!("sha256.{}", hex(64)));

        let other_tag = Tag::from("custom-tag".to_string());
        assert!(matches!(other_tag, Tag::Other(_)));
        assert_eq!(other_tag.to_string(), "custom-tag");
    }

    // ── Reserved-tag verdict (ADR D7) ─────────────────────────────

    #[test]
    fn tag_classifies_the_sha256_dot_form_as_canonical() {
        let tag = Tag::from(format!("sha256.{}", hex(64)));
        assert!(
            matches!(&tag, Tag::Canonical { algorithm, hex: body }
                if *algorithm == Algorithm::Sha256 && body.len() == 64),
            "got {tag:?}"
        );
        assert!(tag.is_reserved());
    }

    /// The retarget is over `Algorithm::ALL`, not `sha256` alone — and the hex
    /// length is per-algorithm, so a 64-hex `sha384.` tag is not canonical.
    #[test]
    fn tag_classifies_every_algorithm_dot_form_as_canonical() {
        for (algorithm, len) in [
            (Algorithm::Sha256, 64usize),
            (Algorithm::Sha384, 96),
            (Algorithm::Sha512, 128),
        ] {
            let raw = format!("{}.{}", algorithm.prefix(), hex(len));
            let tag = Tag::from(raw.clone());
            assert!(
                matches!(&tag, Tag::Canonical { algorithm: a, .. } if *a == algorithm),
                "{raw}: {tag:?}"
            );
            assert_eq!(tag.to_string(), raw);
        }

        let wrong_length = Tag::from(format!("sha384.{}", hex(64)));
        assert!(matches!(wrong_length, Tag::Other(_)), "got {wrong_length:?}");
        assert!(!wrong_length.is_reserved());
    }

    #[test]
    fn tag_round_trips_the_dot_form_verbatim() {
        for raw in [
            format!("sha256.{}", hex(64)),
            format!("sha384.{}", hex(96)),
            format!("sha512.{}", hex(128)),
        ] {
            let tag = Tag::from(raw.clone());
            let round_tripped: String = tag.clone().into();
            assert_eq!(round_tripped, raw, "String::from({tag:?})");
            assert_eq!(
                serde_json::to_string(&tag).expect("serialize"),
                format!("\"{raw}\""),
                "Serialize routes through From<Tag> for String"
            );
        }
    }

    // ── Referrer fallback tag verdict ─────────────────────────────

    /// The dash-form `<algorithm>-<hex>` is the OCI Referrers tag-schema
    /// fallback shape. It stays classified `Tag::Other` (unlike the dot-form
    /// alias, it is not `Tag::Canonical`) but must still be reserved.
    #[test]
    fn tag_classifies_sha256_dash_form_as_reserved_other() {
        let tag = Tag::from(format!("sha256-{}", hex(64)));
        assert!(matches!(tag, Tag::Other(_)), "got {tag:?}");
        assert!(tag.is_reserved());
    }

    #[test]
    fn tag_classifies_dash_form_sig_suffix_as_reserved() {
        let raw = format!("sha256-{}.sig", hex(64));
        let tag = Tag::from(raw.clone());
        assert!(matches!(tag, Tag::Other(_)), "got {tag:?}");
        assert!(tag.is_reserved(), "'{raw}' should be reserved");
    }

    #[test]
    fn tag_classifies_dash_form_att_suffix_as_reserved() {
        let raw = format!("sha256-{}.att", hex(64));
        let tag = Tag::from(raw.clone());
        assert!(matches!(tag, Tag::Other(_)), "got {tag:?}");
        assert!(tag.is_reserved(), "'{raw}' should be reserved");
    }

    #[test]
    fn tag_does_not_reserve_dash_form_boundary_cases() {
        for raw in [
            format!("sha256-{}", hex(63)),        // wrong hex length
            format!("sha256-{}.sbom", hex(64)),   // unrecognized suffix
            "v1.2.3".to_string(),                 // no dash-digest shape at all
        ] {
            let tag = Tag::from(raw.clone());
            assert!(!tag.is_reserved(), "'{raw}' should not be reserved, got {tag:?}");
        }
    }

    /// `:` is illegal in an OCI tag, so the colon form is not a tag OCX ever
    /// writes and is not classified.
    #[test]
    fn tag_rejects_the_colon_form_as_other() {
        let raw = format!("sha256:{}", hex(64));
        let tag = Tag::from(raw.clone());
        assert!(matches!(tag, Tag::Other(_)), "got {tag:?}");
        assert!(!Tag::is_reserved_str(&raw));
    }

    #[test]
    fn tag_reserves_bare_ocx_and_ocxfoo_and_uppercase() {
        for raw in [
            "__ocx.desc",
            "__ocx.patch",
            "__ocx",
            "__ocxfoo",
            "__OCX.desc",
            "__ocx.FUTURE",
            "__Ocx",
        ] {
            let tag = Tag::from(raw.to_string());
            assert!(
                matches!(tag, Tag::Internal(_)),
                "'{raw}' should be Internal, got {tag:?}"
            );
            assert!(tag.is_reserved(), "'{raw}' should be reserved");
            assert_eq!(tag.to_string(), raw);
        }
    }

    #[test]
    fn tag_does_not_reserve_boundary_forms() {
        for raw in [
            "3.28.1",
            "latest",
            "debug-3.12",
            "custom-tag",
            "debug",
            "__oc",
            "x__ocx",
        ] {
            let tag = Tag::from(raw.to_string());
            assert!(!tag.is_reserved(), "'{raw}' should not be reserved, got {tag:?}");
        }

        for raw in [
            format!("sha256.{}", hex(63)),
            format!("sha256.{}", "z".repeat(64)),
            format!("sha256{}", hex(64)),
        ] {
            let tag = Tag::from(raw.clone());
            assert!(matches!(tag, Tag::Other(_)), "'{raw}' should be Other, got {tag:?}");
            assert!(!tag.is_reserved(), "'{raw}' should not be reserved");
        }
    }

    /// The D7 verdict table: every row states the expected verdict, so the
    /// assertion never compares the implementation against itself. Both entry
    /// points — the parsed [`Tag`] and the `&str` wrapper — are checked against
    /// that stated verdict.
    #[test]
    fn d7_reserved_tag_verdict_table() {
        let cases = [
            ("__ocx.desc".to_string(), true),
            ("__ocx.patch".to_string(), true),
            ("__ocx".to_string(), true),
            ("__ocxfoo".to_string(), true),
            ("__OCX.desc".to_string(), true),
            ("__ocx.FUTURE".to_string(), true),
            ("__Ocx".to_string(), true),
            (format!("sha256.{}", hex(64)), true),
            (format!("sha384.{}", hex(96)), true),
            (format!("sha512.{}", hex(128)), true),
            (format!("sha256-{}", hex(64)), true),
            (format!("sha256-{}.sig", hex(64)), true),
            (format!("sha256-{}.att", hex(64)), true),
            (format!("sha256-{}", hex(63)), false),
            (format!("sha256-{}.sbom", hex(64)), false),
            (format!("sha256:{}", hex(64)), false),
            (format!("sha256.{}", hex(63)), false),
            (format!("sha384.{}", hex(64)), false),
            (format!("sha256.{}", "z".repeat(64)), false),
            (format!("sha256{}", hex(64)), false),
            ("3.28.1".to_string(), false),
            ("latest".to_string(), false),
            ("debug-3.12".to_string(), false),
            ("debug".to_string(), false),
            ("custom-tag".to_string(), false),
            ("__oc".to_string(), false),
            ("x__ocx".to_string(), false),
        ];
        for (raw, expected) in cases {
            assert_eq!(Tag::from(raw.clone()).is_reserved(), expected, "Tag::from('{raw}')");
            assert_eq!(Tag::is_reserved_str(&raw), expected, "Tag::is_reserved_str('{raw}')");
        }
    }

    #[test]
    fn tag_internal_description() {
        let tag = Tag::from("__ocx.desc".to_string());
        assert!(matches!(tag, Tag::Internal(InternalTag::Description)));
        assert_eq!(tag.to_string(), "__ocx.desc");
    }

    #[test]
    fn tag_internal_unknown_forward_compat() {
        let tag = Tag::from("__ocx.sbom".to_string());
        assert!(matches!(tag, Tag::Internal(InternalTag::Unknown(_))));
        assert_eq!(tag.to_string(), "__ocx.sbom");
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

    /// `PATCH_TAG` must be auto-excluded by the `__ocx` namespace without any
    /// extra filtering. `Tag::is_reserved` is the verdict.
    #[test]
    fn patch_tag_is_reserved() {
        assert_eq!(InternalTag::PATCH_TAG, "__ocx.patch");
        assert!(Tag::is_reserved_str(InternalTag::PATCH_TAG));
        let tag = Tag::from(InternalTag::PATCH_TAG.to_string());
        assert!(tag.is_reserved(), "Tag::from(PATCH_TAG) must be reserved");
    }

    /// `Tag::from(PATCH_TAG)` must produce `Tag::Internal(InternalTag::Patch)`,
    /// not the `Unknown` fallback.
    #[test]
    fn patch_tag_maps_to_patch_variant() {
        let tag = Tag::from(InternalTag::PATCH_TAG.to_string());
        assert!(
            matches!(tag, Tag::Internal(InternalTag::Patch)),
            "Tag::from(PATCH_TAG) must yield Tag::Internal(InternalTag::Patch), got: {tag:?}"
        );
        assert_eq!(tag.to_string(), "__ocx.patch");
    }

    // ── Variant tag parsing tests ─────────────────────────────────

    #[test]
    fn tag_variant_prefixed_version() {
        let tag = Tag::from("debug-3.12".to_string());
        assert!(matches!(tag, Tag::Version(_)));
        if let Tag::Version(v) = &tag {
            assert_eq!(v.variant(), Some("debug"));
            assert_eq!(v.major(), 3);
            assert_eq!(v.minor(), Some(12));
        }
        assert_eq!(tag.to_string(), "debug-3.12");
    }

    #[test]
    fn tag_variant_prefixed_with_build() {
        let tag = Tag::from("pgo.lto-3.12.5_b1".to_string());
        assert!(matches!(tag, Tag::Version(_)));
        if let Tag::Version(v) = &tag {
            assert_eq!(v.variant(), Some("pgo.lto"));
        }
        assert_eq!(tag.to_string(), "pgo.lto-3.12.5_b1");
    }

    #[test]
    fn tag_bare_variant_is_other() {
        for name in ["debug", "pgo.lto", "slim", "canary"] {
            let tag = Tag::from(name.to_string());
            assert!(matches!(tag, Tag::Other(_)), "'{name}' should be Tag::Other");
            assert_eq!(tag.to_string(), name);
        }
    }

    #[test]
    fn tag_custom_tag_still_other() {
        let tag = Tag::from("custom-tag".to_string());
        assert!(matches!(tag, Tag::Other(_)));

        let tag = Tag::from("my-custom-thing".to_string());
        assert!(matches!(tag, Tag::Other(_)));
    }

    #[test]
    fn tag_backward_compat_existing_formats() {
        assert!(matches!(Tag::from("latest".to_string()), Tag::Latest));
        assert!(matches!(Tag::from("3.28.1".to_string()), Tag::Version(_)));
        assert!(matches!(Tag::from("3.28.1-alpha".to_string()), Tag::Version(_)));
        assert!(matches!(Tag::from("3.28.1_b1".to_string()), Tag::Version(_)));
        assert!(matches!(Tag::from("3.28.1-alpha_b1".to_string()), Tag::Version(_)));
        assert!(matches!(
            Tag::from(format!("sha256.{}", hex(64))),
            Tag::Canonical { .. }
        ));
        assert!(matches!(Tag::from("custom-tag".to_string()), Tag::Other(_)));
        assert!(matches!(Tag::from("__ocx.desc".to_string()), Tag::Internal(_)));
    }
}
