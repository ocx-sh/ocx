# PRD: Package Entry Points — Generated Launchers Wrapping `ocx exec`

## Overview

**Status:** Draft
**Author:** worker-architect (Phase 4 of /swarm-plan high #61)
**Date:** 2026-04-21
**Version:** 1.0
**GitHub Issue:** [max-oberwasserlechner/ocx#61](https://github.com/max-oberwasserlechner/ocx/issues/61)
**Stakeholders:** OCX maintainers, CI tool integrators (GitHub Actions, Bazel rules, devcontainer features)
**Related ADR:** [`adr_package_entry_points.md`](./adr_package_entry_points.md)
**Soft Depends On:** #60 (env template resolver — landed `5d56c76`), #32 (deps interpolation — landed `b17b179`)
**Correlates With:** #25 (portable OCX home), #23 (relative symlinks)

## Problem Statement

Today a user who wants `cmake --version` from an OCX-installed package has three options, each with friction:

1. `ocx exec cmake:3.28 -- cmake --version` — requires knowing OCX syntax and typing the identifier every call. Unusable from Makefiles, IDE run configurations, and ad-hoc shell history.
2. `ocx find cmake:3.28 --current` → embed the resulting path in the user's own PATH — works but the path is unstable across versions, and the user must maintain the PATH plumbing themselves.
3. `ocx shell profile add cmake:3.28` + reopen shell — persistent, but ties the tool to a shell that was loaded through `profile load` and is hostile to CI runners that do not load the profile.

Competitors fill this gap with launcher scripts: Homebrew emits symlinks into `/opt/homebrew/bin`; Nix `makeWrapper` emits shell wrappers beside the original binary; npm `cmd-shim` emits `.cmd`/`.sh`/`.ps1` trios in `node_modules/.bin`; pipx emits `console_scripts` entries. Without a comparable feature, OCX is unusable as the primary tool-distribution mechanism for any workflow that invokes installed binaries by name — the stated primary target (GitHub Actions, Bazel rules, devcontainer features) cannot be served without forcing every caller through `ocx exec`.

### Evidence

**Quantitative**

- The `ocx exec` invocation pattern is load-bearing for every `env`/`exec` flow today, yet every alternative tool-manager in the competitive matrix (mise, asdf, rustup, Homebrew) ships shim/launcher binaries as their default user surface — zero competitors require a wrapper-invocation UX. The gap is structural, not stylistic.
- Issue #61 has `area/package`, `area/package-manager`, `area/cli` labels — three subsystems affected, confirming the cross-cutting product impact.
- `ocx shell profile` requires 9 shell implementations today (Ash, Ksh, Dash, Bash, Zsh, Elvish, Fish, Batch, PowerShell — `shell.rs:10-31`) and emits `export PATH=${installPath}/bin` lines. Launchers collapse that surface to Unix `.sh` + Windows `.cmd` (2 targets) and eliminate the per-shell export generator code path on the PATH axis.

**Qualitative**

- From `product-context.md` Target Users: "Primary: Automation tools — GitHub Actions, Bazel rules, devcontainer features, CI scripts." These targets cannot invoke `ocx exec cmake:3.28 -- cmake` inside a build rule that expects to run `cmake` as a bare command. Without launchers the primary audience is underserved.
- From product-context.md Product Principles: "Clean-env execution — `ocx exec` runs with package-declared vars only." Launchers preserve this — they delegate to `ocx exec`, so the env discipline is retained. A path-addition strategy (competitor default: add `${installPath}/bin` to PATH) would break it by allowing the ambient PATH of the shell to bleed in.
- From research artifact §5 (Product-Context Alignment Check): "Launchers preserve this differentiator. No matrix update needed." Launchers are the first UX feature that lets casual invocation keep OCX's clean-env guarantee.

## Goals & Success Metrics

| Goal | Metric | Target |
|------|--------|--------|
| Casual invocation of installed tools | User runs `<tool-name>` after `ocx install <pkg>` | Works on Linux/macOS/Windows without shell-profile reload |
| Preserve clean-env guarantee | `<tool-name>` produces the same env as `ocx exec <pkg> -- <tool-name>` | Byte-identical on the env entries declared by the metadata |
| CI/automation fit | GitHub Actions / Bazel rules invoke tools by bare name | Zero per-runner PATH reload; single `ocx install` + launcher availability |
| Small maintenance surface | Launcher template count | 2 (POSIX sh + Windows cmd); no `.ps1` in v1 |
| Small runtime overhead | Wall-clock overhead of launcher vs direct `ocx exec` call | <5 ms on Linux (dominated by `ocx` binary startup, not launcher script) |

## User Stories

### Package Publisher

- As a package publisher, I want to declare which binaries in my package are user-facing entry points so that consumers can invoke them by name.
  - **Acceptance:** Publisher adds `entrypoints: [{name: "cmake", target: "${installPath}/bin/cmake"}]` to `metadata.json`. `ocx package create` / `ocx package push` validates the list (name slug regex, unique names, target paths that resolve under `${installPath}` or a declared dep's `installPath`). Invalid lists reject at publish time with a structured error.
- As a package publisher, I want entry-point names to follow the same naming conventions as dependency aliases so that I only have to learn one rule.
  - **Acceptance:** Names must match `^[a-z0-9][a-z0-9_-]*$`. Uniqueness enforced at deserialization like `Dependencies::new()`. Error message cites both the offending name and the existing rule.

### Package Consumer (Interactive Dev)

- As a developer, I want to invoke the installed tool by its short name so that my muscle memory works without learning OCX-specific invocation syntax.
  - **Acceptance:** After `ocx install cmake:3.28 --select`, running `cmake --version` in a shell whose PATH has been updated by `ocx shell profile load` produces the same output as `ocx exec cmake:3.28 -- cmake --version`.
- As a developer switching between versions, I want `ocx select cmake:3.29` to immediately change which `cmake` my shell resolves.
  - **Acceptance:** After `ocx select`, the active launcher for `cmake` points to the new install's `content/` without requiring a shell reload. Previous version remains installed; rollback via `ocx select cmake:3.28` works identically.

### Package Consumer (CI / Automation)

- As a GitHub Actions author, I want to invoke the installed tool by its name inside later steps without adding it to PATH manually.
  - **Acceptance:** After `ocx install cmake:3.28 --select`, a follow-up step with `run: cmake --version` succeeds. PATH-integration details are one command (documented under `ocx ci export` or `ocx shell profile`).
- As a Bazel rule author, I want the launcher path to be deterministic and stable across versions of the same package tag so that the rule does not embed a digest-dependent absolute path.
  - **Acceptance:** The canonical PATH surface is a stable symlink anchor (not a digest-sharded path). Flipping the `current` symlink changes which package the launcher resolves to, but the PATH entry itself does not move.

### Package Consumer (Windows Native)

- As a Windows developer invoking the launcher from cmd.exe or PowerShell, I want argument forwarding to respect spaces and `--` boundaries.
  - **Acceptance:** `cmake --version` and `cmake "-DFOO=bar baz"` work from `cmd.exe`. PowerShell invocation of the same `.cmd` launcher via `cmake --version` works (users call `.cmd`, not `.ps1`, per research §3).

## Requirements

### Functional Requirements

| ID | Requirement | Priority | Notes |
|----|-------------|----------|-------|
| FR-1 | `Metadata::Bundle` gains a new optional field `entrypoints: Option<Vec<EntryPoint>>` | Must Have | Additive-optional, does not break existing packages |
| FR-2 | `EntryPoint` has two validated fields: `name` (slug regex `^[a-z0-9][a-z0-9_-]*$`) and `target` (template string resolved against `${installPath}` + `${deps.NAME.installPath}`) | Must Have | Same validation rules as the existing deps-interpolation surface (#32) |
| FR-3 | Entry-point `name` uniqueness is enforced at deserialization time, matching `Dependencies::new()` | Must Have | Error type `EntryPointError::DuplicateName { name }` |
| FR-4 | Publish-time validation of `target` templates in `ValidMetadata::try_from` | Must Have | Must reference `${installPath}` or declared dep names; unknown fields rejected |
| FR-5 | On `ocx install` (`pull.rs`), generate Unix `.sh` and Windows `.cmd` launchers into the package-root `entrypoints/` directory | Must Have | Launchers land in temp dir; carried by existing atomic `temp → packages/` move |
| FR-6 | Launchers invoke `ocx exec <absolute-path-to-content> -- "$@"` | Must Have | **One-Way Door contract.** Host OCX is on PATH; launcher does not bake the `ocx` binary path |
| FR-7 | A per-package floating symlink `symlinks/{registry}/{repo}/entrypoints-current` points at the current install's `entrypoints/` directory and is flipped by `ocx select` | Must Have | Mirror of the existing `current` symlink pattern |
| FR-8 | `ocx shell profile add` (and its `load` output) treats the `entrypoints-current` directory as a PATH entry just like `${installPath}/bin` today | Must Have | Unifies with existing shell-profile plumbing |
| FR-9 | On `ocx uninstall`, the package's `entrypoints/` directory is removed alongside `content/` by the existing package-root GC pattern | Must Have | No new GC logic — sibling dir is package-rooted |
| FR-10 | Launcher template is versioned by OCX binary (not baked-in version field); rebuilding launchers is a `ocx install` side effect | Should Have | No `ocx regen-launchers` command v1 |
| FR-11 | When a package-root move relocates the `$OCX_HOME` tree (future #25), launchers become stale and must be regenerated | Should Have | Staleness detection deferred to #25; v1 bakes absolute paths per research recommendation |

### Non-Functional Requirements

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-1 | Launcher invocation overhead | <5 ms wall-clock on Linux/macOS (dominated by `/bin/sh` fork+exec, not by launcher logic). Windows `.cmd`: <20 ms (cmd.exe startup). |
| NFR-2 | Launcher file count | Exactly one file per `(entry_point, platform)` — Unix: `.sh` (no extension on PATH convention: use the bare name), Windows: `.cmd` |
| NFR-3 | Launcher file size | <200 bytes per launcher |
| NFR-4 | Compatibility with non-launcher consumers | Existing `ocx exec <identifier> -- <cmd>` CLI unchanged; no breaking change for CI scripts already invoking `ocx exec` |
| NFR-5 | Clean-env parity | Env entries emitted inside the launched process are byte-identical to those emitted by `ocx exec` for the same install |

## Scope

### In Scope (v1)

- `Metadata::Bundle.entrypoints: Option<Vec<EntryPoint>>` schema field
- `EntryPoint { name, target }` type with slug + template validation
- Unix POSIX sh launcher template
- Windows `.cmd` launcher template
- Install-time generation in `pull.rs` between content assembly and `post_download_actions`
- Package-root `entrypoints/` sibling directory to `content/` + `refs/`
- Per-package `entrypoints-current` floating symlink updated by `ocx select`
- Shell-profile integration so PATH picks up `entrypoints-current`
- Publish-time validation of entry-point name slugs, uniqueness, and target templates

### Out of Scope / Deferred

| Item | Deferral reason |
|------|-----------------|
| Per-entry-point env/dep declarations | Explicitly out-of-scope per issue body; YAGNI until publisher demand surfaces |
| PowerShell `.ps1` launcher | Research §3 identified documented `--` parsing bug in PS when forwarding to native exe; `.cmd` is callable from PowerShell today |
| `ocx regen-launchers` command | Launchers regenerate on `ocx install`; explicit command only needed if #25 (portable OCX home) moves the tree underneath existing launchers |
| Launcher name collision across packages in the same repo | See ADR decision — rejected at install time (no namespacing v1); document for future |
| Relative-path launchers for #25 (portable OCX home) | Research §6 + ADR Tension 5 recommend absolute paths now + staleness detection during `$OCX_HOME` move handler in #25 |
| `ocx exec --install-dir=<path>` flag as a separate CLI surface | ADR Tension 1 picks positional-overload (Option A); flag alternative recorded in ADR |
| Entry-point visibility levels (sealed/public/interface) | Not in issue scope; env-var visibility already covers this axis |
| Namespaced launcher names (`<package>-<entry>`) | Rejected v1 — issue specifies user-friendly short names; collisions resolved by rejecting at install |

## Dependencies

| Dependency | Owner | Status | Risk |
|------------|-------|--------|------|
| #60 env template resolver extraction | OCX core | **Landed** (`5d56c76`) | None |
| #32 `${deps.NAME.installPath}` interpolation | OCX core | **Landed** (`b17b179`) | None |
| Existing `ocx exec` positional-path contract | OCX CLI (`exec.rs:29-34`) | Shipped; `ocx exec <path>` already works when `path` is resolved to a package content dir | Low — contract becomes load-bearing post-ship (see ADR Tension 1 reversibility) |
| #25 portable OCX home | Open | Future work — launchers bake absolute paths, so moving `$OCX_HOME` requires launcher regeneration hook in #25 | Medium — #25 implementation must include launcher regen step |

## Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `ocx exec <absolute-path>` contract change after ship | Low | High | ADR locks the contract; acceptance tests assert it; changing requires a multi-version deprecation |
| Launcher name collision across multiple installed packages | Medium | Medium | Reject at install time with actionable error; document the resolution (alias via `--select` of a different package, or publisher picks a unique name). No silent last-wins |
| Windows cmd.exe `%*` drops empty arguments | Low | Low | Documented cmd.exe limitation (research §3); affects any cmd.exe-based launcher; acceptance test verifies non-empty arg forwarding |
| MSYS2/Git Bash path translation of the baked Windows install path | Low | Low | Paths baked as native-Windows (accepted by MSYS2 inbound); smoke test covers Git Bash invocation of `.cmd` |
| `$OCX_HOME` moved → launchers become stale | Low (today) | High (after #25) | Deferred to #25 — issue must include "regenerate launchers" step; document in the #25 plan |
| User invokes launcher without `ocx` on PATH | Medium | Low | Launcher fails with `ocx: command not found`; CLI message points to `ocx shell profile load` or manual PATH export. No attempt to bake `ocx` location |
| Launcher lands in gitignored CI cache and is reused after OCX upgrade | Low | Low | Launcher content is stable across OCX versions (just calls `ocx exec <path> -- "$@"`); upgrading `ocx` does not invalidate existing launchers |

## Open Questions

- [ ] **Shell profile PATH semantics for CI-without-shell-init:** Acceptance test must cover a CI step that runs `ocx install --select && cmake --version` in a single job without `shell profile load` — is the `entrypoints-current` PATH entry applied via `ocx ci export` automatically, or must the user opt in? (ADR defers; implementation plan should resolve before stubs.)
- [ ] **Windows PATH wiring for CI:** GitHub Actions Windows runners source PowerShell profile, not cmd.exe profile. Does the shell-profile `add` invocation handle both? (Current `shell.rs` has `Shell::Batch` + `Shell::PowerShell`, so yes — smoke test needed.)
- [ ] **Entry-point rename is a breaking change:** A publisher changing `name: cmake` to `name: cmake3` in a new version breaks callers who hardcoded `cmake3.28 cmake --version`. Document as a publisher-discipline concern; no enforcement v1.

## Appendix

### Related Research

- [`research_launcher_patterns.md`](./research_launcher_patterns.md) — prior art, argument-forwarding recipes, Windows/MSYS hazards, self-location idioms
- [`discover_package_entry_points.md`](./discover_package_entry_points.md) — end-to-end flow map, hook points, reusable surfaces
- [`adr_package_entry_points.md`](./adr_package_entry_points.md) — architectural decisions, one-way-door trade-offs

### Example `metadata.json` (after this PRD)

```json
{
  "type": "bundle",
  "version": 1,
  "env": [
    { "key": "PATH", "type": "path", "value": "${installPath}/bin" }
  ],
  "dependencies": [
    { "identifier": "ocx.sh/openssl:3@sha256:..." }
  ],
  "entrypoints": [
    { "name": "cmake", "target": "${installPath}/bin/cmake" },
    { "name": "ctest", "target": "${installPath}/bin/ctest" }
  ]
}
```

### Reference Invocation Flow

```
user types:   cmake --version
shell finds:  $OCX_HOME/symlinks/ocx.sh/cmake/entrypoints-current/cmake
symlink → $OCX_HOME/packages/ocx.sh/sha256/.../entrypoints/cmake
launcher content:
    #!/bin/sh
    exec ocx exec '$OCX_HOME/packages/ocx.sh/sha256/.../content' -- "$@"
ocx exec:    resolves env from metadata.json + resolve.json at the given path
exec's child: cmake (with clean env + package-declared PATH/JAVA_HOME/etc.)
```

## Version History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-04-21 | worker-architect (opus) | Initial draft after Phase 1 Discover + Phase 2 Research |

## Next Steps & Handoffs

After PRD approval:

1. [x] Architect Review — this PRD is paired with [`adr_package_entry_points.md`](./adr_package_entry_points.md) resolving the four deliberate tensions
2. [ ] Design Spec — ADR already contains component contracts; no separate design spec needed
3. [ ] Implementation Plan — generated from this PRD + ADR by the Phase 5 planner
4. [ ] GitHub Issue — issue #61 already exists; close with the merged feature

**Related Artifacts**:

- ADR: [`adr_package_entry_points.md`](./adr_package_entry_points.md)
- Research: [`research_launcher_patterns.md`](./research_launcher_patterns.md)
- Discover: [`discover_package_entry_points.md`](./discover_package_entry_points.md)
- Meta-plan: [`.claude/state/plans/meta-plan_package-entry-points.md`](../state/plans/meta-plan_package-entry-points.md)
