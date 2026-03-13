# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
[0.1.0]: https://github.com/ocx-sh/ocx/tree/v0.1.0
