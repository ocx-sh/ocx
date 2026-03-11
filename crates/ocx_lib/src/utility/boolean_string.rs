// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::Error;

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

impl clap_builder::ValueEnum for BooleanString {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            Self::True1,
            Self::TrueY,
            Self::TrueYes,
            Self::TrueTrue,
            Self::False0,
            Self::FalseN,
            Self::FalseNo,
            Self::FalseFalse,
        ]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        use clap_builder::builder::PossibleValue;

        Some(match self {
            Self::True1 => PossibleValue::new("1"),
            Self::TrueY => PossibleValue::new("y"),
            Self::TrueYes => PossibleValue::new("yes"),
            Self::TrueOn => PossibleValue::new("on"),
            Self::TrueTrue => PossibleValue::new("true"),
            Self::False0 => PossibleValue::new("0"),
            Self::FalseN => PossibleValue::new("n"),
            Self::FalseNo => PossibleValue::new("no"),
            Self::FalseOff => PossibleValue::new("off"),
            Self::FalseFalse => PossibleValue::new("false"),
        })
    }
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

impl TryFrom<&str> for BooleanString {
    type Error = Error;

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
            _ => Err(Error::ConfigInvalidBooleanString(value.to_string())),
        }
    }
}
