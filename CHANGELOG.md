# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Multi-layer package push and pull. `ocx package push` now accepts multiple layer arguments, each either a file path or a `sha256:<hex>.tar.gz` digest reference. *(package)*
- Layered configuration from `/etc/ocx/config.toml`, `~/.config/ocx/config.toml`, `$OCX_HOME/config.toml` with `--config` / `OCX_CONFIG_FILE` overrides and `OCX_NO_CONFIG` kill-switch. *(config)*
- Typed `ExitCode` taxonomy aligned with BSD sysexits (64/65/69/74/75/77/78/79/80/81). Scripts can now `case $?` reliably. *(cli)*
- Project-tier toolchain: declare a repository's tools in `ocx.toml` and lock them to digests in `ocx.lock`. The lock carries a `declaration_hash` over the canonicalized `ocx.toml` ([RFC 8785][rfc-8785]) so downstream commands refuse to run with stale digests. *(project)*
- `ocx lock` resolves every advisory tag in `ocx.toml` to an immutable digest and writes `ocx.lock`. Repeatable `--group` flag scopes resolution to one or more named groups (`ci`, `release`, …). *(cli)*
- `ocx update [PKG]` re-resolves tags in the lock — opt-in upgrade flow distinct from `ocx lock`'s "freeze whatever the registry surfaces today" semantics. *(cli)*
- `ocx pull` (project tier, distinct from `ocx package pull`) pre-warms the package store from `ocx.lock` without creating install symlinks — ideal for CI matrix builds and direnv-driven workstations. *(cli)*
- `ocx hook-env` (stateful prompt-hook entry point) and `ocx shell-hook` (stateless direnv-style export generator) emit shell exports for the resolved project toolchain. The fingerprint env var `_OCX_APPLIED` keeps the prompt cheap by skipping unchanged invocations. *(shell)*
- `ocx shell init <SHELL>` prints a per-shell init snippet that wires `hook-env` into Bash `PROMPT_COMMAND`, Zsh `precmd`, Fish `fish_prompt`, and Nushell `pre_prompt` hooks. *(shell)*
- `ocx generate direnv` writes a `.envrc` file wiring `ocx shell-hook` into [direnv](https://direnv.net/), watching `ocx.toml` and `ocx.lock` for re-evaluation. *(cli)*
- `ocx shell profile generate` emits the same exports as `shell profile load` to a file the user sources once from their shell rc. This is the only `shell profile` subcommand that survives v2. *(shell)*
- Home-tier `ocx.toml` at `$OCX_HOME/ocx.toml` (default `~/.ocx/ocx.toml`) serves as a fallback when no project file is in scope, so user-wide tools surface in scratch directories and system shells. *(project)*
- JSON Schema for `ocx.toml` (`https://ocx.sh/schemas/project/v1.json`) and `ocx.lock` (`https://ocx.sh/schemas/project-lock/v1.json`); both wired through [taplo][taplo] for editor auto-completion. The `project-lock` schema carries a top-level `$comment` flagging the format as machine-generated. *(schema)*

### Deprecated

- `ocx shell profile add`, `shell profile remove`, `shell profile list`, and `shell profile load` are deprecated in v1 and will be removed in v2. Use the project-tier `ocx.toml` plus `ocx shell init` (or `ocx generate direnv`) instead — see the [migration guide](https://ocx.sh/docs/user-guide.html#project-toolchain-migration). `shell profile generate` survives v2 as a one-shot file-generating convenience. *(shell)*

### Changed

- **Breaking:** `--remote` / `OCX_REMOTE` semantics narrowed — tag and catalog lookups now bypass the local tag store and query the registry directly, but digest-addressed blob reads still use the local cache with write-through to `$OCX_HOME/blobs/`. Previously, `--remote` routed all operations to the registry. Only `$OCX_HOME/tags/` is no longer updated under `--remote`. *(oci)*
- **Breaking:** `ocx index update` no longer pre-fetches manifest or layer blobs. It writes only tag→digest pointers to `$OCX_HOME/tags/`. Run `ocx install <pkg>` online first to populate the blob cache before using `--offline`. *(index)*
- Error messages normalized across 11 modules per Rust API Guidelines `C-GOOD-ERR` (lowercase, no trailing punctuation). *(error)*

### Breaking

- **Breaking:** `ocx-mirror` exit codes changed from `0/2/3/4` to `0/65/79/1/69` to align with the sysexits-based taxonomy. Wrapper scripts matching historic codes must be updated. *(mirror)*

### Fixed

- `ocx --offline install <pkg>` after a bare `ocx index update <pkg>` now fails with a clear `OfflineManifestMissing` error naming the missing digest instead of a silent failure. Recovery: run `ocx install <pkg>` online to populate the blob cache. *(oci)*

### Security

- Pulled layer blobs are verified against the claimed digest before extraction; mismatched blobs are deleted and fail the pull with a clear error. *(oci)*

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
[0.2.1]: https://github.com/ocx-sh/ocx/compare/v0.2.0..v0.2.1
[0.2.0]: https://github.com/ocx-sh/ocx/compare/v0.1.0..v0.2.0
[0.1.0]: https://github.com/ocx-sh/ocx/tree/v0.1.0
