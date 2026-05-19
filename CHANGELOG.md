# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security

- Windows entrypoint launchers are now a native `.exe` shim only — the `.cmd` launcher is no longer generated. The shim invokes `ocx launcher exec` through `CreateProcessW` directly, never routing through `cmd.exe`, which fully closes the `%*` caller-argv re-parse surface (`BatBadBut` / [CVE-2024-24576][ghsa-q455]). Because no `.cmd` is emitted, there is no orphan path that could re-open the injection class — the vector is eliminated, not merely shadowed. *(package)*

### Added

- Windows entrypoint launchers are two files per entry: `<name>.exe` (native shim) and `<name>.shim` (one-line `pkg_root` sidecar). No `.cmd` is generated. The `.exe` shim reads `<name>.shim` at invocation time to locate the package root, then spawns `ocx launcher exec` via `CreateProcessW` without routing through `cmd.exe`. `.EXE` is unconditionally in the default Windows `PATHEXT`, so bare-name resolution finds the shim with no `PATHEXT` configuration needed. `OCX_BINARY_PIN` is honored with Windows `IF DEFINED` semantics (defined-even-empty → use it; only completely unset → `PATH` `ocx`). Shim blobs are unsigned in this release (~138 KiB x86_64 / ~128 KiB aarch64; [Authenticode][authenticode-ref] signing via [SignPath Foundation][signpath-ref] is the documented follow-on step). Both architecture blobs are committed in-tree and selected via `#[cfg(target_arch)]`. *(package)*
- The Windows `PATHEXT` inject/warn machinery is removed: `ocx env` no longer emits a synthetic `PATHEXT` entry, `ocx exec` / `ocx run` / `launcher exec` / `package test` no longer manipulate the child `PATHEXT`, and `install` / `select` / `shell env` / `ci export` no longer print a `PATHEXT` warning. It is dead under the `.exe`-only launcher (`.EXE` is always in the default Windows `PATHEXT`). *(cli)*
- `--global` toolchain tier on `add`, `remove`, `lock`, `upgrade`, `pull`, and `run`. `--global` re-targets the project file to `$OCX_HOME/ocx.toml` and is mutually exclusive with `--project` (exit 64). Strict isolation: `run` is hermetic and never reads the global file without `--global`. *(project)*
- `OCX_GLOBAL` env var — resolution-affecting equivalent of `--global`; forwarded to child ocx processes via `apply_ocx_config`. *(cli)*
- `ocx env [--global] [--shell[=NAME]]` — new root toolchain-tier env exporter. Default output is JSON (`{"entries": [...]}`). `--shell=<NAME>` is the only eval-safe output channel. `--global` targets `$OCX_HOME/ocx.toml`. *(cli)*
- `ocx package` command group with `install`, `uninstall`, `select`, `deselect`, `exec`, `env`, `which`, `deps` — OCI-tier package primitives moved under this group. *(cli)*
- Global toolchain activation via `$OCX_HOME/env.sh`, written by the in-repo installer. The file runs `eval "$(ocx env --global --shell=sh)"` and is sourced from the login profile via a block-marker idempotent line. *(shell)*
- `ocx package push --build-timestamp [datetime|date|none]` appends a UTC build-metadata segment to the published tag. `datetime` (default when the flag is passed bare) yields `_YYYYMMDDhhmmss`; `date` yields `_YYYYMMDD`; `none` is a no-op. Designed for continuous-deploy pipelines that publish rolling pre-release builds, e.g. `0.3.0-dev_20260514120000`. The identifier tag must be in `X.Y.Z` form (with optional variant or pre-release) and must not already carry build metadata. *(cli)*
- `Error::BuildMeta` variant on the package error chain, wrapping `BuildMetaError::{NoPatch, AlreadyPresent}` from `crates/ocx_lib/src/package/version/build_meta.rs`. Classified as `ExitCode::DataError` (65). *(package)*
- `ocx package test` for local pre-push validation: materializes a package without a registry round-trip and runs a command in its composed env. Temp directory is auto-cleaned on exit; `--keep` opts in to preservation for inspection. *(cli)*
- `ocx clean --force` bypasses the project registry and collects packages held only by other projects' `ocx.lock` files. Live install symlinks are still honoured. *(cli)*
- Multi-project GC retention: `ocx clean` now retains packages pinned by any registered project's `ocx.lock` on the machine, not just the active project. Projects register automatically in the `$OCX_HOME/projects/` symlink ledger when `ocx lock` runs; a departed project's link is pruned silently on the next `ocx clean`. *(project)*
- `ocx clean --dry-run` output gains a `Held By` column listing the `ocx.lock` paths that pin each retained package; also surfaced in JSON output as `held_by` array on each `CleanEntry`. *(cli)*
- Multi-layer package push and pull. `ocx package push` now accepts multiple layer arguments, each either a file path or a `sha256:<hex>.tar.gz` digest reference. *(package)*
- Layered configuration from `/etc/ocx/config.toml`, `~/.config/ocx/config.toml`, `$OCX_HOME/config.toml` with `--config` / `OCX_CONFIG` overrides and `OCX_NO_CONFIG` kill-switch. *(config)*
- Typed `ExitCode` taxonomy aligned with BSD sysexits (64/65/69/74/75/77/78/79/80/81). Scripts can now `case $?` reliably. *(cli)*
- `entrypoints` field in package metadata. Publishers declare named launchers (e.g. `cmake`, `ctest`) keyed by the invocable name; `ocx install` generates per-platform launchers under `<package>/entrypoints/<name>` (POSIX) and `<package>/entrypoints/<name>.exe` + `<name>.shim` (Windows native shim) that delegate to `ocx launcher exec` against a baked package root, resolving the binary against the composed `PATH` from the package's `env` block at exec time. *(package)*
- Optional `command` field on an `entrypoint` value object. Lets the invocable launcher name differ from the binary it dispatches (e.g. expose `fmt` while running `cargo-fmt`); omitted means the invocable name is dispatched directly. `command` follows the same slug constraint as the entrypoint name (`[a-z0-9][a-z0-9_-]*`, at most 64 bytes). `ocx launcher exec` resolves the dispatch target via `Entrypoints::dispatch_command`. *(package)*
- `ocx package inspect ID` — read-only, ref-shape-adaptive package inspection. An image-index reference lists the platform candidates (no metadata, no platform select); a single-manifest reference (flat tag or `@digest`) emits the declared metadata; `--resolve` platform-selects and emits metadata plus the OCI resolution chain. Accepts `@digest`. Plain output is a tree; an entrypoint whose dispatch `command` diverges from its name is annotated `→ <command>`. Exit codes: 79 not found, 81 offline blob miss, 65 malformed metadata. *(cli)*
- `${deps.NAME.installPath}` template interpolation in env-var values. Env modifier values can now reference dependency install paths. Unrecognized `${...}` tokens are rejected at publish time. *(package)*
- `name` field on `Dependency`. Lets a package import a dependency under a different name, disambiguating basename collisions when two deps would otherwise resolve to the same launcher slot. *(package)*
- `ocx package exec` accepts a `file://<absolute-package-root>` URI. Generated entrypoint launchers bake this URI into a single `ocx package exec 'file://<root>' -- "$(basename $0)"` form so they survive a re-select to a different version (the symlink target moves; the URI does not). *(cli)*
- Flat install layout: `current` and `candidates/{tag}` target the package root, so generated launchers are reachable through `current/entrypoints/`. The previous `entrypoints-current` symlink is gone; selection state lives on a single per-repo `current` anchor. *(file-structure)*
- `ValidMetadata` typestate on package metadata. Publish-time validation rejects bundles with malformed entry points, undeclared dep references, or duplicate launcher names; downstream code consumes only validated metadata. *(package)*
- `EntrypointNameCollision` structured error at `install --select` / `select` and at consumption time (`ocx env`, `ocx exec`) when two packages in the same visible closure declare the same entrypoint `name`. Surfaces with exit code 65 (`DataError`); recovery: deselect one package before selecting the other. *(package-manager)*
- Synthetic `PATH ⊳ <pkg-root>/entrypoints` entry emitted per visible package with a non-empty `entrypoints` array, so generated launchers are reachable through `ocx env` / `ocx exec` / `ocx shell env` without manual PATH wiring. *(env)*
- Project-tier toolchain: declare a repository's tools in `ocx.toml` and lock them to digests in `ocx.lock`. The lock carries a `declaration_hash` over the canonicalized `ocx.toml` ([RFC 8785][rfc-8785]) so downstream commands refuse to run with stale digests. *(project)*
- `ocx lock` resolves every advisory tag in `ocx.toml` to an immutable digest and writes `ocx.lock`. Repeatable `--group` flag scopes resolution to one or more named groups (`ci`, `release`, …). *(cli)*
- `ocx upgrade [PKG]` re-resolves advisory tags in the lock — opt-in upgrade flow distinct from `ocx lock`'s "freeze whatever the registry surfaces today" semantics. *(cli)*
- `ocx pull` (project tier, distinct from `ocx package pull`) pre-warms the package store from `ocx.lock` without creating install symlinks — ideal for CI matrix builds and direnv-driven workstations. *(cli)*
- `ocx direnv init` writes a `.envrc` file wiring `ocx direnv export` into [direnv](https://direnv.net/), watching `ocx.toml` and `ocx.lock` for re-evaluation. *(cli)*
- JSON Schema for `ocx.toml` (`https://ocx.sh/schemas/project/v1.json`) and `ocx.lock` (`https://ocx.sh/schemas/project-lock/v1.json`); both wired through [taplo][taplo] for editor auto-completion. The `project-lock` schema carries a top-level `$comment` flagging the format as machine-generated. *(schema)*
- `ocx init` writes a `#:schema https://ocx.sh/schemas/project/v1.json` directive on the first line of every generated `ocx.toml`, so [taplo][taplo]-aware editors (VS Code, Zed, Neovim) pick the schema up automatically with no extra wiring. *(cli)*
- `ocx lock --check` verifies `ocx.lock` is current relative to `ocx.toml` without re-resolving or writing. CI primitive: exit 0 on match, 65 on drift, 78 if the lock is absent. *(cli)*
- `ocx upgrade --check` verifies the candidate lock would match the predecessor and exits without writing. Mirrors `lock --check` for the partial-resolve flow: exit 0 on match, 65 when an advisory tag has moved upstream for any selected or preserved entry, 78 when the predecessor lock is absent. *(cli)*

OCX is locked-by-default for read paths: `ocx pull` and `ocx run` already exit 65 on lock drift, and `--offline` covers the "no implicit network" CI case. Per-command `--locked` / `--frozen` flags are deliberately not added — see [Lock-first by default](https://ocx.sh/docs/user-guide.html#locked-frozen-equivalents) for the mapping from `uv` / Cargo / pnpm idioms.

### Changed

- `ocx add` now allows the same binding name to coexist in the default group and in named `[group.*]` tables. `ocx remove` gains `--group <NAME>` for unambiguous removal; without `--group` it errors when the name is ambiguous across groups. *(project)*
- **Breaking:** `--remote` / `OCX_REMOTE` semantics narrowed — tag and catalog lookups now bypass the local tag store and query the registry directly, but digest-addressed blob reads still use the local cache with write-through to `$OCX_HOME/blobs/`. Previously, `--remote` routed all operations to the registry. Only `$OCX_HOME/tags/` is no longer updated under `--remote`. *(oci)*
- **Breaking:** `ocx index update` no longer pre-fetches manifest or layer blobs. It writes only tag→digest pointers to `$OCX_HOME/tags/`. Run `ocx package install <pkg>` online first to populate the blob cache before using `--offline`. *(index)*
- **Breaking:** `ocx env --format json` now emits `{"entries": [{"key": ..., "value": ..., "type": ...}, ...]}` (canonical envelope) instead of a bare top-level array. Update consumers that deserialize JSON output to read the `entries` field. The shape is shared across env-related JSON outputs for forward compatibility. *(cli)*
- Error messages normalized across 11 modules per Rust API Guidelines `C-GOOD-ERR` (lowercase, no trailing punctuation). *(error)*

### Breaking

- **Breaking:** Root `ocx install`, `ocx select`, `ocx deselect`, `ocx uninstall`, `ocx exec`, `ocx which`, and `ocx deps` commands removed — they exit 64 (`UsageError`). Use the `ocx package` group instead: `ocx package install`, `ocx package select`, `ocx package deselect`, `ocx package uninstall`, `ocx package exec`, `ocx package which`, `ocx package deps`. *(cli)*
- **Breaking:** `ocx shell hook`, `ocx shell init`, and `ocx shell env` removed — they exit 64. The per-prompt shell hook model is replaced by `$OCX_HOME/env.sh`, written by the in-repo installer with a block-marker idempotent `.`-source line in the login profile. The file runs `eval "$(ocx env --global --shell=sh)"`. The `_OCX_APPLIED` fingerprint variable is gone. *(shell)*
- **Breaking:** `ocx ci export` removed — exits 64. CI workflows: use `ocx pull` (project-tier) or `ocx package pull` (OCI-tier) to pre-warm the store, then `ocx run` or `ocx package env` to compose the environment. *(cli)*
- **Breaking:** `ocx install --global` removed. To add a tool to the global toolchain: `ocx add --global <pkg>`. *(cli)*
- **Breaking:** New root `ocx env [--global] [--shell[=NAME]]` command for toolchain-tier env export. Default output is JSON (`{"entries": [...]}`). Use `--shell=<NAME>` for eval-safe shell export lines. `--shell` requires the equals-form (`--shell=bash`, not `--shell bash`). *(cli)*
- **Breaking:** `$OCX_HOME/projects.json` and `$OCX_HOME/.projects.lock` (the prior JSON project ledger) are obsolete and safe to delete. The project GC ledger is now a flat symlink store at `$OCX_HOME/projects/` — one symlink per registered project, self-pruning on `ocx clean`. The multi-project GC contract (packages pinned by project A are retained when `ocx clean` runs from project B) is unchanged. *(project)*
- **Breaking:** The implicit `$OCX_HOME/ocx.toml` home-tier fallback is removed. Commands that previously discovered it automatically must now pass `--global` (or set `OCX_GLOBAL=1`). *(project)*
- **Breaking:** `ocx package push` and `ocx package test` now require the identifier as a `-i`/`--identifier` flag instead of a positional argument. Update scripts: `ocx package push <id> <bundle>` → `ocx package push -i <id> <bundle>`. Same change for `package test`. *(cli)*
- **Breaking:** Package metadata field renamed from `entry_points` to `entrypoints`. Publishers must update `metadata.json` files; bundles using the old field name fail validation at `package create`. *(package)*
- **Breaking:** Package metadata `entrypoints` is now a JSON object keyed by command name, not an array of `{name}` structs. Publishers must rewrite `"entrypoints": [{"name": "cmake"}, {"name": "ctest"}]` as `"entrypoints": {"cmake": {}, "ctest": {}}`. The map shape gives intra-package uniqueness via JSON object key semantics; per-entry future fields land naturally in each value object. Duplicate keys are rejected at deserialization (overrides the `serde_json` last-wins default) with a descriptive error citing the offending name. *(package)*
- **Breaking:** Dependency JSON field key renamed from `alias` to `name`. Existing bundles must be re-published with `"name"` in place of `"alias"`. The `${deps.NAME.installPath}` template token is unchanged — `NAME` was always the placeholder keyword, never the literal field name. *(package)*
- **Breaking:** Project lock is now an in-place exclusive flock on `ocx.toml`; `.ocx-lock` and `ocx.lock.lock` are no longer created. Remove `.ocx-lock` from `.gitignore`; run `git rm .ocx-lock` if previously committed. *(project)*
- **Breaking:** `ocx-mirror` exit codes changed from `0/2/3/4` to `0/65/79/1/69` to align with the sysexits-based taxonomy. Wrapper scripts matching historic codes must be updated. *(mirror)*

### Fixed

- `ocx --offline install <pkg>` after a bare `ocx index update <pkg>` now fails with a clear `OfflineManifestMissing` error naming the missing digest instead of a silent failure. Recovery: run `ocx install <pkg>` online to populate the blob cache. *(oci)*

### Security

- Pulled layer blobs are verified against the claimed digest before extraction; mismatched blobs are deleted and fail the pull with a clear error. *(oci)*
- Hardened the Windows `.cmd` launcher template by adding `DisableDelayedExpansion` to the `SETLOCAL` directive to mitigate registry-level `!`-expansion vectors when forwarding untrusted argv (`BatBadBut` interim mitigation). The `%*` parameter remains unescaped at the `.cmd` level — see `.claude/artifacts/adr_windows_cmd_argv_injection.md` for the threat model and tracked compiled-shim follow-up. *(package)*

### Breaking (v2 schema and exec-mode defaults)

- **Breaking:** `Var.visibility` field added to package metadata `env` entries. Default is `"private"` — env entries declared without an explicit `visibility` field are now treated as private (self-mode only) in consumer-mode execution. Publishers must add `"visibility": "public"` to any env entry meant to be visible to direct consumers (`ocx package exec PKG -- cmd`, `ocx package env PKG`). *(package)*
- **Breaking:** `ExecMode::Consumer` is now the default for `ocx package exec`, `ocx env`, `ocx package env`, and `ocx package deps`. The previous behavior was a union of all non-sealed visibility levels (equivalent to `--mode=full`). Packages that relied on private dep env being visible at direct exec must either declare those binaries as entry points (which routes through `--mode=self` automatically) or elevate the relevant dep to `visibility: "public"`. *(cli)*

## [0.2.1] - 2026-03-24

### Added

- Colorize JSON output and deduplicate table rendering *(cli)*
- Enable parallel XZ compression by default *(compression)*
- Auto-detect progress indicators based on stderr TTY *(cli)*
- Add progress bar to package create *(bundle)*
- Add transfer progress bars to push and pull operations *(oci)*
- Replace upstream Platform with owned OperatingSystem and Architecture enums *(oci)*
- Add package variant support

### Changed

- Rename Reportable to Printable and move JSON printing to Printer *(cli)*
- Increase default push chunk size to 16 MiB *(oci)*
- Migrate to thiserror with typed subsystem errors *(error)*

### Fixed

- Use glibc bun builds and update musl guidance *(mirror)*
- Skip missing logo gracefully in package info *(cli)*
- Stub of java -version *(docs)*
- Ensure authentication before all transport operations *(oci)*
- Split repository path segments for correct Windows paths *(fs)*
- Clean up partial output file on failed package create *(bundle)*
- Resolve context menu shadow and sidebar shift on catalog detail page

## [0.2.0] - 2026-03-16

### Added

- Separate strip_components for rebundling and support multiple --version flags *(mirror)*
- Add package pull, ci export command, and setup-ocx GitHub Action *(ci)*
- Add package describe and package info commands
- Add package catalog with build-time data generation *(website)*
- Add bun and git-cliff mirrors, restructure mirror layout *(mirror)*
- Add shell profile management commands *(shell)*
- Replace hello-world with realistic packages in recordings and docs *(docs)*
- Add per-platform strip_components config *(mirror)*
- Add shellcheck and uv mirrors *(mirror)*
- Add generator-based url_index sources *(mirror)*
- Support tag-scoped index update *(index)*
- Add cache, github, and text utility modules *(mirror-sdk)*
- Add spec extends, --latest flag, and backfill order *(mirror)*
- Add corretto mirror configuration *(mirror)*
- Add --color flag with NO_COLOR/CLICOLOR support *(cli)*
- Add asset_type config with binary support and shfmt mirror *(mirror)*
- Align recordings with real-world packages *(recordings)*

### Changed

- Rename OCX_DISABLE_CODESIGN to OCX_NO_CODESIGN
- Rework table printer styling and clean up idioms *(cli)*

### Fixed

- Install scripts use --remote for bootstrap and improve UX
- Add SBOM generation to website deploy pipeline *(ci)*
- Add catalog generation to deploy website workflow *(ci)*
- Pipe PowerShell Invoke-Expression through Out-String *(install)*
- Filter internal tags at IndexImpl level *(index)*
- Correct RUST_LOG empty-string check *(log)*
- Resolve shellcheck warnings in shell scripts *(lint)*
- Buffer env var writes in ci export to fix path accumulation *(ci)*
- Handle ANSI escape sequences in table realignment *(recordings)*
- Use musl target for Linux in setup-ocx *(action)*
- Correct inaccuracies across docs and CLI help text *(docs)*

## [0.1.0] - 2026-03-13

### Added

- Initial commit
- Support catalog for local index
- Add env command
- Support symlink paths for env and shell env
- Add select command
- Add initial reference manager
- Add deselect, uninstall, and clean commands
- Add find command
- Add Vue components for website
- Add command line docs and fix xz bundling
- Support bundling of single files
- Add warning when using tags with --current
- Add acceptance test suite and fix initial bugs
- Add terminal recordings
- Add macOS code signing
- Add website searchbar
- Add --index flag and OCX_INDEX env var
- Make cascade push platform-aware
- Use underscore as build separator
- Add JSON Schema generation for metadata.json *(schema)*
- Redesign info command with logo, color, and format support *(cli)*
- Add zip format support and symlink security hardening *(archive)*
- Add workspace version (0.1.0) and shared version utility
- Add git-cliff config and initial CHANGELOG.md
- Add Taskfile release commands (changelog, preview, prepare)
- Add install.sh bootstrap script for Unix/macOS
- Add install.ps1 bootstrap script for Windows
- Initialize cargo-dist for release builds
- Add update check notification on CLI startup
- Add SBOM generation and dependency explorer page
- Add ocx-mirror prototype for mirroring GitHub releases to OCI registries

### Changed

- Find_or_install
- Adapt env command and extract modifier
- Consistent naming of api data
- Extract symlink handling into file_structure
- Index structure
- Migrate install and select to reference manager
- Extract CLI tasks into library
- OCI client
- Pre-fetch terminal recordings
- Standardize CLI API data types with single-table pattern
- Replace oci_spec::Reference with custom Identifier
- Move website help examples into documentation skill

### Documentation

- Add initial documentation
- Symlink management
- File structure
- Versioning
- Improve content path documentation
- Add user-guide for indices
- Add getting started guide
- Add design records and pitch guide
- Add release plan
- Add changelog page to website with sidebar entry
- Add installation page, README install section, and home quick start
- Shorten website feature cards
- Rewrite README for public launch

### Fixed

- Fetching of just pushed manifest
- Flaky tests relying on env variables
- Package manager task stability and error handling
- Website
- Windows symlinks with junctions
- Tree component hover behavior
- Path and exec resolution on Windows
- Formatting and linter issues
- Improve CLI output consistency across commands
- Temp dir leftovers after install
- MacOS codesigning, sign per-file, drop --deep and CS_RUNTIME flags
- User-guide typo
- Discord notification website link
- Allow all tools in Claude Code Action workflow *(ci)*
- Fix changelog template formatting
- Bootstrap OCX via self-install instead of manual file copy
- Correct auth env vars in publish workflow and remove double update check
- Align cargo-dist config with workflow filename and pin action versions
- Add rust-toolchain.toml to pin Rust 1.94.0 for CI builds
- Exclude LFS assets from source tarball and reduce CI targets
- Add required type and version fields to packaging metadata
- Add required field to path env var in packaging metadata
- Website deploy pipeline and install script improvements
- Correct checksum filename and parsing in install scripts
- Clippy warning, test build target, and mirror test assertions
- Restore Python language skill to correct location
- Restore crate-level allow(deprecated) for clippy -D warnings
- Build and upload ocx-mirror binary in CI acceptance tests
- Replace ring with aws-lc-rs to fix aarch64-pc-windows-msvc release build
- Add contents:read permission to verify-deep workflow *(ci)*
- Build ocx-mirror in verify-deep and fix discord webhook *(ci)*
- Remove push-to-main Discord notifications *(ci)*

<!-- Links -->
[rfc-8785]: https://www.rfc-editor.org/rfc/rfc8785
[taplo]: https://taplo.tamasfe.dev/
[ghsa-q455]: https://github.com/rust-lang/rust/security/advisories/GHSA-q455-m56c-85mh
[authenticode-ref]: https://learn.microsoft.com/en-us/windows-hardware/drivers/install/authenticode
[signpath-ref]: https://about.signpath.io/
[0.2.1]: https://github.com/ocx-sh/ocx/compare/v0.2.0..v0.2.1
[0.2.0]: https://github.com/ocx-sh/ocx/compare/v0.1.0..v0.2.0
[0.1.0]: https://github.com/ocx-sh/ocx/tree/v0.1.0
