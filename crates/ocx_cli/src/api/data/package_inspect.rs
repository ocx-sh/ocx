// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Serialize, ser::SerializeStruct};

use ocx_lib::{
    cli::{Annotation, DataInterface, Theme, TreeItem, human_bytes},
    oci,
    package::metadata::{Binaries, Metadata, env::modifier::ModifierKind, visibility::Visibility},
    package_manager::{
        ClosureConflicts, ClosureEdge, ClosureEnvVar, ClosureNode, InspectClosure, InspectResult, ResolvedChain,
        Surface,
    },
};

use crate::api::{
    Printable,
    data::env::{BinaryAttribution, EnvEntry},
};

/// Semantic role of a tree annotation. Text is stored raw; the [`Theme`]
/// inks it at render time (`annotations`) so no style is hard-coded here.
/// Each variant maps to one palette entry, so semantically identical things
/// share a colour across every `inspect` view and follow the active theme.
#[derive(Clone)]
enum SemanticAnnotation {
    /// A content digest, or a whole digest-bearing identifier. In the tree
    /// view a full `registry/repo:tag@digest` annotation is deliberately inked
    /// as ONE digest-coloured span — not split into the per-part identifier
    /// palette the root label uses — so an identifier read as an aside next to
    /// a leaf stays a single visual unit.
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
/// and whether `--resolve` was given. Every shape carries `name` (how the
/// caller addressed this entry — the raw request string for
/// `ocx package inspect`, the `ocx.toml` binding for `ocx inspect`) and
/// `identifier`. A shape that selected one artifact also carries
/// `pinned_identifier` (the identifier with its digest, so a consumer never
/// has to splice one together) and `pinned_digest`:
///
/// - **candidates** (default mode): the available platform children — `{ …,
///   candidates: [...] }`. `ocx package inspect` reads them off an image index
///   and pins the index itself; `ocx inspect` projects them from `ocx.lock`,
///   which records per-platform leaf digests and no index digest, so the two
///   pinned fields are absent there and each candidate's `media_type` / `size`
///   with them.
/// - **metadata** (default mode, ref is a single manifest): the declared
///   metadata document plus the manifest's layers — `{ …, metadata, layers }`.
/// - **resolution** (`--resolve`): platform-selected metadata and layers plus
///   the OCI resolution chain — `{ …, platform, metadata, layers, resolution }`.
///   Each `resolution.chain` entry carries `{ digest, role, media_type, size }`
///   (role ∈ `index` | `manifest` | `config`); the layers live at the entry's
///   top level, not inside `resolution`.
///
/// Plain format: a tree rooted at the pinned identifier — or at the plain
/// identifier where nothing was pinned — inked with the active theme like
/// every other identifier, with the shape-appropriate section(s). Byte sizes
/// render human-readable (binary units); JSON keeps the raw integer `size`.
pub struct PackageInspect {
    name: String,
    identifier: oci::Identifier,
    body: Body,
}

enum Body {
    Candidates {
        pinned: oci::PinnedIdentifier,
        candidates: Vec<CandidateOut>,
    },
    /// A toolchain binding projected straight from `ocx.lock` — the locked
    /// platform leaves, nothing resolved and nothing fetched.
    ///
    /// No `pinned`: the lock records one leaf digest per platform and never
    /// the index digest that carried them, so there is no single artifact to
    /// name until `--resolve` picks a platform.
    Locked { candidates: Vec<CandidateOut> },
    Manifest {
        pinned: oci::PinnedIdentifier,
        metadata: Metadata,
        layers: Vec<Layer>,
        closure: Option<ClosureOut>,
    },
    Resolved {
        pinned: oci::PinnedIdentifier,
        platform: oci::Platform,
        metadata: Metadata,
        layers: Vec<Layer>,
        resolution: Resolution,
        closure: Option<ClosureOut>,
    },
}

/// The dependency closure emitted with `--closure`. Everything nests under one
/// object: `deps` (the transitive dependencies in transitive-closure order),
/// `surface` (the interface + private projections), and interface-projection
/// `conflicts`.
#[derive(Serialize)]
struct ClosureOut {
    /// Transitive dependencies in transitive-closure order (deps before
    /// dependents). The inspected root is NOT listed here — it is named by the
    /// top-level `identifier` and appears in each surface's attributions.
    deps: Vec<ClosureDepOut>,
    surface: SurfacesOut,
    conflicts: ConflictsOut,
}

/// One transitive dependency of a [`ClosureOut`], in transitive-closure order.
#[derive(Serialize)]
struct ClosureDepOut {
    /// Short display name — the repository's final path segment (e.g.
    /// `deps-mid`). The flat plain tree labels each dep by this.
    name: String,
    identifier: String,
    digest: String,
    /// Composed-from-root visibility — always present (the root, whose axis is
    /// undefined, is excluded from `deps`).
    effective_visibility: String,
    /// Tri-state, mirrors `Bundle.binaries`: key absent = undeclared,
    /// `Some(empty)` = publisher asserts zero interface executables.
    #[serde(skip_serializing_if = "Option::is_none")]
    binaries: Option<Vec<String>>,
    entrypoints: Vec<String>,
    /// The dep's own declared dependency edges (as authored) — lets a consumer
    /// rebuild the DAG from the flat list.
    dependencies: Vec<ClosureEdgeOut>,
}

/// A declared dependency edge (as authored) inside a [`ClosureDepOut`].
#[derive(Serialize)]
struct ClosureEdgeOut {
    identifier: String,
    visibility: String,
    name: String,
}

/// The two symmetric surface projections of a closure — "what binaries /
/// entrypoints / env keys would land, and on which axis, if this were
/// installed", without installing.
#[derive(Serialize)]
struct SurfacesOut {
    /// Consumer-facing: what reaches someone installing the root.
    interface: SurfaceOut,
    /// Internal: what is visible on the package's own private axis. Public
    /// entries appear in both surfaces (public crosses both axes).
    private: SurfaceOut,
}

/// One projected surface — binaries/entrypoints/env admitted on a single axis.
#[derive(Serialize)]
struct SurfaceOut {
    binaries: Vec<BinaryAttribution>,
    entrypoints: Vec<BinaryAttribution>,
    /// Env keys each admitted node exposes on this axis, attributed to the
    /// declaring package. Values are omitted — they are `${installPath}`-
    /// templated and only concrete after install.
    env: Vec<EnvVarAttribution>,
    /// `false` iff any admitted node has undeclared `binaries`
    /// ("couldn't determine \u{2260} determined zero"). Entrypoints have no
    /// such flag — the entrypoint map keys are always authoritative.
    binaries_complete: bool,
}

/// One env key exposed on the interface surface, attributed to the package that
/// declares it. `type` is the modifier kind (`path` | `constant`). No value —
/// see [`SurfaceOut::env`].
#[derive(Serialize)]
struct EnvVarAttribution {
    key: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
}

impl EnvVarAttribution {
    /// Projects admitted `(identifier, ClosureEnvVar)` pairs into the wire
    /// shape — the env sibling of [`BinaryAttribution::from_pairs`].
    fn from_pairs(pairs: &[(oci::PinnedIdentifier, ClosureEnvVar)]) -> Vec<Self> {
        pairs
            .iter()
            .map(|(identifier, var)| Self {
                key: var.key.clone(),
                kind: var.kind.to_string(),
                package: Some(identifier.to_string()),
            })
            .collect()
    }
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

/// Projects a lib-level metadata closure into the wire shape — one `closure`
/// object with `deps` (non-root nodes in transitive-closure order), the two
/// `surface` projections, and interface-projection `conflicts`. The surface
/// attribution arrays reuse [`BinaryAttribution::from_pairs`] — the same
/// `(PinnedIdentifier, T: Display)` projection `ocx env` / `ocx package env`
/// already use for their admitted-set attribution arrays.
fn project_closure(closure: InspectClosure) -> ClosureOut {
    let InspectClosure {
        nodes,
        interface,
        private,
        conflicts,
    } = closure;

    ClosureOut {
        // The root is excluded from `deps` — it is the inspected package, named
        // by the top-level `identifier` and present in the surface attributions.
        deps: nodes
            .into_iter()
            .filter(|node| !node.is_root)
            .map(closure_dep_out)
            .collect(),
        surface: SurfacesOut {
            interface: surface_out(interface),
            private: surface_out(private),
        },
        conflicts: conflicts_out(conflicts),
    }
}

/// Projects one lib-level [`Surface`] into its wire shape.
fn surface_out(surface: Surface) -> SurfaceOut {
    SurfaceOut {
        binaries: BinaryAttribution::from_pairs(&surface.binaries),
        entrypoints: BinaryAttribution::from_pairs(&surface.entrypoints),
        env: EnvVarAttribution::from_pairs(&surface.env),
        binaries_complete: surface.binaries_complete,
    }
}

/// Projects one non-root lib-level [`ClosureNode`] into a wire `deps` entry.
fn closure_dep_out(node: ClosureNode) -> ClosureDepOut {
    ClosureDepOut {
        name: node.identifier.as_identifier().name().to_string(),
        identifier: node.identifier.to_string(),
        digest: node.identifier.digest().to_string(),
        // A non-root node always carries a composed-from-root visibility; the
        // empty fallback is unreachable (the root is filtered out above).
        effective_visibility: node
            .effective_visibility
            .map(|visibility| visibility.to_string())
            .unwrap_or_default(),
        binaries: node
            .binaries
            .map(|binaries| binaries.iter().map(ToString::to_string).collect()),
        entrypoints: node.entrypoints.iter().map(ToString::to_string).collect(),
        dependencies: node.dependencies.into_iter().map(closure_edge_out).collect(),
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

/// One platform child of an image index, or one locked platform leaf.
#[derive(Serialize)]
struct CandidateOut {
    digest: String,
    /// This candidate as a pullable reference — the entry's identifier with
    /// this child's digest attached. Emitted for the same reason the entry
    /// carries `pinned_identifier`: splicing one by hand means knowing where
    /// the tag goes relative to the digest.
    ///
    /// Named `pinned` rather than `pinned_identifier` because a candidate has
    /// exactly one digest and so nothing to disambiguate against — the same
    /// reason `resolution.pinned` is spelled that way.
    pinned: String,
    platform: String,
    /// Absent for a lock-projected candidate: `ocx.lock` records the leaf
    /// digest per platform, not the descriptor that pointed at it.
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<i64>,
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
    /// Builds one report entry from the task result. `name` is how the caller
    /// addressed this package (the raw request string, or an `ocx.toml`
    /// binding); `identifier` is the requested identifier (post
    /// default-registry expansion); `platform` is the platform resolution
    /// selected against (only meaningful in `--resolve` mode).
    pub fn new(name: String, identifier: oci::Identifier, platform: oci::Platform, result: InspectResult) -> Self {
        match result {
            InspectResult::Candidates { pinned, candidates } => Self {
                name,
                identifier,
                body: Body::Candidates {
                    candidates: candidates
                        .into_iter()
                        .map(|c| CandidateOut {
                            digest: c.identifier.digest().to_string(),
                            pinned: c.identifier.to_string(),
                            platform: c.platform.to_string(),
                            media_type: Some(c.media_type),
                            size: Some(c.size),
                        })
                        .collect(),
                    pinned,
                },
            },
            InspectResult::Manifest {
                pinned,
                metadata,
                layers,
                closure,
            } => Self {
                name,
                identifier,
                body: Body::Manifest {
                    pinned,
                    metadata: metadata.into(),
                    layers: Layer::from_descriptors(&layers),
                    closure: closure.map(project_closure),
                },
            },
            InspectResult::Resolved {
                pinned,
                metadata,
                chain,
                closure,
            } => Self {
                name,
                identifier,
                body: Body::Resolved {
                    pinned,
                    platform,
                    metadata: metadata.into(),
                    layers: Layer::from_descriptors(&chain.final_manifest.layers),
                    resolution: Resolution::from_chain(&chain),
                    closure: closure.map(project_closure),
                },
            },
        }
    }

    /// Builds one entry straight from a locked toolchain binding — the
    /// `ocx inspect` default mode.
    ///
    /// `platforms` is the lock entry's platform-to-leaf map, so this is a pure
    /// projection: no registry read, no host-leaf selection, and therefore no
    /// pinned artifact at the entry level. Each platform becomes a candidate
    /// naming the leaf it would resolve to.
    pub fn locked(
        name: String,
        identifier: oci::Identifier,
        platforms: &std::collections::BTreeMap<String, oci::Digest>,
    ) -> Self {
        let candidates = platforms
            .iter()
            .map(|(platform, digest)| CandidateOut {
                digest: digest.to_string(),
                pinned: identifier.clone_with_digest(digest.clone()).to_string(),
                platform: platform.clone(),
                media_type: None,
                size: None,
            })
            .collect();
        Self {
            name,
            identifier,
            body: Body::Locked { candidates },
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
        }
    }
}

impl PackageInspect {
    /// The single artifact this entry pinned, when it pinned one — the
    /// requested identifier with its resolved digest attached.
    ///
    /// Emitted as its own field so a consumer never has to splice `identifier`
    /// and `pinned_digest` back together (and get the `:tag@digest` punctuation
    /// right) to name the exact artifact. `None` for a lock projection, which
    /// selects nothing: there the per-candidate `pinned_identifier` is the
    /// pullable reference.
    fn pinned_identifier(&self) -> Option<&oci::PinnedIdentifier> {
        match &self.body {
            Body::Candidates { pinned, .. } | Body::Manifest { pinned, .. } | Body::Resolved { pinned, .. } => {
                Some(pinned)
            }
            Body::Locked { .. } => None,
        }
    }

    /// The identifier the plain tree roots at: the pinned one where something
    /// was pinned, else the identifier as addressed.
    ///
    /// A lock projection must root at the DECLARED identifier — the lock
    /// stores the bare repository shared by every platform leaf, so rooting at
    /// anything lock-derived would silently drop the `:tag` every other
    /// inspect view shows.
    fn root_identifier(&self) -> &oci::Identifier {
        self.pinned_identifier()
            .map_or(&self.identifier, oci::PinnedIdentifier::as_identifier)
    }

    /// The whole plain-format tree for this entry: the root identifier with
    /// the shape-appropriate section(s) beneath it.
    fn tree(&self) -> Node {
        let sections = match &self.body {
            Body::Candidates { candidates, .. } | Body::Locked { candidates } => vec![candidates_node(candidates)],
            Body::Manifest {
                metadata,
                layers,
                closure,
                ..
            } => {
                let mut sections = vec![metadata_node(metadata), layers_node(layers)];
                if let Some(closure) = closure {
                    sections.push(closure_node(closure));
                }
                sections
            }
            Body::Resolved {
                platform,
                metadata,
                layers,
                resolution,
                closure,
                ..
            } => {
                let mut sections = vec![
                    metadata_node(metadata),
                    layers_node(layers),
                    resolution_node(resolution, platform),
                ];
                if let Some(closure) = closure {
                    sections.push(closure_node(closure));
                }
                sections
            }
        };
        Node::identifier_branch(self.root_identifier().clone(), sections)
    }
}

impl Serialize for PackageInspect {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Field count varies by body shape; name + identifier are always
        // present, the two pinned fields only when something was pinned.
        // `closure` is additive-optional (present only under `--closure`) and
        // nests deps + surface + conflicts under one key.
        let pinned = self.pinned_identifier();
        let len = 2
            + 2 * usize::from(pinned.is_some())
            + match &self.body {
                Body::Candidates { .. } | Body::Locked { .. } => 1,
                Body::Manifest { closure, .. } => 2 + usize::from(closure.is_some()),
                Body::Resolved { closure, .. } => 4 + usize::from(closure.is_some()),
            };
        let mut s = serializer.serialize_struct("PackageInspect", len)?;
        s.serialize_field("name", &self.name)?;
        s.serialize_field("identifier", &self.identifier)?;
        if let Some(pinned) = pinned {
            s.serialize_field("pinned_identifier", &pinned.to_string())?;
            s.serialize_field("pinned_digest", &pinned.digest().to_string())?;
        }
        match &self.body {
            Body::Candidates { candidates, .. } | Body::Locked { candidates } => {
                s.serialize_field("candidates", candidates)?;
            }
            Body::Manifest {
                metadata,
                layers,
                closure,
                ..
            } => {
                s.serialize_field("metadata", metadata)?;
                s.serialize_field("layers", layers)?;
                if let Some(closure) = closure {
                    s.serialize_field("closure", closure)?;
                }
            }
            Body::Resolved {
                platform,
                metadata,
                layers,
                resolution,
                closure,
                ..
            } => {
                s.serialize_field("platform", platform)?;
                s.serialize_field("metadata", metadata)?;
                s.serialize_field("layers", layers)?;
                s.serialize_field("resolution", resolution)?;
                if let Some(closure) = closure {
                    s.serialize_field("closure", closure)?;
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
    // No media type: every child of an image index is an image manifest, so the
    // column is a constant that pushes the size off a narrow terminal. Size is
    // annotated only when known — a lock projection has no descriptor to read
    // it from.
    let entries = candidates
        .iter()
        .map(|c| {
            let node = Node::leaf(c.platform.clone()).with_digest(c.digest.clone());
            match c.size {
                Some(size) => node.with_note(human_bytes(size)),
                None => node,
            }
        })
        .collect();
    Node::branch("candidates", entries)
}

/// The discriminating tail of a media type — `…image.layer.v1.tar+gzip` renders
/// as `tar+gzip`. Every layer of a package repeats the same 30-character
/// `application/vnd.oci.image.layer.v1` prefix, which pushes the layer size
/// past the right edge; only the suffix tells two layers apart.
fn media_type_suffix(media_type: &str) -> &str {
    media_type.rsplit('.').next().unwrap_or(media_type)
}

/// Renders the manifest's layer descriptors as a `layers` branch — one indexed
/// `[i]` leaf per layer with digest / media type suffix / human size. Shared by
/// the default-manifest and resolved views.
fn layers_node(layers: &[Layer]) -> Node {
    let entries = layers
        .iter()
        .enumerate()
        .map(|(i, layer)| {
            Node::leaf(format!("[{i}]"))
                .with_digest(layer.digest.clone())
                .with_note(media_type_suffix(&layer.media_type).to_string())
                .with_note(human_bytes(layer.size))
        })
        .collect();
    Node::branch("layers", entries)
}

/// Renders the `resolution` branch: the platform the walk selected against, then
/// the OCI walk itself. `platform` is the answer under `--platform` (a libc
/// refinement in particular is invisible anywhere else in the tree), so it is
/// rendered even though it also serializes at the top level.
fn resolution_node(resolution: &Resolution, platform: &oci::Platform) -> Node {
    // Chain entries render like layers — role label, then digest / size — so the
    // OCI walk (index → manifest → config) is legible instead of an opaque
    // positional digest list. The role doubles as the walk-order marker the bare
    // `[i]` used to carry, and as the media type: an `index` role IS
    // `image.index.v1+json`. Layers are not part of the walk; they render under
    // the manifest itself, not here. No `pinned` leaf either — it is the tree
    // root, byte for byte.
    let chain = resolution
        .chain
        .iter()
        .map(|c| {
            Node::leaf(c.role.clone())
                .with_digest(c.digest.clone())
                .with_note(human_bytes(c.size))
        })
        .collect();
    Node::branch(
        "resolution",
        vec![Node::leaf(format!("platform {platform}")), Node::branch("chain", chain)],
    )
}

/// Renders the `closure` flat array as a `(*)`-deduped tree rooted at the
/// inspected package (ADR D2 plain format). Dedup is keyed by content digest
/// (extracted from an edge's `identifier` suffix via [`edge_digest`]), not by
/// the edge's own tag-bearing identifier string — a diamond's shared node
/// renders in full once; every later visit is a `(*)`-marked leaf.
fn closure_node(closure: &ClosureOut) -> Node {
    let mut children = Vec::new();

    // Flat dependency list in transitive-closure order — each dep once, with
    // its composed-from-root visibility. No nested re-tree and no repetition of
    // the inspected root (which already heads the whole inspect tree); the DAG
    // edges live in the JSON `deps[].dependencies` for programmatic use.
    if !closure.deps.is_empty() {
        let deps = closure.deps.iter().map(closure_dep_leaf).collect();
        children.push(Node::branch("deps", deps));
    }

    children.push(surfaces_node(&closure.surface));

    // Interface-projection conflicts render as their own branches, one child per
    // colliding party. This is the one place worth spending vertical space: it
    // fires exactly when the user has a decision to make, and a joined list of
    // pinned identifiers is the widest cell the whole view can produce.
    for conflict in &closure.conflicts.entrypoints {
        let packages = conflict
            .packages
            .iter()
            .map(|p| Node::leaf(without_digest(p)))
            .collect();
        children.push(Node::branch(
            format!("entrypoint '{}' claimed by multiple packages", conflict.name),
            packages,
        ));
    }
    for conflict in &closure.conflicts.repositories {
        let digests = conflict.digests.iter().map(|d| Node::leaf(short_digest(d))).collect();
        children.push(Node::branch(
            format!("repository '{}' resolves to multiple digests", conflict.repository),
            digests,
        ));
    }

    Node::branch("closure", children)
}

/// Renders one dependency as a flat leaf: the short name, the whole identifier
/// as one digest-inked (blue) span, and the composed-from-root visibility tag.
fn closure_dep_leaf(dep: &ClosureDepOut) -> Node {
    let mut leaf = Node::leaf(dep.name.clone()).with_digest(dep.identifier.clone());
    if let Some(visibility) = parse_visibility(&dep.effective_visibility) {
        leaf = leaf.with_visibility(visibility);
    }
    leaf
}

/// A wire identifier string with its digest stripped — `registry/repo:tag`. The
/// conflict leaves answer *which packages* collide; the digest is not that
/// answer, and repository conflicts report digests in their own branch. Falls
/// back to the verbatim string if the wire value does not parse.
fn without_digest(identifier: &str) -> String {
    oci::Identifier::parse(identifier)
        .map_or_else(|_| identifier.to_string(), |parsed| parsed.without_digest().to_string())
}

/// A wire digest string in its canonical short form (`sha256:` + 12 hex). Falls
/// back to the verbatim string if the wire value does not parse.
fn short_digest(digest: &str) -> String {
    oci::Digest::try_from(digest).map_or_else(|_| digest.to_string(), |parsed| parsed.to_short_string())
}

/// The short display name of a wire identifier string — the repository's final
/// path segment, the same name [`ClosureDepOut::name`] carries, so the `deps`
/// branch reads as the legend for every surface attribution. Falls back to the
/// verbatim string if the wire value does not parse.
fn attribution_name(identifier: &str) -> String {
    oci::Identifier::parse(identifier).map_or_else(|_| identifier.to_string(), |parsed| parsed.name().to_string())
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

/// Renders the two surface projections under one `surface` branch: `interface`
/// (consumer axis) and `private` (internal axis).
fn surfaces_node(surfaces: &SurfacesOut) -> Node {
    Node::branch(
        "surface",
        vec![
            surface_node("interface", &surfaces.interface),
            surface_node("private", &surfaces.private),
        ],
    )
}

/// Renders one [`SurfaceOut`] as a labelled branch: binaries + entrypoints + env
/// leaves, plus an incompleteness note when `binaries_complete == false`.
fn surface_node(label: &str, surface: &SurfaceOut) -> Node {
    let mut children = Vec::new();
    if !surface.binaries.is_empty() {
        let leaves = surface.binaries.iter().map(binary_attribution_leaf).collect();
        children.push(Node::branch("binaries", leaves));
    }
    if !surface.entrypoints.is_empty() {
        let leaves = surface.entrypoints.iter().map(binary_attribution_leaf).collect();
        children.push(Node::branch("entrypoints", leaves));
    }
    if !surface.env.is_empty() {
        let leaves = surface.env.iter().map(env_var_attribution_leaf).collect();
        children.push(Node::branch("env", leaves));
    }
    if !surface.binaries_complete {
        // Wording matters: the trigger is an UNDECLARED claim (key absent), not
        // a declared-empty one — `binaries: []` asserts zero and keeps the
        // aggregate complete (tri-state, adr_declared_binaries_metadata.md §1).
        children.push(Node::leaf(
            "binaries incomplete: at least one admitted package leaves binaries undeclared",
        ));
    }
    Node::branch(label.to_string(), children)
}

/// Renders one [`BinaryAttribution`] as a leaf, attributing the claim to the
/// owning package by its short name (see [`attribution_name`]) when known. The
/// full pinned identifier would otherwise repeat once per binary per package —
/// a 3-dependency, 5-binary closure prints it 15 times.
fn binary_attribution_leaf(attribution: &BinaryAttribution) -> Node {
    let leaf = Node::leaf(attribution.name.clone());
    match &attribution.package {
        Some(package) => leaf.with_note(attribution_name(package)),
        None => leaf,
    }
}

/// Renders one [`EnvVarAttribution`] as a leaf: the env key, its modifier kind
/// as a note (`path` / `constant`), and the owning package's short name when
/// attribution is known.
fn env_var_attribution_leaf(attribution: &EnvVarAttribution) -> Node {
    let leaf = Node::leaf(attribution.key.clone()).with_note(attribution.kind.clone());
    match &attribution.package {
        Some(package) => leaf.with_note(attribution_name(package)),
        None => leaf,
    }
}

impl Printable for PackageInspect {
    // Inspect output is inherently structured (nested sections), not a single
    // table — same tree exemption `deps` uses.
    fn print_plain(&self, data: &DataInterface) {
        data.print_tree(&self.tree());
    }
}

/// The report both inspect commands emit: `ocx package inspect` over raw
/// identifiers, and `ocx inspect` over an `ocx.toml` toolchain selection.
///
/// Plain format: each package's tree rendered in order (inspect holds the
/// single-table exemption — its output is inherently a nested tree, not a row).
///
/// JSON format: `{ platform?, packages: [...], env: [...] }`.
///
/// `platform` is present only when the run actually selected a platform —
/// `--resolve` / `--closure`, on either command. Default mode lists candidates
/// without selecting, so there is no platform to report and `-p` stays inert,
/// exactly as its help promises.
///
/// `packages` is an **array**, not an object keyed by request string: entry
/// order is meaningful (input order for `ocx package inspect`, selection order
/// for `ocx inspect`) and JSON object key order is not guaranteed. Each entry
/// names itself via its `name` field.
///
/// `env` is the composed project-tier environment in application order —
/// `[env]`, then each selected group's `[group.<name>.env]` in `-g` order, then
/// `--env`. `ocx package inspect` reads no `ocx.toml`, so its `env` carries the
/// `--env` overrides alone. Package-declared env is NOT here: it lives inside
/// each entry's `closure.surface.env`, where it is attributed per package and
/// carries no value (those are `${installPath}`-templated and only concrete
/// after install). The array is always present, empty when nothing applies.
pub struct InspectReport {
    platform: Option<String>,
    packages: Vec<PackageInspect>,
    env: Vec<EnvEntry>,
}

impl InspectReport {
    /// `platform` is `Some` only when the run selected one. Passing it
    /// unconditionally would make `-p` observable in a default-mode
    /// `ocx package inspect`, which selects no platform at all — the report
    /// would then name a platform nothing was resolved against.
    pub fn new(platform: Option<&oci::Platform>, packages: Vec<PackageInspect>, env: Vec<EnvEntry>) -> Self {
        Self {
            platform: platform.map(ToString::to_string),
            packages,
            env,
        }
    }

    /// Every interface-projection conflict the closure walk detected, across
    /// every entry.
    ///
    /// Empty means the selection is realizable. A non-empty result is what
    /// drives the command's exit code — install/compose would hard-reject this
    /// set, so inspect reports it AND exits non-zero rather than handing back a
    /// green that hides an unusable surface.
    pub fn has_conflicts(&self) -> bool {
        self.packages.iter().any(|package| {
            let closure = match &package.body {
                Body::Candidates { .. } | Body::Locked { .. } => None,
                Body::Manifest { closure, .. } | Body::Resolved { closure, .. } => closure.as_ref(),
            };
            closure.is_some_and(|closure| {
                !closure.conflicts.entrypoints.is_empty() || !closure.conflicts.repositories.is_empty()
            })
        })
    }
}

impl Serialize for InspectReport {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("InspectReport", 2 + usize::from(self.platform.is_some()))?;
        if let Some(platform) = &self.platform {
            s.serialize_field("platform", platform)?;
        }
        s.serialize_field("packages", &self.packages)?;
        s.serialize_field("env", &self.env)?;
        s.end()
    }
}

impl Printable for InspectReport {
    fn print_plain(&self, data: &DataInterface) {
        for inspect in &self.packages {
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

    // ── `--closure` projection: closure { deps, surface, conflicts } ─────────
    //
    // The wire projection (`project_closure`) and the plain-render helpers
    // (`closure_node`, `surface_node`) map a hand-built lib-level
    // [`InspectClosure`] into the nested `closure` object and its flat tree.
    // The interface-vs-private axis FILTERING is a lib concern (tested in
    // `package_manager::tasks::inspect`); these tests pin the WIRE shape and
    // the plain render.

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

    fn env_var(key: &str, kind: ModifierKind, visibility: Visibility) -> ClosureEnvVar {
        ClosureEnvVar {
            key: key.to_string(),
            kind,
            visibility,
        }
    }

    /// Builds a minimal `Manifest`-mode `InspectResult` carrying `closure`
    /// (or `None`, the no-`--closure` case).
    fn manifest_result(root: oci::PinnedIdentifier, closure: Option<InspectClosure>) -> InspectResult {
        InspectResult::Manifest {
            pinned: root,
            metadata: ValidMetadata::try_from(bundle_metadata(None)).expect("bare bundle metadata is always valid"),
            layers: vec![],
            closure,
        }
    }

    /// A non-root closure node with the given composed-from-root visibility.
    fn dep_node(identifier: oci::PinnedIdentifier, effective_visibility: Visibility) -> ClosureNode {
        ClosureNode {
            identifier,
            effective_visibility: Some(effective_visibility),
            binaries: None,
            entrypoints: vec![],
            env: vec![],
            dependencies: vec![],
            is_root: false,
        }
    }

    /// The root closure node — no composed-from-root visibility.
    fn root_node(identifier: oci::PinnedIdentifier) -> ClosureNode {
        ClosureNode {
            identifier,
            effective_visibility: None,
            binaries: None,
            entrypoints: vec![],
            env: vec![],
            dependencies: vec![],
            is_root: true,
        }
    }

    /// A lib-level [`Surface`] with only env entries (binaries/entrypoints
    /// empty), the common shape the wire tests need.
    fn surface_with_env(env: Vec<(oci::PinnedIdentifier, ClosureEnvVar)>, binaries_complete: bool) -> Surface {
        Surface {
            binaries: vec![],
            entrypoints: vec![],
            env,
            binaries_complete,
        }
    }

    fn empty_surface(binaries_complete: bool) -> Surface {
        surface_with_env(vec![], binaries_complete)
    }

    fn closure_of(nodes: Vec<ClosureNode>, interface: Surface, private: Surface) -> InspectClosure {
        InspectClosure {
            nodes,
            interface,
            private,
            conflicts: ClosureConflicts::default(),
        }
    }

    /// An empty wire [`SurfaceOut`] for the render tests.
    fn empty_surface_out(binaries_complete: bool) -> SurfaceOut {
        SurfaceOut {
            binaries: vec![],
            entrypoints: vec![],
            env: vec![],
            binaries_complete,
        }
    }

    // ── Lock projection (`ocx inspect` default mode) ──────────────────────

    /// The lock's platform-to-leaf map becomes the `candidates` array, one
    /// entry per platform, each naming the pullable reference so a consumer
    /// never splices `identifier` and `digest` by hand.
    #[test]
    fn json_locked_projects_every_platform_as_a_candidate() {
        let platforms = std::collections::BTreeMap::from([
            ("linux/amd64".to_string(), oci::Digest::Sha256("a".repeat(64))),
            ("darwin/arm64".to_string(), oci::Digest::Sha256("b".repeat(64))),
        ]);
        let report = PackageInspect::locked("toolchain".into(), test_identifier(), &platforms);
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");

        assert_eq!(value["name"], "toolchain", "the entry names itself by binding");
        assert_eq!(value["identifier"], "example.com/toolchain:1.0");
        let candidates = value["candidates"].as_array().expect("candidates is an array");
        assert_eq!(
            candidates.iter().map(|c| c["platform"].as_str()).collect::<Vec<_>>(),
            [Some("darwin/arm64"), Some("linux/amd64")],
            "candidates follow the lock's canonical platform-key order: {candidates:?}"
        );
        assert_eq!(
            candidates[1]["pinned"],
            format!("example.com/toolchain:1.0@sha256:{}", "a".repeat(64)),
            "each candidate is a pullable reference"
        );
    }

    /// Nothing was selected, so nothing is pinned at the entry level and no
    /// descriptor was read: `pinned_identifier`, `pinned_digest`, `media_type`
    /// and `size` are all absent rather than faked.
    ///
    /// Absence is the signal — the same convention `ClosureDepOut.binaries`
    /// uses. A zero size or an empty pinned string would read as a measured
    /// value.
    #[test]
    fn json_locked_omits_what_the_lock_does_not_record() {
        let platforms =
            std::collections::BTreeMap::from([("linux/amd64".to_string(), oci::Digest::Sha256("a".repeat(64)))]);
        let report = PackageInspect::locked("toolchain".into(), test_identifier(), &platforms);
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        let object = value.as_object().expect("top-level JSON is an object");

        assert!(
            !object.contains_key("pinned_identifier") && !object.contains_key("pinned_digest"),
            "a lock projection selects no artifact, so it pins none: {value}"
        );
        let candidate = value["candidates"][0].as_object().expect("candidate is an object");
        assert!(
            !candidate.contains_key("media_type") && !candidate.contains_key("size"),
            "the lock records leaf digests, not descriptors: {value}"
        );
    }

    /// A lock projection roots its plain tree at the DECLARED identifier —
    /// tag and all. The lock stores the bare repository, so rooting anywhere
    /// lock-derived drops the `:tag` every other inspect view shows.
    ///
    /// Asserts on `tree()`, the node `print_plain` actually renders, not on a
    /// hand-assembled equivalent.
    #[test]
    fn locked_plain_tree_roots_at_the_declared_identifier() {
        let platforms =
            std::collections::BTreeMap::from([("linux/amd64".to_string(), oci::Digest::Sha256("a".repeat(64)))]);
        let report = PackageInspect::locked("toolchain".into(), test_identifier(), &platforms);

        let root = report.tree();
        assert_eq!(
            root.identifier.as_ref().map(ToString::to_string),
            Some("example.com/toolchain:1.0".to_string()),
            "the root keeps the declared tag"
        );
        let mut text = Vec::new();
        collect_node_text(&root, &mut text);
        assert!(
            text.iter().any(|entry| entry == "candidates"),
            "the only section is the candidate listing: {text:?}"
        );
        assert!(
            !text.iter().any(|entry| entry.contains("iB") || entry.ends_with(" B")),
            "no size annotation without a descriptor to read one from: {text:?}"
        );
    }

    /// The `--resolve` sibling: where something WAS pinned, the tree roots at
    /// the pinned identifier, so this test and the one above pin both sides of
    /// `root_identifier`'s branch.
    #[test]
    fn resolved_plain_tree_roots_at_the_pinned_identifier() {
        let root = pinned("toolchain", 'a');
        let report = PackageInspect::new(
            "toolchain".into(),
            test_identifier(),
            test_platform(),
            manifest_result(root, None),
        );
        assert_eq!(
            report.tree().identifier.as_ref().map(ToString::to_string),
            Some(format!("example.com/toolchain@sha256:{}", "a".repeat(64))),
        );
    }

    // ── JSON projection ───────────────────────────────────────────────────

    /// Backward-compat pin ("existing inspect bodies byte-unchanged without
    /// `--closure`"): with no closure requested, the top-level JSON object
    /// must not carry a `closure` key at all.
    #[test]
    fn json_closure_key_absent_without_closure_flag() {
        let root = pinned("toolchain", 'a');
        let report = PackageInspect::new(
            "test".into(),
            test_identifier(),
            test_platform(),
            manifest_result(root, None),
        );
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        assert!(
            !value
                .as_object()
                .expect("top-level JSON is an object")
                .contains_key("closure"),
            "no --closure requested, closure key must be absent: {value}"
        );
    }

    /// `closure.deps` lists the transitive dependencies (never the root) in
    /// transitive-closure order, each carrying its composed-from-root
    /// `effective_visibility`.
    #[test]
    fn json_closure_deps_exclude_root_and_carry_effective_visibility() {
        let root = pinned("root", 'a');
        let dep = pinned("dep", 'b');
        let closure = closure_of(
            vec![dep_node(dep, Visibility::PUBLIC), root_node(root.clone())],
            empty_surface(true),
            empty_surface(true),
        );
        let report = PackageInspect::new(
            "test".into(),
            test_identifier(),
            test_platform(),
            manifest_result(root, Some(closure)),
        );
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");

        let deps = value["closure"]["deps"].as_array().expect("closure.deps is an array");
        assert_eq!(deps.len(), 1, "the root is excluded from deps: {deps:?}");
        assert!(
            deps.iter()
                .all(|d| !d["identifier"].as_str().unwrap_or_default().contains("root")),
            "root must never appear in deps: {deps:?}"
        );
        let dep_entry = &deps[0];
        assert!(dep_entry["identifier"].as_str().unwrap_or_default().contains("dep"));
        assert_eq!(dep_entry["effective_visibility"], "public");
    }

    /// Tri-state `binaries` wire contract per dep: key absent for undeclared,
    /// `[]` for an explicit empty claim, `[names...]` for a declared claim.
    #[test]
    fn json_closure_dep_binaries_tri_state() {
        let root = pinned("root", 'a');
        // Marker names must not be substrings of one another — `find` matches
        // by `contains`, and "no-claim-dep".contains("claim-dep") is true.
        let undeclared = pinned("no-claim-dep", 'b');
        let empty = pinned("zero-claim-dep", 'c');
        let declared = pinned("named-claim-dep", 'd');

        let closure = closure_of(
            vec![
                ClosureNode {
                    binaries: None,
                    ..dep_node(undeclared, Visibility::PUBLIC)
                },
                ClosureNode {
                    binaries: Some(binaries_of(&[])),
                    ..dep_node(empty, Visibility::PUBLIC)
                },
                ClosureNode {
                    binaries: Some(binaries_of(&["x"])),
                    ..dep_node(declared, Visibility::PUBLIC)
                },
                root_node(root.clone()),
            ],
            empty_surface(false),
            empty_surface(false),
        );
        let report = PackageInspect::new(
            "test".into(),
            test_identifier(),
            test_platform(),
            manifest_result(root, Some(closure)),
        );
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        let deps = value["closure"]["deps"].as_array().expect("closure.deps is an array");
        let find = |marker: &str| {
            deps.iter()
                .find(|d| d["identifier"].as_str().unwrap_or_default().contains(marker))
                .unwrap_or_else(|| panic!("no dep matches '{marker}': {deps:?}"))
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

    /// `closure.surface` carries BOTH the `interface` and `private`
    /// projections (each with the four keys), and `closure.conflicts` is
    /// always present — never omitted, even when empty.
    #[test]
    fn json_closure_surface_carries_interface_private_and_conflicts() {
        let root = pinned("root", 'a');
        let closure = closure_of(vec![root_node(root.clone())], empty_surface(true), empty_surface(true));
        let report = PackageInspect::new(
            "test".into(),
            test_identifier(),
            test_platform(),
            manifest_result(root, Some(closure)),
        );
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        let closure_val = value["closure"].as_object().expect("closure is an object");

        assert!(closure_val.contains_key("deps"), "closure carries a deps array");
        let surface = closure_val["surface"].as_object().expect("surface object present");
        for axis in ["interface", "private"] {
            let projection = surface
                .get(axis)
                .and_then(serde_json::Value::as_object)
                .unwrap_or_else(|| panic!("surface.{axis} object present: {surface:?}"));
            assert!(projection.get("binaries").is_some_and(serde_json::Value::is_array));
            assert!(projection.get("entrypoints").is_some_and(serde_json::Value::is_array));
            assert!(projection.get("env").is_some_and(serde_json::Value::is_array));
            assert!(
                projection
                    .get("binaries_complete")
                    .is_some_and(serde_json::Value::is_boolean)
            );
        }
        let conflicts = closure_val["conflicts"]
            .as_object()
            .expect("closure.conflicts object present");
        assert!(conflicts.get("entrypoints").is_some_and(serde_json::Value::is_array));
        assert!(conflicts.get("repositories").is_some_and(serde_json::Value::is_array));
    }

    /// A surface `env` array projects each exposed env key with its modifier
    /// kind under `type` and the declaring package under `package`.
    #[test]
    fn json_closure_surface_env_carries_key_type_and_package() {
        let root = pinned("root", 'a');
        let dep = pinned("dep", 'b');
        let interface = surface_with_env(
            vec![
                (root.clone(), env_var("PATH", ModifierKind::Path, Visibility::PUBLIC)),
                (
                    dep.clone(),
                    env_var("DEP_HOME", ModifierKind::Constant, Visibility::PUBLIC),
                ),
            ],
            true,
        );
        let closure = closure_of(
            vec![dep_node(dep, Visibility::PUBLIC), root_node(root.clone())],
            interface,
            empty_surface(true),
        );
        let report = PackageInspect::new(
            "test".into(),
            test_identifier(),
            test_platform(),
            manifest_result(root, Some(closure)),
        );
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        let env = value["closure"]["surface"]["interface"]["env"]
            .as_array()
            .expect("closure.surface.interface.env is an array");

        let path = env.iter().find(|e| e["key"] == "PATH").expect("PATH env entry present");
        assert_eq!(path["type"], "path", "modifier kind serializes under `type`");
        assert!(
            path["package"].as_str().unwrap_or_default().contains("root"),
            "env entry carries its declaring package: {path}"
        );
        let dep_home = env
            .iter()
            .find(|e| e["key"] == "DEP_HOME")
            .expect("DEP_HOME env entry present");
        assert_eq!(dep_home["type"], "constant");
        assert!(dep_home["package"].as_str().unwrap_or_default().contains("dep"));
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

    /// Collect every annotation across a rendered subtree.
    fn collect_annotations<'a>(node: &'a Node, out: &mut Vec<&'a SemanticAnnotation>) {
        out.extend(node.annotations.iter());
        for child in &node.children {
            collect_annotations(child, out);
        }
    }

    /// One wire `deps` entry with the given short name / digest hex / visibility.
    fn dep_out(name: &str, hex: char, visibility: &str) -> ClosureDepOut {
        ClosureDepOut {
            name: name.to_string(),
            identifier: format!("example.com/{name}:1.0@{}", fake_digest(hex)),
            digest: fake_digest(hex),
            effective_visibility: visibility.to_string(),
            binaries: None,
            entrypoints: vec![],
            dependencies: vec![],
        }
    }

    /// The `closure` branch renders a FLAT `deps` list — each dep once, labelled
    /// by short name with the whole identifier digest-inked and its visibility
    /// tagged — plus a `surface` branch. No `(*)` markers, no root repetition,
    /// no nesting of a dep's own dependencies.
    #[test]
    fn closure_node_renders_flat_deps_with_visibility_and_surface_branch() {
        let closure = ClosureOut {
            deps: vec![
                dep_out("deps-mid", 'm', "interface"),
                dep_out("deps-leaf", 'l', "public"),
            ],
            surface: SurfacesOut {
                interface: empty_surface_out(true),
                private: empty_surface_out(true),
            },
            conflicts: ConflictsOut {
                entrypoints: vec![],
                repositories: vec![],
            },
        };

        let node = closure_node(&closure);
        assert_eq!(node.label, "closure");
        let top_labels: Vec<&str> = node.children.iter().map(|child| child.label.as_str()).collect();
        assert!(top_labels.contains(&"deps"), "a deps branch is present: {top_labels:?}");
        assert!(
            top_labels.contains(&"surface"),
            "a surface branch is present: {top_labels:?}"
        );

        let mut text = Vec::new();
        collect_node_text(&node, &mut text);
        let joined = text.join(" | ");
        assert!(joined.contains("deps-mid") && joined.contains("deps-leaf"));
        assert!(
            !joined.contains("(*)"),
            "the flat deps list carries no (*) markers: {joined}"
        );
        assert_eq!(
            text.iter().filter(|t| t.contains(&fake_digest('m'))).count(),
            1,
            "each dep identifier renders exactly once (no re-expansion): {joined}"
        );

        // Each dep leaf carries a whole-identifier digest annotation and a
        // visibility tag.
        let mut annotations = Vec::new();
        collect_annotations(&node, &mut annotations);
        assert!(
            annotations
                .iter()
                .any(|a| matches!(a, SemanticAnnotation::Digest(text) if text.contains("deps-mid"))),
            "a dep's identifier is a whole-identifier digest span"
        );
        assert!(
            annotations
                .iter()
                .any(|a| matches!(a, SemanticAnnotation::Visibility(_))),
            "a dep carries its composed-from-root visibility tag"
        );
    }

    /// Interface-projection conflicts render under the `closure` branch as
    /// their own branches — the colliding entrypoint / repository names the
    /// branch, and each colliding party is one child leaf, shortened
    /// (identifier without digest / short digest). Never one joined cell: a
    /// joined list of pinned identifiers is the widest line the view can
    /// produce, and this fires exactly when the user must decide something.
    #[test]
    fn closure_node_renders_conflicts_as_child_leaves() {
        let closure = ClosureOut {
            deps: vec![],
            surface: SurfacesOut {
                interface: empty_surface_out(true),
                private: empty_surface_out(true),
            },
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

        let node = closure_node(&closure);
        let find = |marker: &str| {
            node.children
                .iter()
                .find(|child| child.label.contains(marker))
                .unwrap_or_else(|| panic!("no conflict branch names '{marker}'"))
        };

        let entrypoint = find("shared-ep");
        let packages: Vec<&str> = entrypoint.children.iter().map(|child| child.label.as_str()).collect();
        assert_eq!(
            packages,
            vec!["example.com/a", "example.com/b"],
            "one leaf per colliding package, digest stripped"
        );
        assert!(
            entrypoint.annotations.is_empty(),
            "the colliding packages are children, never one joined annotation"
        );

        let repository = find("shared-lib");
        let digests: Vec<&str> = repository.children.iter().map(|child| child.label.as_str()).collect();
        assert_eq!(
            digests,
            vec!["sha256:eeeeeeeeeeee", "sha256:ffffffffffff"],
            "one leaf per colliding digest, shortened to 12 hex"
        );
        assert!(
            repository.annotations.is_empty(),
            "the colliding digests are children, never one joined annotation"
        );
    }

    /// A labelled `surface` branch renders declared binaries/entrypoints/env as
    /// leaves and, when `binaries_complete == false`, an incompleteness note.
    #[test]
    fn surface_node_renders_binaries_entrypoints_env_and_incomplete_note() {
        let surface = SurfaceOut {
            binaries: BinaryAttribution::from_pairs(&[(pinned("cmake", 'a'), "cmake".to_string())]),
            entrypoints: BinaryAttribution::from_pairs(&[(pinned("toolchain", 'b'), "cc".to_string())]),
            env: EnvVarAttribution::from_pairs(&[(
                pinned("cmake", 'a'),
                env_var("CMAKE_ROOT", ModifierKind::Constant, Visibility::PUBLIC),
            )]),
            binaries_complete: false,
        };

        let node = surface_node("interface", &surface);
        assert_eq!(node.label, "interface");

        let mut text = Vec::new();
        collect_node_text(&node, &mut text);
        let joined = text.join(" | ");

        assert!(
            joined.contains("cmake"),
            "declared binary name renders as a leaf: {joined}"
        );
        assert!(
            joined.contains("cc"),
            "declared entrypoint name renders as a leaf: {joined}"
        );
        assert!(
            joined.contains("CMAKE_ROOT"),
            "an exposed env key renders as a leaf under the env branch: {joined}"
        );
        assert!(
            joined.contains("binaries incomplete: at least one admitted package leaves binaries undeclared"),
            "binaries_complete=false renders the undeclared-claim note verbatim — \
             the wording must name the UNDECLARED case, not read as declared-zero: {joined}"
        );
    }

    /// Surface attribution names the owning package by its short name — the
    /// same name the `deps` branch labels it with, so that branch reads as the
    /// legend. The pinned identifier is never re-printed per claim: it would
    /// otherwise repeat once per binary per package (89 columns each).
    #[test]
    fn surface_attribution_names_package_by_short_name() {
        let surface = SurfaceOut {
            binaries: BinaryAttribution::from_pairs(&[(pinned("toolchain/cmake", 'a'), "cmake".to_string())]),
            entrypoints: vec![],
            env: EnvVarAttribution::from_pairs(&[(
                pinned("toolchain/cmake", 'a'),
                env_var("CMAKE_ROOT", ModifierKind::Constant, Visibility::PUBLIC),
            )]),
            binaries_complete: true,
        };
        let node = surface_node("interface", &surface);

        let mut annotations = Vec::new();
        collect_annotations(&node, &mut annotations);
        assert_eq!(
            annotations
                .iter()
                .filter(|a| matches!(a, SemanticAnnotation::Note(text) if text == "cmake"))
                .count(),
            2,
            "the binary leaf and the env leaf each attribute to the package's short name"
        );

        let mut text = Vec::new();
        collect_node_text(&node, &mut text);
        let joined = text.join(" | ");
        assert!(
            !joined.contains(&fake_digest('a')),
            "the pinned identifier must not be re-printed per claim: {joined}"
        );
    }

    /// The completeness note is conditional — a complete surface
    /// (`binaries_complete == true`) must not render an "incomplete" note.
    ///
    /// The zero-binaries + complete shape is exactly what a declared-empty
    /// claim (`binaries: []`) projects to: asserted zero is honest, not a
    /// gap, so no note — distinct from the UNDECLARED case (key absent),
    /// which flips `binaries_complete` and renders the note (tri-state,
    /// `adr_declared_binaries_metadata.md` §1).
    #[test]
    fn surface_node_omits_incomplete_note_when_complete() {
        let node = surface_node("private", &empty_surface_out(true));
        let mut text = Vec::new();
        collect_node_text(&node, &mut text);
        let joined = text.join(" | ");
        assert!(
            !joined.to_lowercase().contains("incomplete"),
            "a complete surface must not render an incomplete note: {joined}"
        );
    }
}
