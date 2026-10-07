// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use ocx_console::{Annotation, Cell, Theme, TreeItem};
use ocx_package::metadata::visibility::Visibility;

use crate::api::Printable;

/// `registry/repo[:tag]` with the tag coloured; the digest has its own column.
fn name_tag(id: &ocx_oci::PackageRef, theme: &Theme) -> String {
    let mut out = format!("{}/{}", id.registry(), id.repository());
    if let Some(tag) = id.tag() {
        out.push_str(&theme.tag(format!(":{tag}")));
    }
    out
}

/// A node in the dependency tree (for tree view output).
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[schemars(rename = "DependencyNode")]
pub struct Dependency {
    /// The package, digest-pinned.
    pub identifier: ocx_oci::PackageRef,
    /// Whether this package already appeared earlier in the tree; its dependencies are listed there.
    pub repeated: bool,
    /// The visibility the parent declared for this edge, not the propagated result; absent on a root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    /// This package's own dependencies.
    pub dependencies: Vec<Dependency>,
}

/// Tree view of the dependency graph (default output).
#[derive(Serialize, schemars::JsonSchema)]
#[schemars(rename = "DependencyTree")]
pub struct Dependencies {
    /// One tree per requested package, in request order.
    pub items: Vec<Dependency>,
}

impl Dependencies {
    pub fn new(items: Vec<Dependency>) -> Self {
        Self { items }
    }
}

impl TreeItem for Dependency {
    fn label(&self, theme: &Theme) -> String {
        name_tag(&self.identifier, theme)
    }

    fn children(&self) -> &[Self] {
        if self.repeated { &[] } else { &self.dependencies }
    }

    fn annotations(&self, theme: &Theme) -> Vec<Annotation> {
        // Digest last, or the full-length hash pushes the short tags out of eyeline.
        let mut out = Vec::new();
        if let Some(vis) = self.visibility {
            out.push(Annotation::new(
                theme.visibility(crate::api::data::visibility_style(vis), vis.to_string()),
            ));
        }
        if self.repeated {
            out.push(Annotation::new(theme.repeated("repeated")));
        }
        if let Some(digest) = self.identifier.digest() {
            out.push(Annotation::new(theme.digest(digest.to_string())));
        }
        out
    }
}

impl Printable for Dependencies {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "Dependencies";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        for root in &self.items {
            printer.print_tree(root);
        }
    }
}

/// Flat view of the resolved dependency order.
#[derive(Serialize, schemars::JsonSchema)]
pub struct FlatDependencies {
    /// Every dependency in resolution order.
    pub items: Vec<FlatDependency>,
}

/// One dependency in the flat view.
#[derive(Serialize, schemars::JsonSchema)]
pub struct FlatDependency {
    /// The package, digest-pinned.
    pub identifier: ocx_oci::PackageRef,
    /// The dependency's resolved visibility; a requested root is `public`.
    pub visibility: Visibility,
}

impl FlatDependencies {
    pub fn new(items: Vec<FlatDependency>) -> Self {
        Self { items }
    }
}

impl Printable for FlatDependencies {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "FlatDependencies";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        // Cells are pre-inked and carry no style of their own, so colour-off output is byte-identical.
        let theme = printer.theme();
        let mut rows: [Vec<Cell>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.items {
            let id = &entry.identifier;
            rows[0].push(Cell::new(name_tag(id, &theme)));
            rows[1].push(Cell::new(theme.visibility(
                crate::api::data::visibility_style(entry.visibility),
                entry.visibility.to_string(),
            )));
            rows[2].push(Cell::new(
                id.digest().map_or_else(String::new, |d| theme.digest(d.to_string())),
            ));
        }
        printer.print_table(&["Package".into(), "Visibility".into(), "Digest".into()], &rows);
    }
}

/// Why view — all paths from roots to a target dependency.
#[derive(Serialize, schemars::JsonSchema)]
pub struct DependenciesTrace {
    /// Every path from a requested root to the target, root first.
    pub paths: Vec<Vec<ocx_oci::PackageRef>>,
    /// Why no path was found; present only when `paths` is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl DependenciesTrace {
    pub fn new(paths: Vec<Vec<ocx_oci::PackageRef>>) -> Self {
        Self { paths, message: None }
    }
}

impl Printable for DependenciesTrace {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "DependenciesTrace";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        if self.paths.is_empty() {
            if let Some(ref msg) = self.message {
                printer.print_hint(msg);
            } else {
                printer.print_hint("No dependency paths found.");
            }
            return;
        }
        let theme = printer.theme();
        for path in &self.paths {
            let steps: Vec<String> = path
                .iter()
                .map(|id| crate::api::data::ink_identifier(&theme, id))
                .collect();
            printer.print_steps(&steps);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_console::TreeItem;
    use ocx_package::metadata::visibility::Visibility;

    fn make_digest(hex_char: char) -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256(hex_char.to_string().repeat(64))
    }

    fn make_identifier(s: &str) -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::parse_with_default_registry(s, "ocx.sh").unwrap()
    }

    fn make_node(identifier: &str, digest: ocx_oci::Digest, repeated: bool, deps: Vec<Dependency>) -> Dependency {
        Dependency {
            identifier: make_identifier(identifier).clone_with_digest(digest),
            repeated,
            visibility: Some(Visibility::PUBLIC),
            dependencies: deps,
        }
    }

    // Colour off: pre-inked text equals the plain form, so these
    // assertions exercise composition without ANSI noise.
    fn theme() -> Theme {
        Theme::new(false)
    }

    #[test]
    fn tree_node_label_returns_identifier() {
        let node = make_node("ocx.sh/cmake:3.28", make_digest('a'), false, vec![]);
        assert_eq!(node.label(&theme()), "ocx.sh/cmake:3.28");
    }

    fn annotation_texts(node: &Dependency) -> Vec<String> {
        node.annotations(&theme())
            .into_iter()
            .map(|a| a.text.into_owned())
            .collect()
    }

    #[test]
    fn tree_node_digest_annotation() {
        let node = make_node("ocx.sh/cmake:3.28", make_digest('a'), false, vec![]);
        let texts = annotation_texts(&node);
        // Digest is full-length and the last annotation.
        assert_eq!(*texts.last().unwrap(), make_digest('a').to_string());
    }

    #[test]
    fn tree_root_only_digest_annotation() {
        let mut node = make_node("pkg", make_digest('a'), false, vec![]);
        node.visibility = None;
        let texts = annotation_texts(&node);
        assert_eq!(texts.len(), 1, "root node should only have digest");
        assert_eq!(texts[0], make_digest('a').to_string());
    }

    #[test]
    fn tree_node_public_annotation() {
        let node = make_node("pkg", make_digest('a'), false, vec![]);
        let texts = annotation_texts(&node);
        assert!(texts.contains(&make_digest('a').to_string()));
        assert!(texts.contains(&"public".to_string()));
    }

    #[test]
    fn tree_node_repeated_public_has_three_annotations() {
        let node = make_node("pkg", make_digest('a'), true, vec![]);
        let texts = annotation_texts(&node);
        assert!(texts.contains(&make_digest('a').to_string()));
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
    fn annotations_order_is_visibility_repeated_digest() {
        let node = make_node("pkg", make_digest('a'), true, vec![]);
        let texts = annotation_texts(&node);
        assert_eq!(texts.len(), 3);
        assert_eq!(texts[0], "public");
        assert_eq!(texts[1], "repeated");
        assert_eq!(texts[2], make_digest('a').to_string());
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
