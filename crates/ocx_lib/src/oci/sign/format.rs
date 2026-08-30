// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The signature wire-shape vocabulary shared by sign, attest and verify.

use serde::{Deserialize, Serialize};

/// Which cosign wire shape a signature is written in, and which shape a verify
/// pins.
///
/// Format and key model are **orthogonal**: either format can be produced
/// keyless or with a key pair, so this enum never says anything about how the
/// signing material was obtained.
///
/// One vocabulary, two channels — serde for config and JSON, `ValueEnum` for
/// the command line. The two spellings are identical by contract, and
/// `signature_format_slugs_are_frozen` asserts that rather than assuming it:
/// the enum is hand-written on the clap side (`ocx_lib` carries
/// `clap_builder`, not the `clap` derive), so nothing makes the two agree
/// automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureFormat {
    /// An OCI 1.1 referrer carrying a Sigstore bundle v0.3. The default, and
    /// the only shape `cosign sign` v3 produces against a registry that
    /// implements the Referrers API.
    #[default]
    Bundle,
    /// The cosign `sha256-<hex>.sig` sidecar tag, whose layers carry
    /// simplesigning payloads.
    Simplesigning,
    /// Write both shapes.
    ///
    /// **Write-side only.** A verify pins exactly one shape, because "either
    /// of two signatures satisfied me" is not a statement a verification
    /// result can carry; the read-side resolver refuses this value.
    Both,
}

impl SignatureFormat {
    /// Whether this selection writes the OCI 1.1 + Sigstore-bundle shape.
    ///
    /// Added by loop C: the write path branches on the selection twice, and two
    /// `matches!` arms spelled at the call sites is how `Both` gets forgotten in
    /// one of them.
    #[must_use]
    pub const fn writes_bundle(self) -> bool {
        matches!(self, Self::Bundle | Self::Both)
    }

    /// Whether this selection writes the cosign `sha256-<hex>.sig` sidecar.
    #[must_use]
    pub const fn writes_simplesigning(self) -> bool {
        matches!(self, Self::Simplesigning | Self::Both)
    }

    /// Every variant, in declaration order — the order clap renders in help
    /// and the order the frozen-slug test walks.
    pub const ALL: &'static [Self] = &[Self::Bundle, Self::Simplesigning, Self::Both];

    /// The frozen wire slug, identical on the serde and clap channels.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bundle => "bundle",
            Self::Simplesigning => "simplesigning",
            Self::Both => "both",
        }
    }
}

impl std::fmt::Display for SignatureFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl clap_builder::ValueEnum for SignatureFormat {
    fn value_variants<'a>() -> &'a [Self] {
        Self::ALL
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        use clap_builder::builder::PossibleValue;

        Some(match self {
            Self::Bundle => PossibleValue::new("bundle"),
            Self::Simplesigning => PossibleValue::new("simplesigning"),
            Self::Both => PossibleValue::new("both"),
        })
    }
}

#[cfg(test)]
mod tests {
    use clap_builder::ValueEnum as _;

    use super::*;

    /// T-07. Pins the three slugs on **both** channels and pins the two
    /// channels to each other, so a rename reds whichever half moved: the
    /// literal catches a matched pair of edits, and the cross-channel
    /// equality catches a single-channel edit that the literals alone would
    /// let through.
    #[test]
    fn signature_format_slugs_are_frozen() {
        let frozen = [
            (SignatureFormat::Bundle, "bundle"),
            (SignatureFormat::Simplesigning, "simplesigning"),
            (SignatureFormat::Both, "both"),
        ];

        assert_eq!(
            SignatureFormat::ALL.len(),
            frozen.len(),
            "a variant was added or removed"
        );

        for (format, slug) in frozen {
            let serde_slug = serde_json::to_value(format).expect("SignatureFormat serializes");
            assert_eq!(
                serde_slug,
                serde_json::Value::String(slug.to_owned()),
                "serde slug moved"
            );

            let clap_name = format
                .to_possible_value()
                .expect("every variant is selectable on the command line");
            assert_eq!(clap_name.get_name(), slug, "clap value name moved");

            // The two channels must not be able to drift apart.
            assert_eq!(
                serde_slug.as_str(),
                Some(clap_name.get_name()),
                "serde and clap disagree about {format:?}"
            );

            let parsed: SignatureFormat = serde_json::from_value(serde_slug).expect("slug parses back");
            assert_eq!(parsed, format);
        }

        // Documented default: an unspecified format writes a bundle referrer.
        assert_eq!(SignatureFormat::default(), SignatureFormat::Bundle);
    }
}
