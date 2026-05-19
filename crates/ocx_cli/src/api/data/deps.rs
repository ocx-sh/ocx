// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use ocx_lib::{
    cli::{Annotation, Cell, Theme, TreeItem},
    oci,
    package::metadata::visibility::Visibility,
};

use crate::api::Printable;

/// `registry/repo[:tag]` with the tag coloured by the theme and the
/// digest deliberately omitted (it has its own column / annotation).
fn name_tag(id: &oci::Identifier, theme: &Theme) -> String {
    let mut out = format!("{}/{}", id.registry(), id.repository());
    if let Some(tag) = id.tag() {
        out.push_str(&theme.tag(format!(":{tag}")));
    }
    out
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
    fn label(&self, theme: &Theme) -> String {
        name_tag(&self.identifier, theme)
    }

    fn children(&self) -> &[Self] {
        if self.repeated { &[] } else { &self.dependencies }
    }

    fn annotations(&self, theme: &Theme) -> Vec<Annotation> {
        // Text is pre-inked by the theme; the annotation carries no style
        // so the renderer emits it verbatim (same colour everywhere).
        // Digest is full-length and goes last so the variable-width hash
        // never pushes the short visibility / repeated tags out of eyeline.
        let mut out = Vec::new();
        if let Some(vis) = self.visibility {
            out.push(Annotation::new(theme.visibility(vis, vis.to_string())));
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
    // Tree output is inherently non-tabular, so we use printer.print_tree()
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
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
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        // Column-major. Every cell is pre-inked by the theme (same colours
        // as the tree view); cells carry no per-cell style so the renderer
        // emits the styled text verbatim and the colour-off path is
        // byte-identical to the plain form.
        let theme = printer.theme();
        let mut rows: [Vec<Cell>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            // Display identifier without digest — the digest has its own column.
            let id = &entry.identifier;
            rows[0].push(Cell::new(name_tag(id, &theme)));
            rows[1].push(Cell::new(
                theme.visibility(entry.visibility, entry.visibility.to_string()),
            ));
            rows[2].push(Cell::new(
                id.digest().map_or_else(String::new, |d| theme.digest(d.to_string())),
            ));
        }
        printer.print_table(&["Package".into(), "Visibility".into(), "Digest".into()], &rows);
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
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
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
            let steps: Vec<String> = path.iter().map(|id| theme.of(id)).collect();
            printer.print_steps(&steps);
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
