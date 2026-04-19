# PR-FAQ: Project Toolchain Config for OCX (`ocx.toml` + `ocx.lock`)

## Overview

**Status:** Draft
**Author:** Architect worker (sion worktree)
**Date:** 2026-04-19
**GitHub Issue:** #33 (project-tier config walk)
**Related ADR:** [`adr_project_toolchain_config.md`](./adr_project_toolchain_config.md)
**Related PRD:** [`prd_project_toolchain.md`](./prd_project_toolchain.md)

---

# PRESS RELEASE

## OCX Ships Project Toolchain Config — Commit One File, Build Reproducibly Forever

**San Francisco — 2026-Q3** — The OCX team today announced project-level toolchain config for OCX, the OCI-registry-backed binary package manager used by GitHub Actions workflows, Bazel wrappers, and developer shells across the Rust ecosystem. With this release, any repository can commit a single `ocx.toml` file declaring the tools it needs, run `ocx lock` to pin those tools to registry manifest digests, and get byte-identical tool resolution on every developer laptop, CI runner, and production build box — for years, not days.

### The Problem

Teams building Rust and polyglot backends use a familiar set of binary tools — CMake, shellcheck, shfmt, lychee, protoc, grpcurl, a dozen more — and today every repository re-states that toolchain in a different place: `.github/workflows/ci.yml` installs one version, `.devcontainer/` installs another, the README tells new hires to `brew install` a third, and the build inevitably fails in whichever environment was last updated. Existing tools either solve one environment at a time (`asdf`, `mise`) or pull from a single package format (`rustup` for Rust, `uv` for Python), leaving "the binaries we need to build this project" as a persistent drift problem. Developers spend hours every month debugging "works on my machine" incidents that trace back to a patch version bump nobody noticed.

### The Solution

OCX now supports a committed, machine-lockable toolchain manifest: list your tools once in `ocx.toml`, run `ocx lock`, commit both files, and every future `ocx exec -- ...` invocation resolves to the exact digests you locked. No auto-updates. No silent drift. No "latest at the time" surprises. The `ocx.lock` file covers every platform (Linux amd64 / arm64, macOS amd64 / arm64, Windows amd64) by default, so CI on Linux and a developer on Apple Silicon resolve to the same logical tool and provably-identical contents.

### How It Works

- **Declare once in `ocx.toml`**: `cmake = "3.28"`, `shellcheck = "0.11"`, etc. — syntax familiar to anyone who has opened a `Cargo.toml`.
- **Lock once with `ocx lock`**: OCX contacts your configured registries, resolves each tag to a manifest digest per platform, and writes `ocx.lock`.
- **Commit both files**: `ocx.toml` is the human-readable intent; `ocx.lock` is the machine-written proof of what got resolved. Both belong in version control.
- **Build reproducibly with `ocx exec`**: inside a repo with `ocx.toml`, `ocx exec -- cmake --build build` pulls only the digests in the lock — never re-resolves tags, never surprises you with a patch bump.
- **Activate interactively with `ocx shell init`**: a prompt-hook exports the right PATH and env vars as you `cd` into your project; the hook never installs anything on its own, so you always know what state you are in.
- **Update deliberately with `ocx update`**: `ocx update cmake` re-resolves one tool; `ocx update` re-resolves everything. It is the only command that ever changes your lock file.

### Quote from OCX Maintainer

> "OCX has always been about using OCI registries as the backend for binary tools. What was missing was the one artifact a project commits to Git that says 'these are the tools this repo runs against.' That file is `ocx.toml`, and the lock file next to it is what makes OCX a serious answer for anyone who cares about reproducibility."
>
> — Michael Herwig, OCX Maintainer

### Quote from Early User

> "We swapped four different tool-install steps in our CI workflow for one line — `ocx pull` — and deleted 200 lines of `apt-get install` scripts. Every new hire clones the repo and runs `ocx exec -- make`. That's the onboarding."
>
> — Platform Engineer, early-access customer

### Getting Started

Run `ocx init` in your project to scaffold an `ocx.toml`. Add the tools you use. Run `ocx lock`. Commit both files. Run `ocx pull` to fetch everything, then `ocx exec -- <your build command>`. For interactive shell activation, run `ocx shell init bash` (or your shell) and add the emitted line to your `.bashrc`.

---

# INTERNAL FAQ

## Strategic Questions

### Why should we build this now?

Three signals converge:

1. **OCX's positioning is backend-tool-first.** GitHub Actions, Bazel, Python scripts — all of them want a committed, reproducible "toolchain for this repo" declaration. We have the storage story (OCI-backed three-store model); what we lack is the consumer-facing declaration.
2. **Prior art is mature.** mise.lock, PDM's `content_hash`, Cargo.lock, and direnv have been battle-tested for years. We are not pioneering; we are composing proven patterns in the OCX shape.
3. **The hook is already wired.** `ConfigInputs.cwd` exists at `loader.rs:33`, documented as the #33 extension point. Delaying further accrues no benefit and lets the unused hook bitrot.

The breaking-changes window in the next release is also a forcing function — we either take this one-way door now with the freedom to design cleanly, or we take it later with compat constraints.

### What is the target market size?

| Metric | Value | Source |
|--------|-------|--------|
| TAM | Every repository that uses OCI binary tools in CI (growing segment, ~millions of repos) | Adjacent: Docker Hub + GHCR traffic |
| SAM | Teams who currently use mise / asdf / rtx / homebrew Brewfile / Nix Flakes | mise has ~50k GitHub stars; asdf ~22k; nix flakes are adopted in >10k repos |
| SOM (year 1) | OCX's existing adopters + Rust-ecosystem teams looking to replace ad-hoc `cargo binstall` + `brew install` chains | Internal adoption funnel |

### Who are the competitors?

| Competitor | Strengths | Weaknesses | Our Differentiation |
|---|---|---|---|
| `mise` (formerly rtx) | Great UX, plugin ecosystem, mise.lock | Plugin-based (shell-script install plumbing), no OCI storage, trust surface is `asdf`-plugins-wide | OCX uses OCI registries directly — fewer moving parts, stronger provenance, platform-native sigstore/SLSA story |
| `asdf` | Ubiquitous, 10+ years of community plugins | Version installation is fragile, no lock file in core, slow cold starts | OCX is faster (singleflight pulls, object store hardlinks) and has a real lock file |
| `nix` / `flakes` | Bit-perfect reproducibility | Massive learning curve, build-from-source by default, weeks to onboard | OCX is pre-built-binary-first; a new user is productive in 10 minutes |
| `Brewfile` | Familiar to Mac developers | macOS-only; no Linux story; tap-based provenance | OCX is multi-platform from day one |
| `.tool-versions` (asdf) | Dead simple | No lock file; tags resolve at install time — no reproducibility | OCX has `ocx.lock` with digest pinning |

### What are the key risks?

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Lock file format becomes technical debt | High | High | ADR commits to one-way-door discipline; `lock_version` via `serde_repr` rejects unknown versions; canonicalization is frozen by test |
| Users `.gitignore` the lock file by reflex | Medium | High | First-run `ocx lock` emits a visible "commit your ocx.lock" note when the file isn't already tracked; user guide has a prominent section |
| Prompt-hook latency degrades shell responsiveness | Medium | Medium | `_OCX_APPLIED` fingerprint cache skips re-exports when state is unchanged; target <5ms in steady state |
| Breaking change in `ocx.toml` schema within v1 | Low | Very High | Schema is minimal in v1 (only string values); inline-table extensions are deferred to v2 with forward-compat via `lock_version` bump |
| Shell hook security boundary weakens over time | Medium | High | Boundary is documented in the ADR and in the subsystem rule; tests assert no network calls in hook paths; CI enforces via an offline-sandbox test |

### What does success look like?

| Timeframe | Metric | Target |
|---|---|---|
| Launch (Day 1) | `ocx.toml` + `ocx.lock` committed in the OCX repo itself (dogfood) | 1 (the OCX dogfood initiative) |
| 30 days | External repos with `ocx.toml` checked in (GitHub search) | ≥ 10 |
| 90 days | `ocx lock` / `ocx exec` adoption in the mirror catalog's own CI | 100% of `mirrors/*/` that have CI |
| 1 year | `ocx.toml` is the documented onboarding path in OCX's user guide | Docs restructured around it; legacy `ocx install` → `shell profile` path deprecated |

### What resources are required?

| Resource | Estimate | Notes |
|---|---|---|
| Engineering | ~4 weeks across 10 phases | Each phase is a shippable increment; no phase blocks the next except Phase 1 → 2 |
| Docs | ~1 week for the user guide restructure + JSON schema generation | Integrated with website/ subsystem |
| External dependencies | None | All infrastructure (config loader hook, pull_all, ProfileBuilder) exists |
| Hard blocker | `feature/dependencies` branch must land first | Transitive deps are required for tools that declare their own runtime deps |

## Technical Questions

### Is this technically feasible?

Yes, with high confidence. The three hard pieces are already solved:

1. **Config loader hook** — `ConfigInputs.cwd` is wired through and unused; extension is additive.
2. **Pull pipeline** — `pull_all` accepts digest-pinned identifiers today; the project path produces them from the lock and passes them through unchanged.
3. **Shell export generation** — `ProfileBuilder` already produces per-shell export syntax for 9 shells; the hook commands consume it as-is.

The new code is narrowly scoped: three library modules (`project/`, `project/lock/`, `project/resolver.rs`) and six new CLI command files. No refactor of `PackageManager`, `FileStructure`, `Index`, or `Client`.

### What are the technical dependencies?

- **`feature/dependencies`** — hard blocker. Tools that themselves declare runtime deps (e.g., `ocx-mirror` depends on `tar`, `sha256sum`) need transitive pulling for the project-config pull to be correct.
- **`toml` crate** — already in use for `Config` parsing.
- **`serde_repr`** — already in use elsewhere in the codebase; used here for `lock_version`.
- **`sha2`** — already in use; provides the declaration hash.

No new dependencies required.

### What's the estimated timeline?

| Phase | Duration | Deliverable |
|---|---|---|
| Phase 1 (loader) | 3 days | `ProjectLoader::discover` returns `(Option<ProjectConfig>, Option<ProjectLock>)` |
| Phase 2 (schema + hash) | 3 days | `ProjectConfig`, `ProjectLock`, canonicalization locked by test |
| Phase 3 (ocx lock) | 5 days | `ocx lock [--group]` resolves and writes deterministically |
| Phase 4 (ocx exec) | 3 days | `exec.rs` branch for project-config + lock read |
| Phase 5 (ocx update) | 2 days | Re-resolve one or all tools |
| Phase 6 (ocx pull) | 1 day | No-args `ocx pull` in a project directory |
| Phase 7 (shell trio) | 5 days | `hook-env`, `shell-hook`, `shell init` |
| Phase 8 (direnv) | 1 day | `ocx generate direnv` |
| Phase 9 (home `ocx.toml` + deprecation) | 2 days | Profile deprecation path |
| Phase 10 (docs) | 5 days | User guide, JSON schema generation, taplo integration |
| **Total** | **~6 calendar weeks with review loops** | **Shippable increments each phase** |

---

# EXTERNAL FAQ

## Customer Questions

### What is `ocx.toml`?

A TOML file you commit to your repository declaring which binary tools it depends on. Example:

```toml
[tools]
cmake = "3.28"
shellcheck = "0.11"
shfmt = "3"

[group.ci]
lychee = "0"
```

Every tool in `[tools]` is always pulled. Tools under `[group.<name>]` are pulled when you pass `--group <name>` to `ocx exec` or `ocx pull`.

### What is `ocx.lock`? Why commit it?

`ocx.lock` is the machine-written pin file. After you run `ocx lock`, it contains the exact OCI manifest digest for every tool in `ocx.toml`, across every supported platform. Committing it is what makes your builds reproducible: next week, next year, or next team, the same `ocx.lock` resolves to the same bytes.

If you ignore `ocx.lock` (as you might ignore `node_modules`), you lose reproducibility. `ocx` prints a visible note the first time `ocx lock` runs to encourage you to commit it.

### How do I use `ocx exec` with a project config?

From inside a directory with `ocx.toml`:

```sh
ocx exec -- cmake --build build
```

No package list needed on the command line — `ocx` reads the lock and pulls the tools listed there. You can narrow to a group:

```sh
ocx exec --group ci -- lychee --check-retry **/*.md
```

If the lock is missing or stale (your `ocx.toml` changed and you haven't re-run `ocx lock`), `ocx exec` exits with exit code 65 (data error) and prints exactly which section changed. You never get a silent re-resolve.

### How do I activate the project tools in my shell?

Three options, in increasing order of magic:

1. **`ocx exec`** — no activation; just prefix every command. Most hermetic; always correct.
2. **Shell hook** — run `ocx shell init bash` once and add the emitted line to your `.bashrc`. After that, `cd`ing into a directory with `ocx.toml` automatically exports the right PATH / env vars. The hook never installs anything — if a tool is listed but not yet pulled, it prints a one-line note to stderr telling you to run `ocx pull`.
3. **direnv** — run `ocx generate direnv` to write an `.envrc`. Works with stock direnv; the resulting `.envrc` is `eval "$(ocx --offline shell-hook)"` plus a `watch_file` for `ocx.toml` and `ocx.lock`.

All three paths export the same environment. Choose based on your workflow.

### What happens when I update a tool version?

You edit `ocx.toml` (say, `cmake = "3.29"`), then run `ocx update cmake` (or just `ocx update` for everything). OCX re-resolves the tag to the current digest on the registry and writes the new `ocx.lock`. Commit both files. Until you do, `ocx exec` refuses to run — the lock is stale and would silently disagree with your intent.

This is the only command that changes `ocx.lock`. There is no auto-update path. Ever.

### Is this compatible with the current `ocx install` / `ocx shell profile`?

`ocx install` is unchanged — it still installs into the home tier (`~/.ocx/symlinks/...`) for tools you want globally. If you don't create an `ocx.toml`, OCX behaves exactly as it does today.

`ocx shell profile` (add / remove / list / load) is deprecated in this release and will be removed one or two releases later. The replacement is a home-tier `~/.ocx/ocx.toml` file, which uses the exact same schema as project `ocx.toml`. You can migrate with:

```sh
ocx shell profile list --format json | ocx ... # migration helper (exact CLI TBD)
```

In the transition period, both systems work side by side with a deprecation note.

### Is my data secure?

The shell hook (`ocx hook-env`, `ocx shell-hook`) does NOT make network calls, does NOT install anything, does NOT modify symlinks, and does NOT read any file outside the `OCX_CEILING_PATH` walk. It reads `ocx.toml` + `ocx.lock` + the object store's install sentinels, and emits `export` lines. That's it. This boundary is enforced by tests that run the hook in an offline sandbox.

### What if I need help?

`ocx <command> --help` for every new command. The user guide at ocx.sh/docs has a dedicated "Project Toolchain" section. File issues at github.com/[org]/ocx.

---

## Appendix

### Customer Research

Feedback from early OCX adopters consistently calls out three pain points:

1. "Our CI has a 30-line `apt-get install` block that doesn't match my laptop."
2. "New hires spend their first day installing the right version of every tool."
3. "The production build box has an older lychee that passes different links than my local."

All three are solved by `ocx.toml` + `ocx.lock`.

### Mockups/Visuals

No visual mockups — this is a CLI/file-format feature. Example `ocx.toml` and `ocx.lock` files are embedded in the ADR.

---

## Approval

| Role | Name | Date | Decision |
|---|---|---|---|
| Product | | | Pending |
| Engineering | | | Pending |
| Leadership | | | Pending |

---

## Next Steps

After PR-FAQ approval:
1. [x] ADR drafted — [`adr_project_toolchain_config.md`](./adr_project_toolchain_config.md)
2. [x] PRD drafted — [`prd_project_toolchain.md`](./prd_project_toolchain.md)
3. [ ] GitHub issue linked (#33) as the tracking parent; decompose into phase-level issues
4. [ ] Architect review for ADR sign-off
5. [ ] Builder agent implements Phase 1 (loader extension) first — unblocks the rest
