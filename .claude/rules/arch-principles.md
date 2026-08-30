---
paths:
  - crates/**/*.rs
  - external/**/*.rs
---

# OCX Architecture Principles

Auto-load on every Rust file edit. Provide stable architectural context — "why" behind design. For dynamic discovery of current code state, launch `worker-architecture-explorer`.

## Crate Layout

| Crate | Purpose | Dependency Direction |
|-------|---------|---------------------|
| `ocx_lib` | Core lib — stores, OCI, packages, manager | Depend nothing internal |
| `ocx_cli` | Thin CLI shell — args, context, commands, reporting | Depend `ocx_lib` |
| (mirror tool) | Moved to own repo [ocx-sh/ocx-mirror](https://github.com/ocx-sh/ocx-mirror) — vendors ocx as submodule, `ocx_lib` path dep | — |
| `ocx_schema` | JSON schema gen (build-only) | Depend `ocx_lib` |

Patched dep: `oci-client` at `external/rust-oci-client` (local git submodule).

## Core vs Plugin Boundary (owner doctrine, 2026-07-16)

- **`ocx` core is self-contained**: complete primitive set (`ocx package *`, toolchain tier) stays in the one binary. No verb extraction to slim the binary.
- **Add-ons are `ocx-<name>` plugins** (git/cargo-style dispatch, shipped — `app/plugin_dispatch.rs`). No plugin ABI, ever.
- **Boundary is behavioral, not link-level**: package/store/registry *operations* go through the CLI surface (`ocx package create/push/test`, …) — CLI = the stable contract. Linking *vocabulary/utility* crates (version, identifier, slug, platform types) is fine.
- **Long-term**: split `ocx_lib` into smaller, cleanly layered crates (future `ocx-lib` repo); plugins link foundation crates, drive operations via CLI.
- Known drift: `ocx-mirror` reaches into operational internals — migration target is CLI for operations (pending refactor).

## Design Principles

| Principle | Where | Why |
|-----------|-------|-----|
| **Facade** | `PackageManager`, `Index`, `Client` | Single coordination point hide subsystem complexity |
| **Strategy / trait dispatch** | `IndexImpl` (local/remote), `OciTransport` (native/test), archive backends (tar/zip) | Testability, swappable impls |
| **Composite root** | `FileStructure` wraps `BlobStore` + `LayerStore` + `PackageStore` + `IndexStore` (`index/` — the local index collection) + `SymlinkStore` + `StateStore` + `TempStore`; `--index`/`OCX_INDEX` construct a second `IndexStore` outside `FileStructure` instead of using its `index` field | Three-tier CAS: raw blobs → extracted layers → assembled packages; symlinks = mutable namespace on top |
| **Three-layer errors** | `Error` → `PackageError` → `PackageErrorKind` | Per-package diagnosis in batch ops |
| **Command pattern** | CLI: args → identifiers → manager task → report data → API output | Uniform flow input → output |
| **Ref separation for GC** | `refs/symlinks/` (install back-refs, GC roots), `refs/deps/`, `refs/layers/`, `refs/blobs/` (forward-refs) | Single BFS pass across all three CAS tiers for reachability; lock-free |
| **Option-based results** | `IndexImpl` returns `Option` (None = not found) | Not-found not error at index layer |
| **Extension traits** | `StringExt` (slugify), `ResultExt`, `VecExt` in prelude | Ergonomic API surface no pollute core types |
| **Builder pattern** | `ClientBuilder`, `BundleBuilder` | Fluent construction with many optional params |
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
| **Blob** | Raw OCI blob (manifests, image indexes, referrers) stored at `blobs/{registry}/{algorithm}/{2hex}/{remaining_hex}/` |
| **Layer** | Extracted OCI tar layer at `layers/{registry}/{algorithm}/{2hex}/{remaining_hex}/content/` |
| **Package** | Assembled package at `packages/{registry}/{algorithm}/{2hex}/{remaining_hex}/`; `content/` files hardlinked from `layers/` |
| **Index** | One wire format (hosted `index.ocx.sh` grammar), differing only by location (hosted/mirrored/shipped/local), provenance (published copy vs OCX-derived), and completeness (partial vs full-mirror). Local home is a **collection** at `index/{source}/` — dispatch objects only (`o/<algo>/<hex>.json`), never a leaf manifest. Resolves *version choice* offline; plays no part in `ocx.lock` resolution (locks are index-free). |
| **Candidate** | Symlink at `symlinks/{registry}/{repo}/candidates/{tag}` — pinned at install time |
| **Current** | Floating symlink at `symlinks/{registry}/{repo}/current` — set by `ocx select` |
| **Project ledger** | Flat symlink store at `$OCX_HOME/projects/` — one symlink per registered project, name = 16-hex SHA-256 of canonical project dir, target = project dir. GC roots for multi-project clean. Self-link for the global toolchain is prohibited (global file's project dir is `$OCX_HOME`); instead `clean::collect_project_roots` adds an **implicit `$OCX_HOME/ocx.lock` root** so global lock-pinned packages are GC roots without a ledger entry (ADR `adr_global_toolchain_tier.md` D5 amended 2026-05-19). ADR: `adr_project_gc_symlink_ledger.md`. **Not to be confused with `$OCX_HOME/state/projects/<key>/`** — same key derivation, but that root holds per-project shell-activation consent stamps (deletable at any time), not GC truth (`adr_shell_env_overhaul.md` Decision 2). |
| **Global toolchain** | `$OCX_HOME/ocx.toml` + `$OCX_HOME/ocx.lock`, reachable only via explicit root `--global` flag (before the subcommand) or `OCX_GLOBAL` env var. `--global` is defined once on `ContextOptions` (peer of `--project`); per-command `--global` flags do not exist. Strict isolation: never composes into project resolution; `run`/`exec` are always hermetic. Env resolution = **lock-pinned digests, offline** (`resolve_global_pinned_env`) — the project tier with a different load site; the `current` symlink is a separate install/uninstall/select-only abstraction and is NOT consulted, so `ocx --global update` takes effect with no select step (ADR D5 amended 2026-05-19). Shell-exposed via `$OCX_HOME/env.sh` sourced from the login profile (managed activation block written by `ocx self setup`; runs `eval "$(ocx --global env --shell=sh)"`). No static `$OCX_HOME/init.<shell>`, no per-prompt hook, no PATH strip — isolation by PATH precedence. ADRs: `adr_global_toolchain_tier.md`, `handshake_toolchain_cli.md`, `adr_self_setup.md`. |
| **Digest** | SHA-256 content hash — immutable identity of package version |
| **Tag** | Mutable alias to digest (e.g., `3.28`, `latest`) |
| **Cascade** | Publisher convention: push `3.28.1` and auto-update `3.28`, `3`, `latest` tags |
| **Platform** | OS/arch pair (e.g., `linux/amd64`) for multi-platform manifest resolution; optionally refined by `variant` and ABI features (libc family on Linux via `os.features`, e.g. `libc.glibc`). Canonical grammar: `os/arch[/variant][+feature[,feature...]]` \| `any` — single, injective, round-tripping string shared by `--platform`, `ocx.lock` keys, and the build receipt `ocx package create` writes beside a bundle (`crates/ocx_cli/src/build_receipt.rs` — a build artifact with no schema, read back by `push`/`test`, never published; dependency pins carry no separate platform encoding of their own, just a bare digest). Resolution is the directed relation `is_compatible(required, offered)` + lexicographic `select_best` scoring — not strict equality; one shared helper across fresh-resolve, lock-read, and authoring pinning. One platform per authoring invocation (no bundle-level target set); `patch sync`'s bare-invocation concrete-ship-matrix fan-out is the one sanctioned exception. Host libc detected per-host by `HostCapabilities::detect` via discovery-then-identify (a system binary's `PT_INTERP` + arch-filtered loader scan + allowlist fallback, then `--version` banner classification). ADRs: `adr_platform_model_unification.md` (relation, grammar, lock V3, single-platform authoring), `adr_platform_libc_os_features.md` (libc namespace + host detection). |
| **Slug** | Filesystem-safe encoding: `to_relaxed_slug()` preserves `[a-zA-Z0-9._-]`, replaces rest with `_` |
| **Identifier** | Parsed OCI reference: `registry/repo:tag@digest` with default registry fallback |
| **Manifest** | OCI image manifest or image index (multi-platform) |
| **Refs** | Reference sub-dirs inside `packages/.../refs/`: `symlinks/` (GC roots from install symlinks), `deps/` (forward-refs to other packages), `layers/` (forward-refs to layers), `blobs/` (forward-refs to blobs), `origins/` (provenance markers — one regular file per repository this host resolved this digest under (logical, not transport), written only on a genuine fetch; deliberately outside the GC reachability graph, since it records provenance rather than liveness — consumed by the shell-activation namespace-grant predicate, `crate::project::consent::verified_sources`) |
| **DirtyRcBlock (exit 82)** | `ExitCode::DirtyRcBlock = 82` — `ocx self setup` exits 82 when a managed activation block in a shell profile carries user edits inside the fence and `--force` was not passed. Scripts can `case $? in 82)` to detect and re-run with `--force`. Distinct from `ConfigError` (78): the RC content is valid but intentionally modified by the user. |
| **State** | Ephemeral, registry-scoped or subsystem-scoped runtime state at `state/{subsystem}/{key}.json`; TTL-bound; not GC-walked — with **one exception**, `state/projects/<key>/` (project-scoped, not subsystem-scoped), swept by `ocx clean` on the stamp's own recorded `project_dir` liveness. Examples: `state/referrers/<registry>.json` for OCI Referrers API capability cache (`adr_oci_referrers_signing_v1.md` Amendment 3); `state/trust_root/<rekor-authority>.json` for the offline-verify trust-root cache (`adr_offline_verify_trust_cache.md`); `state/projects/<key>/consent.json` for the per-project shell-activation consent stamp (`adr_shell_env_overhaul.md` Decision 2); `state/host/capabilities.json` for the Linux-only host libc-detection cache (`HostCapabilities::detect`, 1h TTL). |

## ADR Index

| ADR | Decision |
|-----|----------|
| `adr_cascade_platform_aware_push.md` | Per-platform version filtering + index merging |
| `adr_platform_libc_os_features.md` | libc family differentiation via `os.features` + `libc.*` namespace; `can_run()` subset matcher (superseded by `adr_platform_model_unification.md` D1's `is_compatible`/`select_best`) |
| `adr_platform_model_unification.md` | Directed compatibility relation (`is_compatible`/`compatibility_score`/`select_best`, one shared helper across fresh-resolve, lock-read, authoring pinning); canonical single-grammar platform string (`os/arch[/variant][+feature[,feature...]]` \| `any`); `ocx.lock` V3 (only supported version, canonical-key validation, no digest-value uniqueness); single-platform resolution + authoring (`TargetPlatforms` deleted, `patch sync` keeps the one sanctioned multi-platform fan-out) |
| `adr_codesign_inside_out_signing.md` | Recursive inside-out Mach-O signing |
| `adr_codesign_per_file_signing.md` | Per-file signing replace bundle signing |
| `adr_custom_oci_identifier.md` | Custom identifier parser replace `oci_spec::Reference` |
| `adr_mirror_source_generators.md` | Generator-based URL index for mirror sources |
| `adr_oci_artifact_enrichment.md` | Signatures, SBOMs, descriptive metadata on OCI artifacts |
| `adr_oci_referrers_discovery.md` | OCI Referrers API for signature + SBOM discovery (superseded by v2) |
| `adr_oci_referrers_signing_v1.md` | Keyless Sigstore signing via OCI Referrers (Slice 1 — sign + verify) |
| `adr_trust_policy.md` | Identity-pinned verify via `[[trust.policy]]` in `config.toml` + `ocx.toml`; operator `config.toml` array-appends + is authoritative over project `ocx.toml` (`resolve_tiered`), most-specific-scope + ANY-of within a tier, `--certificate-identity`/`-oidc-issuer` optional-when-policy-matches (#98) |
| `adr_ocx_mirror.md` | Standalone binary mirroring tool design |
| `adr_release_install_strategy.md` | Release + install phased strategy |
| `adr_sbom_attestations.md` | DSSE in-toto attestations as cosign v3 bundles over OCI referrers; `attest`/`sbom` commands, `verify --attestation` mode; CycloneDX SBOM read + `--summary` |
| `adr_sbom_strategy.md` | SBOM gen approach |
| `adr_version_build_separator.md` | Underscore as build separator in version tags |
| `adr_three_tier_cas_storage.md` | Three-tier content-addressed storage (blobs + layers + packages) |
| `adr_index_routing_semantics.md` | `IndexOperation::{Query, Resolve}` enum; pinned-id pulls skip tag commit |
| `adr_cli_high_low_layering.md` | Formalize high-level (project-tier) vs OCI-tier CLI split; add `ocx run`; reserve `all` keyword |
| `adr_windows_exe_shim.md` | Native Windows `.exe` shim + `.shim` sidecar replaces the `.cmd` launcher (no `.cmd` emitted; PATHEXT inject/warn machinery removed); fully eliminates BatBadBut `%*`; committed-blob embed (A1 + B1 + C2 + D1) |
| `adr_project_gc_symlink_ledger.md` | Flat symlink store `$OCX_HOME/projects/` as project GC ledger (supersedes `adr_clean_project_backlinks.md`) |
| `adr_global_toolchain_tier.md` | Explicit `--global` toolchain tier, strict isolation, no implicit home fallback (supersedes Amendment C of `adr_project_toolchain_config.md`) |
| `handshake_toolchain_cli.md` | **AUTHORITY for current CLI model** — `ocx package` group (OCI tier), root `ocx [--global] env [--shell]` (`--global` is a root flag before the subcommand), `ocx shell` carries `{completion, state}` (`state` added by `adr_shell_env_overhaul.md` §2), root `install/uninstall/select/exec/deselect/which/deps/ci/shell hook/init/env` removed (exit 64), activation via `$OCX_HOME/env.sh` block-marker, no PATH strip. Decisions 3/4/6/7 of `adr_global_toolchain_tier.md` superseded here. Per-command `--global` and `with_command_global` seam deleted 2026-05-17 (root-only collapse). |
| `adr_progress_architecture.md` | Span-free progress: `cli::progress::ProgressManager` owns `indicatif::MultiProgress`; RAII guards (`Spinner`/`BytesBar`) instead of `tracing-indicatif` span-attached bars. Kills the concurrent sharded-registry clone-after-close panic by construction. `tracing-indicatif` dropped; fmt logs route through `ProgressManager::writer()` (suspend-coordinated). |
| `adr_ci_env_export_flag.md` | Realize handshake §6 CI export as `--ci[=provider]` flag on `ocx env`/`ocx package env` (not a command); GitHub autodetected two-file sink (rejects `--export-file`); GitLab JSON-lines, stdout default / `--export-file`; `--ci` ⟂ `--shell`; GitLab flavor added. |
| `adr_self_setup.md` | `ocx self setup` — bare-binary install complement to the install script: bootstrap + env shim write + managed RC-block (`# >>> ocx v1 <hash> >>>`) in user shell profiles; `ExitCode::DirtyRcBlock` (82) for user-edited blocks; `ocx self update` refreshes shims post-swap (4C). |
| `adr_cli_plugin_pattern.md` | Git-style `ocx <name>` → `ocx-<name>` PATH dispatch; plugins inherit parent env (trust boundary); built-ins always shadow |
| `adr_index_indirection.md` | One index format, many copies — differing only by location/provenance/completeness; `LocalIndex` owns the local **collection** (home `--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index`, one subtree per source, outside GC graph); object store holds dispatch objects only (`o/<algo>/<hex>.json`), leaf manifests never copied, absence ⇒ image-index self-heal from the blob store, digest-verified; crash-safe per-package updates with derivable catalog entries; index resolves *version choice* only — `ocx.lock` is index-free; logical-identity keying with transport-only physical refs (mirror-seam precedent); keep tags (`__ocx.keep.<algorithm>-<hex>`) default-on at push; index.ocx.sh two-hop client (root→dispatch, frozen ● shapes, yank/deprecation surfacing, authored `c/index.json`, no catalog sync) + `[registries."<ns>"] index` protocol selector + `[mirrors]` registry/index role split (F5); components `LocalIndex`/`OcxIndex`/`OciIndex`/`ChainedIndex`; `TagLock`/`TagStore`/`TagGuard` deleted, no migration |
| `adr_oci_index_only_dispatch.md` | `o/` holds OCI image indices verbatim, indices-only enforced (no bare-manifest tags recorded), reserved tags (`__ocx*`, including the keep tag, plus the frozen legacy `sha256.<hex>` form) never versions; supersedes `adr_index_indirection.md` A2/A3/C1/D/F1/F4 |
| `adr_managed_config_tier.md` | Corporate managed configuration tier (`[managed]`) — scope: One-Way-Door Medium. Seed pointer in `config.toml` resolves to an operator-published `config.toml` package (v2: config-as-package, superseding the v1 custom-artifact wire shape), identity-gated snapshot merged as a synthetic 5th precedence tier, `ocx config push`/`ocx config update` reuse ordinary package machinery (versioning, cascade, rollback). |
| `adr_project_toolchain_links.md` | **(Proposed)** Stable addressing for composed toolchains: `.ocx/toolchain/<group>/<entry>` link tree next to its `ocx.toml` (global: `$OCX_HOME/toolchain/`), lock-entry links only (deps stay digest-pinned), `${installPath}` override map, heal-before-emit, junctions on Windows. Consumer matrix: dereference→link, exec-with-composed-env→entrypoint launcher, ambient exec→linked bin dir |
| `adr_live_env_reload.md` | Original typed three-way reconciler proposal with provenance ledger, evaluated against `OCX_PATH_BACKUP` conda-style stash/restore and direnv-style untyped diff alternatives (superseded in full by `adr_shell_env_overhaul.md`, which carried the chosen option forward) |
| `adr_shell_env_overhaul.md` | Typed, provenance-tagged three-way per-prompt shell-env reconciler (desired D / current C / private `__OCX_ENV_STATE` ledger L), riding `ocx self activate`'s hidden `--reconcile` arm; project-scoped consent stamps at `state/projects/<key>/consent.json` gate activation (`paths`/`namespaces` grants, `ShellConsent`'s `deny_unknown_fields` carve-out); `[shell]` config section (`hook`/`completions` rungs); new read-only `ocx shell state` diagnostic (Decision 10). Supersedes `adr_live_env_reload.md` in full; amends `handshake_toolchain_cli.md` §2 + §7/§7a |
| `adr_interpolation_token_grammar.md` | Unified `${…}` interpolation grammar — one single-pass scanner (`metadata/template/scanner.rs`) replaces five old recognition sites; four recognised bodies (`installPath` bare + `self.installPath` exact alias, `self.env.KEY`, `deps.NAME.installPath`), each optional `:native`/`:posix` render modifier; `$${` is the only escape; claim-all/closed-world (D3) — an unrecognised token is refused, never passed through; refusal scoped to resolution not reading (D14) — `pull`/`install`/`inspect`/`deps`/`which` echo an unknown token verbatim, `env`/`exec`/`run`/compose/`package create`·`push` refuse (exit 65); three-branch diagnostic (suggested root / escape hint / supported-body list, D13); supersedes `adr_entrypoint_args_interpolation.md` D6's factual claim about the implementation (that scanner type was never built) while preserving its decision |
| `adr_index_sync_performance.md` | **(Proposed)** `ocx index sync`/`index update` resilience without stored remote state: auth request-cache coalescing + host-level 401 purge (D-001–D-003a), per-package root-fetch coalescing (D-004/D-005), within-package partial-commit granularity by content digest rather than whole-package (D-008), an index-transport retry ladder with full-jitter backoff + clamped `Retry-After` (D-010), and eviction-on-read for `utility::singleflight::Group` (D-005a) |

ADRs live in `.claude/artifacts/adr_*.md`. Read relevant ADRs before decisions in same domain.

## Code Style Conventions

Project-wide conventions enforced by reviewer:

| Convention | Rule | Deviation = Bug |
|------------|------|-----------------|
| **Type names** | Full descriptive names (`OperatingSystem`, `Architecture`), not abbreviations (`Os`, `Arch`) | Abbreviated type names |
| **Fleet forward-compat on fleet-read config** | No `deny_unknown_fields` anywhere in the `Config` tree — root sections and every nested table (`[registry]`, `[registries.<name>]`, `[mirrors."<host>"]`, `[patches]`, `[managed]`) ignore unknown fields. The `[managed]` tier makes one `config.toml` fleet-wide state, so a payload written for a newer ocx must degrade to its known parts, never fail the whole file on older binaries. Typo detection belongs to the published JSON schema, not the deserializer. A change tolerance cannot cover (a key's meaning or value shape) ships under a new tag, not a stricter parser. See the 2026-07-31 amendment in `adr_managed_config_tier.md`. **Carve-out (`adr_shell_env_overhaul.md` Decision 4):** a consent-bearing table is exempt, because dropping an unknown *narrowing* key **widens** trust rather than narrowing it — the one direction fleet forward-compat must not take. `ShellConsent` (reachable from `Config`) carries `deny_unknown_fields`, and its `namespaces` field deserializes through a strict `ConsentScopeSpec` wrapper that refuses unknown keys inside the table; `ShellConfig.hook`/`.completions` keep the tolerant behaviour — only the consent table is strict. Same reasoning `trust.rs:252-257` already states for `[[trust.policy]]`'s `Set` variant. | `deny_unknown_fields` on any struct reachable from `Config` — **except** a consent-bearing table under the carve-out above |
| **Module structure** | One concept per file, deep nested modules (`platform/operating_system.rs`) — no `mod.rs`, use named module files | Monolithic files, `mod.rs` files |
| **Internal enum exhaustiveness** | Omit `#[non_exhaustive]` on internal non-error enums so matches stay total across workspace. Binary = only consumer — no stable lib API ship. Error enums exempt: grow routinely and `#[non_exhaustive]` still aid safe expansion. | `#[non_exhaustive]` on closed internal enum |
| **Test-only seams** | Force test state via the canonical seam pattern, never a bespoke override: gate `#[cfg(any(test, feature = "__testing"))]`, name env vars `__OCX_*` (double-underscore), keep them out of user docs + `apply_ocx_config`. Full convention + reference impl in [`subsystem-tests.md`](./subsystem-tests.md) "Test-Only Seams". | New `cfg(test)`-only override or a non-`__OCX_` env var for a test seam |

## Where Features Land

| Feature type | Location | Notes |
|--------------|----------|-------|
| New CLI command | `crates/ocx_cli/src/command/` | One file per command, follow command pattern |
| Project-tier env-composition command | `crates/ocx_cli/src/command/toolchain_exec.rs` | Project-tier mirror of OCI-tier `exec.rs`; calls `load_project_with_lock` from `app/project_context.rs`, then `compose_tool_set` + `expand_all_keyword`, then `child_process::exec` |
| Toolchain env exporter (project + global) | `crates/ocx_cli/src/command/toolchain_env.rs` (root) | Root `ocx [--global] env [--shell[=NAME]]`; `--global` is a root flag (before subcommand); output format = context concern (root `--format`, default plain — no subcommand `--format`); `--shell` is the eval-safe channel; reuses `resolve_env` → `composer::compose` |
| OCI-tier package primitives group | `crates/ocx_cli/src/command/package.rs` | `ocx package {install,uninstall,select,deselect,exec,env,which,deps}` — moved from root; root forms removed (exit 64) |
| Shared shell emit helper | `crates/ocx_cli/src/app/conventions.rs` | `emit_lines(shell, &[Entry])` consumed by `ocx env`, `ocx package env`, `ocx direnv export` |
| Shared project-resolve prologue | `crates/ocx_cli/src/app/project_context.rs` | `load_project_with_lock` helper consumed by `pull.rs` and `toolchain_exec.rs`; returns `ProjectContext` (owned — no borrow on `Context`) |
| New task method | `crates/ocx_lib/src/package_manager/tasks/` | Add error variant to `error.rs` if needed |
| New output format | `crates/ocx_cli/src/api/data/` | Impl `Printable` trait |
| New storage path | `crates/ocx_lib/src/file_structure/` | Add to appropriate store |
| New index operation | `crates/ocx_lib/src/oci/index/` | Impl on `IndexImpl` trait |
| New metadata field | `crates/ocx_lib/src/package/metadata/` | Update types + schema + docs |
| New acceptance test | `test/tests/test_*.py` | Use fixtures, maintain test isolation |
| Project config mutation | `crates/ocx_lib/src/project/mutate.rs` | `add_binding` / `remove_binding` / `init_project` — atomic read-modify-write under in-place exclusive flock on `ocx.toml` via `acquire_project_lock` |
| Shell integration (env shims + RC blocks) | `crates/ocx_lib/src/setup/` | `ocx self setup` orchestrator; sub-modules: `bootstrap` (latest-published install), `rc_block` (fence state machine + diff-gate), `shims` (env.* file consts + atomic write), `profiles` (target detection), `error` |
| Per-prompt shell-env reconciliation — the **pure** pieces | `crates/ocx_lib/src/shell/reconcile.rs` (+ `reconcile/{ledger,plan,fingerprint}.rs`) | Typed, provenance-tagged three-way planner (desired D / current C / private `__OCX_ENV_STATE` ledger L), the carrier codec and the fingerprint fold. Each is pure and testable alone (`adr_shell_env_overhaul.md`) |
| Per-prompt shell-env reconciliation — the **sequencing** | `crates/ocx_lib/src/activation.rs` (`crate::activation`) | The order that binds the pure pieces into one answer — resolve the global tier, evaluate consent over the walk's project, compose only what consent authorized, diff, record. Driven by `ocx self activate --reconcile` (applies) and `ocx shell state` (reports), so both share one derivation. `Context`-free: every input is a plain field on `SessionInput`. **At the crate root, never under `shell/`** — `project::consent` reads `shell`, so sequencing under `shell/` closes a `use` cycle that does not compile across a crate boundary and blocks the `ocx_lib` split (addendum A-45) |
| Project activation consent | `crates/ocx_lib/src/project/consent.rs` | `evaluate()` — the stamp/paths/namespaces predicate gating whether a project's `ocx.toml` may compose at all; stamp lives at `state/projects/<key>/consent.json` |

## Cross-Cutting Modules

These `crates/ocx_lib/src/` modules have no dedicated subsystem rule — serve multiple subsystems:

| Module | Purpose | Used By |
|--------|---------|---------|
| `archive/` | Tar/zip extraction + bundling with pluggable backends | Mirror pipeline, package creation |
| `auth/` | `AuthType` enum with env var + docker cred fallback | OCI Client |
| `ci/` | CI flavor dispatch — `CiFlavor` enum + `Flavor` trait; GitHub Actions (`$GITHUB_ENV`/`$GITHUB_PATH` two-file sink) and GitLab CI/CD (`gitlab_flavor.rs`, JSON-lines `{"name","value"}`, no path channel, stdout/`--export-file`) flavors; `detect()` autodetect. Both flavors gate env-var keys via the shared `env::is_valid_env_key` (same validator as the shell emitters) and skip invalid keys; GitHub additionally rejects newline-bearing `$GITHUB_PATH` values (env-injection class, CWE-77 / CWE-426). Shared path-prepend semantics live in `ci::prepend_existing`. Wired into the CLI via the `--ci[=provider]` flag on `ocx env` / `ocx package env` (NOT the deleted `ocx ci` command). ADR `adr_ci_env_export_flag.md`, `handshake_toolchain_cli.md` §6. | `ocx env`, `ocx package env` (via `conventions::export_ci`) |
| `shell/` (`shell.rs`, `shell/reconcile.rs`, `shell/hook.rs`, `shell/coexistence.rs`) | `Shell` export helpers (`export_path`/`export_constant`/`export_list`/`remove_list_element`/`unset`/`emit_message`) + `conventions::emit_lines` — shell-specific export gen; env-var key validation delegated to the shared `env::is_valid_env_key` (also used by `ci/`); `reconcile.rs` is the typed three-way per-prompt planner (`adr_shell_env_overhaul.md`); `hook.rs` emits the per-shell prompt-hook registration + `ocx` wrapper; `coexistence.rs` detects a live direnv/mise session to yield to. **`shell/` is pure and must not import `crate::project`** — the per-prompt sequencing lives at `crate::activation` for exactly that reason, and the `shell_does_not_import_project` directory-walk test in `activation.rs` holds the boundary (addendum A-45). The root `ocx shell hook`/`init`/`env` **commands** are removed — the per-prompt hook itself is not; it rides `ocx self activate`'s hidden `--reconcile` arm | `ocx env`, `ocx package env`, `ocx direnv export`; consumed by `ocx self activate --reconcile` (applies) and `ocx shell state` (reports) |
| `utility/` | Extension traits + async + fs helpers — see [Utility Catalog](#utility-catalog) below | Everywhere (prelude for extension traits) |
| `compression/` | Compression level config | Archive, OCI push |
| `codesign/` | macOS ad-hoc code signing for Mach-O binaries | Package extraction |
| `shim.rs` | Arch-gated `include_bytes!` embed of committed Windows `.exe` shim blobs + `SHIM_SHA256` corruption canary; `SHIM_BYTES = &[]` on non-Windows. See `adr_windows_exe_shim.md`. | Launcher generation (`launcher::generate`) |

## Utility Catalog

**Rule: before writing small helper inside module, check this table.** Helper reinvented in one module = wasted effort + drift risk. If new helper broadly applicable, upstream to `utility/` (or crate-root module for linking/locking primitives) in same change and re-export via `prelude` when universally useful.

| Need | Use | Where |
|---|---|---|
| Append extra extension (`foo.json` → `foo.json.lock`) | `Path::with_added_extension(..)` | std (stable) |
| Read / write JSON with path-context errors | `SerdeExt::read_json` / `write_json` | prelude |
| Slugify for filesystem use | `StringExt::to_slug` / `to_relaxed_slug` | prelude |
| Sorted / dedup `Vec` fluently | `VecExt::sorted` / `unique_clone` | prelude |
| Ignore `Result` deliberately | `ResultExt::ignore` | prelude |
| Cross-process advisory file lock with RAII + in-place I/O via the lock-owning handle | `utility::fs::LockedFile` (+ `LockedJsonFile<T>` / `LockedTomlFile<T>` codec wrappers) | `crates/ocx_lib/src/utility/fs/locked_file.rs` |
| Cross-process advisory lock keyed by a guarded directory's file identity, homed in a central sharded `locks/` root (never a sidecar next to the guarded data) | `utility::fs::lock_scoped(locks_root, scope, guarded_dir, discriminator, timeout)` | `crates/ocx_lib/src/utility/fs/scoped_lock.rs` |
| Stateless content-addressed blob write/read (tempfile + atomic rename + Windows-cfg retry-with-backoff) | `BlobStore::write_blob` / `read_blob` | `crates/ocx_lib/src/file_structure/blob_store.rs` |
| Per-pull-operation singleflight dedup of concurrent same-digest blob writes | `package_manager::tasks::pull_local::PullCoordinator` (wraps `singleflight::Group<oci::Digest, ()>`) | `crates/ocx_lib/src/package_manager/tasks/pull_local.rs` |
| RAII "delete path on drop" guard | `utility::fs::DropFile` | `utility/fs/drop_file.rs` |
| Watch-based async singleflight (dedupe in-flight work by key) | `utility::singleflight` | `utility/singleflight.rs` |
| Parallel directory tree walk with pruning decisions | `utility::fs::{DirWalker, WalkDecision}` | `utility/fs/dir_walker.rs` |
| Lexical path normalize / containment check (no FS I/O) | `utility::fs::path::{lexical_normalize, escapes_root, validate_symlinks_in_dir}` | `utility/fs/path.rs` |
| Join an untrusted relative path under a containment root (lexical, host-independent Windows drive/UNC/verbatim rejection); bounded, non-escaping relative-path newtype for untrusted annotation input | `utility::fs::path::join_under_root` + `RelativePath` | `utility/fs/path.rs` |
| Move directory (same-filesystem rename, overwrite-safe) | `utility::fs::move_dir` | `utility/fs.rs` |
| Rename a path with Windows transient access/lock retry (`ERROR_ACCESS_DENIED`/`ERROR_SHARING_VIOLATION` backoff — the directory sibling of `persist_temp_file`; single rename off-Windows) | `utility::fs::rename_with_windows_retry` | `utility/fs.rs` (used by `move_dir`, `finalize_layer_dir`) |
| Atomically publish a written `NamedTempFile` to a target path (Windows transient-lock retry — `ERROR_SHARING_VIOLATION`/`ERROR_ACCESS_DENIED` backoff; single persist off-Windows; blocking — wrap in `spawn_blocking`) | `utility::fs::persist_temp_file` | `utility/fs.rs` (the one atomic-publish primitive; used by `BlobStore::write_blob`) |
| Atomically write bytes to a target as a private (`0o600` on Unix) file (temp-in-parent → `persist_temp_file`; parent must already exist; blocking — wrap in `spawn_blocking`) | `utility::fs::write_bytes_atomic` | `utility/fs.rs` (thin private-file wrapper over `persist_temp_file`; used by the referrers + trust-root caches, managed-config snapshot/pause, setup profile + shims) |
| Probe whether path exists, swallow I/O errors as `false` with debug log | `utility::fs::path_exists_lossy` | `utility/fs.rs` |
| Refuse a destination path whose ancestor chain contains any symlink (security guard) | `utility::fs::refuse_if_symlink_in_path` | `utility/fs/symlink_walk.rs` |
| Cross-platform same-filesystem check (Unix dev / Win32 GetVolumePathNameW) | `utility::fs::same_filesystem` | `utility/fs/same_filesystem.rs` |
| Verify a path is absent or an empty directory | `utility::fs::ensure_empty_or_absent` | `utility/fs/empty_or_absent.rs` |
| Read a whole file under a byte ceiling, refusing anything that is not a regular file (blocking — wrap in `spawn_blocking`); separates over-cap from not-a-regular-file from I/O so each caller maps them to its own error | `utility::fs::read_bounded(path, cap)` | `utility/fs/bounded_read.rs` |
| Hardlink file (dedup layer into package) | `hardlink::create` / `update` | `crates/ocx_lib/src/hardlink.rs` |
| Create / update / probe symlink (cross-platform, junction-aware) | `symlink::create` / `update` / `remove` / `is_link` | `crates/ocx_lib/src/symlink.rs` |
| Assemble layer's content tree into package (hardlinks + symlinks); layout-aware entrypoint applies per-layer strip + output prefix before the overlap merge | `utility::fs::assemble_from_layer(s)` / `assemble_from_layers_with_layouts` + `LayerPlacement` | `utility/fs/assemble.rs` |
| Boolean-like env string (`true/1/yes/on`) | `utility::boolean_string::BooleanString` | `utility/boolean_string.rs` |
| Forward child `ExitStatus` to process `ExitCode` (Unix passthrough, Windows saturate) | `utility::child_process::propagate_exit_code` | `utility/child_process.rs` |
| Move-to-front dedup of a `PATH`-style value (drop empties + existing occurrence, prepend; `OsStr`-native via `std::env::split_paths`) | `utility::path::move_to_front` | `utility/path.rs` |
| Remove one segment from a `PATH`-style value (drop empties + the matching segment; the emit-parity inverse of `move_to_front`, same quote-strip/case-fold precondition as the emitted shell arms) | `utility::path::remove_segment` | `utility/path.rs` |
| File error with path context | `error::file_error(path, io_err)` | `crates/ocx_lib/src/error.rs` |
| Seed a hand-rolled `reqwest::ClientBuilder` with the bundled Mozilla CA roots | `utility::tls::seed_embedded_roots` | `utility/tls.rs` |

## Locking Policy

| Data shape | Mechanism |
|---|---|
| Stable inode, edited in place | `LockedFile` — lock the data file itself |
| Atomic-rename-replaced data (inode rotates) | `lock_scoped` into `$OCX_HOME/locks` — never a persistent sidecar; sidecars outside `$OCX_HOME/locks` are a review Block-tier finding |

**Check std first, then this catalog, then invent.** Most "small helper" needs already covered by `std::path`, `tokio::fs`, or existing entry above. If add new entry here, keep row to one line and put impl details in target module's doc comment, not this table.
