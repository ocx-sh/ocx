// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::{constant, path};

/// Determines how an environment variable value is resolved at install time.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Modifier {
    /// A path variable is prepended to any existing value of the environment variable.
    Path(path::Path),
    /// A constant variable replaces any existing value of the environment variable.
    Constant(constant::Constant),
}

/// The modifier kind stripped of inner data — suitable for display and serialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModifierKind {
    Path,
    Constant,
}

impl From<&Modifier> for ModifierKind {
    fn from(modifier: &Modifier) -> Self {
        match modifier {
            Modifier::Path(_) => ModifierKind::Path,
            Modifier::Constant(_) => ModifierKind::Constant,
        }
    }
}

impl fmt::Display for ModifierKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModifierKind::Path => write!(f, "path"),
            ModifierKind::Constant => write!(f, "constant"),
        }
    }
}

/// The spelling `str` carried, when it named no [`ModifierKind`].
///
/// Carries `found` structurally rather than pre-formatting a message, so each
/// caller can fold it into its own typed error: the `ocx.toml` parser into
/// `ProjectErrorKind::EnvUnknownModifier` (exit 78 — a config-shape fault in a
/// file), the `ocx run --env` parser into `cli::UsageError` (exit 64 — CLI
/// misuse). One grammar, two exit codes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown modifier type '{found}'; expected `path` or `constant`")]
pub struct ParseModifierKindError {
    /// The unrecognized spelling, verbatim.
    pub found: String,
}

impl FromStr for ModifierKind {
    type Err = ParseModifierKindError;

    /// The inverse of [`ModifierKind`]'s [`fmt::Display`], so the pair
    /// round-trips. Both spellings are consumer-authored — in a `{ type = … }`
    /// table and in `ocx run --env KEY:TYPE=VALUE` — and a second hand-rolled
    /// match would let the two surfaces drift.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "path" => Ok(ModifierKind::Path),
            "constant" => Ok(ModifierKind::Constant),
            found => Err(ParseModifierKindError {
                found: found.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Display` and `FromStr` are inverses. Pinning the round-trip is what
    /// keeps a future variant from landing with only one half implemented.
    #[test]
    fn modifier_kind_display_and_from_str_round_trip() {
        for kind in [ModifierKind::Path, ModifierKind::Constant] {
            let spelled = kind.to_string();
            assert_eq!(
                spelled.parse::<ModifierKind>(),
                Ok(kind.clone()),
                "'{spelled}' must parse back to the kind that spelled it"
            );
        }
    }

    /// An unknown spelling carries itself into the error, so each caller can
    /// name it in its own message.
    #[test]
    fn modifier_kind_rejects_unknown_spelling() {
        let error = "bogus".parse::<ModifierKind>().expect_err("'bogus' names no modifier");
        assert_eq!(error.found, "bogus");
        assert!(
            error.to_string().contains("path") && error.to_string().contains("constant"),
            "the error must name the accepted values; got: {error}"
        );
    }
}
