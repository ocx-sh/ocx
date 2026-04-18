# ADR: Package Dependencies

> **Current implementation state** — for the live module map, facade pattern, error model, and task surface, see [`.claude/rules/subsystem-package-manager.md`](../rules/subsystem-package-manager.md) and [`.claude/rules/subsystem-package.md`](../rules/subsystem-package.md). This ADR is the design rationale record; read it for *why*, not *what is true today*.

## Metadata

**Status:** Amended
**Date:** 2026-04-05
**Deciders:** mherwig, architect
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** data | api
**Supersedes:** N/A
**Superseded By:** N/A

## Context

OCX currently has no concept of package dependencies. Each package is a self-contained unit — install it, get its files, set up its environment. But real-world tool chains have dependencies: a Java application needs a JRE, a Python package needs a Python interpreter, a CMake project needs both CMake and a compiler.

Today, users must manually install all required tools and compose their environments via `ocx exec pkg1 pkg2 -- ...` or multi-package `ocx env`. This works but has three problems:

1. **No declared contract.** A package publisher knows their tool requires Java 21, but there is no way to express that in the package metadata. Consumers discover missing dependencies at runtime.
2. **No automatic installation.** When `ocx install myapp:1.0` resolves, it does not know that `java:21` must also be present.
3. **No GC safety.** If a user installs `java:21` only because `myapp:1.0` needs it, there is no reference preventing `java:21` from being garbage collected after the user removes the direct install symlinks.

### Reproducibility constraint

OCX's core value proposition is reproducibility. The publisher of a package has a specific view of the registry at publish time — they tested with a particular Java build, identified by digest. The consumer may have a different local index. Therefore, **dependencies must be pinned by digest**, not by mutable tag. This eliminates version resolution entirely: there is no "find a compatible version" step. The publisher declares exactly which dependency they tested against.

### Platform resolution

An OCI digest can reference either:
- An **Image Index** (multi-platform manifest list) — the consumer's platform determines which actual manifest is pulled.
- A **single manifest** — bypasses platform selection entirely.

Both must be supported. The common case is referencing an Image Index (the consumer's platform is auto-detected). For cross-compilation scenarios where a dependency must resolve to a foreign architecture (e.g., an arm64 sysroot on an amd64 host), the publisher pins the platform-specific manifest digest directly — this is explicit and requires no additional metadata field.

### Scope — what this ADR covers and what it does not

This ADR covers **dependency declaration, pre-fetching, GC, and environment composition**. Four related but distinct concerns are explicitly out of scope:

| Concern | Status | Relationship to dependencies |
|---------|--------|------------------------------|
| **Index locking** | Already solved | The local index (`$OCX_HOME/index/`) and `OCX_INDEX` override handle offline/reproducible resolution. Orthogonal to dependencies. |
| **Shims** | Future ADR | Auto-generated launcher scripts that invoke dependencies through `ocx exec` for per-invocation environment isolation. Uses dependencies as input but is a separate mechanism. |
| **OCX version pinning** | Future ADR (with shims) | Pinning the OCX binary version for launcher scripts. Related to shims — the shim generator would use the current (or pinned) OCX version. |
| **Project config / lockfile** | Future ADR | A project-level config file (analogous to `pyproject.toml` / `Cargo.toml` + lockfile) where dependencies are declared by tag, resolved to digests via a `lock` command, and platform overrides live. Update tooling (`update-deps`) belongs at this layer, not in package metadata. |

Dependencies are the foundational layer. Shims, project config, and OCX version pinning build on top of it but have their own design concerns.

## Decision Drivers

- **Reproducibility**: Same metadata = same dependency graph on any machine, always.
- **Simplicity**: Dependencies are a first pass. The design must be simple enough to implement incrementally and reason about easily.
- **GC correctness**: Objects referenced by installed packages must not be collected.
- **Deterministic environment composition**: The order in which dependency environments are applied must be well-defined and produce identical results across runs.
- **Compatibility with existing concepts**: Must work with the three-store architecture (objects, index, installs) and the reference manager pattern.

## Background Research

### How other package managers handle this

**Nix** — Content-addressed store with automatic runtime dependency detection. After building a package, Nix scans the output bytes for occurrences of other store path hashes. If a hash appears in the binary, it's a runtime dependency. GC walks the reference graph from roots (profile symlinks) and removes unreachable paths. The graph is precomputed and stored in SQLite. Key insight: Nix never resolves version ranges — each derivation pins its exact inputs by hash. ([nixos.org][nix-pills-09])

**Guix** — Fork of the Nix daemon with the same content-addressed store and reference scanning model. GC is identical: reachability from roots protects the transitive closure. Adds "grafts" for fast security patches without full rebuilds. ([guix.gnu.org][guix-features])

**Homebrew** — Dependencies declared by formula name (no version pinning). Resolution selects the latest available version. One global version per formula — conflicts resolved by `keg_only` or `conflicts_with`. GC via `brew autoremove` removes formulae only installed as dependencies. No content addressing. ([docs.brew.sh][homebrew-cookbook])

**Go Modules / MVS** — Minimum Version Selection: when multiple dependents require different minimum versions of the same module, the highest minimum wins. Deterministic, polynomial-time, no SAT-solving. The `go.sum` file records SHA-256 hashes of all modules for integrity verification. Key property: MVS never selects a version not listed by some `go.mod` — upgrades are always explicit. ([research.swtch.com][go-mvs])

**Bazel (Bzlmod)** — Also uses MVS for module resolution. The lockfile (`MODULE.bazel.lock`) records SHA-256 hashes of all registry files accessed, ensuring identical inputs on every machine. Dependency visibility is restricted: a module can only use repositories from its direct dependencies, not transitive ones. ([bazel.build][bazel-lockfile])

**OCI** — No native dependency concept between images. Layer sharing is implicit via content-addressed deduplication. The OCI 1.1 `subject` field links attestations (SBOMs, signatures) to manifests — a forward-reference for associated metadata, not a dependency mechanism. ([opencontainers.org][oci-manifest])

### Environment composition patterns

All surveyed tools (Nix, direnv, mise, asdf) use the same fundamental model: accumulator variables (PATH, PKG_CONFIG_PATH) are merged by prepending/appending; scalar variables (JAVA_HOME, CC) are set by last-writer-wins. No tool has a principled declarative mechanism for resolving scalar conflicts between independently-managed packages. OCX already has this distinction: `path` (prepend) vs. `constant` (set).

**No tool supports two conflicting versions of the same tool simultaneously in one environment.** Nix errors with a "collision" when two packages at the same priority provide the same binary. Bazel avoids the problem via hermetic per-target isolation. The only way to get per-invocation isolation is forking a subprocess with a modified environment — which is what `ocx exec` does. This is the domain of shims (future ADR), not dependencies.

### Topological sort for deterministic ordering

Kahn's algorithm with a stable sort key (package identifier as tiebreaker) produces the same ordering for the same input graph, every time. Dependencies-first ordering ensures a dependency's environment is applied before its dependent's. For parallel installation, packages at the same topological level (no ordering constraint between them) can be installed concurrently. ([Wikipedia][topo-sort])

## Design

### Dependency declaration in metadata.json

A new optional `dependencies` array in the `Bundle` metadata type. Each entry is a dependency descriptor containing the identifier and pinned digest:

```json
{
  "type": "bundle",
  "version": 1,
  "env": [...],
  "dependencies": [
    {
      "identifier": "ocx.sh/java:21@sha256:a1b2c3d4e5f6...",
      "visibility": "private"
    },
    {
      "identifier": "ocx.sh/cmake:3.28@sha256:f6e5d4c3b2a1..."
    }
  ]
}
```

**Why an array, not a JSON object (map)?** RFC 8259 explicitly states that JSON objects are unordered collections. While some parsers (Python `json`, serde with `IndexMap`) preserve insertion order, others do not (Go `encoding/json`, `jq`). Since dependency ordering has semantic meaning for environment composition, encoding it as array position makes the ordering explicit and survives any JSON parser. The `Dependencies` wrapper type enforces uniqueness internally.

**Identifier format:** The `identifier` field is a fully qualified OCX identifier — `registry/repository` with an optional `:tag`. **The registry is required.** Default registry resolution is not applied to dependency identifiers because the consumer may have a different default registry configured than the publisher. Every dependency must be unambiguous regardless of client configuration.

The tag portion is **advisory**: it records what the publisher pinned against and enables future update tooling (`ocx package update-deps` can check where the tag now points), but it is never used for resolution. Only the digest is authoritative.

| Field | Required | Description |
|-------|----------|-------------|
| `identifier` | Yes | Fully qualified pinned OCX identifier with registry and inline OCI digest (`@sha256:...`), optionally with `:tag`. e.g. `ocx.sh/java:21@sha256:a1b2c3d4e5f6...`, `ghcr.io/myorg/tool@sha256:...`. **Registry is mandatory** — no default registry fallback. The digest may reference an Image Index (platform resolution applies at install time) or a single manifest (no platform resolution needed). For cross-compilation, pin the platform-specific manifest digest directly. |
| `visibility` | No | Enum: `sealed` (default), `private`, `public`, `interface`. Controls how the dependency's environment variables propagate — see [Dependency visibility](#dependency-visibility) below. |

**Ordering matters.** Dependencies are processed in array order. This is the canonical import order for environment composition.

**No version ranges. No resolution.** The digest is the complete truth. There is nothing to resolve. The tag in the identifier is purely informational — the consumer's view of the index is irrelevant. The publisher pinned a specific digest, and that is what gets installed.

**Future update tooling.** Because the identifier carries the tag, a future `ocx package update-deps` command can look up where each tag currently points in the registry and show which dependencies have newer digests available. The publisher can then choose to update. This is opt-in, never automatic.

### Dependency graph and transitive dependencies

Dependencies can themselves have dependencies, forming a DAG. A cycle is a fatal error at install time.

**Graph flattening algorithm** (for environment composition and installation order):

1. Build the dependency graph from metadata. Each node is identified by `(identifier, resolved_digest)` where `resolved_digest` is the platform-specific manifest digest (after Image Index resolution).
2. Detect cycles. If found, abort with an error listing the cycle.
3. Topological sort using Kahn's algorithm with lexicographic tiebreaker on `(identifier, digest)`. This produces a deterministic total order.
4. Deduplicate: if the same `(identifier, resolved_digest)` appears multiple times (diamond dependency), keep only the first occurrence in topological order.
5. The result is a flat, ordered list of packages: dependencies before dependents, deterministic, deduplicated.

**Duplicate detection and deduplication policy:**

| Scenario | Behavior |
|----------|----------|
| Same resolved digest (regardless of identifier) | Deduplicated silently. The package appears once in the flattened list at its first topological position. Content is stored once in the object store. |
| Same repository, different digests | Both versions are installed as separate objects. Both contribute to the environment. **This is problematic in most cases** — see below. |

**Same repository, different digests — the environment conflict problem.** When two packages depend on the same tool (e.g., Java) at different digests, both are installed to the object store (they are different objects). However, their environments cannot coexist cleanly in one shell session: if both set `JAVA_HOME` to different paths, only one value survives (last-writer-wins per the `constant` modifier semantics).

The existing `ConstantTracker` detects when a scalar variable is overwritten by a different package and **emits a warning**. This is consistent with how `ocx exec pkg1 pkg2 -- cmd` handles conflicts today — dependencies do not introduce a new problem, they just make it possible for conflicts to occur transitively.

### Dependency visibility

Each dependency declaration carries a `visibility` field that controls how the dependency's environment variables propagate. This replaces the earlier `export: bool` flag with a richer model inspired by CMake's `target_link_libraries` visibility (PUBLIC/PRIVATE/INTERFACE) and Gradle's `api`/`implementation` split.

**The two axes.** Visibility answers two independent questions about a dependency's env vars:

1. **Self-use:** Does the package need this dep's env vars for its own execution (shims, entry points)?
2. **Consumer propagation:** Do consumers of this package see this dep's env vars?

**The four levels:**

| Visibility | Self-use | Consumer | Use case |
|---|---|---|---|
| `sealed` (default) | No | No | Structural dependency — content accessed by mount point, symlink, or direct path. Env vars irrelevant. Most deps in a tool-focused package manager. |
| `private` | Yes | No | Package's own shims/entry points need the dep's env to execute, but consumers don't see it. E.g., `my-tool` needs `java` internally for its shims. |
| `public` | Yes | Yes | Both the package and its consumers need the dep's env. E.g., `maven` needs `java` and consumers also need Java to compile. |
| `interface` | No | Yes | Dep's env forwarded to consumers but not used by the package itself. Meta-packages or stack packages that compose environments for others. |

**Default: `sealed`.** This is the most restrictive level and prevents environment pollution by default. The package author must explicitly opt in to env propagation. This matches the prior `export: false` default behavior.

**Propagation rule.** When a parent consumes a dependency, the parent is a *consumer* of that dependency. What the parent sees from the dep's tree is what the dep **exports to its consumers** (consumer-visible axis). The parent's declared visibility for the dep determines the *terms* of the relationship.

**The rule is: if the child exports (consumer-visible), the result equals the edge. Otherwise, the result is sealed.**

```
propagate(edge, child) = if child.is_consumer_visible() { edge } else { Sealed }
```

*Propagation table* (edge × child → effective visibility from parent):

| Parent → Dep ↓ \ Dep → Transitive → | `public` | `interface` | `private` | `sealed` |
|---|---|---|---|---|
| **`public`** | `public` | `public` | `sealed` | `sealed` |
| **`private`** | `private` | `private` | `sealed` | `sealed` |
| **`interface`** | `interface` | `interface` | `sealed` | `sealed` |
| **`sealed`** | `sealed` | `sealed` | `sealed` | `sealed` |

Pattern: child exports (`public`/`interface`) → result = edge. Child doesn't export (`private`/`sealed`) → `sealed`.

This works recursively in the bottom-up `ResolvedPackage::with_dependencies()` builder: each dep's transitive deps already carry their cumulative effective visibility from deeper levels. `propagate` only checks whether the immediate child exports.

**Diamond merge.** When two paths reach the same dep with different effective visibilities, take the **most open** — OR on each axis independently. If *any* path in the graph makes a dep visible on an axis, it stays visible.

```
merge(a, b) = from_axes(a.self || b.self, a.consumer || b.consumer)
```

*Diamond merge table* (path A ∨ path B → merged visibility):

| Path A ↓ \ Path B → | `sealed` | `private` | `public` | `interface` |
|---|---|---|---|---|
| **`sealed`** | `sealed` | `private` | `public` | `interface` |
| **`private`** | `private` | `private` | `public` | `public` |
| **`public`** | `public` | `public` | `public` | `public` |
| **`interface`** | `interface` | `public` | `public` | `interface` |

**Conflict detection scope:** Only consumer-visible dependencies participate in conflict detection. Two sealed or private deps with the same repository but different digests do NOT trigger a conflict — their env vars are never composed in the consumer context, so there is nothing to conflict.

**Current implementation scope:** All four visibility levels are fully implemented in the propagation algebra and persisted in `resolve.json`. The `resolve_env()` consumer path filters by `is_consumer_visible()` — `public` and `interface` contribute env vars, `private` and `sealed` do not. The self-visible axis (`public`, `private`) activates when self-execution environments (shims, entry points) are built — at that point, `private` deps will contribute to a package's own shim env while remaining invisible to consumers.

**Future direction — per-variable overrides.** The visibility enum sets the **default propagation policy** for all of a dependency's env vars. A future extension may allow per-variable overrides (e.g., `expose: [PATH]` on a `sealed` dep to get tools on PATH without the full env). This layers on top of visibility without changing the enum.

**Future direction — per-invocation isolation via shims (separate ADR).** Auto-generated launcher scripts that invoke dependencies through `ocx exec`, giving each tool its own clean environment. Shims implement the self-use axis: when a shim runs, it resolves env for its package as root, including `private` deps. The shim subprocess env never leaks to the caller. The `visibility` field controls the dependency relationship — "how does this dep's env flow?" — regardless of whether the package is consumed via shim or `ocx exec`. The `${deps.NAME.installPath}` interpolation syntax is the natural companion to `sealed` visibility — letting packages reference dependency install paths without requiring env propagation.

### Environment composition order

Given a user command like `ocx exec A B -- cmd`:

1. Flatten the dependency graph for package A (including transitive deps).
2. Flatten the dependency graph for package B (including transitive deps).
3. Concatenate: `[A_deps..., A, B_deps..., B]`.
4. Deduplicate the combined list: for each `(identifier, resolved_digest)`, keep only the first occurrence.
5. Apply environments in this order. Each package's `env` array is applied in its declared order.

This means:
- A dependency's environment is always applied before its dependent's.
- If both A and B depend on the same Java, Java's env is applied once (at the position of whichever appeared first).
- A dependent can override a dependency's scalar variable (e.g., A sets `JAVA_HOME` after Java's own env is applied).
- Between top-level packages, left-to-right ordering is preserved (A before B, matching existing behavior).

### Pull as the foundational operation

**`pull` is the single code path for bringing packages into the object store.** Every command that may cause a package to be fetched — whether explicitly or implicitly — must delegate to `pull`. This ensures a single implementation for downloading, extracting, creating dependency forward-refs in `deps/`, and maintaining consistent object store state. There must be no alternative code path that bypasses `pull`.

**Commands that delegate to `pull`:**

| Command | Trigger | What `pull` does | What the command adds on top |
|---------|---------|------------------|------------------------------|
| `ocx install` | Explicit | Downloads package + transitive deps, creates `deps/` forward-refs | Creates candidate symlink (+ current if `--select`) for top-level only |
| `ocx package pull` | Explicit | Downloads package + transitive deps, creates `deps/` forward-refs | Nothing — `pull` is the entire operation |
| `ocx exec` | Implicit (auto-install) | Downloads missing package + transitive deps, creates dep back-refs | Composes environment, runs command |
| `ocx env` | Implicit (auto-install) | Downloads missing package + transitive deps, creates dep back-refs | Composes and prints environment |

The implicit auto-install in `exec` and `env` already exists today via `find_or_install_all()`. With dependencies, the `install` step inside `find_or_install` delegates to `pull` to handle transitive deps, then optionally creates symlinks.

**Commands that do NOT trigger `pull`:** `find`, `select`, `deselect`, `uninstall`, `clean`, `shell env`, `ci export` — these operate only on locally-present packages and fail with `NotFound` if a package is missing.

### Symlink behavior

Only explicit `install` creates candidate/current symlinks. Dependencies pulled transitively live in the object store only — no install symlinks. This matches the principle of least surprise: the user asked to install `myapp:1.0`, not `java:21`. Dependencies are implementation details of the package.

**Explicit installation of a dependency** is still possible: `ocx install java:21 --select` creates its own symlinks independently. The two references (metadata dependency + explicit install symlink) coexist — the object is shared via content addressing.

### Garbage collection

#### The problem

Currently, GC checks if `refs/` is empty to determine if an object is unreferenced. Symlinks (candidate, current) create back-references in `refs/`. But dependency relationships are encoded in metadata, not symlinks — there is no `refs/` entry for "package A depends on me."

#### Considered approaches

**Option A: Metadata-only — iterative GC**

No filesystem changes for dependencies. GC reads metadata of all remaining objects to determine if any depend on a candidate-for-deletion. Requires iterative passes: deleting A may make A's dependency B unreferenced, which requires another pass.

| Pros | Cons |
|------|------|
| No new filesystem structures | O(n) metadata reads per GC pass |
| Metadata is source of truth | Multiple passes needed for cascading deletions |
| Simple to implement | Potentially slow on large stores |

**Option B: Dependency back-references in `refs/`**

When installing a package with dependencies, create a back-reference in each dependency's `refs/` directory pointing to the dependent's object directory (not a symlink — a reference to another object). GC becomes single-pass again: an object is unreferenced only if `refs/` is empty (no symlinks and no dependent objects reference it).

| Pros | Cons |
|------|------|
| Single-pass GC (existing algorithm works) | New reference type in `refs/` (object → object, not symlink → object) |
| Consistent with existing pattern | Must maintain refs when installing/uninstalling |
| Fast GC regardless of store size | Adds complexity to reference manager |

**Option C: Dependency symlinks in a `deps/` directory**

Create a `deps/` sibling to `content/` in each object directory. Symlinks in `deps/` point to dependency content directories, named by index (preserving order). Each dependency also gets a back-reference in its `refs/`.

| Pros | Cons |
|------|------|
| Filesystem makes dependencies visible | Redundant with metadata |
| `ls deps/` shows what a package needs | Ordering via filenames (fragile) |
| Could be used for fast resolution | Extra I/O on install |

#### Decision: `refs/` for install symlinks, `deps/` for dependency forward-references

Two single-purpose directories with no overlap:

- **`refs/`** stores **install symlink back-references only** (unchanged from pre-dependency behavior). Each entry points back to a candidate or current symlink in `installs/`. An object with a non-empty `refs/` (after discarding broken refs) is a **root** — something in `installs/` depends on it directly.
- **`deps/`** stores **dependency forward-references**. Symlinks from the dependent's `deps/` directory point to each dependency's content directory. This encodes the dependency graph on the filesystem, provides fast resolution for environment composition (avoiding metadata re-parsing), and handles the Image Index → platform digest indirection (the symlink points to the resolved platform-specific content, not the Image Index digest declared in metadata).

No dependency back-refs exist in `refs/`. Dependencies are tracked solely through `deps/` forward-refs. This keeps `refs/` purely about install symlinks and avoids maintaining two reference types in one directory.

**GC — reachability walk (not Kahn's):**

GC is a simple graph reachability analysis — the same approach `git gc` uses:

1. **Build graph:** Scan all objects. For each object, read `deps/` to discover edges (dependent → dependency).
2. **Find roots:** Two sources: (a) For each object, check `refs/`. Discard broken refs (symlinks to deleted installs). If any valid ref remains, the object is a root. (b) Resolve profile content-mode entries — objects referenced by the shell profile manifest are also roots (profiled packages must not be GC'd).
3. **Walk from roots:** BFS from all root nodes, following `deps/` edges. Every visited node is reachable (alive).
4. **Collect:** Every node NOT visited is unreachable — delete it.

Complexity: O(N + E) where N = objects, E = dependency edges. No ref-count bookkeeping needed.

**Note:** Kahn's algorithm is still used for **topological sorting** during environment resolution (dependency envs must be applied before dependent envs). The simplification here applies only to GC, which needs reachability, not ordering.

**Updated filesystem layout for an object with dependencies:**

```
objects/{registry}/{repo}/{algorithm}/{shard_a}/{shard_b}/{shard_c}/
  content/              # extracted package files
  metadata.json         # includes "dependencies" array
  resolve.json          # resolved PinnedIdentifier + transitive dep closure (written by pull)
  refs/
    a1b2c3d4e5f6a1b2   # → /path/to/installs/.../candidates/1.0  (install symlink ref)
  deps/
    c3d4e5f6a1b2c3d4   # → /path/to/objects/.../content  (forward-ref to dependency)
```

**Why `deps/` instead of re-reading metadata?** The `deps/` symlinks point to the **resolved platform-specific content path**. Metadata declares a dependency by digest, which may reference an Image Index (multi-platform). The actual content on disk is stored under the platform-specific manifest digest. The `deps/` symlink bridges this indirection — it was created at install time when platform resolution happened, so consumers (GC, env resolver, `ocx deps`) can follow it directly without repeating the resolution.

### Data model changes

#### Rust types (in `crates/ocx_lib/src/package/metadata/`)

New file: `dependency.rs`

```rust
/// A pinned dependency descriptor.
///
/// The digest references either an OCI Image Index (for platform-aware
/// resolution) or a single manifest (for explicit platform pinning).
/// For cross-compilation, pin the platform-specific manifest digest directly.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Dependency {
    /// Fully qualified pinned OCX identifier with required explicit registry
    /// and digest. The tag portion is advisory (for update tooling) — only
    /// the digest is used for resolution.
    pub identifier: oci::PinnedIdentifier,

    /// Controls how this dependency's environment variables propagate.
    /// Default: `Sealed` — no env contribution.
    #[serde(default)]
    pub visibility: Visibility,
}

/// How a dependency's environment variables propagate to the package
/// and its consumers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// No env propagation. Content accessed structurally (mount, path).
    #[default]
    Sealed,
    /// Env available for self-execution (shims), not propagated to consumers.
    Private,
    /// Env available for self-execution AND propagated to consumers.
    Public,
    /// Env propagated to consumers but not used by the package itself.
    Interface,
}
```

New type: `Dependencies` (ordered list wrapper with uniqueness enforcement)

```rust
/// Ordered list of package dependencies.
///
/// Serializes as a JSON array. Array position defines the canonical
/// environment import order. This avoids relying on JSON object key
/// ordering, which is unordered per RFC 8259 and not preserved by
/// all parsers (e.g., Go encoding/json, jq).
///
/// Deserialization validates that each identifier contains an explicit
/// registry (no default registry fallback) and that no identifier
/// appears more than once.
#[derive(Debug, Clone, Default)]
pub struct Dependencies {
    entries: Vec<Dependency>,
}
```

Internally, `Dependencies` provides map-like lookup by identifier (via linear scan — dependency lists are small) and enforces uniqueness on construction/deserialization. Array position = declaration order = environment import order.

The identifier is the `oci::PinnedIdentifier` type — an `Identifier` guaranteed to carry a digest. The digest is encoded inline in the identifier string (e.g. `ocx.sh/java:21@sha256:a1b2...`). Custom `Serialize`/`Deserialize` impls on `PinnedIdentifier` ensure the string form always includes the registry and digest, and that deserialization rejects identifiers without an explicit registry or without a digest.

New type: `PinnedIdentifier` (in `crates/ocx_lib/src/oci/pinned_identifier.rs`)

```rust
/// An `Identifier` that is guaranteed to carry a digest.
///
/// `TryFrom<Identifier>` validates the invariant; deserialization
/// rejects digest-less strings. Used in `ResolvedPackage` and
/// `InstallInfo` to ensure all resolved references are fully pinned.
pub struct PinnedIdentifier(Identifier);
```

New type: `ResolvedPackage` (in `crates/ocx_lib/src/package/resolved_package.rs`)

Persisted as `resolve.json` alongside `metadata.json` in each object directory. Written by `pull` at install time after platform resolution and transitive dependency collection. Read by `find`, `deps`, `env`/`exec` for environment composition and dependency inspection.

```rust
/// A dependency in the transitive closure with its pre-computed visibility.
///
/// The `visibility` field encodes the effective visibility from the root
/// package's perspective, computed via `Visibility::propagate` through
/// the dependency chain. Diamond deps use `Visibility::merge` (OR on
/// each axis) — if ANY path makes a dep visible, it stays visible.
pub struct ResolvedDependency {
    pub identifier: PinnedIdentifier,
    pub visibility: Visibility,
}

/// The fully resolved dependency closure for a package.
///
/// `identifier` is the platform-specific pinned identifier (after
/// Image Index resolution). `dependencies` is the transitive
/// closure in topological order (deps before dependents). The root
/// package itself is NOT included in `dependencies`.
pub struct ResolvedPackage {
    pub identifier: PinnedIdentifier,
    pub dependencies: Vec<ResolvedDependency>,
}
```

**Why `resolve.json` is not just a cache.** The metadata `dependencies` array declares digests that may reference Image Indexes (multi-platform). At install time, `pull` resolves these to platform-specific manifest digests. The `resolve.json` file captures this resolved state — consumers (GC, env resolver, `ocx deps`) can follow it directly without repeating platform resolution. For `deps/` symlinks, both `resolve.json` and the symlink targets point to the same resolved content — they are consistent by construction.

Changes to `Bundle`:

```rust
pub struct Bundle {
    version: Version,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    strip_components: Option<u8>,
    env: Env,
    /// Ordered list of package dependencies, pinned by digest.
    /// Array order defines environment import order.
    #[serde(skip_serializing_if = "Dependencies::is_empty", default)]
    dependencies: Dependencies,
}
```

#### Metadata version

This is a backward-compatible addition (new optional field with a default). The metadata `version` remains `V1`. Existing packages without `dependencies` deserialize with an empty `Dependencies` (empty map).

### Impact on CLI commands

| Command | Change |
|---------|--------|
| `ocx install` | Delegates to `pull` for the package + transitive deps. Only top-level gets symlinks. |
| `ocx package pull` | **Core change.** Recursively pulls transitive dependencies. Creates `deps/` forward-refs. This is the foundational operation all other fetching commands delegate to. |
| `ocx exec` | Auto-install (via `find_or_install`) now delegates to `pull` for transitive deps. Environment composition includes dependency envs in topological order. Conflict warnings via `ConstantTracker` (same as existing multi-package behavior). |
| `ocx env` | Same as `exec` — auto-install delegates to `pull`, environment includes transitive deps with conflict warnings. |
| `ocx find` | No change — finds the requested package only, no auto-install. |
| `ocx select` | No change — operates on locally-present packages only. |
| `ocx uninstall` | Removes the package's symlink. Does **not** remove dependencies (they may be shared). |
| `ocx uninstall --purge` | Removes the object (including its `deps/` directory). Dependencies may become unreachable and eligible for GC. |
| `ocx clean` | Graph-based GC: builds reachability graph from `deps/` edges, finds roots from `refs/` (and profile content-mode entries), BFS walk marks reachable objects, deletes unreachable (single pass, O(N + E)). |
| `ocx index update` | Transitively fetches and caches manifests for dependency digests. Since dependency metadata is part of the index (it lives in `metadata.json` which is pulled as part of the manifest), the index already contains the information needed to resolve transitive dependencies. Required for offline installs of packages with dependencies. |
| `ocx package create` | Validates dependency metadata: rejects cycles in the declared dependency graph (catches the problem at the earliest possible point). |
| `ocx deps` | **New command.** Shows the dependency tree (logical or flattened). `--flat` shows resolved evaluation order. `--why <dep>` traces dependency paths. Operates on locally-present packages only. |
| `ocx shell env` / `ocx ci export` | No change — these do not auto-install. Fail with `NotFound` if package is missing. |

### Dependency inspection commands

Two commands for inspecting the dependency graph. Both operate on locally-present packages only (no auto-install) and support `--json` output.

**`ocx deps <pkg>...`** — Show the dependency tree for one or more packages.

Accepts multiple packages, just like `exec` and `env`. When given multiple packages, the command builds the combined dependency graph — the same graph `exec` would use for environment composition.

Default output is the **logical tree** — the raw dependency graph as declared in metadata, rendered as an indented tree. Each node shows `identifier@digest` (digest truncated to first 12 hex chars for readability). Diamond dependencies appear at each position in the tree (no dedup in tree view) but are marked with `(*)` on repeated occurrences to indicate the subtree is not expanded again. With multiple packages, each top-level package is a root in the tree:

```
$ ocx deps myapp:1.0 mytool:2.0
myapp:1.0 (sha256:aaa1b2c3)
├── ocx.sh/java:21 (sha256:bbb4e5f6)
└── ocx.sh/cmake:3.28 (sha256:ccc7d8e9)
    └── ocx.sh/gcc:13 (sha256:ddd0a1b2)
mytool:2.0 (sha256:eee3f4a5)
└── ocx.sh/gcc:13 (sha256:ddd0a1b2) (*)
```

Flags:
- `--flat` — Show the **resolved / flattened view**: the combined evaluation order after topological sort, deduplication, and platform resolution across all given packages. This is the exact order `ocx exec <pkg>... -- cmd` uses for environment composition. Useful for debugging environment composition issues — particularly when multiple top-level packages share or conflict on transitive dependencies.
- `--depth N` — Limit tree depth (default: unlimited). `--depth 1` shows direct dependencies only.
- `--json` — JSON output. Tree view emits nested objects; flat view emits an ordered array.

```
$ ocx deps --flat myapp:1.0 mytool:2.0
ocx.sh/gcc:13       sha256:ddd0a1b2...
ocx.sh/cmake:3.28   sha256:ccc7d8e9...
ocx.sh/java:21      sha256:bbb4e5f6...
myapp:1.0            sha256:aaa1b2c3...
mytool:2.0           sha256:eee3f4a5...
```

Note: `gcc:13` appears once in the flat view despite being a dependency of both `cmake` (via `myapp`) and `mytool` directly — deduplication keeps the first occurrence in topological order, matching `exec` behavior.

**`ocx deps --why <dep> <pkg>...`** — Explain why a dependency is pulled in. Shows all path(s) from any of the given packages to `<dep>` in the combined dependency graph. Useful when a transitive dependency causes an unexpected environment conflict.

```
$ ocx deps --why ocx.sh/gcc:13 myapp:1.0 mytool:2.0
myapp:1.0 → ocx.sh/cmake:3.28 → ocx.sh/gcc:13
mytool:2.0 → ocx.sh/gcc:13
```

**Design notes:**
- `deps` is a top-level command (like `find`, `env`), not a subcommand of `package` — it operates on installed/pullable packages, not on package artifacts.
- Both views are derived from the same graph data structure used by `exec`/`env` for environment composition and by `clean` for GC. No separate data path.
- Multi-package `--flat` output is the primary debugging tool: if the environment from `ocx exec myapp:1.0 mytool:2.0 -- env` looks wrong, `ocx deps --flat myapp:1.0 mytool:2.0` shows exactly which packages contribute and in what order — including how deduplication resolved shared dependencies.

### Open questions for future consideration

1. **Circular dependency detection during `package push`**: Cycles are detected at `package create` time (the metadata is fully known at that point) and again at install time (as a safety net for packages created by other tools). Detection at push time is not feasible — it would require the registry to understand OCX metadata semantics.

3. **~~Resolved dependency tree cache.~~** Implemented as `resolve.json` — persisted alongside `metadata.json` in each object directory by `pull`. Contains a `ResolvedPackage` with the platform-resolved `PinnedIdentifier` and the transitive dependency closure in topological order. Used by `find`, `deps`, `env`/`exec` for offline environment composition without re-traversing metadata. See the Data model section for type definitions.

4. **Shims (separate ADR).** Auto-generated launcher scripts that invoke dependencies through `ocx exec` for per-invocation environment isolation. Shims would solve the same-repository-different-digest conflict problem by giving each tool its own clean environment. Related concerns: cross-platform script generation (bash/cmd/ps1), OCX version pinning, `PATHEXT` handling on Windows, environment variable stability contracts across OCX versions.

### Future directions (exploratory)

The following directions are areas we may explore in the future. They are not commitments or agreed-upon designs — just initial thinking captured while the context is fresh. Each would require its own ADR if pursued.

#### 5. Relaxing the digest-pin requirement

Currently, dependencies must be pinned by digest. We may explore allowing tag-only dependency declarations (e.g., `java:21` without a digest) in some contexts. The most likely home for this would be a project config / lockfile layer (future ADR), where an `ocx lock` command resolves tags to digests — similar to how `Cargo.toml` declares version ranges while `Cargo.lock` stores exact pins. Package metadata published to registries would likely retain mandatory digest pins to preserve reproducibility, but the project-level layer could offer more flexibility for development workflows where tracking rolling tags is desirable.

#### 6. Transitive collision auto-resolution via cascade lineage

When two packages depend on the same repository at different digests, both are currently installed and the environment conflict produces a warning. In many cases, these collisions involve versions within the same cascade lineage — e.g., `java:21.0.1` and `java:21.0.2` both cascade to `java:21`.

OCX's cascade convention establishes a publisher-declared compatibility relationship: when a publisher pushes `21.0.2` with `--cascade`, they assert it is a drop-in replacement under the `21` rolling tag. We could potentially leverage this lineage information to automatically prefer the higher version when both digests share a common rolling tag ancestor — conceptually similar to Go's Minimum Version Selection applied to cascade families.

Open considerations: this depends on having cascade history in the local index, only works for version-tagged packages, and would need clear semantics for when lineage cannot be determined. The project config / lockfile layer is a likely home, where `ocx lock` could perform resolution and record the decision transparently.

#### 7. Consumer-side dependency version overrides

There are scenarios where a consumer or project author may want to force a specific version of a transitive dependency across the entire graph — security patches not yet picked up by upstream packages, organizational compliance policies, or testing against newer versions. Inspiration comes from npm `overrides`, yarn `resolutions`, Cargo `[patch]`, and Maven `dependencyManagement`.

Since OCX distributes pre-built binaries (no recompilation), an override would simply swap one binary for another. The consumer takes responsibility for compatibility. Key aspects to consider if we pursue this:

- Overrides would likely be per-repository (registry + repo path), not per-tag, since the goal is typically to unify on one version of a tool everywhere.
- Clear diagnostics showing which packages had their dependencies overridden and what was replaced.
- Interaction with auto-resolution (direction 6) — overrides would presumably take precedence.
- Overrides would be a consumer-side concern only — they would not appear in published package metadata.
- A future `ocx audit` command could verify that overrides stay within the cascade compatibility lineage.

These three directions share a common theme: they add flexibility on top of the immutable, digest-pinned foundation. The project config / lockfile layer (already mentioned as a future ADR in this document's scope section) is the natural home for all three, keeping package metadata strict and reproducible while giving project authors the tools they need.

## Considered Options

### Option 1: Tag-based dependencies with resolution

**Description:** Dependencies declared with version ranges (e.g., `java >= 21`). A resolver finds a compatible version from the local index.

| Pros | Cons |
|------|------|
| Familiar to users of npm, cargo, pip | Breaks reproducibility — different index states yield different results |
| Flexible for publishers | Requires a version resolution algorithm (SAT-solving or MVS) |
| Can auto-upgrade transitive deps | Complex implementation |
| | Publisher and consumer may disagree on what "compatible" means |

### Option 2: Digest-pinned dependencies

**Description:** Dependencies pinned by OCI digest. No version resolution. Publisher declares exactly what they tested against.

| Pros | Cons |
|------|------|
| Perfectly reproducible — same metadata = same deps always | Publisher must update metadata to change a dependency |
| Zero resolution algorithm needed | No automatic security patches for transitive deps |
| Simple mental model | Requires tooling to help publishers manage digests |
| Matches OCI content-addressing semantics | |
| Works offline with no index at all | |

### Option 3: Hybrid — digest-pinned with advisory tags in an ordered array

**Description:** Like Option 2, but each entry includes the full identifier (with optional advisory tag) for human readability and update tooling. The tag is never used for resolution. Dependencies are an array to guarantee ordering across all JSON parsers.

| Pros | Cons |
|------|------|
| Same reproducibility as Option 2 | Slightly more complex metadata |
| Human-readable dependency lists | Advisory tag can drift from digest (harmless) |
| Enables future `update-deps` tooling | |

## Decision Outcome

**Chosen Option:** Option 3 — Digest-pinned with advisory tags, implemented as an ordered array

**Rationale:** The advisory tag adds significant UX value at negligible implementation cost. The tag is part of the identifier string but is never parsed for resolution — only the digest matters. The identifier enables future update tooling: `ocx package update-deps` can parse the tag, look up where it currently points in the registry, and show which digests have changed. An array (not a JSON object/map) is used because dependency ordering has semantic meaning for environment composition, and JSON object key ordering is not guaranteed by RFC 8259 — many parsers (Go `encoding/json`, `jq`) silently reorder keys. Array position makes ordering explicit and survives any parser.

### Consequences

**Positive:**
- Fully reproducible dependency graphs — the metadata is the complete truth.
- No resolution algorithm to implement, test, or debug.
- Works offline with no index whatsoever (digests are the pin).
- GC is correct via the existing `refs/` pattern with minimal extension.
- Backward compatible — existing packages deserialize with empty dependencies.

**Negative:**
- Publishers must explicitly update dependency digests when they want newer versions. This is by design (reproducibility > convenience), but tooling should help (e.g., `ocx package update-deps`).
- No automatic transitive security patches. If `java:21` gets a security fix, every package depending on the old digest must be republished.
- Same-repository-different-digest conflicts produce a warning (consistent with existing `exec` multi-package behavior). Proper resolution requires shims or restrictive dependencies (future ADRs).

**Risks:**
- **Deep transitive trees may slow installation.** Mitigation: parallel fetching of independent subtrees; caching of already-present objects.
- **Advisory tags in keys may drift from digest.** Mitigation: harmless — the digest is authoritative. Future tooling can detect drift.
- **~~GC fixpoint loop may be slow on very large stores with deep dependency chains.~~** Resolved: replaced with BFS reachability walk from roots — single-pass O(N + E) regardless of chain depth.
- **~~JSON object key order not guaranteed by all parsers.~~** Resolved: dependencies use an array, not a JSON object. Array ordering is guaranteed by all JSON parsers.

## Technical Details

### Architecture

```
ocx install myapp:1.0 --select
│
│  ┌─── pull (foundational operation) ───────────────────────────────────┐
│  │                                                                     │
│  ├─ Download myapp → objects/.../sha256:aaa.../                        │
│  │  └─ metadata.json declares: dependencies:                           │
│  │       [{ "identifier": "ocx.sh/java:21@sha256:bbb..." }]           │
│  │                                                                     │
│  ├─ Resolve dependency: java@sha256:bbb (Image Index → sha256:ccc)     │
│  │                                                                     │
│  ├─ Download java → objects/.../sha256:ccc.../                         │
│  │  └─ metadata.json: no dependencies (leaf)                           │
│  │                                                                     │
│  └─ Create dependency forward-ref:                                     │
│     └─ objects/.../sha256:aaa.../deps/{hash} → objects/.../sha256:ccc  │
│  └─────────────────────────────────────────────────────────────────────┘
│
├─ Create candidate symlink for myapp:
│  └─ installs/ocx.sh/myapp/candidates/1.0 → objects/.../sha256:aaa.../content
│  └─ objects/.../sha256:aaa.../refs/{hash_of_candidate} → installs/.../candidates/1.0
│
└─ Create current symlink for myapp (--select):
   └─ installs/ocx.sh/myapp/current → objects/.../sha256:aaa.../content
   └─ objects/.../sha256:aaa.../refs/{hash_of_current} → installs/.../current

Environment for "ocx exec myapp:1.0 -- cmd":
  1. Apply java env (dependency, topological order)
  2. Apply myapp env (dependent)
  ⚠ Same-repo-different-digest conflict → warning (consistent with exec)
```

### GC Algorithm (Reachability Walk)

```
fn clean(dry_run: bool) -> Vec<CleanedObject> {
    let all_objects = objects.list_all();

    // Phase 1: Build dependency graph from deps/ directories
    let mut edges: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    for obj in &all_objects {
        let deps: Vec<ObjectId> = obj.deps_dir()
            .read_symlinks()
            .filter_map(|symlink| objects.id_for_content(symlink.target()))
            .collect();
        edges.insert(obj.id(), deps);
    }

    // Phase 2: Find roots — objects with valid install symlink refs
    //          OR profile content-mode references
    let profile_roots = profile.content_mode_paths();
    let mut roots: Vec<ObjectId> = Vec::new();
    for obj in &all_objects {
        // Discard broken refs (symlinks to deleted installs)
        obj.refs_dir().cleanup_broken();
        if !obj.refs_dir().is_empty() || profile_roots.contains(&obj.dir()) {
            roots.push(obj.id());
        }
    }

    // Phase 3: Walk from roots — mark all reachable objects
    let mut reachable: HashSet<ObjectId> = HashSet::new();
    let mut queue: VecDeque<ObjectId> = roots.into();
    while let Some(id) = queue.pop_front() {
        if !reachable.insert(id.clone()) {
            continue; // already visited
        }
        if let Some(deps) = edges.get(&id) {
            queue.extend(deps.iter().cloned());
        }
    }

    // Phase 4: Collect unreachable objects
    let mut cleaned = Vec::new();
    for obj in &all_objects {
        if !reachable.contains(&obj.id()) {
            if !dry_run {
                remove_dir_all(objects.dir_for(&obj.id()));
            }
            cleaned.push(obj.id());
        }
    }

    cleaned
}
```

### Data Model

```
metadata.json (v1, Bundle):
{
  "type": "bundle",
  "version": 1,
  "strip_components": 1,
  "env": [
    { "key": "PATH", "type": "path", "value": "${installPath}/bin" },
    { "key": "MYAPP_HOME", "type": "constant", "value": "${installPath}" }
  ],
  "dependencies": [
    {
      "identifier": "ocx.sh/java:21@sha256:a1b2c3d4e5f6...",
      "visibility": "private"
    }
  ]
}
```

## Implementation Plan

1. [x] Add `Dependency` struct, `Dependencies` ordered list wrapper, and `dependencies` field to `Bundle` (backward-compatible)
2. [x] Implement dependency graph building and cycle detection (integrated into `pull` task)
3. [x] Implement topological sort with deduplication (persisted in `resolve.json` as `ResolvedPackage`)
4. [x] Add `PinnedIdentifier` type for digest-guaranteed identifiers
5. [x] Extend `ReferenceManager` with `link_dependency(dependent_content, dependency_content)` and `unlink_dependency()`
6. [x] Implement `pull` task to recursively pull transitive dependencies and create `deps/` forward-refs (foundational — all fetching commands delegate here)
7. [x] Update `install` task to delegate to `pull`, then create symlinks for top-level only
8. [x] Update `find_or_install` (used by `exec`, `env`) to delegate to `pull` for transitive deps when auto-installing
9. [x] Update `uninstall --purge` to delegate to `GarbageCollector` for cascading purge
10. [x] Update `clean` to use `GarbageCollector` with BFS reachability walk (build graph from `deps/`, find roots from `refs/` + profile content-mode, BFS to mark reachable, collect unreachable)
11. [x] Update `env`/`exec` environment composition to include dependency environments in topological order (via `resolve_env()` reading `resolve.json`)
12. [x] Integrate `ConstantTracker` (already exists) for conflict warnings during dependency env composition (consistent with existing `exec` behavior)
13. [x] Implement `ocx deps` command: tree view (default), `--flat` (resolved order), `--why` (path tracing), `--depth`, `--json`
14. [ ] Add cycle detection to `package create` (earliest possible validation)
15. [ ] Update `index update` to transitively fetch manifests for dependency digests
16. [x] Update JSON Schema generation (`task schema:generate`)
17. [x] Update metadata documentation (`website/src/docs/reference/metadata.md`)
18. [x] Add acceptance tests: install with deps, GC with deps, env composition with deps, diamond dedup, exec/env auto-install with deps, deps tree/flat/why output

## Validation

- [x] Backward compatibility: existing packages without `dependencies` continue to work unchanged
- [x] Cycle detection: circular dependencies produce a clear error (at pull/install time)
- [x] Diamond dedup: same dependency from multiple paths is installed once
- [x] GC correctness: dependency objects are not collected while a dependent is referenced
- [x] GC completeness: after removing all references to a dependent, its dependencies become collectible
- [x] Environment order: deterministic, reproducible across runs and platforms
- [ ] Cross-compilation: pinning a platform-specific manifest digest bypasses Image Index resolution
- [x] Required registry: dependency identifiers without explicit registry are rejected at deserialization
- [x] Array round-trip: dependencies survive serialize → deserialize with order preserved
- [x] Duplicate rejection: duplicate identifiers in dependencies array are rejected at deserialization
- [x] Env conflict warning: same-repo-different-digest dependencies produce a warning (consistent with existing `exec` behavior)
- [ ] Cycle detection at create: `package create` rejects metadata with circular dependencies
- [x] Pull is foundational: `install` delegates to `pull` for all object store operations
- [ ] Index update with deps: `index update` fetches manifests for transitive dependency digests
- [x] Deps tree: `ocx deps <pkg>` shows correct logical tree with `(*)` for repeated subtrees
- [x] Deps flat: `ocx deps --flat <pkg>` matches the exact evaluation order used by `exec`/`env`
- [x] Deps why: `ocx deps --why <dep> <pkg>` shows all paths from pkg to dep

## Links

- [Nix Pills: Automatic Runtime Dependencies][nix-pills-09]
- [Go Minimum Version Selection][go-mvs]
- [Bazel Lockfile Documentation][bazel-lockfile]
- [OCI Image Manifest Specification][oci-manifest]
- [Guix Features Manual][guix-features]
- [Homebrew Formula Cookbook][homebrew-cookbook]
- [Topological Sorting][topo-sort]
- [Existing conflict detection module](../../crates/ocx_lib/src/package/metadata/env/conflict.rs)
- [Reference manager](../../crates/ocx_lib/src/reference_manager.rs)
- [Clean task](../../crates/ocx_lib/src/package_manager/tasks/clean.rs)

[nix-pills-09]: https://nixos.org/guides/nix-pills/09-automatic-runtime-dependencies.html
[go-mvs]: https://research.swtch.com/vgo-mvs
[bazel-lockfile]: https://bazel.build/external/lockfile
[oci-manifest]: https://github.com/opencontainers/image-spec/blob/main/manifest.md
[guix-features]: https://guix.gnu.org/manual/1.5.0/en/html_node/Features.html
[homebrew-cookbook]: https://docs.brew.sh/Formula-Cookbook
[topo-sort]: https://en.wikipedia.org/wiki/Topological_sorting

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-21 | architect | Initial draft |
| 2026-03-21 | architect | Revised: map format (identifier:tag → digest), pull creates refs, required registry, Identifier type for keys, platform field clarified, same-repo-different-digest conflict analysis, dep_refs/ alternative noted, index update impact, resolved tree cache |
| 2026-03-21 | architect | Stripped to clean scope: dependencies only. Removed shims, launcher scripts, OCX runtime pin, bundled indexes — these are separate concerns (index locking, shims, OCX version pinning) that belong in future ADRs. |
| 2026-03-22 | review | Added `ocx deps` command to design: tree view (default), `--flat` (resolved order), `--why` (path tracing), `--depth`, `--json`. Moved from open questions into core design. Replaced GC fixpoint loop with Kahn's algorithm (O(N+E) single-pass). |
| 2026-03-22 | review | Array format (not JSON object map) — RFC 8259 says objects are unordered. Cycle detection at `package create`. `pull` as foundational operation for all fetching commands. Removed `platform` field — cross-compilation via direct manifest digest pin; platform overrides belong in future project config / lockfile layer. Conflict handling: warning (consistent with existing `exec` multi-package behavior), not error. Simplified env model: dependencies = `ocx exec` with auto-pulling. No `export` field, no `${dep:...}` expansion, no restrictive deps — all env auto-applied in topological order. Finer-grained env control deferred to shims ADR. |
| 2026-03-28 | architect | Added "Future directions (exploratory)" section: relaxing digest-pin at project config layer, transitive collision auto-resolution via cascade lineage, consumer-side version overrides. All exploratory — not design commitments. |
| 2026-03-28 | review | Simplified reference model: `refs/` for install symlink back-refs only, `deps/` for dependency forward-refs only. Removed dependency back-refs from `refs/` — redundant with graph reachability walk. GC simplified from Kahn's ref-count to plain BFS reachability from roots. Kahn's algorithm retained for topological sorting in env resolution. |
| 2026-04-03 | review | Post-implementation consistency pass. Status → Accepted. Fixed stale Kahn's refs in CLI impact table and risks. Promoted `resolve.json` from open question to core design. Added `PinnedIdentifier` and `ResolvedPackage` to data model. Updated `Dependency.digest` type from `String` to `oci::Digest`. Removed `jsonschema` feature gate. Added profile content-mode entries as GC roots. Updated implementation plan and validation checklists to reflect current state. |
| 2026-04-05 | architect | Replaced `export: bool` with `Visibility` enum (`sealed`, `private`, `public`, `interface`). Inspired by CMake's `target_link_libraries` visibility model. `sealed` (default) = no env propagation (backward-compatible with `export: false`). `private` = self-execution only (new). `public` = self + consumers (= old `export: true`). `interface` = consumers only (new). Added propagation tables for both consumer-visible and self-visible axes. Per-variable overrides deferred as future extension. |
