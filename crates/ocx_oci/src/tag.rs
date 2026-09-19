// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The OCI-side tag vocabulary: the names a registry holds that are not
//! package versions.
//!
//! Three conventions live here, and none of them is OCX's package grammar:
//! the OCX-internal `__ocx` namespace (description, patch and keep tags), the
//! frozen legacy keep-tag form `<algorithm>.<hex>`, and the OCI Referrers
//! tag-schema fallback `<algorithm>-<hex>` together with cosign's `.sig` /
//! `.att` / `.sbom` sidecar suffixes. Every one of them is a registry-wire
//! spelling, which is why they sit beside the client that writes and reads
//! them rather than beside `ocx_package::tag::Tag` (still in `ocx_lib`),
//! whose job is to decide what a tag *means* to the package layer.
//!
//! [`is_reserved_tag`] is the verdict this module answers on its own terms —
//! version-free, so nothing here has to know what a version looks like.
//! `Tag::is_reserved_str` delegates to it, and
//! `crates/ocx_package/tests/tag_verdicts.rs` pins the two to the same answer over
//! every vendored fixture string. That equivalence is the whole reason the
//! split is safe: which tags are reserved is wire-visible (a reserved tag is
//! never offered as a package version), so the rule may move but may not
//! change.

use super::{Algorithm, Digest};

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
    /// [`crate::Digest`] because a tag spells them `<algorithm>-<hex>` —
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
    /// It sits in the `__ocx` namespace, so
    /// `ocx_package::tag::Tag::is_reserved` (still in `ocx_lib`) returns
    /// `true` for it and it is excluded from user-facing tag listings without
    /// any additional filtering.
    pub const PATCH_TAG: &str = "__ocx.patch";

    /// The OCI tag prefix for keep tags. The `<algorithm>-<hex>` digest body
    /// follows it verbatim, so a full keep tag reads
    /// `__ocx.keep.sha256-<64 hex>`.
    pub const KEEP_TAG_PREFIX: &str = "__ocx.keep.";

    /// Classify an internal tag string. The caller has already established
    /// that it sits in the `__ocx` namespace ([`is_internal_namespace`]).
    pub fn from_tag(value: &str) -> Self {
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

/// Whether `tag` names the OCX-internal `__ocx` namespace. Case-insensitive and
/// prefix-based, so `__ocx`, `__ocxfoo` and `__OCX.desc` all match.
pub fn is_internal_namespace(tag: &str) -> bool {
    tag.get(..RESERVED_INTERNAL_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RESERVED_INTERNAL_PREFIX))
}

/// Matches a keep-tag digest body `<algorithm><separator><hex>` over every
/// supported algorithm.
///
/// `separator` is the one axis the two keep-tag forms differ on: `'.'` for the
/// frozen legacy form (`ocx_package::tag::Tag::LegacyKeep`, still in
/// `ocx_lib`), `'-'` for the namespaced form ([`InternalTag::Keep`]) — so one hex/length
/// validator serves both. Returns `None` on a wrong separator, a wrong hex
/// length, or a non-hex body.
///
/// Deliberately wider than the `sha256` tags `push_keep_tag` writes today:
/// reserving a name costs nothing, and a `sha384` body would be no more a
/// version than a `sha256` one.
pub fn parse_keep(value: &str, separator: char) -> Option<(Algorithm, &str)> {
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
/// The writer that appends to the index and `is_referrer_fallback_tag`, which
/// refuses to read the same string back as a package version, both derive from
/// here so the two cannot disagree.
///
/// Not the keep tag: that is `__ocx.keep.<algorithm>-<hex>`, classified in the
/// `__ocx` namespace at step 2 of `ocx_package::tag::Tag::from` (still
/// in `ocx_lib`) and deliberately *not* the bare spec-reserved form this
/// returns.
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
/// here, `ocx_lib::oci::verify::simplesigning_read::SidecarKind::suffix` (still
/// in `ocx_lib`) for the other two — asks the registry for it. Spelled once so a change to
/// either side cannot leave the classifier reserving a name the reader no
/// longer asks for, which is exactly the shape of the gap `.sbom` closed: the
/// classifier stripped it and nothing read it.
pub const SIG_SIDECAR_SUFFIX: &str = ".sig";
pub const ATT_SIDECAR_SUFFIX: &str = ".att";
pub(crate) const SBOM_SIDECAR_SUFFIX: &str = ".sbom";

/// Every cosign sidecar suffix, for a caller that sweeps all three.
///
/// A bare suffix list and deliberately **not** a fourth
/// `ocx_lib::oci::verify::simplesigning_read::SidecarKind` variant:
/// that enum names the doors a *reader* reaches, and re-adding `.sbom` to it to
/// serve a copy-side consumer would make a documented reader gap look covered —
/// the exact shape the variant was deleted for. Iterating suffixes carries no
/// such claim.
///
/// Ordered `.sig`, `.att`, `.sbom` so a sweep's request order is fixed rather
/// than incidental.
pub(crate) const SIDECAR_SUFFIXES: [&str; 3] = [SIG_SIDECAR_SUFFIX, ATT_SIDECAR_SUFFIX, SBOM_SIDECAR_SUFFIX];

/// `<algorithm>-<hex><suffix>` — the cosign sidecar tag naming an attachment of
/// `subject`.
///
/// The one place the sidecar tag shape is spelled. Both typed doors delegate
/// here — [`sbom_sidecar_tag`] and
/// `ocx_lib::oci::verify::simplesigning_read::sidecar_tag` (still in `ocx_lib`) —
/// so the truncated-digest half cannot drift between the three suffixes, and neither
/// can drift from [`referrer_fallback_tag`], which it is derived from.
pub fn sidecar_tag(subject: &Digest, suffix: &str) -> String {
    let mut tag = referrer_fallback_tag(subject);
    tag.push_str(suffix);
    tag
}

/// The cosign `sha256-<hex>.sbom` sidecar tag naming `subject`'s SBOM
/// attachment.
///
/// Derived from [`referrer_fallback_tag`] for the reason
/// `ocx_lib::oci::verify::simplesigning_read::sidecar_tag` derives its `.sig` / `.att`
/// siblings from it: the truncated-digest half is spelled in one place, so the
/// three sidecar doors and the fallback-index writer cannot disagree about it.
///
/// Measured against cosign v3.1.1: `cosign attach sbom <ref>` uploads to
/// exactly this tag, and a second attach **replaces** the manifest rather than
/// appending a layer to it.
pub fn sbom_sidecar_tag(subject: &Digest) -> String {
    sidecar_tag(subject, SBOM_SIDECAR_SUFFIX)
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

/// Whether `tag` names something the registry holds that is not a package
/// version: the OCX-internal namespace (which carries the keep tag), the frozen
/// legacy keep-tag form, or an OCI Referrers fallback / cosign
/// signature-artifact tag.
///
/// **Version-free by construction.** It never asks whether the string parses as
/// a version, because every shape it recognises is one a version can never
/// legally take — and because `oci` must not know the package layer's grammar.
/// `ocx_package::tag::Tag::is_reserved_str` (still in `ocx_lib`)
/// delegates here.
///
/// The equivalence with the parsed verdict is not self-evident and is therefore
/// pinned, not assumed: `Tag::from` runs the version parser at step 3, *ahead*
/// of both digest arms, so a version grammar that ever accepted a
/// `<algorithm>-<hex>` or `<algorithm>.<hex>` string would make the two answers
/// differ. `crates/ocx_package/tests/tag_verdicts.rs` asserts both halves — equality
/// over every vendored fixture string, and that no `parse_keep`/fallback form
/// parses as a version.
///
/// `"latest"` needs no clause of its own, and deliberately has none: it is not
/// in the `__ocx` namespace, is not `<algorithm>.<hex>`, and is not
/// `<algorithm>-<hex>`, so all three predicates already decline it. Spelling the
/// literal here would put a second copy of
/// `ocx_package::tag::LATEST_STR` (still in `ocx_lib`) in the OCI tag
/// vocabulary for no verdict it changes — pinned by
/// `latest_needs_no_special_case`.
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
    /// `Tag::is_reserved` lives in `tests/tag_verdicts.rs` (D-020); this table
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
