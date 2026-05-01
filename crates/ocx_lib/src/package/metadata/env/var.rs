// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use super::{constant, path};
use crate::package::metadata::visibility::{
    Visibility, default_entry_visibility, deserialize_entry_visibility, entry_visibility_schema,
};

pub use super::modifier::{Modifier, ModifierKind};

/// An environment variable declaration.
///
/// Each variable has a key (the variable name), a [modifier](Modifier) that
/// determines how the value is resolved, and a visibility that controls which
/// exec surfaces load the entry. The modifier's type and fields are flattened
/// into this object in JSON.
///
/// `visibility` defaults to [`Visibility::PRIVATE`] per ADR
/// `adr_visibility_two_axis_and_exec_modes.md` Tension 1 (A): publishers must
/// opt in explicitly to expose entries on the consumer axis. `"sealed"` is
/// rejected at parse time — a `Var` invisible on every surface is dead config
/// (ADR Tension 4).
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Var {
    /// The environment variable name (e.g. `PATH`, `JAVA_HOME`).
    pub key: String,

    #[serde(flatten)]
    pub modifier: Modifier,

    /// Visibility on the entry axis — controls which exec surface (interface vs private)
    /// sees this entry; see [`crate::package_manager::composer`]. Defaults to
    /// `private` — publishers explicitly mark contract entries as `public` or
    /// `interface` to expose them to consumers. `"sealed"` is rejected at parse
    /// time (ADR Tension 4).
    #[serde(
        default = "default_entry_visibility",
        deserialize_with = "deserialize_entry_visibility"
    )]
    #[schemars(schema_with = "entry_visibility_schema")]
    pub visibility: Visibility,
}

impl Var {
    /// Constructs a path-modifier `Var` with default visibility
    /// ([`Visibility::PRIVATE`]). Post-ADR-flip semantics: callers that
    /// want consumer-visible PATH entries must use
    /// [`Var::new_path_with_visibility`].
    pub fn new_path(key: impl ToString, value: impl ToString, required: bool) -> Self {
        Var {
            key: key.to_string(),
            modifier: Modifier::Path(path::Path {
                required,
                value: value.to_string(),
            }),
            visibility: Visibility::PRIVATE,
        }
    }

    /// Constructs a path-modifier `Var` with the supplied visibility.
    pub fn new_path_with_visibility(
        key: impl ToString,
        value: impl ToString,
        required: bool,
        visibility: Visibility,
    ) -> Self {
        Var {
            key: key.to_string(),
            modifier: Modifier::Path(path::Path {
                required,
                value: value.to_string(),
            }),
            visibility,
        }
    }

    /// Constructs a constant-modifier `Var` with default visibility
    /// ([`Visibility::PRIVATE`]). See [`Var::new_path`] note on the
    /// post-ADR-flip default.
    pub fn new_constant(key: impl ToString, value: impl ToString) -> Self {
        Var {
            key: key.to_string(),
            modifier: Modifier::Constant(constant::Constant {
                value: value.to_string(),
            }),
            visibility: Visibility::PRIVATE,
        }
    }

    /// Constructs a constant-modifier `Var` with the supplied visibility.
    pub fn new_constant_with_visibility(key: impl ToString, value: impl ToString, visibility: Visibility) -> Self {
        Var {
            key: key.to_string(),
            modifier: Modifier::Constant(constant::Constant {
                value: value.to_string(),
            }),
            visibility,
        }
    }

    pub fn value(&self) -> Option<&str> {
        match &self.modifier {
            Modifier::Path(path_var) => Some(&path_var.value),
            Modifier::Constant(constant_var) => Some(&constant_var.value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::metadata::visibility::Visibility;

    // ── Var.visibility deserialization (plan §337, ADR Tension 1) ─────────────
    //
    // Per plan §169 + ADR Tension 1 (decision A, post-research flip), absent
    // visibility on a Var deserializes to `Visibility::PRIVATE` — NOT public.
    // This is the load-bearing post-research-flip migration commitment from the
    // ADR changelog 2026-04-29.
    //
    // `"sealed"` is rejected at parse time via the custom `deserialize_entry_visibility`
    // function on `Var.visibility` (ADR Tension 4).

    #[test]
    fn var_deserialize_absent_visibility_defaults_to_private() {
        // No `visibility` field — must default to private under post-research-flip
        // semantics (NOT public; reviewer A1 carry-forward).
        let json = r#"{"key":"PATH","type":"path","value":"${installPath}/bin"}"#;
        let var: Var = serde_json::from_str(json).expect("absent visibility must default to private");
        assert_eq!(var.visibility, Visibility::PRIVATE);
    }

    #[test]
    fn var_deserialize_explicit_private_roundtrips() {
        let json = r#"{"key":"_OCX_INTERNAL","type":"constant","value":"1","visibility":"private"}"#;
        let var: Var = serde_json::from_str(json).expect("explicit private must parse");
        assert_eq!(var.visibility, Visibility::PRIVATE);
        assert_eq!(var.key, "_OCX_INTERNAL");
    }

    #[test]
    fn var_deserialize_explicit_public_roundtrips() {
        let json = r#"{"key":"PATH","type":"path","value":"${installPath}/bin","visibility":"public"}"#;
        let var: Var = serde_json::from_str(json).expect("explicit public must parse");
        assert_eq!(var.visibility, Visibility::PUBLIC);
    }

    #[test]
    fn var_deserialize_explicit_interface_roundtrips() {
        let json = r#"{"key":"JAVA_HOME","type":"constant","value":"${installPath}","visibility":"interface"}"#;
        let var: Var = serde_json::from_str(json).expect("explicit interface must parse");
        assert_eq!(var.visibility, Visibility::INTERFACE);
    }

    /// Sealed on Var.visibility is rejected with a structured error per ADR Tension 4.
    /// The rejection surfaces as a serde error whose message contains the canonical
    /// lowercase phrase from `quality-rust-errors.md`.
    #[test]
    fn var_deserialize_sealed_rejected_with_structured_error() {
        let json = r#"{"key":"X","type":"constant","value":"y","visibility":"sealed"}"#;
        let result = serde_json::from_str::<Var>(json);
        let err = result.expect_err("sealed must be rejected on Var.visibility (ADR Tension 4)");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("sealed is not a valid entry-level visibility"),
            "error message must contain `sealed is not a valid entry-level visibility`; got: {msg}"
        );
    }

    /// Unknown variants on `visibility` are rejected by serde's default
    /// behaviour. Pins this so a future relaxation (e.g. accepting
    /// "Sealed"-uppercase or arbitrary strings) doesn't slip through.
    #[test]
    fn var_deserialize_unknown_visibility_value_rejected() {
        let json = r#"{"key":"X","type":"constant","value":"y","visibility":"omega"}"#;
        let result = serde_json::from_str::<Var>(json);
        assert!(
            result.is_err(),
            "unknown visibility value must be rejected (no silent-default semantics)"
        );
    }
}
