// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use crate::{
    oci::{Algorithm, Digest},
    package::version,
};

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
    /// A keep tag naming a platform manifest by its own digest
    /// (`__ocx.keep.<algorithm>-<hex>`), written by `Client::push_keep_tag`.
    ///
    /// It holds the manifest reachable so registry garbage collection — or a
    /// stray delete of a rolling or cascade tag — can never orphan a digest a
    /// lock still pins (`adr_index_indirection.md` Decision E).
    ///
    /// The parts are carried separately rather than as an
    /// [`crate::oci::Digest`] because a tag spells them `<algorithm>-<hex>` —
    /// OCI forbids `:` in a tag, which is the separator `Digest`'s `Display`
    /// emits.
    Keep {
        /// The digest algorithm the tag names.
        algorithm: Algorithm,
        /// The lower- or upper-case hex digest body, verbatim as tagged.
        hex: String,
    },
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

    /// The OCI tag prefix for keep tags. The `<algorithm>-<hex>` digest body
    /// follows it verbatim, so a full keep tag reads
    /// `__ocx.keep.sha256-<64 hex>`.
    pub const KEEP_TAG_PREFIX: &str = "__ocx.keep.";

    fn from_tag(value: &str) -> Self {
        match value {
            Self::DESCRIPTION_TAG => InternalTag::Description,
            Self::PATCH_TAG => InternalTag::Patch,
            // The keep tag is the one parameterized internal tag, so it is
            // matched by prefix-strip rather than by literal — before the
            // `Unknown` fallthrough, which would otherwise swallow it.
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

/// Semantic classification of an OCI tag string.
///
/// Parsed from a raw tag string via `Tag::from(String)`. The parse order is:
/// 1. `"latest"` → [`Latest`](Tag::Latest)
/// 2. The `__ocx` namespace → [`Internal`](Tag::Internal) — this is where the
///    keep tag `__ocx.keep.<algorithm>-<hex>` is classified, at step 2 and so
///    ahead of every digest-shaped arm below; it never reaches
///    [`is_referrer_fallback_tag`]
/// 3. Version-parseable (digit-first or variant-prefixed) → [`Version`](Tag::Version)
/// 4. The frozen legacy keep-tag form (`sha256.<hex>`) → [`LegacyKeep`](Tag::LegacyKeep)
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
    /// The **frozen legacy keep-tag form**: a tag naming a platform manifest by
    /// its own digest, spelled `"sha256.abcdef…"`.
    ///
    /// This is a read arm and nothing else. OCX never writes this form — a keep
    /// tag written today is [`InternalTag::Keep`]
    /// (`__ocx.keep.<algorithm>-<hex>`). The arm stays because already-published
    /// repositories carry these tags, and they must keep classifying as reserved
    /// so they are never read back as a version.
    ///
    /// The parts are carried separately rather than as an [`crate::oci::Digest`]
    /// because a tag spells them `<algorithm>.<hex>` — OCI forbids `:` in a tag,
    /// which is the separator `Digest`'s `Display` emits.
    LegacyKeep {
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

/// Matches a keep-tag digest body `<algorithm><separator><hex>` over every
/// supported algorithm.
///
/// `separator` is the one axis the two keep-tag forms differ on: `'.'` for the
/// frozen legacy form ([`Tag::LegacyKeep`]), `'-'` for the namespaced form
/// ([`InternalTag::Keep`]) — so one hex/length validator serves both. Returns
/// `None` on a wrong separator, a wrong hex length, or a non-hex body.
///
/// Deliberately wider than the `sha256` tags `push_keep_tag` writes today:
/// reserving a name costs nothing, and a `sha384` body would be no more a
/// version than a `sha256` one.
fn parse_keep(value: &str, separator: char) -> Option<(Algorithm, &str)> {
    Algorithm::ALL.iter().find_map(|algorithm| {
        let hex = value.strip_prefix(algorithm.prefix())?.strip_prefix(separator)?;
        (hex.len() == algorithm.hex_len() && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some((*algorithm, hex))
    })
}

/// Length of the encoded section in an OCI Referrers fallback tag.
///
/// The distribution spec truncates it: *"The Truncated Encoded section
/// associated with a Content Digest MUST match the digest's `encoded` section
/// truncated to 64 characters."* For sha256 that is the whole hex body and the
/// truncation is a no-op; for sha384 (96) and sha512 (128) it is not, so two
/// subjects sharing a 64-character prefix share one referrers tag. The spec
/// accepts that collision; this constant is where it comes from.
const REFERRER_FALLBACK_ENCODED_LEN: usize = 64;

/// The OCI Referrers tag-schema fallback tag naming `digest`'s referrers index.
///
/// `<algorithm>-<encoded truncated to 64>` — the one place this tag is spelled.
/// The writer that appends to the index and [`is_referrer_fallback_tag`], which
/// refuses to read the same string back as a package version, both derive from
/// here so the two cannot disagree.
///
/// Not the keep tag: that is `__ocx.keep.<algorithm>-<hex>`, classified in the
/// `__ocx` namespace at step 2 of [`Tag::from`] and deliberately *not* the bare
/// spec-reserved form this returns.
pub fn referrer_fallback_tag(digest: &Digest) -> String {
    let (algorithm, hex) = digest.parts();
    // `hex` is ASCII by construction, so a byte slice would do; `char_indices`
    // keeps it total for a `Digest` built by an in-crate tuple construction
    // that bypassed `TryFrom`'s validation.
    let encoded: String = hex.chars().take(REFERRER_FALLBACK_ENCODED_LEN).collect();
    format!("{algorithm}-{encoded}")
}

/// Suffixes of the three cosign sidecar tags `<algorithm>-<hex>.{sig,att,sbom}`.
///
/// One literal, two readers each: [`is_referrer_fallback_tag`] refuses to read
/// the string back as a package version, and the writer — [`sbom_sidecar_tag`]
/// here, [`SidecarKind::suffix`](crate::oci::verify::simplesigning_read::SidecarKind::suffix)
/// for the other two — asks the registry for it. Spelled once so a change to
/// either side cannot leave the classifier reserving a name the reader no
/// longer asks for, which is exactly the shape of the gap `.sbom` closed: the
/// classifier stripped it and nothing read it.
pub(crate) const SIG_SIDECAR_SUFFIX: &str = ".sig";
pub(crate) const ATT_SIDECAR_SUFFIX: &str = ".att";
pub(crate) const SBOM_SIDECAR_SUFFIX: &str = ".sbom";

/// The cosign `sha256-<hex>.sbom` sidecar tag naming `subject`'s SBOM
/// attachment.
///
/// Derived from [`referrer_fallback_tag`] for the reason
/// `verify::simplesigning_read::sidecar_tag` derives its `.sig` / `.att`
/// siblings from it: the truncated-digest half is spelled in one place, so the
/// three sidecar doors and the fallback-index writer cannot disagree about it.
///
/// Measured against cosign v3.1.1: `cosign attach sbom <ref>` uploads to
/// exactly this tag, and a second attach **replaces** the manifest rather than
/// appending a layer to it.
pub fn sbom_sidecar_tag(subject: &Digest) -> String {
    let mut tag = referrer_fallback_tag(subject);
    tag.push_str(SBOM_SIDECAR_SUFFIX);
    tag
}

/// Matches the OCI Referrers tag-schema fallback shape `<algorithm>-<hex>`
/// and its `cosign` artifact suffixes `<algorithm>-<hex>.sig` / `.att` /
/// `.sbom` — the dash-separated digest tags a registry without native
/// Referrers-API support (or `cosign` in sidecar mode) parks referrers indices
/// and signature/attestation/SBOM manifests under. They name a referrers index
/// or a signature artifact, never a package version — the same rule the frozen
/// legacy keep tag [`parse_keep`] follows, spelled with a dash because that is
/// the tag-schema convention.
///
/// Two encoded lengths match, and the pair is deliberate: 64 is what
/// [`referrer_fallback_tag`] emits for every algorithm, and the algorithm's own
/// `hex_len()` is the untruncated form OCX classified as reserved before the
/// truncation rule was applied. Reserving a name costs nothing, so the set only
/// ever grows — narrowing it would let a tag that *was* refused as a version
/// suddenly be accepted as one.
fn is_referrer_fallback_tag(value: &str) -> bool {
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

impl Tag {
    /// Returns `true` if this tag is not a version pointer: the OCX-internal
    /// namespace (which carries the keep tag), the frozen legacy keep-tag form
    /// naming a platform manifest by its own digest, or an OCI Referrers
    /// fallback / `cosign` signature-artifact tag
    /// ([`is_referrer_fallback_tag`]). None of these may appear as a version
    /// in the index.
    pub fn is_reserved(&self) -> bool {
        match self {
            Tag::Internal(_) | Tag::LegacyKeep { .. } => true,
            Tag::Other(value) => is_referrer_fallback_tag(value),
            Tag::Latest | Tag::Version(_) => false,
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
        } else if let Some((algorithm, hex)) = parse_keep(&value, '.') {
            Tag::LegacyKeep {
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
            Tag::LegacyKeep { algorithm, hex } => format!("{}.{}", algorithm.prefix(), hex),
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

        let legacy_keep_tag = Tag::from(format!("sha256.{}", hex(64)));
        assert!(matches!(legacy_keep_tag, Tag::LegacyKeep { .. }));
        assert_eq!(legacy_keep_tag.to_string(), format!("sha256.{}", hex(64)));

        let keep_tag = Tag::from(format!("__ocx.keep.sha256-{}", hex(64)));
        assert!(matches!(keep_tag, Tag::Internal(InternalTag::Keep { .. })));
        assert_eq!(keep_tag.to_string(), format!("__ocx.keep.sha256-{}", hex(64)));

        let other_tag = Tag::from("custom-tag".to_string());
        assert!(matches!(other_tag, Tag::Other(_)));
        assert_eq!(other_tag.to_string(), "custom-tag");
    }

    // ── Reserved-tag verdict (ADR D7) ─────────────────────────────

    #[test]
    fn tag_classifies_the_sha256_dot_form_as_legacy_keep() {
        let tag = Tag::from(format!("sha256.{}", hex(64)));
        assert!(
            matches!(&tag, Tag::LegacyKeep { algorithm, hex: body }
                if *algorithm == Algorithm::Sha256 && body.len() == 64),
            "got {tag:?}"
        );
        assert!(tag.is_reserved());
    }

    /// The retarget is over `Algorithm::ALL`, not `sha256` alone — and the hex
    /// length is per-algorithm, so a 64-hex `sha384.` tag is not a keep tag.
    #[test]
    fn tag_classifies_every_algorithm_dot_form_as_legacy_keep() {
        for (algorithm, len) in [
            (Algorithm::Sha256, 64usize),
            (Algorithm::Sha384, 96),
            (Algorithm::Sha512, 128),
        ] {
            let raw = format!("{}.{}", algorithm.prefix(), hex(len));
            let tag = Tag::from(raw.clone());
            assert!(
                matches!(&tag, Tag::LegacyKeep { algorithm: a, .. } if *a == algorithm),
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

    // ── Keep tag (`__ocx.keep.<algorithm>-<hex>`) ─────────────────

    /// The written form. It classifies as `Internal(Keep)` — at parse step 2,
    /// inside the `__ocx` namespace — and round-trips verbatim through
    /// `Display`.
    #[test]
    fn tag_classifies_the_namespaced_dash_form_as_internal_keep() {
        for (algorithm, len) in [
            (Algorithm::Sha256, 64usize),
            (Algorithm::Sha384, 96),
            (Algorithm::Sha512, 128),
        ] {
            let raw = format!("__ocx.keep.{}-{}", algorithm.prefix(), hex(len));
            let tag = Tag::from(raw.clone());
            assert!(
                matches!(&tag, Tag::Internal(InternalTag::Keep { algorithm: a, hex: body })
                    if *a == algorithm && body.len() == len),
                "{raw}: {tag:?}"
            );
            assert!(tag.is_reserved(), "'{raw}' should be reserved");
            assert_eq!(tag.to_string(), raw, "Display must round-trip '{raw}'");
        }
    }

    /// A malformed digest body does not make a `Keep`. It stays inside the
    /// `__ocx` namespace, so it lands on `Unknown` (and stays reserved) rather
    /// than escaping to `Other`.
    #[test]
    fn tag_does_not_build_keep_from_a_malformed_digest_body() {
        for raw in [
            format!("__ocx.keep.sha256-{}", hex(63)),        // wrong hex length
            format!("__ocx.keep.sha256-{}", "z".repeat(64)), // non-hex body
            format!("__ocx.keep.sha256.{}", hex(64)),        // wrong separator
            format!("__ocx.keep.sha384-{}", hex(64)),        // hex length of another algorithm
        ] {
            let tag = Tag::from(raw.clone());
            assert!(
                matches!(tag, Tag::Internal(InternalTag::Unknown(_))),
                "'{raw}' must not classify as Keep, got {tag:?}"
            );
            assert_eq!(tag.to_string(), raw);
        }
    }

    // ── Referrer fallback tag verdict ─────────────────────────────

    /// The dash-form `<algorithm>-<hex>` is the OCI Referrers tag-schema
    /// fallback shape. It stays classified `Tag::Other` (unlike the frozen
    /// legacy dot form, it is not `Tag::LegacyKeep`) but must still be
    /// reserved.
    /// The referrers fallback form stays reserved for **every** algorithm,
    /// including an all-digit body.
    ///
    /// This guards an invariant nothing else states. `Tag::from` runs
    /// `Version::parse` at step 3, *ahead* of both digest arms, and
    /// `<prefix>-<digits>` is a shape it will happily try. Today an all-digit
    /// body always loses, but only by arithmetic: every `hex_len()` is 64, 96
    /// or 128, and a number that long overflows the `u32` the version parser
    /// wants. Add a shorter-digest algorithm and `shaN-12345678` would parse as
    /// a version and escape reservation silently — no test would say so.
    #[test]
    fn every_algorithm_dash_form_stays_reserved_with_an_all_digit_body() {
        for algorithm in Algorithm::ALL {
            let tag = format!("{}-{}", algorithm.prefix(), "1".repeat(algorithm.hex_len()));
            assert!(
                Tag::from(tag.clone()).is_reserved(),
                "{tag} is the referrers fallback form and must never read as a version"
            );
        }
    }

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
            format!("sha256-{}", hex(63)), // neither 64 nor sha256's hex_len
            "v1.2.3".to_string(),          // no dash-digest shape at all
        ] {
            let tag = Tag::from(raw.clone());
            assert!(!tag.is_reserved(), "'{raw}' should not be reserved, got {tag:?}");
        }
    }

    /// `.sbom` joins `.sig` and `.att`. Previously the boundary-case test above
    /// asserted the opposite — an `.sbom` sidecar tag read back as an ordinary
    /// tag, and so as a candidate package version.
    #[test]
    fn tag_classifies_dash_form_sbom_suffix_as_reserved() {
        let raw = format!("sha256-{}.sbom", hex(64));
        let tag = Tag::from(raw.clone());
        assert!(matches!(tag, Tag::Other(_)), "got {tag:?}");
        assert!(tag.is_reserved(), "'{raw}' should be reserved");
    }

    /// The tag the SBOM sidecar reader asks for is the exact string this
    /// module refuses to read back as a version.
    ///
    /// The classifier and the reader are the two halves of one contract, and
    /// they were written years apart: the `.sbom` suffix was reserved here
    /// before anything read it. A round trip through both is what stops the
    /// pair drifting — the test above proves the classifier reserves a
    /// hand-spelled `.sbom` tag, and this one proves the reader asks for a tag
    /// of exactly that shape rather than for one the classifier would hand back
    /// as a package version.
    ///
    /// The literal is spelled out rather than derived: deriving it from
    /// `sbom_sidecar_tag` would make the test agree with the writer by
    /// construction, which is the one thing it must not do.
    #[test]
    fn the_sbom_sidecar_tag_is_the_string_the_classifier_reserves() {
        let digest = Digest::Sha256("a".repeat(64));
        let tag = sbom_sidecar_tag(&digest);

        assert_eq!(tag, format!("sha256-{}.sbom", "a".repeat(64)));
        assert!(
            Tag::from(tag.clone()).is_reserved(),
            "the reader must ask for a tag this module refuses as a version, got '{tag}'",
        );
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

    /// The writer and the classifier are one rule. A tag
    /// [`referrer_fallback_tag`] emits that [`Tag::is_reserved`] does not refuse
    /// is a tag OCX writes to a registry and then offers back as a package
    /// version.
    ///
    /// Asserted through `Tag::from(..).is_reserved()`, the public verdict, not
    /// through the private predicate: `is_referrer_fallback_tag` is reachable
    /// only via `Tag::from`'s `Other` arm, after `Version::parse` has declined,
    /// so a change to the version grammar could un-reserve every fallback tag
    /// while a predicate-level assertion stayed green.
    #[test]
    fn every_tag_the_writer_emits_is_reserved() {
        for algorithm in Algorithm::ALL {
            for body in ["a", "1", "0"] {
                let digest = Digest::try_from(format!("{}:{}", algorithm.prefix(), body.repeat(algorithm.hex_len())))
                    .expect("a full-length hex body is a valid digest");
                let tag = referrer_fallback_tag(&digest);
                assert!(
                    Tag::from(tag.clone()).is_reserved(),
                    "{tag} is written by referrer_fallback_tag and must never read back as a version"
                );
            }
        }
    }

    /// The widening only ever adds. The untruncated dash form was reserved
    /// before the truncation rule was applied and stays reserved, so no tag that
    /// was refused as a version becomes acceptable as one.
    #[test]
    fn the_untruncated_dash_form_stays_reserved() {
        for algorithm in Algorithm::ALL {
            let tag = format!("{}-{}", algorithm.prefix(), "a".repeat(algorithm.hex_len()));
            assert!(
                Tag::from(tag.clone()).is_reserved(),
                "{tag} was reserved before the truncation rule and must stay reserved"
            );
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
            (format!("__ocx.keep.sha256-{}", hex(64)), true),
            (format!("__ocx.keep.sha384-{}", hex(96)), true),
            // Malformed keep tags are still reserved — the `__ocx` namespace
            // is what reserves them, not the digest body.
            (format!("__ocx.keep.sha256-{}", hex(63)), true),
            ("__ocx.keep.nonsense".to_string(), true),
            (format!("sha256-{}", hex(64)), true),
            (format!("sha256-{}.sig", hex(64)), true),
            (format!("sha256-{}.att", hex(64)), true),
            (format!("sha256-{}.sbom", hex(64)), true),
            // The spec truncates the encoded section to 64 for every algorithm,
            // so these — not the untruncated forms two lines up — are the tags a
            // registry actually parks a sha384/sha512 referrers index under.
            (format!("sha384-{}", hex(64)), true),
            (format!("sha512-{}", hex(64)), true),
            (format!("sha384-{}", hex(96)), true),
            (format!("sha512-{}", hex(128)), true),
            (format!("sha256-{}", hex(63)), false),
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
            Tag::LegacyKeep { .. }
        ));
        assert!(matches!(Tag::from("custom-tag".to_string()), Tag::Other(_)));
        assert!(matches!(Tag::from("__ocx.desc".to_string()), Tag::Internal(_)));
    }
}
