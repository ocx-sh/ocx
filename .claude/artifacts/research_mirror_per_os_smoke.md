# Research: Repo-per-Mirror + Per-OS Pre-Publish Smoke Test Patterns

**Date:** 2026-05-12
**Author:** worker-researcher
**Scope:** Informs OCX design decision on per-mirror CI smoke testing before OCI registry push. Covers repo-per-mirror CI generation patterns, per-OS execution strategies (QEMU/Wine/Rosetta), smoke test taxonomy, and breakage reporting.
**Context:** OCX mirrors upstream binaries (CMake, Node, gh CLI, etc.) into OCI registries. Each `mirror.yml` declares multi-platform asset patterns. We want per-platform smoke tests before publish, with no staging registry.

---

## Pattern 1: conda-forge Feedstocks (One-Repo-Per-Package + Rendered CI)

**Model:** One GitHub repo per package (`<pkg>-feedstock`). Maintainer owns only `recipe/meta.yaml` (the recipe) and `conda-forge.yml` (build config). All CI files — `.github/workflows/`, `.ci_support/`, `.scripts/` — are **generated** by `conda-smithy` and must never be edited by hand.

**Renderer workflow:**
1. Maintainer edits `recipe/meta.yaml` or `conda-forge.yml`
2. Triggers rerender via bot comment `@conda-forge-admin, please rerender` or local `conda smithy rerender`
3. `conda-smithy` reads config → generates `.github/workflows/`, `.ci_support/*.yaml` (one per matrix job), `.scripts/`
4. Commits generated files into the PR

**CI generation key design:** Each `.ci_support/*.yaml` file corresponds to one matrix job (one OS/arch combination). The `.ci_support/` directory is the rendered matrix — adding a new platform means running rerender, not editing YAML by hand.

**Build → publish pipeline:**
- Packages built and uploaded to `cf-staging` (staging channel)
- Webservices validates: hash sum match + feedstock token auth + output repo authorization
- On success: copied to production `conda-forge` channel

**Multi-arch execution:** Azure Pipelines emulates ARM and IBM Power (ppc64le) via QEMU on x86_64 Linux runners. macOS and Windows run natively.

**Breakage reporting:** `regro-cf-autotick-bot` opens PRs for version bumps. Failures visible as GitHub PR check failures. Long-running migrations tracked at https://conda-forge.org/status/. Community support via Zulip.

**OCX relevance:**
- The `mirror.yml` spec already defines the platform matrix via `assets:` platform keys
- A `ocx-mirror-ci generate` subcommand could render per-platform smoke test jobs from the `assets:` field — exact analog to conda-smithy rerender
- Generated workflow files can be committed alongside `mirror.yml`

**Sources:**
- https://conda-forge.org/docs/maintainer/infrastructure/
- https://conda-forge.org/docs/maintainer/understanding_conda_forge/feedstocks/
- https://github.com/conda-forge/conda-smithy

---

## Pattern 2: Homebrew test-bot (Per-Formula test do Block)

**Model:** Monorepo (`homebrew-core`). Each formula is a single Ruby file with an optional `test do` block. `brew test-bot` runs this block plus `brew audit --new --formula` as the pre-publish gate.

**Smoke test taxonomy — Homebrew's explicit guidance:**
- **Bad tests (explicitly called out):** `foo --version` and `foo --help` — "despite their widespread use"
- **Good tests:** Actual functionality — `foo build-foo input.foo`, compile+link a snippet against a library, "confirm it fails as expected" with invalid credentials
- **Libraries:** Compile and run code that links against the library
- **GUI programs:** Find any CLI-accessible functionality (format conversion, config reading)

**Infrastructure:** Physical Mac hardware (Mac Pros, Mac minis, Intel + M1) plus Ubuntu cloud instances. No QEMU for foreign arch — native hardware only.

**Per-formula CI:** Not per-repo — all formulae in `homebrew-core`. CI triggered on PR touching formula file; `brew test-bot` determines which formula changed.

**Breakage reporting:** GitHub PR check annotations directly. No Discord/webhook per-formula alerting.

**OCX relevance:**
- Homebrew's guidance that `--version` is a "bad test" is worth noting — but their context is testing functionality of installed packages, not validating pre-built binary integrity
- For pre-built binary mirroring, `--version` output matching IS the right bar: it proves the binary starts, links correctly, and exits 0
- The `test do` block model (each formula owns its test) maps cleanly to a `smoke:` field in `mirror.yml`

**Sources:**
- https://docs.brew.sh/Formula-Cookbook
- https://docs.brew.sh/BrewTestBot
- https://github.com/Homebrew/homebrew-test-bot

---

## Pattern 3: Nixpkgs / Ofborg (Build Farm + passthru.tests)

**Model:** Monorepo (`nixpkgs`). Ofborg is the CI bot; Hydra is the post-merge build farm.

**Pre-merge validation (Ofborg):**
- Automatically builds packages named in commit titles (e.g., `cmake: 3.28 -> 3.29` triggers cmake build)
- Runs `passthru.tests` derivations if present
- Reports via GitHub PR check annotations
- Native platform coverage: **x86_64-linux, aarch64-linux, x86_64-darwin, aarch64-darwin**

**`testers.testVersion` — the canonical version smoke test:**
```nix
passthru.tests.version = testers.testVersion {
  package = cmake;
  command = "cmake --version";
  version = finalAttrs.version;
};
```
- Runs the version command
- Checks output contains the expected version string
- Explicitly catches **dynamic linking errors** as a secondary benefit
- This is Nixpkgs' minimum pre-publish bar

**`passthru.tests` model:** Tests declared as Nix derivations alongside the package. Run automatically by Ofborg on PR. Not mandatory — but widely adopted.

**Breakage reporting:** Ofborg posts GitHub PR check status + comments. Hydra has a web dashboard for build farm status.

**OCX relevance:**
- `testers.testVersion` is the strongest prior art for OCX smoke tests: version output matching as minimum bar, with dynamic linker failure detection as a bonus
- "Ofborg builds on aarch64-linux natively" — OCX can do the same now that linux/arm64 GHA runners are GA
- The passthru.tests pattern (per-package test declaration in spec) maps to a `smoke:` or `verify:` section in `mirror.yml`

**Sources:**
- https://ryantm.github.io/nixpkgs/builders/testers/
- https://github.com/NixOS/ofborg
- https://github.com/nixos/nixpkgs/issues/424273 (required status checks)

---

## Pattern 4: AUR / Arch Linux (check() Function)

**Model:** Per-package PKGBUILD, no centralized CI. `check()` function runs between `build()` and `package()` — optional, community-authored.

**check() characteristics:**
- Runs in `bash -e` mode (non-zero exit aborts)
- Invokes upstream test suite (`make check`, `ctest`, etc.)
- `checkdepends=()` for test-only dependencies
- Not mandatory — many AUR packages skip it

**Relevance to OCX:** Minimal — AUR is source-build with no centralized CI, no per-architecture testing infrastructure. The `check()` / `package()` phase separation pattern (test before package) is conceptually applicable.

**Sources:**
- https://wiki.archlinux.org/title/PKGBUILD
- https://fusion809.github.io/man/PKGBUILD.5.html

---

## Pattern 5: aquaproj Registry (Version Command Metadata)

**Model:** Each tool in aqua-registry declares a `version_command` and `version_filter` in its YAML config. aqua uses these to validate installed binary version matches expected.

**Key design:** The registry YAML encodes how to extract version string from each tool's output — because tools vary wildly (`cmake --version`, `node --version`, `gh --version`). This is exactly the problem OCX faces: each mirrored package has a different version command and output format.

**OCX relevance:**
- A `smoke:` section in `mirror.yml` should encode `command:` and `version_regex:` per-tool, not assume `--version` always works or always produces parseable output
- aquaproj's registry is a good reference for which tools have non-standard version output

**Sources:**
- https://aquaproj.github.io/docs/reference/registry/
- https://github.com/aquaproj/aqua-registry

---

## Execution Strategy: QEMU Foreign Arch on GHA

**Action:** `uraimo/run-on-arch-action` (3.3k+ stars, actively maintained)
- Architectures: armv6, armv7, **aarch64**, **s390x**, **ppc64le**, **riscv64**
- Mechanism: Docker + QEMU user-mode emulation (binfmt_misc)
- Distributions: ubuntu24.04, ubuntu22.04, bookworm, bullseye, fedora_latest, alpine_latest

**Critical gotcha for pre-built binaries (not building from source):**
Raw `qemu-riscv64-static ./binary` fails: `qemu-riscv64-static: Could not open '/lib/ld-linux-riscv64-lp64d.so.1': No such file or directory`

The dynamic linker path is missing. Solutions:
1. **Docker with `--platform` flag** (recommended): `docker run --platform linux/arm64 ubuntu:24.04 ./binary` — Docker handles the full sysroot
2. **QEMU_LD_PREFIX env var**: Point to a sysroot containing target libc
3. `uraimo/run-on-arch-action` with `run:` block handles this automatically via its container setup

**Performance:** Visible overhead vs native. s390x and ppc64le are slowest (~3-5x). aarch64 is fastest (~1.5x overhead). For a `--version` smoke test, overhead is seconds, not minutes.

**Alternative for aarch64:** GHA now has native linux/arm64 runners (GA Sept 2024). `runs-on: ubuntu-latest` + `platform: linux/arm64` or explicit `ubuntu-24.04-arm`. No QEMU overhead, actual hardware test. Costs ~2x standard linux runner.

**mips and sparc:** Not supported by `run-on-arch-action`. No GHA runner option. QEMU user-mode with manual sysroot is the only path — not worth the complexity for a smoke test.

**Sources:**
- https://github.com/uraimo/run-on-arch-action
- https://blog.ludovic.dev/2023/11/19/qemu-and-gha.html

---

## Execution Strategy: Wine for Windows Binaries on Ubuntu

**Feasibility verdict: Conditional — GNU-linked only**

**Works:** Rust binaries compiled for `x86_64-unknown-linux-gnu` or `x86_64-pc-windows-gnu` (MinGW) run under Wine on Ubuntu runners. The `Reloaded-Project/devops-rust-test-in-latest-wine` GHA action automates setup.

**Does not work reliably:** MSVC-linked binaries (compiled with `x86_64-pc-windows-msvc`) require `vcruntime140.dll`, `ucrtbase.dll`, and friends. Wine's vcruntime support has persistent issues:
- `vcruntime140.dll not found` / module import errors
- `wine64` returns non-zero on vcruntime install attempts
- Status 193 (BAD_EXE_FORMAT) when mixing 32/64-bit vcruntime

**Reality for OCX mirrors:** cmake, gh CLI, node, bun — all MSVC-linked on Windows. Wine cannot reliably test these. The correct path is `windows-latest` runner.

**Wine is a non-starter for the primary OCX use case.**

**Sources:**
- https://github.com/Reloaded-Project/devops-rust-test-in-latest-wine
- https://github.com/actions/runner-images/issues/7589 (Wine removed from ubuntu-22.04)
- https://github.com/Winetricks/winetricks/issues/2065

---

## Execution Strategy: Rosetta for darwin/amd64 on ARM Runners

**Current GHA macOS runner landscape (2026):**
| Runner label | Architecture | Cost/min | Status |
|---|---|---|---|
| `macos-13` | Intel x86_64 | $0.06 | Deprecated Sept 2025, retiring |
| `macos-14` | ARM64 (M1) | $0.06 (free for public repos) | Current default |
| `macos-15` | ARM64 (M2/M3) | $0.06 | Current |
| `macos-15-intel` | Intel x86_64 | TBD | Announced, available until Aug 2027 |

**Testing darwin/amd64 on ARM runners:**
`arch -x86_64 ./binary` on an M-series runner invokes Rosetta 2 translation. Confirmed working for dynamically linked macOS binaries. Some caveats:
- `uname -m` may return `x86_64` even under Rosetta (confuses detection)
- Rosetta adds ~15% performance overhead
- Not native hardware — real x86_64 Mac silicon behavior may differ in edge cases (memory ordering, SIGBUS on unaligned access)

**Practical recommendation for OCX:**
- Use `macos-latest` (ARM64) for darwin/arm64 smoke tests — native, cheap, fast
- Use `arch -x86_64` on the same runner for darwin/amd64 smoke tests — validates Rosetta compatibility, avoids separate Intel runner cost
- Keep `macos-15-intel` as a periodic (weekly) sanity check for true x86_64 behavior until Aug 2027

**Sources:**
- https://github.com/actions/runner-images/issues/9741
- https://github.com/blog/changelog/2025-09-19-github-actions-macos-13-runner-image-is-closing-down/
- https://github.com/lima-vm/lima/discussions/2215

---

## Smoke Test Taxonomy

| Level | What it tests | Catches | Ecosystem using it |
|---|---|---|---|
| **L0: Binary exists** | `test -f ./binary && test -x ./binary` | Missing file, wrong arch (partial) | Build pipeline primitives |
| **L1: Version output** | `./binary --version` exits 0 and stdout contains expected version string | Missing dynamic linker, missing runtime DLLs, wrong binary extracted, severe startup crash | Nixpkgs `testers.testVersion`, aquaproj `version_command` |
| **L2: Help output** | `./binary --help` exits 0 (or 1 for tools that exit 1 on --help) | Panic/crash on flag parsing, missing resources | Homebrew explicitly calls this "bad" for functionality, but acceptable as linkage check |
| **L3: Functional smoke** | `cmake -E echo hello`, `node -e "console.log(1)"`, `gh --version` | Configuration errors, missing stdlib, JVM/runtime startup failures | Homebrew `test do` blocks, Nixpkgs NixOS VM tests |
| **L4: Integration** | Full workflow with real files/network | Business logic regressions | Not appropriate for pre-publish mirror validation |

**Recommendation for OCX:** L1 + per-tool override for tools with known L1 problems. The version string check is the sweet spot: proves binary starts, proves dynamic linker path resolves, proves correct binary was extracted, catches platform mismatch. Minimal false negatives.

**Per-tool variation examples:**
- `cmake --version` → exit 0, `cmake version X.Y.Z` on stdout
- `node --version` → exit 0, `vX.Y.Z` on stdout
- `gh --version` → exit 0, `gh version X.Y.Z` on stdout
- `ctest --version` → exit 0, `ctest version X.Y.Z` on stdout
- Some tools exit 1 on `--help` (not `--version`) — avoid `--help` as smoke signal

---

## Breakage Reporting Patterns

| Ecosystem | Mechanism | Latency | Dashboard |
|---|---|---|---|
| conda-forge | GitHub PR check + Zulip notification | Minutes | https://conda-forge.org/status/ |
| Homebrew | GitHub PR check (BrewTestBot) | Minutes | GitHub PR UI |
| Nixpkgs (Ofborg) | GitHub PR check + comment | Minutes | https://monitoring.ofborg.org/ |
| AUR | No centralized CI | N/A | AUR package page (user-reported) |
| aquaproj | No pre-publish CI for the registry | N/A | GitHub Issues |

**Finding:** No ecosystem uses Discord webhooks for per-package CI breakage. All use GitHub-native PR check annotations. The summary view emerges from GitHub's check suite aggregation.

**OCX recommendation:** Per-mirror smoke workflow posts check annotations. A summary job aggregates all platform results. Optional: GitHub issue auto-creation on persistent failure (conda-forge regro-bot model) — useful when mirrors go stale due to upstream asset naming changes.

---

## Comparison Matrix

| Dimension | conda-forge | Homebrew | Nixpkgs/Ofborg | OCX (proposed) |
|---|---|---|---|---|
| Repo model | One repo per package | Monorepo | Monorepo | One `mirror.yml` per package in monorepo |
| CI generation | conda-smithy renders from `conda-forge.yml` | Per-formula `test do` block | Nix expression per package | Generated from `assets:` keys in `mirror.yml` |
| Trigger | PR open/commit | PR touching formula | PR with pkg in commit msg | Mirror sync completion (post-bundle, pre-push) |
| Native linux/amd64 | Yes | Yes | Yes | Yes |
| Native linux/arm64 | No (QEMU) | No | Yes (Ofborg farm) | Yes (GHA arm64 runners, GA Sept 2024) |
| native darwin/arm64 | Yes (M1 Mac minis) | Yes (M1 Mac minis) | Yes (Ofborg farm) | Yes (macos-14/15, free for public repos) |
| darwin/amd64 | Yes (Intel Mac minis) | Yes (Intel Mac minis) | Yes (Ofborg farm) | Rosetta on ARM runner or macos-15-intel |
| windows/amd64 | Yes (Azure native) | No | No | windows-latest |
| s390x / ppc64le | Yes (QEMU) | No | No | uraimo/run-on-arch-action (QEMU) |
| riscv64 | No | No | No | uraimo/run-on-arch-action (QEMU, alpha) |
| Smoke test level | L3+ (build from source) | L3 (functional) | L1 testVersion + L3 NixOS VM | L1 (version output matching) |
| Breakage reporting | Status dashboard + Zulip | PR checks | PR checks + Ofborg comments | PR checks (proposed) |

---

## OCX-Specific Recommendations

### Recommendation 1: Generate smoke workflow from `mirror.yml` `assets:` keys

**What:** Add an `ocx-mirror smoke generate` subcommand (or a Taskfile task) that reads `mirrors/<pkg>/mirror.yml`, inspects the `assets:` platform keys, and emits `.github/workflows/smoke-<pkg>.yml` with a matrix job per declared platform.

**How it works:**
```yaml
# mirror.yml assets: section declares platforms
assets:
  linux/amd64: [...]
  linux/arm64: [...]
  darwin/amd64: [...]
  darwin/arm64: [...]
  windows/amd64: [...]

# Generated workflow gets:
matrix:
  platform: [linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64]
```

**Tradeoff:** Generated files need to be committed (like conda-forge). Adds ~1 file per mirror to the repo. Alternatively, generate dynamically as a reusable workflow called from a single dispatcher — avoids file proliferation but loses per-mirror PR check granularity.

**Why:** Platform matrix is already declared in `mirror.yml`. Deriving CI from spec avoids drift between "what we say we support" and "what we test."

---

### Recommendation 2: Use L1 smoke tests (version output matching) as the standard bar

**What:** Each smoke test runs `<binary> --version` (or configured version command), asserts exit code 0, asserts stdout contains the declared version string.

**How to encode in mirror.yml:**
```yaml
smoke:
  command: ["cmake", "--version"]
  version_regex: "cmake version {version}"  # {version} = interpolated from mirrored version
```

Default: auto-derive from primary entrypoint in `metadata.json`. Override per-tool for exceptions.

**Tradeoff:** L1 catches linkage failures but not functional regressions. That's intentional — OCX is mirroring upstream binaries, not building them. Linkage + startup = the failure modes we can actually detect pre-publish.

**Why:** Nixpkgs `testers.testVersion` is the direct analog. It explicitly catches dynamic linker failures. The Homebrew critique ("--version is a bad test") applies to testing a tool's functionality — not to validating a freshly extracted pre-built binary.

---

### Recommendation 3: Platform execution strategy — tier by cost

**What:** Structure the smoke matrix in cost tiers. Expensive runners gate behind cheap runners passing.

```
Tier 1 (always run, ~$0.006/min):
  - linux/amd64 (ubuntu-latest, native)
  - linux/arm64 (ubuntu-24.04-arm, native, ~2x cost)

Tier 2 (run if Tier 1 passes, ~$0.062/min):
  - darwin/arm64 (macos-latest, native)
  - darwin/amd64 (arch -x86_64 on macos-latest via Rosetta)
  - windows/amd64 (windows-latest, native)

Tier 3 (weekly scheduled, optional):
  - linux/s390x (ubuntu-latest + uraimo/run-on-arch-action, QEMU)
  - linux/ppc64le (ubuntu-latest + uraimo/run-on-arch-action, QEMU)
  - linux/riscv64 (alpha, QEMU)
```

**Tradeoff:** Tier 3 (QEMU) adds meaningful CI time (~5-15 min per arch for startup + emulation overhead) for architectures that represent a small fraction of actual users. Acceptable for scheduled runs, not for every mirror push.

**Why:** OCX CI already uses this pattern for acceptance tests (Linux first, Windows/macOS as matrix). Cost factors from `subsystem-ci.md` apply: macOS is 10x more expensive than Linux.

---

### Recommendation 4: Skip Wine entirely; use windows-latest for Windows smoke tests

**What:** Do not attempt Wine-based Windows binary testing on Ubuntu runners. Always use `windows-latest` for `windows/amd64` smoke tests.

**Tradeoff:** `windows-latest` costs ~1.7x Linux. For a `--version` smoke test (< 30 seconds), this is negligible. Wine would save that cost but adds unreliability for MSVC-linked binaries (cmake, gh CLI, node — all MSVC-linked on Windows).

**Why:** Wine's vcruntime support is consistently broken across 2023-2025 GitHub issues. The failure mode is silent incorrect results and non-zero exit codes on setup, not a clean "unsupported" error. This makes Wine-based tests unreliable as a quality gate.

---

### Recommendation 5: Add `smoke:` section to mirror.yml spec with per-tool override

**What:** Extend the `MirrorSpec` struct with an optional `smoke:` section:

```yaml
smoke:
  command: ["cmake", "--version"]   # defaults to [<primary-entrypoint>, "--version"]
  version_regex: "cmake version {version}"  # {version} = semver string
  expect_exit: 0                    # default 0; some tools exit 1 on --help
  platforms:                        # optional per-platform overrides
    windows/amd64:
      command: ["cmake.exe", "--version"]
```

Smoke runs post-bundle, pre-push, on the local artifact (extracted layer). No registry push if smoke fails.

**Tradeoff:** Adds a new spec field and MirrorSpec deserialization surface. Low complexity — optional field, sane defaults. The per-tool override is important: `node --version` emits `v20.0.0` (not `node version 20.0.0`), `cmake --version` emits `cmake version 3.28.0`. A rigid format assumption breaks on real tools.

**Why:** Encodes the test contract alongside the mirror spec — single source of truth, same pattern as conda-forge's `recipe/meta.yaml` owning its test block.

---

## Surprises and Gotchas

1. **QEMU user-mode vs Docker QEMU**: Running `qemu-riscv64-static ./binary` directly fails for dynamically linked binaries because the emulator can't find the target's dynamic linker. You must use Docker with `--platform` or `run-on-arch-action`, which sets up the full container sysroot. This distinction is almost never documented in "how to use QEMU in CI" tutorials.

2. **macOS-13 sunset**: `macos-13` (Intel) is deprecated as of September 2025 and will be fully removed. `macos-15-intel` is the announced replacement, available until August 2027. Any design that assumes Intel Mac runners are free/always-available is wrong going forward.

3. **`arch -x86_64` inheritance**: On macOS ARM runners, `arch -x86_64 ./parent-process` causes child processes to also run under x86_64 emulation (Rosetta) unless explicitly overridden with `arch -arm64`. This matters for tools that spawn subprocesses during startup (cmake's cmake-gui would try to spawn X11, etc.).

4. **Homebrew explicitly bans `--version`**: Their reasoning is sound for testing functionality — but it does NOT apply to validating that a pre-built binary was correctly extracted and links correctly. For OCX's use case, version output matching is the right minimum bar.

5. **conda-forge uses `cf-staging` as buffer, not a true staging registry**: Packages are pushed to cf-staging, validated (hash + token), then promoted to production. OCX's design ("testing happens on a local artifact before any registry push") is actually stronger than conda-forge — no staging registry needed, validation is entirely local.

6. **Wine was removed from ubuntu-22.04 runner images** (GitHub issue #7589). If any existing workflow assumed Wine is pre-installed, it broke. Wine must be explicitly installed, and even then, MSVC binary support remains unreliable.

7. **GHA linux/arm64 runners are now GA** (September 2024) — this materially changes the QEMU calculus. For aarch64, native runners are now available, cheaper than QEMU overhead + debugging time, and produce more reliable results.

8. **s390x is QEMU-only on GHA** with no path to native runners. IBM Z hardware is not in any cloud runner catalog. For OCX mirrors targeting s390x, QEMU via `run-on-arch-action` is the only option. For most mirrors (CMake, Node, bun), s390x support is either absent upstream or extremely rare in practice.

---

## Sources

- [conda-forge infrastructure](https://conda-forge.org/docs/maintainer/infrastructure/) — build pipeline, staging channel, bot automation
- [conda-forge feedstock model](https://conda-forge.org/docs/maintainer/understanding_conda_forge/feedstocks/) — one-repo-per-package pattern
- [conda-smithy](https://github.com/conda-forge/conda-smithy) — CI renderer tool
- [Homebrew Formula Cookbook](https://docs.brew.sh/Formula-Cookbook) — `test do` block, good/bad test guidance
- [BrewTestBot docs](https://docs.brew.sh/BrewTestBot) — pre-publish validation pipeline
- [Nixpkgs testers](https://ryantm.github.io/nixpkgs/builders/testers/) — `testVersion`, `passthru.tests` pattern
- [NixOS/ofborg](https://github.com/NixOS/ofborg) — CI bot, multi-arch build coverage
- [uraimo/run-on-arch-action](https://github.com/uraimo/run-on-arch-action) — QEMU foreign arch action
- [QEMU and GHA blog](https://blog.ludovic.dev/2023/11/19/qemu-and-gha.html) — dynamic linker pitfall for pre-built binaries
- [Reloaded-Project/devops-rust-test-in-latest-wine](https://github.com/Reloaded-Project/devops-rust-test-in-latest-wine) — Wine-based Windows binary testing
- [GHA arm64 runners GA](https://github.blog/changelog/2024-09-03-github-actions-arm64-linux-and-windows-runners-are-now-generally-available/)
- [macos-13 deprecation](https://github.blog/changelog/2025-09-19-github-actions-macos-13-runner-image-is-closing-down/)
- [macos-14 arm64 only](https://github.com/actions/runner-images/issues/9741) — Intel runners sunset
- [aquaproj registry docs](https://aquaproj.github.io/docs/reference/registry/) — version_command pattern
- [Wine removed from ubuntu-22.04](https://github.com/actions/runner-images/issues/7589) — Wine install breakage
- [Winetricks vcruntime failures](https://github.com/Winetricks/winetricks/issues/2065) — MSVC runtime on Wine
