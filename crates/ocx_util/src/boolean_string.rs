// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! ocx's one boolean vocabulary, and the refusal it raises.
//!
//! `utility` becomes `ocx_util`, the bottom tier — so this module carries
//! neither a clap dependency (plan DEC-2: a CLI-parsing crate has no business
//! in a domain-free primitives crate) nor a reach back up to
//! `config::Error` for its refusal (plan C-042).

/// Every spelling `BooleanString::try_from` offers when it refuses one.
///
/// Byte-for-byte the string the deleted `clap_builder::ValueEnum` impl
/// rendered from `value_variants()` — which listed **eight** of the ten
/// variants: `on` and `off` parse but were never advertised, and this const
/// preserves that asymmetry rather than quietly fixing it, because the text
/// is what a user reads when they mistype a boolean in `ocx.toml`.
pub(crate) const POSSIBLE: &str = "1, y, yes, true, 0, n, no, false";

/// A value that is not one of ocx's boolean spellings.
///
/// The `Display` text is a byte-for-byte copy of the deleted
/// `config::Error::InvalidBooleanString` literal (plan C-042, spec D-012), and
/// exit-code classification maps it to the same `DataError` that variant took
/// (`ocx_cli::exit::ocx_util`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid boolean string '{value}', possible values are: {possible}")]
pub struct BooleanStringError {
    /// The value that failed to parse, as the user spelled it.
    pub value: String,
    /// The advertised spellings — always this crate's one list of them.
    ///
    /// Not linked: the list is `pub(crate)` (DEC-40, it has no consumer
    /// outside `ocx_util`), so an intra-doc link to it would resolve for
    /// nobody reading this field from outside.
    pub possible: String,
}

#[derive(Debug, Clone, Copy)]
pub enum BooleanString {
    True1,
    TrueY,
    TrueYes,
    TrueOn,
    TrueTrue,
    False0,
    FalseN,
    FalseNo,
    FalseOff,
    FalseFalse,
}

impl From<BooleanString> for bool {
    fn from(val: BooleanString) -> Self {
        match val {
            BooleanString::True1
            | BooleanString::TrueY
            | BooleanString::TrueYes
            | BooleanString::TrueOn
            | BooleanString::TrueTrue => true,
            BooleanString::False0
            | BooleanString::FalseN
            | BooleanString::FalseNo
            | BooleanString::FalseOff
            | BooleanString::FalseFalse => false,
        }
    }
}

impl std::str::FromStr for BooleanString {
    type Err = BooleanStringError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value)
    }
}

impl TryFrom<&str> for BooleanString {
    type Error = BooleanStringError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.to_lowercase().as_str() {
            "1" => Ok(Self::True1),
            "y" => Ok(Self::TrueY),
            "yes" => Ok(Self::TrueYes),
            "on" => Ok(Self::TrueOn),
            "true" => Ok(Self::TrueTrue),
            "0" => Ok(Self::False0),
            "n" => Ok(Self::FalseN),
            "no" => Ok(Self::FalseNo),
            "off" => Ok(Self::FalseOff),
            "false" => Ok(Self::FalseFalse),
            _ => Err(BooleanStringError {
                value: value.to_string(),
                possible: POSSIBLE.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DEC-2: the const is the exact string `value_variants()` rendered
    /// before the `ValueEnum` impl was deleted — eight spellings, in variant
    /// order, `", "`-joined, with `on`/`off` absent.
    ///
    /// A literal, not a re-derivation: rebuilding the list from the variants
    /// would assert that the list equals itself, and would have silently
    /// *added* `on`/`off` — changing what a user reads on a typo.
    #[test]
    fn possible_is_the_string_value_variants_rendered() {
        assert_eq!(POSSIBLE, "1, y, yes, true, 0, n, no, false");
    }

    /// C-042 / D-012: the refusal's `Display` is byte-for-byte the deleted
    /// `config::Error::InvalidBooleanString` message. This is the line a user
    /// sees when they mistype a boolean in `ocx.toml` or an `OCX_*` variable.
    #[test]
    fn boolean_string_error_display_is_the_moved_literal() {
        let error = BooleanString::try_from("maybe").expect_err("`maybe` is not a boolean spelling");
        assert_eq!(
            error.to_string(),
            "invalid boolean string 'maybe', possible values are: 1, y, yes, true, 0, n, no, false"
        );
    }

    /// The value is echoed as the user spelled it, not case-folded — the fold
    /// happens on the match arm only, so `TRUE` parses while `Maybe` is
    /// reported back with its capital.
    #[test]
    fn the_refusal_echoes_the_value_as_spelled() {
        let error = BooleanString::try_from("Maybe").expect_err("`Maybe` is not a boolean spelling");
        assert_eq!(error.value, "Maybe");
        assert_eq!(error.possible, POSSIBLE);
        assert!(bool::from(
            BooleanString::try_from("TRUE").expect("case is folded on the accepting side")
        ));
    }

    /// DEC-2: `FromStr` is the half of the deleted `ValueEnum` impl that was
    /// kept — `.parse()` answers exactly what `try_from` answers, refusal
    /// included.
    #[test]
    fn from_str_agrees_with_try_from() {
        assert!(bool::from("yes".parse::<BooleanString>().expect("`yes` parses")));
        assert_eq!(
            "maybe".parse::<BooleanString>().expect_err("`maybe` does not parse"),
            BooleanString::try_from("maybe").expect_err("`maybe` does not parse")
        );
    }

    /// All ten spellings parse, including the two `POSSIBLE` does not
    /// advertise — the asymmetry the const deliberately preserves.
    #[test]
    fn every_spelling_parses_including_the_unadvertised_pair() {
        for truthy in ["1", "y", "yes", "on", "true"] {
            assert!(
                bool::from(BooleanString::try_from(truthy).expect(truthy)),
                "{truthy} must parse as true"
            );
        }
        for falsy in ["0", "n", "no", "off", "false"] {
            assert!(
                !bool::from(BooleanString::try_from(falsy).expect(falsy)),
                "{falsy} must parse as false"
            );
        }
        for unadvertised in ["on", "off"] {
            assert!(
                !POSSIBLE.split(", ").any(|spelling| spelling == unadvertised),
                "{unadvertised} parses but must stay absent from POSSIBLE"
            );
        }
    }
}
