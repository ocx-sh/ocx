# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.10] - 2026-06-30

### Added

- Bake entrypoint args with ${installPath} interpolation
- Support tar+zstd layer format (#58)

### Changed

- Unify metadata interpolation behind capability-gated engine

### Fixed

- Scope host-leaf resolution to named bindings *(run)*
- Make nushell shim PATH activation parse on nu without get --optional *(setup)*
- Wire ~/.profile for non-bash POSIX login shells *(setup)*
- Decouple elvish completion from PATH activation *(setup)*

## [0.3.9] - 2026-06-21

### Added

- Idempotent move-to-front PATH manipulation *(shell)*
- Idempotent batch PATH, remove OCX_ACTIVATED for all shells *(shell)*

### Fixed

- Resolve latest through configured index, honour OCX_INDEX *(self-update)*
- Serialize tag locks with deterministic key order *(index)*
- Deterministic key order for CLI output structs *(cli)*
- Repair Windows activation harness to use `ocx self setup` *(test)*
- Batch single-statement move-to-front + FOR/F delims= *(shell)*
- Apply non-POSIX shell activation as data (nushell) + capture elvish eval *(shell)*
- Break release-readiness concurrency self-deadlock *(ci)*

### Release

- V0.3.9

## [0.3.8] - 2026-06-15

### Added

- Typed Starlark host API for package testing *(script)*
- `ocx package test --script` embedded Starlark runner *(cli)*
- Pipeline subcommand + per-platform applicability + Discord/JUnit reporting *(mirror)*
- Drift guard ignores action-pin bumps; SHA-pin setup-ocx *(mirror)*
- Add --frozen flag to freeze tag resolution to the local index *(cli)* **BREAKING**
- Add version pinning to ocx self setup (#156) *(cli)*
- Per-platform lock pinning with whole-file lock/upgrade model *(project)* **BREAKING**

### Changed

- Make ChainMode matching exhaustive *(oci)*
- Move ocx-mirror to its own repository **BREAKING**
- Quick wins on the install hot path *(install)*
- Streaming single-pass pull pipeline *(oci)* **BREAKING**
- Collapse redundant create_dir_all in tar extraction *(archive)*
- Parallelize V2 index-retention scan *(gc)*

### Documentation

- Fix digest-verification callout for streaming pull *(website)*

### Fixed

- Stop baking metadata.json into bundle content *(mirror)*
- Parallelize per-tag digest fetch in index update *(oci)*
- Fail-safe target-registry reads in discover and sync *(mirror)*
- Stop prepare legs re-crawling the source (N+1 crawls) *(mirror)*
- Bound decompression budget and reject zero-size layers *(oci)*
- Deterministic exit code on concurrent symlink failures *(install)*
- Route eval-safe-output advisory through the log module *(cli)*
- Use job-level defaults.run.shell for matrixed Windows shells *(ci)*

### Release

- V0.3.8

## [0.3.7] - 2026-06-07

### Fixed

- Guard shell activation against an unset OCX_HOME *(setup)*

## [0.3.6] - 2026-06-07

### Added

- Show platform icons in package catalog *(website)*
- Show manifest layers in default package inspect *(cli)*
- Add setup module for shell scaffold ownership *(lib)*
- Add ocx self setup command + self-update shim refresh *(cli)*

### Changed

- Share canonical ocx_cli_identifier helper *(oci)*
- Slim install scripts to delegate to ocx self setup *(install)*

### Documentation

- Document ocx self setup + bare-binary install path *(self-setup)*

### Fixed

- Read remote tag queries through to source *(oci)*
- Address review-fix loop findings *(self-setup)*
- Harden ZDOTDIR + document ocx-owned dedicated files *(self-setup)*
- Report current path on install --select *(cli)*
- Error on conflicting versions in one environment *(package)*

### Release

- V0.3.6

## [0.3.5] - 2026-06-02

### Added

- Client-declared registry mirrors via [mirrors] config *(oci)*
- CI env export via --ci on env / package env *(cli,oci)*
- Ocx package push emits a structured push report *(cli)*

### Fixed

- Keep zsh completions alive across a late compinit *(install)*
- Wire both bash and zsh profiles, not just $SHELL *(install)*
- Authenticate before every registry operation *(oci)*
- Add short description to package push subcommand *(cli)*
- Git-style plugin dispatch + correct not-found hint *(cli)*
- Advance floating prerelease tag on every cascade build *(package)*

### Release

- V0.3.5

## [0.3.4] - 2026-05-31

### Documentation

- Strip internal design references from --help text *(cli)*

### Fixed

- Escape angle brackets in changelog *(release)*
- Mark `package test` trailing command as a `last` positional *(cli)*
- Emit shell completions inline, ASCII-safe (fixes zsh + PowerShell activation) *(cli)*
- Cross-platform pwsh env.ps1 + local test-install mode *(install)*
- Make Windows installers ASCII- and StrictMode-safe *(install)*
- Keep shell-completion help text ASCII-safe for PowerShell *(cli)*

### Release

- V0.3.4

## [0.3.3] - 2026-05-31

### Fixed

- Repair Windows PowerShell shell activation *(cli)*
- Rewrite ocx.toml in place through the lock-owning handle *(project)*
- Give touch_state_atomic collision-free temp names *(update-check)*
- Reject a non-regular config path consistently across platforms *(config)*
- Read ocx.toml through the lock-owning handle *(project)*

### Release

- V0.3.3

## [0.3.2] - 2026-05-28

### Added

- Enrich `ocx version` + `ocx about` with build provenance *(cli)*
- Dispatch unknown subcommands to ocx-&lt;name&gt; plugins on PATH *(cli)*

### Changed

- Unify on LockedFile primitive *(file-lock)*

### Fixed

- Accept binary-mode sha256.sum *(install)*
- Support Windows PowerShell 5.1 in install.ps1 *(install)*
- Write tag store through lock-owning handle on Windows *(oci)*
- Tolerate populated build-info blocks in CI test *(cli)*
- Make download tests fast and meaningful *(ocx-mirror)*
- Emit package root from ocx pull, not content/ subdir *(cli)*
- Align torn-JSON test with locked-reader contract *(auth)*

### Release

- 0.3.2

## [0.3.1] - 2026-05-27

### Added

- Direct PATH activation with completions, write both login and rc profiles *(install)*
- Add `ocx self` command group with throttled update checks *(cli)*

### Changed

- Nest ocx mirror as ocx/cli and adopt package install / add CLI

### Fixed

- Exclude ocx_shim from cargo-dist plan *(release)*
- Resolve via current symlink + correct test wire shape *(self-update)*
- Build smoke artifact with __testing feature; parallelize acceptance tests *(ci)*
- Blob store locking + path normalization *(windows)*

### Release

- 0.3.1

## [0.3.0] - 2026-05-26

### Added

- Redesign landing page with feature sections, scroll reveal, and licensed asset pipeline *(website)*
- Redesign roadmap as standalone page with scroll-driven timeline *(website)*
- Add package dependency resolution with deps CLI
- Replace export bool with Visibility enum on dependencies
- Per-platform asset_type override + lychee mirror *(mirror)*
- Three-tier content-addressed storage **BREAKING**
- Multi-registry support for index catalog command *(cli)*
- Transparent tag fallback on local index miss *(oci)*
- Serialise local tag log writes with per-repo lock *(oci)*
- Chain refs resolution and --remote as CAS cache mode *(oci)* **BREAKING**
- Multi-layer package push and pull (#20) *(package)* **BREAKING**
- Surface ClientError::BlobNotFound as PackageErrorKind::BlobNotFound *(error)*
- Support zero-layer config-only OCI artifacts *(package)*
- List accepted extensions in bare-digest error *(publisher)*
- Add async layered configuration system with registry resolution *(config)*
- Typed exit codes and error normalization *(cli)*
- Add JSON Schema generation and taplo auto-completion for config.toml *(config)*
- Add /next skill for state-aware next-step suggestions *(claude)*
- Package entry points *(package)* **BREAKING**
- Add dev.ocx.sh staging deploy with banner *(website)*
- Add ocx package test for local pre-push validation *(cli)* **BREAKING**
- Shell-driven scenario harness for acceptance tests *(test)*
- Project toolchain (ocx.toml + ocx.lock + run/add/remove/init) *(project)* **BREAKING**
- --build-timestamp + dev.ocx.sh continuous deploy
- Add ocx login and ocx logout commands *(cli)* **BREAKING**
- Add ocx package inspect command *(cli)*
- Add Entrypoint.command dispatch divergence *(package)*
- Unify inspect digest rendering across views *(cli)*
- Windows native .exe shim, .cmd cutover (resolves #66) *(launcher)* **BREAKING**
- Publish first-party binaries under ocx/ namespace *(release)*
- Symlink GC ledger + explicit --global toolchain tier *(project)* **BREAKING**
- Toolchain CLI taxonomy + global activation via env exporter *(cli)* **BREAKING**
- Doc-script render harness — doc_scripts tree, publish render layer, scenario migration *(test,website)*
- Decorated table output with per-column/cell styles *(cli)*
- Activate Nushell + Elvish shells in install.sh *(install)*
- Readable inspect output — themed root, human sizes, descriptor chain *(cli)* **BREAKING**
- Env output format is a context-only concern *(cli)* **BREAKING**
- Global toolchain ocx.lock is an implicit GC root *(clean)*
- Eager pull default for lock/upgrade + --pull/--no-pull pair; pull touches lock for direnv *(cli)* **BREAKING**

### Changed

- Route archive symlink extraction through symlink::create
- Extract garbage collector with BFS reachability
- Add PinnedIdentifier and decompose OCI pull pipeline
- Drop redundant root identifier from ResolvedPackage *(package)*
- Offload layer SHA-256 hashing to spawn_blocking *(oci)*
- Algorithm-typed digest dispatch and enriched BlobNotFound *(oci)*
- Add path_exists_lossy and adopt across call sites *(utility)*
- Unify bounded-concurrency fan-out on stream::buffered
- Drop --shell from ocx shell direnv *(cli)*
- Drop Entrypoint.target, dispatch via composed PATH *(package)* **BREAKING**
- Convert entrypoints from array to map shape *(package)* **BREAKING**
- Extract config-blob metadata loader into common.rs *(package)*
- Extract resolve_top_manifest shared helper *(package-manager)*
- Rename update command to upgrade *(cli)* **BREAKING**
- Rename find command to which *(cli)* **BREAKING**
- Rename info command to about *(cli)* **BREAKING**
- Consolidate direnv into a dedicated ocx direnv group *(cli)* **BREAKING**
- Move ocx which under the ocx package group *(cli)* **BREAKING**
- Collapse --global to single root-only flag *(cli)* **BREAKING**
- Move ocx deps under the ocx package group *(cli)* **BREAKING**
- Remove dead static-init/profile/path-strip shell scaffolding
- Remove dead _OCX_APPLIED fingerprint helpers

### Documentation

- Correct push layer arguments and digest reference syntax *(cli)*
- Redesign user guide as use-case-driven walkthrough *(website)*
- Add package authoring guide *(website)*
- Soften read-only wording, document command field *(package-inspect)*
- Signed-off handshake + harden superseded tier ADR *(toolchain)*
- Shell-profile activation prior-art for handshake §4 *(research)*
- Retruth shell-activation model — supersede stale ADRs, fix release/arch rules + user docs
- Shell-activation model, global toolchain, inspect chain

### Fixed

- Stub missing licensed assets so CI build succeeds *(website)*
- Preserve original licensed asset URLs in build output *(website)*
- Add --remote flag to first-run commands *(website)*
- Enable clippy --all-targets to lint test code
- Cap ProgressWriter write size for smoother download progress
- Update workflow task references to rust: namespace *(ci)*
- Guard shell RC source line so deleting $OCX_HOME does not error *(install)*
- Verify pulled layer bytes match claimed digest *(oci)*
- Make LayerRef media type total via ArchiveMediaType enum *(publisher)*
- Verify file digest with manifest-declared algorithm *(mirror)*
- Static commands survive malformed ambient config *(cli)*
- Harden config loader, fix error chain rendering, and extend exit-code coverage *(config,cli)*
- Route ChainedIndex catalog and tag list by ChainMode *(oci)*
- Exclude worktrees from .claude markdown scans *(tests)*
- Completion error casing, debug log level, install docs *(shell)*
- Trigger roadmap item fade-in on initial load *(website)*
- Emit synth-entrypoints PATH after declared bin/ *(package-manager)*
- Rehydrate package offline from cached blobs and layers *(package-manager)*
- Relax AC2 disjoint-refs assertion to per-side uniqueness *(test)*
- Mark recording setup env entries public so consumer view emits PATH *(test)*
- Zero uid/gid in tar headers for reproducibility *(archive)*
- Make discord website-deploy notification env-aware *(ci)*
- Make macOS portable *(tests)*
- Surface malformed image-index child digest as structured error *(cli)*
- Refresh drifted Windows shim blobs; align windows acceptance tests *(shim)*
- Hermetic reproducible cross-build via cargo-zigbuild *(shim)*
- Reconcile entrypoints schema tests with array contract *(test)*
- Fail-closed GC ledger + global/project conflict seam *(project)*
- Adopt install.ps1 to global toolchain model + pwsh shell alias *(install)*
- Toolchain mutators no longer create candidate symlinks *(cli)* **BREAKING**
- Restore entrypoints schema tests to object contract *(test)*
- Replace nightly windows_by_handle with stable Win32 API *(windows)*
- Drop deleted synthetic_pathext_entry call + unused-mut/import warnings *(windows)*

### Release

- V0.3.0

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

### Release

- V0.1.0
[0.3.10]: https://github.com/ocx-sh/ocx/compare/v0.3.9..v0.3.10
[0.3.9]: https://github.com/ocx-sh/ocx/compare/v0.3.8..v0.3.9
[0.3.8]: https://github.com/ocx-sh/ocx/compare/v0.3.7..v0.3.8
[0.3.7]: https://github.com/ocx-sh/ocx/compare/v0.3.6..v0.3.7
[0.3.6]: https://github.com/ocx-sh/ocx/compare/v0.3.5..v0.3.6
[0.3.5]: https://github.com/ocx-sh/ocx/compare/v0.3.4..v0.3.5
[0.3.4]: https://github.com/ocx-sh/ocx/compare/v0.3.3..v0.3.4
[0.3.3]: https://github.com/ocx-sh/ocx/compare/v0.3.2..v0.3.3
[0.3.2]: https://github.com/ocx-sh/ocx/compare/v0.3.1..v0.3.2
[0.3.1]: https://github.com/ocx-sh/ocx/compare/v0.3.0..v0.3.1
[0.3.0]: https://github.com/ocx-sh/ocx/compare/v0.2.1..v0.3.0
[0.2.1]: https://github.com/ocx-sh/ocx/compare/v0.2.0..v0.2.1
[0.2.0]: https://github.com/ocx-sh/ocx/compare/v0.1.0..v0.2.0
[0.1.0]: https://github.com/ocx-sh/ocx/compare/v0.0.0..v0.1.0

