// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;

use serde::{Deserialize, Serialize};

/// Controls how a dependency's environment variables propagate to the
/// package and its consumers.
///
/// Inspired by CMake's `target_link_libraries` visibility model
/// (PUBLIC/PRIVATE/INTERFACE). Each variant encodes two orthogonal axes:
/// self-visible (package's own execution) and consumer-visible (propagated
/// to consumers of the package).
///
/// Propagation through dependency chains uses [`propagate`](Self::propagate).
/// Diamond deduplication uses [`merge`](Self::merge).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// No env propagation. Content accessed structurally (mount, path).
    /// Most deps in a tool-focused package manager.
    #[default]
    Sealed,

    /// Env available for the package's own execution (shims, entry points),
    /// but not propagated to consumers.
    Private,

    /// Env available for the package's own execution AND propagated to
    /// consumers.
    Public,

    /// Env propagated to consumers but not used by the package itself.
    /// Typical for meta-packages that compose environments.
    Interface,
}

impl Visibility {
    /// Does this dep's env contribute to the package's own execution?
    pub fn is_self_visible(self) -> bool {
        matches!(self, Self::Public | Self::Private)
    }

    /// Does this dep's env propagate to consumers of the package?
    pub fn is_consumer_visible(self) -> bool {
        matches!(self, Self::Public | Self::Interface)
    }

    /// Is this dep visible at all — either for self-execution or consumers?
    ///
    /// Returns `true` for everything except `Sealed`. Used by `resolve_env()`
    /// when the package is a direct exec/env target: the user is both executing
    /// the package (self) and consuming its output (consumer).
    pub fn is_visible(self) -> bool {
        !matches!(self, Self::Sealed)
    }

    /// Propagate visibility through a dependency edge.
    ///
    /// Encodes the propagation table as an explicit match on the Cartesian
    /// product of (edge, child). The child decides whether to export
    /// (consumer-visible); the parent's edge decides the terms.
    ///
    /// Used in `ResolvedPackage::with_dependencies()` when applying an edge
    /// to a child's already-resolved transitive deps.
    #[rustfmt::skip]
    pub fn propagate(self, child: Self) -> Self {
        use Visibility::*;
        match (self, child) {
            (Public,    Public)    => Public,
            (Public,    Interface) => Public,
            (Private,   Public)    => Private,
            (Private,   Interface) => Private,
            (Interface, Public)    => Interface,
            (Interface, Interface) => Interface,
            (_,         Private)   => Sealed,
            (_,         Sealed)    => Sealed,
            (Sealed,    _)         => Sealed,
        }
    }

    /// Merge two paths in a diamond — take the most open per axis.
    ///
    /// If *any* path makes a dep visible on an axis, it stays visible.
    /// Implements the OR operator on the (self, consumer) axes.
    pub fn merge(self, other: Self) -> Self {
        Self::from_axes(
            self.is_self_visible() || other.is_self_visible(),
            self.is_consumer_visible() || other.is_consumer_visible(),
        )
    }

    /// Construct from the two boolean axes.
    fn from_axes(self_visible: bool, consumer_visible: bool) -> Self {
        match (self_visible, consumer_visible) {
            (true, true) => Self::Public,
            (true, false) => Self::Private,
            (false, true) => Self::Interface,
            (false, false) => Self::Sealed,
        }
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sealed => write!(f, "sealed"),
            Self::Private => write!(f, "private"),
            Self::Public => write!(f, "public"),
            Self::Interface => write!(f, "interface"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use Visibility::{Interface, Private, Public, Sealed};

    // --- Serde & Display ---

    #[test]
    fn default_is_sealed() {
        assert_eq!(Visibility::default(), Sealed);
    }

    #[test]
    fn serde_roundtrip_sealed() {
        let json = serde_json::to_string(&Sealed).unwrap();
        assert_eq!(json, r#""sealed""#);
        assert_eq!(serde_json::from_str::<Visibility>(&json).unwrap(), Sealed);
    }

    #[test]
    fn serde_roundtrip_private() {
        let json = serde_json::to_string(&Private).unwrap();
        assert_eq!(json, r#""private""#);
        assert_eq!(serde_json::from_str::<Visibility>(&json).unwrap(), Private);
    }

    #[test]
    fn serde_roundtrip_public() {
        let json = serde_json::to_string(&Public).unwrap();
        assert_eq!(json, r#""public""#);
        assert_eq!(serde_json::from_str::<Visibility>(&json).unwrap(), Public);
    }

    #[test]
    fn serde_roundtrip_interface() {
        let json = serde_json::to_string(&Interface).unwrap();
        assert_eq!(json, r#""interface""#);
        assert_eq!(serde_json::from_str::<Visibility>(&json).unwrap(), Interface);
    }

    #[test]
    fn display_lowercase() {
        assert_eq!(Sealed.to_string(), "sealed");
        assert_eq!(Private.to_string(), "private");
        assert_eq!(Public.to_string(), "public");
        assert_eq!(Interface.to_string(), "interface");
    }

    // --- Axis truth tables ---

    #[test]
    fn is_self_visible() {
        assert!(!Sealed.is_self_visible());
        assert!(Private.is_self_visible());
        assert!(Public.is_self_visible());
        assert!(!Interface.is_self_visible());
    }

    #[test]
    fn is_consumer_visible() {
        assert!(!Sealed.is_consumer_visible());
        assert!(!Private.is_consumer_visible());
        assert!(Public.is_consumer_visible());
        assert!(Interface.is_consumer_visible());
    }

    #[test]
    fn is_visible() {
        assert!(!Sealed.is_visible());
        assert!(Private.is_visible());
        assert!(Public.is_visible());
        assert!(Interface.is_visible());
    }

    // --- Propagation table (all 16 cases) ---

    #[test]
    fn propagation_table() {
        let cases: &[(Visibility, Visibility, Visibility)] = &[
            (Public, Public, Public),
            (Public, Interface, Public),
            (Public, Private, Sealed),
            (Public, Sealed, Sealed),
            (Private, Public, Private),
            (Private, Interface, Private),
            (Private, Private, Sealed),
            (Private, Sealed, Sealed),
            (Interface, Public, Interface),
            (Interface, Interface, Interface),
            (Interface, Private, Sealed),
            (Interface, Sealed, Sealed),
            (Sealed, Public, Sealed),
            (Sealed, Interface, Sealed),
            (Sealed, Private, Sealed),
            (Sealed, Sealed, Sealed),
        ];

        for &(edge, child, expected) in cases {
            assert_eq!(
                edge.propagate(child),
                expected,
                "{edge}.propagate({child}) should be {expected}"
            );
        }
    }

    // --- Merge table (all 16 cases) ---

    #[test]
    fn merge_table() {
        let cases: &[(Visibility, Visibility, Visibility)] = &[
            (Sealed, Sealed, Sealed),
            (Sealed, Private, Private),
            (Sealed, Public, Public),
            (Sealed, Interface, Interface),
            (Private, Sealed, Private),
            (Private, Private, Private),
            (Private, Public, Public),
            (Private, Interface, Public),
            (Public, Sealed, Public),
            (Public, Private, Public),
            (Public, Public, Public),
            (Public, Interface, Public),
            (Interface, Sealed, Interface),
            (Interface, Private, Public),
            (Interface, Public, Public),
            (Interface, Interface, Interface),
        ];

        for &(a, b, expected) in cases {
            assert_eq!(a.merge(b), expected, "{a}.merge({b}) should be {expected}");
        }
    }

    // --- Merge commutativity ---

    #[test]
    fn merge_is_commutative() {
        let all = [Sealed, Private, Public, Interface];
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
        let all = [Sealed, Private, Public, Interface];
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
        // Step 1: Interface.propagate(Public) = Interface  (C→D through B's Interface edge)
        // Step 2: Public.propagate(Interface) = Public     (B→C through A's Public edge)
        // Step 3: Private.propagate(Public) = Private      (A→B through Root's Private edge)
        let d_at_c = Interface.propagate(Public);
        assert_eq!(d_at_c, Interface);

        let d_at_b = Public.propagate(d_at_c);
        assert_eq!(d_at_b, Public);

        let d_at_root = Private.propagate(d_at_b);
        assert_eq!(d_at_root, Private);
    }

    #[test]
    fn diamond_sealed_and_public_merges_to_public() {
        // Path A: Public.propagate(Private) = Sealed
        // Path B: Public.propagate(Public) = Public
        // Merge: Sealed.merge(Public) = Public
        let path_a = Public.propagate(Private);
        assert_eq!(path_a, Sealed);

        let path_b = Public.propagate(Public);
        assert_eq!(path_b, Public);

        assert_eq!(path_a.merge(path_b), Public);
    }

    #[test]
    fn diamond_private_and_interface_merges_to_public() {
        // Private: self=true, consumer=false
        // Interface: self=false, consumer=true
        // Merge: self=true||false=true, consumer=false||true=true → Public
        assert_eq!(Private.merge(Interface), Public);
    }
}
