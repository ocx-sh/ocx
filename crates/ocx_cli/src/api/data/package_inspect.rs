// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Serialize, ser::SerializeStruct};

use ocx_lib::{
    cli::{Annotation, DataInterface, Style, TreeItem},
    oci,
    package::metadata::{Metadata, env::modifier::ModifierKind},
    package_manager::{InspectResult, ResolvedChain},
};

use crate::api::Printable;

const STYLE_DIGEST: Style = Style::new().style(console::Style::new().color256(117)); // light sky blue
const STYLE_DETAIL: Style = Style::new().style(console::Style::new().dim());

/// Read-only view of a package. The shape adapts to the requested reference
/// and whether `--resolve` was given:
///
/// - **candidates** (default mode, ref is an image index): the available
///   platform children — `{ identifier, pinned_digest, candidates: [...] }`.
/// - **metadata** (default mode, ref is a single manifest): the declared
///   metadata document — `{ identifier, pinned_digest, metadata }`.
/// - **resolution** (`--resolve`): platform-selected metadata plus the OCI
///   resolution chain — `{ identifier, pinned_digest, platforms, metadata,
///   resolution }`.
///
/// Plain format: a tree rooted at the pinned identifier with the
/// shape-appropriate section(s).
pub struct PackageInspect {
    identifier: oci::Identifier,
    pinned_digest: String,
    body: Body,
}

enum Body {
    Candidates {
        pinned: String,
        candidates: Vec<CandidateOut>,
    },
    Manifest {
        pinned: String,
        metadata: Metadata,
    },
    Resolved {
        pinned: String,
        platforms: Vec<oci::Platform>,
        metadata: Metadata,
        resolution: Resolution,
    },
}

/// One platform child of an image index.
#[derive(Serialize)]
struct CandidateOut {
    digest: String,
    platform: String,
    media_type: String,
    size: i64,
}

/// The OCI resolution chain for the selected platform.
#[derive(Serialize)]
struct Resolution {
    pinned: String,
    chain: Vec<String>,
    layers: Vec<Layer>,
}

/// A single layer descriptor from the platform-selected manifest.
#[derive(Serialize)]
struct Layer {
    digest: String,
    media_type: String,
    size: i64,
}

impl PackageInspect {
    /// Builds the report from the task result. `identifier` is the requested
    /// identifier (post default-registry expansion); `platforms` is the
    /// platform list resolution considered (only meaningful in `--resolve`
    /// mode).
    pub fn new(identifier: oci::Identifier, platforms: Vec<oci::Platform>, result: InspectResult) -> Self {
        match result {
            InspectResult::Candidates { pinned, candidates } => Self {
                identifier,
                pinned_digest: pinned.digest().to_string(),
                body: Body::Candidates {
                    pinned: pinned.to_string(),
                    candidates: candidates
                        .into_iter()
                        .map(|c| CandidateOut {
                            digest: c.identifier.digest().to_string(),
                            platform: c.platform.to_string(),
                            media_type: c.media_type,
                            size: c.size,
                        })
                        .collect(),
                },
            },
            InspectResult::Manifest { pinned, metadata } => Self {
                identifier,
                pinned_digest: pinned.digest().to_string(),
                body: Body::Manifest {
                    pinned: pinned.to_string(),
                    metadata: metadata.into(),
                },
            },
            InspectResult::Resolved {
                pinned,
                metadata,
                chain,
            } => Self {
                identifier,
                pinned_digest: pinned.digest().to_string(),
                body: Body::Resolved {
                    pinned: pinned.to_string(),
                    platforms,
                    metadata: metadata.into(),
                    resolution: Resolution::from_chain(&chain),
                },
            },
        }
    }
}

impl Resolution {
    fn from_chain(chain: &ResolvedChain) -> Self {
        Self {
            pinned: chain.pinned.to_string(),
            chain: chain.chain.iter().map(|p| p.digest().to_string()).collect(),
            layers: chain
                .final_manifest
                .layers
                .iter()
                .map(|d| Layer {
                    digest: d.digest.clone(),
                    media_type: d.media_type.clone(),
                    size: d.size,
                })
                .collect(),
        }
    }
}

impl Serialize for PackageInspect {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Field count varies by body shape; identifier + pinned_digest are
        // always present.
        let len = 2 + match &self.body {
            Body::Candidates { .. } => 1,
            Body::Manifest { .. } => 1,
            Body::Resolved { .. } => 3,
        };
        let mut s = serializer.serialize_struct("PackageInspect", len)?;
        s.serialize_field("identifier", &self.identifier)?;
        s.serialize_field("pinned_digest", &self.pinned_digest)?;
        match &self.body {
            Body::Candidates { candidates, .. } => {
                s.serialize_field("candidates", candidates)?;
            }
            Body::Manifest { metadata, .. } => {
                s.serialize_field("metadata", metadata)?;
            }
            Body::Resolved {
                platforms,
                metadata,
                resolution,
                ..
            } => {
                s.serialize_field("platforms", platforms)?;
                s.serialize_field("metadata", metadata)?;
                s.serialize_field("resolution", resolution)?;
            }
        }
        s.end()
    }
}

/// A plain-text tree node. Built only for `print_plain`; the JSON path uses
/// the `Serialize` impls above.
struct Node {
    label: String,
    annotations: Vec<Annotation>,
    children: Vec<Node>,
}

impl Node {
    fn leaf(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            annotations: Vec::new(),
            children: Vec::new(),
        }
    }

    fn branch(label: impl Into<String>, children: Vec<Node>) -> Self {
        Self {
            label: label.into(),
            annotations: Vec::new(),
            children,
        }
    }

    fn with_annotation(mut self, annotation: Annotation) -> Self {
        self.annotations.push(annotation);
        self
    }
}

impl TreeItem for Node {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn children(&self) -> &[Self] {
        &self.children
    }

    fn annotations(&self) -> Vec<Annotation> {
        self.annotations
            .iter()
            .map(|a| match a.style.clone() {
                Some(style) => Annotation::new(a.text.clone()).with_style(style),
                None => Annotation::new(a.text.clone()),
            })
            .collect()
    }
}

fn metadata_node(metadata: &Metadata) -> Node {
    let mut children = vec![Node::leaf(format!("version {}", metadata.version() as u8))];
    if let Some(strip) = metadata.strip_components() {
        children.push(Node::leaf(format!("strip_components {strip}")));
    }

    if let Some(env) = metadata.env()
        && !env.is_empty()
    {
        let vars = env
            .into_iter()
            .map(|var| {
                let kind = ModifierKind::from(&var.modifier);
                let mut node = Node::leaf(var.key.clone())
                    .with_annotation(Annotation::new(kind.to_string()).with_style(STYLE_DETAIL))
                    .with_annotation(Annotation::new(var.visibility.to_string()).with_style(STYLE_DETAIL));
                if let Some(value) = var.value() {
                    node = node.with_annotation(Annotation::new(value.to_string()));
                }
                node
            })
            .collect();
        children.push(Node::branch("env", vars));
    }

    let deps = metadata.dependencies();
    if !deps.is_empty() {
        let dep_nodes = deps
            .iter()
            .map(|dep| {
                Node::leaf(dep.name().to_string())
                    .with_annotation(Annotation::new(dep.identifier.to_string()).with_style(STYLE_DIGEST))
            })
            .collect();
        children.push(Node::branch("dependencies", dep_nodes));
    }

    if let Some(entrypoints) = metadata.entrypoints()
        && !entrypoints.is_empty()
    {
        let names = entrypoints
            .iter()
            .map(|(name, entry)| {
                let node = Node::leaf(name.to_string());
                // Annotate the dispatch command only when it diverges from
                // the invocable name — no noise for the common case where
                // they coincide.
                match entry.command() {
                    Some(cmd) if cmd.as_str() != name.as_str() => {
                        node.with_annotation(Annotation::new(format!("→ {cmd}")).with_style(STYLE_DETAIL))
                    }
                    _ => node,
                }
            })
            .collect();
        children.push(Node::branch("entrypoints", names));
    }

    Node::branch("metadata", children)
}

fn candidates_node(candidates: &[CandidateOut]) -> Node {
    let entries = candidates
        .iter()
        .map(|c| {
            Node::leaf(c.platform.clone())
                .with_annotation(Annotation::new(c.digest.clone()).with_style(STYLE_DIGEST))
                .with_annotation(Annotation::new(c.media_type.clone()).with_style(STYLE_DETAIL))
                .with_annotation(Annotation::new(format!("{} bytes", c.size)).with_style(STYLE_DETAIL))
        })
        .collect();
    Node::branch("candidates", entries)
}

fn resolution_node(resolution: &Resolution) -> Node {
    let chain = resolution.chain.iter().map(|d| Node::leaf(d.clone())).collect();
    let layers = resolution
        .layers
        .iter()
        .map(|l| {
            Node::leaf(l.digest.clone())
                .with_annotation(Annotation::new(l.media_type.clone()).with_style(STYLE_DETAIL))
                .with_annotation(Annotation::new(format!("{} bytes", l.size)).with_style(STYLE_DETAIL))
        })
        .collect();
    Node::branch(
        "resolution",
        vec![
            Node::leaf(format!("pinned {}", resolution.pinned)),
            Node::branch("chain", chain),
            Node::branch("layers", layers),
        ],
    )
}

impl Printable for PackageInspect {
    // Inspect output is inherently structured (nested sections), not a single
    // table — same tree exemption `deps` uses.
    fn print_plain(&self, data: &DataInterface) {
        let (pinned, sections) = match &self.body {
            Body::Candidates { pinned, candidates } => (pinned, vec![candidates_node(candidates)]),
            Body::Manifest { pinned, metadata } => (pinned, vec![metadata_node(metadata)]),
            Body::Resolved {
                pinned,
                metadata,
                resolution,
                ..
            } => (pinned, vec![metadata_node(metadata), resolution_node(resolution)]),
        };
        let root = Node::branch(pinned.clone(), sections);
        data.print_tree(&root);
    }
}
