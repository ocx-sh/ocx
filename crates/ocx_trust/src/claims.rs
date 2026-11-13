// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fulcio certificate claim keys: the names and raw OIDs a `claims` table may use.
//!
//! Values and OIDs follow `research_fulcio_claim_oids.md`.

/// How a claim extension's value is encoded in the certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimEncoding {
    /// The extension value is the string itself (`.1.1`-`.1.6`).
    Raw,
    /// The extension value is a minimal DER UTF8String (`.1.8` and up).
    Der,
}

/// A resolved claim key: which extension to read and how to decode it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimSelector {
    /// The extension OID, dotted.
    pub oid: String,
    /// How the extension value decodes.
    pub encoding: ClaimEncoding,
}

/// Why a claim key does not name a matchable Fulcio extension.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaimKeyError {
    /// Not a known claim name and not a dotted OID.
    #[error("unknown claim name `{0}`")]
    UnknownName(String),
    /// A dotted key outside the Fulcio arc, or not a canonical arc number.
    #[error("claim OID `{0}` is outside the Fulcio arc 1.3.6.1.4.1.57264.1.")]
    OutsideFulcioArc(String),
    /// A key inside the Fulcio arc that is not a certificate claim (`.1.7`, the SAN OtherName).
    #[error("claim OID `{0}` is not a certificate claim")]
    NotAClaim(String),
}

/// Resolve a claim key (a snake_case Fulcio name or a raw OID) to its selector.
///
/// # Errors
/// [`ClaimKeyError`] naming why the key matches no extension.
pub(crate) fn resolve_claim_key(key: &str) -> Result<ClaimSelector, ClaimKeyError> {
    const ARC: &str = "1.3.6.1.4.1.57264.1";

    if let Some(number) = NAMES.iter().find_map(|(name, n)| (*name == key).then_some(*n)) {
        return Ok(ClaimSelector {
            oid: format!("{ARC}.{number}"),
            encoding: ClaimEncoding::Der,
        });
    }
    if !key.contains('.') {
        return Err(ClaimKeyError::UnknownName(key.to_owned()));
    }
    let outside = || ClaimKeyError::OutsideFulcioArc(key.to_owned());
    let tail = key
        .strip_prefix(ARC)
        .and_then(|rest| rest.strip_prefix('.'))
        .ok_or_else(outside)?;
    // Canonical arc only: digits, no leading zero, fits u32; a sub-arc (`8.1`) fails the digit check.
    if !tail.bytes().all(|byte| byte.is_ascii_digit()) || tail.starts_with('0') {
        return Err(outside());
    }
    match tail.parse::<u32>().map_err(|_| outside())? {
        7 => Err(ClaimKeyError::NotAClaim(key.to_owned())),
        number if number < 7 => Ok(ClaimSelector {
            oid: key.to_owned(),
            encoding: ClaimEncoding::Raw,
        }),
        _ => Ok(ClaimSelector {
            oid: key.to_owned(),
            encoding: ClaimEncoding::Der,
        }),
    }
}

/// Fulcio's name table: the last arc of each named extension (`.1.8`-`.1.24`, `.1.7` excluded).
const NAMES: [(&str, u32); 17] = [
    ("issuer", 8),
    ("build_signer_uri", 9),
    ("build_signer_digest", 10),
    ("runner_environment", 11),
    ("source_repository_uri", 12),
    ("source_repository_digest", 13),
    ("source_repository_ref", 14),
    ("source_repository_identifier", 15),
    ("source_repository_owner_uri", 16),
    ("source_repository_owner_identifier", 17),
    ("build_config_uri", 18),
    ("build_config_digest", 19),
    ("build_trigger", 20),
    ("run_invocation_uri", 21),
    ("source_repository_visibility_at_signing", 22),
    ("deployment_environment", 23),
    ("token_subject", 24),
];

/// A claim ready to match against a leaf certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledClaim {
    /// The extension to read.
    pub selector: ClaimSelector,
    /// The byte-exact expected value.
    pub expected: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARC: &str = "1.3.6.1.4.1.57264.1";

    fn selector(key: &str) -> ClaimSelector {
        resolve_claim_key(key).unwrap_or_else(|error| panic!("`{key}` must resolve: {error}"))
    }

    /// every name of Fulcio's table `.1.8`-`.1.24` resolves to its OID, DER-encoded.
    #[test]
    fn c006_every_table_name_resolves_to_its_oid() {
        let table = [
            ("issuer", 8),
            ("build_signer_uri", 9),
            ("build_signer_digest", 10),
            ("runner_environment", 11),
            ("source_repository_uri", 12),
            ("source_repository_digest", 13),
            ("source_repository_ref", 14),
            ("source_repository_identifier", 15),
            ("source_repository_owner_uri", 16),
            ("source_repository_owner_identifier", 17),
            ("build_config_uri", 18),
            ("build_config_digest", 19),
            ("build_trigger", 20),
            ("run_invocation_uri", 21),
            ("source_repository_visibility_at_signing", 22),
            ("deployment_environment", 23),
            ("token_subject", 24),
        ];
        for (name, n) in table {
            assert_eq!(
                selector(name),
                ClaimSelector {
                    oid: format!("{ARC}.{n}"),
                    encoding: ClaimEncoding::Der
                },
                "{name}"
            );
        }
    }

    /// raw OID keys dispatch on N: raw up to 6, `.7` is not a claim, DER from 8, also above the table.
    #[test]
    fn c006_raw_oid_keys_dispatch_on_the_arc_number() {
        for (n, encoding) in [
            (1, ClaimEncoding::Raw),
            (6, ClaimEncoding::Raw),
            (8, ClaimEncoding::Der),
            (24, ClaimEncoding::Der),
            (99, ClaimEncoding::Der),
            (4_294_967_295_u32, ClaimEncoding::Der),
        ] {
            let key = format!("{ARC}.{n}");
            assert_eq!(selector(&key), ClaimSelector { oid: key, encoding }, "N = {n}");
        }
        let seven = format!("{ARC}.7");
        assert_eq!(resolve_claim_key(&seven), Err(ClaimKeyError::NotAClaim(seven)));
    }

    /// foreign arcs, leading zeros and sub-arcs are `OutsideFulcioArc`; other strings are `UnknownName`.
    #[test]
    fn c006_non_canonical_and_foreign_keys_are_refused() {
        for key in [
            "1.2.840.113549.1.1.1",
            "1.3.6.1.4.1.57264.1.08",
            "1.3.6.1.4.1.57264.1.8.1",
            "1.3.6.1.4.1.57264.1.0",
            "1.3.6.1.4.1.57264.1.4294967296",
            "1.3.6.1.4.1.57264.1.",
            "1.3.6.1.4.1.57264.1.+8",
            " 1.3.6.1.4.1.57264.1.8",
            "1.3.6.1.4.1.57264.1.8 ",
            "1.3.6.1.4.1.57264.2.8",
        ] {
            assert_eq!(
                resolve_claim_key(key),
                Err(ClaimKeyError::OutsideFulcioArc(key.to_owned())),
                "{key}"
            );
        }
        assert_eq!(
            resolve_claim_key("not_a_claim"),
            Err(ClaimKeyError::UnknownName("not_a_claim".to_owned()))
        );
    }
}
