# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.0.0] - 2026-03-13

### Added

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

### Documentation

- Add changelog page to website with sidebar entry
- Add installation page, README install section, and home quick start

### Fixed

- Allow all tools in Claude Code Action workflow *(ci)*
- Fix changelog template formatting
- Bootstrap OCX via self-install instead of manual file copy
- Correct auth env vars in publish workflow and remove double update check
[0.0.0]: https://github.com/ocx-sh/ocx/tree/v0.0.0

