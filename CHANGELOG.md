# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Multi-layer package push and pull. `ocx package push` now accepts multiple layer arguments, each either a file path or a `sha256:<hex>.tar.gz` digest reference. *(package)*
- Layered configuration from `/etc/ocx/config.toml`, `~/.config/ocx/config.toml`, `$OCX_HOME/config.toml` with `--config` / `OCX_CONFIG_FILE` overrides and `OCX_NO_CONFIG` kill-switch. *(config)*
- Typed `ExitCode` taxonomy aligned with BSD sysexits (64/65/69/74/75/77/78/79/80/81). Scripts can now `case $?` reliably. *(cli)*
- `entrypoints` field in package metadata. Publishers declare named launchers (e.g. `cmake`, `ctest`); `ocx install` generates per-platform launchers under `<package>/entrypoints/<name>` (POSIX) and `<package>/entrypoints/<name>.cmd` (Windows) that delegate to `ocx exec` against a baked `file://<package-root>` URI. *(package)*
- `${deps.NAME.installPath}` template interpolation in env-var values. Entrypoint `target` strings and env modifier values can now reference dependency install paths. Unrecognized `${...}` tokens are rejected at publish time. *(package)*
- `name` field on `Dependency`. Lets a package import a dependency under a different name, disambiguating basename collisions when two deps would otherwise resolve to the same launcher slot. *(package)*
- `ocx exec` accepts a `file://<absolute-package-root>` URI. Generated entrypoint launchers bake this URI into a single `ocx exec 'file://<root>' -- "$(basename $0)"` form so they survive a re-`select` to a different version (the symlink target moves; the URI does not). *(cli)*
- Flat install layout: `current` and `candidates/{tag}` target the package root, so generated launchers are reachable through `current/entrypoints/`. The previous `entrypoints-current` symlink is gone; selection state lives on a single per-repo `current` anchor. *(file-structure)*
- `ValidMetadata` typestate on package metadata. Publish-time validation rejects bundles with malformed entry points, undeclared dep references, or duplicate launcher names; downstream code consumes only validated metadata. *(package)*
- `EntrypointNameCollision` structured error at `install --select` / `select` and at consumption time (`ocx env`, `ocx exec`) when two packages in the same visible closure declare the same entrypoint `name`. Surfaces with exit code 65 (`DataError`); recovery: deselect one package before selecting the other. *(package-manager)*
- Synthetic `PATH ⊳ <pkg-root>/entrypoints` entry emitted per visible package with a non-empty `entrypoints` array, so generated launchers are reachable through `ocx env` / `ocx exec` / `ocx shell env` without manual PATH wiring. *(env)*
- Windows-only synthetic `PATHEXT ⊳ .CMD` prepend on `ocx env` and auto-injected by `ocx exec` so generated `.cmd` launchers are discoverable when the host shell's `PATHEXT` does not already include `.CMD`. Consumer-boundary commands (`install`, `select`, `shell env`, `ci export`, `shell profile load`) emit a stderr warning when `PATHEXT` is missing `.CMD`. *(cli)*

### Changed

- **Breaking:** `--remote` / `OCX_REMOTE` semantics narrowed — tag and catalog lookups now bypass the local tag store and query the registry directly, but digest-addressed blob reads still use the local cache with write-through to `$OCX_HOME/blobs/`. Previously, `--remote` routed all operations to the registry. Only `$OCX_HOME/tags/` is no longer updated under `--remote`. *(oci)*
- **Breaking:** `ocx index update` no longer pre-fetches manifest or layer blobs. It writes only tag→digest pointers to `$OCX_HOME/tags/`. Run `ocx install <pkg>` online first to populate the blob cache before using `--offline`. *(index)*
- **Breaking:** `ocx env --format json` and `ocx ci export --format json` now emit `{"entries": [{"key": ..., "value": ..., "type": ...}, ...]}` (canonical envelope) instead of a bare top-level array. Update consumers that deserialize JSON output to read the `entries` field. The shape is shared across env-related JSON outputs for forward compatibility. *(cli)*
- Error messages normalized across 11 modules per Rust API Guidelines `C-GOOD-ERR` (lowercase, no trailing punctuation). *(error)*

### Breaking

- **Breaking:** Package metadata field renamed from `entry_points` to `entrypoints`. Publishers must update `metadata.json` files; bundles using the old field name fail validation at `package create`. *(package)*
- **Breaking:** Dependency JSON field key renamed from `alias` to `name`. Existing bundles must be re-published with `"name"` in place of `"alias"`. The `${deps.NAME.installPath}` template token is unchanged — `NAME` was always the placeholder keyword, never the literal field name. *(package)*
- **Breaking:** `ocx-mirror` exit codes changed from `0/2/3/4` to `0/65/79/1/69` to align with the sysexits-based taxonomy. Wrapper scripts matching historic codes must be updated. *(mirror)*

### Fixed

- `ocx --offline install <pkg>` after a bare `ocx index update <pkg>` now fails with a clear `OfflineManifestMissing` error naming the missing digest instead of a silent failure. Recovery: run `ocx install <pkg>` online to populate the blob cache. *(oci)*

### Security

- Pulled layer blobs are verified against the claimed digest before extraction; mismatched blobs are deleted and fail the pull with a clear error. *(oci)*
- Hardened the Windows `.cmd` launcher template by adding `DisableDelayedExpansion` to the `SETLOCAL` directive to mitigate registry-level `!`-expansion vectors when forwarding untrusted argv (`BatBadBut` interim mitigation). The `%*` parameter remains unescaped at the `.cmd` level — see `.claude/artifacts/adr_windows_cmd_argv_injection.md` for the threat model and tracked compiled-shim follow-up. *(package)*

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
[0.2.1]: https://github.com/ocx-sh/ocx/compare/v0.2.0..v0.2.1
[0.2.0]: https://github.com/ocx-sh/ocx/compare/v0.1.0..v0.2.0
[0.1.0]: https://github.com/ocx-sh/ocx/tree/v0.1.0
