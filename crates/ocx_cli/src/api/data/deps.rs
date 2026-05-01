// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use ocx_lib::{
    cli::{Annotation, TreeItem},
    oci,
    package::metadata::visibility::Visibility,
};

use crate::api::Printable;

const STYLE_DIGEST: console::Style = console::Style::new().color256(117); // light sky blue
const STYLE_REPEATED: console::Style = console::Style::new().italic().dim();

const fn visibility_style(vis: Visibility) -> console::Style {
    match (vis.private, vis.interface) {
        (true, true) => console::Style::new().color256(114), // public — soft green
        (true, false) => console::Style::new().italic().color256(179), // private — warm amber
        (false, true) => console::Style::new().italic().color256(141), // interface — lavender
        (false, false) => console::Style::new().italic().dim().color256(245), // sealed — muted gray
    }
}

/// A node in the dependency tree (for tree view output).
///
/// `visibility` is `None` for root nodes (the packages the user asked about)
/// and `Some(v)` for dependencies, where `v` is the visibility as declared
/// by the parent — not the propagated result.
#[derive(Debug, Clone, Serialize)]
pub struct Dependency {
    pub identifier: oci::Identifier,
    pub repeated: bool,
    pub visibility: Option<Visibility>,
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

    fn children(&self) -> &[Self] {
        if self.repeated { &[] } else { &self.dependencies }
    }

    fn annotations(&self) -> Vec<Annotation> {
        let mut out = Vec::new();
        if let Some(digest) = self.identifier.digest() {
            out.push(Annotation::new(digest.to_short_string()).with_style(STYLE_DIGEST));
        }
        if let Some(vis) = self.visibility {
            out.push(Annotation::new(vis.to_string()).with_style(visibility_style(vis)));
        }
        if self.repeated {
            out.push(Annotation::new("repeated").with_style(STYLE_REPEATED));
        }
        out
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
    pub visibility: Visibility,
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
            rows[1].push(entry.visibility.to_string());
            rows[2].push(id.digest().map_or_else(String::new, |d| d.to_string()));
        }
        printer.print_table(&["Package", "Visibility", "Digest"], &rows);
    }
}

/// Why view — all paths from roots to a target dependency.
#[derive(Serialize)]
pub struct DependenciesTrace {
    pub paths: Vec<Vec<oci::Identifier>>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
    use ocx_lib::package::metadata::visibility::Visibility;

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
            visibility: Some(Visibility::PUBLIC),
            dependencies: deps,
        }
    }

    #[test]
    fn tree_node_label_returns_identifier() {
        let node = make_node("ocx.sh/cmake:3.28", make_digest('a'), false, vec![]);
        assert_eq!(node.label(), "ocx.sh/cmake:3.28");
    }

    fn annotation_texts(node: &Dependency) -> Vec<String> {
        node.annotations().into_iter().map(|a| a.text.into_owned()).collect()
    }

    #[test]
    fn tree_node_digest_annotation() {
        let node = make_node("ocx.sh/cmake:3.28", make_digest('a'), false, vec![]);
        let texts = annotation_texts(&node);
        assert_eq!(texts[0], make_digest('a').to_short_string());
    }

    #[test]
    fn tree_root_only_digest_annotation() {
        let mut node = make_node("pkg", make_digest('a'), false, vec![]);
        node.visibility = None;
        let texts = annotation_texts(&node);
        assert_eq!(texts.len(), 1, "root node should only have digest");
        assert_eq!(texts[0], make_digest('a').to_short_string());
    }

    #[test]
    fn tree_node_public_annotation() {
        let node = make_node("pkg", make_digest('a'), false, vec![]);
        let texts = annotation_texts(&node);
        assert!(texts.contains(&make_digest('a').to_short_string()));
        assert!(texts.contains(&"public".to_string()));
    }

    #[test]
    fn tree_node_repeated_public_has_three_annotations() {
        let node = make_node("pkg", make_digest('a'), true, vec![]);
        let texts = annotation_texts(&node);
        assert!(texts.contains(&make_digest('a').to_short_string()));
        assert!(texts.contains(&"repeated".to_string()));
        assert!(texts.contains(&"public".to_string()));
    }

    #[test]
    fn tree_node_sealed_annotation() {
        let mut node = make_node("pkg", make_digest('a'), false, vec![]);
        node.visibility = Some(Visibility::SEALED);
        let texts = annotation_texts(&node);
        assert!(texts.contains(&"sealed".to_string()));
    }

    #[test]
    fn tree_node_repeated_and_sealed_shows_both() {
        let mut node = make_node("pkg", make_digest('a'), true, vec![]);
        node.visibility = Some(Visibility::SEALED);
        let texts = annotation_texts(&node);
        assert!(texts.contains(&"repeated".to_string()));
        assert!(texts.contains(&"sealed".to_string()));
    }

    #[test]
    fn tree_node_private_annotation() {
        let mut node = make_node("pkg", make_digest('a'), false, vec![]);
        node.visibility = Some(Visibility::PRIVATE);
        assert!(annotation_texts(&node).contains(&"private".to_string()));
    }

    #[test]
    fn tree_node_interface_annotation() {
        let mut node = make_node("pkg", make_digest('a'), false, vec![]);
        node.visibility = Some(Visibility::INTERFACE);
        assert!(annotation_texts(&node).contains(&"interface".to_string()));
    }

    #[test]
    fn tree_node_repeated_and_private_shows_both() {
        let mut node = make_node("pkg", make_digest('a'), true, vec![]);
        node.visibility = Some(Visibility::PRIVATE);
        let texts = annotation_texts(&node);
        assert!(texts.contains(&"repeated".to_string()));
        assert!(texts.contains(&"private".to_string()));
    }

    #[test]
    fn annotations_order_is_digest_visibility_repeated() {
        let node = make_node("pkg", make_digest('a'), true, vec![]);
        let texts = annotation_texts(&node);
        assert_eq!(texts.len(), 3);
        assert_eq!(texts[0], make_digest('a').to_short_string());
        assert_eq!(texts[1], "public");
        assert_eq!(texts[2], "repeated");
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
