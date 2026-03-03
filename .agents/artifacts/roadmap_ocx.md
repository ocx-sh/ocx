# Roadmap: OCX

## Overview

**Status:** Active
**Owner:** mherwig
**Last Updated:** 2026-03-03
**Timeline:** 2026-Q1 — ongoing

## Vision

OCX becomes the standard backend tool for distributing pre-built binaries via OCI registries — reliable, reproducible, and trivially integrable into any CI/CD or build system. The roadmap focuses on hardening the core, expanding platform reach, and building the ecosystem integrations that make OCX the obvious choice.

## Goals

| Goal | Key Result | Status |
|------|------------|--------|
| Production-grade reliability | Atomic installs, process-ref GC safety, no partial state | Not Started |
| Cross-platform CI coverage | Test matrix: Linux arm64, macOS, Windows, musl | Not Started |
| Ecosystem integrations | Bazel rules, GitHub Actions, install scripts | Not Started |
| Self-service distribution | Homebrew, Scoop, install scripts for OCX itself | Not Started |

## Phases

### Phase 0: Public Release Readiness (Gate)

**Status:** Not Started

Everything required before flipping the repository to public. Grouped into three tiers: hard blockers, should-fix, and nice-to-have.

#### P0 — Hard Blockers

These prevent a credible public launch.

- [ ] Fix compile errors: invalid regex in `accumulator.rs:10`, never-loop in `version.rs:368`
- [ ] Add LICENSE file (Apache-2.0 recommended given dependency landscape)
- [ ] Fix submodule URL in `.gitmodules` to absolute HTTPS (`https://github.com/ocx-sh/rust-oci-client`)
- [ ] Fix landing page: replace 3× lorem ipsum in feature cards, fix CTA button links (`/markdown-examples` → docs), fix `ocx run` → `ocx exec`
- [ ] Write Installation page (`website/src/docs/installation.md` — currently empty)
- [ ] Write Getting Started page (`website/src/docs/getting-started.md` — currently empty)

#### P1 — Should Fix

These affect credibility and safety for a public project.

- [ ] Add push/PR triggers to CI workflow (`build-binaries.yml` is `workflow_dispatch` only)
- [ ] Add clippy + fmt check step to CI
- [ ] SHA-pin actions in `deploy-website.yml` (has SSH key access) + fix `actions/checkout@v6` → `@v4`
- [ ] Add NOTICE file for Apache-2.0 attribution (oci-client fork, aws-lc-sys/OpenSSL)
- [ ] Add `version`, `description`, `license`, `repository` to both `Cargo.toml` files
- [ ] Run `cargo fmt` across workspace
- [ ] Fix user guide auth typo: `OXC_AUTH_*` → `OCX_AUTH_*`
- [ ] Fill or remove empty user guide stub sections (Indices, Mirrors)
- [ ] Remove VitePress template pages ("MD", "API") from public nav
- [ ] Add `.beads/interactions.jsonl` to `.gitignore`
- [ ] Scrub personal username (`mherwig`) from tracked planning files

#### P2 — Nice to Have

Improve first impressions but not strictly blocking.

- [ ] Write a README with: what OCX is, install instructions, basic usage, link to docs
- [ ] Add CONTRIBUTING.md (build, test, PR workflow)
- [ ] Add CHANGELOG.md
- [ ] Fix 11 clippy warnings
- [ ] Remove unused `lazy_static` dependency if confirmed unused
- [ ] Upgrade VitePress from `^2.0.0-alpha.16` to current stable

**Dependencies:** None — this phase gates everything else

**Risks:**
- Submodule repo (`ocx-sh/rust-oci-client`) must also be public before OCX goes public
- oci-client patches need a plan for crates.io if OCX is ever published there

---

### Phase 1: Harden Core (Foundation)

**Status:** Not Started

High-impact items that fix existing gaps or unblock future work. No new features — just make what exists bulletproof.

**Deliverables:**
- [ ] Atomic installs: write to temp dir, rename on success (prevents partial state on interrupt)
- [ ] Local installed catalog: `ocx index catalog --installed` leveraging `ObjectStore::list_all()`
- [ ] Process refs: running processes lock packages to prevent GC without persistent symlinks
- [ ] Auto-index of unknown packages: auto-sync index metadata when a package is referenced but not yet indexed locally
- [ ] User guide: document `.tar.xz` vs `.tar.gz` trade-off for `ocx package create`

**Dependencies:** None

**Risks:**
- Atomic rename semantics differ across filesystems (ext4 vs NTFS vs APFS)

---

### Phase 2: Platform & CI (Reach)

**Status:** Not Started

Expand platform coverage and make OCX easy to install and test everywhere.

**Deliverables:**
- [ ] CI rework: multi-platform test matrix (Windows, macOS, musl, Linux arm64)
- [ ] Snapshot releases: continuous deploy to GitHub Releases
- [ ] Multiple install methods: Homebrew, Chocolatey/Scoop, install scripts
- [ ] Fix GitHub Pages base path (`/ocx/`) vs reverse proxy path (`/`) broken links
- [ ] Platform-aware cascade: rolling releases computed relative to the same platform
- [ ] Platform-specific candidate symlinks (e.g. `linux/arm64/my-package`)

**Dependencies:** Phase 1 (atomic installs) for reliable cross-platform behavior

**Risks:**
- Windows symlink semantics require elevated privileges or developer mode
- macOS code signing adds per-platform complexity

---

### Phase 3: Developer Experience (Polish)

**Status:** Not Started

Quality-of-life improvements that make OCX pleasant to use interactively and easier to debug.

**Deliverables:**
- [ ] Progress bars for long operations (downloads, extractions)
- [ ] Advanced log settings with layers and filters
- [ ] `update` command: update symlinked packages to latest versions
- [ ] Shell profile integration: `ocx shell install` writes export blocks into `.bashrc`/`.zshrc`
- [ ] API output: support comma-separated tags and platforms in report formatting
- [ ] Rustdoc: shorten type links for readability

**Dependencies:** None (can overlap with Phase 2)

**Risks:**
- Shell integration is fragile across shell flavors (bash, zsh, fish, PowerShell)

---

### Phase 4: Architecture (Internal Quality)

**Status:** Not Started

Refactors and internal improvements that pay down tech debt and prepare for future features.

**Deliverables:**
- [ ] Refactor OCI reference data types (`Reference`, `Identifier`, `Digest`) for clarity
- [ ] Externalize index cache: replace internal `RwLock` cache with `IndexCache` trait
- [ ] Layered storage: cached/temporary layers above the content-addressed store
- [ ] `link`/`unlink` commands: GC-aware user-managed symlinks (deferred until use case arises)

**Dependencies:** Phase 1 core hardening informs cache design decisions

**Risks:**
- Reference type refactor touches many call sites

---

### Phase 5: Ecosystem (Long Term)

**Status:** Not Started

Integrations, advanced packaging, and the features that make OCX a platform.

**Deliverables:**
- [ ] Dependencies and lockfiles
- [ ] Bazel rules for consuming OCX packages
- [ ] GitLab CI steps for consuming OCX packages
- [ ] Mirror CLI: `ocx mirror` to re-publish from GitHub/GitLab releases, Cargo, npm
- [ ] SBOM and signature support
- [ ] Metadata schema v2: richer package metadata (deps, build instructions, config)
- [ ] Shims and lazy loading: on-demand download triggered by shim executable
- [ ] Package hooks/plugins: company-specific post-install actions
- [ ] OCI manifest mirror URLs (challenge: URL mutates digest, breaks lockfile integrity)
- [ ] Modern UI components for the documentation website

**Dependencies:** Phase 1-3 stable, lockfile design requires ADR

**Risks:**
- Lockfile + mirror URL tension (digest integrity vs URL rewriting)
- Plugin system scope creep

---

## Milestones

| Milestone | Target | Status |
|-----------|--------|--------|
| Public release readiness (P0 blockers) | 2026-Q1 | Pending |
| Public release readiness (P1 should-fix) | 2026-Q1 | Pending |
| Atomic installs + local catalog | 2026-Q1 | Pending |
| CI multi-platform matrix green | 2026-Q2 | Pending |
| Homebrew + install script available | 2026-Q2 | Pending |
| `update` + shell integration | 2026-Q3 | Pending |
| Lockfile design ADR | 2026-Q3 | Pending |
| Bazel rules alpha | 2026-Q4 | Pending |

## Ideas (Unscheduled)

- `ocx test`: packages declare test scripts; ocx runs them to verify correct installation

## Related Documents

- [User Guide](../../website/src/docs/user-guide.md)
- [CLI Reference](../../website/src/docs/reference/command-line.md)
- [Environment Reference](../../website/src/docs/reference/environment.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-03 | mherwig | Initial roadmap migrated from TODO.md |
