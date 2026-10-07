// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Serialize, ser::SerializeStruct};

use ocx_console::{Annotation, DataInterface, Theme, TreeItem, human_bytes};
use ocx_oci::digest::error::DigestError;
use ocx_oci::platform::error::PlatformError;
use ocx_oci::{Digest, PinnedPackageRef, Platform};
use ocx_package::metadata::{Binaries, Metadata, env::modifier::ModifierKind, visibility::Visibility};
use ocx_package_manager::{
    ClosureConflicts, ClosureEdge, ClosureEnvVar, ClosureNode, InspectClosure, InspectResult, ResolvedChain, Surface,
};
use ocx_util::size::ByteSize;

use crate::api::{
    Printable,
    data::env::{BinaryAttribution, EnvEntry},
};

/// Semantic role of a tree annotation; the [`Theme`] inks each role from one palette entry at render time.
#[derive(Clone)]
enum SemanticAnnotation {
    /// A content digest, or a whole digest-bearing identifier inked as one span so it reads as a single aside.
    Digest(String),
    /// An env-entry visibility tag, in the same palette entry as everywhere else.
    Visibility(Visibility),
    /// A short note next to a value: modifier kind, media type, byte size or dispatch-command divergence.
    Note(String),
    /// Carried verbatim with the renderer's default annotation style.
    Plain(String),
}

impl SemanticAnnotation {
    fn ink(&self, theme: &Theme) -> Annotation {
        match self {
            SemanticAnnotation::Digest(text) => Annotation::new(theme.digest(text)),
            SemanticAnnotation::Visibility(visibility) => Annotation::new(
                theme.visibility(crate::api::data::visibility_style(*visibility), visibility.to_string()),
            ),
            SemanticAnnotation::Note(text) => Annotation::new(theme.note(text)),
            SemanticAnnotation::Plain(text) => Annotation::new(text.clone()),
        }
    }
}

/// Read-only view of a package, one `Body` variant per shape of reference and `--resolve`.
/// A shape that selected one artifact adds `pinned_identifier` and `pinned_digest`.
pub struct PackageInspect {
    name: String,
    identifier: ocx_oci::PackageRef,
    body: Body,
}

enum Body {
    /// Default mode over an image index: its platform children.
    Candidates {
        pinned: ocx_oci::PinnedPackageRef,
        candidates: Vec<CandidateOut>,
    },
    /// A toolchain binding projected straight from `ocx.lock`, nothing resolved or fetched.
    /// No `pinned`: the lock never records the index digest, so there is no single artifact to name.
    Locked { candidates: Vec<CandidateOut> },
    /// Default mode over a single manifest: `{ …, metadata, layers }`.
    Manifest {
        pinned: ocx_oci::PinnedPackageRef,
        metadata: Metadata,
        layers: Vec<Layer>,
        closure: Option<ClosureOut>,
    },
    /// `--resolve`: `{ …, platform, metadata, layers, resolution }`.
    Resolved {
        pinned: ocx_oci::PinnedPackageRef,
        platform: ocx_oci::Platform,
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
#[derive(Serialize, schemars::JsonSchema)]
struct ClosureOut {
    /// Transitive dependencies in transitive-closure order (deps before
    /// dependents). The inspected root is NOT listed here — it is named by the
    /// top-level `identifier` and appears in each surface's attributions.
    deps: Vec<ClosureDepOut>,
    /// What would land on each axis if the root were installed.
    surface: SurfacesOut,
    /// Conditions on the interface projection that install would refuse.
    conflicts: ConflictsOut,
}

/// One transitive dependency in the `--closure` object, in transitive-closure order.
#[derive(Serialize, schemars::JsonSchema)]
struct ClosureDepOut {
    /// Short display name — the repository's final path segment (e.g.
    /// `deps-mid`). The flat plain tree labels each dep by this.
    name: String,
    /// Always digest-pinned — a closure node is a resolved artifact, never a
    /// tag. There is no separate `digest` key because this one already ends in
    /// it.
    identifier: PinnedPackageRef,
    /// The visibility composed from the root down to this dependency.
    // Set on every dep: the root, whose axis is undefined, is excluded from `deps`.
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_visibility: Option<Visibility>,
    /// Tri-state, mirrors `Bundle.binaries`: key absent = undeclared,
    /// `Some(empty)` = publisher asserts zero interface executables.
    #[serde(skip_serializing_if = "Option::is_none")]
    binaries: Option<Vec<String>>,
    /// The dep's declared entrypoint names.
    entrypoints: Vec<String>,
    /// The dep's own declared integration namespace keys, lexicographically
    /// ordered. Keys only — a closure node is not installed, so
    /// `${installPath}` has no value and an interpolated payload would be a
    /// half-truth. Always present, `[]` when the dep declares none: absent and
    /// empty are the same state here (unlike `binaries`' tri-state).
    integrations: Vec<String>,
    /// The dep's own declared dependency edges (as authored) — lets a consumer
    /// rebuild the DAG from the flat list.
    dependencies: Vec<ClosureEdgeOut>,
}

/// A declared dependency edge (as authored) of one closure dependency.
#[derive(Serialize, schemars::JsonSchema)]
struct ClosureEdgeOut {
    /// The dependency, digest-pinned.
    identifier: PinnedPackageRef,
    /// The visibility the edge declares.
    visibility: Visibility,
    /// The dependency's name as the edge declares it.
    name: String,
}

/// The two symmetric surface projections of a closure — "what binaries /
/// entrypoints / env keys would land, and on which axis, if this were
/// installed", without installing.
#[derive(Serialize, schemars::JsonSchema)]
struct SurfacesOut {
    /// Consumer-facing: what reaches someone installing the root.
    interface: SurfaceOut,
    /// Internal: what is visible on the package's own private axis. Public
    /// entries appear in both surfaces (public crosses both axes).
    private: SurfaceOut,
}

/// One projected surface — binaries/entrypoints/env/integrations admitted on
/// a single axis.
#[derive(Serialize, schemars::JsonSchema)]
struct SurfaceOut {
    /// Admitted `binaries` claims on this axis, attributed to their packages.
    binaries: Vec<BinaryAttribution>,
    /// Admitted `entrypoints` claims on this axis, attributed to their packages.
    entrypoints: Vec<BinaryAttribution>,
    /// Env keys each admitted node exposes on this axis, attributed to the
    /// declaring package. Values are omitted — they are `${installPath}`-
    /// templated and only concrete after install.
    env: Vec<EnvVarAttribution>,
    /// Integration namespace keys each admitted node declares, attributed to
    /// the declaring package. Payload-free — a closure node is not installed,
    /// so `${installPath}` has no value and the payload would be a half-truth,
    /// the same reason `env` omits values.
    integrations: Vec<NamespaceAttribution>,
    /// `false` iff any admitted node has undeclared `binaries`
    /// ("couldn't determine \u{2260} determined zero"). Entrypoints have no
    /// such flag — the entrypoint map keys are always authoritative.
    binaries_complete: bool,
}

/// One env key exposed on the interface surface, attributed to the package that
/// declares it. No value: values are `${installPath}`-templated and only concrete after install.
#[derive(Serialize, schemars::JsonSchema)]
struct EnvVarAttribution {
    /// The environment variable name.
    key: String,
    /// How the value combines with the variable's existing value.
    kind: ModifierKind,
    /// The declared separator for a `list`-kind entry; `None` for every other
    /// kind. Skipped in JSON when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    separator: Option<String>,
    /// The declaring package.
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
}

impl EnvVarAttribution {
    /// Projects admitted `(identifier, ClosureEnvVar)` pairs into the wire shape.
    fn from_pairs(pairs: &[(ocx_oci::PinnedPackageRef, ClosureEnvVar)]) -> Vec<Self> {
        pairs
            .iter()
            .map(|(identifier, var)| Self {
                key: var.key.clone(),
                kind: var.kind.clone(),
                separator: var.separator.clone(),
                package: Some(identifier.to_string()),
            })
            .collect()
    }
}

// `namespace` matches the flat `ocx env` envelope's key: one concept, one spelling (`adr_package_integrations.md`).
/// One integration namespace a closure node declares, attributed to the
/// declaring package.
///
/// No `value`: the closure envelope is payload-free — a closure node is not
/// installed, so `${installPath}` has no value.
#[derive(Serialize, schemars::JsonSchema)]
struct NamespaceAttribution {
    /// The integration namespace key.
    namespace: String,
    /// The declaring package.
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
}

impl NamespaceAttribution {
    /// Projects admitted `(identifier, namespace key)` pairs into the wire shape.
    fn from_pairs(pairs: &[(ocx_oci::PinnedPackageRef, String)]) -> Vec<Self> {
        pairs
            .iter()
            .map(|(identifier, namespace)| Self {
                namespace: namespace.clone(),
                package: Some(identifier.to_string()),
            })
            .collect()
    }
}

/// Install/compose-gate conditions detected over the interface projection.
/// Both arrays always present; empty means the surface is
/// realizable. Inspect stays a view, not a gate — exit 0 either way.
#[derive(Serialize, schemars::JsonSchema)]
struct ConflictsOut {
    /// Entrypoint names claimed by more than one package.
    entrypoints: Vec<EntrypointConflictOut>,
    /// Repositories resolved to more than one digest.
    repositories: Vec<RepositoryConflictOut>,
}

/// Two or more interface-admitted closure nodes declare the same entrypoint
/// name.
#[derive(Serialize, schemars::JsonSchema)]
struct EntrypointConflictOut {
    /// The contested entrypoint name.
    name: String,
    /// Every package claiming it.
    packages: Vec<String>,
}

/// One repository resolved to two or more distinct digests on the interface
/// projection.
#[derive(Serialize, schemars::JsonSchema)]
struct RepositoryConflictOut {
    /// The contested repository.
    repository: String,
    /// Every digest it resolved to.
    digests: Vec<Digest>,
}

/// Projects a lib-level metadata closure into the wire shape: `deps`, both `surface` views and `conflicts`.
fn project_closure(closure: InspectClosure) -> ClosureOut {
    let InspectClosure {
        nodes,
        interface,
        private,
        conflicts,
    } = closure;

    ClosureOut {
        // The root is not a dep: the top-level `identifier` already names it.
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

fn surface_out(surface: Surface) -> SurfaceOut {
    SurfaceOut {
        binaries: BinaryAttribution::from_pairs(&surface.binaries),
        entrypoints: BinaryAttribution::from_pairs(&surface.entrypoints),
        env: EnvVarAttribution::from_pairs(&surface.env),
        integrations: NamespaceAttribution::from_pairs(&surface.integrations),
        binaries_complete: surface.binaries_complete,
    }
}

fn closure_dep_out(node: ClosureNode) -> ClosureDepOut {
    ClosureDepOut {
        name: node.identifier.as_identifier().name().to_string(),
        identifier: node.identifier,
        effective_visibility: node.effective_visibility,
        binaries: node
            .binaries
            .map(|binaries| binaries.iter().map(ToString::to_string).collect()),
        entrypoints: node.entrypoints.iter().map(ToString::to_string).collect(),
        integrations: node.integrations,
        dependencies: node.dependencies.into_iter().map(closure_edge_out).collect(),
    }
}

fn closure_edge_out(edge: ClosureEdge) -> ClosureEdgeOut {
    ClosureEdgeOut {
        identifier: edge.identifier,
        visibility: edge.visibility,
        name: edge.name.to_string(),
    }
}

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
                digests: conflict.digests,
            })
            .collect(),
    }
}

/// One platform child of an image index, or one locked platform leaf.
#[derive(Serialize, schemars::JsonSchema)]
struct CandidateOut {
    /// The candidate manifest's digest.
    digest: Digest,
    /// This candidate as a pullable reference — the entry's identifier with
    /// this child's digest attached. Emitted for the same reason the entry
    /// carries `pinned_identifier`: splicing one by hand means knowing where
    /// the tag goes relative to the digest.
    // `pinned`, not `pinned_identifier`: a candidate has one digest, nothing to disambiguate.
    pinned: PinnedPackageRef,
    /// The platform the candidate serves.
    platform: Platform,
    /// Absent for a lock-projected candidate: `ocx.lock` records the leaf
    /// digest per platform, not the descriptor that pointed at it.
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
    /// The candidate manifest's size; absent for a lock-projected candidate.
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<ByteSize>,
}

/// The OCI resolution chain for the selected platform. Carries only the walk
/// (`index` → `manifest` → `config`); the platform-selected manifest's layers
/// are rendered alongside the metadata, not inside the chain.
#[derive(Serialize, schemars::JsonSchema)]
struct Resolution {
    /// The resolved artifact, digest-pinned.
    pinned: PinnedPackageRef,
    /// The blobs walked, in order: index when there is one, manifest, config.
    chain: Vec<ChainOut>,
}

/// One blob in the resolution chain. Same descriptor surface as a layer
/// (digest, media type, size) plus the OCI `role` so a consumer can tell
/// the index from the manifest from the config without decoding digests.
#[derive(Serialize, schemars::JsonSchema)]
struct ChainOut {
    /// The blob's digest.
    digest: Digest,
    /// What the blob is in the walk: `index`, `manifest` or `config`.
    role: String,
    /// The blob's media type.
    media_type: String,
    /// The blob's size; absent when it is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<ByteSize>,
}

/// A single layer descriptor from the inspected manifest (default mode) or the
/// platform-selected manifest (`--resolve`).
#[derive(Serialize, schemars::JsonSchema)]
struct Layer {
    /// The layer's digest.
    digest: Digest,
    /// The layer's media type.
    media_type: String,
    /// The layer's size; absent when the descriptor records a negative one.
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<ByteSize>,
}

impl Layer {
    /// Projects raw OCI layer descriptors onto the report surface.
    fn from_descriptors(descriptors: &[ocx_oci::Descriptor]) -> Result<Vec<Self>, DigestError> {
        descriptors
            .iter()
            .map(|descriptor| {
                Ok(Layer {
                    digest: Digest::try_from(descriptor.digest.as_str())?,
                    media_type: descriptor.media_type.clone(),
                    size: byte_size(descriptor.size),
                })
            })
            .collect()
    }
}

/// A wire size from the manager, which reports an unknown one as negative.
fn byte_size(size: i64) -> Option<ByteSize> {
    u64::try_from(size).ok().map(ByteSize::from)
}

/// The plain rendering of a size.
fn human_size(size: ByteSize) -> String {
    human_bytes(i64::try_from(size.get()).unwrap_or(i64::MAX))
}

impl PackageInspect {
    /// Builds one report entry; `name` is how the caller addressed the package, `identifier` the
    /// expanded request, and `platform` matters only under `--resolve`.
    ///
    /// # Errors
    ///
    /// A layer descriptor whose digest is not an OCI digest.
    pub fn new(
        name: String,
        identifier: ocx_oci::PackageRef,
        platform: ocx_oci::Platform,
        result: InspectResult,
    ) -> Result<Self, DigestError> {
        Ok(match result {
            InspectResult::Candidates { pinned, candidates } => Self {
                name,
                identifier,
                body: Body::Candidates {
                    candidates: candidates
                        .into_iter()
                        .map(|c| CandidateOut {
                            digest: c.identifier.digest(),
                            pinned: c.identifier,
                            platform: c.platform,
                            media_type: Some(c.media_type),
                            size: byte_size(c.size),
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
                    layers: Layer::from_descriptors(&layers)?,
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
                    layers: Layer::from_descriptors(&chain.final_manifest.layers)?,
                    resolution: Resolution::from_chain(&chain),
                    closure: closure.map(project_closure),
                },
            },
        })
    }

    /// Builds one entry straight from a locked toolchain binding (`ocx inspect` default mode): a pure
    /// projection with no registry read and no pinned artifact.
    ///
    /// # Errors
    ///
    /// A platform key that is not a canonical platform string.
    pub fn locked(
        name: String,
        identifier: ocx_oci::PackageRef,
        platforms: &std::collections::BTreeMap<String, Digest>,
    ) -> Result<Self, PlatformError> {
        let candidates = platforms
            .iter()
            .map(|(platform, digest)| {
                Ok(CandidateOut {
                    digest: digest.clone(),
                    pinned: PinnedPackageRef::pin(&identifier, digest.clone()),
                    platform: platform.parse()?,
                    media_type: None,
                    size: None,
                })
            })
            .collect::<Result<_, PlatformError>>()?;
        Ok(Self {
            name,
            identifier,
            body: Body::Locked { candidates },
        })
    }
}

impl Resolution {
    fn from_chain(chain: &ResolvedChain) -> Self {
        Self {
            pinned: chain.pinned.clone(),
            chain: chain
                .chain
                .iter()
                .map(|blob| ChainOut {
                    digest: blob.identifier.digest(),
                    role: blob.role.to_string(),
                    media_type: blob.media_type.clone(),
                    size: byte_size(blob.size),
                })
                .collect(),
        }
    }
}

impl PackageInspect {
    /// The single artifact this entry pinned, digest attached so a consumer never splices one;
    /// `None` for a lock projection, which selects nothing.
    fn pinned_identifier(&self) -> Option<&ocx_oci::PinnedPackageRef> {
        match &self.body {
            Body::Candidates { pinned, .. } | Body::Manifest { pinned, .. } | Body::Resolved { pinned, .. } => {
                Some(pinned)
            }
            Body::Locked { .. } => None,
        }
    }

    /// The identifier the plain tree roots at: the pinned one, else the declared one. A lock
    /// projection roots at the declared identifier, or the tree drops the `:tag` the lock does not store.
    fn root_identifier(&self) -> &ocx_oci::PackageRef {
        self.pinned_identifier()
            .map_or(&self.identifier, ocx_oci::PinnedPackageRef::as_identifier)
    }

    /// The whole plain-format tree for this entry.
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
        // Keep `len` in step with the fields written below, per body shape.
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
            s.serialize_field("pinned_identifier", pinned)?;
            s.serialize_field("pinned_digest", &pinned.digest())?;
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

/// A plain-text tree node; the JSON path uses the `Serialize` impls.
struct Node {
    label: String,
    /// When set, the label is this identifier inked at render time; takes precedence over `label`.
    identifier: Option<ocx_oci::PackageRef>,
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

    /// A branch whose label is an identifier inked at render time.
    fn identifier_branch(identifier: ocx_oci::PackageRef, children: Vec<Node>) -> Self {
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
            Some(identifier) => crate::api::data::ink_identifier(theme, identifier),
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
                // A tree render is not a gate: an unknown modifier still gets a row, since `ValidMetadata`
                // already refused bad input on every ingress path.
                let note = match ModifierKind::try_from(&var.modifier) {
                    Ok(kind) => kind.to_string(),
                    Err(_) => "unknown type".to_string(),
                };
                let mut node = Node::leaf(var.key.clone())
                    .with_note(note)
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
                // Only when it diverges from the invocable name.
                match entry.command() {
                    Some(cmd) if cmd.as_str() != name.as_str() => node.with_note(format!("-> {cmd}")),
                    _ => node,
                }
            })
            .collect();
        children.push(Node::branch("entrypoints", names));
    }

    // `None` = undeclared (no node); `Some(empty)` = declared zero executables, rendered explicitly.
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
    // No media type: every image-index child is a manifest, and the constant pushes size off a
    // narrow terminal. A lock projection has no size.
    let entries = candidates
        .iter()
        .map(|c| {
            let node = Node::leaf(c.platform.to_string()).with_digest(c.digest.to_string());
            match c.size {
                Some(size) => node.with_note(human_size(size)),
                None => node,
            }
        })
        .collect();
    Node::branch("candidates", entries)
}

/// The discriminating tail of a media type (`…image.layer.v1.tar+gzip` → `tar+gzip`).
fn media_type_suffix(media_type: &str) -> &str {
    media_type.rsplit('.').next().unwrap_or(media_type)
}

/// Renders the manifest's layers as a `layers` branch, one indexed leaf per layer.
fn layers_node(layers: &[Layer]) -> Node {
    let entries = layers
        .iter()
        .enumerate()
        .map(|(i, layer)| {
            let node = Node::leaf(format!("[{i}]"))
                .with_digest(layer.digest.to_string())
                .with_note(media_type_suffix(&layer.media_type).to_string());
            match layer.size {
                Some(size) => node.with_note(human_size(size)),
                None => node,
            }
        })
        .collect();
    Node::branch("layers", entries)
}

/// Renders the `resolution` branch: the selected platform, then the OCI walk. The platform shows
/// here because a libc refinement is visible nowhere else in the tree.
fn resolution_node(resolution: &Resolution, platform: &ocx_oci::Platform) -> Node {
    // No `pinned` leaf: it is the tree root, byte for byte.
    let chain = resolution
        .chain
        .iter()
        .map(|c| {
            let node = Node::leaf(c.role.clone()).with_digest(c.digest.to_string());
            match c.size {
                Some(size) => node.with_note(human_size(size)),
                None => node,
            }
        })
        .collect();
    Node::branch(
        "resolution",
        vec![Node::leaf(format!("platform {platform}")), Node::branch("chain", chain)],
    )
}

/// Renders the `closure` as a `(*)`-deduped tree rooted at the inspected package. Dedup keys on
/// content digest, not the tag-bearing identifier, so a diamond's shared node renders once.
fn closure_node(closure: &ClosureOut) -> Node {
    let mut children = Vec::new();

    // Flat list in transitive-closure order; the DAG edges live in the JSON `deps[].dependencies`.
    if !closure.deps.is_empty() {
        let deps = closure.deps.iter().map(closure_dep_leaf).collect();
        children.push(Node::branch("deps", deps));
    }

    children.push(surfaces_node(&closure.surface));

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
        let digests = conflict
            .digests
            .iter()
            .map(|digest| Node::leaf(digest.to_short_string()))
            .collect();
        children.push(Node::branch(
            format!("repository '{}' resolves to multiple digests", conflict.repository),
            digests,
        ));
    }

    Node::branch("closure", children)
}

/// One dependency as a flat leaf: short name, digest-inked identifier and composed visibility.
fn closure_dep_leaf(dep: &ClosureDepOut) -> Node {
    let mut leaf = Node::leaf(dep.name.clone()).with_digest(dep.identifier.to_string());
    if let Some(visibility) = dep.effective_visibility {
        leaf = leaf.with_visibility(visibility);
    }
    leaf
}

/// A wire identifier without its digest, or verbatim when it does not parse.
fn without_digest(identifier: &str) -> String {
    ocx_oci::PackageRef::parse(identifier)
        .map_or_else(|_| identifier.to_string(), |parsed| parsed.without_digest().to_string())
}

/// The repository's final path segment, as [`ClosureDepOut::name`] carries it, so `deps` reads as
/// the legend for every attribution; verbatim when it does not parse.
fn attribution_name(identifier: &str) -> String {
    ocx_oci::PackageRef::parse(identifier).map_or_else(|_| identifier.to_string(), |parsed| parsed.name().to_string())
}

/// Renders the `interface` and `private` surfaces under one `surface` branch.
fn surfaces_node(surfaces: &SurfacesOut) -> Node {
    Node::branch(
        "surface",
        vec![
            surface_node("interface", &surfaces.interface),
            surface_node("private", &surfaces.private),
        ],
    )
}

/// Renders one [`SurfaceOut`], with a note when `binaries_complete == false`.
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
    if !surface.integrations.is_empty() {
        let leaves = surface.integrations.iter().map(namespace_attribution_leaf).collect();
        children.push(Node::branch("integrations", leaves));
    }
    if !surface.binaries_complete {
        // The trigger is an undeclared claim, never `binaries: []`, which asserts zero and keeps the
        // aggregate complete (`adr_declared_binaries_metadata.md` §1).
        children.push(Node::leaf(
            "binaries incomplete: at least one admitted package leaves binaries undeclared",
        ));
    }
    Node::branch(label.to_string(), children)
}

/// One [`BinaryAttribution`] leaf, attributed by short name so a pinned identifier is not repeated per binary.
fn binary_attribution_leaf(attribution: &BinaryAttribution) -> Node {
    attribution_leaf(&attribution.name, attribution.package.as_deref())
}

/// One [`NamespaceAttribution`] leaf; never the payload, which the closure envelope does not carry.
fn namespace_attribution_leaf(attribution: &NamespaceAttribution) -> Node {
    attribution_leaf(&attribution.namespace, attribution.package.as_deref())
}

/// A claimed name, noted with the owning package's short name when known.
fn attribution_leaf(name: &str, package: Option<&str>) -> Node {
    let leaf = Node::leaf(name);
    match package {
        Some(package) => leaf.with_note(attribution_name(package)),
        None => leaf,
    }
}

/// One [`EnvVarAttribution`] leaf: env key, modifier kind and owning package.
fn env_var_attribution_leaf(attribution: &EnvVarAttribution) -> Node {
    let leaf = Node::leaf(attribution.key.clone()).with_note(attribution.kind.to_string());
    match &attribution.package {
        Some(package) => leaf.with_note(attribution_name(package)),
        None => leaf,
    }
}

/// The report both inspect commands emit: `ocx package inspect` over identifiers, `ocx inspect` over `ocx.toml`.
pub struct InspectReport {
    /// Present only when the run selected a platform, so `-p` stays inert in default mode.
    platform: Option<Platform>,
    /// An array, not keyed by request: entry order is meaningful and JSON key order is not.
    packages: Vec<PackageInspect>,
    /// The composed project-tier environment in application order, empty when nothing applies.
    /// Package-declared env sits per entry in `closure.surface.env`: its templates are concrete only
    /// after install.
    env: Vec<EnvEntry>,
}

impl InspectReport {
    /// `platform` is `Some` only when the run selected one, or the report names a platform nothing resolved against.
    pub fn new(platform: Option<&ocx_oci::Platform>, packages: Vec<PackageInspect>, env: Vec<EnvEntry>) -> Self {
        Self {
            platform: platform.cloned(),
            packages,
            env,
        }
    }

    /// Whether the closure walk found any interface-projection conflict; true drives a non-zero exit,
    /// since install would reject the set.
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
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "InspectReport";

    fn print_plain(&self, data: &DataInterface) {
        for inspect in &self.packages {
            data.print_tree(&inspect.tree());
        }
    }
}

/// A `$ref` or inline schema with a description beside it.
fn described(mut schema: schemars::Schema, description: &str) -> schemars::Schema {
    schema.insert("description".to_owned(), description.into());
    schema
}

// Hand-written: both `Serialize` impls build their field list at run time.
impl schemars::JsonSchema for InspectReport {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "InspectReport".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "description": "The report `ocx inspect` and `ocx package inspect` emit.",
            "properties": {
                "platform": described(
                    generator.subschema_for::<Platform>(),
                    "The platform the run selected; absent when it selected none.",
                ),
                "packages": described(
                    generator.subschema_for::<Vec<PackageInspect>>(),
                    "One entry per inspected package, in request order.",
                ),
                "env": described(
                    generator.subschema_for::<Vec<EnvEntry>>(),
                    "The composed project-tier environment in application order; empty when nothing applies.",
                ),
            },
            "required": ["packages", "env"],
        })
    }
}

impl schemars::JsonSchema for PackageInspect {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PackageInspect".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "description": "One inspected package. `pinned_identifier` and `pinned_digest` appear together, \
        only when the entry pinned one artifact. The other keys come from one shape: `candidates` (an image \
        index or an `ocx.lock` binding), `metadata` + `layers` (a manifest), or `platform` + `metadata` + \
        `layers` + `resolution` (a resolved package); `closure` joins the last two under `--closure`.",
            "properties": {
                "name": {"type": "string", "description": "The package as the caller addressed it."},
                "identifier": described(
                    generator.subschema_for::<ocx_oci::PackageRef>(),
                    "The expanded request.",
                ),
                "pinned_identifier": described(
                    generator.subschema_for::<PinnedPackageRef>(),
                    "The one artifact this entry pinned.",
                ),
                "pinned_digest": described(
                    generator.subschema_for::<Digest>(),
                    "The digest of `pinned_identifier`.",
                ),
                "candidates": described(
                    generator.subschema_for::<Vec<CandidateOut>>(),
                    "The platform children of an image index, or the platforms an `ocx.lock` binding pins.",
                ),
                "platform": described(
                    generator.subschema_for::<Platform>(),
                    "The platform `--resolve` selected.",
                ),
                "metadata": described(generator.subschema_for::<Metadata>(), "The package metadata."),
                "layers": described(
                    generator.subschema_for::<Vec<Layer>>(),
                    "The selected manifest's layers, in order.",
                ),
                "resolution": described(
                    generator.subschema_for::<Resolution>(),
                    "The resolution walk `--resolve` took.",
                ),
                "closure": described(
                    generator.subschema_for::<ClosureOut>(),
                    "The dependency closure `--closure` computed.",
                ),
            },
            "required": ["name", "identifier"],
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ocx_package::metadata::{
        BinaryName, Entrypoints, ValidMetadata,
        bundle::{Bundle, Version},
        dependency::Dependencies,
        env::Env,
    };
    use ocx_package_manager::{ClosureConflicts, ClosureNode, InspectClosure};

    use super::*;

    fn bundle_metadata(binaries: Option<Binaries>) -> Metadata {
        Metadata::Bundle(Bundle {
            binaries,
            version: Version::V1,
            strip_components: None,
            env: Env::default(),
            dependencies: Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
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
    // `ocx_package_manager::tasks::inspect`); these tests pin the WIRE shape and
    // the plain render.

    fn test_identifier() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry("toolchain", "example.com").clone_with_tag("1.0")
    }

    fn test_platform() -> ocx_oci::Platform {
        ocx_oci::Platform::any()
    }

    fn pinned(repo: &str, hex_char: char) -> ocx_oci::PinnedPackageRef {
        let id = ocx_oci::PackageRef::new_registry(repo, "example.com")
            .clone_with_digest(ocx_oci::Digest::Sha256(hex_char.to_string().repeat(64)));
        ocx_oci::PinnedPackageRef::try_from(id).expect("digest-bearing identifier is always pinnable")
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
            separator: None,
            visibility,
        }
    }

    /// A `list`-kind fixture carrying an explicit separator — the sibling of
    /// [`env_var`] for the one kind that has one.
    fn env_list_var(key: &str, separator: &str, visibility: Visibility) -> ClosureEnvVar {
        ClosureEnvVar {
            key: key.to_string(),
            kind: ModifierKind::List,
            separator: Some(separator.to_string()),
            visibility,
        }
    }

    /// Builds a minimal `Manifest`-mode `InspectResult` carrying `closure`
    /// (or `None`, the no-`--closure` case).
    fn manifest_result(root: ocx_oci::PinnedPackageRef, closure: Option<InspectClosure>) -> InspectResult {
        InspectResult::Manifest {
            pinned: root,
            metadata: ValidMetadata::try_from(bundle_metadata(None)).expect("bare bundle metadata is always valid"),
            layers: vec![],
            closure,
        }
    }

    /// A non-root closure node with the given composed-from-root visibility.
    fn dep_node(identifier: ocx_oci::PinnedPackageRef, effective_visibility: Visibility) -> ClosureNode {
        ClosureNode {
            identifier,
            config_digest: ocx_oci::Digest::Sha256("e".repeat(64)),
            effective_visibility: Some(effective_visibility),
            binaries: None,
            entrypoints: vec![],
            env: vec![],
            integrations: vec![],
            dependencies: vec![],
            is_root: false,
        }
    }

    /// The root closure node — no composed-from-root visibility.
    fn root_node(identifier: ocx_oci::PinnedPackageRef) -> ClosureNode {
        ClosureNode {
            identifier,
            config_digest: ocx_oci::Digest::Sha256("e".repeat(64)),
            effective_visibility: None,
            binaries: None,
            entrypoints: vec![],
            env: vec![],
            integrations: vec![],
            dependencies: vec![],
            is_root: true,
        }
    }

    /// A lib-level [`Surface`] with only env entries (binaries/entrypoints
    /// empty), the common shape the wire tests need.
    fn surface_with_env(env: Vec<(ocx_oci::PinnedPackageRef, ClosureEnvVar)>, binaries_complete: bool) -> Surface {
        Surface {
            binaries: vec![],
            entrypoints: vec![],
            env,
            integrations: vec![],
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
            integrations: vec![],
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
            ("linux/amd64".to_string(), ocx_oci::Digest::Sha256("a".repeat(64))),
            ("darwin/arm64".to_string(), ocx_oci::Digest::Sha256("b".repeat(64))),
        ]);
        let report =
            PackageInspect::locked("toolchain".into(), test_identifier(), &platforms).expect("canonical platform keys");
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");

        assert_eq!(value["name"], "toolchain", "the entry names itself by binding");
        assert_eq!(value["identifier"], "example.com/toolchain:1.0");
        let candidates = value["candidates"].as_array().expect("candidates is an array");
        assert_eq!(
            candidates.iter().map(|c| c["platform"].clone()).collect::<Vec<_>>(),
            [
                serde_json::json!({"os": "darwin", "architecture": "arm64"}),
                serde_json::json!({"os": "linux", "architecture": "amd64"}),
            ],
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
            std::collections::BTreeMap::from([("linux/amd64".to_string(), ocx_oci::Digest::Sha256("a".repeat(64)))]);
        let report =
            PackageInspect::locked("toolchain".into(), test_identifier(), &platforms).expect("canonical platform keys");
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
            std::collections::BTreeMap::from([("linux/amd64".to_string(), ocx_oci::Digest::Sha256("a".repeat(64)))]);
        let report =
            PackageInspect::locked("toolchain".into(), test_identifier(), &platforms).expect("canonical platform keys");

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
        )
        .expect("well-formed layer digests");
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
        )
        .expect("well-formed layer digests");
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
        )
        .expect("well-formed layer digests");
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
        )
        .expect("well-formed layer digests");
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

    /// C-016: a `deps` entry's `integrations` is a plain array — always
    /// present, `[]` when the dep declares none (unlike `binaries`' tri-state,
    /// there is no `skip_serializing_if` here). Pins that contract so adding
    /// one later reds this test instead of silently dropping the key on a
    /// consumer that always reads it.
    #[test]
    fn json_closure_dep_integrations_present_as_empty_array_when_none_declared() {
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
        )
        .expect("well-formed layer digests");
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        let dep_entry = &value["closure"]["deps"][0];
        assert_eq!(
            dep_entry["integrations"],
            serde_json::json!([]),
            "a dep declaring no integrations must still carry the key as an empty array, \
             never omitted: {dep_entry}"
        );
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
        )
        .expect("well-formed layer digests");
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

    /// A layer descriptor whose digest does not parse fails the projection rather than reporting a bogus layer.
    #[test]
    fn layer_from_descriptors_rejects_unparseable_digest() {
        let descriptor = ocx_oci::Descriptor {
            digest: "nope".to_string(),
            ..Default::default()
        };
        assert!(Layer::from_descriptors(&[descriptor]).is_err());
    }

    /// A surface `env` array projects each exposed env key with its modifier
    /// kind under `kind` and the declaring package under `package`.
    #[test]
    fn json_closure_surface_env_carries_key_kind_and_package() {
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
        )
        .expect("well-formed layer digests");
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        let env = value["closure"]["surface"]["interface"]["env"]
            .as_array()
            .expect("closure.surface.interface.env is an array");

        let path = env.iter().find(|e| e["key"] == "PATH").expect("PATH env entry present");
        assert_eq!(path["kind"], "path", "modifier kind serializes under `kind`");
        assert!(
            path["package"].as_str().unwrap_or_default().contains("root"),
            "env entry carries its declaring package: {path}"
        );
        let dep_home = env
            .iter()
            .find(|e| e["key"] == "DEP_HOME")
            .expect("DEP_HOME env entry present");
        assert_eq!(dep_home["kind"], "constant");
        assert!(dep_home["package"].as_str().unwrap_or_default().contains("dep"));
    }

    /// A `list`-kind surface entry additionally carries its declared
    /// `separator`; a `path` entry sharing the same array has no such key.
    #[test]
    fn json_closure_surface_env_list_entry_carries_separator() {
        let root = pinned("root", 'a');
        let interface = surface_with_env(
            vec![
                (root.clone(), env_list_var("GODEBUG", ",", Visibility::PUBLIC)),
                (root.clone(), env_var("PATH", ModifierKind::Path, Visibility::PUBLIC)),
            ],
            true,
        );
        let closure = closure_of(vec![root_node(root.clone())], interface, empty_surface(true));
        let report = PackageInspect::new(
            "test".into(),
            test_identifier(),
            test_platform(),
            manifest_result(root, Some(closure)),
        )
        .expect("well-formed layer digests");
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        let env = value["closure"]["surface"]["interface"]["env"]
            .as_array()
            .expect("closure.surface.interface.env is an array");

        let godebug = env
            .iter()
            .find(|e| e["key"] == "GODEBUG")
            .expect("GODEBUG env entry present");
        assert_eq!(godebug["kind"], "list");
        assert_eq!(godebug["separator"], ",");

        let path = env.iter().find(|e| e["key"] == "PATH").expect("PATH env entry present");
        assert!(
            path.get("separator").is_none(),
            "a path entry must carry no separator key: {path}"
        );
    }

    /// D16: the closure surface spells the attributed key `namespace` — the
    /// same word the flat `ocx env` / `ocx package env` envelope uses — and
    /// carries no payload.
    ///
    /// The literal key is asserted, not just the pair's presence: a consumer
    /// filters with `select(.namespace=="...")`, and `jq` answers `null` for a
    /// missing key instead of erroring, so a regression to `name` would be
    /// silent on the consumer side.
    #[test]
    fn json_closure_surface_integrations_key_is_namespace() {
        let root = pinned("root", 'a');
        let interface = Surface {
            integrations: vec![(root.clone(), "com.example.tool".to_string())],
            ..empty_surface(true)
        };
        let closure = closure_of(vec![root_node(root.clone())], interface, empty_surface(true));
        let report = PackageInspect::new(
            "test".into(),
            test_identifier(),
            test_platform(),
            manifest_result(root, Some(closure)),
        )
        .expect("well-formed layer digests");
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        let integrations = value["closure"]["surface"]["interface"]["integrations"]
            .as_array()
            .expect("closure.surface.interface.integrations is an array");
        let entry = integrations
            .first()
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| panic!("one admitted namespace projects one row: {integrations:?}"));

        assert_eq!(
            entry.get("namespace").and_then(serde_json::Value::as_str),
            Some("com.example.tool"),
            "the namespace key is spelled `namespace`, matching the flat env envelope: {entry:?}"
        );
        assert!(
            !entry.contains_key("name"),
            "`name` is the binaries/entrypoints spelling and must not appear here: {entry:?}"
        );
        assert!(
            !entry.contains_key("value"),
            "the closure envelope is payload-free: {entry:?}"
        );
        assert!(
            entry
                .get("package")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .contains("root"),
            "the row attributes to the declaring package: {entry:?}"
        );
    }

    /// The array is always present, `[]` when nothing declares a namespace —
    /// absent and empty are not two states here (unlike `deps[].binaries`).
    #[test]
    fn json_closure_surface_integrations_present_as_empty_array() {
        let root = pinned("root", 'a');
        let closure = closure_of(vec![root_node(root.clone())], empty_surface(true), empty_surface(true));
        let report = PackageInspect::new(
            "test".into(),
            test_identifier(),
            test_platform(),
            manifest_result(root, Some(closure)),
        )
        .expect("well-formed layer digests");
        let value = serde_json::to_value(&report).expect("PackageInspect always serializes");
        for axis in ["interface", "private"] {
            assert_eq!(
                value["closure"]["surface"][axis]["integrations"],
                serde_json::json!([]),
                "surface.{axis}.integrations must be an empty array, never omitted"
            );
        }
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
    fn dep_out(name: &str, hex: char, visibility: Visibility) -> ClosureDepOut {
        let identifier = ocx_oci::PackageRef::parse(&format!("example.com/{name}:1.0")).expect("valid identifier");
        let digest = ocx_oci::Digest::try_from(fake_digest(hex).as_str()).expect("valid digest");
        ClosureDepOut {
            name: name.to_string(),
            identifier: PinnedPackageRef::pin(&identifier, digest),
            effective_visibility: Some(visibility),
            binaries: None,
            entrypoints: vec![],
            integrations: vec![],
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
                dep_out("deps-mid", 'c', Visibility::INTERFACE),
                dep_out("deps-leaf", 'd', Visibility::PUBLIC),
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
            text.iter().filter(|t| t.contains(&fake_digest('c'))).count(),
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
                    digests: vec![
                        ocx_oci::Digest::try_from(fake_digest('e').as_str()).expect("valid digest"),
                        ocx_oci::Digest::try_from(fake_digest('f').as_str()).expect("valid digest"),
                    ],
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

    /// A labelled `surface` branch renders declared
    /// binaries/entrypoints/env/integrations as leaves and, when
    /// `binaries_complete == false`, an incompleteness note.
    #[test]
    fn surface_node_renders_binaries_entrypoints_env_and_incomplete_note() {
        let surface = SurfaceOut {
            binaries: BinaryAttribution::from_pairs(&[(pinned("cmake", 'a'), "cmake".to_string())]),
            entrypoints: BinaryAttribution::from_pairs(&[(pinned("toolchain", 'b'), "cc".to_string())]),
            env: EnvVarAttribution::from_pairs(&[(
                pinned("cmake", 'a'),
                env_var("CMAKE_ROOT", ModifierKind::Constant, Visibility::PUBLIC),
            )]),
            integrations: NamespaceAttribution::from_pairs(&[(
                pinned("cmake", 'a'),
                "com.microsoft.vscode".to_string(),
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
            joined.contains("com.microsoft.vscode"),
            "a declared integration namespace renders as a leaf: {joined}"
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
            integrations: vec![],
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
