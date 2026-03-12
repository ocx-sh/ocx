# ADR: Release and Install Strategy

**Status:** Proposed
**Date:** 2026-03-12
**Author:** Principal Architect

---

## Context

OCX needs a complete release ecosystem: when a semver tag (e.g. `v0.5.0`) is pushed to GitHub, pre-built binaries should be published as a GitHub Release, and the binary should be installable via multiple channels:

1. **GitHub Releases** with pre-built binaries for all major platforms
2. **Shell install script** (`curl https://ocx.sh/install.sh | sh`)
3. **PowerShell install script** (`irm https://ocx.sh/install.ps1 | iex`)
4. **Homebrew** (`brew install ocx-sh/tap/ocx`)
5. **Cargo** (`cargo install ocx` or `cargo binstall ocx`)

### Current State

- **Existing CI/CD** (4 workflows):
  - `deploy-canary.yml` — Multi-stage canary pipeline: builds 3 native targets (linux-gnu x86_64, macOS arm64, Windows x86_64), runs unit + acceptance tests, then cross-compiles 5 additional targets (linux-gnu aarch64, linux-musl x86_64/aarch64, macOS x86_64, Windows arm64) using `houseabsolute/actions-rust-cross@v1.0.5`. Creates a `canary` pre-release on main with all 8 binaries via `softprops/action-gh-release`. Triggered manually (`workflow_dispatch`).
  - `verify-basic.yml` — PR smoke tests (fmt, clippy, build, nextest)
  - `verify-licenses.yml` — License compliance checks
  - `deploy-website.yml` — VitePress site deployment to GitHub Pages
  - `.github/actions/build-rust/action.yml` — Custom composite action for native Rust builds (toolchain setup, caching, binary upload)
- **8 targets already built** (canary): `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`
- **No version field**: Neither workspace Cargo.toml nor crate Cargo.toml files define a `version`
- **Patched dependency**: `oci-client` is patched via `[patch.crates-io]` pointing to `external/rust-oci-client` (a git submodule with a flush fix in `pull_blob`). This **blocks crates.io publishing** until the patch is upstreamed or the fork is published separately
- **Binary name**: `ocx` (defined in `crates/ocx_cli/Cargo.toml` as `[[bin]] name = "ocx"`)
- **Website**: VitePress site at `website/`, deployed to `ocx.sh` via GitHub Pages
- **GitHub org**: `ocx-sh` (repo: `ocx-sh/ocx`)

---

## Decision

### Primary Tool: cargo-dist

**Recommendation: Use [cargo-dist](https://github.com/axodotdev/cargo-dist) (v0.31.0, ~2k stars, actively maintained by axodotdev) as the release automation backbone.**

cargo-dist is the most comprehensive, well-maintained solution for Rust binary releases. It covers all requirements out of the box:

| Requirement | cargo-dist Support |
|---|---|
| GitHub Release with binaries | Built-in (auto-generates `release.yml`) |
| Shell installer (`install.sh`) | Built-in (Jinja2 templates, checksum verification) |
| PowerShell installer (`install.ps1`) | Built-in |
| Homebrew tap | Built-in (auto-generates formula, publishes to tap repo) |
| Cross-platform builds | Built-in (matrix of targets via GitHub Actions) |
| Checksums | Built-in (sha256, embedded in installers since v0.26.0) |

#### Why not alternatives?

| Tool | Stars | Role | Why Not Primary |
|---|---|---|---|
| **cargo-dist** | ~2,000 | Release automation | **Selected** |
| **cross-rs** | ~8,000 | Cross-compilation | Useful complement, but cargo-dist handles this via `cargo-zigbuild` |
| **cargo-release** | ~1,500 | Version bump + publish | Only handles `cargo publish`, no binary builds |
| **release-plz** | ~1,300 | Version bump + changelog | Complementary for changelog, but cargo-dist handles releases |
| **houseabsolute/actions-rust-cross** | ~200 | GH Action for cross builds | Retained for `aarch64-unknown-linux-musl` and `aarch64-pc-windows-msvc` (cargo-dist broken) |
| **Homebrew Releaser** | ~200 | GH Action for Homebrew | Redundant, cargo-dist handles this natively |

**Recommended combination**: The most common pattern in the Rust ecosystem is **release-plz + cargo-dist**: release-plz automates version bumps, changelog generation (via git-cliff), and crates.io publishing; cargo-dist handles binary packaging and GitHub Releases. Consider adding release-plz after Phase 1 is validated.

**Self-update feature**: cargo-dist supports bundling [axoupdater](https://github.com/axodotdev/axoupdater) (`install-updater = true` in config), which installs a separate `ocx-update` binary alongside `ocx`. It checks GitHub Releases for newer versions and self-updates in-place. Also available as a Rust library for embedding `ocx self-update` directly. Consider for a future phase.

### Target Matrix

The existing `deploy-canary.yml` already builds 8 targets. For the release pipeline, we recommend **keeping all 8 targets** since the infrastructure is proven:

| Target | Runner | Method | Tier | Notes |
|---|---|---|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` | Native | 1 | Primary Linux |
| `aarch64-unknown-linux-gnu` | `ubuntu-latest` | Cross (`actions-rust-cross`) | 1 | ARM64 Linux |
| `x86_64-unknown-linux-musl` | `ubuntu-latest` | Cross (`actions-rust-cross`) | 2 | Static binary, Alpine/containers |
| `aarch64-unknown-linux-musl` | `ubuntu-latest` | Cross (`actions-rust-cross`) | 2 | Static ARM64, Alpine/containers |
| `x86_64-apple-darwin` | `macos-latest` | Cross (`actions-rust-cross`) | 1 | Intel Mac |
| `aarch64-apple-darwin` | `macos-latest` | Native | 1 | Apple Silicon |
| `x86_64-pc-windows-msvc` | `windows-latest` | Native | 1 | Windows x64 |
| `aarch64-pc-windows-msvc` | `windows-latest` | Cross (`actions-rust-cross`) | 2 | Windows ARM64 |

**Note**: The canary workflow already builds and tests all 8 targets successfully. cargo-dist would replace `houseabsolute/actions-rust-cross` with its own cross-compilation approach (`cargo-zigbuild`). Alternatively, the release workflow can be built on the existing canary infrastructure rather than adopting cargo-dist, reducing migration risk.

**cargo-dist limitation**: `aarch64-unknown-linux-musl` cross-compilation is **broken in cargo-dist** ([issue #1581](https://github.com/axodotdev/cargo-dist/issues/1581), open since Nov 2024). If musl ARM64 is required, either keep the `actions-rust-cross` approach for that target or defer it. `x86_64-unknown-linux-musl` works fine in cargo-dist.

**Practical recommendation**: Start cargo-dist with 6 targets (the 5 Tier 1 + `x86_64-unknown-linux-musl`). Keep `actions-rust-cross` as a separate job for `aarch64-unknown-linux-musl` and `aarch64-pc-windows-msvc` until cargo-dist fixes support.

---

## Architecture

### 1. Release Trigger Flow

cargo-dist generates a `release.yml` workflow that uses a **fan-out/fan-in** pattern:

1. **Plan** — a single job runs `cargo dist plan`, producing a build manifest (which targets, which installers, which artifacts)
2. **Build** — N parallel jobs, one per target platform. Each runs on the appropriate runner (ubuntu/macos/windows), compiles the binary, and uploads it as a GitHub Actions artifact. cargo-dist handles native and cross-compilation (via `cargo-zigbuild`) automatically based on the target
3. **Host** — a single job that downloads all build artifacts from the previous step, creates the GitHub Release, and uploads everything (binaries, installers, checksums)
4. **Publish** — pushes the Homebrew formula to the tap repo, etc.
5. **Announce** — finalizes the GitHub Release (marks it non-draft)

```
Developer pushes semver tag (v0.5.0)
        |
        v
    ┌── Plan (cargo dist plan → manifest) ──┐
    │                                        │
    │   Fan-out: one build job per target     │
    │                                        │
    ├── build-x86_64-linux-gnu    (ubuntu)  ─┤
    ├── build-aarch64-linux-gnu   (ubuntu)  ─┤
    ├── build-x86_64-linux-musl   (ubuntu)  ─┤
    ├── build-aarch64-linux-musl* (ubuntu)  ─┤
    ├── build-x86_64-apple-darwin (macos)   ─┤
    ├── build-aarch64-apple-darwin(macos)   ─┤
    ├── build-x86_64-windows-msvc (windows) ─┤
    ├── build-aarch64-windows*    (windows) ─┤
    │                                        │
    │   Fan-in: collect all artifacts        │
    │                                        │
    └── Host ────────────────────────────────┘
        │
        ├── GitHub Release with:
        │     ocx-x86_64-unknown-linux-gnu.tar.xz
        │     ocx-aarch64-unknown-linux-gnu.tar.xz
        │     ocx-x86_64-unknown-linux-musl.tar.xz
        │     ocx-aarch64-unknown-linux-musl.tar.xz
        │     ocx-x86_64-apple-darwin.tar.xz
        │     ocx-aarch64-apple-darwin.tar.xz
        │     ocx-x86_64-pc-windows-msvc.zip
        │     ocx-aarch64-pc-windows-msvc.zip
        │     ocx-installer.sh
        │     ocx-installer.ps1
        │     sha256sum.txt
        │
        ├── Publish: update Homebrew tap
        │
        └── Announce: finalize GitHub Release

  * targets where cargo-dist has known cross-compilation issues;
    add custom build jobs using actions-rust-cross, uploading
    artifacts in the same format so the Host job collects them
    alongside the cargo-dist-managed builds.
```

**Implementation hint for problematic targets**: cargo-dist's `release.yml` is a generated file but can be extended with custom jobs. For `aarch64-unknown-linux-musl` and `aarch64-pc-windows-msvc` (where cargo-dist's `cargo-zigbuild` is broken — [issue #1581](https://github.com/axodotdev/cargo-dist/issues/1581)), add separate build jobs that use `houseabsolute/actions-rust-cross` (already proven in the existing canary workflow). These jobs upload their artifacts using the same naming convention, and the `Host` job collects them alongside the cargo-dist-managed builds. This hybrid approach preserves full 8-target coverage while benefiting from cargo-dist's installer and Homebrew automation.

### 2. Pre-Release Verification Workflow

Repurpose the existing `deploy-canary.yml` into a **deep verification workflow** (`verify-release.yml`) that acts as a manual gate before tagging a release.

**Rationale**: The release workflow (cargo-dist) is triggered by a semver tag push and is hard to undo once artifacts are published. A separate verification workflow lets you build, test, and validate across all 8 targets *before* committing to a release. This catches platform-specific regressions, cross-compilation issues, and acceptance test failures before they become a botched release.

**Release ceremony**:

```
1. Developer decides to release
        |
        v
2. Manually trigger verify-release.yml (workflow_dispatch)
        |
        +---> Build all 8 targets (native + cross)
        +---> Run unit tests on native targets
        +---> Run acceptance tests (registry:2)
        +---> Upload binaries as artifacts (for manual smoke testing)
        +---> Report pass/fail
        |
        v
3. Developer reviews results, optionally downloads and tests binaries
        |
        v
4. Developer pushes semver tag (git tag v0.5.0 && git push origin v0.5.0)
        |
        v
5. release.yml (cargo-dist) triggers automatically
        → Builds, packages, publishes GitHub Release + Homebrew
```

**Key differences from the release workflow**:

| Aspect | verify-release.yml | release.yml (cargo-dist) |
|---|---|---|
| Trigger | `workflow_dispatch` (manual) | Tag push (`v*`) |
| Tests | Full: unit + acceptance + cross-build | Build only (no tests) |
| Artifacts | Uploaded as workflow artifacts (7-day retention) | Published to GitHub Release (permanent) |
| Homebrew | No | Yes (pushes formula) |
| Installers | No | Yes (generates sh/ps1) |
| Purpose | Validate before release | Produce the release |

**Implementation**: Evolve `deploy-canary.yml` → `verify-release.yml`:
- Keep the existing 3-stage structure (native build+test → acceptance tests → cross-build)
- Remove the `snapshot-release` job (no more canary pre-release)
- Add an optional `ref` input so it can verify a specific branch or commit
- Keep `workflow_dispatch` trigger

```yaml
on:
  workflow_dispatch:
    inputs:
      ref:
        description: 'Git ref to verify (branch, tag, or SHA)'
        required: false
        default: ''
```

This separates concerns cleanly: **verify-release proves the code is releasable**; **release.yml produces the release**. The canary snapshot release becomes unnecessary once proper releases exist.

### 3. Install Script Hosting on ocx.sh

**Important finding**: cargo-dist does **not** natively support custom domain hosting for install scripts. It generates `install.sh` and `install.ps1` with hardcoded GitHub Releases URLs and attaches them to each release. There is no `custom-domain` config key.

**Two approaches for `ocx.sh/install.sh`:**

| Approach | Pros | Cons |
|---|---|---|
| **A. Redirect proxy** | Zero maintenance, always serves latest | Extra HTTP hop, depends on GitHub Releases URLs |
| **B. Custom installer script** | Full control, single request, branded | ~50 lines to maintain, must update URL patterns on release format changes |

**Recommended: Approach A** — place a thin shell wrapper in `website/src/public/install.sh` that fetches the cargo-dist-generated installer from the latest GitHub Release:

```bash
#!/bin/sh
# ocx.sh/install.sh — Thin wrapper that fetches the real installer from GitHub Releases
set -eu
exec curl -fsSL "https://github.com/ocx-sh/ocx/releases/latest/download/ocx-installer.sh" | sh "$@"
```

The VitePress `public/` directory serves files as-is on GitHub Pages, so `https://ocx.sh/install.sh` will serve this script directly (no HTML redirect needed).

**Fallback (Approach B)**: If the redirect adds too much latency or breaks `curl | sh` piping, write a ~50-line custom installer (like `mise`, `fnm`, and others do) that detects `uname -s`/`uname -m`, downloads the correct tarball from GitHub Releases, verifies checksums, and installs to `~/.ocx/bin`.

**One-liner install commands:**
```bash
# Unix/macOS
curl -fsSL https://ocx.sh/install.sh | sh

# Windows PowerShell
irm https://ocx.sh/install.ps1 | iex
```

### 4. Homebrew Tap

Create a new repository: `ocx-sh/homebrew-tap`

cargo-dist will auto-generate and push a formula (`Formula/ocx.rb`) to this repo on each release. The formula downloads pre-built binaries per platform (using `on_macos`/`on_linux` + `on_arm`/`on_intel` blocks).

**User install:**
```bash
brew install ocx-sh/tap/ocx
```

cargo-dist handles:
- Formula generation with correct SHA256 checksums
- SPDX-to-Homebrew license translation
- Running `brew style --fix` before committing
- Only publishing stable releases (not prereleases)

### 5. Cargo Install / cargo-binstall

#### 4a. `cargo install` (from source)

**Blocker: patched `oci-client` dependency.** crates.io does not allow `[patch.crates-io]` entries. Options:

| Option | Effort | Risk |
|---|---|---|
| **A. Upstream the patch** | Low | Depends on upstream review timeline |
| **B. Publish fork as `oci-client-ocx`** | Medium | Maintenance burden of separate crate |
| **C. Defer crates.io publishing** | None | Users can't `cargo install` |

**Recommendation: Option A first, Option C as fallback.** The patch is a single `flush()` call in `pull_blob` — a clear bugfix that upstream should accept. If upstream is slow, defer crates.io publishing; the other install channels (script, Homebrew, GitHub Release) cover the user need.

When ready to publish:
1. Add `version = "0.1.0"` (or appropriate) to workspace and crate Cargo.toml files
2. Set `publish = false` on `ocx_lib` (it depends on the patched `oci-client`)
3. Publish only `ocx_cli` to crates.io (requires `ocx_lib` to also be published, so this needs the patch resolved first)
4. Use [Trusted Publishing](https://crates.io/docs/trusted-publishing) (OIDC-based, no long-lived API tokens)

#### 4b. `cargo binstall` (pre-built binaries)

cargo-dist generates artifacts compatible with `cargo binstall` out of the box. Add metadata to `crates/ocx_cli/Cargo.toml`:

```toml
[package.metadata.binstall]
pkg-url = "{ repo }/releases/download/v{ version }/ocx-{ target }.tar.xz"
bin-dir = "ocx-{ target }/{ bin }{ binary-ext }"
pkg-fmt = "txz"
```

This works even before crates.io publishing — users can run:
```bash
cargo binstall --git https://github.com/ocx-sh/ocx ocx
```

### 6. Version Management

cargo-dist uses git tags as the source of truth for versions. The flow:

1. Update version in `Cargo.toml` (can be automated with `cargo-release` or manual)
2. Commit: `chore: bump version to 0.5.0`
3. Tag: `git tag v0.5.0`
4. Push tag: `git push origin v0.5.0`
5. cargo-dist CI kicks in automatically

**For changelog generation**, consider adding [git-cliff](https://github.com/orhun/git-cliff) (10k+ stars) later, either standalone or via release-plz integration.

---

## Implementation Plan

### Approach: cargo-dist vs Evolving Existing CI

Two viable strategies exist given the existing CI infrastructure:

| Approach | Pros | Cons |
|---|---|---|
| **A. cargo-dist** | Auto-generated installers (sh, ps1, Homebrew), auto-updated CI via `dist init`, cargo-binstall compat, checksums | Replaces working canary pipeline, may conflict with `actions-rust-cross` approach, less control over cross-compilation |
| **B. Evolve canary** | Builds on proven 8-target matrix, keeps `actions-rust-cross`, full control | Must write install scripts manually, must maintain Homebrew formula manually, more maintenance long-term |

**Recommendation: Option A (cargo-dist)** — the install script generation, checksum embedding, and Homebrew automation justify the migration. The canary workflow can coexist during transition and be retired after the release pipeline is validated.

### Phase 1: Core Release Pipeline

1. **Add version to workspace**:
   ```toml
   # Cargo.toml
   [workspace.package]
   version = "0.1.0"
   ```
   And in each crate: `version.workspace = true`

2. **Initialize cargo-dist**:
   ```bash
   cargo install cargo-dist
   cargo dist init
   ```
   Select: GitHub CI, shell installer, PowerShell installer, Homebrew

   This generates:
   - `dist-workspace.toml` (config)
   - `.github/workflows/release.yml` (CI pipeline)

3. **Configure targets** in `dist-workspace.toml` (match existing canary coverage):
   ```toml
   [dist]
   targets = [
     "x86_64-unknown-linux-gnu",
     "aarch64-unknown-linux-gnu",
     "x86_64-unknown-linux-musl",
     "aarch64-unknown-linux-musl",
     "x86_64-apple-darwin",
     "aarch64-apple-darwin",
     "x86_64-pc-windows-msvc",
     "aarch64-pc-windows-msvc",
   ]
   installers = ["shell", "powershell", "homebrew"]
   install-path = "~/.ocx/bin"
   ```

4. **Create Homebrew tap repo**: `ocx-sh/homebrew-tap`

5. **Configure Homebrew publishing** in `dist-workspace.toml`:
   ```toml
   [dist.publish.homebrew]
   tap = "ocx-sh/homebrew-tap"
   formula = "ocx"
   ```

6. **Validate against canary**: Compare cargo-dist output against existing canary binaries to ensure parity. Test with a pre-release tag: `git tag v0.1.0-rc.1 && git push origin v0.1.0-rc.1`

7. **Retire canary workflow**: Once the release pipeline is validated, update `deploy-canary.yml` to either trigger `release.yml` or be removed. Keep `verify-basic.yml` for PR checks.

### Phase 2: Install Script Hosting

8. **Add redirect for `ocx.sh/install.sh`**: Create `website/src/public/install.sh` as a shell script that redirects to the latest release:
   ```bash
   #!/bin/sh
   # Redirect to cargo-dist installer from latest GitHub release
   exec curl -fsSL "https://github.com/ocx-sh/ocx/releases/latest/download/ocx-installer.sh" "$@"
   ```

   Or use a Cloudflare redirect rule if the site is behind Cloudflare.

9. **Add redirect for `ocx.sh/install.ps1`**: Same pattern for PowerShell.

### Phase 3: Cargo Publishing (when patch is upstreamed)

10. **Upstream the `oci-client` flush fix**
11. **Remove `[patch.crates-io]` section** once upstream publishes the fix
12. **Add `cargo binstall` metadata** to `ocx_cli/Cargo.toml`
13. **Set up Trusted Publishing** on crates.io
14. **Add `cargo publish` step** to release workflow

### Phase 4: CI Hardening

15. **Add Dependabot** or Renovate for dependency updates (existing `verify-basic.yml` and `verify-licenses.yml` already cover PR validation)

---

## Trade-off Analysis

| Dimension | cargo-dist | Custom GH Actions | GoReleaser |
|---|---|---|---|
| **Setup effort** | Low (`dist init`) | High (write YAML) | Medium (config file) |
| **Maintenance** | Auto-updated via `dist init` | Manual | Manual |
| **Rust support** | Native, first-class | Generic | Generic |
| **Install scripts** | Auto-generated with checksums | Must write from scratch | Shell only |
| **Homebrew** | Auto-generated formula + publish | Separate action needed | Supported |
| **npm** | Supported | Manual | Not supported |
| **MSI** | Supported | Manual | Not supported |
| **Flexibility** | Config-driven, extensible | Unlimited | Config-driven |
| **Community** | Growing (2k stars) | N/A | Large (14k stars, Go-focused) |

**cargo-dist wins** for Rust projects due to native Rust understanding, auto-generated CI that stays in sync, and comprehensive installer support.

---

## Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| cargo-dist breaking changes | CI breaks on update | Pin cargo-dist version, test before updating |
| cargo-dist migration breaks existing targets | Missing binaries for some platforms | Run cargo-dist in parallel with canary workflow during validation; compare output binaries |
| cargo-dist cross-compilation differs from `actions-rust-cross` | Binary compatibility issues (musl, Windows ARM) | Test binaries on target platforms before retiring canary; fall back to custom workflow for problematic targets |
| Upstream oci-client slow to merge patch | Can't publish to crates.io | Defer crates.io; binaries + Homebrew cover 95% of installs |
| macOS code signing requirements | Users get Gatekeeper warnings | cargo-dist supports ad-hoc signing; proper signing can be added later |
| GitHub Actions costs | Paid runners for ARM builds | Start with cross-compilation (free), upgrade to native ARM runners if needed |

---

## References

- [cargo-dist documentation](https://opensource.axo.dev/cargo-dist/book/)
- [cargo-dist GitHub](https://github.com/axodotdev/cargo-dist) (~2,000 stars, v0.31.0, Feb 2026)
- [cross-rs](https://github.com/cross-rs/cross) (~8,000 stars, Docker-based cross-compilation)
- [cargo-release](https://github.com/crate-ci/cargo-release) (~1,500 stars, version bump + publish)
- [release-plz](https://github.com/release-plz/release-plz) (~1,300 stars, changelog + publish automation)
- [Homebrew Tap docs](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap)
- [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) (pre-built binary install)
- [Trusted Publishing RFC](https://rust-lang.github.io/rfcs/3691-trusted-publishing-cratesio.html) (OIDC for crates.io)
- [Rust Platform Support](https://doc.rust-lang.org/rustc/platform-support.html) (Tier 1/2/3 targets)

### Install Script Precedents

| Tool | Shell | PowerShell | Install Location |
|---|---|---|---|
| rustup | `curl https://sh.rustup.rs \| sh` | N/A | `~/.cargo/bin` |
| Deno | `curl -fsSL https://deno.land/install.sh \| sh` | `irm https://deno.land/install.ps1 \| iex` | `~/.deno/bin` |
| Bun | `curl -fsSL https://bun.sh/install \| bash` | `irm bun.sh/install.ps1 \| iex` | `~/.bun/bin` |
| mise | `curl https://mise.run \| sh` | N/A | `~/.local/bin` |
| starship | `curl -sS https://starship.rs/install.sh \| sh` | N/A | `/usr/local/bin` |

OCX install path recommendation: **`~/.ocx/bin`** (consistent with `OCX_HOME` convention).
