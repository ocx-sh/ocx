# Plan: Getting Started Guide

<!--
Implementation Plan
Filename: artifacts/plan_getting_started_guide.md
Owner: Builder (/builder)
Handoff to: Builder (/builder), QA Engineer (/qa-engineer)
Related Skills: documentation
-->

## Overview

**Status:** Draft
**Author:** Architect
**Date:** 2026-03-11
**Beads Issue:** N/A
**Related PRD:** N/A
**Related ADR:** N/A

## Objective

Create a comprehensive getting-started page at `website/src/docs/getting-started.md` that takes a new user from zero to productive with ocx in under 10 minutes. The page should cover the core workflow (exec, install, find, select/deselect, uninstall) with terminal recordings demonstrating each step, then bridge to the user guide for deeper concepts.

## Scope

### In Scope

- Full page content for `getting-started.md`
- 5 new recording scripts for workflows not covered by existing recordings
- Terminal component placements for all recordings (existing + new)
- Reference-style links, tooltips, callout boxes per documentation rules
- Sidebar already has the entry; page just needs content

### Out of Scope

- Changes to the installation page (assumes ocx is already installed)
- New recording setups (all scripts use existing `basic` or `multi-version` setups)
- Changes to the Terminal Vue component
- Changes to the user guide or reference pages

## Page Structure

The page follows a progressive disclosure pattern: start with the simplest possible command (`exec`), then layer on persistence (`install`), stable paths (`find --candidate`/`--current`), version switching (`select`/`deselect`), and cleanup (`uninstall`). Each section builds on the previous one.

### Section 1: `## Quick Start {#quick-start}`

**Anchor:** `#quick-start`

The fastest path to value. One command that auto-installs and runs a package. Demonstrates that ocx can be used without any setup beyond installation.

**Key points:**
- `ocx exec` auto-installs if the package is not present, then runs the command
- No separate install step needed for one-off use
- Packages come from the default registry (`ocx.sh`)
- Multiple packages can be passed — their environments are merged in order: `ocx exec cmake:3.28 python:3.12 -- cmake --version`

**Terminal:** Reuse existing `exec.cast` (single package). Add new `exec-multi.cast` showing two packages.

**Callout:** `:::tip` -- mention that `exec` is the fastest way to try a tool without committing to an install

---

### Section 2: `## Installing {#installing}`

**Anchor:** `#installing`

Persisting a package so it is available offline and across sessions. Introduces the candidate symlink concept without diving into the three-store architecture.

**Idea:** Running a tool once is useful, but projects need tools to be reliably present across terminal sessions and CI runs.
**Problem:** `exec` re-resolves the package every time. For repeated use you want a persistent, named install.
**Solution:** `ocx install` downloads the package and creates a stable `candidates/{tag}` symlink.

**Key points:**
- `ocx install hello-world:1` fetches and creates a candidate symlink
- The candidate path is stable -- it does not change when the underlying binary is updated
- Multiple versions can coexist (candidates are per-tag)
- `ocx find --candidate hello-world:1` returns the stable symlink path

**Terminals:**
- Reuse existing `install.cast`
- New recording: `find-candidate.cast` (demonstrates `ocx find --candidate`)

**Callout:** `:::info` -- analogy to `apt install` vs `npx`/`bunx`: install persists, exec is ephemeral

---

### Section 3: `## Switching Versions {#switching-versions}`

**Anchor:** `#switching-versions`

Introduces the `current` symlink as a floating pointer, and `select`/`deselect` to manage it. This is the version-manager aspect of ocx.

**Idea:** Projects often need to switch between tool versions -- testing against an older release, matching a CI environment, or evaluating an upgrade.
**Problem:** With only candidate symlinks, every config that references a tool needs to know the exact tag. Changing versions means updating every reference.
**Solution:** `current` is a single path that always points to whichever version you last selected. Update once with `ocx select`, and everything referencing `current` follows.

**Key points:**
- `ocx install --select` installs and sets `current` in one step
- `ocx select hello-world:1` points `current` at an already-installed candidate
- `ocx find --current hello-world` returns the `current` symlink path (no tag needed)
- `ocx deselect hello-world` removes `current` without uninstalling anything
- `current` never moves automatically -- only when you run `select`

**Terminals:**
- New recording: `install-select.cast` (install --select, then find --current)
- New recording: `select-deselect.cast` (select between two versions, deselect)

**Callout:** `:::info` -- analogy to SDKMAN's `sdk use`/`sdk default` or `nvm use`/`nvm alias default`

**Callout:** `:::tip` -- recommend `--current` paths for shell profiles and IDE configs since they survive version switches

---

### Section 4: `## Uninstalling {#uninstalling}`

**Anchor:** `#uninstalling`

How to remove packages and reclaim disk space.

**Idea:** Installed packages accumulate over time. You need a way to remove what you no longer need.
**Problem:** Naive deletion risks breaking other references. If two tags point to the same binary, removing one should not affect the other.
**Solution:** `ocx uninstall` removes the candidate symlink (and optionally `current`). The underlying binary is only removed if no other references exist.

**Key points:**
- `ocx uninstall hello-world:1` removes the candidate symlink
- The binary object remains if other candidates or refs still point to it
- `ocx uninstall --deselect` also removes `current` in one step
- Mention `ocx clean` for bulk GC of unreferenced objects (link to user guide)

**Terminal:**
- New recording: `uninstall.cast`

**Callout:** `:::warning` -- uninstall removes the candidate symlink, not necessarily the binary. Use `--purge` to force-remove the object if no refs remain.

---

### Section 5: `## Environment {#environment}`

**Anchor:** `#environment`

How ocx exposes and composes package-declared environment variables across multiple packages.

**Idea:** Real builds rarely use a single tool. A CMake project needs CMake, a compiler, and Python for scripts — each with its own `PATH` entries and tool-specific variables.
**Problem:** Manually exporting variables per tool is error-prone, order-sensitive, and causes silent conflicts between packages.
**Solution:** Packages declare their environment in metadata. `ocx env` prints the merged, resolved variables for any number of packages in one shot; `ocx exec` injects them into a clean child process automatically.

**Key points:**
- `ocx env cmake:3.28 python:3.12` shows the composed environment from both packages — PATH entries merged, no conflicts
- `ocx exec cmake:3.28 python:3.12 -- cmake --version` runs with a clean environment containing only the declared vars from both packages (no ambient PATH pollution)
- Variables of type `path` are appended in package order; `constant` vars are set once; `accumulator` vars are merged across packages
- `ocx shell env` emits shell-specific `export` statements for eval in profiles — supports multiple packages too

**Terminals:**
- New recording: `env-multi.cast` (two packages, showing merged PATH)
- Existing `env.cast` can be shown first as the single-package baseline

**Callout:** `:::tip` -- show the multi-package eval pattern:
```sh
eval "$(ocx shell env --current cmake python node)"
```

**Callout:** `:::info` -- analogy: "Like [direnv][direnv]'s `.envrc`, but the env is declared by the package itself and composed automatically — no manual merging."

---

### Section 6: `## Next Steps {#next-steps}`

**Anchor:** `#next-steps`

No terminal recording. Short section with links to deeper topics.

**Key points:**
- Link to user guide for three-store architecture, versioning, locking
- Link to reference/command-line for full flag reference
- Link to reference/environment for env var configuration
- Mention `ocx index` commands for browsing available packages (link to existing `index.cast` as a collapsed terminal)

**Terminal:** Reuse existing `index.cast` (collapsed, as a preview of index commands)

---

## New Recording Scripts

### Script 0: `exec-multi.sh`

```sh
# title: Running multiple packages together
# setup: full-catalog
ocx exec cmake:3.28 python:3.12 -- cmake --version
```

**Rationale:** Demonstrates multi-package exec — the composed environment lets both tools run in the same invocation. Uses `full-catalog` setup which already publishes cmake and python.

---

### Script 1: `find-candidate.sh`

```sh
# title: Finding an installed package
# setup: basic
ocx install hello-world:1
ocx find --candidate hello-world:1
```

**Rationale:** Demonstrates the candidate symlink path after install. Bridges the gap between "I installed it" and "where is it on disk?" Shows the stable path concept.

---

### Script 2: `install-select.sh`

```sh
# title: Installing and selecting a version
# setup: basic
ocx install --select hello-world:1
ocx find --current hello-world
```

**Rationale:** Shows the one-step install+select workflow and the `current` symlink. This is the recommended workflow for most users.

---

### Script 3: `select-deselect.sh`

```sh
# title: Switching and removing the active version
# setup: multi-version
ocx install python:3.12
ocx install python:3.11
ocx select python:3.12
ocx find --current python
ocx select python:3.11
ocx find --current python
ocx deselect python
```

**Rationale:** Demonstrates the full version-switching lifecycle: install two versions, select one, switch to the other, then deselect. Uses `multi-version` setup for realism with a well-known tool name.

---

### Script 4: `uninstall.sh`

```sh
# title: Uninstalling a package
# setup: basic
ocx install hello-world:1
ocx uninstall hello-world:1
```

**Rationale:** Shows the clean removal flow. Keeps it simple -- install then uninstall.

---

### Script 5: `shell-env.sh`

```sh
# title: Shell profile integration
# setup: basic
ocx install --select hello-world:1
ocx shell env --current hello-world
```

**Rationale:** Demonstrates the `shell env` command that produces `export` statements for eval. This is the recommended pattern for shell profile integration and directly supports the tip callout in the Environment section.

---

### Script 6: `env-multi.sh`

```sh
# title: Composing environments from multiple packages
# setup: full-catalog
ocx env cmake:3.28 python:3.12
```

**Rationale:** Shows the merged environment output from two packages — the key multi-package composition feature. Uses `full-catalog` setup. Demonstrates how PATH entries from both packages appear in a single table.

---

## Terminal Component Placements

| Section | Cast File | Props | Notes |
|---------|-----------|-------|-------|
| Quick Start | `/casts/exec.cast` | `title="Running a package"` | Existing recording. Single-package baseline. |
| Quick Start | `/casts/exec-multi.cast` | `title="Running multiple packages together"` | New. Shows composed exec with cmake + python. |
| Installing | `/casts/install.cast` | `title="Installing a package"` | Existing recording. |
| Installing | `/casts/find-candidate.cast` | `title="Finding an installed package"` | New. Place after the candidate symlink explanation. |
| Switching Versions | `/casts/install-select.cast` | `title="Installing and selecting a version"` | New. Shows the one-step workflow. |
| Switching Versions | `/casts/select-deselect.cast` | `title="Switching and removing the active version"`, `collapsed` | New. Collapsed because it is a longer demo. Click to expand. |
| Uninstalling | `/casts/uninstall.cast` | `title="Uninstalling a package"` | New. |
| Environment | `/casts/env.cast` | `title="Package environment"` | Existing recording. Single-package baseline. |
| Environment | `/casts/env-multi.cast` | `title="Composing environments from multiple packages"` | New. Main multi-package demo. Core differentiator. |
| Environment | `/casts/shell-env.cast` | `title="Shell profile integration"`, `collapsed` | New. Collapsed -- supplementary. |
| Next Steps | `/casts/index.cast` | `title="Browsing available packages"`, `collapsed` | Existing recording. Collapsed as a teaser for index commands. |

---

## Doc Content Outline

### Frontmatter

```yaml
---
outline: deep
---
```

### Page Title

```markdown
# Getting Started {#getting-started}
```

Opening paragraph (2-3 sentences): This guide walks through the core ocx workflow -- from running your first package to managing multiple versions. It assumes ocx is already installed (link to installation page). Each section builds on the previous one, but they can be read independently.

### Quick Start

- One sentence: the fastest way to use a package is `ocx exec`
- Code block showing `ocx exec hello-world:1 -- hello`
- Terminal recording
- Brief explanation: exec auto-installs if needed, runs the command, done
- `:::tip` -- "Use `exec` for one-off tasks. For persistent installs, read on."

### Installing

- **Idea:** Persisting tools so they survive across sessions
- **Problem:** `exec` re-resolves every time; you want a named, persistent install
- **Solution:** `ocx install` creates a stable candidate symlink
- Terminal: `install.cast`
- Explain candidate path: `~/.ocx/installs/.../candidates/{tag}`
- Mention that the path is stable and content-addressed storage handles dedup behind the scenes
- Terminal: `find-candidate.cast`
- `:::info` analogy: "Like `apt install` vs `npx` -- install persists the tool, exec is ephemeral"
- Link to [user guide objects section][fs-objects] and [installs section][fs-installs] for the full three-store explanation

### Switching Versions

- **Idea:** Projects need to pin and switch between tool versions
- **Problem:** Candidate paths include the tag -- changing versions means updating every reference
- **Solution:** `current` is a tag-free floating pointer
- Show `ocx install --select hello-world:1`
- Terminal: `install-select.cast`
- Explain `current` symlink: one path, always the selected version
- `ocx find --current` to retrieve the current path
- Show switching: `ocx select` / `ocx deselect`
- Terminal: `select-deselect.cast` (collapsed)
- `:::info` analogy: "Same pattern as [SDKMAN][sdkman]'s `sdk default` or [nvm][nvm]'s `nvm alias default` -- a stable name that re-points to the active version"
- `:::tip` -- "Use `--current` paths in shell profiles and IDE settings. They survive version switches without config changes."
- Link to [user guide installs section][fs-installs] for the two-tier symlink architecture

### Uninstalling

- **Idea:** Removing packages you no longer need
- **Problem:** Blind deletion can break shared references
- **Solution:** `ocx uninstall` removes the symlink; `ocx clean` handles orphaned objects
- Show `ocx uninstall hello-world:1`
- Terminal: `uninstall.cast`
- Mention `--deselect` flag to also remove `current`
- `:::warning` -- "Uninstall removes the candidate symlink. The binary object may remain if other candidates reference it. Use `ocx clean` to remove all unreferenced objects."
- Link to [user guide objects section][fs-objects] for GC details

### Environment

- **Idea:** Tools need environment variables, not just a binary
- **Problem:** Manual exports are error-prone and conflict-prone
- **Solution:** Packages declare env vars in metadata; ocx resolves them
- `ocx env` prints resolved variables
- Terminal: `env.cast`
- `ocx exec` injects them into a clean child process
- `ocx shell env` for shell profile integration
- Terminal: `shell-env.cast` (collapsed)
- `:::tip` -- show the eval pattern:
  ```sh
  eval "$(ocx shell env --current hello-world)"
  ```

### Next Steps

- Bullet list with links:
  - **[User Guide][user-guide]** -- three-store architecture, versioning, locking, authentication
  - **[Command Reference][cmd-ref]** -- full flag and option documentation for every command
  - **[Environment Reference][env-ref]** -- all environment variables including auth configuration
- Terminal: `index.cast` (collapsed) with a sentence like "Use `ocx index catalog` to browse available packages"

### Link Definitions (bottom of file)

```markdown
<!-- external -->
[sdkman]: https://sdkman.io/
[nvm]: https://github.com/nvm-sh/nvm
[direnv]: https://direnv.net/

<!-- pages -->
[installation]: ./installation.md
[user-guide]: ./user-guide.md
[cmd-ref]: ./reference/command-line.md
[env-ref]: ./reference/environment.md

<!-- commands -->
[cmd-install]: ./reference/command-line.md#install
[cmd-find]: ./reference/command-line.md#find
[cmd-exec]: ./reference/command-line.md#exec
[cmd-select]: ./reference/command-line.md#select
[cmd-deselect]: ./reference/command-line.md#deselect
[cmd-uninstall]: ./reference/command-line.md#uninstall
[cmd-clean]: ./reference/command-line.md#clean
[cmd-index-catalog]: ./reference/command-line.md#index-catalog

<!-- user guide sections -->
[fs-objects]: ./user-guide.md#file-structure-objects
[fs-installs]: ./user-guide.md#file-structure-installs
[fs-index]: ./user-guide.md#file-structure-index
```

---

## Dependency Order

Implementation must follow this sequence because each phase produces artifacts consumed by the next.

### Phase 1: Recording Scripts

Create the 5 new `.sh` files in `test/recordings/scripts/`.

- [ ] **1.1** Create `test/recordings/scripts/find-candidate.sh`
- [ ] **1.2** Create `test/recordings/scripts/install-select.sh`
- [ ] **1.3** Create `test/recordings/scripts/select-deselect.sh`
- [ ] **1.4** Create `test/recordings/scripts/uninstall.sh`
- [ ] **1.5** Create `test/recordings/scripts/shell-env.sh`

No new setups needed. Scripts use existing `basic` and `multi-version` setups.

### Phase 2: Generate Cast Files

Run the recording test suite to produce `.cast` files from all scripts (existing + new).

- [ ] **2.1** Run `cd /home/mherwig/dev/ocx/test && uv run pytest recordings/ -v` to generate all casts
- [ ] **2.2** Verify 9 `.cast` files exist in `website/src/public/casts/` (4 existing + 5 new)
- [ ] **2.3** Spot-check new casts by previewing in browser (optional but recommended)

### Phase 3: Write the Documentation Page

- [ ] **3.1** Write `website/src/docs/getting-started.md` following the content outline above
- [ ] **3.2** Verify all internal links resolve (anchors in user-guide, command-line, environment)
- [ ] **3.3** Verify all Terminal `src` paths match actual `.cast` filenames
- [ ] **3.4** Run VitePress dev server (`cd website && bun run vitepress dev`) and review the rendered page

### Phase 4: Verification

- [ ] **4.1** Check that the sidebar entry in `config.mts` already points to `/docs/getting-started` (it does)
- [ ] **4.2** Navigate through the page in the dev server -- confirm all terminals render, collapsed terminals expand on click, links work
- [ ] **4.3** Check the right-hand TOC renders short, readable section titles

## Files to Modify

| File | Action | Description |
|------|--------|-------------|
| `test/recordings/scripts/exec-multi.sh` | Create | Recording: exec with two packages (cmake + python) |
| `test/recordings/scripts/find-candidate.sh` | Create | Recording: install + find --candidate |
| `test/recordings/scripts/install-select.sh` | Create | Recording: install --select + find --current |
| `test/recordings/scripts/select-deselect.sh` | Create | Recording: select between two versions + deselect |
| `test/recordings/scripts/uninstall.sh` | Create | Recording: install + uninstall |
| `test/recordings/scripts/shell-env.sh` | Create | Recording: install --select + shell env --current |
| `test/recordings/scripts/env-multi.sh` | Create | Recording: env with two packages showing merged output |
| `website/src/public/casts/exec-multi.cast` | Generate | Output of recording test run |
| `website/src/public/casts/find-candidate.cast` | Generate | Output of recording test run |
| `website/src/public/casts/install-select.cast` | Generate | Output of recording test run |
| `website/src/public/casts/select-deselect.cast` | Generate | Output of recording test run |
| `website/src/public/casts/uninstall.cast` | Generate | Output of recording test run |
| `website/src/public/casts/shell-env.cast` | Generate | Output of recording test run |
| `website/src/public/casts/env-multi.cast` | Generate | Output of recording test run |
| `website/src/docs/getting-started.md` | Rewrite | Full page content (currently empty) |

## Risks

| Risk | Mitigation |
|------|------------|
| Recording scripts fail due to setup differences | All scripts use existing setups (`basic`, `multi-version`) that are already tested by the 4 existing scripts |
| `select-deselect.sh` is too long and produces a cluttered cast | Use `collapsed` prop on the Terminal component so it does not dominate the page |
| `ocx shell env` output varies by detected shell | Acceptable -- the recording environment uses a consistent shell. Document that output format depends on the active shell |
| Links to user guide anchors break if user guide is reorganized | All anchors use explicit `{#id}` syntax which is stable. Verify during Phase 4 |

## Notes

- The existing `getting-started.md` file exists in the sidebar config and on disk but is empty (contains only a blank line). No structural changes to the site config are needed.
- The `multi-version` setup publishes `python:3.12.0` and `python:3.11.0`. The `select-deselect.sh` script uses `python:3.12` and `python:3.11` (without patch) which should resolve via the cascaded tags (the setup uses `cascade=True` by default in `make_package`).
- Terminal components with `src` prop default to `autoPlay: false`. Inline `<Frame>` children default to `autoPlay: true`. All recordings on this page use `src`, so users click to play -- appropriate for a documentation page with multiple recordings.
