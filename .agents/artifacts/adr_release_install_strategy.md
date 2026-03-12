# ADR: Release and Install Strategy

**Status:** Accepted
**Date:** 2026-03-12
**Author:** Principal Architect

---

## Context

OCX needs a complete release ecosystem: when a semver tag (e.g. `v0.5.0`) is pushed to GitHub, pre-built binaries should be published as a GitHub Release with release notes, and the binary should be installable via multiple channels:

1. **GitHub Releases** with pre-built binaries for all major platforms
2. **Shell install script** (`curl https://ocx.sh/install.sh | sh`) — hosted on the website
3. **PowerShell install script** (`irm https://ocx.sh/install.ps1 | iex`) — hosted on the website
4. **Changelog** maintained in-repo (keep-a-changelog format) and published to the website

### Deferred to Future ADRs

The following distribution channels are out of scope for this ADR and will be addressed separately:

- **Homebrew tap** (`brew install ocx-sh/tap/ocx`) — requires tap repo setup, formula generation, and publishing automation
- **Cargo install / cargo-binstall** (`cargo install ocx`) — blocked by the patched `oci-client` dependency until the flush fix is upstreamed

### Current State

- **Existing CI/CD** (4 workflows):
  - `deploy-canary.yml` — Multi-stage canary pipeline: builds 3 native targets (linux-gnu x86_64, macOS arm64, Windows x86_64), runs unit + acceptance tests, then cross-compiles 5 additional targets (linux-gnu aarch64, linux-musl x86_64/aarch64, macOS x86_64, Windows arm64) using `houseabsolute/actions-rust-cross@v1.0.5`. Creates a `canary` pre-release on main with all 8 binaries via `softprops/action-gh-release`. Triggered manually (`workflow_dispatch`).
  - `verify-basic.yml` — PR smoke tests (fmt, clippy, build, nextest)
  - `verify-licenses.yml` — License compliance checks
  - `deploy-website.yml` — VitePress site deployment via rsync
  - `.github/actions/build-rust/action.yml` — Custom composite action for native Rust builds (toolchain setup, caching, binary upload)
- **8 targets already built** (canary): `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`
- **No version field**: Neither workspace Cargo.toml nor crate Cargo.toml files define a `version` (defaults to `0.0.0`)
- **No changelog**: No `CHANGELOG.md` or release notes infrastructure exists
- **Patched dependency**: `oci-client` is patched via `[patch.crates-io]` pointing to `external/rust-oci-client` (a git submodule with a flush fix in `pull_blob`). This **blocks crates.io publishing** until the patch is upstreamed or the fork is published separately
- **Binary name**: `ocx` (defined in `crates/ocx_cli/Cargo.toml` as `[[bin]] name = "ocx"`)
- **Website**: VitePress site at `website/`, deployed to `ocx.sh` via rsync
- **GitHub org**: `ocx-sh` (repo: `ocx-sh/ocx`)

---

## Decision

### 1. Primary Tool: cargo-dist (Builds and Releases Only)

**Use [cargo-dist](https://github.com/axodotdev/cargo-dist) (~2k stars, actively maintained by axodotdev) for binary builds, archive packaging, checksum generation, and GitHub Release publishing.**

**Important scope limitation:** cargo-dist is used **only for builds and releases** — NOT for installer generation. OCX uses custom install scripts that bootstrap OCX with itself (see Section 5). cargo-dist's built-in shell/PowerShell installers are incompatible with OCX's three-store architecture and self-managing install pattern. Do not enable `installers` in the cargo-dist config.

| Requirement | cargo-dist Support | Used? |
|---|---|---|
| GitHub Release with binaries | Built-in (auto-generates `release.yml`) | **Yes** |
| Cross-platform builds | Built-in (matrix of targets via GitHub Actions) | **Yes** |
| Archive packaging (.tar.xz, .zip) | Built-in | **Yes** |
| Checksums (sha256) | Built-in | **Yes** |
| Shell installer (`install.sh`) | Built-in (Jinja2 templates) | **No** — custom bootstrap script instead |
| PowerShell installer (`install.ps1`) | Built-in | **No** — custom bootstrap script instead |

#### Why not alternatives?

| Tool | Stars | Role | Why Not Primary |
|---|---|---|---|
| **cargo-dist** | ~2,000 | Release automation | **Selected** |
| **cross-rs** | ~8,000 | Cross-compilation | Useful complement, but cargo-dist handles this via `cargo-zigbuild` |
| **cargo-release** | ~1,500 | Version bump + publish | Only handles `cargo publish`, no binary builds |
| **release-plz** | ~1,300 | Version bump + changelog | Complementary; consider after Phase 1 is validated |
| **houseabsolute/actions-rust-cross** | ~200 | GH Action for cross builds | Retained for targets where cargo-dist has issues |

### 2. Changelog Tool: git-cliff

**Use [git-cliff](https://github.com/orhun/git-cliff) (11.6k stars, Rust-native) for changelog generation and version bumping.**

#### Changelog Tool Comparison

| Feature | git-cliff | towncrier | changie | release-please | semantic-release | release-plz |
|---|---|---|---|---|---|---|
| **Stars** | 11.6k | 887 | 865 | 6.5k | 23.4k | 1.3k |
| **Approach** | Commit-based | Fragment | Fragment | Commit + PR | Commit (auto) | Commit + PR |
| **Runtime** | None (Rust binary) | Python | None (Go binary) | None (GH Action) | Node.js | None (Rust binary) |
| **Keep-a-changelog** | Yes (built-in template) | Manual template | Yes (default) | Partial | No (plugin) | Yes (via git-cliff) |
| **GitHub Releases** | Yes (via action output) | Manual scripting | Yes (action) | Yes (built-in) | Yes (plugin) | Yes (built-in) |
| **Rust/Cargo aware** | No (language agnostic) | No | No | Partial | No | Yes (native) |
| **Version bumping** | Yes (`--bump`) | No | No | Yes | Yes | Yes |
| **Maintenance** | Excellent (monthly releases) | Good | Good | Good (Google) | Excellent | Excellent |
| **Last release** | Jan 2026 | Aug 2025 | Nov 2025 | Active 2026 | Jan 2026 | Mar 2026 |

**Why git-cliff:**

1. **Rust-native** — single binary, zero runtime dependencies, installable via `cargo install git-cliff`
2. **Most adopted** — 11.6k stars, used by dozens of prominent Rust projects, active monthly releases
3. **Keep-a-changelog format** — ships with a `keepachangelog` template out of the box
4. **Built-in version bumping** — `--bump` calculates the next semver from commit types (`feat:` → minor, `fix:` → patch, breaking → major); `--bumped-version` outputs just the version string for scripting
5. **GitHub Actions integration** — official `orhun/git-cliff-action@v4` outputs changelog directly for use in GitHub Release body
6. **Flexible templates** — Tera (Jinja2-inspired) templates in `cliff.toml`; can customize output for both `CHANGELOG.md` and per-release notes
7. **Composable** — git-cliff is the changelog engine used internally by release-plz; adopting it now preserves the option to wrap it with release-plz later

**Why not the others:**

- **towncrier** — Python runtime dependency is out of place in a Rust project; no native keep-a-changelog output
- **changie** — Low adoption (865 stars); fragment workflow adds PR overhead without Cargo awareness
- **release-please** — Config duplication with `Cargo.toml`; opinionated PR-merge workflow
- **semantic-release** — Node.js runtime; fully automatic releases on every push is too aggressive for this project
- **release-plz** — Strong candidate for a future phase (wraps git-cliff + adds version bumping + crates.io publishing), but overkill before crates.io publishing is unblocked

### 3. Version Source of Truth: `Cargo.toml` + git-cliff `--bump`

**`Cargo.toml` workspace version is the single source of truth.** No separate `VERSION` file.

**Rationale:**

- Cargo.toml is what `cargo build` reads, what cargo-dist reads, and what `env!("CARGO_PKG_VERSION")` embeds at compile time. It is the canonical version source in the Rust ecosystem.
- Cargo has **no `version = { file = "VERSION" }` syntax** ([issue #6583](https://github.com/rust-lang/cargo/issues/6583), open since 2019). A `VERSION` file would require a sync step that adds complexity and a failure mode for zero benefit.
- git-cliff `--bumped-version` computes the next version from conventional commits, eliminating the need for a human-maintained version file.
- For CI/shell scripting, the version is trivially extractable: `cargo metadata --format-version 1 | jq -r '.packages[] | select(.name=="ocx_cli") | .version'`, or via `grep` on `Cargo.toml`.

**Version consistency check:** A unit test in `ocx_cli` verifies that the compiled `CARGO_PKG_VERSION` is set to a real version (not the default `0.0.0`), catching the case where `version.workspace = true` is missing or the workspace version was never set:

```rust
#[test]
fn version_is_set() {
    let version = env!("CARGO_PKG_VERSION");
    assert!(!version.is_empty(), "CARGO_PKG_VERSION must not be empty");
    assert_ne!(version, "0.0.0", "workspace version must be set in Cargo.toml");
}
```

**Note:** The previous approach using `include_str!("../../Cargo.toml")` with `contains()` was rejected — it's fragile (matches any line containing the version string, including dependency versions). The simpler `assert_ne!` approach catches the most common failure mode (forgetting to set the version) without false positives.

### 4. Pre-Release (Canary) Workflow

Keep the existing `canary` tag pattern — a single overwritten pre-release — but enrich the release body with traceability metadata.

**Why NOT versioned pre-releases:**

git-cliff `--bump` derives the next version from commits since the last tag. This creates instability:

1. After `v0.5.0`, a `fix:` commit → `--bumped-version` returns `0.5.1`
2. A `feat:` commit lands → now `--bumped-version` returns `0.6.0`
3. Pre-releases built between these points have different base versions — confusing for anyone tracking them

Versioned pre-releases like `0.5.1-canary.20260312` are unstable by nature when the next version isn't predetermined. A single `canary` tag that always points to the latest build is simpler and honest about what it is: "latest from main, not a release."

**Updated canary flow:**

```
Developer triggers deploy-canary.yml (workflow_dispatch)
        |
        v
1. Build all 8 targets (same matrix as release)
2. Run unit + acceptance tests
3. Create/overwrite GitHub pre-release:
   - Tag: canary
   - Title: "Canary (main@{SHORT_SHA})"
   - Body: commit SHA, build date, git-cliff --unreleased (changes since last release tag)
   - Mark as pre-release, not latest
```

The release body provides full traceability (SHA, date, unreleased changelog) without polluting the tag namespace.

### 5. Install Script: Bootstrap OCX with Itself

**Decision: The install script bootstraps OCX, then uses OCX to install and manage itself.**

OCX is published to its own registry (`ocx.sh/ocx`) as part of the release pipeline (see Section 12). The install script downloads a temporary bootstrap binary, uses it to `ocx install ocx --select`, then discards the bootstrap. From that point on, OCX manages its own updates like any other package.

Instead of maintaining custom install scripts that duplicate OCX's own capabilities (platform detection, checksum verification, archive extraction), the install script is a thin bootstrap that hands off to OCX as quickly as possible.

**Install scripts** in `website/src/public/`:

| File | URL | Purpose |
|---|---|---|
| `website/src/public/install.sh` | `https://ocx.sh/install.sh` | Unix/macOS bootstrap |
| `website/src/public/install.ps1` | `https://ocx.sh/install.ps1` | Windows PowerShell bootstrap |

**OCX package directory structure requirement:**

The `~/.ocx/env` bootstrap file resolves the OCX binary at `$OCX_HOME/installs/ocx.sh/ocx/current/bin/ocx`. This means the OCX package archive **must** contain `bin/ocx` (or `bin/ocx.exe` on Windows) at its root. The release pipeline must package the binary inside a `bin/` directory, and the `metadata.json` must declare `PATH` pointing to `${installPath}/bin`. If the archive layout differs from this convention, the env file bootstrap breaks.

**Bootstrap flow (`install.sh`):**

```
1. Detect platform (uname -s / uname -m)
2. Download bootstrap binary from GitHub Releases → $TMPDIR/ocx
3. Verify SHA256 against sha256sum.txt
4. Run: $TMPDIR/ocx install ocx --select
5. Remove bootstrap binary
6. Create ~/.ocx/env (POSIX shell bootstrap file)
7. For Fish: create ~/.config/fish/conf.d/ocx.fish
8. Detect user's shell and append source line to profile (with existence check)
9. Honor --no-modify-path flag / OCX_NO_MODIFY_PATH=1 to skip steps 7-8
10. Print success message with instructions
```

**Step 6-7: Shell profile integration — use OCX to bootstrap its own environment:**

The install script creates `~/.ocx/env` — a thin shell script that locates the `ocx` binary and calls it to bootstrap its own environment. This is the OCX-native approach: instead of hardcoding PATH logic, the env file delegates to `ocx shell env` and `ocx shell completion`, which already know how to resolve packages, handle `OCX_HOME`, and generate shell-specific output.

**POSIX version (`~/.ocx/env`):**

```sh
#!/bin/sh
# OCX environment — generated by install.sh
# Source this from your shell profile: . "$HOME/.ocx/env"

# Static PATH export — the current symlink path never changes,
# only its target is re-pointed on update. Zero exec cost.
_ocx_home="${OCX_HOME:-$HOME/.ocx}"
export PATH="${_ocx_home}/installs/ocx.sh/ocx/current/bin:$PATH"

# Dynamic completions — loaded once per session, optional.
_ocx_bin="${_ocx_home}/installs/ocx.sh/ocx/current/bin/ocx"
if [ -x "$_ocx_bin" ]; then
  eval "$("$_ocx_bin" shell completion 2>/dev/null)" 2>/dev/null || true
fi

unset _ocx_home _ocx_bin
```

**Fish version (`~/.config/fish/conf.d/ocx.fish`):**

```fish
# OCX environment — generated by install.sh
set -l _ocx_home (set -q OCX_HOME; and echo $OCX_HOME; or echo $HOME/.ocx)

# Static PATH — the current symlink path is stable
fish_add_path --path "$_ocx_home/installs/ocx.sh/ocx/current/bin"

# Dynamic completions — loaded once per session, optional
set -l _ocx_bin "$_ocx_home/installs/ocx.sh/ocx/current/bin/ocx"
if test -x "$_ocx_bin"
  "$_ocx_bin" shell completion --shell fish 2>/dev/null | source
end
```

**Key design decisions:**

- **Static PATH, dynamic completions**: PATH uses a static export (zero exec cost on shell startup). Only completions invoke the binary — loaded lazily, failure-tolerant, one-time-per-session.
- **`OCX_HOME` respected**: The env file reads `$OCX_HOME` (defaulting to `~/.ocx`) to locate the binary — if the user has a custom `OCX_HOME`, it just works.
- **No regeneration needed**: The `current` symlink path is stable by design — only its target changes on update. The env file never needs to be rewritten.
- **`--current` symlink**: The env file depends on the `current` symlink being set (via `ocx install ocx --select` during bootstrap). If the symlink is missing, PATH just points to a non-existent directory — no error, no breakage, but `ocx` won't resolve.

**Shell startup performance — static PATH, dynamic completions:**

The `current` symlink path is stable by design — it never changes, only its target is re-pointed on update. OCX's own metadata declares a single env var (`PATH=${installPath}/bin`). This means the env file can use a **static export** for PATH (zero exec cost) and a **dynamic eval** only for completions (one-time-per-session, failure-tolerant).

This avoids the full `eval "$(ocx shell env ...)"` pattern that would fork+exec the OCX binary on every shell startup (~50-200ms estimated). Compare: rustup's `~/.cargo/env` is a static `export PATH="..."` — same approach.

Completions remain dynamic because they change across OCX versions and are loaded lazily by most shells. The `2>/dev/null || true` guard ensures a missing or broken binary doesn't block shell startup.

**Profile modification — append source line:**

The install script appends a single source line to the user's detected shell profile:

| Shell | Profile file | Line added |
|---|---|---|
| bash | `~/.bash_profile` (or `~/.profile`) | `. "$HOME/.ocx/env"` |
| zsh | `~/.zshenv` | `. "$HOME/.ocx/env"` |
| fish | (automatic via `conf.d/`) | (no profile edit needed) |

The script checks whether the source line already exists before appending — preventing duplicates on re-install. For Fish, the `conf.d/` directory is auto-sourced by Fish, so no profile edit is required.

**Why this pattern (rustup-style indirection + OCX-native bootstrap):**

- **Idempotent**: `ocx shell env` output is safe to eval multiple times; completions swallow errors
- **Single profile edit**: Only one line is added to the profile; all logic lives in `~/.ocx/env`
- **Updatable**: Future changes to environment logic happen in OCX itself — the env file is a stable wrapper
- **Discoverable**: `~/.ocx/env` is a readable file the user can inspect or source manually
- **`OCX_HOME`-aware**: Works with custom `OCX_HOME` without regeneration

**Opt-out:** The install script honors `--no-modify-path` (or `OCX_NO_MODIFY_PATH=1`) to skip profile modification. The `~/.ocx/env` file is still created; the user just needs to source it themselves.

**One-liner install commands:**
```bash
# Unix/macOS
curl -fsSL https://ocx.sh/install.sh | sh

# Windows PowerShell
irm https://ocx.sh/install.ps1 | iex
```

**Post-install output:**
```
ocx 0.5.0 installed successfully!

To get started, restart your shell or run:

  . ~/.ocx/env

Then verify with:

  ocx version
```

### 6. Release Artifact Format

**Decision: Compressed archives for all platforms.**

Every major Rust CLI tool (ripgrep, bat, fd, starship, delta, zoxide) ships compressed archives — not raw binaries. cargo-dist produces `.tar.xz` for Linux/macOS and `.zip` for Windows by default.

#### Why not raw binaries?

| Factor | Compressed archive | Raw binary |
|---|---|---|
| **Download size** | 3-4x smaller (mise: 21 MB `.tar.xz` vs 87 MB raw) | Full binary size |
| **Bundled extras** | Completions, LICENSE, README | Binary only |
| **Ecosystem tools** | `cargo-binstall`, `eget`, `ubi` expect archives | Require hints for raw binaries |
| **File permissions** | Preserved in tar | Require manual `chmod +x` |
| **Convention** | Universal in Rust ecosystem | Only mise ships both |
| **Install script** | `curl | tar xz` — well understood | Simpler `curl -o` but needs rename + chmod |

For OCX (a single binary, ~20-80 MB), `.tar.xz` achieves ~4x compression. The install script handles extraction transparently. Raw binaries add complexity to naming (the asset name includes the full triple) without meaningful benefit.

**Artifact naming:**
- Linux/macOS: `ocx-{target}.tar.xz` (e.g., `ocx-x86_64-unknown-linux-gnu.tar.xz`)
- Windows: `ocx-{target}.zip` (e.g., `ocx-x86_64-pc-windows-msvc.zip`)
- Checksums: `sha256sum.txt`

### 7. Changelog Strategy

#### 7a. `CHANGELOG.md` in repository root

Maintained in [keep-a-changelog](https://keepachangelog.com) format. git-cliff generates this from conventional commits:

```markdown
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2026-03-15

### Added
- New `ocx env` command for environment variable resolution
- Multi-platform support for `ocx install`

### Fixed
- Truncated downloads when using tokio::fs::File (#244)

## [0.4.0] - 2026-02-28

### Changed
- Refactored file structure to use content-addressed object store
```

**git-cliff configuration** (`cliff.toml` at repo root):

- Uses `keepachangelog` preset as the base template
- Maps conventional commit types: `feat` → Added, `fix` → Fixed, `refactor`/`perf` → Changed, etc.
- Filters out `chore`, `ci`, `docs`, `style`, `test` commits (noise reduction for end users)
- Groups by version tag
- `[bump]` section configures `initial_tag = "0.0.0"` for the first release

#### 7b. Per-release notes in GitHub Releases

The release workflow runs `git-cliff --latest --strip header` to extract only the current release's changes and pipes that into the GitHub Release body via `softprops/action-gh-release` or the `orhun/git-cliff-action` output.

Every GitHub Release includes a clean markdown summary of what changed.

#### 7c. Changelog on the website

Add a `Changelog` page to the VitePress site that includes `CHANGELOG.md` from the repo root.

**Approach:** VitePress supports `<!-- @include: path -->` syntax for file includes. Create `website/src/docs/changelog.md`:

```markdown
---
outline: deep
---

# Changelog

<!--@include: ../../../CHANGELOG.md{3,}-->
```

This includes `CHANGELOG.md` starting from line 3 (skipping the `# Changelog` header to avoid duplication). The changelog is always up-to-date with the repo because it's included at build time.

**Navigation:** Add to sidebar:

```typescript
// In config.mts sidebar
{
  text: "Changelog",
  link: "/docs/changelog",
}
```

### 8. Conventional Commit Enforcement

**Use [cocogitto](https://github.com/cocogitto/cocogitto) (1.1k stars, Rust-native) to validate all PR commits follow conventional commit format.**

#### Commit Linting Tool Comparison

| Feature | cocogitto | commitlint | git-cliff (repurposed) |
|---|---|---|---|
| **Stars** | 1.1k | 16k (Node) | 11.6k (not a validator) |
| **Runtime** | None (Rust binary) | Node.js | N/A |
| **Range validation** | `cog check base..head` | `--from base --to head` | Template hack only |
| **GitHub Action** | `cocogitto/cocogitto-action@v4` | `wagoid/commitlint-github-action@v6` | Not designed for this |
| **Rebase workflow** | Validates each commit individually | Validates each commit individually | N/A |

**Why cocogitto:**

- **Rust-native** — fits the project ecosystem, no Node.js in CI
- **Range validation** — `cog check base.sha..head.sha` validates exactly the commits in a PR, perfect for rebase-merge workflows where every commit lands individually on main
- **Single binary** — available as a musl-static binary, no compile step in CI

**CI workflow** (added to `verify-basic.yml`):

```yaml
conventional-commits:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
      with:
        fetch-depth: 0
        ref: ${{ github.event.pull_request.head.sha }}

    - name: Check conventional commits
      uses: cocogitto/cocogitto-action@v4
      with:
        command: check
        args: "${{ github.event.pull_request.base.sha }}..${{ github.event.pull_request.head.sha }}"
```

**Allowed commit types:** `feat`, `fix`, `refactor`, `perf`, `docs`, `test`, `ci`, `chore`, `style`, `build`, `revert`. Scopes are optional and freeform.

### 9. Dependabot with Conventional Commits

Configure dependabot to produce `chore(deps):` prefixed commit messages, which git-cliff filters out of the changelog. **Preserve the existing dependency groups** (`actions`, `rust-deps`) and add the npm ecosystem entry for the website.

```yaml
# .github/dependabot.yml
version: 2
updates:
  - package-ecosystem: "cargo"
    directory: "/"
    schedule:
      interval: "weekly"
    groups:
      rust-deps:
        patterns:
          - "*"
    commit-message:
      prefix: "chore(deps)"

  - package-ecosystem: "github-actions"
    directory: "/"
    schedule:
      interval: "weekly"
    groups:
      actions:
        patterns:
          - "*"
    commit-message:
      prefix: "chore(deps)"

  - package-ecosystem: "npm"
    directory: "/website"
    schedule:
      interval: "weekly"
    groups:
      npm-deps:
        patterns:
          - "*"
    commit-message:
      prefix: "chore(deps)"
```

This ensures dependency bumps pass the conventional commit check while being automatically excluded from user-facing changelogs (git-cliff skips `chore` commits). The existing `groups` configuration is preserved to batch updates into fewer PRs.

### 10. Automated Post-Release Version Bump

After a release tag is pushed, the release workflow automatically opens a PR that bumps `Cargo.toml` to the next development version.

**Flow:**

```
Release tag v0.5.0 pushed
        |
        v
release.yml runs → builds, publishes GitHub Release
        |
        v
Post-release job:
  1. git-cliff --bumped-version → next version
     (Edge case: immediately after tagging, there are zero unreleased commits.
      git-cliff with `features_always_bump_minor = true` defaults to a patch bump
      of the current version. The CI script must handle this: if bumped-version
      equals the just-released version, default to ${MAJOR}.${MINOR+1}.0.)
  2. cargo set-version $NEXT (via cargo-edit)
  3. Create PR: "chore: bump version to $NEXT"
  4. Human merges the PR
```

**Why a PR, not a direct push:**

- Follows the project convention of never pushing directly to main
- Human reviews the proposed next version (git-cliff's `--bump` may suggest patch when you want minor)
- The PR commit passes conventional commit checks (`chore:` prefix)

**CI check for version bump:** A workflow or test verifies that the `Cargo.toml` version on `main` is strictly greater than the latest release tag. This catches "forgot to merge the version bump PR" before the next release:

```yaml
- name: Check version is ahead of latest release
  run: |
    LATEST_TAG=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || echo "v0.0.0")
    TAG_VERSION="${LATEST_TAG#v}"
    CARGO_VERSION=$(cargo metadata --format-version 1 --no-deps | jq -r '.packages[] | select(.name=="ocx_cli") | .version')
    if [ "$CARGO_VERSION" = "$TAG_VERSION" ]; then
      echo "::warning::Cargo.toml version ($CARGO_VERSION) has not been bumped since last release ($LATEST_TAG)"
    fi
```

This is a warning, not a hard failure — there may be legitimate reasons for the version to match (e.g., right after release, before the bump PR is merged).

### 11. Update Strategy: OCX Updates Itself

**Decision: OCX manages its own updates via `ocx install ocx --select`. No external update crates needed.**

Since OCX is published to its own registry (Section 12) and the install script bootstraps OCX with itself (Section 5), updating is just another `ocx install`:

```bash
ocx install ocx --select          # install latest, set as current
ocx install ocx:0.6.0 --select    # install specific version
```

This is not a "future consideration" — it's the primary update mechanism from day one. The install script bootstraps the first copy; after that, OCX is a regular package in its own store.

**Why this works:**

- OCX's `current` symlink is what's on PATH (set up by `~/.ocx/env` from the install script)
- `ocx install ocx:NEW --select` installs a new version and re-points `current`
- The running binary is in the *old* object store path — the `current` symlink is updated atomically, not the running binary
- No `self-replace` dance needed — the next shell invocation picks up the new version via the symlink
- Works identically on Windows (symlink re-point, not binary replacement)

**Why not `update-informer`, `self_update`, or `self-replace`:**

| Crate | Why not |
|---|---|
| `update-informer` | Checks GitHub API — but OCX already has an index that tracks available versions. Adds a dependency for something OCX can do natively. |
| `self_update` | Downloads from GitHub Releases and replaces the running binary — but OCX already downloads from registries and manages versions. The binary replacement pattern is fragile (Windows locks, permission issues). |
| `self-replace` | Solves the running-binary-on-Windows problem — but the symlink pattern sidesteps it entirely. The running binary is never touched. |

Zero external dependencies for update functionality. OCX eats its own dog food.

#### Phase 1: Update Check Notification

On CLI invocation, check the local index for a newer OCX version and print a notice to stderr.

**Implementation (no external crates):**

```rust
// In CLI startup, after Context is built.
// Reuses the same version resolution path as `ocx version` / `ocx info`.
fn check_for_update(ctx: &Context) {
    let current = Version::parse(env!("CARGO_PKG_VERSION"));
    // Query the local index for ocx tags (already cached, no network)
    let tags = ctx.default_index().list_tags(&ocx_identifier);
    if let Some(latest) = tags.and_then(|t| t.latest_semver()) {
        if latest > current {
            eprintln!(
                "ocx {latest} available (current: {current}). \
                 Run: ocx install ocx:{latest} --select"
            );
        }
    }
}
```

**Implementation note:** The version comparison must reuse the same `CARGO_PKG_VERSION` reading used by `ocx version` and `ocx info` — a single shared function, not duplicated `env!()` calls across commands.

**Behavior:**
- Checks the **local index only** — zero network cost, instant
- After `ocx index update ocx`, the local index reflects the latest published version
- Output goes to stderr only — never pollutes stdout (safe for `ocx env` piped into `eval`)
- No background thread, no timeout, no cache file — the local index *is* the cache

**Disable conditions (checked in order):**

| Condition | Detection | Rationale |
|---|---|---|
| `OCX_NO_UPDATE_CHECK=true` | Env var (truthy values: `1`, `true`, `yes`) | Explicit user opt-out |
| `CI=true` | Env var | Standard CI detection (GitHub Actions, GitLab CI, CircleCI, Travis, Jenkins all set this) |
| `OCX_OFFLINE=true` | Env var | Offline mode — index may be stale, notification would be misleading |
| stderr is not a terminal | `!std::io::stderr().is_terminal()` | Piped/redirected output — don't inject noise into captured stderr |

**Environment variable: `OCX_NO_UPDATE_CHECK`**

| Variable | Purpose | Default |
|---|---|---|
| `OCX_NO_UPDATE_CHECK` | Disable update check notification | `false` |

Added to the environment variable reference table and `reference/environment.md`.

#### Considered: `ocx self update` Command

A dedicated `ocx self update` convenience command was considered but **deferred**. The rationale:

- Updating is already `ocx install ocx --select` — the same command as any other package. No special syntax needed.
- The `--remote` flag already exists as a global flag, so `ocx --remote install --select ocx` handles the "refresh index + install" flow.
- Adding a `self update` subcommand introduces a second way to do the same thing, which adds API surface without adding capability.
- If convenience becomes a user request, it can be added later as a trivial alias.

**Update workflow (no special command needed):**
```bash
ocx index update ocx          # refresh local index from registry
ocx install ocx --select      # install latest, re-point current symlink
# or combined:
ocx --remote install --select ocx
```

#### How other tools handle updates (comparison)

| Tool | Update mechanism | OCX advantage |
|---|---|---|
| **rustup** | `rustup self update` — custom code + atomic binary replacement | OCX: symlink re-point, no binary replacement needed |
| **mise** | `mise self-update` — downloads from GitHub Releases | OCX: uses its own registry, not GitHub API |
| **uv** | `uv self update` — axoupdater (cargo-dist receipts) | OCX: zero external dependencies |
| **starship/bat/ripgrep** | None — defer to package managers | OCX: self-manages, no package manager required |
| **Homebrew** | `brew upgrade` — git pull on own repo | OCX: `ocx install ocx --select` — same mechanism as any package |

### 12. Publishing OCX to Its Own Registry

**Decision: The release pipeline publishes OCX binaries to `ocx.sh/ocx` as an OCX package, in addition to GitHub Releases.**

This enables the self-bootstrapping install flow (Section 5) and the update-via-install mechanism (Section 11). OCX is treated as a first-class package in its own registry.

**Release pipeline addition** (after GitHub Release is published):

```yaml
publish-to-registry:
  needs: [host]
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4

    - name: Download release artifacts
      uses: actions/download-artifact@v4

    - name: Push to OCX registry
      run: |
        VERSION="${GITHUB_REF_NAME#v}"
        for archive in artifacts/ocx-*.tar.xz artifacts/ocx-*.zip; do
          PLATFORM=$(echo "$archive" | sed -E 's/.*ocx-(.*)\.(tar\.xz|zip)/\1/' | \
            sed 's/x86_64-unknown-linux-gnu/linux\/amd64/' | \
            sed 's/aarch64-unknown-linux-gnu/linux\/arm64/' | \
            # ... mapping for all 8 targets
          )
          ocx package push "ocx.sh/ocx:${VERSION}" "$archive" \
            --platform "$PLATFORM" --cascade
        done
      env:
        OCX_AUTH_ocx_sh_USERNAME: ${{ secrets.OCX_REGISTRY_USERNAME }}
        OCX_AUTH_ocx_sh_PASSWORD: ${{ secrets.OCX_REGISTRY_PASSWORD }}
```

**Package metadata** (`metadata.json` for the OCX package):

```json
{
  "env": [
    {
      "key": "PATH",
      "value": "${installPath}/bin",
      "type": "path"
    }
  ]
}
```

This ensures `ocx shell env ocx --current` produces the correct PATH export.

**Tag convention:** OCX follows its own cascade convention — `ocx:0.5.0+build.1` (build-tagged), plus `ocx:0.5.0`, `ocx:0.5`, `ocx:0`, `ocx:latest` via `--cascade`.

### 13. Documentation Updates

The release process must update user-facing documentation to reflect the new installation methods.

#### 13a. Website Installation Page (`website/src/docs/installation.md`)

Update the existing installation page to document all available install methods:

```markdown
## Quick Install

### macOS / Linux
```bash
curl -fsSL https://ocx.sh/install.sh | sh
```

### Windows (PowerShell)
```powershell
irm https://ocx.sh/install.ps1 | iex
```

## GitHub Releases

Download pre-built binaries directly from [GitHub Releases](https://github.com/ocx-sh/ocx/releases/latest).

| Platform | Architecture | Download |
|---|---|---|
| Linux | x86_64 | `ocx-x86_64-unknown-linux-gnu.tar.xz` |
| Linux | aarch64 | `ocx-aarch64-unknown-linux-gnu.tar.xz` |
| Linux (static) | x86_64 | `ocx-x86_64-unknown-linux-musl.tar.xz` |
| Linux (static) | aarch64 | `ocx-aarch64-unknown-linux-musl.tar.xz` |
| macOS | Apple Silicon | `ocx-aarch64-apple-darwin.tar.xz` |
| macOS | Intel | `ocx-x86_64-apple-darwin.tar.xz` |
| Windows | x86_64 | `ocx-x86_64-pc-windows-msvc.zip` |
| Windows | ARM64 | `ocx-aarch64-pc-windows-msvc.zip` |

## Updating

OCX manages its own updates:
```bash
ocx index update ocx          # refresh local index
ocx install ocx --select      # install latest, set as current
```

Or re-run the install script:
```bash
curl -fsSL https://ocx.sh/install.sh | sh
```

## Verify Installation

```bash
ocx version
ocx info
```

## Uninstalling

To completely remove OCX:
```bash
rm -rf ~/.ocx                         # remove all data, packages, and index
```

Then remove the source line from your shell profile:
- **bash**: remove `. "$HOME/.ocx/env"` from `~/.bash_profile` or `~/.profile`
- **zsh**: remove `. "$HOME/.ocx/env"` from `~/.zshenv`
- **fish**: remove `~/.config/fish/conf.d/ocx.fish`
```

#### 13b. Repository README

Add an installation section to `README.md` with the quick-install one-liners and a link to the full installation page:

```markdown
## Installation

```bash
# macOS / Linux
curl -fsSL https://ocx.sh/install.sh | sh

# Windows (PowerShell)
irm https://ocx.sh/install.ps1 | iex
```

See the [installation guide](https://ocx.sh/docs/installation) for all options.
```

### 14. Website Auto-Deploy on Release

**Decision: Trigger a website deployment automatically after each release.**

The current `deploy-website.yml` is manual (`workflow_dispatch`). After a release, the website must be redeployed because:

1. The changelog page includes `CHANGELOG.md` via VitePress `@include` — it must reflect the new release
2. The installation page may reference version-specific download URLs
3. Install scripts in `website/src/public/` query the GitHub API for the latest release, but any version references in docs need to be current

**Implementation:** Add a `workflow_call` trigger to `deploy-website.yml` and call it from the release workflow:

```yaml
# deploy-website.yml — add trigger
on:
  workflow_dispatch:  # keep manual trigger
  workflow_call:      # allow other workflows to trigger it
```

```yaml
# release.yml — add post-release step
deploy-website:
  needs: [host]  # after GitHub Release is published
  uses: ./.github/workflows/deploy-website.yml
  secrets: inherit
```

**Alternative: trigger on release event.** Instead of `workflow_call`, add a `release` trigger to `deploy-website.yml`:

```yaml
on:
  workflow_dispatch:
  release:
    types: [published]
```

This is simpler (no coupling between workflows) but triggers on *any* release including pre-releases. Add a condition to skip canary:

```yaml
jobs:
  deploy:
    if: ${{ !github.event.release.prerelease }}
```

**Recommendation:** Use the `release: published` trigger with the pre-release guard. It's declarative, self-documenting, and doesn't require modifying the release workflow.

---

## Architecture

### Target Matrix

The existing `deploy-canary.yml` already builds 8 targets. For the release pipeline, **keep all 8 targets**:

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

**cargo-dist limitation**: `aarch64-unknown-linux-musl` cross-compilation is **broken in cargo-dist** ([issue #1581](https://github.com/axodotdev/cargo-dist/issues/1581), open since Nov 2024). Keep `actions-rust-cross` for this target and `aarch64-pc-windows-msvc`. Start cargo-dist with 6 targets; add custom jobs for the remaining 2.

### Release Trigger Flow

```
Developer decides to release
        |
        v
1. Run `git-cliff --bumped-version` to see proposed next version (e.g., "0.5.0")
2. Run `git-cliff --bump --unreleased` to preview changelog
3. Run `cargo set-version 0.5.0` (if not already set from post-release bump)
4. Run `git-cliff --bump -o CHANGELOG.md` to update changelog with new version header
5. Commit: "release: v0.5.0"
6. Tag: git tag v0.5.0
7. Push tag: git push origin v0.5.0
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
        ├── Run git-cliff --latest → release notes markdown
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
        │     sha256sum.txt
        │     Release notes (from git-cliff)
        │
        ├── Post-release: open PR to bump version for next cycle
        │
        └── Announce: finalize GitHub Release

  * targets where cargo-dist has known cross-compilation issues;
    add custom build jobs using actions-rust-cross.
```

### Pre-Release Verification Workflow

Repurpose the existing canary into a **verify-release workflow** that acts as a manual gate before tagging:

| Aspect | deploy-canary.yml | release.yml (cargo-dist) |
|---|---|---|
| Trigger | `workflow_dispatch` (manual) | Tag push (`v*`) |
| Tests | Full: unit + acceptance + cross-build | Build only (no tests) |
| Artifacts | GitHub pre-release (`canary` tag, overwritten) | GitHub Release (permanent, versioned) |
| Release notes | `git-cliff --unreleased` | `git-cliff --latest` |
| Purpose | Validate before release + provide latest build | Produce the release |

### Version Flow Summary

```
Cargo.toml [workspace.package] version (source of truth)
    │
    ├──→ env!("CARGO_PKG_VERSION") ──→ compiled into binary
    │
    ├──→ cargo-dist reads it for release
    │
    ├──→ Canary: builds whatever version is in Cargo.toml, tags as `canary`
    │
    ├──→ Release: verifies Cargo.toml version matches pushed tag
    │
    └──→ Post-release: git-cliff --bumped-version → cargo set-version → PR
```

**Release ceremony** (Taskfile command):

```bash
task release:prepare    # 1. git-cliff --bump -o CHANGELOG.md
                        # 2. cargo set-version $(git-cliff --bumped-version)
                        # 3. task verify
                        # 4. Print: "Ready to commit, tag, and push"
```

---

## Implementation Plan

> **Implementation rules:** See `.claude/rules/release-implementation.md` for detailed constraints, cross-references, and gotchas that apply across all phases.
>
> **Documentation rules:** All website pages must follow `.claude/rules/documentation.md` (narrative structure, headers, real-world examples, link syntax).
>
> **CI patterns:** Follow existing workflow patterns in `.github/workflows/`. SHA-pin all third-party actions.

### Phase 1: Version and Changelog Infrastructure

1. **Add version to workspace Cargo.toml**:
   ```toml
   [workspace.package]
   version = "0.1.0"
   ```
   And in each crate (`ocx_cli/Cargo.toml`, `ocx_lib/Cargo.toml`): add `version.workspace = true`

2. **Add version guard test** in `ocx_cli`:
   ```rust
   #[test]
   fn version_is_set() {
       let version = env!("CARGO_PKG_VERSION");
       assert!(!version.is_empty(), "CARGO_PKG_VERSION must not be empty");
       assert_ne!(version, "0.0.0", "workspace version must be set in Cargo.toml");
   }
   ```

3. **Extract shared version utility**: Create a function in `ocx_cli` that returns the compiled version string (used by `version`, `info`, and the future update check). Currently `version.rs` calls `env!("CARGO_PKG_VERSION")` directly — extract this to a single shared function to avoid duplication.

4. **Install and configure git-cliff** — create `cliff.toml`:
   ```toml
   [changelog]
   header = """
   # Changelog\n
   All notable changes to this project will be documented in this file.\n
   The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
   and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).\n
   """
   body = """
   ## [{{ version | trim_start_matches(pat="v") }}] - {{ timestamp | date(format="%Y-%m-%d") }}
   {% for group, commits in commits | group_by(attribute="group") %}
   ### {{ group | upper_first }}
   {% for commit in commits %}
   - {{ commit.message | split(pat="\n") | first | trim }}\
     {% if commit.scope %} *({{ commit.scope }})*{% endif %}\
     {% if commit.breaking %} **BREAKING**{% endif %}
   {%- endfor %}
   {% endfor %}
   """
   trim = true

   [git]
   conventional_commits = true
   filter_unconventional = true
   commit_parsers = [
     { message = "^feat", group = "Added" },
     { message = "^fix", group = "Fixed" },
     { message = "^refactor", group = "Changed" },
     { message = "^perf", group = "Changed" },
     { message = "^doc", group = "Documentation" },
     { message = "^chore", skip = true },
     { message = "^ci", skip = true },
     { message = "^style", skip = true },
     { message = "^test", skip = true },
     { message = "^build", skip = true },
   ]
   tag_pattern = "v[0-9].*"

   [bump]
   initial_tag = "0.0.0"
   features_always_bump_minor = true
   breaking_always_bump_major = false  # stay pre-1.0 until intentional
   ```

5. **Generate initial `CHANGELOG.md`** from git history:
   ```bash
   git-cliff --output CHANGELOG.md
   ```

6. **Add Taskfile commands**:
   ```yaml
   changelog:
     desc: Update CHANGELOG.md from git history
     cmds:
       - git-cliff --output CHANGELOG.md

   changelog:preview:
     desc: Preview unreleased changes
     cmds:
       - git-cliff --unreleased

   release:prepare:
     desc: Prepare a release (compute version, update changelog, verify)
     cmds:
       - |
         NEXT=$(git-cliff --bumped-version)
         echo "Next version: $NEXT"
         cargo set-version "${NEXT#v}"
       - git-cliff --bump -o CHANGELOG.md
       - task verify
       - echo "Ready to commit, tag, and push"
   ```

### Phase 2: Conventional Commit Enforcement

7. **Add cocogitto check to `verify-basic.yml`** — validates all commits in PRs:
   ```yaml
   conventional-commits:
     runs-on: ubuntu-latest
     if: github.event_name == 'pull_request'
     steps:
       - uses: actions/checkout@v4
         with:
           fetch-depth: 0
           ref: ${{ github.event.pull_request.head.sha }}
       - uses: cocogitto/cocogitto-action@v4
         with:
           command: check
           args: "${{ github.event.pull_request.base.sha }}..${{ github.event.pull_request.head.sha }}"
   ```

8. **Configure dependabot with conventional commits** — update `.github/dependabot.yml`:
   Preserve existing `groups` (`actions`, `rust-deps`), add `commit-message.prefix: "chore(deps)"` to each ecosystem, and add the `npm` ecosystem for `/website`. See Section 9 for full config.

### Phase 3: Website Changelog

9. **Create `website/src/docs/changelog.md`**:
    ```markdown
    ---
    outline: deep
    ---

    # Changelog

    <!--@include: ../../../CHANGELOG.md{3,}-->
    ```

10. **Add to VitePress sidebar** in `config.mts`:
    ```typescript
    {
      text: "Changelog",
      link: "/docs/changelog",
    }
    ```

### Phase 4: Install Scripts on Website (Bootstrap with OCX)

11. **Create `website/src/public/install.sh`** — bootstrap installer:
    - Detect OS/arch via `uname -s` / `uname -m`
    - Map to Rust target triple (e.g., `Linux` + `x86_64` → `x86_64-unknown-linux-gnu`)
    - Query GitHub API (`https://api.github.com/repos/ocx-sh/ocx/releases/latest`) for asset URLs
    - Download correct `.tar.xz` + `sha256sum.txt` to `$TMPDIR`
    - Verify SHA256: `sha256sum --check` (or `shasum -a 256` on macOS)
    - Extract bootstrap binary from archive
    - Run `$TMPDIR/ocx install ocx --select` (OCX installs itself into its own store)
    - Remove bootstrap binary from `$TMPDIR`
    - Create `~/.ocx/env` (POSIX shell bootstrap file — see Section 5 for content)
    - For Fish: create `~/.config/fish/conf.d/ocx.fish` (see Section 5 for content)
    - Detect user's shell (`$SHELL`) and append `. "$HOME/.ocx/env"` to profile:
      - bash: `~/.bash_profile` (fallback `~/.profile`)
      - zsh: `~/.zshenv`
      - fish: no profile edit needed (`conf.d/` is auto-sourced)
    - Check if source line already exists before appending (prevent duplicates on re-install)
    - Honor `--no-modify-path` flag / `OCX_NO_MODIFY_PATH=1` env var to skip profile modification
    - Print success message with "restart your shell or run `. ~/.ocx/env`"
    - **Error handling**: Clear error messages for: curl/wget not found, GitHub API unreachable, checksum mismatch, unsupported platform, `ocx install` failure

12. **Create `website/src/public/install.ps1`** — PowerShell bootstrap equivalent:
    - Detect architecture via `[System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture`
      (fallback: `$env:PROCESSOR_ARCHITECTURE`)
    - Map to target: `AMD64` → `x86_64-pc-windows-msvc`, `ARM64` → `aarch64-pc-windows-msvc`
    - Download `.zip` + `sha256sum.txt` via `Invoke-WebRequest`
    - Verify SHA256 via `Get-FileHash`
    - Extract with `Expand-Archive`
    - Run `$TMPDIR\ocx.exe install ocx --select`
    - Remove bootstrap files
    - Create `$env:OCX_HOME\env.ps1` (PowerShell bootstrap script):
      ```powershell
      # OCX environment — generated by install.ps1
      # Static PATH — the current symlink path is stable
      $OcxHome = if ($env:OCX_HOME) { $env:OCX_HOME } else { "$env:USERPROFILE\.ocx" }
      $OcxBin = Join-Path $OcxHome "installs\ocx.sh\ocx\current\bin"
      if ($env:PATH -notlike "*$OcxBin*") {
          $env:PATH = "$OcxBin;$env:PATH"
      }
      # Dynamic completions — optional
      $OcxExe = Join-Path $OcxBin "ocx.exe"
      if (Test-Path $OcxExe) {
          try { & $OcxExe shell completion --shell powershell 2>$null | Invoke-Expression } catch {}
      }
      ```
    - Modify `$PROFILE.CurrentUserCurrentHost`:
      - Create profile file if it doesn't exist (`New-Item -Force`)
      - Append `. "$env:USERPROFILE\.ocx\env.ps1"` with existence check
    - Honor `OCX_NO_MODIFY_PATH=1` env var to skip profile modification
    - Print success message

13. **Test install scripts in CI** — add to canary workflow or as a separate job:
    - `install.sh`: test in a clean Docker container (`ubuntu:latest`, `alpine:latest`)
    - `install.ps1`: test on `windows-latest` runner
    - Verify: binary resolves on PATH, `ocx version` outputs expected version
    - This can be a manual workflow initially, automated later

### Phase 5: Release Pipeline (cargo-dist)

14. **Initialize cargo-dist**:
    ```bash
    cargo install cargo-dist
    cargo dist init
    ```
    Select: GitHub CI only. **No installers** (OCX uses custom install scripts). **No Homebrew** (deferred).

15. **Configure targets** in `dist-workspace.toml`:
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
    # No installers — OCX uses custom bootstrap scripts (see Section 5)
    # installers = []
    unix-archive = ".tar.xz"
    windows-archive = ".zip"
    ```

    **Note:** `aarch64-unknown-linux-musl` and `aarch64-pc-windows-msvc` have known cargo-dist cross-compilation issues ([#1581](https://github.com/axodotdev/cargo-dist/issues/1581)). Add custom build jobs using `actions-rust-cross` for these targets alongside the cargo-dist pipeline.

16. **Add git-cliff step to release workflow** — after build, before GitHub Release creation:
    ```yaml
    - name: Generate release notes
      uses: orhun/git-cliff-action@v4
      with:
        config: cliff.toml
        args: --latest --strip header
      env:
        OUTPUT: RELEASE_NOTES.md

    - name: Create GitHub Release
      uses: softprops/action-gh-release@v2
      with:
        body_path: RELEASE_NOTES.md
    ```

17. **Add version verification** to release workflow:
    ```yaml
    - name: Verify Cargo.toml version matches tag
      run: |
        TAG_VERSION="${GITHUB_REF_NAME#v}"
        CARGO_VERSION=$(cargo metadata --format-version 1 --no-deps \
          | jq -r '.packages[] | select(.name=="ocx_cli") | .version')
        if [ "$TAG_VERSION" != "$CARGO_VERSION" ]; then
          echo "ERROR: Cargo.toml version ($CARGO_VERSION) does not match tag ($TAG_VERSION)"
          exit 1
        fi
    ```

18. **Add post-release version bump job**:
    ```yaml
    post-release:
      needs: [host]
      runs-on: ubuntu-latest
      steps:
        - uses: actions/checkout@v4
          with:
            fetch-depth: 0
        - name: Compute next version
          run: |
            RELEASED="${GITHUB_REF_NAME#v}"
            NEXT=$(git-cliff --bumped-version)
            NEXT="${NEXT#v}"
            # Edge case: if git-cliff returns the same version (no unreleased commits),
            # default to bumping the minor version
            if [ "$NEXT" = "$RELEASED" ]; then
              IFS='.' read -r MAJOR MINOR PATCH <<< "$RELEASED"
              NEXT="${MAJOR}.$((MINOR + 1)).0"
            fi
            echo "NEXT_VERSION=$NEXT" >> $GITHUB_ENV
            cargo set-version "$NEXT"
        - name: Create version bump PR
          uses: peter-evans/create-pull-request@v6
          with:
            title: "chore: bump version to ${{ env.NEXT_VERSION }}"
            branch: chore/bump-version
            commit-message: "chore: bump version to ${{ env.NEXT_VERSION }}"
    ```

### Phase 6: Canary Enrichment

19. **Enrich canary release body** in `deploy-canary.yml`:
    - Add `git-cliff --unreleased` step to generate changelog since last release tag
    - Update the `softprops/action-gh-release` step to include:
      - Commit SHA
      - Build date
      - Unreleased changelog from git-cliff
    - Update title to `"Canary (main@{SHORT_SHA})"`

### Phase 7: Documentation and Website

20. **Update `website/src/docs/installation.md`** with all install methods:
    - Quick install one-liners (shell + PowerShell)
    - GitHub Releases download table (all 8 targets)
    - Updating instructions (both `ocx install ocx --select` and re-run install script)
    - Uninstall instructions (remove `~/.ocx`, remove profile source line)
    - Verification steps (`ocx version`, `ocx info`)
    - Follow `.claude/rules/documentation.md` for narrative structure, headers, links

21. **Update `README.md`** with installation section:
    - Quick install one-liners
    - Link to full installation guide on website

22. **Add `release: published` trigger to `deploy-website.yml`**:
    ```yaml
    on:
      workflow_dispatch:
      release:
        types: [published]

    jobs:
      deploy:
        if: ${{ !github.event.release.prerelease }}
        # ... existing deployment steps
    ```
    This auto-deploys the website after each stable release, ensuring the changelog page and install docs are current.

### Phase 8: Update Notification and Registry Publishing

23. **Add update check on CLI startup** (in `main.rs` or app initialization):
    - Reuse the shared version function from Phase 1 step 3
    - Query local index for `ocx.sh/ocx` tags (zero network, instant)
    - Compare latest semver tag with `CARGO_PKG_VERSION`
    - Print to stderr: `ocx {latest} available. Run: ocx install ocx:{latest} --select`
    - Disabled when: `OCX_NO_UPDATE_CHECK=true`, `CI=true`, `OCX_OFFLINE=true`, or stderr is not a terminal

24. **Add `OCX_NO_UPDATE_CHECK` and `OCX_NO_MODIFY_PATH` to documentation**:
    - Add both to `CLAUDE.md` environment variable table
    - Add both to `website/src/docs/reference/environment.md`
    - Use the same truthy-value parsing as `OCX_OFFLINE` and `OCX_REMOTE`

25. **Publish OCX to `ocx.sh/ocx`** as part of the release pipeline (Section 12):
    - Add `publish-to-registry` job to `release.yml`
    - Push all 8 platform binaries with `--cascade`
    - Create OCX package `metadata.json` with PATH env declaration
    - **Important**: The archive must contain `bin/ocx` at root to match the `~/.ocx/env` bootstrap path

### Phase 9: CI Hardening

26. **Add version-ahead-of-release check** to `verify-basic.yml` (warning, not failure):
    ```yaml
    - name: Check version bumped since last release
      if: github.ref == 'refs/heads/main'
      run: |
        LATEST_TAG=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || echo "v0.0.0")
        TAG_VERSION="${LATEST_TAG#v}"
        CARGO_VERSION=$(cargo metadata --format-version 1 --no-deps \
          | jq -r '.packages[] | select(.name=="ocx_cli") | .version')
        if [ "$CARGO_VERSION" = "$TAG_VERSION" ]; then
          echo "::warning::Cargo.toml version ($CARGO_VERSION) has not been bumped since $LATEST_TAG"
        fi
    ```

---

## Trade-off Analysis

### Release Tool

| Dimension | cargo-dist | Custom GH Actions | GoReleaser |
|---|---|---|---|
| **Setup effort** | Low (`dist init`) | High (write YAML) | Medium (config file) |
| **Maintenance** | Auto-updated via `dist init` | Manual | Manual |
| **Rust support** | Native, first-class | Generic | Generic |
| **Install scripts** | Available but not used (OCX custom) | Must write from scratch | Shell only |
| **Flexibility** | Config-driven, extensible | Unlimited | Config-driven |
| **Community** | Growing (2k stars) | N/A | Large (14k stars, Go-focused) |

### Changelog Tool

| Dimension | git-cliff | towncrier | changie | release-plz |
|---|---|---|---|---|
| **Stars** | 11.6k | 887 | 865 | 1.3k |
| **Runtime** | None (Rust binary) | Python | Go binary | Rust binary |
| **Keep-a-changelog** | Built-in template | Custom template | Default | Via git-cliff |
| **Rust ecosystem** | Native | Foreign | Neutral | Native |
| **Version bumping** | `--bump` / `--bumped-version` | No | No | Via git-cliff |
| **Contributor overhead** | Conventional commits | Fragment files | Fragment files | Conventional commits |
| **GitHub Release notes** | Official action | Manual | Action | Built-in |
| **Upgrade path** | → release-plz wraps it | — | — | Final form |

### Version Source of Truth

| Approach | Pros | Cons |
|---|---|---|
| **`Cargo.toml`** (selected) | Native to Rust; cargo-dist reads it; `env!()` works at compile time; no sync step | Slightly harder to read from shell (need `jq` or `grep`) |
| **`VERSION` file** | Trivially readable from any context | Must sync to Cargo.toml; Cargo has no `file = "VERSION"` syntax; extra maintenance |
| **Git tags only** | No file to maintain | Can't derive version before tagging; canaries need a base version |
| **git-cliff `--bumped-version`** | Zero files to maintain; automatic | Unstable between releases (changes as commits land); not suitable as persistent source of truth |

### Commit Linting Tool

| Dimension | cocogitto | commitlint | git-cliff (repurposed) |
|---|---|---|---|
| **Runtime** | None (Rust binary) | Node.js | N/A |
| **Range validation** | `cog check base..head` | `--from base --to head` | Template hack only |
| **Ecosystem fit** | Rust-native | Foreign | Not a validator |
| **Stars** | 1.1k | 16k | 11.6k |
| **Maintenance** | Active | Very active | N/A |

### Artifact Format

| Format | Size (typical Rust CLI) | Ecosystem convention | Extras bundled | Install complexity |
|---|---|---|---|---|
| **`.tar.xz`** (selected for Unix) | ~4x smaller than raw | Universal for Rust tools | Yes (completions, LICENSE) | `tar xJf` |
| **`.tar.gz`** | ~2.5x smaller than raw | Common alternative | Yes | `tar xzf` |
| **`.zip`** (selected for Windows) | ~3x smaller than raw | Universal on Windows | Yes | `Expand-Archive` |
| **Raw binary** | Full size (e.g., 87 MB for mise) | Only mise ships both | No | `curl -o && chmod +x` |

### Update Strategy

| Approach | Complexity | User impact | Fit for backend tool |
|---|---|---|---|
| **Self-hosting: `ocx install ocx`** (selected) | Low (zero new deps) | Natural — same UX as any package | Excellent — CI uses the same command |
| **`update-informer` + `self_update` crates** | Medium (~100 lines + 3 deps) | Different UX from regular packages | Good but inconsistent |
| **Automatic background update** | High | Breaks reproducibility | Poor — violates CI expectations |
| **No update mechanism** | None | Users must know to re-run install script | Acceptable but suboptimal |

### Install Script Strategy

| Approach | Complexity | Self-update | Shell integration |
|---|---|---|---|
| **Bootstrap with OCX** (selected) | Medium (bootstrap + `ocx install`) | Built-in (`ocx install ocx`) | `~/.ocx/env` + profile source line |
| **Standalone installer** | Low (download + extract + PATH) | Requires re-running installer | Manual PATH instructions |
| **cargo-dist installer** | Low (generated) | axoupdater (cargo-dist receipts) | cargo-dist manages receipt |

**Note:** cargo-dist's generated installers are **not used** because they install to a flat directory and know nothing about OCX's three-store architecture. OCX uses custom bootstrap scripts that delegate to `ocx install ocx --select`.

### Shell Profile Auto-Configuration

| Approach | UX | Safety | Precedent |
|---|---|---|---|
| **Auto-modify with opt-out** (selected) | Best — works immediately | Existence check prevents duplicates; `--no-modify-path` opt-out | rustup, Homebrew, nvm |
| **Print instructions only** | Safe but friction | User may forget or mistype | starship, fnm |
| **Interactive prompt** | Moderate | User decides | No major tools do this |
| **`ocx shell setup` command** | Flexible but extra step | Explicit | mise (partial) |

---

## Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| cargo-dist breaking changes | CI breaks on update | Pin cargo-dist version, test before updating |
| cargo-dist cross-compilation differs from `actions-rust-cross` | Missing or incompatible binaries | Keep `actions-rust-cross` for problematic targets; compare output |
| Conventional commit discipline lapses | Noisy/empty changelogs | cocogitto enforces in CI; PR check blocks merge |
| Post-release version bump PR not merged | Next release ceremony is confusing | CI warning on main when version matches latest tag |
| Install script maintenance burden | Broken installs on new platforms | Test install scripts in CI against all platforms; keep scripts minimal |
| Dependabot PRs fail commit check | Dependency updates blocked | `chore(deps)` prefix configured; test in first Dependabot cycle |
| Website not updated after release | Stale changelog/install docs | `release: published` trigger auto-deploys; pre-release guard prevents canary deploys |
| Update notification in CI environments | Noise in CI logs | Disabled when `CI=true`, `OCX_NO_UPDATE_CHECK=true`, or stderr is not a terminal |
| Self-hosting chicken-and-egg: OCX must exist to install OCX | Bootstrap fails | Install script downloads bootstrap binary from GitHub Releases first; OCX registry is only used after bootstrap |
| Shell profile modification on install | Unwanted changes to dotfiles | Existence check prevents duplicates; `--no-modify-path` / `OCX_NO_MODIFY_PATH=1` opt-out; indirection via `~/.ocx/env` keeps profile changes minimal |
| OCX registry (`ocx.sh`) unavailable | `ocx install ocx --select` fails | GitHub Releases remains the fallback; install script always uses GitHub Releases for bootstrap; re-run install script as escape hatch |
| Wrong shell profile detected | PATH not set on next login | Installer logs which file it modified; `--no-modify-path` + manual instructions as escape hatch |
| OCX package archive layout mismatch | `~/.ocx/env` bootstrap fails to find binary | Release pipeline must produce archives with `bin/ocx` at root; test in CI; document the required layout |
| Completions eval on shell startup | Minor shell startup latency from `ocx shell completion` | Static PATH (zero cost); only completions are dynamic and failure-tolerant (`2>/dev/null || true`) |

---

## References

- [cargo-dist documentation](https://opensource.axo.dev/cargo-dist/book/)
- [cargo-dist GitHub](https://github.com/axodotdev/cargo-dist) (~2,000 stars, v0.31.0, Feb 2026)
- [cargo-dist archives docs](https://axodotdev.github.io/cargo-dist/book/artifacts/archives.html)
- [git-cliff documentation](https://git-cliff.org/)
- [git-cliff GitHub](https://github.com/orhun/git-cliff) (~11,600 stars, v2.12.0, Jan 2026)
- [git-cliff `--bump` documentation](https://git-cliff.org/docs/usage/bump-version/)
- [git-cliff `[bump]` configuration](https://git-cliff.org/docs/configuration/bump/)
- [git-cliff GitHub Action](https://github.com/orhun/git-cliff-action)
- [cocogitto GitHub](https://github.com/cocogitto/cocogitto) (~1,100 stars, Rust-native conventional commit toolbox)
- [cocogitto GitHub Action](https://github.com/cocogitto/cocogitto-action)
- [keep-a-changelog](https://keepachangelog.com/en/1.1.0/)
- [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
- [cross-rs](https://github.com/cross-rs/cross) (~8,000 stars, Docker-based cross-compilation)
- [release-plz](https://github.com/release-plz/release-plz) (~1,300 stars, future consideration)
- [cargo-edit (`cargo set-version`)](https://crates.io/crates/cargo-edit)
- [Automated Rust Releases (Orhun's blog)](https://blog.orhun.dev/automated-rust-releases/)
- [Rust Platform Support](https://doc.rust-lang.org/rustc/platform-support.html)
- [Dependabot commit-message configuration](https://docs.github.com/en/code-security/dependabot/dependabot-version-updates/configuration-options-for-the-dependabot.yml-file#commit-message)
- [update-informer](https://github.com/mgrachev/update-informer) (224 stars, version check notification crate)
- [self_update](https://github.com/jaemk/self_update) (927 stars, self-update library for Rust)
- [self-replace](https://github.com/mitsuhiko/self-replace) (89 stars, atomic binary replacement by Armin Ronacher)
- [axoupdater](https://github.com/axodotdev/axoupdater) (28 stars, cargo-dist updater — not applicable without cargo-dist receipts)
- [cargo-dist updater docs](https://axodotdev.github.io/cargo-dist/book/installers/updater.html)

### Install Script Precedents

| Tool | Shell | PowerShell | Install Location |
|---|---|---|---|
| rustup | `curl https://sh.rustup.rs \| sh` | N/A | `~/.cargo/bin` |
| Deno | `curl -fsSL https://deno.land/install.sh \| sh` | `irm https://deno.land/install.ps1 \| iex` | `~/.deno/bin` |
| Bun | `curl -fsSL https://bun.sh/install \| bash` | `irm bun.sh/install.ps1 \| iex` | `~/.bun/bin` |
| mise | `curl https://mise.run \| sh` | N/A | `~/.local/bin` |
| starship | `curl -sS https://starship.rs/install.sh \| sh` | N/A | `/usr/local/bin` |

OCX install path: **`~/.ocx/installs/ocx.sh/ocx/current/bin/ocx`** (resolved via `current` symlink into the content-addressed object store). The `~/.ocx/env` bootstrap file locates the binary at this stable symlink path.
