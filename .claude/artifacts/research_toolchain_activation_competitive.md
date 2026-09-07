# Research: Name-keyed bin/shim directories in package & toolchain managers

<!--
Technology Landscape Research
Filename: artifacts/research_toolchain_activation_competitive.md
Owner: Researcher (worker-researcher)
Handoff to: Architect (/architect), Swarm Plan (/swarm-plan)
Related Skills: architect, swarm-plan

Purpose: Persist tech landscape findings to inform ADRs, plans, design decisions.
Artifacts decay — check dates before trusting findings.
-->

## Metadata

**Date:** 2026-09-04
**Domain:** packaging | cli
**Triggered by:** ocx toolchain-activation decision — `<home>/toolchain/bin/<name>` launcher
trampolines carrying per-package composed env, plus `<group>/<entry>/` links, gated by
`ocx.toml` `activate = env|bin|none`. Decision brief:
`/home/mherwig/.cache/hex/toolchain_activation_decision.md`.
**Expires:** 2027-03-04 (this space is moving fast — mise overtook asdf's GitHub star count
in Feb 2026, asdf shipped a from-scratch Go rewrite in Feb 2025, Volta announced
end-of-maintenance Nov 2025; re-verify any adoption/perf claim after 6 months)

## Direct Answer

Every tool surveyed converges on the same shape ocx's D1/D2 already picked: **one stable,
name-keyed directory of thin per-tool launchers** (a proxy binary, a generated script, or a
sidecar-driven shim.exe) that re-resolves the real target — version, install location, and
often env vars — **on every invocation**, rather than a static symlink baked in at install
time. That live re-resolution is the load-bearing feature: it's what lets `cargo` in
`~/.cargo/bin` follow `rustup toolchain default` without re-linking, and what lets a mise/asdf/
proto/volta shim honor a project's pinned version by walking up from cwd for a config file
each time it runs. Confirms **D2** directly — a generated launcher that resolves its target at
exec time, not a static symlink, is the field's consensus, not an ocx invention.

The **hook (env-activation) mode and the bin/shim-dir mode are never framed as alternatives** —
every actively maintained tool ships both and documents when to use which: hook/activate for
interactive shells (full env, arbitrary `[env]` vars, hooks run on `cd`), bin/shim dir for
IDEs, non-interactive shells, and CI, because those contexts never re-run the shell hook. mise's
own IDE-integration docs say this almost verbatim: hook mode "cannot be relied on" once an IDE
process starts, so "IDEs work better with shims." This directly confirms ocx's **D6** PATH
ordering choice (hook-mode real bin dirs prepended ahead of `toolchain/bin`, so trampolines
never fire in an activated shell) and the **D3** `activate = env|bin|none` split — mise's own
two modes map almost 1:1 onto ocx's `env` and `bin`.

**D8** (global `bin/` composes only the global toolchain, no cwd walk; a project's toolchain
reaches PATH via hook, IDE workspace config, CI `$GITHUB_PATH`, or `.envrc`) is the one point
where the field splits, and ocx's choice is the *less* common one among the polyglot version
managers but the *standard* one among single-purpose package managers. mise, asdf, proto, and
Volta all make their **one shim directory globally on PATH** and push the cwd-awareness into
the shim's per-exec logic — so for them there is no separate "project bin dir," the global shim
dir *is* project-aware by construction. rustup, Homebrew, Nix profiles, pipx, and `uv tool`,
by contrast, are exactly like ocx's D8: one static global bin dir, no cwd walk, and a *separate*
mechanism (rustup's `rust-toolchain.toml` override lives inside the *toolchain* selection layer,
not the bin-dir layer) covers per-project pinning. Since ocx already has a project-tier
`.ocx/toolchain/bin` that a *different* mechanism (hook, IDE config, CI) puts on PATH, D8 is
squarely in the rustup/Homebrew/Nix camp, not the mise/asdf camp — sharpened detail below.

## Technology Landscape

### Trending (gaining momentum)

| Tool/Pattern | Adoption Signal | Key Benefit | Relevance to OCX |
|---|---|---|---|
| [mise-en-place](https://mise.jdx.dev/dev-tools/shims.html) | Overtook asdf's GitHub star count around Feb 2026 (mise ~33k vs asdf ~25k stars per [jdx/mise#7967](https://github.com/jdx/mise/discussions/7967)); listed "Adopt" on [Thoughtworks Technology Radar](https://www.thoughtworks.com/radar/tools/mise); ~397k monthly active users, 10th most-installed Homebrew formula | Ships hook mode *and* shim mode as first-class, documents exactly when to use each | Closest prior art to ocx's `activate = env\|bin` split; its shim/activate tradeoff writeup is the clearest existing statement of D6's rationale |
| [uv tool](https://docs.astral.sh/uv/) | Astral's uv is displacing pipx for Python CLI installs — [comparison](https://pydevtools.com/handbook/explanation/how-do-uv-tool-and-pipx-compare/) notes near-uvx-hot-cache install speed vs pipx's multi-second pip+virtualenv path | Same `~/.local/bin` static shim dir as pipx, but faster and does version/env resolution without a shim indirection | Precedent for "one flat global bin dir, no per-exec shim logic, no cwd walk" done well at high speed |
| [pkgx](https://docs.pkgx.sh/pkgx/pkgx) | Actively pitched as a Homebrew/asdf/mise/Nix alternative; content-addressed, relocatable builds | `pkgm shim <tool>` writes a one-line shebang shim (`#!/usr/bin/env -S pkgx -q! git`) instead of a compiled proxy — shows how far you can go with the "shortest possible shim" end of the spectrum | Useful negative data point: a one-line shebang shim is not portable to Windows and carries a `pkgx` cold-start cost per exec |

### Established (proven, widely accepted)

| Tool/Pattern | Status | Notes |
|---|---|---|
| [rustup](https://rust-lang.github.io/rustup/concepts/index.html) | De facto standard for Rust toolchains | argv0-dispatch proxy binaries in `~/.cargo/bin`; closest architectural sibling to ocx's trampolines |
| [Homebrew](https://brew.sh/) | Dominant on macOS, growing on Linux | Cellar + prefix symlink model — static, no per-exec logic, install-time linking only |
| [Nix profiles](https://nix.dev/manual/nix/2.28/package-management/profiles) | Mature, used by NixOS and `nix profile` users | Profile is a symlink tree into the store; `~/.nix-profile/bin` on PATH; fully static |
| [pipx](https://pipx.pypa.io/latest/) | Long-standing Python CLI tool standard | `~/.local/bin` shims, one venv per tool; being displaced by `uv tool` on speed but same shape |

### Emerging (early but promising)

| Tool/Pattern | Signal | Worth Watching Because |
|---|---|---|
| [proto](https://moonrepo.dev/docs/proto) (moonrepo) | Newer entrant, backed by the moon build-system team | Explicitly ships **both** a shims dir and a bins dir (multiple symlinks per major/minor version) simultaneously on PATH, shims first — a third coexistence model worth comparing against ocx's single `bin/` |
| [aqua](https://aquaproj.github.io/docs/reference/lazy-install/) | Declarative, GitHub-Release-centric, Renovate-integrated continuous update | `aqua-proxy` lazy-install-on-first-exec via hardlinks is the cleanest prior art for ocx's "deferred package → launcher pulls on first exec" (D2) |

### Declining (losing mindshare)

| Tool/Pattern | Signal | Avoid Because |
|---|---|---|
| [asdf bash shims (pre-0.16)](http://stratus3d.com/blog/2025/02/03/asdf-has-been-rewritten-in-go/) | Historically ~120ms/exec overhead, `asdf current` took 10-20s in large setups | Bash shim spawned a subprocess per shim call; **caveat**: the Feb 2025 Go rewrite ([0.16.0](https://asdf-vm.com/guide/upgrading-to-v0-16.html)) cut `exec` to ~20ms (5x+) and made `reshim` "almost comparable to the others" — asdf is a modernized incumbent now, not purely declining; mise remains faster but the practical gap "with no practical difference in daily use" per [multiple](https://www.pkgpulse.com/guides/mise-vs-proto-vs-asdf-polyglot-version-managers-2026) [sources](https://mise.jdx.dev/dev-tools/comparison-to-asdf.html) |
| [Volta](https://docs.volta.sh/guide/understanding) | Maintainer announced end-of-maintenance Nov 2025 ([GitHub Issue #2080](https://github.com/volta-cli/volta/issues/492), summarized in [migration guide](https://lilting.ch/en/articles/volta-discontinued-migration-guide)) | No new features, OS/ecosystem-break fixes stopped; documented PATH-precedence footgun where its shim layer conflicts with globally-installed tools; maintainer's own recommended replacement is mise |

## Design Patterns Worth Considering

- **argv0/self-name dispatch, no sidecar file** — [rustup](https://rust-lang.github.io/rustup/concepts/index.html): every proxy in `~/.cargo/bin` is the *same* binary; it inspects how it was invoked (hardlink/copy name) to decide whether to act as `cargo`, `rustc`, `rust-analyzer`, etc., then resolves the active toolchain (env override → `rust-toolchain.toml` walk → default) and execs straight into `$RUSTUP_HOME/toolchains/<toolchain>/bin/<tool>`. No sidecar file, no shell script — closest existing analogue to ocx's single generated-launcher-per-name trampoline.
- **Config-walk-on-every-exec shim** — [mise](https://mise.jdx.dev/dev-tools/shims.html), [asdf](http://stratus3d.com/blog/2025/05/02/asdf-performance-improvements/), [proto](https://moonrepo.dev/docs/proto/workflows), [Volta](https://docs.volta.sh/guide/understanding): the shim's whole job is "walk up from cwd for a manifest/config file, resolve a version, exec the real binary with that version's env." This is the mechanism that makes their single global shim dir behave as if it were project-aware — precisely the capability ocx's D8 explicitly declines to build into the trampoline itself, delegating it to the hook instead.
- **Lazy install on first exec** — [aqua-proxy](https://aquaproj.github.io/docs/reference/lazy-install/), mise `lazy = true`, proto: hardlink/shim calls a resolver subcommand which downloads+installs on cache miss, then execs. Same shape as ocx's D2 "deferred package → launcher pulls on first exec," with `AQUA_DISABLE_LAZY_INSTALL` as the precedent for a CI-hardening escape hatch.
- **Sidecar text file beside a native launcher** — [scoop's shim.exe](https://github.com/ScoopInstaller/Shim/blob/main/README.md): `foo.exe` pairs with `foo.shim`, a `key = value` text file (`path`, `args`, `cwd`, env overrides, `%~dp0`-style expansion). Directly comparable to ocx's existing `.shim`/`.shimref` grammar (D2) — scoop's format is the one other widely-used prior art for "native launcher + adjacent config file" versus a fully self-contained generated script.
- **Static symlink into a content-addressed store, no per-exec logic at all** — [Homebrew](https://brew.sh/) (`bin/foo -> ../Cellar/foo/1.2.3/bin/foo`) and [Nix profiles](https://nix.dev/manual/nix/2.28/package-management/profiles) (`~/.nix-profile` symlink tree into `/nix/store`): switching versions means re-linking, not re-resolving at runtime. This is the "pinned" analogue in ocx's own D3 (`pinned = true` renders digest paths, no `<group>/<entry>` links) — Homebrew/Nix effectively run "pinned" mode as their *only* mode.
- **Global shim dir + separate numbered-symlink bin dir, both on PATH simultaneously** — [proto](https://moonrepo.dev/docs/proto/commands/bin): `~/.proto/shims` (thin `proto run` wrappers) *and* `~/.proto/bin` (direct symlinks per major/major.minor version, primary always highest-installed) are both prepended, shims first, "so shims run first and fall through to native binaries if a shim doesn't exist." Different from ocx's single `bin/` — worth noting as a road not taken, not a pattern to adopt (it doubles the surface for a marginal benefit ocx's trampoline-carries-env design already gets more simply).

## Key Findings

1. **Rustup's proxy dispatch is argv0-based, and Cargo has special-cased it further**: when Cargo itself needs to invoke rustc, it detects that the rustc on PATH is a rustup proxy (by comparing inode/hardlink identity to the rustup binary) and skips the proxy indirection, execing the toolchain binary directly — an optimization PR ([rust-lang/cargo#11917](https://github.com/rust-lang/cargo/pull/11917)) purely to avoid the extra process hop in hot paths. Relevant if ocx's trampolines are ever called from within another ocx-aware process — a direct-exec fast path is a known, precedented optimization, not a design smell.
2. **rustup's override resolution is closest-directory-wins, walked toward the root**: `rust-toolchain.toml` / directory overrides are resolved by walking from cwd up to `/`, and the first file found wins over a more distant one — [Overrides](https://rust-lang.github.io/rustup/overrides.html). This is the "cwd walk" mechanism ocx's D8 deliberately keeps *out* of the trampoline and pushes to the hook/IDE/CI layer instead.
3. **Installer PATH handling is platform-forked in exactly the way ocx's D5 already assumes**: rustup writes `HKEY_CURRENT_USER\Environment` on Windows and appends `. "$HOME/.cargo/env"` sourcing to shell rc files on Unix — [installation docs](https://rust-lang.github.io/rustup/installation/other.html) — mirroring ocx's `~/.config/environment.d/ocx.conf` (Linux) / registry write (Windows) / LaunchAgent (macOS) split in D5. No tool in this survey does anything materially different; this is a solved, low-risk problem.
4. **mise's own docs state, near-verbatim, ocx's D6 rationale**: "IDEs work better with shims... the default `mise activate` method cannot be relied on" once an IDE has started, because it never re-sources the shell hook — [IDE Integration](https://mise.jdx.dev/ide-integration.html). This is direct confirming evidence for why ocx needs *both* `activate=env` (interactive shells) and `activate=bin` (IDEs, CI, non-interactive) rather than picking one.
5. **Shims silently drop arbitrary env vars until invoked, which is the same tradeoff ocx's D2 makes explicit**: mise's shims docs state plainly that `[env]` vars are "only set when a shim is executed" — a `mise set` env var doesn't reach `echo $VAR` in the parent shell, only a child process launched *through* a shim — [Shims](https://mise.jdx.dev/dev-tools/shims.html). Ocx's D2 already states trampolines carry "per-package closure env... NOT the session envelope" — this is exactly that limitation, independently converged on, not an ocx gap.
6. **Collision handling across the field is uniformly "last/highest wins silently," never a hard error** — mise falls back through mise-managed-tool-priority then system PATH; proto's numbered bin-dir makes the un-suffixed name always resolve to the highest installed version; scoop, notably, is the outlier and gets this wrong: it silently overwrites one app's shim with another's on a name clash, and there is a long-standing open feature request ([ScoopInstaller/Scoop#1261](https://github.com/ScoopInstaller/Scoop/issues/1261), [#1290](https://github.com/ScoopInstaller/Scoop/issues/1290)) for shim namespacing that has never landed. Ocx's D2 "last tool walked lands first... debug-level only, never a warning" is more disciplined than scoop's ad hoc overwrite and matches mise/proto's silent-priority approach — validates the choice, but the scoop history is a cautionary tale for *not* silently overwriting a previously-rendered trampoline for a different package without at least a debug log (which D2 already commits to).
7. **asdf's 2025 Go rewrite substantially undercuts a "shims are just slow" argument** — pure exec overhead dropped from ~120ms to ~20ms (5x+) and `reshim`, previously "the slowest subcommand," is now "almost comparable to the others" ([Stratus3D](http://stratus3d.com/blog/2025/05/02/asdf-performance-improvements/), [asdf 0.16 upgrade guide](https://asdf-vm.com/guide/upgrading-to-v0-16.html)). A compiled-launcher trampoline design (ocx's D2, like rustup/mise/proto) is still the right end of the spectrum, but "shims used to be catastrophically slow" is 2024-era evidence, not current — cite the rewrite if this argument appears in the ADR.
8. **Volta's shim model is the one clear cautionary tale in this survey, and it's dying because of a PATH-precedence bug class, not a shim/hook coexistence bug**: Volta inserts `~/.volta/tools/image/node/<version>/bin` ahead of `~/.volta/bin` on PATH, so a tool installed *through* a Volta shim (e.g. a global npm package) gets permanently pinned to whatever Node version was active at install time, ignoring later `volta pin` changes — root cause of user-reported breakage that contributed to its Nov 2025 end-of-maintenance announcement ([migration guide](https://lilting.ch/en/articles/volta-discontinued-migration-guide)). Relevant to ocx only insofar as it's a reminder to keep the trampoline's env resolution *dynamic at exec time* (which D2 already specifies) rather than baking a resolved path into the trampoline itself.
9. **Scoop's shim.exe has measured, published overhead numbers**, useful as an upper bound for what a native-launcher trampoline costs: +24ms (C# build) to +103ms (Rust build) versus direct exec of `whoami.exe`, dominated by process-creation and file I/O, not by the shim logic itself ([ScoopInstaller/Shim README](https://github.com/ScoopInstaller/Shim/blob/main/README.md)). Since ocx's trampolines are POSIX shell/native launchers doing comparable work (resolve path, maybe read a sidecar, exec), a similar tens-of-ms overhead band should be the expectation, not zero.
10. **No tool in this survey treats hook-mode and shim/bin-mode as mutually exclusive** — mise ships and documents both simultaneously with an explicit "when to use which" table; proto puts both directories on PATH at once by default; asdf-direnv exists specifically to combine a hook (direnv) with asdf's shims. The pattern is consistently additive/layered, never either-or — strong confirming evidence for ocx's D6 (hook prepends real bin dirs ahead of `toolchain/bin`; trampolines are the non-activated-shell fallback, not a competing mechanism).

## Recommendation

**Confirm D1/D2 as-specified — no changes indicated.** The generated-launcher-with-exec-time-
resolution pattern (D2) and the `bin/<name>` + `<group>/<entry>/` split (D1) match the field's
converged design (rustup proxies, mise/asdf/proto shims, aqua-proxy); the "env vars ride the
trampoline's closure only, not the session envelope" limitation in D2 is independently arrived
at by mise's shim mode too (Finding 5) and is documented there as an accepted, load-bearing
tradeoff rather than a defect to fix.

**Confirm D6 with one strengthening citation.** mise's IDE-integration docs make ocx's exact
argument for why hook mode and bin mode must coexist rather than one replacing the other — worth
citing directly in the ADR as external validation, since it's phrased almost as an answer to
"why not just always use the hook."

**Sharpen D8, don't reopen it.** D8's "no cwd walk, deferred" is correctly scoped once you see
where the field's tools land: the polyglot version managers (mise/asdf/proto/Volta) build cwd-
awareness *into the shim itself* because for them there is exactly one shim dir, global, and it
must serve every project. Ocx's architecture is different in a way that changes the calculus:
the project tier already gets its own physical `toolchain/bin` (`.ocx/toolchain/bin`), so the
cwd-walk problem those tools solve inside the shim doesn't exist for ocx — a project's PATH
entry is a directory-selection problem (which `toolchain/bin` is on PATH), not a version-
selection problem inside one shim. That makes ocx's global `bin/` structurally like rustup's
`~/.cargo/bin`, Homebrew's prefix `bin/`, and Nix's profile `bin/` — all of which are flat,
global, non-cwd-aware, and rely on a *separate* project-scoping mechanism (rust-toolchain.toml
+ hook-equivalent shell integration, direnv, `nix develop`) precisely as ocx's D8 already
specifies (hook, IDE workspace config, CI `$GITHUB_PATH`, `.envrc PATH_add`). Recommend adding
one sentence to the ADR naming this precedent explicitly (rustup/Homebrew/Nix vs
mise/asdf/proto/Volta as the two camps) so a future reviewer doesn't mistake D8 for an
oversight rather than a considered architectural consequence of ocx's two-tier home model.

**One gap worth a follow-up issue, not a blocker**: no tool surveyed publishes first-party
Windows per-exec overhead numbers for a *compiled* trampoline (only scoop's shim.exe, which is
a different design). D5's "per-platform verification is a plan item, not a claim" is the right
posture — keep it, and note scoop's 24-103ms band as the working expectation to validate
against once ocx's Windows `.shim`-based launcher is measured.

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| [mise Shims](https://mise.jdx.dev/dev-tools/shims.html) | Docs | current (2026) | Shim vs activate tradeoffs, env var limitation, collision fallback, reshim semantics |
| [mise IDE Integration](https://mise.jdx.dev/ide-integration.html) | Docs | current (2026) | Explicit hook-mode-vs-shim-mode guidance for non-interactive contexts |
| [mise Comparison to asdf](https://mise.jdx.dev/dev-tools/comparison-to-asdf.html) | Docs | current (2026) | Post-Go-rewrite performance comparison |
| [jdx/mise Discussion #7967](https://github.com/jdx/mise/discussions/7967) | Community/maintainer post | Feb 2026 | Star-count overtake data point |
| [jdx/mise Discussion #7727](https://github.com/jdx/mise/discussions/7727) | Maintainer post | 2026 | Year-in-review, adoption numbers (397k MAU) |
| [Thoughtworks Technology Radar — mise](https://www.thoughtworks.com/radar/tools/mise) | Industry radar | 2026 | "Adopt" ring placement, independent trend signal |
| [rustup Concepts](https://rust-lang.github.io/rustup/concepts/index.html) | Docs | current | Proxy binary architecture in `~/.cargo/bin` |
| [rustup Overrides](https://rust-lang.github.io/rustup/overrides.html) | Docs | current | `rust-toolchain.toml` directory-walk override resolution |
| [rustup Other installation methods](https://rust-lang.github.io/rustup/installation/other.html) | Docs | current | Windows registry PATH write vs Unix rc-file append |
| [cargo PR #11917](https://github.com/rust-lang/cargo/pull/11917) | Source/PR | merged | Direct-exec fast path bypassing the rustup proxy hop |
| [moonrepo proto — Workflows](https://moonrepo.dev/docs/proto/workflows) | Docs | current (2026) | Shims dir vs bins dir, both on PATH simultaneously |
| [moonrepo proto — bin command](https://moonrepo.dev/docs/proto/commands/bin) | Docs | current | Numbered symlink scheme (major/minor/canary) |
| [aqua — Lazy Install](https://aquaproj.github.io/docs/reference/lazy-install/) | Docs | current | aqua-proxy hardlink dispatch, download-on-first-exec flow |
| [Volta — Understanding Volta](https://docs.volta.sh/guide/understanding) | Docs | current (pre-EOL) | Project detection framing (high-level only) |
| [Volta discontinued migration guide](https://lilting.ch/en/articles/volta-discontinued-migration-guide) | Blog | Nov 2025 / 2026 | EOL announcement, PATH-precedence root cause, recommended migration to mise/fnm |
| [ScoopInstaller/Shim README](https://github.com/ScoopInstaller/Shim/blob/main/README.md) | Source docs | current | `.shim` sidecar grammar, measured per-exec overhead by implementation language |
| [ScoopInstaller/Scoop #1261](https://github.com/ScoopInstaller/Scoop/issues/1261) | GitHub issue | open | Shim collision/overwrite behavior, unresolved namespacing request |
| [ScoopInstaller/Scoop #1290](https://github.com/ScoopInstaller/Scoop/issues/1290) | GitHub issue | open | Same, alternate feature-request thread |
| [Homebrew](https://brew.sh/) | Docs/site | current | Cellar + prefix symlink model |
| [Nix Profiles manual](https://nix.dev/manual/nix/2.28/package-management/profiles) | Docs | current | Profile symlink-tree-into-store model |
| [pipx Comparisons](https://pipx.pypa.io/latest/explanation/comparisons.html) | Docs | current | `~/.local/bin` shim model baseline |
| [uv tool vs pipx comparison](https://pydevtools.com/handbook/explanation/how-do-uv-tool-and-pipx-compare/) | Blog/guide | 2026 | Speed comparison, same bin-dir convention |
| [pkgx docs](https://docs.pkgx.sh/pkgx/pkgx) | Docs | current (2026) | One-line shebang shim as the minimal end of the design spectrum |
| [asdf rewritten in Go](http://stratus3d.com/blog/2025/02/03/asdf-has-been-rewritten-in-go/) | Blog (asdf core contributor) | Feb 2025 | Rewrite rationale, historical bash-shim overhead |
| [asdf Performance Improvements](http://stratus3d.com/blog/2025/05/02/asdf-performance-improvements/) | Blog (asdf core contributor) | May 2025 | Post-rewrite measured numbers (exec ~20ms, reshim comparable) |
| [asdf 0.16 upgrade guide](https://asdf-vm.com/guide/upgrading-to-v0-16.html) | Docs | current | Official rewrite/perf summary |
| [PkgPulse — mise vs proto vs asdf 2026](https://www.pkgpulse.com/guides/mise-vs-proto-vs-asdf-polyglot-version-managers-2026) | Blog/guide | 2026 | Post-rewrite three-way comparison, cd-activation latency claim |
