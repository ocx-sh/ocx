// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use crate::oci::PinnedIdentifier;
use crate::package::metadata::visibility::Visibility;

/// A dependency in the transitive closure with its pre-computed visibility.
///
/// The `visibility` field encodes the effective visibility from the root
/// package's perspective, computed via [`Visibility::through_edge`] through
/// the dependency chain. Diamond deps use [`Visibility::merge`] (OR on
/// each axis) — if ANY path makes a dep visible, it stays visible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedDependency {
    pub identifier: PinnedIdentifier,
    pub visibility: Visibility,
}

/// Persisted resolution state for an installed package.
///
/// Written to `resolve.json` in each object directory at install time.
/// Contains the package's transitive dependency closure in topological order
/// (deps before dependents). The root package's own identifier is **not**
/// stored here — it is redundant with the caller context and would couple the
/// identity of a shared, deduplicated package directory to whichever installer
/// won the cross-repo race.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPackage {
    /// Transitive dependency closure with pre-computed visibility.
    /// Deps before dependents. The root package itself is **not** included.
    /// Leaf packages (no dependencies) have an empty vec.
    pub dependencies: Vec<ResolvedDependency>,
}

impl ResolvedPackage {
    /// Creates a leaf package with no dependencies.
    pub fn new() -> Self {
        Self {
            dependencies: Vec::new(),
        }
    }

    /// Builds the transitive dependency closure from resolved direct deps.
    ///
    /// Each item is `(child_id, child_resolved, edge_visibility)`. The
    /// identifier is supplied separately because [`ResolvedPackage`] no longer
    /// carries its own root identifier — that would couple shared package
    /// directories to whichever installer won the cross-repo race.
    ///
    /// Edge composition rule: if the child exports (consumer-visible), result =
    /// edge via [`Visibility::through_edge`]; otherwise Sealed. Diamond deps
    /// use [`Visibility::merge`] — if any path makes a dep visible, the final
    /// visibility is the most open.
    ///
    /// Preserves topological order (deps before dependents) and deduplicates
    /// by identity (advisory tags stripped).
    pub fn with_dependencies(
        mut self,
        deps: impl IntoIterator<Item = (PinnedIdentifier, ResolvedPackage, Visibility)>,
    ) -> Self {
        // Maps stripped identity → index in self.dependencies for OR dedup.
        let mut seen: std::collections::HashMap<PinnedIdentifier, usize> = std::collections::HashMap::new();

        for (dep_id, dep, edge) in deps {
            // Bubble up transitive deps first (preserves topological order).
            for transitive in dep.dependencies {
                let propagated = edge.through_edge(transitive.visibility);
                let key = transitive.identifier.strip_advisory();
                if let Some(&idx) = seen.get(&key) {
                    // Diamond merge: take the most open visibility.
                    self.dependencies[idx].visibility = self.dependencies[idx].visibility.merge(propagated);
                } else {
                    let idx = self.dependencies.len();
                    seen.insert(key, idx);
                    self.dependencies.push(ResolvedDependency {
                        identifier: transitive.identifier,
                        visibility: propagated,
                    });
                }
            }

            // Then add the direct dep itself.
            let key = dep_id.strip_advisory();
            if let Some(&idx) = seen.get(&key) {
                self.dependencies[idx].visibility = self.dependencies[idx].visibility.merge(edge);
            } else {
                let idx = self.dependencies.len();
                seen.insert(key, idx);
                self.dependencies.push(ResolvedDependency {
                    identifier: dep_id,
                    visibility: edge,
                });
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::{Digest, Identifier};

    fn sha256_hex() -> String {
        "a".repeat(64)
    }

    fn make_pinned() -> PinnedIdentifier {
        let id = Identifier::new_registry("cmake", "example.com").clone_with_digest(Digest::Sha256(sha256_hex()));
        PinnedIdentifier::try_from(id).unwrap()
    }

    fn make_dep_pinned() -> PinnedIdentifier {
        let id = Identifier::new_registry("zlib", "example.com").clone_with_digest(Digest::Sha256("b".repeat(64)));
        PinnedIdentifier::try_from(id).unwrap()
    }

    fn make_pinned_repo(repo: &str, hex_char: char) -> PinnedIdentifier {
        let id =
            Identifier::new_registry(repo, "ocx.sh").clone_with_digest(Digest::Sha256(hex_char.to_string().repeat(64)));
        PinnedIdentifier::try_from(id).unwrap()
    }

    /// Test wrapper pairing an identifier with its resolved closure.
    ///
    /// Production [`ResolvedPackage`] no longer carries an identifier, but
    /// the test helpers need to chain resolutions by name — the wrapper keeps
    /// the (id, resolved) tuple together while tests build graphs.
    #[derive(Clone)]
    struct TestPkg {
        id: PinnedIdentifier,
        resolved: ResolvedPackage,
    }

    impl std::ops::Deref for TestPkg {
        type Target = ResolvedPackage;
        fn deref(&self) -> &Self::Target {
            &self.resolved
        }
    }

    impl TestPkg {
        fn with_dependencies(mut self, deps: impl IntoIterator<Item = (TestPkg, Visibility)>) -> Self {
            self.resolved = self
                .resolved
                .with_dependencies(deps.into_iter().map(|(p, v)| (p.id, p.resolved, v)));
            self
        }
    }

    /// Helper: build a `TestPkg` leaf with no dependencies.
    fn leaf(repo: &str, hex: char) -> TestPkg {
        TestPkg {
            id: make_pinned_repo(repo, hex),
            resolved: ResolvedPackage::new(),
        }
    }

    /// Helper: assert a dep at index has the expected repo and visibility.
    fn assert_dep(deps: &[ResolvedDependency], idx: usize, repo: &str, visibility: Visibility) {
        assert_eq!(deps[idx].identifier.repository(), repo, "dep[{idx}] repo mismatch");
        assert_eq!(
            deps[idx].visibility, visibility,
            "dep[{idx}] ({repo}) visibility mismatch"
        );
    }

    // ── Serialization tests ─────────────────────────────────────────

    #[test]
    fn serde_roundtrip_visibility_public() {
        let dep = ResolvedDependency {
            identifier: make_dep_pinned(),
            visibility: Visibility::PUBLIC,
        };
        let pkg = ResolvedPackage {
            dependencies: vec![dep.clone()],
        };
        let json = serde_json::to_string(&pkg).unwrap();
        let deserialized: ResolvedPackage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.dependencies.len(), 1);
        assert_eq!(deserialized.dependencies[0], dep);
    }

    #[test]
    fn serde_roundtrip_visibility_sealed() {
        let dep = ResolvedDependency {
            identifier: make_dep_pinned(),
            visibility: Visibility::SEALED,
        };
        let pkg = ResolvedPackage {
            dependencies: vec![dep.clone()],
        };
        let json = serde_json::to_string(&pkg).unwrap();
        let deserialized: ResolvedPackage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.dependencies[0].visibility, Visibility::SEALED);
    }

    #[test]
    fn serde_roundtrip_mixed_visibility() {
        let deps = vec![
            ResolvedDependency {
                identifier: make_dep_pinned(),
                visibility: Visibility::PUBLIC,
            },
            ResolvedDependency {
                identifier: make_pinned_repo("other", 'c'),
                visibility: Visibility::SEALED,
            },
        ];
        let pkg = ResolvedPackage {
            dependencies: deps.clone(),
        };
        let json = serde_json::to_string(&pkg).unwrap();
        let deserialized: ResolvedPackage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.dependencies, deps);
    }

    #[test]
    fn serde_leaf_package_empty_dependencies() {
        let pkg = ResolvedPackage { dependencies: vec![] };
        let json = serde_json::to_string(&pkg).unwrap();
        let deserialized: ResolvedPackage = serde_json::from_str(&json).unwrap();
        assert!(deserialized.dependencies.is_empty());
    }

    #[test]
    fn deserialize_accepts_empty_dependencies() {
        let json = r#"{"dependencies": []}"#;
        let pkg: ResolvedPackage = serde_json::from_str(json).unwrap();
        assert!(pkg.dependencies.is_empty());
    }

    #[test]
    fn deserialize_rejects_missing_dependencies() {
        let json = "{}";
        let result = serde_json::from_str::<ResolvedPackage>(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_old_format_with_identifier_field() {
        // Old format had a root `identifier` — rejected with deny_unknown_fields
        // to force a fresh install rather than silently loading stale repo data.
        let id = make_pinned();
        let json = format!(r#"{{"identifier":"{}","dependencies":[]}}"#, id);
        let result = serde_json::from_str::<ResolvedPackage>(&json);
        assert!(
            result.is_err(),
            "old format with root identifier field should fail deserialization"
        );
    }

    #[test]
    fn deserialize_rejects_old_format_bare_string_deps() {
        let dep = make_dep_pinned();
        let json = format!(r#"{{"dependencies":["{}"]}}"#, dep);
        let result = serde_json::from_str::<ResolvedPackage>(&json);
        assert!(
            result.is_err(),
            "old format with bare string deps should fail deserialization"
        );
    }

    // ── Basic chains (no diamonds) ──────────────────────────────────

    #[test]
    fn with_dependencies_no_deps() {
        let resolved = leaf("app", 'a');
        assert!(resolved.dependencies.is_empty());
    }

    #[test]
    fn single_public_dep() {
        let resolved = leaf("root", 'a').with_dependencies([(leaf("x", 'b'), Visibility::PUBLIC)]);
        assert_eq!(resolved.dependencies.len(), 1);
        assert_dep(&resolved.dependencies, 0, "x", Visibility::PUBLIC);
    }

    #[test]
    fn single_sealed_dep() {
        let resolved = leaf("root", 'a').with_dependencies([(leaf("x", 'b'), Visibility::SEALED)]);
        assert_eq!(resolved.dependencies.len(), 1);
        assert_dep(&resolved.dependencies, 0, "x", Visibility::SEALED);
    }

    #[test]
    fn all_public_chain() {
        // Root→A(Public)→B(Public)→C(Public)
        let c_resolved = leaf("c", 'c').with_dependencies([]);
        let b_resolved = leaf("b", 'b').with_dependencies([(c_resolved, Visibility::PUBLIC)]);
        let a_resolved = leaf("a", 'a').with_dependencies([(b_resolved, Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a_resolved, Visibility::PUBLIC)]);

        assert_eq!(root.dependencies.len(), 3);
        assert_dep(&root.dependencies, 0, "c", Visibility::PUBLIC);
        assert_dep(&root.dependencies, 1, "b", Visibility::PUBLIC);
        assert_dep(&root.dependencies, 2, "a", Visibility::PUBLIC);
    }

    #[test]
    fn break_at_root_edge() {
        // Root→A(Sealed)→B(Public)→C(Public): all become sealed
        let c_resolved = leaf("c", 'c');
        let b_resolved = leaf("b", 'b').with_dependencies([(c_resolved, Visibility::PUBLIC)]);
        let a_resolved = leaf("a", 'a').with_dependencies([(b_resolved, Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a_resolved, Visibility::SEALED)]);

        assert_eq!(root.dependencies.len(), 3);
        assert_dep(&root.dependencies, 0, "c", Visibility::SEALED);
        assert_dep(&root.dependencies, 1, "b", Visibility::SEALED);
        assert_dep(&root.dependencies, 2, "a", Visibility::SEALED);
    }

    #[test]
    fn break_in_middle() {
        // Root→A(Public)→B(Sealed)→C(Public): B and C become sealed
        let c_resolved = leaf("c", 'c');
        let b_resolved = leaf("b", 'b').with_dependencies([(c_resolved, Visibility::PUBLIC)]);
        let a_resolved = leaf("a", 'a').with_dependencies([(b_resolved, Visibility::SEALED)]);
        let root = leaf("root", 'r').with_dependencies([(a_resolved, Visibility::PUBLIC)]);

        assert_eq!(root.dependencies.len(), 3);
        assert_dep(&root.dependencies, 0, "c", Visibility::SEALED);
        assert_dep(&root.dependencies, 1, "b", Visibility::SEALED);
        assert_dep(&root.dependencies, 2, "a", Visibility::PUBLIC);
    }

    #[test]
    fn break_at_leaf_edge() {
        // Root→A(Public)→B(Public)→C(Sealed)
        let c_resolved = leaf("c", 'c');
        let b_resolved = leaf("b", 'b').with_dependencies([(c_resolved, Visibility::SEALED)]);
        let a_resolved = leaf("a", 'a').with_dependencies([(b_resolved, Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a_resolved, Visibility::PUBLIC)]);

        assert_eq!(root.dependencies.len(), 3);
        assert_dep(&root.dependencies, 0, "c", Visibility::SEALED);
        assert_dep(&root.dependencies, 1, "b", Visibility::PUBLIC);
        assert_dep(&root.dependencies, 2, "a", Visibility::PUBLIC);
    }

    // ── Diamond — direct deps share a child ─────────────────────────

    #[test]
    fn diamond_both_paths_public() {
        // Root→A(Public)→C(Public), Root→B(Public)→C(Public)
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC), (b, Visibility::PUBLIC)]);

        assert_eq!(root.dependencies.len(), 3);
        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert_eq!(c_dep.visibility, Visibility::PUBLIC);
    }

    #[test]
    fn diamond_one_parent_sealed() {
        // Root→A(Public)→C(Public), Root→B(Sealed)→C(Public) → C=Public via A
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC), (b, Visibility::SEALED)]);

        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert_eq!(c_dep.visibility, Visibility::PUBLIC, "C should be Public via A path");
        let a_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "a")
            .unwrap();
        assert_eq!(a_dep.visibility, Visibility::PUBLIC);
        let b_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "b")
            .unwrap();
        assert_eq!(b_dep.visibility, Visibility::SEALED);
    }

    #[test]
    fn diamond_one_child_edge_sealed() {
        // Root→A(Public)→C(Sealed), Root→B(Public)→C(Public) → C=Public (merge: Sealed|Public)
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), Visibility::SEALED)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC), (b, Visibility::PUBLIC)]);

        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert_eq!(
            c_dep.visibility,
            Visibility::PUBLIC,
            "C should be Public via B path (merge semantics)"
        );
    }

    #[test]
    fn diamond_both_child_edges_sealed() {
        // Root→A(Public)→C(Sealed), Root→B(Public)→C(Sealed) → C=Sealed
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), Visibility::SEALED)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), Visibility::SEALED)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC), (b, Visibility::PUBLIC)]);

        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert_eq!(c_dep.visibility, Visibility::SEALED);
    }

    #[test]
    fn diamond_both_parents_sealed() {
        // Root→A(Sealed)→C(Public), Root→B(Sealed)→C(Public) → all Sealed
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::SEALED), (b, Visibility::SEALED)]);

        for dep in &root.dependencies {
            assert_eq!(
                dep.visibility,
                Visibility::SEALED,
                "{} should be Sealed",
                dep.identifier.repository()
            );
        }
    }

    #[test]
    fn diamond_mixed_one_parent_blocks_other_exports() {
        // Root→A(Sealed)→C(Public), Root→B(Public)→C(Public) → A=Sealed, B=Public, C=Public (via B)
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::SEALED), (b, Visibility::PUBLIC)]);

        let a_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "a")
            .unwrap();
        let b_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "b")
            .unwrap();
        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert_eq!(a_dep.visibility, Visibility::SEALED);
        assert_eq!(b_dep.visibility, Visibility::PUBLIC);
        assert_eq!(c_dep.visibility, Visibility::PUBLIC, "C Public via B path");
    }

    // ── Diamond — merge ordering ──────────────────────────────────

    #[test]
    fn diamond_merge_sealed_then_public() {
        // Root→A(Public)→C(Sealed), Root→B(Public)→C(Public): A first sets C=Sealed, B merges to Public
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), Visibility::SEALED)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC), (b, Visibility::PUBLIC)]);

        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert_eq!(
            c_dep.visibility,
            Visibility::PUBLIC,
            "C should merge from Sealed to Public via B"
        );
    }

    #[test]
    fn diamond_merge_public_then_sealed() {
        // Root→A(Public)→C(Public), Root→B(Public)→C(Sealed): A first sets C=Public, stays Public
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), Visibility::SEALED)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC), (b, Visibility::PUBLIC)]);

        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert_eq!(
            c_dep.visibility,
            Visibility::PUBLIC,
            "C should stay Public (merge with Sealed)"
        );
    }

    // ── Deep diamond — shared dep at depth > 1 ─────────────────────

    #[test]
    fn deep_diamond_shared_grandchild_one_path_exports() {
        // Root→A(Public)→B(Public)→D(Public), Root→C(Public)→D(Sealed) → D=Public via A→B
        let d1 = leaf("d", 'd');
        let b = leaf("b", 'b').with_dependencies([(d1, Visibility::PUBLIC)]);
        let a = leaf("a", 'a').with_dependencies([(b, Visibility::PUBLIC)]);

        let d2 = leaf("d", 'd');
        let c = leaf("c", 'c').with_dependencies([(d2, Visibility::SEALED)]);

        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC), (c, Visibility::PUBLIC)]);

        let d_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "d")
            .unwrap();
        assert_eq!(d_dep.visibility, Visibility::PUBLIC, "D Public via A→B path");
    }

    #[test]
    fn deep_diamond_shared_grandchild_neither_path_exports() {
        // Root→A(Public)→B(Sealed)→D(Public), Root→C(Public)→D(Sealed) → D=Sealed
        let d1 = leaf("d", 'd');
        let b = leaf("b", 'b').with_dependencies([(d1, Visibility::PUBLIC)]);
        let a = leaf("a", 'a').with_dependencies([(b, Visibility::SEALED)]);

        let d2 = leaf("d", 'd');
        let c = leaf("c", 'c').with_dependencies([(d2, Visibility::SEALED)]);

        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC), (c, Visibility::PUBLIC)]);

        let d_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "d")
            .unwrap();
        assert_eq!(d_dep.visibility, Visibility::SEALED, "D not exported via either path");
    }

    #[test]
    fn deep_diamond_shared_grandchild_both_paths_export() {
        // Root→A(Public)→B(Public)→D(Public), Root→C(Public)→D(Public) → all Public
        let d1 = leaf("d", 'd');
        let b = leaf("b", 'b').with_dependencies([(d1, Visibility::PUBLIC)]);
        let a = leaf("a", 'a').with_dependencies([(b, Visibility::PUBLIC)]);

        let d2 = leaf("d", 'd');
        let c = leaf("c", 'c').with_dependencies([(d2, Visibility::PUBLIC)]);

        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC), (c, Visibility::PUBLIC)]);

        for dep in &root.dependencies {
            assert_eq!(
                dep.visibility,
                Visibility::PUBLIC,
                "{} should be Public",
                dep.identifier.repository()
            );
        }
    }

    // ── Transitive propagation through multiple levels ──────────────

    #[test]
    fn four_level_chain_break_at_level_2() {
        // Root→A(Public)→B(Sealed)→C(Public)→D(Public) → A=Public, B=Sealed, C=Sealed, D=Sealed
        let d = leaf("d", 'd');
        let c = leaf("c", 'c').with_dependencies([(d, Visibility::PUBLIC)]);
        let b = leaf("b", 'b').with_dependencies([(c, Visibility::PUBLIC)]);
        let a = leaf("a", 'a').with_dependencies([(b, Visibility::SEALED)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC)]);

        assert_dep(&root.dependencies, 0, "d", Visibility::SEALED);
        assert_dep(&root.dependencies, 1, "c", Visibility::SEALED);
        assert_dep(&root.dependencies, 2, "b", Visibility::SEALED);
        assert_dep(&root.dependencies, 3, "a", Visibility::PUBLIC);
    }

    #[test]
    fn four_level_chain_break_at_level_3() {
        // Root→A(Public)→B(Public)→C(Sealed)→D(Public) → A=Public, B=Public, C=Sealed, D=Sealed
        let d = leaf("d", 'd');
        let c = leaf("c", 'c').with_dependencies([(d, Visibility::PUBLIC)]);
        let b = leaf("b", 'b').with_dependencies([(c, Visibility::SEALED)]);
        let a = leaf("a", 'a').with_dependencies([(b, Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC)]);

        assert_dep(&root.dependencies, 0, "d", Visibility::SEALED);
        assert_dep(&root.dependencies, 1, "c", Visibility::SEALED);
        assert_dep(&root.dependencies, 2, "b", Visibility::PUBLIC);
        assert_dep(&root.dependencies, 3, "a", Visibility::PUBLIC);
    }

    // ── Deduplication with visibility ───────────────────────────────

    #[test]
    fn diamond_dedup_preserves_topological_order_and_count() {
        // Root→A(Public)→C(Public), Root→B(Public)→C(Public) → C appears once, before A and B
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC), (b, Visibility::PUBLIC)]);

        let c_count = root
            .dependencies
            .iter()
            .filter(|d| d.identifier.repository() == "c")
            .count();
        assert_eq!(c_count, 1, "C should appear exactly once");
        // C first (dep before dependent), then A, then B
        assert_dep(&root.dependencies, 0, "c", Visibility::PUBLIC);
    }

    // ── Edge: public dep with sealed subdeps ────────────────────────

    #[test]
    fn public_parent_sealed_child() {
        // Root→A(Public)→B(Sealed) → A=Public, B=Sealed
        let a = leaf("a", 'a').with_dependencies([(leaf("b", 'b'), Visibility::SEALED)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC)]);

        assert_eq!(root.dependencies.len(), 2);
        assert_dep(&root.dependencies, 0, "b", Visibility::SEALED);
        assert_dep(&root.dependencies, 1, "a", Visibility::PUBLIC);
    }

    // ── Visibility propagation with Private/Interface ───────────────

    #[test]
    fn propagation_public_then_private_is_sealed() {
        // Root→(Public)→A→(Private)→B: Private doesn't export, so B is Sealed from Root
        let a = leaf("a", 'a').with_dependencies([(leaf("b", 'b'), Visibility::PRIVATE)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC)]);

        assert_eq!(root.dependencies.len(), 2);
        assert_dep(&root.dependencies, 0, "b", Visibility::SEALED);
        assert_dep(&root.dependencies, 1, "a", Visibility::PUBLIC);
    }

    #[test]
    fn propagation_private_then_public_is_private() {
        // Root→(Private)→A→(Public)→B: Public exports, result = edge = Private
        let a = leaf("a", 'a').with_dependencies([(leaf("b", 'b'), Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PRIVATE)]);

        assert_eq!(root.dependencies.len(), 2);
        assert_dep(&root.dependencies, 0, "b", Visibility::PRIVATE);
        assert_dep(&root.dependencies, 1, "a", Visibility::PRIVATE);
    }

    #[test]
    fn propagation_interface_then_public_is_interface() {
        // Root→(Interface)→A→(Public)→B: Public exports, result = edge = Interface
        let a = leaf("a", 'a').with_dependencies([(leaf("b", 'b'), Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::INTERFACE)]);

        assert_eq!(root.dependencies.len(), 2);
        assert_dep(&root.dependencies, 0, "b", Visibility::INTERFACE);
        assert_dep(&root.dependencies, 1, "a", Visibility::INTERFACE);
    }

    #[test]
    fn propagation_public_then_interface_is_public() {
        // Root→(Public)→A→(Interface)→B: Interface exports (consumer-visible), result = edge = Public
        let a = leaf("a", 'a').with_dependencies([(leaf("b", 'b'), Visibility::INTERFACE)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC)]);

        assert_eq!(root.dependencies.len(), 2);
        assert_dep(&root.dependencies, 0, "b", Visibility::PUBLIC);
        assert_dep(&root.dependencies, 1, "a", Visibility::PUBLIC);
    }

    #[test]
    fn diamond_merge_private_and_interface_gives_public() {
        // Two paths to C: one Private, one Interface → merge = Public (self|consumer both true)
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), Visibility::PRIVATE)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), Visibility::INTERFACE)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PUBLIC), (b, Visibility::PUBLIC)]);

        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        // Private is (self=T, consumer=F), Interface is (self=F, consumer=T)
        // merge OR on each axis: (T, T) = Public
        assert_eq!(c_dep.visibility, Visibility::PUBLIC, "Private | Interface = Public");
    }

    #[test]
    fn sealed_inside_public_inside_interface_chain() {
        // Root→(Interface)→A→(Public)→B→(Sealed)→C
        //
        // Walk from leaf upward:
        //   C@B          = Sealed (edge)
        //   C@A          = Public.through_edge(Sealed)     = Sealed (Sealed never exports)
        //   C@Root       = Interface.through_edge(Sealed)  = Sealed
        //   B@A          = Public (edge)
        //   B@Root       = Interface.through_edge(Public)  = Interface
        //   A@Root       = Interface (edge)
        //
        // C must stay Sealed under Root regardless of the outer Interface
        // wrapper, and the intermediate Public hop is what enforces it: a
        // Sealed grandchild cannot leak through Public→Interface chaining.
        let c = leaf("c", 'c');
        let b = leaf("b", 'b').with_dependencies([(c, Visibility::SEALED)]);
        let a = leaf("a", 'a').with_dependencies([(b, Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::INTERFACE)]);

        assert_eq!(root.dependencies.len(), 3);
        assert_dep(&root.dependencies, 0, "c", Visibility::SEALED);
        assert_dep(&root.dependencies, 1, "b", Visibility::INTERFACE);
        assert_dep(&root.dependencies, 2, "a", Visibility::INTERFACE);
    }

    #[test]
    fn four_level_chain_mixed_visibility() {
        // Root→(Private)→A→(Public)→B→(Interface)→C→(Public)→D
        // D's visibility from C: Interface.through_edge(Public) = Interface (Public exports, result=edge)
        // C's visibility from B: resolved as Interface edge
        // C and D from A: Private.through_edge(Interface) = Private (Interface exports, result=edge)
        //                 Private.through_edge(Interface) = Private for D too
        // From Root: Private edge, so A=Private, and transitives through A are Private
        let d = leaf("d", 'd');
        let c = leaf("c", 'c').with_dependencies([(d, Visibility::PUBLIC)]);
        let b = leaf("b", 'b').with_dependencies([(c, Visibility::INTERFACE)]);
        let a = leaf("a", 'a').with_dependencies([(b, Visibility::PUBLIC)]);
        let root = leaf("root", 'r').with_dependencies([(a, Visibility::PRIVATE)]);

        assert_eq!(root.dependencies.len(), 4);
        // Walk the chain:
        // C declares D as Public → D is Public within C's resolution
        // B declares C as Interface → Interface.through_edge(Public for D) = Interface (Public exports, result=Interface)
        //   So from B: D=Interface, C=Interface
        // A declares B as Public → Public.through_edge(Interface for D) = Public (Interface exports, result=Public)
        //   Public.through_edge(Interface for C) = Public
        //   So from A: D=Public, C=Public, B=Public
        // Root declares A as Private → Private.through_edge(Public for D) = Private
        //   So from Root: D=Private, C=Private, B=Private, A=Private
        assert_dep(&root.dependencies, 0, "d", Visibility::PRIVATE);
        assert_dep(&root.dependencies, 1, "c", Visibility::PRIVATE);
        assert_dep(&root.dependencies, 2, "b", Visibility::PRIVATE);
        assert_dep(&root.dependencies, 3, "a", Visibility::PRIVATE);
    }
}
