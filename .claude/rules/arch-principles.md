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
| **Composite root** | `FileStructure` wraps `BlobStore` + `LayerStore` + `PackageStore` + `TagStore` + `SymlinkStore` + `TempStore` | Three-tier CAS: raw blobs → extracted layers → assembled packages; tags + symlinks = mutable namespace on top |
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
| **Index** | Local JSON snapshot of registry metadata (tags, manifests) for offline/reproducibility |
| **Candidate** | Symlink at `symlinks/{registry}/{repo}/candidates/{tag}` — pinned at install time |
| **Current** | Floating symlink at `symlinks/{registry}/{repo}/current` — set by `ocx select` |
| **Project ledger** | Flat symlink store at `$OCX_HOME/projects/` — one symlink per registered project, name = 16-hex SHA-256 of canonical project dir, target = project dir. GC roots for multi-project clean. Self-link for the global toolchain is prohibited (global file's project dir is `$OCX_HOME`); instead `clean::collect_project_roots` adds an **implicit `$OCX_HOME/ocx.lock` root** so global lock-pinned packages are GC roots without a ledger entry (ADR `adr_global_toolchain_tier.md` D5 amended 2026-05-19). ADR: `adr_project_gc_symlink_ledger.md`. |
| **Global toolchain** | `$OCX_HOME/ocx.toml` + `$OCX_HOME/ocx.lock`, reachable only via explicit root `--global` flag (before the subcommand) or `OCX_GLOBAL` env var. `--global` is defined once on `ContextOptions` (peer of `--project`); per-command `--global` flags do not exist. Strict isolation: never composes into project resolution; `run`/`exec` are always hermetic. Env resolution = **lock-pinned digests, offline** (`resolve_global_pinned_env`) — the project tier with a different load site; the `current` symlink is a separate install/uninstall/select-only abstraction and is NOT consulted, so `ocx --global update` takes effect with no select step (ADR D5 amended 2026-05-19). Shell-exposed via `$OCX_HOME/env.sh` sourced from the login profile (managed activation block written by `ocx self setup`; runs `eval "$(ocx --global env --shell=sh)"`). No static `$OCX_HOME/init.<shell>`, no per-prompt hook, no PATH strip — isolation by PATH precedence. ADRs: `adr_global_toolchain_tier.md`, `handshake_toolchain_cli.md`, `adr_self_setup.md`. |
| **Digest** | SHA-256 content hash — immutable identity of package version |
| **Tag** | Mutable alias to digest (e.g., `3.28`, `latest`) |
| **Cascade** | Publisher convention: push `3.28.1` and auto-update `3.28`, `3`, `latest` tags |
| **Platform** | OS/arch pair (e.g., `linux/amd64`) for multi-platform manifest resolution; optionally refined by `variant` and ABI features (libc family on Linux via `os.features`, e.g. `libc.glibc`). Canonical grammar: `os/arch[/variant][+feature[,feature...]]` \| `any` — single, injective, round-tripping string shared by `--platform`, `ocx.lock` keys, and dependency pin-map keys. Resolution is the directed relation `is_compatible(required, offered)` + lexicographic `select_best` scoring — not strict equality; one shared helper across fresh-resolve, lock-read, and authoring pinning. One platform per authoring invocation (no bundle-level target set); `patch sync`'s bare-invocation concrete-ship-matrix fan-out is the one sanctioned exception. Host libc detected per-host by `HostCapabilities::detect` via discovery-then-identify (a system binary's `PT_INTERP` + arch-filtered loader scan + allowlist fallback, then `--version` banner classification). ADRs: `adr_platform_model_unification.md` (relation, grammar, lock V3, single-platform authoring), `adr_platform_libc_os_features.md` (libc namespace + host detection). |
| **Slug** | Filesystem-safe encoding: `to_relaxed_slug()` preserves `[a-zA-Z0-9._-]`, replaces rest with `_` |
| **Identifier** | Parsed OCI reference: `registry/repo:tag@digest` with default registry fallback |
| **Manifest** | OCI image manifest or image index (multi-platform) |
| **Refs** | Reference sub-dirs inside `packages/.../refs/`: `symlinks/` (GC roots from install symlinks), `deps/` (forward-refs to other packages), `layers/` (forward-refs to layers), `blobs/` (forward-refs to blobs) |
| **DirtyRcBlock (exit 82)** | `ExitCode::DirtyRcBlock = 82` — `ocx self setup` exits 82 when a managed activation block in a shell profile carries user edits inside the fence and `--force` was not passed. Scripts can `case $? in 82)` to detect and re-run with `--force`. Distinct from `ConfigError` (78): the RC content is valid but intentionally modified by the user. |

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
| `adr_ocx_mirror.md` | Standalone binary mirroring tool design |
| `adr_release_install_strategy.md` | Release + install phased strategy |
| `adr_sbom_strategy.md` | SBOM gen approach |
| `adr_version_build_separator.md` | Underscore as build separator in version tags |
| `adr_three_tier_cas_storage.md` | Three-tier content-addressed storage (blobs + layers + packages) |
| `adr_index_routing_semantics.md` | `IndexOperation::{Query, Resolve}` enum; pinned-id pulls skip tag commit |
| `adr_cli_high_low_layering.md` | Formalize high-level (project-tier) vs OCI-tier CLI split; add `ocx run`; reserve `all` keyword |
| `adr_windows_exe_shim.md` | Native Windows `.exe` shim + `.shim` sidecar replaces the `.cmd` launcher (no `.cmd` emitted; PATHEXT inject/warn machinery removed); fully eliminates BatBadBut `%*`; committed-blob embed (A1 + B1 + C2 + D1) |
| `adr_project_gc_symlink_ledger.md` | Flat symlink store `$OCX_HOME/projects/` as project GC ledger (supersedes `adr_clean_project_backlinks.md`) |
| `adr_global_toolchain_tier.md` | Explicit `--global` toolchain tier, strict isolation, no implicit home fallback (supersedes Amendment C of `adr_project_toolchain_config.md`) |
| `handshake_toolchain_cli.md` | **AUTHORITY for current CLI model** — `ocx package` group (OCI tier), root `ocx [--global] env [--shell]` (`--global` is a root flag before the subcommand), `ocx shell` reduced to `{completion}`, root `install/uninstall/select/exec/deselect/which/deps/ci/shell hook/init/env` removed (exit 64), activation via `$OCX_HOME/env.sh` block-marker, no PATH strip. Decisions 3/4/6/7 of `adr_global_toolchain_tier.md` superseded here. Per-command `--global` and `with_command_global` seam deleted 2026-05-17 (root-only collapse). |
| `adr_progress_architecture.md` | Span-free progress: `cli::progress::ProgressManager` owns `indicatif::MultiProgress`; RAII guards (`Spinner`/`BytesBar`) instead of `tracing-indicatif` span-attached bars. Kills the concurrent sharded-registry clone-after-close panic by construction. `tracing-indicatif` dropped; fmt logs route through `ProgressManager::writer()` (suspend-coordinated). |
| `adr_ci_env_export_flag.md` | Realize handshake §6 CI export as `--ci[=provider]` flag on `ocx env`/`ocx package env` (not a command); GitHub autodetected two-file sink (rejects `--export-file`); GitLab JSON-lines, stdout default / `--export-file`; `--ci` ⟂ `--shell`; GitLab flavor added. |
| `adr_self_setup.md` | `ocx self setup` — bare-binary install complement to the install script: bootstrap + env shim write + managed RC-block (`# >>> ocx v1 <hash> >>>`) in user shell profiles; `ExitCode::DirtyRcBlock` (82) for user-edited blocks; `ocx self update` refreshes shims post-swap (4C). |
| `adr_cli_plugin_pattern.md` | Git-style `ocx <name>` → `ocx-<name>` PATH dispatch; plugins inherit parent env (trust boundary); built-ins always shadow |
| `adr_managed_config_tier.md` | Corporate managed configuration tier (`[managed]`) — scope: One-Way-Door Medium. Seed pointer in `config.toml` resolves to an operator-published `config.toml` package (v2: config-as-package, superseding the v1 custom-artifact wire shape), identity-gated snapshot merged as a synthetic 5th precedence tier, `ocx config push`/`ocx config update` reuse ordinary package machinery (versioning, cascade, rollback). |

ADRs live in `.claude/artifacts/adr_*.md`. Read relevant ADRs before decisions in same domain.

## Code Style Conventions

Project-wide conventions enforced by reviewer:

| Convention | Rule | Deviation = Bug |
|------------|------|-----------------|
| **Type names** | Full descriptive names (`OperatingSystem`, `Architecture`), not abbreviations (`Os`, `Arch`) | Abbreviated type names |
| **Fleet forward-compat on fleet-read config** | Config surfaces read by many binary versions at once (`[managed]` seed, managed payload, root `Config`) tolerate unknown fields/sections — no `deny_unknown_fields`. A config written for a newer ocx must not brick older fleet binaries reading the same file. | `deny_unknown_fields` on a fleet-distributed config struct |
| **Module structure** | One concept per file, deep nested modules (`platform/operating_system.rs`) — no `mod.rs`, use named module files | Monolithic files, `mod.rs` files |
| **Internal enum exhaustiveness** | Omit `#[non_exhaustive]` on internal non-error enums so matches stay total across workspace. Binary = only consumer — no stable lib API ship. Error enums exempt: grow routinely and `#[non_exhaustive]` still aid safe expansion. | `#[non_exhaustive]` on closed internal enum |
| **Test-only seams** | Force test state via the canonical seam pattern, never a bespoke override: gate `#[cfg(any(test, feature = "__testing"))]`, name env vars `__OCX_*` (double-underscore), keep them out of user docs + `apply_ocx_config`. Full convention + reference impl in [`subsystem-tests.md`](./subsystem-tests.md) "Test-Only Seams". | New `cfg(test)`-only override or a non-`__OCX_` env var for a test seam |

## Where Features Land

| Feature type | Location | Notes |
|--------------|----------|-------|
| New CLI command | `crates/ocx_cli/src/command/` | One file per command, follow command pattern |
| Project-tier env-composition command | `crates/ocx_cli/src/command/run.rs` | Project-tier mirror of OCI-tier `exec.rs`; calls `load_project_with_lock` from `app/project_context.rs`, then `compose_tool_set` + `expand_all_keyword`, then `child_process::exec` |
| Toolchain env exporter (project + global) | `crates/ocx_cli/src/command/toolchain_env.rs` (root) | Root `ocx [--global] env [--shell[=NAME]]`; `--global` is a root flag (before subcommand); output format = context concern (root `--format`, default plain — no subcommand `--format`); `--shell` is the eval-safe channel; reuses `resolve_env` → `composer::compose` |
| OCI-tier package primitives group | `crates/ocx_cli/src/command/package.rs` | `ocx package {install,uninstall,select,deselect,exec,env,which,deps}` — moved from root; root forms removed (exit 64) |
| Shared shell emit helper | `crates/ocx_cli/src/app/conventions.rs` | `emit_lines(shell, &[Entry])` consumed by `ocx env`, `ocx package env`, `ocx direnv export` |
| Shared project-resolve prologue | `crates/ocx_cli/src/app/project_context.rs` | `load_project_with_lock` helper consumed by `pull.rs` and `run.rs`; returns `ProjectContext` (owned — no borrow on `Context`) |
| New task method | `crates/ocx_lib/src/package_manager/tasks/` | Add error variant to `error.rs` if needed |
| New output format | `crates/ocx_cli/src/api/data/` | Impl `Printable` trait |
| New storage path | `crates/ocx_lib/src/file_structure/` | Add to appropriate store |
| New index operation | `crates/ocx_lib/src/oci/index/` | Impl on `IndexImpl` trait |
| New metadata field | `crates/ocx_lib/src/package/metadata/` | Update types + schema + docs |
| New acceptance test | `test/tests/test_*.py` | Use fixtures, maintain test isolation |
| Project config mutation | `crates/ocx_lib/src/project/mutate.rs` | `add_binding` / `remove_binding` / `init_project` — atomic read-modify-write under in-place exclusive flock on `ocx.toml` via `acquire_project_lock` |
| Shell integration (env shims + RC blocks) | `crates/ocx_lib/src/setup/` | `ocx self setup` orchestrator; sub-modules: `bootstrap` (latest-published install), `rc_block` (fence state machine + diff-gate), `shims` (env.* file consts + atomic write), `profiles` (target detection), `error` |

## Cross-Cutting Modules

These `crates/ocx_lib/src/` modules have no dedicated subsystem rule — serve multiple subsystems:

| Module | Purpose | Used By |
|--------|---------|---------|
| `archive/` | Tar/zip extraction + bundling with pluggable backends | Mirror pipeline, package creation |
| `auth/` | `AuthType` enum with env var + docker cred fallback | OCI Client |
| `ci/` | CI flavor dispatch — `CiFlavor` enum + `Flavor` trait; GitHub Actions (`$GITHUB_ENV`/`$GITHUB_PATH` two-file sink) and GitLab CI/CD (`gitlab_flavor.rs`, JSON-lines `{"name","value"}`, no path channel, stdout/`--export-file`) flavors; `detect()` autodetect. Both flavors gate env-var keys via the shared `env::is_valid_env_key` (same validator as the shell emitters) and skip invalid keys; GitHub additionally rejects newline-bearing `$GITHUB_PATH` values (env-injection class, CWE-77 / CWE-426). Shared path-prepend semantics live in `ci::prepend_existing`. Wired into the CLI via the `--ci[=provider]` flag on `ocx env` / `ocx package env` (NOT the deleted `ocx ci` command). ADR `adr_ci_env_export_flag.md`, `handshake_toolchain_cli.md` §6. | `ocx env`, `ocx package env` (via `conventions::export_ci`) |
| `shell/` | `Shell` export helpers (`export_path`/`export_constant`/`unset`) + `conventions::emit_lines` — shell-specific export gen; env-var key validation delegated to the shared `env::is_valid_env_key` (also used by `ci/`) | `ocx env`, `ocx package env`, `ocx direnv export`; `shell hook`/`init`/`env` commands removed |
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
| Stateless content-addressed blob write/read (tempfile + atomic rename + Windows-cfg retry-with-backoff) | `BlobStore::write_blob` / `read_blob` | `crates/ocx_lib/src/file_structure/blob_store.rs` |
| Per-pull-operation singleflight dedup of concurrent same-digest blob writes | `package_manager::tasks::pull_local::PullCoordinator` (wraps `singleflight::Group<oci::Digest, ()>`) | `crates/ocx_lib/src/package_manager/tasks/pull_local.rs` |
| RAII "delete path on drop" guard | `utility::fs::DropFile` | `utility/fs/drop_file.rs` |
| Watch-based async singleflight (dedupe in-flight work by key) | `utility::singleflight` | `utility/singleflight.rs` |
| Parallel directory tree walk with pruning decisions | `utility::fs::{DirWalker, WalkDecision}` | `utility/fs/dir_walker.rs` |
| Lexical path normalize / containment check (no FS I/O) | `utility::fs::path::{lexical_normalize, escapes_root, validate_symlinks_in_dir}` | `utility/fs/path.rs` |
| Join an untrusted relative path under a containment root (lexical, host-independent Windows drive/UNC/verbatim rejection); bounded, non-escaping relative-path newtype for untrusted annotation input | `utility::fs::path::join_under_root` + `RelativePath` | `utility/fs/path.rs` |
| Move directory (same-filesystem rename, overwrite-safe) | `utility::fs::move_dir` | `utility/fs.rs` |
| Atomically publish a written `NamedTempFile` to a target path (Windows transient-lock retry — `ERROR_SHARING_VIOLATION`/`ERROR_ACCESS_DENIED` backoff; single persist off-Windows; blocking — wrap in `spawn_blocking`) | `utility::fs::persist_temp_file` | `utility/fs.rs` (the one atomic-publish primitive; used by `BlobStore::write_blob`) |
| Probe whether path exists, swallow I/O errors as `false` with debug log | `utility::fs::path_exists_lossy` | `utility/fs.rs` |
| Refuse a destination path whose ancestor chain contains any symlink (security guard) | `utility::fs::refuse_if_symlink_in_path` | `utility/fs/symlink_walk.rs` |
| Cross-platform same-filesystem check (Unix dev / Win32 GetVolumePathNameW) | `utility::fs::same_filesystem` | `utility/fs/same_filesystem.rs` |
| Verify a path is absent or an empty directory | `utility::fs::ensure_empty_or_absent` | `utility/fs/empty_or_absent.rs` |
| Hardlink file (dedup layer into package) | `hardlink::create` / `update` | `crates/ocx_lib/src/hardlink.rs` |
| Create / update / probe symlink (cross-platform, junction-aware) | `symlink::create` / `update` / `remove` / `is_link` | `crates/ocx_lib/src/symlink.rs` |
| Assemble layer's content tree into package (hardlinks + symlinks); layout-aware entrypoint applies per-layer strip + output prefix before the overlap merge | `utility::fs::assemble_from_layer(s)` / `assemble_from_layers_with_layouts` + `LayerPlacement` | `utility/fs/assemble.rs` |
| Boolean-like env string (`true/1/yes/on`) | `utility::boolean_string::BooleanString` | `utility/boolean_string.rs` |
| Forward child `ExitStatus` to process `ExitCode` (Unix passthrough, Windows saturate) | `utility::child_process::propagate_exit_code` | `utility/child_process.rs` |
| Move-to-front dedup of a `PATH`-style value (drop empties + existing occurrence, prepend; `OsStr`-native via `std::env::split_paths`) | `utility::path::move_to_front` | `utility/path.rs` |
| File error with path context | `error::file_error(path, io_err)` | `crates/ocx_lib/src/error.rs` |

**Check std first, then this catalog, then invent.** Most "small helper" needs already covered by `std::path`, `tokio::fs`, or existing entry above. If add new entry here, keep row to one line and put impl details in target module's doc comment, not this table.