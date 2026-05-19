// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Serialize, ser::SerializeStruct};

use ocx_lib::{
    cli::{Annotation, DataInterface, Theme, TreeItem, human_bytes},
    oci,
    package::metadata::{Metadata, env::modifier::ModifierKind, visibility::Visibility},
    package_manager::{InspectResult, ResolvedChain},
};

use crate::api::Printable;

/// Semantic role of a tree annotation. Text is stored raw; the [`Theme`]
/// inks it at render time (`annotations`) so no style is hard-coded here.
/// Each variant maps to one palette entry, so semantically identical things
/// share a colour across every `inspect` view and follow the active theme.
#[derive(Clone)]
enum SemanticAnnotation {
    /// A content digest (or digest-bearing identifier).
    Digest(String),
    /// An env-entry visibility tag — same palette entry as visibility
    /// everywhere else (e.g. `ocx package deps`).
    Visibility(Visibility),
    /// A short informational note next to a value: modifier kind, media
    /// type, byte size, or dispatch-command divergence.
    Note(String),
    /// Carried verbatim with the renderer's default annotation style.
    Plain(String),
}

impl SemanticAnnotation {
    fn ink(&self, theme: &Theme) -> Annotation {
        match self {
            SemanticAnnotation::Digest(text) => Annotation::new(theme.digest(text)),
            SemanticAnnotation::Visibility(visibility) => {
                Annotation::new(theme.visibility(*visibility, visibility.to_string()))
            }
            SemanticAnnotation::Note(text) => Annotation::new(theme.note(text)),
            SemanticAnnotation::Plain(text) => Annotation::new(text.clone()),
        }
    }
}

/// Read-only view of a package. The shape adapts to the requested reference
/// and whether `--resolve` was given:
///
/// - **candidates** (default mode, ref is an image index): the available
///   platform children — `{ identifier, pinned_digest, candidates: [...] }`.
/// - **metadata** (default mode, ref is a single manifest): the declared
///   metadata document — `{ identifier, pinned_digest, metadata }`.
/// - **resolution** (`--resolve`): platform-selected metadata plus the OCI
///   resolution chain — `{ identifier, pinned_digest, platforms, metadata,
///   resolution }`. Each `resolution.chain` entry carries `{ digest, role,
///   media_type, size }` (role ∈ `index` | `manifest` | `config`).
///
/// Plain format: a tree rooted at the pinned identifier (inked with the
/// active theme like every other identifier) with the shape-appropriate
/// section(s). Byte sizes render human-readable (binary units); JSON keeps
/// the raw integer `size`.
pub struct PackageInspect {
    identifier: oci::Identifier,
    pinned_digest: String,
    body: Body,
}

enum Body {
    Candidates {
        pinned: oci::PinnedIdentifier,
        candidates: Vec<CandidateOut>,
    },
    Manifest {
        pinned: oci::PinnedIdentifier,
        metadata: Metadata,
    },
    Resolved {
        pinned: oci::PinnedIdentifier,
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
    chain: Vec<ChainOut>,
    layers: Vec<Layer>,
}

/// One blob in the resolution chain. Same descriptor surface as a layer
/// (digest, media type, size) plus the OCI `role` so a consumer can tell
/// the index from the manifest from the config without decoding digests.
#[derive(Serialize)]
struct ChainOut {
    digest: String,
    role: String,
    media_type: String,
    size: i64,
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
                    pinned,
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
                    pinned,
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
                    pinned,
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
            chain: chain
                .chain
                .iter()
                .map(|blob| ChainOut {
                    digest: blob.identifier.digest().to_string(),
                    role: blob.role.to_string(),
                    media_type: blob.media_type.clone(),
                    size: blob.size,
                })
                .collect(),
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
    /// When set, the label is an identifier inked with the active theme at
    /// render time (so the root reads like every other identifier). Takes
    /// precedence over `label`.
    identifier: Option<oci::Identifier>,
    annotations: Vec<SemanticAnnotation>,
    children: Vec<Node>,
}

impl Node {
    fn leaf(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            identifier: None,
            annotations: Vec::new(),
            children: Vec::new(),
        }
    }

    fn branch(label: impl Into<String>, children: Vec<Node>) -> Self {
        Self {
            label: label.into(),
            identifier: None,
            annotations: Vec::new(),
            children,
        }
    }

    /// A branch whose label is an identifier — inked with the active theme
    /// at render time so it matches digest/identifier colouring everywhere
    /// else in the tree.
    fn identifier_branch(identifier: oci::Identifier, children: Vec<Node>) -> Self {
        Self {
            label: String::new(),
            identifier: Some(identifier),
            annotations: Vec::new(),
            children,
        }
    }

    fn with_digest(mut self, text: impl Into<String>) -> Self {
        self.annotations.push(SemanticAnnotation::Digest(text.into()));
        self
    }

    fn with_visibility(mut self, visibility: Visibility) -> Self {
        self.annotations.push(SemanticAnnotation::Visibility(visibility));
        self
    }

    fn with_note(mut self, text: impl Into<String>) -> Self {
        self.annotations.push(SemanticAnnotation::Note(text.into()));
        self
    }

    fn with_plain(mut self, text: impl Into<String>) -> Self {
        self.annotations.push(SemanticAnnotation::Plain(text.into()));
        self
    }
}

impl TreeItem for Node {
    fn label(&self, theme: &Theme) -> String {
        match &self.identifier {
            Some(identifier) => theme.of(identifier),
            None => self.label.clone(),
        }
    }

    fn children(&self) -> &[Self] {
        &self.children
    }

    fn annotations(&self, theme: &Theme) -> Vec<Annotation> {
        self.annotations.iter().map(|a| a.ink(theme)).collect()
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
                    .with_note(kind.to_string())
                    .with_visibility(var.visibility);
                if let Some(value) = var.value() {
                    node = node.with_plain(value.to_string());
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
            .map(|dep| Node::leaf(dep.name().to_string()).with_digest(dep.identifier.to_string()))
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
                    Some(cmd) if cmd.as_str() != name.as_str() => node.with_note(format!("→ {cmd}")),
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
                .with_digest(c.digest.clone())
                .with_note(c.media_type.clone())
                .with_note(human_bytes(c.size))
        })
        .collect();
    Node::branch("candidates", entries)
}

fn resolution_node(resolution: &Resolution) -> Node {
    // Chain entries render exactly like layers — role label, then digest /
    // media type / size annotations — so the OCI walk (index → manifest →
    // config) is legible instead of an opaque positional digest list. The
    // role doubles as the walk-order marker the bare `[i]` used to carry.
    let chain = resolution
        .chain
        .iter()
        .map(|c| {
            Node::leaf(c.role.clone())
                .with_digest(c.digest.clone())
                .with_note(c.media_type.clone())
                .with_note(human_bytes(c.size))
        })
        .collect();
    let layers = resolution
        .layers
        .iter()
        .enumerate()
        .map(|(i, l)| {
            Node::leaf(format!("[{i}]"))
                .with_digest(l.digest.clone())
                .with_note(l.media_type.clone())
                .with_note(human_bytes(l.size))
        })
        .collect();
    Node::branch(
        "resolution",
        vec![
            Node::leaf("pinned".to_string()).with_digest(resolution.pinned.clone()),
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
        let root = Node::identifier_branch(pinned.as_identifier().clone(), sections);
        data.print_tree(&root);
    }
}
