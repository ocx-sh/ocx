# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-05-26

### Added

- Three-tier content-addressed storage **BREAKING**
- Project toolchain — `ocx.toml` + `ocx.lock`, `run`/`add`/`remove`/`init`, explicit `--global` tier, GC ledger, global lock as implicit GC root *(project)* **BREAKING**
- CLI taxonomy reshuffle *(cli)* **BREAKING**
  - Renamed: `update`→`upgrade`, `find`→`which`, `info`→`about`
  - Grouped: `which`, `deps`, `test`, `inspect` under `ocx package`; direnv commands under `ocx direnv`
  - `--global` collapsed to single root-only flag; global activation via env exporter
  - New: `ocx login`/`logout`, `ocx package test`, `ocx package inspect`
- Package entrypoints overhaul — map shape, `Entrypoint.command` dispatch, composed-PATH resolution *(package)* **BREAKING**
- Multi-layer package push/pull (#20), zero-layer config-only artifacts, dep resolution with `deps` CLI, `Visibility` enum on deps *(package)* **BREAKING**
- Index/OCI — chain refs + `--remote` as CAS cache mode, multi-registry catalog, transparent tag fallback, per-repo tag-log lock, `BlobNotFound` surfacing *(oci)* **BREAKING**
- Layered async config + registry resolution, JSON Schema + taplo completion for `config.toml` *(config)*
- Readable inspect output — themed root, human sizes, descriptor chain, unified digest rendering *(cli)* **BREAKING**
- Env output format is context-only *(cli)* **BREAKING**
- Eager pull default for lock/upgrade + `--pull`/`--no-pull`; pull touches lock for direnv *(cli)* **BREAKING**
- Windows native `.exe` shim, `.cmd` cutover (#66) *(launcher)* **BREAKING**
- Publish first-party binaries under `ocx/` namespace *(release)*
- Typed exit codes + error normalization *(cli)*
- Decorated table output with per-column/cell styles *(cli)*
- Activate Nushell + Elvish in `install.sh` *(install)*
- Per-platform `asset_type` override + lychee mirror *(mirror)*
- Doc-script render harness — `doc_scripts` tree, publish render layer, scenario migration *(test,website)*
- Shell-driven scenario harness *(test)*
- List accepted extensions in bare-digest error *(publisher)*
- Website — landing redesign with scroll reveal + licensed asset pipeline, standalone roadmap with timeline, `dev.ocx.sh` staging deploy with banner, `--build-timestamp` continuous deploy *(website)*
- `/next` skill for state-aware next-step suggestions *(claude)*

### Changed

- OCI pull pipeline — `PinnedIdentifier` decomposition, algorithm-typed digest dispatch, drop redundant root id from `ResolvedPackage`, offload SHA-256 to `spawn_blocking`, extract `resolve_top_manifest`, extract config-blob metadata loader *(oci,package)*
- Garbage collector — extract with BFS reachability
- Symlink/path — route archive extraction through `symlink::create`, `path_exists_lossy` across call sites, unify bounded-concurrency on `stream::buffered`
- Drop `--shell` from `ocx shell direnv` *(cli)*
- Remove dead static-init/profile/path-strip scaffolding + `_OCX_APPLIED` fingerprint helpers

### Documentation

- Redesign user guide as use-case-driven walkthrough; add package authoring guide *(website)*
- Shell-activation model, global toolchain, inspect chain — supersede stale ADRs, fix release/arch rules + user docs; signed-off handshake + harden superseded tier ADR; prior-art research for handshake §4
- Correct push layer arguments + digest reference syntax; soften read-only wording, document `command` field *(cli,package-inspect)*

### Fixed

- Config — harden loader, error-chain rendering, exit-code coverage; static commands survive malformed ambient config *(config,cli)*
- OCI/index — verify pulled layer bytes match claimed digest, route `ChainedIndex` catalog/tag list by `ChainMode`, surface malformed image-index child digest as structured error *(oci,cli)*
- Entrypoints/package-manager — emit synth-entrypoints PATH after declared `bin/`, rehydrate offline from cached blobs/layers, `LayerRef` media type total via `ArchiveMediaType`, reconcile then restore schema tests *(package-manager,test)*
- Toolchain mutators no longer create candidate symlinks *(cli)* **BREAKING**
- Fail-closed GC ledger + global/project conflict seam *(project)*
- Windows shim — refresh drifted blobs + align acceptance tests, hermetic reproducible cross-build via cargo-zigbuild, replace nightly `windows_by_handle` with stable Win32, drop deleted `synthetic_pathext_entry` call *(shim,windows)*
- Install — adopt `install.ps1` to global toolchain + pwsh alias; guard shell RC source line; completion error casing, debug log level, install docs *(install,shell)*
- Mirror/archive — verify file digest with manifest-declared algorithm, zero uid/gid in tar headers for reproducibility *(mirror,archive)*
- Website — stub missing licensed assets so CI build succeeds, preserve original licensed asset URLs, `--remote` on first-run commands, trigger roadmap fade-in on initial load *(website)*
- Tests — relax AC2 disjoint-refs assertion to per-side uniqueness, mark recording setup env public, exclude worktrees from `.claude` scans, make macOS portable *(test)*
- CI — update workflow task refs to `rust:` namespace, make discord notification env-aware *(ci)*
- Misc — enable `clippy --all-targets` on test code, cap `ProgressWriter` write size *(cli)*

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
- Prevent PowerShell Invoke-Expression error on empty shell output

### Release

- V0.2.1

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

### Release

- V0.2.0

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

[0.3.0]: https://github.com/ocx-sh/ocx/compare/v0.2.1..v0.3.0
[0.2.1]: https://github.com/ocx-sh/ocx/compare/v0.2.0..v0.2.1
[0.2.0]: https://github.com/ocx-sh/ocx/compare/v0.1.0..v0.2.0
[0.1.0]: https://github.com/ocx-sh/ocx/tree/v0.1.0

