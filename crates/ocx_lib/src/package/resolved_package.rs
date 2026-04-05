// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use crate::oci::PinnedIdentifier;

/// A dependency in the transitive closure with its pre-computed export flag.
///
/// `exported` is `true` when every edge on at least one path from the root
/// to this dependency has `export: true`. Diamond deps use OR semantics:
/// if ANY all-exported path reaches a dep, it is exported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedDependency {
    pub identifier: PinnedIdentifier,
    pub exported: bool,
}

/// Persisted resolution state for an installed package.
///
/// Written to `resolve.json` in each object directory at install time.
/// Contains the package's own fully-resolved identifier and its transitive
/// dependency closure in topological order (deps before dependents).
///
/// The `identifier` is always the **platform-resolved** identifier — for
/// multi-platform packages this is the platform-specific manifest digest,
/// never the Image Index digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedPackage {
    /// The fully resolved identifier of this package (registry/repo:tag@digest).
    pub identifier: PinnedIdentifier,

    /// Transitive dependency closure with pre-computed export flags.
    /// Deps before dependents. The root package itself is **not** included.
    /// Leaf packages (no dependencies) have an empty vec.
    pub dependencies: Vec<ResolvedDependency>,
}

impl ResolvedPackage {
    /// Creates a leaf package with no dependencies.
    pub fn new(identifier: PinnedIdentifier) -> Self {
        Self {
            identifier,
            dependencies: Vec::new(),
        }
    }

    /// Builds the transitive dependency closure from resolved direct deps.
    ///
    /// Each item is `(child_resolved, edge_export)` where `edge_export` is the
    /// parent's declared export flag for this edge. Propagation rule: a
    /// transitive dep is exported if `edge_export && transitive.exported`.
    /// Diamond deps use OR semantics — if any all-exported path reaches a dep,
    /// the final flag is `true`.
    ///
    /// Preserves topological order (deps before dependents) and deduplicates
    /// by identity (advisory tags stripped).
    pub fn with_dependencies(mut self, deps: impl IntoIterator<Item = (ResolvedPackage, bool)>) -> Self {
        // Maps stripped identity → index in self.dependencies for OR dedup.
        let mut seen: std::collections::HashMap<PinnedIdentifier, usize> = std::collections::HashMap::new();

        for (dep, edge_export) in deps {
            // Bubble up transitive deps first (preserves topological order).
            for transitive in dep.dependencies {
                let propagated = edge_export && transitive.exported;
                let key = transitive.identifier.strip_advisory();
                if let Some(&idx) = seen.get(&key) {
                    // OR semantics: flip false→true if this path exports.
                    self.dependencies[idx].exported |= propagated;
                } else {
                    let idx = self.dependencies.len();
                    seen.insert(key, idx);
                    self.dependencies.push(ResolvedDependency {
                        identifier: transitive.identifier,
                        exported: propagated,
                    });
                }
            }

            // Then add the direct dep itself.
            let key = dep.identifier.strip_advisory();
            if let Some(&idx) = seen.get(&key) {
                self.dependencies[idx].exported |= edge_export;
            } else {
                let idx = self.dependencies.len();
                seen.insert(key, idx);
                self.dependencies.push(ResolvedDependency {
                    identifier: dep.identifier,
                    exported: edge_export,
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

    /// Helper: build a ResolvedPackage leaf with the given export edge flag.
    fn leaf(repo: &str, hex: char) -> ResolvedPackage {
        ResolvedPackage::new(make_pinned_repo(repo, hex))
    }

    /// Helper: assert a dep at index has the expected repo and export flag.
    fn assert_dep(deps: &[ResolvedDependency], idx: usize, repo: &str, exported: bool) {
        assert_eq!(deps[idx].identifier.repository(), repo, "dep[{idx}] repo mismatch");
        assert_eq!(deps[idx].exported, exported, "dep[{idx}] ({repo}) exported mismatch");
    }

    // ── Serialization tests ─────────────────────────────────────────

    #[test]
    fn serde_roundtrip_exported_true() {
        let dep = ResolvedDependency {
            identifier: make_dep_pinned(),
            exported: true,
        };
        let pkg = ResolvedPackage {
            identifier: make_pinned(),
            dependencies: vec![dep.clone()],
        };
        let json = serde_json::to_string(&pkg).unwrap();
        let deserialized: ResolvedPackage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.dependencies.len(), 1);
        assert_eq!(deserialized.dependencies[0], dep);
    }

    #[test]
    fn serde_roundtrip_exported_false() {
        let dep = ResolvedDependency {
            identifier: make_dep_pinned(),
            exported: false,
        };
        let pkg = ResolvedPackage {
            identifier: make_pinned(),
            dependencies: vec![dep.clone()],
        };
        let json = serde_json::to_string(&pkg).unwrap();
        let deserialized: ResolvedPackage = serde_json::from_str(&json).unwrap();
        assert!(!deserialized.dependencies[0].exported);
    }

    #[test]
    fn serde_roundtrip_mixed_flags() {
        let deps = vec![
            ResolvedDependency {
                identifier: make_dep_pinned(),
                exported: true,
            },
            ResolvedDependency {
                identifier: make_pinned_repo("other", 'c'),
                exported: false,
            },
        ];
        let pkg = ResolvedPackage {
            identifier: make_pinned(),
            dependencies: deps.clone(),
        };
        let json = serde_json::to_string(&pkg).unwrap();
        let deserialized: ResolvedPackage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.dependencies, deps);
    }

    #[test]
    fn serde_leaf_package_empty_dependencies() {
        let pkg = ResolvedPackage {
            identifier: make_pinned(),
            dependencies: vec![],
        };
        let json = serde_json::to_string(&pkg).unwrap();
        let deserialized: ResolvedPackage = serde_json::from_str(&json).unwrap();
        assert!(deserialized.dependencies.is_empty());
    }

    #[test]
    fn deserialize_rejects_missing_identifier() {
        let json = r#"{"dependencies": []}"#;
        let result = serde_json::from_str::<ResolvedPackage>(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_missing_dependencies() {
        let id = make_pinned();
        let json = format!(r#"{{"identifier": "{}"}}"#, id);
        let result = serde_json::from_str::<ResolvedPackage>(&json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_old_format_bare_strings() {
        let id = make_pinned();
        let dep = make_dep_pinned();
        let json = format!(r#"{{"identifier":"{}","dependencies":["{}"]}}"#, id, dep);
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
    fn single_exported_dep() {
        let resolved = leaf("root", 'a').with_dependencies([(leaf("x", 'b'), true)]);
        assert_eq!(resolved.dependencies.len(), 1);
        assert_dep(&resolved.dependencies, 0, "x", true);
    }

    #[test]
    fn single_non_exported_dep() {
        let resolved = leaf("root", 'a').with_dependencies([(leaf("x", 'b'), false)]);
        assert_eq!(resolved.dependencies.len(), 1);
        assert_dep(&resolved.dependencies, 0, "x", false);
    }

    #[test]
    fn all_exported_chain() {
        // Root→A(T)→B(T)→C(T)
        let c_resolved = leaf("c", 'c').with_dependencies([]);
        let b_resolved = leaf("b", 'b').with_dependencies([(c_resolved, true)]);
        let a_resolved = leaf("a", 'a').with_dependencies([(b_resolved, true)]);
        let root = leaf("root", 'r').with_dependencies([(a_resolved, true)]);

        assert_eq!(root.dependencies.len(), 3);
        assert_dep(&root.dependencies, 0, "c", true);
        assert_dep(&root.dependencies, 1, "b", true);
        assert_dep(&root.dependencies, 2, "a", true);
    }

    #[test]
    fn break_at_root_edge() {
        // Root→A(F)→B(T)→C(T): all become non-exported
        let c_resolved = leaf("c", 'c');
        let b_resolved = leaf("b", 'b').with_dependencies([(c_resolved, true)]);
        let a_resolved = leaf("a", 'a').with_dependencies([(b_resolved, true)]);
        let root = leaf("root", 'r').with_dependencies([(a_resolved, false)]);

        assert_eq!(root.dependencies.len(), 3);
        assert_dep(&root.dependencies, 0, "c", false);
        assert_dep(&root.dependencies, 1, "b", false);
        assert_dep(&root.dependencies, 2, "a", false);
    }

    #[test]
    fn break_in_middle() {
        // Root→A(T)→B(F)→C(T): B and C become non-exported
        let c_resolved = leaf("c", 'c');
        let b_resolved = leaf("b", 'b').with_dependencies([(c_resolved, true)]);
        let a_resolved = leaf("a", 'a').with_dependencies([(b_resolved, false)]);
        let root = leaf("root", 'r').with_dependencies([(a_resolved, true)]);

        assert_eq!(root.dependencies.len(), 3);
        assert_dep(&root.dependencies, 0, "c", false);
        assert_dep(&root.dependencies, 1, "b", false);
        assert_dep(&root.dependencies, 2, "a", true);
    }

    #[test]
    fn break_at_leaf_edge() {
        // Root→A(T)→B(T)→C(F)
        let c_resolved = leaf("c", 'c');
        let b_resolved = leaf("b", 'b').with_dependencies([(c_resolved, false)]);
        let a_resolved = leaf("a", 'a').with_dependencies([(b_resolved, true)]);
        let root = leaf("root", 'r').with_dependencies([(a_resolved, true)]);

        assert_eq!(root.dependencies.len(), 3);
        assert_dep(&root.dependencies, 0, "c", false);
        assert_dep(&root.dependencies, 1, "b", true);
        assert_dep(&root.dependencies, 2, "a", true);
    }

    // ── Diamond — direct deps share a child ─────────────────────────

    #[test]
    fn diamond_both_paths_export_child() {
        // Root→A(T)→C(T), Root→B(T)→C(T)
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), true)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), true)]);
        let root = leaf("root", 'r').with_dependencies([(a, true), (b, true)]);

        assert_eq!(root.dependencies.len(), 3);
        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert!(c_dep.exported);
    }

    #[test]
    fn diamond_one_parent_not_exported() {
        // Root→A(T)→C(T), Root→B(F)→C(T) → C=T via A
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), true)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), true)]);
        let root = leaf("root", 'r').with_dependencies([(a, true), (b, false)]);

        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert!(c_dep.exported, "C should be exported via A path");
        let a_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "a")
            .unwrap();
        assert!(a_dep.exported);
        let b_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "b")
            .unwrap();
        assert!(!b_dep.exported);
    }

    #[test]
    fn diamond_one_child_edge_not_exported() {
        // Root→A(T)→C(F), Root→B(T)→C(T) → C=T (OR: F via A, T via B)
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), false)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), true)]);
        let root = leaf("root", 'r').with_dependencies([(a, true), (b, true)]);

        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert!(c_dep.exported, "C should be exported via B path (OR semantics)");
    }

    #[test]
    fn diamond_both_child_edges_not_exported() {
        // Root→A(T)→C(F), Root→B(T)→C(F) → C=F
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), false)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), false)]);
        let root = leaf("root", 'r').with_dependencies([(a, true), (b, true)]);

        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert!(!c_dep.exported);
    }

    #[test]
    fn diamond_both_parents_not_exported() {
        // Root→A(F)→C(T), Root→B(F)→C(T) → all F
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), true)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), true)]);
        let root = leaf("root", 'r').with_dependencies([(a, false), (b, false)]);

        for dep in &root.dependencies {
            assert!(!dep.exported, "{} should not be exported", dep.identifier.repository());
        }
    }

    #[test]
    fn diamond_mixed_one_parent_blocks_other_exports() {
        // Root→A(F)→C(T), Root→B(T)→C(T) → A=F, B=T, C=T (via B)
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), true)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), true)]);
        let root = leaf("root", 'r').with_dependencies([(a, false), (b, true)]);

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
        assert!(!a_dep.exported);
        assert!(b_dep.exported);
        assert!(c_dep.exported, "C exported via B path");
    }

    // ── Diamond — OR flip ordering ──────────────────────────────────

    #[test]
    fn diamond_or_flip_false_then_true() {
        // Root→A(T)→C(F), Root→B(T)→C(T): A first sets C=F, B flips to T
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), false)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), true)]);
        let root = leaf("root", 'r').with_dependencies([(a, true), (b, true)]);

        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert!(c_dep.exported, "C should flip from false to true via B");
    }

    #[test]
    fn diamond_or_flip_true_then_false() {
        // Root→A(T)→C(T), Root→B(T)→C(F): A first sets C=T, stays T
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), true)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), false)]);
        let root = leaf("root", 'r').with_dependencies([(a, true), (b, true)]);

        let c_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "c")
            .unwrap();
        assert!(c_dep.exported, "C should stay true (OR with false)");
    }

    // ── Deep diamond — shared dep at depth > 1 ─────────────────────

    #[test]
    fn deep_diamond_shared_grandchild_one_path_exports() {
        // Root→A(T)→B(T)→D(T), Root→C(T)→D(F) → D=T via A→B
        let d1 = leaf("d", 'd');
        let b = leaf("b", 'b').with_dependencies([(d1, true)]);
        let a = leaf("a", 'a').with_dependencies([(b, true)]);

        let d2 = leaf("d", 'd');
        let c = leaf("c", 'c').with_dependencies([(d2, false)]);

        let root = leaf("root", 'r').with_dependencies([(a, true), (c, true)]);

        let d_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "d")
            .unwrap();
        assert!(d_dep.exported, "D exported via A→B path");
    }

    #[test]
    fn deep_diamond_shared_grandchild_neither_path_exports() {
        // Root→A(T)→B(F)→D(T), Root→C(T)→D(F) → D=F
        let d1 = leaf("d", 'd');
        let b = leaf("b", 'b').with_dependencies([(d1, true)]);
        let a = leaf("a", 'a').with_dependencies([(b, false)]);

        let d2 = leaf("d", 'd');
        let c = leaf("c", 'c').with_dependencies([(d2, false)]);

        let root = leaf("root", 'r').with_dependencies([(a, true), (c, true)]);

        let d_dep = root
            .dependencies
            .iter()
            .find(|d| d.identifier.repository() == "d")
            .unwrap();
        assert!(!d_dep.exported, "D not exported via either path");
    }

    #[test]
    fn deep_diamond_shared_grandchild_both_paths_export() {
        // Root→A(T)→B(T)→D(T), Root→C(T)→D(T) → all T
        let d1 = leaf("d", 'd');
        let b = leaf("b", 'b').with_dependencies([(d1, true)]);
        let a = leaf("a", 'a').with_dependencies([(b, true)]);

        let d2 = leaf("d", 'd');
        let c = leaf("c", 'c').with_dependencies([(d2, true)]);

        let root = leaf("root", 'r').with_dependencies([(a, true), (c, true)]);

        for dep in &root.dependencies {
            assert!(dep.exported, "{} should be exported", dep.identifier.repository());
        }
    }

    // ── Transitive propagation through multiple levels ──────────────

    #[test]
    fn four_level_chain_break_at_level_2() {
        // Root→A(T)→B(F)→C(T)→D(T) → A=T, B=F, C=F, D=F
        let d = leaf("d", 'd');
        let c = leaf("c", 'c').with_dependencies([(d, true)]);
        let b = leaf("b", 'b').with_dependencies([(c, true)]);
        let a = leaf("a", 'a').with_dependencies([(b, false)]);
        let root = leaf("root", 'r').with_dependencies([(a, true)]);

        assert_dep(&root.dependencies, 0, "d", false);
        assert_dep(&root.dependencies, 1, "c", false);
        assert_dep(&root.dependencies, 2, "b", false);
        assert_dep(&root.dependencies, 3, "a", true);
    }

    #[test]
    fn four_level_chain_break_at_level_3() {
        // Root→A(T)→B(T)→C(F)→D(T) → A=T, B=T, C=F, D=F
        let d = leaf("d", 'd');
        let c = leaf("c", 'c').with_dependencies([(d, true)]);
        let b = leaf("b", 'b').with_dependencies([(c, false)]);
        let a = leaf("a", 'a').with_dependencies([(b, true)]);
        let root = leaf("root", 'r').with_dependencies([(a, true)]);

        assert_dep(&root.dependencies, 0, "d", false);
        assert_dep(&root.dependencies, 1, "c", false);
        assert_dep(&root.dependencies, 2, "b", true);
        assert_dep(&root.dependencies, 3, "a", true);
    }

    // ── Deduplication with export ───────────────────────────────────

    #[test]
    fn diamond_dedup_preserves_topological_order_and_count() {
        // Root→A(T)→C(T), Root→B(T)→C(T) → C appears once, before A and B
        let a = leaf("a", 'a').with_dependencies([(leaf("c", 'c'), true)]);
        let b = leaf("b", 'b').with_dependencies([(leaf("c", 'c'), true)]);
        let root = leaf("root", 'r').with_dependencies([(a, true), (b, true)]);

        let c_count = root
            .dependencies
            .iter()
            .filter(|d| d.identifier.repository() == "c")
            .count();
        assert_eq!(c_count, 1, "C should appear exactly once");
        // C first (dep before dependent), then A, then B
        assert_dep(&root.dependencies, 0, "c", true);
    }

    // ── Edge: exported dep with non-exported subdeps ────────────────

    #[test]
    fn exported_parent_non_exported_child() {
        // Root→A(T)→B(F) → A=T, B=F
        let a = leaf("a", 'a').with_dependencies([(leaf("b", 'b'), false)]);
        let root = leaf("root", 'r').with_dependencies([(a, true)]);

        assert_eq!(root.dependencies.len(), 2);
        assert_dep(&root.dependencies, 0, "b", false);
        assert_dep(&root.dependencies, 1, "a", true);
    }
}
