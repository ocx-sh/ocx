// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{HashMap, HashSet};

use serde::{Serialize, ser::SerializeStruct};

use ocx_lib::{
    cli::{Annotation, DataInterface, Theme, TreeItem, human_bytes},
    oci,
    package::metadata::{Binaries, Metadata, env::modifier::ModifierKind, visibility::Visibility},
    package_manager::{ClosureConflicts, ClosureEdge, ClosureNode, InspectClosure, InspectResult, ResolvedChain},
};

use crate::api::{Printable, data::env::BinaryAttribution};

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
///   metadata document plus the manifest's layers —
///   `{ identifier, pinned_digest, metadata, layers }`.
/// - **resolution** (`--resolve`): platform-selected metadata and layers plus
///   the OCI resolution chain — `{ identifier, pinned_digest, platform,
///   metadata, layers, resolution }`. Each `resolution.chain` entry carries
///   `{ digest, role, media_type, size }` (role ∈ `index` | `manifest` |
///   `config`); the layers live at the top level, not inside `resolution`.
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
        layers: Vec<Layer>,
        closure: Option<ClosureOut>,
        interface_surface: Option<InterfaceSurfaceOut>,
    },
    Resolved {
        pinned: oci::PinnedIdentifier,
        platform: oci::Platform,
        metadata: Metadata,
        layers: Vec<Layer>,
        resolution: Resolution,
        closure: Option<ClosureOut>,
        interface_surface: Option<InterfaceSurfaceOut>,
    },
}

/// Flat, digest-keyed dependency closure emitted with `--deps`. Serializes
/// transparently as the bare node array — ADR D2's wire shape is
/// `"closure": [ ... ]`, not a `{ "nodes": [...] }` wrapper.
#[derive(Serialize)]
#[serde(transparent)]
struct ClosureOut {
    nodes: Vec<ClosureNodeOut>,
}

/// One node of a [`ClosureOut`]. See ADR D2's `closure[]` element shape.
#[derive(Serialize)]
struct ClosureNodeOut {
    identifier: String,
    digest: String,
    /// Composed from the root. Key absent iff `root: true` (ADR D2).
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_visibility: Option<String>,
    /// Tri-state, mirrors `Bundle.binaries`: key absent = undeclared,
    /// `Some(empty)` = publisher asserts zero interface executables.
    #[serde(skip_serializing_if = "Option::is_none")]
    binaries: Option<Vec<String>>,
    entrypoints: Vec<String>,
    dependencies: Vec<ClosureEdgeOut>,
    /// Serialized only when `true` (ADR D2).
    #[serde(skip_serializing_if = "is_false")]
    root: bool,
}

/// A declared dependency edge (as authored) inside a [`ClosureNodeOut`].
#[derive(Serialize)]
struct ClosureEdgeOut {
    identifier: String,
    visibility: String,
    name: String,
}

/// The `--deps` interface-surface aggregate — "what binaries/entrypoints
/// would land on PATH if this were installed", without installing. See ADR
/// D2 `interface_surface` shape.
#[derive(Serialize)]
struct InterfaceSurfaceOut {
    binaries: Vec<BinaryAttribution>,
    entrypoints: Vec<BinaryAttribution>,
    /// `false` iff any interface-admitted node has undeclared `binaries`
    /// ("couldn't determine \u{2260} determined zero"). Entrypoints have no
    /// such flag — the entrypoint map keys are always authoritative.
    binaries_complete: bool,
    conflicts: ConflictsOut,
}

/// Install/compose-gate conditions detected over the interface projection
/// (Codex C2). Both arrays always present; empty means the surface is
/// realizable. Inspect stays a view, not a gate — exit 0 either way.
#[derive(Serialize)]
struct ConflictsOut {
    entrypoints: Vec<EntrypointConflictOut>,
    repositories: Vec<RepositoryConflictOut>,
}

/// Two or more interface-admitted closure nodes declare the same entrypoint
/// name.
#[derive(Serialize)]
struct EntrypointConflictOut {
    name: String,
    packages: Vec<String>,
}

/// One repository resolved to two or more distinct digests on the interface
/// projection.
#[derive(Serialize)]
struct RepositoryConflictOut {
    repository: String,
    digests: Vec<String>,
}

/// `skip_serializing_if` helper for [`ClosureNodeOut::root`] — omit the
/// `root` key entirely for non-root closure nodes (ADR D2).
fn is_false(value: &bool) -> bool {
    !*value
}

/// Projects a lib-level metadata closure into the wire shape (`closure` +
/// `interface_surface`, ADR D2). `interface_binaries`/`interface_entrypoints`
/// reuse [`BinaryAttribution::from_pairs`] — the same `(PinnedIdentifier, T:
/// Display)` projection `ocx env`/`ocx package env` already use for their
/// admitted-set attribution arrays.
fn project_closure(closure: InspectClosure) -> (ClosureOut, InterfaceSurfaceOut) {
    let InspectClosure {
        nodes,
        interface_binaries,
        interface_entrypoints,
        interface_binaries_complete,
        conflicts,
    } = closure;

    let closure_out = ClosureOut {
        nodes: nodes.into_iter().map(closure_node_out).collect(),
    };
    let interface_surface = InterfaceSurfaceOut {
        binaries: BinaryAttribution::from_pairs(&interface_binaries),
        entrypoints: BinaryAttribution::from_pairs(&interface_entrypoints),
        binaries_complete: interface_binaries_complete,
        conflicts: conflicts_out(conflicts),
    };
    (closure_out, interface_surface)
}

/// Projects one lib-level [`ClosureNode`] into its wire shape.
fn closure_node_out(node: ClosureNode) -> ClosureNodeOut {
    ClosureNodeOut {
        identifier: node.identifier.to_string(),
        digest: node.identifier.digest().to_string(),
        effective_visibility: node.effective_visibility.map(|visibility| visibility.to_string()),
        binaries: node
            .binaries
            .map(|binaries| binaries.iter().map(ToString::to_string).collect()),
        entrypoints: node.entrypoints.iter().map(ToString::to_string).collect(),
        dependencies: node.dependencies.into_iter().map(closure_edge_out).collect(),
        root: node.is_root,
    }
}

/// Projects one lib-level [`ClosureEdge`] into its wire shape.
fn closure_edge_out(edge: ClosureEdge) -> ClosureEdgeOut {
    ClosureEdgeOut {
        identifier: edge.identifier.to_string(),
        visibility: edge.visibility.to_string(),
        name: edge.name.to_string(),
    }
}

/// Projects the lib-level [`ClosureConflicts`] (Codex C2) into its wire shape.
fn conflicts_out(conflicts: ClosureConflicts) -> ConflictsOut {
    ConflictsOut {
        entrypoints: conflicts
            .entrypoints
            .into_iter()
            .map(|conflict| EntrypointConflictOut {
                name: conflict.name.to_string(),
                packages: conflict.packages.iter().map(ToString::to_string).collect(),
            })
            .collect(),
        repositories: conflicts
            .repositories
            .into_iter()
            .map(|conflict| RepositoryConflictOut {
                repository: conflict.repository.to_string(),
                digests: conflict.digests.iter().map(ToString::to_string).collect(),
            })
            .collect(),
    }
}

/// One platform child of an image index.
#[derive(Serialize)]
struct CandidateOut {
    digest: String,
    platform: String,
    media_type: String,
    size: i64,
}

/// The OCI resolution chain for the selected platform. Carries only the walk
/// (`index` → `manifest` → `config`); the platform-selected manifest's layers
/// are rendered alongside the metadata, not inside the chain.
#[derive(Serialize)]
struct Resolution {
    pinned: String,
    chain: Vec<ChainOut>,
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

/// A single layer descriptor from the inspected manifest (default mode) or the
/// platform-selected manifest (`--resolve`).
#[derive(Serialize)]
struct Layer {
    digest: String,
    media_type: String,
    size: i64,
}

impl Layer {
    /// Projects raw OCI layer descriptors onto the report surface, shared by
    /// the default-manifest and resolved views.
    fn from_descriptors(descriptors: &[oci::Descriptor]) -> Vec<Self> {
        descriptors
            .iter()
            .map(|descriptor| Layer {
                digest: descriptor.digest.clone(),
                media_type: descriptor.media_type.clone(),
                size: descriptor.size,
            })
            .collect()
    }
}

impl PackageInspect {
    /// Builds the report from the task result. `identifier` is the requested
    /// identifier (post default-registry expansion); `platform` is the
    /// platform resolution selected against (only meaningful in `--resolve`
    /// mode).
    pub fn new(identifier: oci::Identifier, platform: oci::Platform, result: InspectResult) -> Self {
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
            InspectResult::Manifest {
                pinned,
                metadata,
                layers,
                closure,
            } => {
                let (closure, interface_surface) = split_projected_closure(closure);
                Self {
                    identifier,
                    pinned_digest: pinned.digest().to_string(),
                    body: Body::Manifest {
                        pinned,
                        metadata: metadata.into(),
                        layers: Layer::from_descriptors(&layers),
                        closure,
                        interface_surface,
                    },
                }
            }
            InspectResult::Resolved {
                pinned,
                metadata,
                chain,
                closure,
            } => {
                let (closure, interface_surface) = split_projected_closure(closure);
                Self {
                    identifier,
                    pinned_digest: pinned.digest().to_string(),
                    body: Body::Resolved {
                        pinned,
                        platform,
                        metadata: metadata.into(),
                        layers: Layer::from_descriptors(&chain.final_manifest.layers),
                        resolution: Resolution::from_chain(&chain),
                        closure,
                        interface_surface,
                    },
                }
            }
        }
    }
}

/// Projects an optional lib-level closure into the wire pair, or `(None,
/// None)` when `--deps` was not requested. Shared by the `Manifest` and
/// `Resolved` construction arms.
fn split_projected_closure(closure: Option<InspectClosure>) -> (Option<ClosureOut>, Option<InterfaceSurfaceOut>) {
    match closure {
        Some(closure) => {
            let (closure, interface_surface) = project_closure(closure);
            (Some(closure), Some(interface_surface))
        }
        None => (None, None),
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
        }
    }
}

impl Serialize for PackageInspect {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Field count varies by body shape; identifier + pinned_digest are
        // always present. `closure`/`interface_surface` are additive-optional
        // (present only under `--deps`) — see `adr_inspect_metadata_closure.md` D2.
        let len = 2 + match &self.body {
            Body::Candidates { .. } => 1,
            Body::Manifest {
                closure,
                interface_surface,
                ..
            } => 2 + usize::from(closure.is_some()) + usize::from(interface_surface.is_some()),
            Body::Resolved {
                closure,
                interface_surface,
                ..
            } => 4 + usize::from(closure.is_some()) + usize::from(interface_surface.is_some()),
        };
        let mut s = serializer.serialize_struct("PackageInspect", len)?;
        s.serialize_field("identifier", &self.identifier)?;
        s.serialize_field("pinned_digest", &self.pinned_digest)?;
        match &self.body {
            Body::Candidates { candidates, .. } => {
                s.serialize_field("candidates", candidates)?;
            }
            Body::Manifest {
                metadata,
                layers,
                closure,
                interface_surface,
                ..
            } => {
                s.serialize_field("metadata", metadata)?;
                s.serialize_field("layers", layers)?;
                if let Some(closure) = closure {
                    s.serialize_field("closure", closure)?;
                }
                if let Some(interface_surface) = interface_surface {
                    s.serialize_field("interface_surface", interface_surface)?;
                }
            }
            Body::Resolved {
                platform,
                metadata,
                layers,
                resolution,
                closure,
                interface_surface,
                ..
            } => {
                s.serialize_field("platform", platform)?;
                s.serialize_field("metadata", metadata)?;
                s.serialize_field("layers", layers)?;
                s.serialize_field("resolution", resolution)?;
                if let Some(closure) = closure {
                    s.serialize_field("closure", closure)?;
                }
                if let Some(interface_surface) = interface_surface {
                    s.serialize_field("interface_surface", interface_surface)?;
                }
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
                    Some(cmd) if cmd.as_str() != name.as_str() => node.with_note(format!("-> {cmd}")),
                    _ => node,
                }
            })
            .collect();
        children.push(Node::branch("entrypoints", names));
    }

    // `None` = undeclared (omit the node); `Some(empty)` = publisher asserts
    // zero interface executables (render explicitly, distinct from absence).
    if let Some(binaries) = metadata.binaries() {
        children.push(binaries_node(binaries));
    }

    Node::branch("metadata", children)
}

fn binaries_node(binaries: &Binaries) -> Node {
    if binaries.is_empty() {
        return Node::leaf("binaries (none declared)");
    }
    let names = binaries.iter().map(|name| Node::leaf(name.to_string())).collect();
    Node::branch("binaries", names)
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

/// Renders the manifest's layer descriptors as a `layers` branch — one indexed
/// `[i]` leaf per layer with digest / media type / human size. Shared by the
/// default-manifest and resolved views.
fn layers_node(layers: &[Layer]) -> Node {
    let entries = layers
        .iter()
        .enumerate()
        .map(|(i, layer)| {
            Node::leaf(format!("[{i}]"))
                .with_digest(layer.digest.clone())
                .with_note(layer.media_type.clone())
                .with_note(human_bytes(layer.size))
        })
        .collect();
    Node::branch("layers", entries)
}

fn resolution_node(resolution: &Resolution) -> Node {
    // Chain entries render like layers — role label, then digest / media type /
    // size annotations — so the OCI walk (index → manifest → config) is legible
    // instead of an opaque positional digest list. The role doubles as the
    // walk-order marker the bare `[i]` used to carry. Layers are not part of
    // the walk; they render under the manifest itself, not here.
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
    Node::branch(
        "resolution",
        vec![
            Node::leaf("pinned".to_string()).with_digest(resolution.pinned.clone()),
            Node::branch("chain", chain),
        ],
    )
}

/// Renders the `closure` flat array as a `(*)`-deduped tree rooted at the
/// inspected package (ADR D2 plain format). Dedup is keyed by content digest
/// (extracted from an edge's `identifier` suffix via [`edge_digest`]), not by
/// the edge's own tag-bearing identifier string — a diamond's shared node
/// renders in full once; every later visit is a `(*)`-marked leaf.
fn closure_node(closure: &ClosureOut) -> Node {
    let nodes_by_digest: HashMap<&str, &ClosureNodeOut> =
        closure.nodes.iter().map(|node| (node.digest.as_str(), node)).collect();
    let root = closure
        .nodes
        .iter()
        .find(|node| node.root)
        .expect("a projected closure always carries exactly one root node (ADR D2)");

    let mut visited = HashSet::new();
    visited.insert(root.digest.clone());
    let tree = Node::branch(
        root.identifier.clone(),
        closure_node_children(root, &nodes_by_digest, &mut visited),
    )
    .with_digest(root.digest.clone());
    Node::branch("closure", vec![tree])
}

/// Extracts the content-digest suffix from a wire identifier string
/// (`registry/repository[:tag]@digest`) — the key `closure_node` dedups by.
fn edge_digest(identifier: &str) -> &str {
    identifier.rsplit('@').next().unwrap_or(identifier)
}

/// Parses a wire `effective_visibility` string back into the palette-typed
/// [`Visibility`] so the plain tree can reuse `SemanticAnnotation::Visibility`
/// (same four canonical strings the `Display` impl produces).
fn parse_visibility(text: &str) -> Option<Visibility> {
    match text {
        "sealed" => Some(Visibility::SEALED),
        "private" => Some(Visibility::PRIVATE),
        "interface" => Some(Visibility::INTERFACE),
        "public" => Some(Visibility::PUBLIC),
        _ => None,
    }
}

/// Renders one closure node's own content: its entrypoints/binaries leaves
/// plus one child per declared dependency edge (recursing or `(*)`-marking
/// per [`closure_edge_node`]).
fn closure_node_children(
    node: &ClosureNodeOut,
    nodes_by_digest: &HashMap<&str, &ClosureNodeOut>,
    visited: &mut HashSet<String>,
) -> Vec<Node> {
    let mut children = Vec::new();
    if !node.entrypoints.is_empty() {
        let names = node.entrypoints.iter().map(|name| Node::leaf(name.clone())).collect();
        children.push(Node::branch("entrypoints", names));
    }
    if let Some(binaries) = closure_binaries_node(&node.binaries) {
        children.push(binaries);
    }
    for edge in &node.dependencies {
        children.push(closure_edge_node(edge, nodes_by_digest, visited));
    }
    children
}

/// Tri-state `binaries` render for one closure node, mirroring
/// [`binaries_node`]'s three-way idiom over the wire `Option<Vec<String>>`
/// shape instead of the lib `Binaries` type: `None` (undeclared) renders
/// nothing, `Some(empty)` (asserted zero) renders a single leaf, `Some(names)`
/// renders a branch.
fn closure_binaries_node(binaries: &Option<Vec<String>>) -> Option<Node> {
    match binaries {
        None => None,
        Some(names) if names.is_empty() => Some(Node::leaf("binaries (none declared)")),
        Some(names) => {
            let leaves = names.iter().map(|name| Node::leaf(name.clone())).collect();
            Some(Node::branch("binaries", leaves))
        }
    }
}

/// Renders one declared dependency edge: the first visit to its digest
/// expands the target node in full (recursing into its own children); every
/// later visit renders a `(*)`-marked leaf instead of re-expanding.
fn closure_edge_node(
    edge: &ClosureEdgeOut,
    nodes_by_digest: &HashMap<&str, &ClosureNodeOut>,
    visited: &mut HashSet<String>,
) -> Node {
    let digest = edge_digest(&edge.identifier);
    let Some(target) = nodes_by_digest.get(digest).copied() else {
        // A dep names a digest absent from the flat array — should not
        // happen for a well-formed closure; render the edge as authored
        // rather than panic on a rendering path.
        return Node::leaf(edge.name.clone()).with_digest(edge.identifier.clone());
    };

    let mut rendered = if visited.insert(digest.to_string()) {
        Node::branch(
            edge.name.clone(),
            closure_node_children(target, nodes_by_digest, visited),
        )
    } else {
        Node::leaf(edge.name.clone()).with_note("(*)")
    };
    rendered = rendered.with_digest(edge.identifier.clone());
    if let Some(visibility) = target.effective_visibility.as_deref().and_then(parse_visibility) {
        rendered = rendered.with_visibility(visibility);
    }
    rendered
}

/// Renders the `interface_surface` aggregate as a branch: binaries +
/// entrypoints leaves, an incompleteness note when `binaries_complete ==
/// false`, and one note leaf per Codex-C2 conflict (ADR D2 plain format).
fn interface_surface_node(surface: &InterfaceSurfaceOut) -> Node {
    let mut children = Vec::new();

    if !surface.binaries.is_empty() {
        let leaves = surface.binaries.iter().map(binary_attribution_leaf).collect();
        children.push(Node::branch("binaries", leaves));
    }
    if !surface.entrypoints.is_empty() {
        let leaves = surface.entrypoints.iter().map(binary_attribution_leaf).collect();
        children.push(Node::branch("entrypoints", leaves));
    }
    if !surface.binaries_complete {
        children.push(Node::leaf(
            "binaries incomplete: at least one interface-admitted package declares no binaries",
        ));
    }

    for conflict in &surface.conflicts.entrypoints {
        children.push(
            Node::leaf(format!("entrypoint '{}' claimed by multiple packages", conflict.name))
                .with_note(conflict.packages.join(", ")),
        );
    }
    for conflict in &surface.conflicts.repositories {
        children.push(
            Node::leaf(format!(
                "repository '{}' resolves to multiple digests",
                conflict.repository
            ))
            .with_note(conflict.digests.join(", ")),
        );
    }

    Node::branch("interface surface", children)
}

/// Renders one [`BinaryAttribution`] as a leaf, annotating the owning
/// package when attribution is known.
fn binary_attribution_leaf(attribution: &BinaryAttribution) -> Node {
    let leaf = Node::leaf(attribution.name.clone());
    match &attribution.package {
        Some(package) => leaf.with_note(package.clone()),
        None => leaf,
    }
}

impl Printable for PackageInspect {
    // Inspect output is inherently structured (nested sections), not a single
    // table — same tree exemption `deps` uses.
    fn print_plain(&self, data: &DataInterface) {
        let (pinned, sections) = match &self.body {
            Body::Candidates { pinned, candidates } => (pinned, vec![candidates_node(candidates)]),
            Body::Manifest {
                pinned,
                metadata,
                layers,
                closure,
                interface_surface,
            } => {
                let mut sections = vec![metadata_node(metadata), layers_node(layers)];
                if let Some(closure) = closure {
                    sections.push(closure_node(closure));
                }
                if let Some(interface_surface) = interface_surface {
                    sections.push(interface_surface_node(interface_surface));
                }
                (pinned, sections)
            }
            Body::Resolved {
                pinned,
                metadata,
                layers,
                resolution,
                closure,
                interface_surface,
                ..
            } => {
                let mut sections = vec![
                    metadata_node(metadata),
                    layers_node(layers),
                    resolution_node(resolution),
                ];
                if let Some(closure) = closure {
                    sections.push(closure_node(closure));
                }
                if let Some(interface_surface) = interface_surface {
                    sections.push(interface_surface_node(interface_surface));
                }
                (pinned, sections)
            }
        };
        let root = Node::identifier_branch(pinned.as_identifier().clone(), sections);
        data.print_tree(&root);
    }
}

/// One or more [`PackageInspect`] views keyed by the requested identifier.
///
/// Plain format: each package's tree rendered in input order (inspect holds the
/// single-table exemption — its output is inherently a nested tree, not a row).
///
/// JSON format: object keyed by the raw request identifier
/// (`{"<id>": {…inspect…}}`), preserving input order — the same keyed-object
/// shape `which` uses, applied even for a single package.
pub struct PackageInspects {
    entries: Vec<(String, PackageInspect)>,
}

impl PackageInspects {
    pub fn new(entries: Vec<(String, PackageInspect)>) -> Self {
        Self { entries }
    }
}

impl Serialize for PackageInspects {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, inspect) in &self.entries {
            map.serialize_entry(key, inspect)?;
        }
        map.end()
    }
}

impl Printable for PackageInspects {
    fn print_plain(&self, data: &DataInterface) {
        for (_, inspect) in &self.entries {
            inspect.print_plain(data);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ocx_lib::package::metadata::{
        BinaryName, Entrypoints, ValidMetadata,
        bundle::{Bundle, Version},
        dependency::Dependencies,
        env::Env,
    };
    use ocx_lib::package_manager::{ClosureConflicts, ClosureNode, InspectClosure};

    use super::*;

    fn bundle_metadata(binaries: Option<Binaries>) -> Metadata {
        Metadata::Bundle(Bundle {
            binaries,
            version: Version::V1,
            strip_components: None,
            env: Env::default(),
            dependencies: Dependencies::default(),
            entrypoints: Entrypoints::default(),
        })
    }

    #[test]
    fn metadata_node_omits_binaries_when_undeclared() {
        let metadata = bundle_metadata(None);
        let node = metadata_node(&metadata);
        assert!(
            !node
                .children
                .iter()
                .any(|child| child.label == "binaries" || child.label == "binaries (none declared)"),
            "undeclared binaries must not render a node"
        );
    }

    #[test]
    fn metadata_node_renders_declared_empty_binaries_as_leaf() {
        let binaries = Binaries::try_from(BTreeSet::new()).expect("empty set is valid");
        let metadata = bundle_metadata(Some(binaries));
        let node = metadata_node(&metadata);
        let leaf = node
            .children
            .iter()
            .find(|child| child.label == "binaries (none declared)")
            .expect("declared-empty binaries renders as a single leaf");
        assert!(leaf.children.is_empty());
    }

    #[test]
    fn metadata_node_lists_declared_binary_names() {
        let names: BTreeSet<BinaryName> = ["ctest", "cmake"]
            .into_iter()
            .map(|name| BinaryName::try_from(name).expect("valid binary name"))
            .collect();
        let binaries = Binaries::try_from(names).expect("no case-fold collisions");
        let metadata = bundle_metadata(Some(binaries));
        let node = metadata_node(&metadata);
        let branch = node
            .children
            .iter()
            .find(|child| child.label == "binaries")
            .expect("declared binaries renders as a branch");
        let labels: Vec<_> = branch.children.iter().map(|child| child.label.clone()).collect();
        assert_eq!(labels, vec!["cmake", "ctest"], "names render in sorted order");
    }

    // ── `--deps` projection: closure / interface_surface (ADR D2) ───────────
    //
    // `project_closure` and the plain-render helpers (`closure_node`,
    // `interface_surface_node`) are `unimplemented!()` stubs — every test
    // below that reaches them is EXPECTED to fail via that panic during the
    // Specify phase. Assertions after the call document the intended
    // contract for when the Implement phase fills the stubs; they do not
    // execute until then. `json_closure_and_interface_surface_keys_absent_without_deps`
    // is the one exception: it exercises only already-implemented code
    // (`closure: None` never reaches `project_closure`) and must pass now.

    fn test_identifier() -> oci::Identifier {
        oci::Identifier::new_registry("toolchain", "example.com").clone_with_tag("1.0")
    }

    fn test_platform() -> oci::Platform {
        oci::Platform::any()
    }

    fn pinned(repo: &str, hex_char: char) -> oci::PinnedIdentifier {
        let id = oci::Identifier::new_registry(repo, "example.com")
            .clone_with_digest(oci::Digest::Sha256(hex_char.to_string().repeat(64)));
        oci::PinnedIdentifier::try_from(id).expect("digest-bearing identifier is always pinnable")
    }

    fn fake_digest(hex_char: char) -> String {
        format!("sha256:{}", hex_char.to_string().repeat(64))
    }

    fn binaries_of(names: &[&str]) -> Binaries {
        let set: BTreeSet<BinaryName> = names
            .iter()
            .map(|name| BinaryName::try_from(*name).expect("valid binary name"))
            .collect();
        Binaries::try_from(set).expect("fixture names never case-fold collide")
    }

    /// Builds a minimal `Manifest`-mode `InspectResult` carrying `closure`
    /// (or `None`, the no-`--deps` case).
    fn manifest_result(root: oci::PinnedIdentifier, closure: Option<InspectClosure>) -> InspectResult {
        InspectResult::Manifest {
            pinned: root,
            metadata: ValidMetadata::try_from(bundle_metadata(None)).expect("bare bundle metadata is always valid"),
            layers: vec![],
            closure,
        }
    }

    fn leaf_node(
        identifier: oci::PinnedIdentifier,
        effective_visibility: Option<Visibility>,
        is_root: bool,
    ) -> ClosureNode {
        ClosureNode {
            identifier,
            effective_visibility,
            binaries: None,
            entrypoints: vec![],
            dependencies: vec![],
            is_root,
        }
    }

    // ── JSON projection ───────────────────────────────────────────────────

    /// Backward-compat pin (acceptance criteria: "existing inspect bodies
    /// byte-unchanged without `--deps`"): with no closure requested, the
    /// top-level JSON object must carry neither `closure` nor
    /// `interface_surface` at all. Exercises only already-implemented code
    /// (`split_projected_closure(None)` never calls `project_closure`), so
    /// this test passes now, unlike its siblings below.
    #[test]
    fn json_closure_and_interface_surface_keys_absent_without_deps() {
        let root = pinned("toolchain", 'a');
        let report = PackageInspect::new(test_identifier(), test_platform(), manifest_result(root, None));
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        let obj = value.as_object().expect("top-level JSON is an object");
        assert!(
            !obj.contains_key("closure"),
            "no --deps requested, closure key must be absent: {value}"
        );
        assert!(
            !obj.contains_key("interface_surface"),
            "no --deps requested, interface_surface key must be absent: {value}"
        );
    }

    /// `effective_visibility` is present with the composed value for every
    /// non-root node, and absent (not `null`) exactly for the root; `root`
    /// is present (`true`) only on the root entry and omitted for every
    /// other node (ADR D2 wire shape).
    #[test]
    fn json_closure_effective_visibility_and_root_key_absence() {
        let root = pinned("root", 'a');
        let dep = pinned("dep", 'b');
        let closure = InspectClosure {
            nodes: vec![
                leaf_node(dep, Some(Visibility::PUBLIC), false),
                leaf_node(root.clone(), None, true),
            ],
            interface_binaries: vec![],
            interface_entrypoints: vec![],
            interface_binaries_complete: true,
            conflicts: ClosureConflicts::default(),
        };
        let report = PackageInspect::new(test_identifier(), test_platform(), manifest_result(root, Some(closure)));
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");

        let nodes = value["closure"].as_array().expect("closure is a flat array (ADR D2)");
        let root_node = nodes
            .iter()
            .find(|n| n["identifier"].as_str().unwrap_or_default().contains("root"))
            .expect("root entry present");
        let dep_node = nodes
            .iter()
            .find(|n| n["identifier"].as_str().unwrap_or_default().contains("dep"))
            .expect("dep entry present");

        assert!(
            !root_node.as_object().unwrap().contains_key("effective_visibility"),
            "the composed-from-root axis is undefined for the root; the key must be absent: {root_node}"
        );
        assert_eq!(root_node["root"], serde_json::json!(true));
        assert_eq!(dep_node["effective_visibility"], "public");
        assert!(
            !dep_node.as_object().unwrap().contains_key("root"),
            "root key must be omitted (not false) for a non-root node: {dep_node}"
        );
    }

    /// Tri-state `binaries` wire contract per node: key absent for
    /// undeclared, `[]` for an explicit empty claim, `[names...]` for a
    /// declared claim.
    #[test]
    fn json_closure_binaries_tri_state_per_node() {
        let root = pinned("root", 'a');
        // Marker names must not be substrings of one another — `find` matches
        // by `contains`, and "undeclared-dep".contains("declared-dep") is true.
        let undeclared = pinned("no-claim-dep", 'b');
        let empty = pinned("zero-claim-dep", 'c');
        let declared = pinned("named-claim-dep", 'd');

        let closure = InspectClosure {
            nodes: vec![
                ClosureNode {
                    binaries: None,
                    ..leaf_node(undeclared, Some(Visibility::PUBLIC), false)
                },
                ClosureNode {
                    binaries: Some(binaries_of(&[])),
                    ..leaf_node(empty, Some(Visibility::PUBLIC), false)
                },
                ClosureNode {
                    binaries: Some(binaries_of(&["x"])),
                    ..leaf_node(declared, Some(Visibility::PUBLIC), false)
                },
                leaf_node(root.clone(), None, true),
            ],
            interface_binaries: vec![],
            interface_entrypoints: vec![],
            interface_binaries_complete: false,
            conflicts: ClosureConflicts::default(),
        };
        let report = PackageInspect::new(test_identifier(), test_platform(), manifest_result(root, Some(closure)));
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        let nodes = value["closure"].as_array().expect("closure is a flat array (ADR D2)");
        let find = |marker: &str| {
            nodes
                .iter()
                .find(|n| n["identifier"].as_str().unwrap_or_default().contains(marker))
                .unwrap_or_else(|| panic!("no closure node matches '{marker}': {nodes:?}"))
        };

        assert!(
            !find("no-claim-dep").as_object().unwrap().contains_key("binaries"),
            "undeclared binaries must omit the key entirely"
        );
        assert_eq!(
            find("zero-claim-dep")["binaries"],
            serde_json::json!([]),
            "an explicit empty declaration must serialize as an empty array"
        );
        assert_eq!(find("named-claim-dep")["binaries"], serde_json::json!(["x"]));
    }

    /// `interface_surface` always carries all four keys — `binaries`,
    /// `entrypoints`, `binaries_complete`, `conflicts.{entrypoints,repositories}`
    /// — even when every value is empty/false. Never omitted via
    /// `skip_serializing_if` the way the tri-state `binaries` key is.
    #[test]
    fn json_interface_surface_always_carries_required_keys_even_when_empty() {
        let root = pinned("root", 'a');
        let closure = InspectClosure {
            nodes: vec![leaf_node(root.clone(), None, true)],
            interface_binaries: vec![],
            interface_entrypoints: vec![],
            interface_binaries_complete: true,
            conflicts: ClosureConflicts::default(),
        };
        let report = PackageInspect::new(test_identifier(), test_platform(), manifest_result(root, Some(closure)));
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        let surface = value["interface_surface"]
            .as_object()
            .expect("interface_surface object present");

        assert!(surface.get("binaries").is_some_and(serde_json::Value::is_array));
        assert!(surface.get("entrypoints").is_some_and(serde_json::Value::is_array));
        assert!(
            surface
                .get("binaries_complete")
                .is_some_and(serde_json::Value::is_boolean)
        );
        let conflicts = surface
            .get("conflicts")
            .and_then(serde_json::Value::as_object)
            .expect("conflicts object present");
        assert!(conflicts.get("entrypoints").is_some_and(serde_json::Value::is_array));
        assert!(conflicts.get("repositories").is_some_and(serde_json::Value::is_array));
    }

    // ── Plain render ──────────────────────────────────────────────────────

    /// Recursively flattens a rendered [`Node`] into its raw label + text
    /// annotations (skipping the `Visibility` variant, which carries no free
    /// text). Reads the private fields directly — this test module is a
    /// descendant of the defining module, same idiom as the `metadata_node`
    /// tests above.
    fn collect_node_text(node: &Node, out: &mut Vec<String>) {
        out.push(node.label.clone());
        for annotation in &node.annotations {
            match annotation {
                SemanticAnnotation::Digest(text) | SemanticAnnotation::Note(text) | SemanticAnnotation::Plain(text) => {
                    out.push(text.clone());
                }
                SemanticAnnotation::Visibility(_) => {}
            }
        }
        for child in &node.children {
            collect_node_text(child, out);
        }
    }

    /// A diamond closure — C reached via two edges from A and B, each using
    /// a DIFFERENT advisory tag on the same digest. Proves two ADR D2 plain
    /// requirements at once: dedup is digest-keyed (not string-identity
    /// keyed — the two edges never share an identifier string), and a
    /// repeat visit renders a `(*)` marker without re-expanding the node's
    /// own content a second time.
    #[test]
    fn closure_node_renders_diamond_repeat_with_digest_keyed_star_marker() {
        let c_digest = fake_digest('c');
        let c = ClosureNodeOut {
            identifier: format!("example.com/c@{c_digest}"),
            digest: c_digest.clone(),
            effective_visibility: Some("public".to_string()),
            binaries: None,
            entrypoints: vec!["c-ep".to_string()],
            dependencies: vec![],
            root: false,
        };
        let a_digest = fake_digest('a');
        let a = ClosureNodeOut {
            identifier: format!("example.com/a@{a_digest}"),
            digest: a_digest.clone(),
            effective_visibility: Some("public".to_string()),
            binaries: None,
            entrypoints: vec![],
            dependencies: vec![ClosureEdgeOut {
                identifier: format!("example.com/c:tag-a@{c_digest}"),
                visibility: "public".to_string(),
                name: "c".to_string(),
            }],
            root: false,
        };
        let b_digest = fake_digest('b');
        let b = ClosureNodeOut {
            identifier: format!("example.com/b@{b_digest}"),
            digest: b_digest.clone(),
            effective_visibility: Some("public".to_string()),
            binaries: None,
            entrypoints: vec![],
            dependencies: vec![ClosureEdgeOut {
                identifier: format!("example.com/c:tag-b@{c_digest}"),
                visibility: "public".to_string(),
                name: "c".to_string(),
            }],
            root: false,
        };
        let root = ClosureNodeOut {
            identifier: format!("example.com/root@{}", fake_digest('r')),
            digest: fake_digest('r'),
            effective_visibility: None,
            binaries: None,
            entrypoints: vec![],
            dependencies: vec![
                ClosureEdgeOut {
                    identifier: format!("example.com/a@{a_digest}"),
                    visibility: "public".to_string(),
                    name: "a".to_string(),
                },
                ClosureEdgeOut {
                    identifier: format!("example.com/b@{b_digest}"),
                    visibility: "public".to_string(),
                    name: "b".to_string(),
                },
            ],
            root: true,
        };
        let closure = ClosureOut {
            nodes: vec![c, a, b, root],
        };

        let tree = closure_node(&closure);

        let mut text = Vec::new();
        collect_node_text(&tree, &mut text);
        let joined = text.join(" | ");

        assert!(
            joined.contains("(*)"),
            "a repeat-visited diamond node must carry a visible (*) marker somewhere in the tree: {joined}"
        );
        let c_occurrences = text.iter().filter(|t| t.contains(&c_digest)).count();
        assert_eq!(
            c_occurrences, 2,
            "C is reached via two edges with distinct advisory tags but the same digest — dedup must be \
             digest-keyed, so C renders twice (once full, once (*)): {joined}"
        );
        let ep_occurrences = text.iter().filter(|t| t.contains("c-ep")).count();
        assert_eq!(
            ep_occurrences, 1,
            "the repeat visit must NOT re-expand C's own content a second time: {joined}"
        );
    }

    /// The `interface surface` branch renders declared binaries/entrypoints
    /// as leaves and, when `binaries_complete == false`, an incompleteness
    /// note; conflicts render as their own note leaves naming the colliding
    /// entrypoint name and repository.
    #[test]
    fn interface_surface_node_renders_binaries_entrypoints_and_conflict_notes() {
        let surface = InterfaceSurfaceOut {
            binaries: BinaryAttribution::from_pairs(&[(pinned("cmake", 'a'), "cmake".to_string())]),
            entrypoints: BinaryAttribution::from_pairs(&[(pinned("toolchain", 'b'), "cc".to_string())]),
            binaries_complete: false,
            conflicts: ConflictsOut {
                entrypoints: vec![EntrypointConflictOut {
                    name: "shared-ep".to_string(),
                    packages: vec![
                        format!("example.com/a@{}", fake_digest('c')),
                        format!("example.com/b@{}", fake_digest('d')),
                    ],
                }],
                repositories: vec![RepositoryConflictOut {
                    repository: "example.com/shared-lib".to_string(),
                    digests: vec![fake_digest('e'), fake_digest('f')],
                }],
            },
        };

        let node = interface_surface_node(&surface);

        let mut text = Vec::new();
        collect_node_text(&node, &mut text);
        let joined = text.join(" | ");

        assert!(
            joined.contains("cmake"),
            "declared binary name must render as a leaf: {joined}"
        );
        assert!(
            joined.contains("cc"),
            "declared entrypoint name must render as a leaf: {joined}"
        );
        assert!(
            joined.to_lowercase().contains("incomplete"),
            "binaries_complete=false must render a completeness note per ADR D2 plain format: {joined}"
        );
        assert!(
            joined.contains("shared-ep"),
            "the entrypoint conflict must render as a note leaf naming the colliding name: {joined}"
        );
        assert!(
            joined.contains("shared-lib"),
            "the repository conflict must render as a note leaf naming the colliding repository: {joined}"
        );
    }

    /// The completeness note is conditional — a complete aggregate
    /// (`binaries_complete == true`) must not render an "incomplete" note.
    #[test]
    fn interface_surface_node_omits_incomplete_note_when_binaries_complete() {
        let surface = InterfaceSurfaceOut {
            binaries: vec![],
            entrypoints: vec![],
            binaries_complete: true,
            conflicts: ConflictsOut {
                entrypoints: vec![],
                repositories: vec![],
            },
        };

        let node = interface_surface_node(&surface);

        let mut text = Vec::new();
        collect_node_text(&node, &mut text);
        let joined = text.join(" | ");

        assert!(
            !joined.to_lowercase().contains("incomplete"),
            "a complete aggregate must not render an incomplete note: {joined}"
        );
    }
}
