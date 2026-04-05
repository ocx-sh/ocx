// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;

use serde::Serialize;

use ocx_lib::{cli::TreeItem, oci};

use crate::api::Printable;

/// Whether a dependency is exported to the package environment.
///
/// `Exported` means the dependency's env vars are included in `exec`/`env`.
/// `Local` means the dependency is installed for GC/tree purposes only.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportStatus {
    Exported,
    Local,
}

impl From<bool> for ExportStatus {
    fn from(exported: bool) -> Self {
        if exported {
            ExportStatus::Exported
        } else {
            ExportStatus::Local
        }
    }
}

impl fmt::Display for ExportStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportStatus::Exported => write!(f, "exported"),
            ExportStatus::Local => write!(f, "local"),
        }
    }
}

/// A node in the dependency tree (for tree view output).
#[derive(Debug, Clone, Serialize)]
pub struct Dependency {
    pub identifier: oci::Identifier,
    pub repeated: bool,
    pub exported: bool,
    pub dependencies: Vec<Dependency>,
}

/// Tree view of the dependency graph (default output).
#[derive(Serialize)]
pub struct Dependencies {
    pub roots: Vec<Dependency>,
}

impl Dependencies {
    pub fn new(roots: Vec<Dependency>) -> Self {
        Self { roots }
    }
}

impl TreeItem for Dependency {
    fn label(&self) -> String {
        // Display identifier without digest — the digest is shown in detail().
        match self.identifier.tag() {
            Some(tag) => format!(
                "{}/{}:{}",
                self.identifier.registry(),
                self.identifier.repository(),
                tag
            ),
            None => format!("{}/{}", self.identifier.registry(), self.identifier.repository()),
        }
    }

    fn detail(&self) -> Option<String> {
        self.identifier.digest().map(|d| format!("({})", d.to_short_string()))
    }

    fn children(&self) -> &[Self] {
        if self.repeated {
            // Don't expand repeated subtrees
            &[]
        } else {
            &self.dependencies
        }
    }

    fn annotation(&self) -> Option<&str> {
        match (self.repeated, self.exported) {
            (true, false) => Some("(*) (local)"),
            (true, true) => Some("(*)"),
            (false, false) => Some("(local)"),
            (false, true) => None,
        }
    }
}

impl Printable for Dependencies {
    // Tree output is inherently non-tabular, so we use printer.print_tree()
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        for root in &self.roots {
            printer.print_tree(root);
        }
    }
}

/// Flat view of the resolved dependency order.
#[derive(Serialize)]
pub struct FlatDependencies {
    pub entries: Vec<FlatDependency>,
}

#[derive(Serialize)]
pub struct FlatDependency {
    pub identifier: oci::Identifier,
    pub exported: ExportStatus,
}

impl FlatDependencies {
    pub fn new(entries: Vec<FlatDependency>) -> Self {
        Self { entries }
    }
}

impl Printable for FlatDependencies {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            // Display identifier without digest — the digest has its own column.
            let id = &entry.identifier;
            rows[0].push(match id.tag() {
                Some(tag) => format!("{}/{}:{}", id.registry(), id.repository(), tag),
                None => format!("{}/{}", id.registry(), id.repository()),
            });
            rows[1].push(entry.exported.to_string());
            rows[2].push(id.digest().map_or_else(String::new, |d| d.to_string()));
        }
        printer.print_table(&["Package", "Exported", "Digest"], &rows);
    }
}

/// Why view — all paths from roots to a target dependency.
#[derive(Serialize)]
pub struct DependenciesTrace {
    pub paths: Vec<Vec<oci::Identifier>>,
    pub message: Option<String>,
}

impl DependenciesTrace {
    pub fn new(paths: Vec<Vec<oci::Identifier>>) -> Self {
        Self { paths, message: None }
    }
}

impl Printable for DependenciesTrace {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        if self.paths.is_empty() {
            if let Some(ref msg) = self.message {
                printer.print_hint(msg);
            } else {
                printer.print_hint("No dependency paths found.");
            }
            return;
        }
        for path in &self.paths {
            printer.print_steps(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_lib::cli::TreeItem;

    fn make_digest(hex_char: char) -> oci::Digest {
        oci::Digest::Sha256(hex_char.to_string().repeat(64))
    }

    fn make_identifier(s: &str) -> oci::Identifier {
        oci::Identifier::parse_with_default_registry(s, "ocx.sh").unwrap()
    }

    fn make_node(identifier: &str, digest: oci::Digest, repeated: bool, deps: Vec<Dependency>) -> Dependency {
        Dependency {
            identifier: make_identifier(identifier).clone_with_digest(digest),
            repeated,
            exported: true,
            dependencies: deps,
        }
    }

    #[test]
    fn tree_node_label_returns_identifier() {
        let node = make_node("ocx.sh/cmake:3.28", make_digest('a'), false, vec![]);
        assert_eq!(node.label(), "ocx.sh/cmake:3.28");
    }

    #[test]
    fn tree_node_detail_returns_short_digest() {
        let node = make_node("ocx.sh/cmake:3.28", make_digest('a'), false, vec![]);
        let detail = node.detail().unwrap();
        assert_eq!(detail, format!("({})", make_digest('a').to_short_string()));
    }

    #[test]
    fn tree_node_repeated_annotation() {
        let node = make_node("pkg", make_digest('a'), true, vec![]);
        assert_eq!(node.annotation(), Some("(*)"));
    }

    #[test]
    fn tree_node_exported_no_annotation() {
        let node = make_node("pkg", make_digest('a'), false, vec![]);
        assert_eq!(node.annotation(), None);
    }

    #[test]
    fn tree_node_local_annotation() {
        let mut node = make_node("pkg", make_digest('a'), false, vec![]);
        node.exported = false;
        assert_eq!(node.annotation(), Some("(local)"));
    }

    #[test]
    fn tree_node_repeated_and_local_shows_both() {
        let mut node = make_node("pkg", make_digest('a'), true, vec![]);
        node.exported = false;
        assert_eq!(node.annotation(), Some("(*) (local)"));
    }

    #[test]
    fn tree_node_repeated_suppresses_children() {
        let child = make_node("child", make_digest('b'), false, vec![]);
        let node = make_node("parent", make_digest('a'), true, vec![child]);
        assert!(node.children().is_empty(), "repeated node should return empty children");
    }

    #[test]
    fn leaf_node_has_empty_dependencies() {
        let node = make_node("ocx.sh/leaf", make_digest('a'), false, vec![]);
        assert!(node.dependencies.is_empty());
        assert!(!node.repeated);
    }
}
