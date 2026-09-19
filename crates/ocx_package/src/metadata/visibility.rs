// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;

use serde::{Deserialize, Serialize};

// ── Entry-level visibility deserializer (§ ADR Tension 3, 4) ─────────────────

/// Reject `"sealed"` on `Var.visibility` at parse time.
///
/// `Var.visibility` uses `Visibility` as its type but restricts the valid wire
/// values to `["private", "public", "interface"]`. `"sealed"` is rejected
/// because a `Var` that is invisible everywhere (neither self nor consumer) is
/// dead config — see ADR Tension 4.
///
/// Used via `#[serde(deserialize_with = "deserialize_entry_visibility")]` on
/// `Var.visibility`. The restriction lives here rather than in a newtype because
/// all production construction goes through constant expressions (`Visibility::PRIVATE`,
/// `Visibility::PUBLIC`, `Visibility::INTERFACE`) and no caller ever needs
/// `TryFrom<Visibility>` outside of parse time.
pub fn deserialize_entry_visibility<'de, D>(deserializer: D) -> Result<Visibility, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Visibility::deserialize(deserializer)?;
    if v == Visibility::SEALED {
        return Err(serde::de::Error::custom(
            "sealed is not a valid entry-level visibility; use private, public, or interface",
        ));
    }
    Ok(v)
}

/// Default value for `Var.visibility` when the field is absent in JSON.
///
/// Returns `Visibility::PRIVATE` — the post-research-flip default (ADR Tension 1,
/// decision A, changelog 2026-04-29). `Visibility::default()` returns `SEALED`
/// (struct default, both booleans false), which is wrong for `Var.visibility`.
/// This function is used via `#[serde(default = "default_entry_visibility")]`.
pub const fn default_entry_visibility() -> Visibility {
    Visibility::PRIVATE
}

/// JSON Schema for `Var.visibility` — restricts the schema to the three valid
/// entry-axis values, excluding `"sealed"` (ADR Tension 4).
///
/// Used via `#[schemars(schema_with = "entry_visibility_schema")]` on
/// `Var.visibility`.
pub fn entry_visibility_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "enum": ["private", "public", "interface"],
        "description": "Entry-axis visibility on a `Var` entry: `private` (self-only), `public` \
                        (consumer + self), `interface` (consumer-only). `sealed` is rejected for \
                        entries — entries always belong to at least the package's own runtime. See \
                        https://ocx.sh/docs/reference/metadata for the full two-axis visibility model."
    })
}

/// Two-axis visibility marker for dependency edges and entry-axis carriers.
///
/// Inspired by CMake's `target_link_libraries` visibility model
/// (PUBLIC/PRIVATE/INTERFACE). The struct is two orthogonal booleans:
///
/// - `private` — self-axis: visible to the package's own execution
///   (shims, entry points).
/// - `interface` — consumer-axis: propagated to consumers of the package.
///
/// The four named values on the wire and in `--help` are the four
/// `(private, interface)` combinations: `sealed` (false, false),
/// `private` (true, false), `interface` (false, true), `public` (true, true).
///
/// Propagation through a dependency chain uses [`through_edge`](Self::through_edge).
/// Diamond deduplication uses [`merge`](Self::merge).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Visibility {
    /// Self-axis: visible to the package's own runtime (shims, entry points).
    pub private: bool,
    /// Consumer-axis: propagated to consumers of the package.
    pub interface: bool,
}

impl Visibility {
    /// No env propagation. Content accessed structurally (mount, path).
    /// Most deps in a tool-focused package manager.
    pub const SEALED: Self = Self {
        private: false,
        interface: false,
    };

    /// Env available for the package's own execution (shims, entry points),
    /// but not propagated to consumers.
    pub const PRIVATE: Self = Self {
        private: true,
        interface: false,
    };

    /// Env available for the package's own execution AND propagated to
    /// consumers.
    pub const PUBLIC: Self = Self {
        private: true,
        interface: true,
    };

    /// Env propagated to consumers but not used by the package itself.
    /// Typical for meta-packages that compose environments.
    pub const INTERFACE: Self = Self {
        private: false,
        interface: true,
    };

    /// Does this visibility contribute to the interface (consumer) surface?
    ///
    /// Returns `true` for `PUBLIC` and `INTERFACE`. Used by the two-env
    /// composer to gate TC entries and env-var entries for the default exec
    /// surface (consumer view, `--self` off).
    pub const fn has_interface(self) -> bool {
        self.interface
    }

    /// Does this visibility contribute to the private (self) surface?
    ///
    /// Returns `true` for `PUBLIC` and `PRIVATE`. Used by the two-env
    /// composer to gate TC entries and env-var entries for the `--self`
    /// runtime surface.
    pub const fn has_private(self) -> bool {
        self.private
    }

    /// Compose visibility through a dependency edge.
    ///
    /// If the child's effective visibility does not export to consumers
    /// (`child_eff.interface == false`), it cannot propagate at all →
    /// result is [`Self::SEALED`]. Otherwise the edge passes through unchanged.
    ///
    /// Used in `ResolvedPackage::with_dependencies()` when applying an edge
    /// to a child's already-resolved transitive deps. Renamed from `propagate`
    /// to clarify the inductive intent: "compose edge with child's effective
    /// visibility" (see `adr_two_env_composition.md`).
    pub const fn through_edge(self, child_eff: Self) -> Self {
        if child_eff.interface { self } else { Self::SEALED }
    }

    /// Merge two paths in a diamond — take the most open per axis.
    ///
    /// If *any* path makes a dep visible on an axis, it stays visible.
    /// Implements the OR operator on the (self, consumer) axes.
    pub const fn merge(self, other: Self) -> Self {
        Self {
            private: self.private || other.private,
            interface: self.interface || other.interface,
        }
    }
}

impl Serialize for Visibility {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = match (self.private, self.interface) {
            (false, false) => "sealed",
            (true, false) => "private",
            (true, true) => "public",
            (false, true) => "interface",
        };
        serializer.serialize_str(s)
    }
}

impl<'de> Deserialize<'de> for Visibility {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = <&str>::deserialize(deserializer)?;
        match s {
            "sealed" => Ok(Self::SEALED),
            "private" => Ok(Self::PRIVATE),
            "public" => Ok(Self::PUBLIC),
            "interface" => Ok(Self::INTERFACE),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["sealed", "private", "public", "interface"],
            )),
        }
    }
}

impl schemars::JsonSchema for Visibility {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Visibility")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "enum": ["sealed", "private", "public", "interface"],
        })
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match (self.private, self.interface) {
            (false, false) => "sealed",
            (true, false) => "private",
            (true, true) => "public",
            (false, true) => "interface",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEALED: Visibility = Visibility::SEALED;
    const PRIVATE: Visibility = Visibility::PRIVATE;
    const PUBLIC: Visibility = Visibility::PUBLIC;
    const INTERFACE: Visibility = Visibility::INTERFACE;

    // --- Serde & Display ---

    #[test]
    fn default_is_sealed() {
        assert_eq!(Visibility::default(), SEALED);
    }

    #[test]
    fn serde_roundtrip_sealed() {
        let json = serde_json::to_string(&SEALED).unwrap();
        assert_eq!(json, r#""sealed""#);
        assert_eq!(serde_json::from_str::<Visibility>(&json).unwrap(), SEALED);
    }

    #[test]
    fn serde_roundtrip_private() {
        let json = serde_json::to_string(&PRIVATE).unwrap();
        assert_eq!(json, r#""private""#);
        assert_eq!(serde_json::from_str::<Visibility>(&json).unwrap(), PRIVATE);
    }

    #[test]
    fn serde_roundtrip_public() {
        let json = serde_json::to_string(&PUBLIC).unwrap();
        assert_eq!(json, r#""public""#);
        assert_eq!(serde_json::from_str::<Visibility>(&json).unwrap(), PUBLIC);
    }

    #[test]
    fn serde_roundtrip_interface() {
        let json = serde_json::to_string(&INTERFACE).unwrap();
        assert_eq!(json, r#""interface""#);
        assert_eq!(serde_json::from_str::<Visibility>(&json).unwrap(), INTERFACE);
    }

    #[test]
    fn display_lowercase() {
        assert_eq!(SEALED.to_string(), "sealed");
        assert_eq!(PRIVATE.to_string(), "private");
        assert_eq!(PUBLIC.to_string(), "public");
        assert_eq!(INTERFACE.to_string(), "interface");
    }

    // --- Propagation table (all 16 cases) ---

    #[test]
    fn through_edge_table() {
        let cases: &[(Visibility, Visibility, Visibility)] = &[
            (PUBLIC, PUBLIC, PUBLIC),
            (PUBLIC, INTERFACE, PUBLIC),
            (PUBLIC, PRIVATE, SEALED),
            (PUBLIC, SEALED, SEALED),
            (PRIVATE, PUBLIC, PRIVATE),
            (PRIVATE, INTERFACE, PRIVATE),
            (PRIVATE, PRIVATE, SEALED),
            (PRIVATE, SEALED, SEALED),
            (INTERFACE, PUBLIC, INTERFACE),
            (INTERFACE, INTERFACE, INTERFACE),
            (INTERFACE, PRIVATE, SEALED),
            (INTERFACE, SEALED, SEALED),
            (SEALED, PUBLIC, SEALED),
            (SEALED, INTERFACE, SEALED),
            (SEALED, PRIVATE, SEALED),
            (SEALED, SEALED, SEALED),
        ];

        for &(edge, child, expected) in cases {
            assert_eq!(
                edge.through_edge(child),
                expected,
                "{edge}.through_edge({child}) should be {expected}"
            );
        }
    }

    // --- Merge table (all 16 cases) ---

    #[test]
    fn merge_table() {
        let cases: &[(Visibility, Visibility, Visibility)] = &[
            (SEALED, SEALED, SEALED),
            (SEALED, PRIVATE, PRIVATE),
            (SEALED, PUBLIC, PUBLIC),
            (SEALED, INTERFACE, INTERFACE),
            (PRIVATE, SEALED, PRIVATE),
            (PRIVATE, PRIVATE, PRIVATE),
            (PRIVATE, PUBLIC, PUBLIC),
            (PRIVATE, INTERFACE, PUBLIC),
            (PUBLIC, SEALED, PUBLIC),
            (PUBLIC, PRIVATE, PUBLIC),
            (PUBLIC, PUBLIC, PUBLIC),
            (PUBLIC, INTERFACE, PUBLIC),
            (INTERFACE, SEALED, INTERFACE),
            (INTERFACE, PRIVATE, PUBLIC),
            (INTERFACE, PUBLIC, PUBLIC),
            (INTERFACE, INTERFACE, INTERFACE),
        ];

        for &(a, b, expected) in cases {
            assert_eq!(a.merge(b), expected, "{a}.merge({b}) should be {expected}");
        }
    }

    // --- Merge commutativity ---

    #[test]
    fn merge_is_commutative() {
        let all = [SEALED, PRIVATE, PUBLIC, INTERFACE];
        for &a in &all {
            for &b in &all {
                assert_eq!(
                    a.merge(b),
                    b.merge(a),
                    "merge should be commutative: {a}.merge({b}) != {b}.merge({a})"
                );
            }
        }
    }

    #[test]
    fn merge_is_associative() {
        let all = [SEALED, PRIVATE, PUBLIC, INTERFACE];
        for &a in &all {
            for &b in &all {
                for &c in &all {
                    assert_eq!(
                        a.merge(b).merge(c),
                        a.merge(b.merge(c)),
                        "merge should be associative: ({a}.merge({b})).merge({c}) != {a}.merge({b}.merge({c}))"
                    );
                }
            }
        }
    }

    // --- Recursive propagation examples ---

    #[test]
    fn recursive_four_level_chain() {
        // Root --(Private)--> A --(Public)--> B --(Interface)--> C --(Public)--> D
        // D starts as Public. Walk edges from D back to Root:
        // Step 1: Interface.through_edge(Public) = Interface  (C→D through B's Interface edge)
        // Step 2: Public.through_edge(Interface) = Public     (B→C through A's Public edge)
        // Step 3: Private.through_edge(Public) = Private      (A→B through Root's Private edge)
        let d_at_c = INTERFACE.through_edge(PUBLIC);
        assert_eq!(d_at_c, INTERFACE);

        let d_at_b = PUBLIC.through_edge(d_at_c);
        assert_eq!(d_at_b, PUBLIC);

        let d_at_root = PRIVATE.through_edge(d_at_b);
        assert_eq!(d_at_root, PRIVATE);
    }

    #[test]
    fn diamond_sealed_and_public_merges_to_public() {
        // Path A: Public.through_edge(Private) = Sealed
        // Path B: Public.through_edge(Public) = Public
        // Merge: Sealed.merge(Public) = Public
        let path_a = PUBLIC.through_edge(PRIVATE);
        assert_eq!(path_a, SEALED);

        let path_b = PUBLIC.through_edge(PUBLIC);
        assert_eq!(path_b, PUBLIC);

        assert_eq!(path_a.merge(path_b), PUBLIC);
    }

    #[test]
    fn diamond_private_and_interface_merges_to_public() {
        // Private: self=true, consumer=false
        // Interface: self=false, consumer=true
        // Merge: self=true||false=true, consumer=false||true=true → Public
        assert_eq!(PRIVATE.merge(INTERFACE), PUBLIC);
    }

    // ── Entry-axis visibility deserializer (ADR Tension 1, Tension 4) ───────────
    //
    // `deserialize_entry_visibility` enforces the 3-valid-value restriction for
    // `Var.visibility`: private, public, interface. `sealed` is rejected because
    // a declared Var invisible on every surface is dead config (ADR Tension 4).
    // Default for absent field is `Visibility::PRIVATE` (post-research flip per
    // ADR Tension 1 changelog 2026-04-29).
    //
    // These tests exercise the deserializer in isolation via a thin wrapper type.
    // End-to-end coverage lives in `var.rs` tests.

    #[derive(serde::Deserialize)]
    struct VisWrapper {
        #[serde(deserialize_with = "super::deserialize_entry_visibility")]
        v: Visibility,
    }

    fn deser_entry(s: &str) -> Result<Visibility, serde_json::Error> {
        let json = format!(r#"{{"v":{s}}}"#);
        serde_json::from_str::<VisWrapper>(&json).map(|w| w.v)
    }

    #[test]
    fn entry_visibility_accepts_private() {
        assert_eq!(deser_entry(r#""private""#).unwrap(), PRIVATE);
    }

    #[test]
    fn entry_visibility_accepts_public() {
        assert_eq!(deser_entry(r#""public""#).unwrap(), PUBLIC);
    }

    #[test]
    fn entry_visibility_accepts_interface() {
        assert_eq!(deser_entry(r#""interface""#).unwrap(), INTERFACE);
    }

    #[test]
    fn entry_visibility_rejects_sealed() {
        let err = deser_entry(r#""sealed""#).expect_err("sealed must be rejected on entries (ADR Tension 4)");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("sealed is not a valid entry-level visibility"),
            "error must mention `sealed is not a valid entry-level visibility`; got: {msg}"
        );
    }

    // ── has_interface() / has_private() accessors ────────────────────────────

    #[test]
    fn has_interface_returns_true_for_public_and_interface() {
        assert!(PUBLIC.has_interface(), "PUBLIC.has_interface() must be true");
        assert!(INTERFACE.has_interface(), "INTERFACE.has_interface() must be true");
        assert!(!PRIVATE.has_interface(), "PRIVATE.has_interface() must be false");
        assert!(!SEALED.has_interface(), "SEALED.has_interface() must be false");
    }

    #[test]
    fn has_private_returns_true_for_public_and_private() {
        assert!(PUBLIC.has_private(), "PUBLIC.has_private() must be true");
        assert!(PRIVATE.has_private(), "PRIVATE.has_private() must be true");
        assert!(!INTERFACE.has_private(), "INTERFACE.has_private() must be false");
        assert!(!SEALED.has_private(), "SEALED.has_private() must be false");
    }

    /// An `INTERFACE` package's interface surface is non-empty: `has_interface()` is
    /// true but `has_private()` is false, confirming the entry is exported to consumers
    /// only (meta-package / composition-only scenario).
    #[test]
    fn interface_visibility_has_non_empty_interface_surface() {
        assert!(
            INTERFACE.has_interface(),
            "INTERFACE package must appear on the interface (consumer) surface"
        );
        assert!(
            !INTERFACE.has_private(),
            "INTERFACE package must NOT appear on the private (self) surface"
        );
    }
}
