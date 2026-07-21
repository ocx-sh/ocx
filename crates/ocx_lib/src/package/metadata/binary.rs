// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Publisher-declared, unverified claim of interface-surface executable
//! names a package exposes on `PATH`. See
//! `adr_declared_binaries_metadata.md`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::slug::SLUG_MAX_LEN;
use super::visibility::Visibility;

/// A validated executable-name claim.
///
/// Bare name (never `.exe`), case-preserving, ASCII printable, no
/// whitespace. Looser than [`super::entrypoint::EntrypointName`]'s slug
/// grammar — admits names like `python3.13`, `c++`, `MSBuild`. Enforced at
/// construction and deserialization. Grammar: `adr_declared_binaries_metadata.md` §5.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BinaryName(String);

impl BinaryName {
    /// Maximum byte length of a binary name.
    ///
    /// Mirrors [`SLUG_MAX_LEN`] — the same upper bound as
    /// [`super::entrypoint::EntrypointName`] and
    /// [`super::dependency::DependencyName`], though `BinaryName` does not
    /// reuse the slug character class (see module docs).
    pub const MAX_LEN: usize = SLUG_MAX_LEN;

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for BinaryName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Windows-reserved filename characters, forbidden outright — `ocx`
/// materializes binary names as real files on Windows. `/` and `\` close
/// the npm/pnpm bin-field path-traversal CVE family (ADR §5): no
/// scoped/nested names exist to normalize.
const FORBIDDEN_CHARS: &[char] = &['/', '\\', '<', '>', ':', '"', '|', '?', '*'];

/// Reserved Windows device names, checked case-insensitively against the
/// basename before the first `.` (ADR §5).
const RESERVED_DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT0",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

impl TryFrom<String> for BinaryName {
    type Error = BinaryError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(BinaryError::Empty);
        }
        // Length checked before the character scan to avoid iterating a
        // pathologically long input (mirrors EntrypointName's precedent).
        if value.len() > Self::MAX_LEN {
            return Err(BinaryError::TooLong { name: value });
        }
        if value.chars().any(char::is_whitespace) {
            return Err(BinaryError::Whitespace { name: value });
        }
        // Forbidden-character check runs before the leading/trailing-dot
        // rule below: a `/`-or-`\`-bearing traversal vector (e.g.
        // `../../etc/passwd`) must report InvalidCharacter, not the dot
        // rule, even though it also starts with `.` (ADR §5 correctness
        // anchor).
        if value
            .chars()
            .any(|c| FORBIDDEN_CHARS.contains(&c) || !c.is_ascii_graphic())
        {
            return Err(BinaryError::InvalidCharacter { name: value });
        }
        if value.starts_with('-') {
            return Err(BinaryError::LeadingDash { name: value });
        }
        if value.starts_with('.') || value.ends_with('.') {
            // Closes bare `..` traversal: no `/` present, so this rule (not
            // the forbidden-character rule) is the anchor (ADR §5).
            return Err(BinaryError::LeadingOrTrailingDot { name: value });
        }
        let basename = value.split('.').next().unwrap_or(value.as_str());
        let basename_upper = basename.to_ascii_uppercase();
        if RESERVED_DEVICE_NAMES.contains(&basename_upper.as_str()) {
            return Err(BinaryError::ReservedWindowsDeviceName {
                name: value,
                reserved: basename_upper,
            });
        }
        Ok(BinaryName(value))
    }
}

impl TryFrom<&str> for BinaryName {
    type Error = BinaryError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        BinaryName::try_from(value.to_string())
    }
}

impl std::fmt::Display for BinaryName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::borrow::Borrow<str> for BinaryName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for BinaryName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        BinaryName::try_from(s).map_err(serde::de::Error::custom)
    }
}

/// Sorted, unique collection of [`BinaryName`] claims.
///
/// Write side always serializes as a plain array of bare strings (derived
/// [`Serialize`]). Read side accepts an untagged `string | object` element
/// union via a custom [`Deserialize`] impl (see [`BinaryElement`]), so a
/// reader of this version tolerates a hypothetical future per-binary object
/// shape without hard-failing. See `adr_declared_binaries_metadata.md` §1.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Binaries(BTreeSet<BinaryName>);

impl Binaries {
    /// The visibility a binaries claim carries as a surface carrier.
    ///
    /// Binaries have no publisher-declared visibility field; they name raw
    /// executables in `bin/`, invocable by consumers *and* by the package's
    /// own shims and entry-point launchers (a launcher's target IS one of
    /// these). That is [`Visibility::PUBLIC`]: both axes — so under the
    /// composer's surface algebra a claim crosses wherever its owning node
    /// is admitted, which is the admission rule
    /// `adr_declared_binaries_metadata.md` §4 records.
    pub const IMPLICIT_VISIBILITY: Visibility = Visibility::PUBLIC;

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Iterates declared binary names in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = &BinaryName> + use<'_> {
        self.0.iter()
    }
}

impl TryFrom<BTreeSet<BinaryName>> for Binaries {
    type Error = BinaryError;

    /// Validates case-fold uniqueness across the set: two names differing
    /// only by case (`Cmake` vs `cmake`) collide on a case-insensitive
    /// target filesystem even though they are distinct `BTreeSet` keys.
    /// Exact-duplicate names never reach here — `BTreeSet` insertion already
    /// collapsed them with zero information loss. Shared by the hand-authored
    /// deserialize path (via [`BinaryElement`]) and the create-time scan
    /// path (`bin_scan::scan_interface_binaries`) — one validation function,
    /// two callers, per `adr_declared_binaries_metadata.md` §5 Decision B.
    fn try_from(names: BTreeSet<BinaryName>) -> Result<Self, Self::Error> {
        let mut seen_folds: BTreeMap<String, BinaryName> = BTreeMap::new();
        for name in &names {
            let fold = name.as_str().to_ascii_lowercase();
            if let Some(first) = seen_folds.get(&fold) {
                return Err(BinaryError::CaseFoldCollision {
                    first: first.clone(),
                    second: name.clone(),
                });
            }
            seen_folds.insert(fold, name.clone());
        }
        Ok(Binaries(names))
    }
}

/// Deserialize-only element shape for a `binaries` array entry.
///
/// Accepts either a bare string (the only shape `ocx` ever writes) or an
/// object carrying at least a `name` key — forward-compat with a
/// hypothetical future per-binary object shape; unknown object keys are
/// ignored via the flattened map. Never constructed by the writer — see
/// [`Binaries`]'s derived `Serialize` impl.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BinaryElement {
    Name(String),
    Object {
        name: String,
        #[serde(flatten)]
        _reserved: serde_json::Map<String, serde_json::Value>,
    },
}

impl<'de> Deserialize<'de> for Binaries {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let elements = Vec::<BinaryElement>::deserialize(deserializer)?;
        let names = elements
            .into_iter()
            .map(|element| {
                let raw = match element {
                    BinaryElement::Name(name) => name,
                    BinaryElement::Object { name, .. } => name,
                };
                BinaryName::try_from(raw)
            })
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(serde::de::Error::custom)?;
        Binaries::try_from(names).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for Binaries {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Binaries")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Write contract only: `ocx` always emits a plain array of bare
        // strings. The read-side string|object leniency (`BinaryElement`)
        // is an internal Rust affordance that never appears in the
        // published schema.
        schemars::json_schema!({
            "type": "array",
            "description": "Publisher-declared, unverified claim of interface-surface executable names exposed on PATH by this package. Absent means undeclared; an empty array means the publisher asserts zero interface binaries. On Windows the claim reflects the default executable-resolution set (.exe/.com/.bat/.cmd); a customized child PATHEXT may resolve fewer.",
            "items": { "type": "string" },
            "uniqueItems": true
        })
    }
}

/// Errors validating a [`BinaryName`] or constructing a [`Binaries`]
/// collection.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BinaryError {
    /// Empty name.
    #[error("binary name must not be empty")]
    Empty,
    /// Contains a character outside the allowed ASCII-printable set, or one
    /// of the Windows-reserved filename characters `/ \ < > : " | ? *`.
    #[error("invalid binary name '{name}': contains a disallowed character")]
    InvalidCharacter { name: String },
    /// Contains whitespace anywhere in the name.
    #[error("invalid binary name '{name}': must not contain whitespace")]
    Whitespace { name: String },
    /// Starts with `-` (shell flag-lookalike hazard).
    #[error("invalid binary name '{name}': must not start with '-'")]
    LeadingDash { name: String },
    /// Starts or ends with `.` (Unix hidden-file ambiguity / Windows silent
    /// strip → on-disk collision).
    #[error("invalid binary name '{name}': must not start or end with '.'")]
    LeadingOrTrailingDot { name: String },
    /// Exceeds [`BinaryName::MAX_LEN`] bytes.
    // The literal `64` must stay in sync with `BinaryName::MAX_LEN` (=
    // slug::SLUG_MAX_LEN); serde/thiserror `#[error]` attributes cannot
    // interpolate a const at compile time.
    #[error("invalid binary name '{name}': exceeds the 64 byte limit")]
    TooLong { name: String },
    /// The basename before the first `.` case-insensitively matches a
    /// reserved Windows device name (`CON`, `PRN`, `AUX`, `NUL`, `COM0`–
    /// `COM9`, `LPT0`–`LPT9`).
    #[error("invalid binary name '{name}': '{reserved}' is a reserved Windows device name")]
    ReservedWindowsDeviceName { name: String, reserved: String },
    /// Two declared names collide once case-folded — a hazard on a
    /// case-insensitive target filesystem even though the raw strings
    /// differ.
    #[error("binary names '{first}' and '{second}' collide case-insensitively")]
    CaseFoldCollision { first: BinaryName, second: BinaryName },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::metadata::bundle::Bundle;

    // ── Grammar: accept ──────────────────────────────────────────────────

    #[test]
    fn grammar_accepts_looser_names_than_entrypoint_slug() {
        for name in ["python3.13", "c++", "7z", "clang-format-18", "MSBuild"] {
            assert!(
                BinaryName::try_from(name).is_ok(),
                "'{name}' must be accepted by the BinaryName grammar (research_bin_name_conventions.md)"
            );
        }
    }

    // ── Grammar: reject, with the specific BinaryError variant ──────────
    //
    // ADR §5 security note: `/` and `\` are forbidden outright (no
    // scoped/nested names to normalize), and leading/trailing-`.` rejection
    // kills bare `..` — the two structural closures for the npm/pnpm
    // bin-field path-traversal CVE family. Each vector below asserts the
    // SPECIFIC violation that closes it, not just `is_err()`.

    #[test]
    fn grammar_rejects_leading_dot() {
        let err = BinaryName::try_from(".hidden").unwrap_err();
        assert!(
            matches!(err, BinaryError::LeadingOrTrailingDot { .. }),
            "unexpected: {err}"
        );
    }

    #[test]
    fn grammar_rejects_leading_dash() {
        let err = BinaryName::try_from("-flag").unwrap_err();
        assert!(matches!(err, BinaryError::LeadingDash { .. }), "unexpected: {err}");
    }

    #[test]
    fn grammar_rejects_trailing_dot() {
        let err = BinaryName::try_from("foo.").unwrap_err();
        assert!(
            matches!(err, BinaryError::LeadingOrTrailingDot { .. }),
            "unexpected: {err}"
        );
    }

    #[test]
    fn grammar_rejects_reserved_device_name_bare() {
        let err = BinaryName::try_from("con").unwrap_err();
        assert!(
            matches!(err, BinaryError::ReservedWindowsDeviceName { .. }),
            "case-insensitive bare device name must be rejected; unexpected: {err}"
        );
    }

    #[test]
    fn grammar_rejects_reserved_device_name_uppercase() {
        let err = BinaryName::try_from("COM7").unwrap_err();
        assert!(
            matches!(err, BinaryError::ReservedWindowsDeviceName { .. }),
            "unexpected: {err}"
        );
    }

    #[test]
    fn grammar_rejects_reserved_device_name_with_suffix() {
        // Windows reserves a device name with ANY extension — the basename
        // before the first `.` is what's checked (ADR §5, Codex gate finding).
        for name in ["CON.txt", "LPT1.log"] {
            let err = BinaryName::try_from(name).unwrap_err();
            assert!(
                matches!(err, BinaryError::ReservedWindowsDeviceName { .. }),
                "'{name}' must reject on the basename before the first dot; unexpected: {err}"
            );
        }
    }

    #[test]
    fn grammar_rejects_forbidden_character_slash() {
        let err = BinaryName::try_from("a/b").unwrap_err();
        assert!(matches!(err, BinaryError::InvalidCharacter { .. }), "unexpected: {err}");
    }

    #[test]
    fn grammar_rejects_whitespace() {
        let err = BinaryName::try_from("foo bar").unwrap_err();
        assert!(matches!(err, BinaryError::Whitespace { .. }), "unexpected: {err}");
    }

    #[test]
    fn grammar_rejects_over_max_length() {
        let too_long = "a".repeat(BinaryName::MAX_LEN + 1);
        let err = BinaryName::try_from(too_long.as_str()).unwrap_err();
        assert!(matches!(err, BinaryError::TooLong { .. }), "unexpected: {err}");
    }

    #[test]
    fn too_long_error_message_cites_max_len_constant() {
        // The #[error(...)] string on TooLong hardcodes "64" today. This test
        // pins the rendered message to BinaryName::MAX_LEN itself, so a
        // future change to MAX_LEN that leaves the string un-synced fails
        // loud here instead of drifting silently.
        let too_long = "a".repeat(BinaryName::MAX_LEN + 1);
        let err = BinaryName::try_from(too_long.as_str()).unwrap_err();
        let rendered = err.to_string();
        assert!(
            rendered.contains(&BinaryName::MAX_LEN.to_string()),
            "TooLong message must cite BinaryName::MAX_LEN ({}), got: {rendered}",
            BinaryName::MAX_LEN
        );
    }

    #[test]
    fn grammar_rejects_non_ascii_character() {
        let err = BinaryName::try_from("café").unwrap_err();
        assert!(matches!(err, BinaryError::InvalidCharacter { .. }), "unexpected: {err}");
    }

    #[test]
    fn grammar_rejects_non_graphic_ascii_character() {
        let err = BinaryName::try_from("a\u{7f}").unwrap_err();
        assert!(matches!(err, BinaryError::InvalidCharacter { .. }), "unexpected: {err}");
    }

    #[test]
    fn grammar_rejects_empty() {
        let err = BinaryName::try_from("").unwrap_err();
        assert!(matches!(err, BinaryError::Empty), "unexpected: {err}");
    }

    // ── Grammar: traversal vectors (explicit npm/pnpm CVE-family closure) ─

    #[test]
    fn grammar_rejects_bare_parent_traversal_via_leading_dot() {
        // ADR §5: "leading/trailing-dot rejection kills bare `..`" — no `/`
        // present, so the closure is the dot rule, not the char-forbid rule.
        let err = BinaryName::try_from("..").unwrap_err();
        assert!(
            matches!(err, BinaryError::LeadingOrTrailingDot { .. }),
            "unexpected: {err}"
        );
    }

    #[test]
    fn grammar_rejects_nested_parent_traversal_via_forbidden_slash() {
        let err = BinaryName::try_from("../../etc/passwd").unwrap_err();
        assert!(matches!(err, BinaryError::InvalidCharacter { .. }), "unexpected: {err}");
    }

    #[test]
    fn grammar_rejects_scoped_traversal_via_forbidden_slash() {
        let err = BinaryName::try_from("@scope/../x").unwrap_err();
        assert!(matches!(err, BinaryError::InvalidCharacter { .. }), "unexpected: {err}");
    }

    #[test]
    fn grammar_rejects_backslash_traversal_via_forbidden_character() {
        let err = BinaryName::try_from("\\evil").unwrap_err();
        assert!(matches!(err, BinaryError::InvalidCharacter { .. }), "unexpected: {err}");
    }

    // ── Case-fold collision (Decision B) ─────────────────────────────────

    #[test]
    fn case_fold_collision_rejected_at_construction() {
        let names: BTreeSet<BinaryName> = ["Cmake", "cmake"]
            .into_iter()
            .map(|s| BinaryName::try_from(s).unwrap())
            .collect();
        let err = Binaries::try_from(names).unwrap_err();
        assert!(
            matches!(err, BinaryError::CaseFoldCollision { .. }),
            "two names differing only by case-fold must be rejected; unexpected: {err}"
        );
    }

    #[test]
    fn exact_duplicate_names_collapse_without_error() {
        // A JSON array with an exact-duplicate string collapses via ordinary
        // BTreeSet Ord/Eq semantics before Binaries::try_from ever sees a
        // collision — zero information loss, per ADR §5 Decision B.
        let binaries: Binaries = serde_json::from_str(r#"["cmake","cmake"]"#).expect("exact duplicates parse");
        assert_eq!(binaries.len(), 1);
    }

    // ── Serde: plain string array round-trip ─────────────────────────────

    #[test]
    fn serde_roundtrips_plain_string_array_sorted() {
        let json = r#"["ctest","cmake"]"#;
        let binaries: Binaries = serde_json::from_str(json).expect("plain string array parses");
        let names: Vec<&str> = binaries.iter().map(BinaryName::as_str).collect();
        assert_eq!(names, vec!["cmake", "ctest"], "Binaries is a sorted BTreeSet");

        let reserialized = serde_json::to_string(&binaries).expect("serializes");
        assert_eq!(
            reserialized, r#"["cmake","ctest"]"#,
            "the write side always emits bare strings, sorted"
        );
    }

    // ── Serde: object-element read (forward-compat) ──────────────────────

    #[test]
    fn object_element_extracts_name_and_ignores_unknown_keys() {
        let json = r#"[{"name":"uv","x":1}]"#;
        let binaries: Binaries = serde_json::from_str(json).expect("object element with unknown keys parses");
        let names: Vec<&str> = binaries.iter().map(BinaryName::as_str).collect();
        assert_eq!(names, vec!["uv"]);
    }

    // ── Serde: typo object key — generic serde error (pinned, not friendly) ─

    #[test]
    fn typo_object_key_produces_generic_untagged_variant_error() {
        // Neither the Name(String) nor the Object{name,..} arm matches an
        // object missing `name` — serde's generic "did not match any
        // variant" message surfaces. Pinned as chosen behavior (plan Notes:
        // "Untagged-union diagnostics (deferred, test-only)"), not a
        // regression to fix.
        let err = serde_json::from_str::<Binaries>(r#"[{"nam":"cmake"}]"#).unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    // ── None vs Some(empty) wire distinction (Bundle.binaries) ───────────

    #[test]
    fn bundle_without_binaries_key_deserializes_to_none_and_omits_key_on_serialize() {
        let json = r#"{"version":1}"#;
        let bundle: Bundle = serde_json::from_str(json).expect("bundle without binaries parses");
        assert!(bundle.binaries().is_none(), "absent key must deserialize to None");

        let serialized = serde_json::to_string(&bundle).expect("serializes");
        assert!(
            !serialized.contains("binaries"),
            "None must be omitted entirely from the wire form: {serialized}"
        );
    }

    #[test]
    fn bundle_with_empty_binaries_array_deserializes_to_some_empty_and_roundtrips_with_key() {
        let json = r#"{"version":1,"binaries":[]}"#;
        let bundle: Bundle = serde_json::from_str(json).expect("bundle with an empty binaries array parses");
        let binaries = bundle
            .binaries()
            .expect("an explicit empty array must deserialize to Some(empty), not None");
        assert!(binaries.is_empty());

        let serialized = serde_json::to_string(&bundle).expect("serializes");
        assert!(
            serialized.contains(r#""binaries":[]"#),
            "Some(empty) must round-trip with the key present: {serialized}"
        );
    }
}
