---
paths:
  - crates/**/*.rs
  - external/**/*.rs
---

# OCX Architecture Principles

Auto-loaded on every Rust file edit. Provides stable architectural context — the "why" behind the design. For dynamic discovery of current code state, launch `worker-architecture-explorer`.

## Crate Layout

| Crate | Purpose | Dependency Direction |
|-------|---------|---------------------|
| `ocx_lib` | Core library — stores, OCI, packages, manager | Depends on nothing internal |
| `ocx_cli` | Thin CLI shell — args, context, commands, reporting | Depends on `ocx_lib` |
| `ocx_mirror` | Standalone mirror tool — upstream → registry pipeline | Depends on `ocx_lib` |
| `ocx_schema` | JSON schema generation (build-only) | Depends on `ocx_lib` |

Patched dependency: `oci-client` at `external/rust-oci-client` (local git submodule).

## Design Principles

| Principle | Where | Why |
|-----------|-------|-----|
| **Facade** | `PackageManager`, `Index`, `Client` | Single coordination point hides subsystem complexity |
| **Strategy / trait dispatch** | `IndexImpl` (local/remote), `OciTransport` (native/test), archive backends (tar/zip) | Testability, swappable implementations |
| **Composite root** | `FileStructure` wraps `BlobStore` + `LayerStore` + `PackageStore` + `TagStore` + `SymlinkStore` + `TempStore` | Three-tier CAS: raw blobs → extracted layers → assembled packages; tags and symlinks are the mutable namespace on top |
| **Three-layer errors** | `Error` → `PackageError` → `PackageErrorKind` | Per-package diagnosis in batch operations |
| **Command pattern** | CLI: args → identifiers → manager task → report data → API output | Uniform flow from input to output |
| **Ref separation for GC** | `refs/symlinks/` (install back-refs, GC roots), `refs/deps/`, `refs/layers/`, `refs/blobs/` (forward-refs) | Single BFS pass across all three CAS tiers for reachability; lock-free |
| **Option-based results** | `IndexImpl` returns `Option` (None = not found) | Not-found is not an error at the index layer |
| **Extension traits** | `StringExt` (slugify), `ResultExt`, `VecExt` in prelude | Ergonomic API surface without polluting core types |
| **Builder pattern** | `ClientBuilder`, `BundleBuilder` | Fluent construction with many optional parameters |
| **Singleton context** | CLI `Context` struct with lazy init | Avoid unused work; one init per invocation |

## End-to-End Command Flow

```
CLI command (clap parse)
  → Context::try_init() — FileStructure, Index, Client, PackageManager, Api
  → command/{name}.rs — transform identifiers → manager.task_all()
    → PackageManager — FileStructure + Index + Client coordination
      → Index (local/remote via IndexImpl) — resolve tag → digest
      → Client (OCI transport) — fetch manifest + layers
      → FileStructure — store object, create symlinks
    → Build report data from task results
  → Api.report() — Printable trait → stdout (plain/JSON)
```

## Key Concepts

| Concept | Definition |
|---------|-----------|
| **Blob** | Raw OCI blob (manifests, image indexes, referrers) stored in `blobs/{registry}/{algorithm}/{2hex}/{remaining_hex}/` |
| **Layer** | Extracted OCI tar layer stored in `layers/{registry}/{algorithm}/{2hex}/{remaining_hex}/content/` |
| **Package** | Assembled package stored in `packages/{registry}/{algorithm}/{2hex}/{remaining_hex}/`; `content/` files are hardlinked from `layers/` |
| **Index** | Local JSON snapshot of registry metadata (tags, manifests) for offline/reproducibility |
| **Candidate** | Symlink at `symlinks/{registry}/{repo}/candidates/{tag}` — pinned at install time |
| **Current** | Floating symlink at `symlinks/{registry}/{repo}/current` — set by `ocx select` |
| **Digest** | SHA-256 content hash — the immutable identity of a package version |
| **Tag** | Mutable alias to a digest (e.g., `3.28`, `latest`) |
| **Cascade** | Publisher convention: push `3.28.1` and auto-update `3.28`, `3`, `latest` tags |
| **Platform** | OS/arch pair (e.g., `linux/amd64`) for multi-platform manifest resolution |
| **Slug** | Filesystem-safe encoding: `to_relaxed_slug()` preserves `[a-zA-Z0-9._-]`, replaces rest with `_` |
| **Identifier** | Parsed OCI reference: `registry/repo:tag@digest` with default registry fallback |
| **Manifest** | OCI image manifest or image index (multi-platform) |
| **Refs** | Reference sub-directories inside `packages/.../refs/`: `symlinks/` (GC roots from install symlinks), `deps/` (forward-refs to other packages), `layers/` (forward-refs to layers), `blobs/` (forward-refs to blobs) |

## ADR Index

| ADR | Decision |
|-----|----------|
| `adr_cascade_platform_aware_push.md` | Per-platform version filtering and index merging |
| `adr_codesign_inside_out_signing.md` | Recursive inside-out Mach-O signing |
| `adr_codesign_per_file_signing.md` | Per-file signing replaces bundle signing |
| `adr_custom_oci_identifier.md` | Custom identifier parser replaces `oci_spec::Reference` |
| `adr_mirror_source_generators.md` | Generator-based URL index for mirror sources |
| `adr_oci_artifact_enrichment.md` | Signatures, SBOMs, and descriptive metadata on OCI artifacts |
| `adr_ocx_mirror.md` | Standalone binary mirroring tool design |
| `adr_release_install_strategy.md` | Release and install phased strategy |
| `adr_sbom_strategy.md` | SBOM generation approach |
| `adr_version_build_separator.md` | Underscore as build separator in version tags |
| `adr_three_tier_cas_storage.md` | Three-tier content-addressed storage (blobs + layers + packages) |

ADRs live in `.claude/artifacts/adr_*.md`. Read relevant ADRs before making decisions in the same domain.

## Code Style Conventions

Project-wide conventions enforced by the reviewer:

| Convention | Rule | Deviation = Bug |
|------------|------|-----------------|
| **Type names** | Full descriptive names (`OperatingSystem`, `Architecture`), not abbreviations (`Os`, `Arch`) | Abbreviated type names |
| **Module structure** | One concept per file, deep nested modules (`platform/operating_system.rs`) — no `mod.rs`, use named module files | Monolithic files, `mod.rs` files |

## Where Features Land

| Feature type | Location | Notes |
|--------------|----------|-------|
| New CLI command | `crates/ocx_cli/src/command/` | One file per command, follows command pattern |
| New task method | `crates/ocx_lib/src/package_manager/tasks/` | Add error variant to `error.rs` if needed |
| New output format | `crates/ocx_cli/src/api/data/` | Implement `Printable` trait |
| New storage path | `crates/ocx_lib/src/file_structure/` | Add to the appropriate store |
| New index operation | `crates/ocx_lib/src/oci/index/` | Implement on `IndexImpl` trait |
| New metadata field | `crates/ocx_lib/src/package/metadata/` | Update types + schema + docs |
| New acceptance test | `test/tests/test_*.py` | Use fixtures, maintain test isolation |

## Cross-Cutting Modules

These `crates/ocx_lib/src/` modules have no dedicated subsystem rule — they serve multiple subsystems:

| Module | Purpose | Used By |
|--------|---------|---------|
| `archive/` | Tar/zip extraction + bundling with pluggable backends | Mirror pipeline, package creation |
| `auth/` | `AuthType` enum with env var + docker cred fallback | OCI Client |
| `ci/` | CI flavor dispatch (GitHub Actions export) | `ci export` command |
| `profile/` | `ProfileManager` + `ProfileManifest` for shell profiles | Shell profile commands |
| `shell/` | `ShellProfileBuilder` — shell-specific export generation | Shell commands |
| `utility/` | Extension traits: `StringExt`, `ResultExt`, `VecExt`, `SerdeExt` | Everywhere via prelude |
| `compression/` | Compression level configuration | Archive, OCI push |
| `codesign/` | macOS ad-hoc code signing for Mach-O binaries | Package extraction |
