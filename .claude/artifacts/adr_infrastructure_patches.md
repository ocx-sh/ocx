# ADR: Infrastructure Patches — Site-Specific Configuration via Environment Overlays

## Metadata

**Status:** Proposed
**Date:** 2026-03-22
**Deciders:** mherwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** oci, architecture, packaging, configuration
**Supersedes:** N/A
**Superseded By:** N/A

## Context

OCX distributes pre-built binaries from OCI registries and composes their declared environment variables at exec time. This works well when the package publisher's environment declarations match the consumer's infrastructure. In enterprise environments, they often don't.

**Common mismatches:**

| Scenario | What the publisher declares | What the enterprise needs |
|----------|---------------------------|--------------------------|
| Java behind Zscaler proxy | `JAVA_HOME=${installPath}` | Also: `JAVA_TOOL_OPTIONS=-Djavax.net.ssl.trustStore=/path/to/company/cacerts` |
| Python tools behind corporate proxy | `PATH=${installPath}/bin` | Also: `UV_INDEX_URL=https://pypi.company.com/simple/` |
| npm behind corporate registry | `PATH=${installPath}/bin` | Also: `NPM_CONFIG_REGISTRY=https://npm.company.com/` |
| Any TLS tool behind MITM proxy | Tool-specific env | Also: `SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE`, `NODE_EXTRA_CA_CERTS` → company CA bundle |

Infrastructure operators cannot modify third-party packages. They need a mechanism to layer site-specific configuration on top of published packages, without forking or rebuilding them.

**Key constraint:** No scripting interface. Patches operate purely through environment variables. They don't modify package content — they layer additional env vars on top at exec time by co-installing companion packages whose metadata declares those variables.

**Relationship to dependencies (evelynn branch):** Dependencies are publisher-declared ("this tool requires Java 21"). Patches are infrastructure-operator-declared ("all Java installs in this company need the Zscaler cert"). Different intent, different lifecycle, but shared mechanics — both result in additional packages being installed with env vars composed at exec time, and both use the same GC model (back-references, ref counting).

## Decision Drivers

- Enterprise environments require site-specific configuration (certificates, proxy settings, mirror URLs) that package publishers cannot anticipate.
- Patches must compose with the existing dependency system (evelynn) and the exec-time environment resolution pipeline.
- The mechanism must work across different source registries — patches should not require push access to the publisher's registry.
- OCX_HOME must remain a self-contained redistributable unit: if you zip it, it works offline. Patches must be part of this contract.
- Solution must not introduce scripting, shell hooks, or content modification. Environment variables are the only injection mechanism.
- `ocx env`, `ocx exec`, `ocx deps` must show the composed result including patches — patches are integral, not optional overlays.

## Industry Context & Research

**Research artifact:** [research_infrastructure_patches_overlays.md](./research_infrastructure_patches_overlays.md)

**Trending approaches:** OCI Referrers API (v1.1) for attaching metadata to existing artifacts; declarative env composition (Nix overlays, mise cascading config); OCI artifacts as configuration delivery vehicles (Flux OCIRepository).

**Key insights:**
1. **Nix overlays** prove that declarative env var injection works without binary modification — the overlay changes runtime env, not the package content.
2. **Kustomize** provides the clearest articulation of the non-destructive invariant: publisher controls base, infra operator controls overlay, neither modifies the other, client applies merge at resolve time.
3. **OCI Referrers API** has broad registry adoption but imposes a same-registry constraint that is incompatible with the enterprise use case (infra operators typically don't have push access to upstream registries).
4. **Conda activate.d** and **Homebrew tap forks** are anti-patterns — shell integration breaks `ocx exec` clean-env, formula forks create maintenance burden.

## Considered Options

### Option 1: OCI Referrers API (Same Registry)

**Description:** Attach patch artifacts to package manifests via the OCI v1.1 `subject` field. Discovered automatically via `GET /v2/{repo}/referrers/{digest}`.

| Pros | Cons |
|------|------|
| Auto-discoverable, no client config needed | **Same-registry constraint** — referrers must live in the same registry as the target. Enterprise operators rarely have push access to upstream registries. |
| Version/platform-specific by design (subject = digest) | GHCR does not support Referrers API |
| Industry-standard mechanism (cosign, Notation, ORAS) | Requires each source registry to host its own patches — doesn't scale across registries |
| | Referrer graph grows unbounded without GC policy |

### Option 2: Well-Known Tag in Source Registry

**Description:** Push a `__ocx.patch` tag into the package's own repository (same pattern as `__ocx.desc`). Contains a JSON descriptor with version-matched patch rules.

| Pros | Cons |
|------|------|
| Works on all registries (no Referrers API needed) | **Requires push access to source registry** — same fundamental problem as Option 1 |
| Discoverable from registry alone | Tag pollution in upstream repositories |
| Single well-known location per repository | Cannot differentiate patches per consumer site |
| | Infra operator changes visible to all consumers of that registry |

### Option 3: Client-Configured Patch Registry (Recommended)

**Description:** Client-side configuration maps source registries to a separate patch registry controlled by the infrastructure operator. OCX looks up the same repository name in the patch registry, reads a patch descriptor from a well-known tag, and co-installs the listed companion packages.

| Pros | Cons |
|------|------|
| **No push access to source registry required** — patches live in the infra operator's own registry | Requires client-side configuration (not zero-config) |
| Works across any combination of source and patch registries | Patch registry must be accessible from every OCX client |
| Patch registry fully controlled by infra operator | Additional registry to maintain |
| Reuses existing OCX install/GC mechanics for companion packages | Resolution adds a network hop per patched repository |
| Offline-safe: patch descriptors and companion packages cached in OCX_HOME | |
| Composable: multiple patches per package, shared companion packages | |

### Option 4: Embedded in Package Metadata

**Description:** Package publishers declare "patch hooks" in `metadata.json` — env var overrides that consumers can activate via config.

| Pros | Cons |
|------|------|
| Zero infrastructure — all data in the package | **Publisher must anticipate all enterprise needs** — defeats the purpose |
| No additional registry or config | Bloats metadata with site-specific concerns |
| | Requires publisher cooperation for every new enterprise scenario |

## Decision Outcome

**Chosen Option:** Option 3 — Client-Configured Patch Registry

**Rationale:** This is the only option that satisfies the core constraint: infrastructure operators must be able to patch packages they don't control, stored in registries they can't push to. Options 1 and 2 both require push access to the source registry. Option 4 requires publisher cooperation.

The client-configured model follows the same pattern as OCX mirror configuration — a local config declares where to find infrastructure-specific resources. The patch registry is a regular OCI registry, fully controlled by the infra operator, storing lightweight patch descriptors and companion config packages.

### Consequences

**Positive:**
- Infrastructure operators get full control over site-specific configuration without depending on package publishers.
- Companion packages are regular OCX packages — no new artifact type, no new install mechanics, no new env resolution logic.
- Patch descriptors cached locally make offline work seamless.
- GC "just works" via the same back-reference mechanism as dependencies.
- Patches are visible and inspectable via `ocx env`, `ocx deps`, `ocx shell env`.

**Negative:**
- Requires client-side configuration — cannot work out of the box without setup.
- Introduces a dependency on a separate patch registry — if it's down and local cache is cold, patches can't be resolved.
- Adds complexity to the install/exec pipeline (one additional lookup per patched package per repository).

**Risks:**

| Risk | Impact | Mitigation |
|------|--------|------------|
| Patch registry unavailable | Packages install without patches (wrong env) | Cache patch descriptors locally; fail loudly if patches are configured but unreachable (patches are integral, not optional) |
| Companion package version drift | Patch references `zscaler-root:latest` but `latest` tag gets re-pointed | Document recommendation to pin companion tags; future: pin by digest in patch descriptor |
| Config file not present on new machines | Developer sets up OCX but forgets patch config | Document setup; future: company-wide default config distribution via OCX bootstrap |
| Multiple patches for same env var | Conflict between two companion packages setting `SSL_CERT_FILE` | Same conflict detection as existing multi-package env (ConstantTracker warning). Last-declared rule wins. |

## Technical Details

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│ OCX Client                                                  │
│                                                             │
│  ┌──────────┐    ┌──────────────┐    ┌───────────────────┐  │
│  │ Config   │───▶│ Patch        │───▶│ Package Manager   │  │
│  │ (TOML)   │    │ Resolver     │    │ (install/exec)    │  │
│  └──────────┘    └──────┬───────┘    └─────────┬─────────┘  │
│                         │                      │             │
│            ┌────────────┼──────────────────────┤             │
│            ▼            ▼                      ▼             │
│  ┌──────────────┐ ┌──────────────┐  ┌──────────────────┐   │
│  │ Patch Index  │ │ Package      │  │ Object Store     │   │
│  │ (cached      │ │ Index        │  │ (base + companion │   │
│  │  descriptors)│ │ (existing)   │  │  packages)        │   │
│  └──────────────┘ └──────────────┘  └──────────────────┘   │
│                                                             │
│  ═══════════════════ OCX_HOME ═════════════════════════════ │
└─────────────────────────────────────────────────────────────┘
         │                    │                    │
         ▼                    ▼                    ▼
  ┌──────────────┐   ┌──────────────┐   ┌──────────────────┐
  │ Patch        │   │ Source       │   │ Companion Pkg    │
  │ Registry     │   │ Registry     │   │ Registry         │
  │ (infra-owned)│   │ (upstream)   │   │ (may be same as  │
  └──────────────┘   └──────────────┘   │  patch registry) │
                                        └──────────────────┘
```

### Configuration Format

Location: `$OCX_HOME/config.toml` (global) — future: project-level `.ocx/config.toml` with cascading.

```toml
[patches]
registry = "internal.company.com/ocx-patches"
```

**Repository path mapping:** When a single patch registry serves patches for multiple source registries, repository names can collide — `ocx.sh/java` and `ghcr.io/myorg/java` both map to `java`. To prevent this, the source registry is included in the repository path within the patch registry by default.

The `path` key controls how the source package's identity maps to a repository path in the patch registry. It is a template string with two variables:

| Variable | Expands to | Example |
|----------|------------|---------|
| `{registry}` | Source package's registry (slugified) | `ocx.sh` → `ocx_sh` |
| `{repository}` | Source package's repository | `java` |

```toml
# Default: includes registry to avoid cross-registry collisions
[patches]
registry = "internal.company.com/ocx-patches"
path = "{registry}/{repository}"
# ocx.sh/java:21 → internal.company.com/ocx-patches/ocx_sh/java:__ocx.patch
# ghcr.io/myorg/java:21 → internal.company.com/ocx-patches/ghcr_io/myorg/java:__ocx.patch
```

```toml
# Single-source shorthand: omit registry prefix when patch registry serves one source only
[patches]
registry = "internal.company.com/ocx-patches"
path = "{repository}"
# ocx.sh/java:21 → internal.company.com/ocx-patches/java:__ocx.patch
```

```toml
# Custom layout: arbitrary prefix or restructuring
[patches]
registry = "internal.company.com"
path = "patches/{registry}/{repository}"
# ocx.sh/java:21 → internal.company.com/patches/ocx_sh/java:__ocx.patch
```

If `path` is omitted, it defaults to `{registry}/{repository}`. The `{registry}` variable is slugified using the same `to_relaxed_slug()` function used for filesystem paths (preserves `[a-zA-Z0-9._-]`, replaces everything else with `_`).

**Future extension** (not in initial scope):

```toml
# Per-source-registry overrides with their own path templates
[patches]
default = "internal.company.com/ocx-patches"

[patches.registries.ocx_sh]
registry = "corp-mirror.company.com/ocx-patches"
path = "{repository}"  # no registry prefix needed — dedicated patch registry

[patches.registries.ghcr_io]
registry = "internal.company.com/ghcr-patches"
path = "{repository}"
```

### Patch Descriptor Format

Stored as an OCI artifact at the well-known tag `__ocx.patch` in the patch registry's repository. The artifact is an OCI ImageManifest with:
- `artifactType: application/vnd.sh.ocx.patch.v1`
- Config: empty descriptor (`application/vnd.oci.empty.v1+json`)
- Single layer: the patch descriptor JSON (`application/vnd.sh.ocx.patch.descriptor.v1+json`)

```json
{
  "version": 1,
  "rules": [
    {
      "match": "*",
      "packages": [
        "internal.company.com/certs/zscaler-root:latest"
      ]
    },
    {
      "match": "21",
      "packages": [
        "internal.company.com/java-config/jdk21-truststore:1.0"
      ]
    }
  ]
}
```

**Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `version` | integer | Schema version (always `1` for now) |
| `rules` | array | Ordered list of patch rules |
| `rules[].match` | string | Glob pattern matched against the package tag. `*` matches all tags. |
| `rules[].packages` | array of strings | OCI identifiers (with registry, repository, and tag) of companion packages to co-install. |

**Resolution semantics:**
- All rules are evaluated against the installed package's tag.
- Multiple rules can match — their package lists are accumulated (union, deduplicated by identifier).
- Rules are evaluated in order; first occurrence of a duplicate identifier wins.
- If no rules match, no patches are applied for this repository.

### Companion Packages

Companion packages are regular OCX packages. They contain no special structure — just the standard `metadata.json` with env var declarations, and optionally file content (e.g., a CA certificate bundle).

**Example: Certificate bundle package** (`internal.company.com/certs/zscaler-root:latest`)

Content:
```
ca-bundle.crt      # PEM-encoded root CA certificate chain
cacerts            # Java keystore with the same certs (for JKS consumers)
```

Metadata:
```json
{
  "type": "bundle",
  "version": 1,
  "env": [
    {
      "key": "SSL_CERT_FILE",
      "type": "constant",
      "value": "${installPath}/ca-bundle.crt"
    },
    {
      "key": "REQUESTS_CA_BUNDLE",
      "type": "constant",
      "value": "${installPath}/ca-bundle.crt"
    },
    {
      "key": "NODE_EXTRA_CA_CERTS",
      "type": "constant",
      "value": "${installPath}/ca-bundle.crt"
    }
  ]
}
```

**Example: Mirror config package** (`internal.company.com/config/pypi-mirror:latest`)

Content: (empty — env-only package)

Metadata:
```json
{
  "type": "bundle",
  "version": 1,
  "env": [
    {
      "key": "UV_INDEX_URL",
      "type": "constant",
      "value": "https://pypi.company.com/simple/"
    },
    {
      "key": "PIP_INDEX_URL",
      "type": "constant",
      "value": "https://pypi.company.com/simple/"
    }
  ]
}
```

### `${installPath}` Resolution

`${installPath}` in a companion package's env vars resolves to that companion package's own content directory. This is consistent with how all OCX packages work — no special rules for patches.

There is no cross-reference mechanism between a companion package and the package it patches. If a companion needs to reference the patched package's install path (e.g., to modify a file within it), that is out of scope — patches work through environment variables only, not content modification.

### Resolution Flow

When `ocx exec ocx.sh/java:21 -- mvn build` is invoked:

```
1. Parse config.toml → patch registry = "internal.company.com/ocx-patches"
                        path template  = "{registry}/{repository}" (default)

2. Resolve base package:
   └─ Install ocx.sh/java:21
      ├─ index:    index/ocx.sh/tags/java.json  →  {"21": "sha256:aaa..."}
      ├─ object:   objects/ocx.sh/java/sha256/aaa.../content/
      └─ install:  installs/ocx.sh/java/candidates/21 → object content

3. Resolve patches:
   ├─ Apply path template: "{registry}/{repository}" → "ocx_sh/java"
   ├─ Check local index: index/internal.company.com_ocx-patches/tags/ocx_sh/java.json
   │   ├─ File missing         → never synced, fetch from remote (or error if offline)
   │   ├─ File exists, no key  → synced, no patch for this repo (skip)
   │   └─ "__ocx.patch" key    → digest of patch descriptor
   │
   ├─ Pull patch descriptor (if not in object store):
   │   ├─ object:  objects/internal.company.com_ocx-patches/ocx_sh/java/sha256/bbb.../content/
   │   │           └─ descriptor.json  (the patch rules JSON)
   │   └─ symlink: artifacts/internal.company.com_ocx-patches/ocx_sh/java/patch
   │               → object content (back-ref created via ReferenceManager)
   │
   ├─ Read descriptor.json via artifacts/ symlink
   ├─ Match tag "21" against rules:
   │   ├─ Rule "*" matches  → ["internal.company.com/certs/zscaler-root:latest"]
   │   └─ Rule "21" matches → ["internal.company.com/java-config/jdk21-truststore:1.0"]
   ├─ Deduplicate package list
   └─ Install companion packages (regular package install):
       ├─ internal.company.com/certs/zscaler-root:latest
       │   ├─ index/object/install as usual
       │   └─ back-ref from companion object → base package install symlink
       └─ internal.company.com/java-config/jdk21-truststore:1.0
           ├─ index/object/install as usual
           └─ back-ref from companion object → base package install symlink

4. Build unified dependency graph:
   ├─ Resolve java:21's declared dependencies (evelynn) → Dependency edges
   ├─ Resolve patches (from step 3) → Patch edges (companion → java:21)
   └─ Resolve each companion's own declared dependencies transitively → Dependency edges

5. Topological sort (Kahn's algorithm, single pass over unified graph):
   ├─ glibc:2.38 env           (dep of dep, topo order)
   ├─ gcc:13.2 env              (dep of java, topo order)
   ├─ java:21 env vars          (base package)
   ├─ zscaler-root env vars     (patch companion, applied after base → overrides)
   ├─ jdk21-truststore env vars (patch companion, applied after base → overrides)
   └─ Conflict detection via ConstantTracker:
       ├─ Patch override of base var → info (intentional)
       ├─ Two companions overriding same var → warn (conflict)
       └─ Dependency conflict → warn (same as evelynn)

6. Spawn: mvn build (with composed env)
```

### Environment Composition Order — Unified Dependency Graph

Patches and dependencies are **edges in the same DAG**, not separate phases. A companion package logically depends on the base package it patches (its env vars must apply after the base to override them). This relationship is modeled as a `Patch` edge in the dependency graph, alongside the existing `Dependency` edges from evelynn.

**Edge types:**

```rust
enum EdgeKind {
    Dependency,  // publisher-declared in metadata.json (evelynn)
    Patch,       // infra-declared via patch descriptor + config (sion)
}
```

Both edge types are treated identically by Kahn's algorithm — they're just edges in the DAG. The distinction matters only for display (`ocx deps` shows `[dep]` vs `[patch]`) and conflict semantics (see below).

**Graph construction** for `ocx exec java:21 -- mvn build`:

```
1. Start with java:21 as root
2. Read java:21's metadata.json → dependencies (evelynn)
   → Add Dependency edges: java:21 → gcc:13.2 → glibc:2.38
3. Resolve patches for java:21 (from config + descriptor)
   → Add Patch edges: zscaler-root → java:21, jdk21-truststore → java:21
4. Recursively resolve each companion's own declared dependencies
   → If zscaler-root depends on ca-updater:1.0, add Dependency edge: zscaler-root → ca-updater:1.0
```

**Resulting graph:**

```
glibc:2.38 ←[dep]── gcc:13.2 ←[dep]── java:21 ←[patch]── zscaler-root ←[dep]── ca-updater:1.0 (if any)
                                          ↑
                                     [patch]── jdk21-truststore
```

**Topological sort** (Kahn's algorithm, single pass, lexicographic tiebreaker):

```
glibc:2.38          (dep of dep)
gcc:13.2            (dep of java)
java:21             (base package)
ca-updater:1.0      (dep of companion, if any)
zscaler-root        (patch companion)
jdk21-truststore    (patch companion)
```

Later entries override earlier entries for `constant` type vars. For `path` type vars, later entries append (same as existing multi-package behavior). No separate composition phases — the unified topological order is the composition order.

**Why this works:** Companions come after the base package in topological order because they "depend on" it via `Patch` edges. Infrastructure intent naturally overrides publisher intent. Example: if `java:21` declares `SSL_CERT_FILE=/usr/share/ca-certificates/ca-bundle.crt` but the company's `zscaler-root` companion sets `SSL_CERT_FILE=${installPath}/ca-bundle.crt`, the companion's value wins — it appears later in the topological order.

**Multi-package exec** (`ocx exec A B -- cmd`): The existing `flatten_multi` algorithm works unchanged — it operates on the richer graph (with both `Dependency` and `Patch` edges) but the algorithm is the same: global topo sort, per-root BFS reachability, dedup by digest, keep first occurrence.

**Companion ordering:** When multiple companions patch the same base package, their relative order is determined by the lexicographic tiebreaker in Kahn's algorithm (same determinism guarantee as dependencies). If a specific override order between companions matters, the infra operator should use companion dependencies to enforce it (companion B depends on companion A → A's env applies first).

**Override intent — conflict detection semantics:**

The `ConstantTracker` must distinguish intentional patch overrides from accidental conflicts:

| Scenario | Level | Message |
|----------|-------|---------|
| Companion overrides base package's env var | **info** | `SSL_CERT_FILE overridden by zscaler-root (patch)` — this is the intended behavior |
| Two companions override the same env var | **warn** | `SSL_CERT_FILE conflict: zscaler-root and company-ca both set it` — ambiguous intent |
| Dependency conflict (same as evelynn) | **warn** | `JAVA_HOME conflict: jdk and jre both set it` — unintended collision |

To implement this, `ConstantTracker` receives the `EdgeKind` of each contributing package. When a constant is overwritten, it checks whether the overriding package is reachable from the overridden package via a `Patch` edge — if so, it's intentional.

**Cycle safety:** Patches cannot create cycles in the normal case because the companion depends on the base, never vice versa. However, a pathological scenario is possible: companion `A` declares a dependency on a package that itself depends on `java:21` (the base being patched). This would create a cycle: `java:21 ←[patch]── A ──[dep]──▶ ... ──[dep]──▶ java:21`. The existing DFS cycle detection (3-color marking from evelynn) catches this and errors with the full cycle path. No new cycle detection needed.

### Filesystem Layout

A patch registry is just another registry. The object store and index are reused as-is. The one new top-level directory is a **per-repo artifact symlink store** — parallel to `installs/` but for non-package artifacts (patch descriptors now, potentially descriptions/SBOMs later).

**Directory name (TBD):** Candidates — `artifacts/`, `enrichments/`, `refs/` (taken), `extra/`, `attached/`. The name should be generic enough for future artifact types. For this ADR, `artifacts/` is used as placeholder.

```
$OCX_HOME/
  config.toml                                          # NEW: client configuration

  artifacts/                                           # NEW: per-repo artifact symlink store
    {patch_registry}/
      {path_template_repo}/
        patch                                          # → objects/{patch_registry}/{repo}/{digest}/content/
                                                       #   (back-ref in object refs/ points back here)
                                                       #   Future: desc, sbom, signature, etc.

  index/
    ocx.sh/                                            # source registry (existing)
      tags/java.json                                   #   {"21": "sha256:aaa...", "latest": "sha256:aaa..."}
      objects/sha256/.../aaa.json
    internal.company.com_ocx-patches/                  # patch registry (same structure)
      tags/ocx_sh/java.json                            #   {"__ocx.patch": "sha256:bbb..."}
      objects/sha256/.../bbb.json
    internal.company.com/                              # companion pkg registry (same structure)
      tags/certs/zscaler-root.json                     #   {"latest": "sha256:ccc..."}
      objects/sha256/.../ccc.json

  objects/
    ocx.sh/java/sha256/.../                            # base package (existing)
      content/
      metadata.json
      refs/
    internal.company.com_ocx-patches/ocx_sh/java/sha256/.../  # patch descriptor object
      content/
        descriptor.json                                # the patch rules JSON
      metadata.json                                    # OCI config (empty descriptor)
      refs/
        {hash}                                         # → artifacts/.../ocx_sh/java/patch
    internal.company.com/certs/zscaler-root/sha256/.../ # companion package
      content/
        ca-bundle.crt
        cacerts
      metadata.json                                    # env var declarations
      refs/
        {hash}                                         # → installs/ocx.sh/java/candidates/21

  installs/                                            # existing — only regular packages
    ocx.sh/java/
      candidates/21                                    # → objects/.../content
      current                                          # → objects/.../content
    internal.company.com/certs/zscaler-root/            # companion package
      candidates/latest                                # → objects/.../content
```

**Design rationale for a separate symlink store:**
- **Clear separation of concerns** — `installs/` holds user-facing packages (selected by `ocx install`/`ocx select`), `artifacts/` holds per-repo metadata artifacts (resolved from config or registry). Walking `artifacts/` enumerates all cached artifacts without mixing them into install listings.
- **Extensible** — the `patch` entry sits alongside future per-repo artifacts. Example future state:
  ```
  artifacts/{registry}/{repo}/
    patch      → patch descriptor object content
    desc       → description object content (currently in __ocx.desc tag)
    sbom       → SBOM object content (from referrers)
  ```
- **Same mechanics** — symlink → object content, back-ref from object `refs/` → symlink path. `ReferenceManager::link()` and `unlink()` work unchanged.
- **GC works identically** — back-refs from artifact objects point to `artifacts/.../` symlinks. When a symlink is removed, the back-ref breaks, and the object becomes collectible by `ocx clean`.

### Three-State Patch Index

The local index provides a clear three-state model for patch presence, using the existing `tags/{repo}.json` file:

| State | Index file | `__ocx.patch` key | Meaning |
|-------|-----------|-------------------|---------|
| **Never synced** | File does not exist | N/A | Patch registry was never checked for this repo. Must fetch before proceeding (or error if offline). |
| **Synced, no patch** | File exists (may be `{}`) | Absent | Patch registry was checked; no `__ocx.patch` tag exists for this repo. This is a valid state — not all repos have patches. |
| **Synced, has patch** | File exists | Present → `"sha256:..."` | Patch exists. Digest points to the patch descriptor manifest. |

This distinction is critical:
- **Never synced** is an error condition when patches are configured — it means the index is incomplete.
- **Synced, no patch** is a valid, expected state — it means the infra operator has no patches for this particular package.
- On `ocx index update`, the sync creates or updates the tags file for each patch repo that was checked, even if no `__ocx.patch` tag exists, to record the "checked" state.

### Patch Synchronization

Patch descriptors are synced via the same `LocalIndex::update()` mechanism as regular packages. No new sync infrastructure needed. **The key rule: whenever the index of a source package is updated, the corresponding patch index is updated too.**

**Sync triggers:**

1. **On `ocx index update <pkg>`** — After syncing the source package's tags, compute the patch repo from the path template, and call `local_index.update(patch_identifier)` on the patch registry's index. This fetches the `__ocx.patch` tag (if it exists) and records the result in `tags/{repo}.json`. If a patch exists and the digest changed, pull the updated descriptor object and update the symlink in `artifacts/`.
2. **On install** — Install triggers an index update for the package (existing behavior). The patch index update piggybacks on this — same flow as trigger 1.
3. **On exec (index miss)** — If the patch registry's `tags/{repo}.json` doesn't exist (never synced), fetch from remote before proceeding. If offline and never synced, **fail with an error**.
3. **On exec (index miss)** — If `tags/{repo}.json` doesn't exist for the patch repo (never synced), fetch from remote. If offline and never synced, **fail with an error** — patches are integral, not optional. If synced and no patch (file exists, no key), proceed without patches.

**Offline contract:** If OCX_HOME contains synced index files and installed objects (both patch descriptors and companion packages), `ocx exec` works fully offline. Zipping OCX_HOME produces a redistributable archive that includes all patch state — because patch data lives in the same stores as all other package data.

### CLI Impact

**Modified commands:**

| Command | Change |
|---------|--------|
| `ocx install <pkg>` | After installing base package, resolve and install companion packages from patch config. Cache patch descriptor. |
| `ocx exec <pkg> -- <cmd>` | Compose env includes companion package env vars. Auto-install companions if not present (same as base auto-install). |
| `ocx env <pkg>` | Output includes companion package env vars. Shows which vars come from patches (annotation or `--show-patches` flag). |
| `ocx shell env <pkg>` | Shell export statements include companion package env vars. |
| `ocx deps <pkg>` | Shows both dependencies and patches in the package graph. Patches distinguished by marker. |
| `ocx uninstall <pkg>` | Companion packages' back-refs are removed. Companions become GC-eligible. |
| `ocx clean` | Unreferenced companion packages collected (same as unreferenced dependency objects). |
| `ocx index update` | Also syncs patch registry index for repos with existing local tags files. Same `LocalIndex::update()` path. |

**`ocx deps` output with patches:**

```
java:21 (ocx.sh)
├── [dep] gcc:13.2 (ocx.sh)
│   └── [dep] glibc:2.38 (ocx.sh)
├── [patch] zscaler-root:latest (internal.company.com/certs)
│   └── [dep] ca-updater:1.0 (internal.company.com/certs)
└── [patch] jdk21-truststore:1.0 (internal.company.com/java-config)
```

**`ocx env` output with patches:**

```
JAVA_HOME=/home/user/.ocx/objects/ocx.sh/java/sha256_abc123/content
PATH=/home/user/.ocx/objects/ocx.sh/java/sha256_abc123/content/bin
SSL_CERT_FILE=/home/user/.ocx/objects/internal.company.com/certs/zscaler-root/sha256_def456/content/ca-bundle.crt  # [patch: zscaler-root:latest]
REQUESTS_CA_BUNDLE=/home/user/.ocx/objects/internal.company.com/certs/zscaler-root/sha256_def456/content/ca-bundle.crt  # [patch: zscaler-root:latest]
JAVA_TOOL_OPTIONS=-Djavax.net.ssl.trustStore=/home/user/.ocx/objects/internal.company.com/java-config/jdk21-truststore/sha256_789ghi/content/cacerts  # [patch: jdk21-truststore:1.0]
```

### Garbage Collection — Reference Direction Analysis

In dependencies (evelynn), the relationship is straightforward:
```
myapp depends_on java  (publisher-declared, stored in myapp's metadata)

Forward:  myapp object deps/0/ → java object content/   ("I depend on this")
Back-ref: java object refs/{hash} → myapp object content/ ("someone needs me")
```

For patches, the dependency direction is inverted — the base package doesn't know about its companions:
```
java is_patched_by zscaler-root  (infra-declared, stored in patch descriptor)

Forward:  java object has NO deps/ entry  (can't modify third-party package objects)
Back-ref: zscaler-root object refs/{hash} → ???
```

**Chosen model — descriptor holds forward refs, base install holds back-ref targets:**

```
Patch descriptor:
  Forward:  descriptor object deps/0/ → zscaler-root content/
            (descriptor declares "these companions are needed")

Companion (zscaler-root):
  Back-ref: zscaler-root refs/{hash} → installs/ocx.sh/java/candidates/21
            (companion is "needed by this specific install")

Descriptor itself:
  Back-ref: descriptor refs/{hash} → artifacts/.../patch symlink
            (descriptor stays alive while patch config references it)
```

This splits the "owning" role between two entities:
- The **descriptor** provides discoverability via `deps/` — answers "which companions belong to this patch?" without consulting config.
- The **base package install symlink** provides version-specific lifecycle — answers "is this companion still needed?"

**GC cascade when `java:21` is uninstalled:**

```
1. installs/ocx.sh/java/candidates/21 symlink removed   (existing uninstall behavior)

2. zscaler-root refs/{hash} → candidates/21              now broken
   zscaler-root refs/{hash2} → candidates/17             still valid (java:17 installed)
   → zscaler-root has 1 valid ref, NOT collected

3. Later: java:17 also uninstalled
   zscaler-root refs/{hash2} → candidates/17             now broken
   zscaler-root has 0 valid refs → collected by ocx clean

4. Descriptor object:
   descriptor refs/{hash} → artifacts/.../patch           still valid (config unchanged)
   → descriptor NOT collected (cheap, stays cached for future installs)
```

**GC cascade when patch config is removed:**

```
1. artifacts/.../patch symlink removed                    (config no longer references this repo)

2. descriptor refs/{hash} → artifacts/.../patch           now broken
   descriptor has 0 valid refs → collected by ocx clean

3. descriptor deps/0/ → zscaler-root content/             edge in reverse graph
   Kahn's algorithm: descriptor collected → decrement zscaler-root's dep-refcount
   If zscaler-root also has no install back-refs → collected
```

**Why this works with existing GC (evelynn's Kahn's algorithm):**
- Phase 1: count `refs/` entries for each object; read `deps/` to build edge graph
- Phase 2: seed queue with zero-refcount objects; traverse reverse graph
- Patch descriptor objects and companion objects are just more nodes in the same graph. The `deps/` on the descriptor provides the edge; the `refs/` on the companion provides the lifecycle signal. No new GC algorithm needed.

**Key asymmetry vs dependencies:**

| Aspect | Dependencies | Patches |
|--------|-------------|---------|
| Forward ref (`deps/`) owner | Dependent's object (myapp) | Patch descriptor object |
| Back-ref target | Dependent's content or install | Base package install symlink |
| Discoverable from object store? | Yes (`deps/` in dependent) | Partially (descriptor `deps/`, but base package has no forward pointer) |
| Discoverability for `ocx deps` | Read `metadata.json` → `deps/` | Read patch config → descriptor → `deps/` |

### Data Model (Rust Types)

```rust
// Patch configuration (from config.toml)
pub struct PatchConfig {
    pub registry: Option<String>,  // patch registry URL
    pub path: Option<String>,      // path template, default: "{registry}/{repository}"
}

// Artifact store (parallel to InstallStore, manages artifacts/ symlinks)
// Name TBD — artifacts/, enrichments/, attached/, etc.
pub struct ArtifactStore {
    root: PathBuf,  // $OCX_HOME/artifacts/
}

impl ArtifactStore {
    pub fn patch(&self, identifier: &Identifier) -> PathBuf;
    // → artifacts/{registry}/{repo}/patch
    // Future: desc(), sbom(), signature(), etc.
}

// Patch descriptor (from __ocx.patch tag content, deserialized from descriptor.json)
pub struct PatchDescriptor {
    pub version: u32,
    pub rules: Vec<PatchRule>,
}

pub struct PatchRule {
    pub match_pattern: String,     // glob pattern against package tag
    pub packages: Vec<String>,     // OCI identifiers of companion packages
}

// Resolution result
pub struct ResolvedPatches {
    pub packages: Vec<oci::Identifier>,  // deduplicated companion packages to install
}

// Edge type in the unified dependency graph (shared with evelynn's Dependency edges)
pub enum EdgeKind {
    Dependency,  // publisher-declared in metadata.json (evelynn)
    Patch,       // infra-declared via patch descriptor + config (sion)
}

// Extended FlatEntry (adds edge_kind to evelynn's existing FlatEntry)
// edge_kind indicates how this package was reached — used for:
// - Display: `ocx deps` shows [dep] vs [patch] markers
// - Conflict semantics: ConstantTracker uses edge_kind to distinguish
//   intentional patch overrides (info) from accidental conflicts (warn)
```

### Interaction with Dependencies

Patches and dependencies are **edges in the same dependency graph**, unified under a single topological sort. They are not independent systems composed in separate phases.

- **Dependencies** (`EdgeKind::Dependency`) are embedded in `metadata.json` and resolved transitively (evelynn).
- **Patches** (`EdgeKind::Patch`) are resolved from external configuration and add companion packages as nodes with `Patch` edges pointing to the base package.
- Both edge types participate in a single Kahn's topological sort. The resulting order is the environment composition order — no special-casing needed.
- A companion package may itself declare dependencies (resolved transitively as `Dependency` edges, same as any other package).
- A dependency and a companion package may be the same package (deduplicated by digest, keep first occurrence in topo order).
- `DependencyResolver::resolve_env()` iterates the unified flattened list — the `Accumulator` / `Exporter` pipeline is unchanged.

See [Environment Composition Order](#environment-composition-order--unified-dependency-graph) for the full graph construction and ordering semantics.

### Well-Known Tag Convention

Following the established `__ocx.desc` pattern:

| Tag | Purpose | Media Type |
|-----|---------|------------|
| `__ocx.desc` | Repository description (README, logo) | `application/vnd.sh.ocx.description.v1` |
| `__ocx.patch` | Patch descriptor (rules → companion packages) | `application/vnd.sh.ocx.patch.v1` |

Both use the `__ocx.` prefix for internal hidden tags. The `is_internal_tag()` function already filters these from user-facing tag listings.

## Implementation Plan

### Phase 1: Configuration Infrastructure

1. [ ] Define `config.toml` schema and loading in `ocx_lib`
2. [ ] Parse `[patches]` section into `PatchConfig` (registry, path template)
3. [ ] Wire config loading into `Context` (available to all commands)
4. [ ] Path template expansion: `{registry}` (slugified) + `{repository}` → patch repo identifier

### Phase 2: Patch Descriptor

5. [ ] Add `ArtifactStore` to `FileStructure` — manages `artifacts/` (TBD name) symlink directory with `patch()` method
6. [ ] Define `PatchDescriptor` and `PatchRule` types with serde
7. [ ] Pull `__ocx.patch` tag via existing `LocalIndex::update()` flow — no new OCI client methods needed
8. [ ] Create symlink `artifacts/{registry}/{repo}/patch` → object content via `ReferenceManager::link()`
9. [ ] Implement glob matching for `PatchRule.match_pattern` against tags

### Phase 3: Companion Package Resolution & Graph Integration

9. [ ] Implement `PatchResolver` — given a package identifier + tag, read patch descriptor from installed candidate, resolve companion packages
10. [ ] Integrate with `PackageManager` install flow — after base install, resolve and install companions as regular packages
11. [ ] Create back-refs from companion objects to base package install symlinks
12. [ ] Integrate with `ocx index update` — when syncing a source package, also sync the patch registry index for the corresponding repo
13. [ ] Add `EdgeKind::Patch` to evelynn's dependency graph — companion packages added as nodes with `Patch` edges to the base package
14. [ ] Resolve companion packages' own declared dependencies transitively (as `Dependency` edges in the same graph)

### Phase 4: Environment Composition (Unified Graph)

15. [ ] `DependencyResolver::resolve_env()` iterates the unified flattened list (no separate patch phase — patches are already in topo order via the graph)
16. [ ] Extend `ConstantTracker` with `EdgeKind`-aware conflict semantics: patch overrides → info, companion-vs-companion conflicts → warn
17. [ ] Annotate patch-sourced env vars in `ocx env` output (using `EdgeKind` from `FlatEntry`)
18. [ ] Extend `ocx shell env` to include companion env vars (no special logic — they're in the flat list)
19. [ ] Extend `ocx deps` to show patch companions with `[patch]` markers (using `EdgeKind`)

### Phase 5: Lifecycle & Edge Cases

17. [ ] Extend `ocx uninstall` to remove companion back-refs
18. [ ] Verify `ocx clean` collects unreferenced companion and descriptor objects (no new GC code)
19. [ ] Three-state index: error on never-synced + offline, skip on synced-no-patch
20. [ ] Add `--show-patches` flag to env/deps commands

## Validation

- [ ] End-to-end: configure patch registry, install package, verify companion packages installed and env composed
- [ ] Offline: install with patches, disconnect, verify `ocx exec` works from cache
- [ ] GC: uninstall patched package, verify companion packages collected by `ocx clean`
- [ ] Multi-rule: verify multiple matching rules accumulate companion packages
- [ ] Override semantics: verify `ConstantTracker` emits info (not warn) when companion overrides base package env var via `Patch` edge
- [ ] Conflict: verify `ConstantTracker` warns when two companions override the same env var
- [ ] `ocx env` / `ocx deps` output includes patch information
- [ ] Three-state index: never-synced → error offline, synced-no-patch → skip, synced-has-patch → resolve
- [ ] Transitive companion deps: companion with own dependencies → all resolved and composed in correct topo order
- [ ] Unified graph ordering: base deps → base → companion deps → companions (verified via `ocx env` output order)
- [ ] Cycle detection: companion whose transitive deps include the base package → clear error with cycle path
- [ ] No regression on unpatched packages (no config → no change in behavior)

## Links

- [ADR: OCI Artifact Enrichment](./adr_oci_artifact_enrichment.md) — establishes `__ocx.` well-known tag convention
- [Research: Infrastructure Patches & Overlay Systems](./research_infrastructure_patches_overlays.md)
- Evelynn branch — dependency system (transitive install, GC, env composition)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-22 | mherwig | Initial draft |
| 2026-03-28 | mherwig | Unified patches into dependency graph as `EdgeKind::Patch` edges — single Kahn's topological sort replaces three-phase env composition; added transitive companion dependency support, override intent semantics for `ConstantTracker`, cycle safety analysis |
