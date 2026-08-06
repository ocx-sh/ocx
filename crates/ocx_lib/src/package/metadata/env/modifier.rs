// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::{constant, list, path};

/// Determines how an environment variable value is resolved at install time.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Modifier {
    /// A path variable is prepended to any existing value of the environment variable.
    Path(path::Path),
    /// A constant variable replaces any existing value of the environment variable.
    Constant(constant::Constant),
    /// A list variable is appended to any existing value of the environment
    /// variable, joined by its own separator, with earlier occurrences of the
    /// same contribution removed.
    List(list::List),
    /// A `type` this binary does not know — a package published against a newer
    /// ocx. Parsing survives so the refusal can name the package and the var
    /// (`ValidMetadata::try_from`); the variant is never written back out and
    /// never appears in the published schema.
    #[serde(skip_serializing)]
    #[schemars(skip)]
    Unknown { type_name: String },
}

impl<'de> Deserialize<'de> for Modifier {
    /// Reads the `type` tag through [`ModifierKind`]'s grammar first, so a tag
    /// this binary does not know becomes [`Modifier::Unknown`] carrying its
    /// spelling instead of a serde `unknown variant` blob. A known tag is then
    /// handed to the derive, which owns every message about the variant's own
    /// fields.
    ///
    /// The buffer is a [`serde_json::Value`]: metadata is a JSON wire format,
    /// and no public serde API buffers an unknown-tagged map without one.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum Known {
            Path(path::Path),
            Constant(constant::Constant),
            List(list::List),
        }

        let buffered = serde_json::Value::deserialize(deserializer)?;
        if let Some(tag) = buffered.get("type").and_then(serde_json::Value::as_str)
            && tag.parse::<ModifierKind>().is_err()
        {
            return Ok(Modifier::Unknown {
                type_name: tag.to_owned(),
            });
        }

        match Known::deserialize(buffered).map_err(serde::de::Error::custom)? {
            Known::Path(value) => Ok(Modifier::Path(value)),
            Known::Constant(value) => Ok(Modifier::Constant(value)),
            Known::List(value) => Ok(Modifier::List(value)),
        }
    }
}

/// How a declared value combines with the variable's existing value.
// This doc line is published verbatim as the schema's `ModifierKind`
// description, so it stays a user sentence. Rationale belongs here: the
// `JsonSchema` derive exists so schemas `$ref` one vocabulary instead of
// hand-spelling the type names beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModifierKind {
    Path,
    Constant,
    List,
}

/// A [`Modifier`] whose `type` tag this binary does not know, so it names no
/// [`ModifierKind`].
///
/// `ValidMetadata::try_from` rejects such a modifier before any consumer of
/// `ModifierKind` runs, so this error only escapes on the paths that convert
/// without that gate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("env modifier declares a type this ocx does not support")]
pub struct UnknownModifierError;

impl TryFrom<&Modifier> for ModifierKind {
    type Error = UnknownModifierError;

    fn try_from(modifier: &Modifier) -> Result<Self, Self::Error> {
        match modifier {
            Modifier::Path(_) => Ok(ModifierKind::Path),
            Modifier::Constant(_) => Ok(ModifierKind::Constant),
            Modifier::List(_) => Ok(ModifierKind::List),
            Modifier::Unknown { .. } => Err(UnknownModifierError),
        }
    }
}

impl fmt::Display for ModifierKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModifierKind::Path => write!(f, "path"),
            ModifierKind::Constant => write!(f, "constant"),
            ModifierKind::List => write!(f, "list"),
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
///
/// The remedy clause matters most to the reader who typed a type name a newer
/// ocx does define: without it, "expected `path`, `constant` or `list`" reads
/// as a typo report for what is really a version gap.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown modifier type '{found}'; expected `path`, `constant` or `list` (a newer ocx may support it)")]
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
            "list" => Ok(ModifierKind::List),
            found => Err(ParseModifierKindError {
                found: found.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::metadata::env::var::Var;
    use crate::package::metadata::visibility::Visibility;

    /// `Display` and `FromStr` are inverses. Pinning the round-trip is what
    /// keeps a future variant from landing with only one half implemented.
    #[test]
    fn modifier_kind_display_and_from_str_round_trip() {
        for kind in [ModifierKind::Path, ModifierKind::Constant, ModifierKind::List] {
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
        let message = error.to_string();
        for accepted in ["path", "constant", "list"] {
            assert!(
                message.contains(accepted),
                "the error must name every accepted value; '{accepted}' missing from: {message}"
            );
        }
    }

    // ── R-6: the `ocx.toml` / `--env` grammar names the version remedy ────────

    /// The shared `FromStr` error must offer the version remedy, not just the
    /// accepted spellings. A user who typed a type a newer ocx defines reads
    /// "expected `path`, `constant` or `list`" as "you made a typo" — the wrong
    /// repair. Surfaces as 78 from `ocx.toml` and 64 from `--env`; both carry
    /// this text.
    #[test]
    fn modifier_kind_unknown_spelling_offers_the_upgrade_remedy() {
        let message = "frobnicate"
            .parse::<ModifierKind>()
            .expect_err("'frobnicate' names no modifier")
            .to_string();
        assert!(
            message.contains("a newer ocx may support it"),
            "the error must point at the version gap; got: {message}"
        );
    }

    // ── R-1: unknown `type` parses to `Unknown`, carrying its spelling ────────

    /// A `type` tag this binary does not know survives parsing and keeps its
    /// spelling, so the refusal downstream can name the feature the package
    /// needs. Without this the reader dies inside serde with an unactionable
    /// `unknown variant` blob.
    #[test]
    fn unknown_type_tag_parses_and_keeps_its_name() {
        let json = r#"{"key":"GODEBUG","type":"frobnicate","separator":",","value":"gctrace=1"}"#;
        let var: Var = serde_json::from_str(json).expect("an unknown type must not break parsing");
        assert_eq!(var.key, "GODEBUG");
        match &var.modifier {
            Modifier::Unknown { type_name } => assert_eq!(type_name, "frobnicate"),
            other => panic!("expected Unknown carrying the tag; got {other:?}"),
        }
    }

    /// Known types are untouched by the `Unknown` fallback.
    #[test]
    fn known_type_tags_still_parse_to_their_variants() {
        let path: Var = serde_json::from_str(r#"{"key":"PATH","type":"path","value":"${installPath}/bin"}"#)
            .expect("path must parse");
        assert!(matches!(path.modifier, Modifier::Path(_)));

        let constant: Var = serde_json::from_str(r#"{"key":"JAVA_HOME","type":"constant","value":"${installPath}"}"#)
            .expect("constant must parse");
        assert!(matches!(constant.modifier, Modifier::Constant(_)));

        let list: Var = serde_json::from_str(r#"{"key":"GODEBUG","type":"list","separator":",","value":"gctrace=1"}"#)
            .expect("list must parse");
        assert!(matches!(list.modifier, Modifier::List(_)));
    }

    /// Characterization: the fallback must not swallow malformed *known* types.
    /// These messages are the pre-change text verbatim — a change here means the
    /// custom `Deserialize` regressed diagnostics for the common case.
    #[test]
    fn known_type_failures_keep_their_messages() {
        for (json, expected) in [
            (
                r#"{"key":"K","type":"path"}"#,
                "missing field `value` at line 1 column 25",
            ),
            (
                r#"{"key":"K","type":"constant"}"#,
                "missing field `value` at line 1 column 29",
            ),
            (r#"{"key":"K","value":"x"}"#, "missing field `type` at line 1 column 23"),
            (
                r#"{"key":"K","type":"path","value":7}"#,
                "invalid type: integer `7`, expected a string at line 1 column 35",
            ),
        ] {
            let message = serde_json::from_str::<Var>(json)
                .expect_err("malformed known type must still fail")
                .to_string();
            assert_eq!(message, expected, "error text drifted for {json}");
        }
    }

    /// The capability firewall is asymmetric on purpose (ADR D3): an unknown
    /// *sibling field* parameterizes an operator and is ignored, while an
    /// unknown *`type` tag* changes who wins and is refused. Pins the ignore
    /// half — adding `deny_unknown_fields` anywhere under `package/metadata/`
    /// would break every already-published package the moment a field is added.
    #[test]
    fn unknown_sibling_field_on_a_known_type_is_still_ignored() {
        let json = r#"{"key":"JAVA_HOME","type":"constant","value":"v","separator":",","future":1}"#;
        let var: Var = serde_json::from_str(json).expect("unknown sibling fields must be ignored, not rejected");
        assert!(matches!(var.modifier, Modifier::Constant(_)));
    }

    // ── R-2: `Unknown` is never written back out ──────────────────────────────

    /// Serializing an unknown modifier is an error, not a lossy round-trip. ocx
    /// cannot re-emit a type it cannot read without inventing the fields it
    /// dropped, so any code path that tries fails loudly instead.
    #[test]
    fn unknown_modifier_refuses_to_serialize() {
        let var = Var {
            key: "GODEBUG".to_string(),
            modifier: Modifier::Unknown {
                type_name: "frobnicate".to_string(),
            },
            visibility: Visibility::PRIVATE,
        };
        let error = serde_json::to_string(&var).expect_err("Unknown must never be emitted");
        assert!(
            error.to_string().contains("cannot be serialized"),
            "serialization must refuse explicitly; got: {error}"
        );
    }

    /// Known types round-trip unchanged.
    #[test]
    fn known_modifiers_round_trip() {
        for json in [
            r#"{"key":"PATH","type":"path","required":false,"value":"${installPath}/bin","visibility":"private"}"#,
            r#"{"key":"JAVA_HOME","type":"constant","value":"${installPath}","visibility":"public"}"#,
            r#"{"key":"GODEBUG","type":"list","separator":",","value":"gctrace=1","visibility":"public"}"#,
        ] {
            let var: Var = serde_json::from_str(json).expect("fixture parses");
            let emitted = serde_json::to_string(&var).expect("known types serialize");
            let reparsed: serde_json::Value = serde_json::from_str(&emitted).expect("emitted JSON parses");
            let original: serde_json::Value = serde_json::from_str(json).expect("fixture JSON parses");
            assert_eq!(reparsed, original, "round-trip changed the wire form of {json}");
        }
    }

    // ── R-5: no `ModifierKind::Unknown`; the conversion is fallible ───────────

    /// `ModifierKind` is the JSON `"type"` vocabulary. It gains no `Unknown`
    /// member — a type ocx cannot execute must never appear as one it can — so
    /// the conversion from a parsed modifier is fallible.
    #[test]
    fn modifier_kind_conversion_rejects_unknown() {
        let known = Modifier::Path(path::Path {
            required: false,
            value: "x".to_string(),
        });
        assert_eq!(ModifierKind::try_from(&known), Ok(ModifierKind::Path));

        let list = Modifier::List(list::List {
            separator: Some(",".to_string()),
            value: "gctrace=1".to_string(),
        });
        assert_eq!(ModifierKind::try_from(&list), Ok(ModifierKind::List));

        let unknown = Modifier::Unknown {
            type_name: "frobnicate".to_string(),
        };
        assert_eq!(ModifierKind::try_from(&unknown), Err(UnknownModifierError));
    }

    // ── W-4: the `list` wire shape ────────────────────────────────────────────

    /// The separator is data the fold depends on, so it must survive the wire
    /// verbatim — including a multi-character or whitespace one, which is
    /// exactly where a "helpfully" trimmed value would corrupt the result.
    #[test]
    fn list_separator_survives_the_wire_verbatim() {
        for separator in [",", " ", "; ", ":"] {
            let json = format!(r#"{{"key":"OPTS","type":"list","separator":"{separator}","value":"-ea"}}"#);
            let var: Var = serde_json::from_str(&json).expect("list fixture parses");
            match &var.modifier {
                Modifier::List(list) => assert_eq!(list.separator.as_deref(), Some(separator)),
                other => panic!("expected List; got {other:?}"),
            }
        }
    }

    /// A `list` without a separator parses. The refusal is deliberately not
    /// serde's: `ValidMetadata` names the variable and the remedy, where a
    /// missing-field error would name a byte offset.
    #[test]
    fn a_list_without_a_separator_parses_so_the_refusal_can_name_the_var() {
        let var: Var = serde_json::from_str(r#"{"key":"OPTS","type":"list","value":"-ea"}"#)
            .expect("the wire-requiredness gate is ValidMetadata's, not serde's");
        match &var.modifier {
            Modifier::List(list) => assert_eq!(list.separator, None),
            other => panic!("expected List; got {other:?}"),
        }
    }
}
