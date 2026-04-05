# System Design: OCX Composition Model

## Metadata

**Status:** Draft
**Author:** mherwig
**Date:** 2026-04-03
**Related ADRs:**
- `adr_package_dependencies.md` — dependency declaration, GC, env composition
- `adr_package_layers.md` (PR #22) — overlap-free multi-layer packages
- Future: shims ADR, project config / lockfile ADR

## Executive Summary

OCX has four composition mechanisms that combine packages and package content in different ways. This document defines the conceptual model, explains why each mechanism exists, how they relate to each other, and identifies a gap (filesystem composition of dependencies) that no current mechanism fills.

---

## 1. The Four Composition Mechanisms

```
Loose coupling                                              Tight coupling
├── Patches         — operator overlays env vars onto packages
├── Dependencies    — publisher declares required external packages
├── Variants        — publisher offers alternative builds (tag selection)
└── Layers          — publisher partitions internal package structure
```

All four are orthogonal: a package can have variants, layers, dependencies, and be patched by an operator simultaneously.

### Comparison Matrix

| Dimension | Dependencies | Layers | Patches | Variants |
|-----------|-------------|--------|---------|----------|
| **What it composes** | Independent packages | Parts of one package | Operator config onto packages | Alternative builds |
| **Who controls it** | Package publisher | Package publisher | Infrastructure operator | Package publisher |
| **Identity** | Each has own OCI identity | Share one manifest | Separate operator registry | Tag-based selection |
| **Filesystem** | Each in own `content/` | Merged into one `content/` | Env overlay only | Separate manifests |
| **`${installPath}`** | Each resolves independently | Single path for assembled package | Resolves to patch content | Per-variant path |
| **Lifecycle** | Independent versioning | Tied to parent manifest | Independent | Independent tags |
| **GC** | Ref-tracked via `deps/` | Ref-tracked via `layers/` | N/A | Standard object GC |
| **OCI standard** | OCX metadata field | Standard `manifest.layers[]` | OCX metadata field | Standard image index |

### Core Principle: Why Both Dependencies and Layers Exist

**Dependencies** compose **independent** packages. Each has its own identity, versioning, publisher, and lifecycle. A dependency can be shared across many consumer packages.

**Layers** compose **a single** package from **structurally coupled** parts. Layers share one package identity and are controlled by one publisher. They are a storage optimization — deduplicating shared content across platform variants of the same package.

The test: **does the component have its own version and release cycle?** If yes, it's a dependency. If it's the publisher partitioning their own files for efficiency, it's a layer.

---

## 2. Export Control on Dependencies

### The Problem

When package A depends on Java, both packages get installed. But should Java's env vars (`JAVA_HOME`, `PATH` entries) appear in A's environment? Before export control, the answer was always yes — every dependency's env leaked into the parent's environment, causing pollution.

### The Design

Each dependency carries an `export` flag (default: `false`):

```json
{
  "identifier": "ocx.sh/java:21@sha256:...",
  "export": true
}
```

| `export` value | Behavior |
|----------------|----------|
| `false` (default) | Dep is installed and GC-protected, but its env vars are **not** included in the parent's environment |
| `true` | Dep's env vars **are** included in the parent's environment |

### What Export Controls — and What It Does Not

Export controls **environment composition only**. It does not affect:
- **Installation** — all deps are pulled regardless of export
- **GC protection** — all deps create `deps/` forward-refs regardless of export
- **Filesystem presence** — the dep's `content/` directory exists in the object store either way

### Propagation Rule

When building the transitive dependency closure:
- Parent `export: true` → preserve child's own export flags
- Parent `export: false` → force ALL of that subtree to `export: false`
- Diamond dedup: first-wins (consistent with existing dedup policy)

```
A → B(export:true) → C(export:true)   → C is exported in A's env
A → B(export:false) → C(export:true)  → C is NOT exported (blocked by B)
```

### Conflict Detection Scope

Only exported dependencies participate in conflict detection. Two non-exported deps with the same repository but different digests do **not** trigger a conflict — their env vars are never composed, so there is nothing to conflict.

---

## 3. Shims and Export: Orthogonal Concerns

Shims (future ADR) are auto-generated launcher scripts that invoke a package through `ocx exec`, giving it a clean subprocess environment.

### Three Independent Decisions

| Decision | Controls | Mechanism |
|----------|----------|-----------|
| `export` on a dependency | "Does this dep's env contribute to **my** environment?" | Dependency metadata |
| Shim wrapping | "Does **my** combined env leak to whoever calls me?" | Shim launcher script |
| Whether to depend at all | "Do I need this package installed?" | Dependency declaration |

These are orthogonal:

```
A depends on Java (export: true)
  → A's environment includes JAVA_HOME
  → A needs JAVA_HOME to function, regardless of how A is invoked

A is wrapped in a shim
  → A's caller does NOT see JAVA_HOME
  → The shim runs `ocx exec A -- binary`, isolating A's env in a subprocess

Without the shim:
  → A's caller DOES see JAVA_HOME (via `ocx exec A other_pkg -- cmd`)
```

**The export flag answers "does this package need the dep's env to function?"** — a property of the package, not of how it's consumed. If A needs `JAVA_HOME`, it must declare `export: true` on its Java dependency whether it runs inside a shim or via `ocx exec` or any other mechanism.

**The shim answers "should this package's combined env (including exported deps) leak to its callers?"** — a property of how the package is consumed, controlled by the tool that generates the shim.

---

## 4. Why Layers Cannot Replace Dependencies (and Vice Versa)

### The Python Example

Consider an app that needs Python 3.12 in a subdirectory (like a venv).

**Option A: Python as a layer**

The publisher embeds the Python runtime as a layer in their manifest:

```
app-manifest:
  layers:
    - sha256:abc...  (Python runtime)
    - sha256:def...  (app code)
  → merged into one content/ directory
```

Problems:
1. **Lost identity.** Python is now an anonymous blob digest — no `ocx.sh/python:3.12` identity. Can't `ocx install python:3.12` separately.
2. **No sharing.** Every app that needs Python embeds its own layer. The registry deduplicates by blob digest only if the exact same archive is used — but different apps may need slightly different Python builds. No structural sharing relationship.
3. **Lifecycle coupling.** When Python gets a security patch, every app that embedded it must republish its entire manifest with the new layer digest. There's no way to say "update all packages that use this Python."
4. **Integrity risk.** Could a manifest reference a layer blob that belongs to another package? At the OCI level, yes — a digest is a digest. But there's no guarantee the other publisher keeps that blob available. If they republish and the old blob is cleaned from the registry, the app manifest has a dangling layer reference. **No cross-publisher integrity guarantee.**

**Option B: Python as a dependency**

```json
{
  "identifier": "ocx.sh/python:3.12@sha256:abc...",
  "export": false
}
```

Correct:
- Python keeps its identity, versioning, lifecycle
- Shared across all consumers (one object in the store)
- GC-protected while any dependent is installed
- `export: false` prevents env pollution

Missing: the app has no way to access Python's files at a known subpath. Python is in its own `content/` directory somewhere in the object store, reachable only via `deps/` symlinks.

### The Boundary Rule

| Question | If yes → | If no → |
|----------|----------|---------|
| Does the component have its own version and release cycle? | Dependency | Could be a layer |
| Is the component published by a different party? | Dependency | Could be a layer |
| Should the component be independently installable? | Dependency | Layer |
| Is this about dedup across platform variants of the same package? | Layer | Probably dependency |

---

## 5. The Gap: Filesystem Composition of Dependencies

Current mechanisms and what they provide:

| Need | Dependencies | Layers |
|------|-------------|--------|
| Independent identity & lifecycle | Yes | No |
| Env var composition | Yes (via `export`) | N/A |
| Filesystem at known subpath | **No** | Yes (merged `content/`) |
| Cross-publisher sharing | Yes | No |
| GC-tracked | Yes | Yes |

The gap: **a dependency whose content is accessible at a known subpath of the parent package**, without requiring env var export.

### Possible Future: Mount Dependencies

A dependency with a `mount` field specifying a subpath:

```json
{
  "identifier": "ocx.sh/python:3.12@sha256:...",
  "mount": ".venv"
}
```

Semantics:
- At pull time, create a symlink: `${installPath}/.venv` → Python's `content/` directory
- The parent knows Python is at `${installPath}/.venv/bin/python`
- No env export needed — the path is structural, not environmental
- Full dependency semantics: identity, lifecycle, GC protection, independent versioning

This would let a package declare "I need Python's files available at `.venv/`" — the same filesystem composition that a layer provides, but with the identity and lifecycle guarantees of a dependency.

### Architectural Consideration

Today, `content/` is the pure extracted archive — immutable after extraction. Adding mount symlinks into it makes it mutable post-extraction. Alternatives:
- **Symlinks inside `content/`** — simplest for consumers (`${installPath}/.venv` just works), but breaks content immutability
- **Sibling `mounts/` directory** — preserves content immutability, but `${installPath}` doesn't naturally include mounts
- **Assembly via hardlinks** (like layers) — creates a merged view but adds extraction complexity

This is an open design question for a future ADR.

---

## 6. Summary: When to Use What

| I want to... | Use |
|--------------|-----|
| Deduplicate shared files across platform variants of my package | **Layers** |
| Declare that my package needs another independently-versioned package | **Dependencies** |
| Make a dependency's env vars available to my package | **Dependencies** with `export: true` |
| Keep a dependency installed but hide its env vars | **Dependencies** with `export: false` (default) |
| Isolate my package's combined env from callers | **Shims** (future) |
| Overlay operator config onto a package's env | **Patches** |
| Offer alternative builds of my package | **Variants** |
| Make a dependency's files available at a known subpath | **Mount dependencies** (future — the gap) |

---

## Open Questions

- [ ] Should `mount` be a field on dependency, or a separate composition mechanism?
- [ ] How to handle mount conflicts (two deps mounted at the same subpath)?
- [ ] Should `export` evolve from boolean to an array of var names for fine-grained control?
- [ ] How do mounts interact with layers? (A multi-layer package with mounted deps)
- [ ] Should the `${deps.NAME.installPath}` interpolation syntax (referenced in the dependencies ADR) be implemented as an alternative to mounts?

---

## References

- `adr_package_dependencies.md` — dependency declaration, export control, GC, env composition
- `adr_package_layers.md` (PR #22) — overlap-free multi-layer packages, storage model
- `research_oci_layers_and_composition.md` (PR #22) — OCI layer spec, industry survey
- ocx-sh/ocx#20 — Supporting Layers (issue)
- ocx-sh/ocx#22 — Multi-Layer Packages (PR, design artifacts)
