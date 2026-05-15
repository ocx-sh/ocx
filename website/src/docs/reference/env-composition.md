---
layout: doc
outline: deep
---

# Environment Composition

This page is the reference-level specification for how OCX assembles environment variables and selects which toolchain tier is active. For the motivation behind these design decisions, see [Environment composition in the user guide][user-guide-global].

## Strict Isolation {#strict-isolation}

OCX enforces a hard boundary between the global toolchain and project-tier resolution. The rule is unconditional:

> **Global tools never compose into, supplement, or fall back into a project's resolved environment.**

This applies without exception to:

- [`ocx run`][cmd-run] — project-tier env-composition command. Reads `ocx.toml` + `ocx.lock`. The global toolchain (`$OCX_HOME/ocx.toml`) is not consulted, not merged, and not used as a fallback for tools the project does not declare.
- [`ocx exec`][cmd-exec] — OCI-tier env-composition command. Never reads any `ocx.toml`, whether project or global. Takes OCI identifiers directly.

Both commands are hermetic: the environment they produce is determined entirely by their declared inputs. An undeclared tool is absent, never filled from the global set.

:::info Why hard isolation instead of gap-fill?

[Volta][volta] pioneered this model for Node.js: global tools are hidden when a project toolchain is active. The alternative — filling in tools the project does not declare from the global set — produces the reproducibility hole OCX is designed to close: collaborators without the same `$OCX_HOME/ocx.toml` get different resolved environments.

:::

### Shell hook behavior {#strict-isolation-shell-hook}

The prompt hook (`ocx shell hook`) enforces isolation at the PATH level. When a project `ocx.toml` is in scope, the hook **removes** global tools from `PATH` entirely — not shadows them, removes them. The comment `# ocx: global toolchain suppressed` appears in the hook's output when this strip fires.

When no project is in scope (the CWD walk finds no `ocx.toml`), the hook emits the global `current` set. Entering a project replaces it; there is no union or composition at any point.

### What "hermetic" means for `ocx run` {#strict-isolation-run}

`ocx run` reads exactly two files: `ocx.toml` and its sibling `ocx.lock`. The resolved environment consists of the tools those files declare — no more. If a tool is not in `ocx.toml`, it is not in the child environment, regardless of what is installed globally or what is on the parent shell's PATH.

By default `ocx run` **inherits** the spawning shell's environment and merely **prepends** the composed tool `bin/` directories to `PATH` — ambient parent-shell `PATH` entries remain reachable after the project tools. The default is *not* hermetic. Pass `--clean` for a hermetic environment that drops the inherited environment and exposes only the composed tool set, exactly like `exec --clean`.

### What "hermetic" means for `ocx exec` {#strict-isolation-exec}

`ocx exec` takes one or more OCI identifiers on the command line. It resolves each identifier, composes the declared environment variables from the resolved packages, and spawns the command with that environment. No `ocx.toml` is read — not the project file, not the global file. The entire operation is stateless with respect to project configuration.

## Tier Selection {#tier-selection}

OCX has two toolchain tiers. Selection is always explicit — there is no implicit fallback from project to global.

| Tier | How to activate | File |
|------|----------------|------|
| Project | CWD walk finds `ocx.toml`; or `--project <path>`; or [`OCX_PROJECT`][env-ocx-project] | nearest `ocx.toml` ancestor |
| Global | `--global` flag; or [`OCX_GLOBAL`][env-ocx-global] | `$OCX_HOME/ocx.toml` |

The two flags are mutually exclusive — combining `--global` with `--project` exits with code 64 (`UsageError`).

**No implicit home-tier discovery.** Earlier versions of OCX fell back to `$OCX_HOME/ocx.toml` when the CWD walk found nothing. That behavior has been removed. The global toolchain is only active when explicitly requested. A CWD walk that finds nothing means no project tier is active — the command operates without a project context.

### Commands that accept `--global` {#tier-selection-commands}

The following project-tier commands accept `--global` to target `$OCX_HOME/ocx.toml`:

| Command | With `--global` |
|---------|----------------|
| [`ocx add`][cmd-add] | Adds binding to the global file |
| [`ocx remove`][cmd-remove] | Removes binding from the global file |
| [`ocx lock`][cmd-lock] | Re-locks the global file |
| [`ocx upgrade`][cmd-upgrade] | Advances a binding in the global file |
| [`ocx pull`][cmd-pull] | Pre-warms packages declared by the global file |
| [`ocx run`][cmd-run] | Composes env from the global file + its lock |
| [`ocx install --global`][cmd-install] | Install + record in global file + advance `current` symlink (implies `--select`) |

## Visibility Surfaces {#visibility-surfaces}

Each OCX package declares two environment surfaces: the **interface surface** (what consumers see) and the **private surface** (what the package's own launchers see).

The `--self` flag on `exec`, `run`, `env`, `shell env`, `shell profile load`, `ci export`, and `deps` switches which surface is emitted:

| `--self` | Surface emitted | Use case |
|----------|----------------|----------|
| off (default) | Interface surface — vars where `has_interface()` is true | Human or CI script using the package |
| on | Private surface — vars where `has_private()` is true | Generated launchers invoking `ocx launcher exec` internally |

Generated launchers force `self_view = true` internally; they do not expose `--self` to callers.

## Composition Order {#composition-order}

When multiple packages contribute to an environment (via `ocx run -g GROUP1,GROUP2` or `ocx exec PKG1 PKG2`), env entries are **prepended** — the last tool walked has its `PATH` entries placed **first** in the resolved `PATH`. In `-g` argument order, groups listed **later** win PATH lookup.

For `ocx run`, the full order rule is:

> First by group-selection order (the order of `-g` flags, after `all` expansion, deduplicated); then alphabetical by binding name within each group.

See [In Depth — Project Toolchain → Composition order rule][in-depth-project-composition] for the worked example with `-g ci,all,release`.

<!-- external -->
[volta]: https://volta.sh/

<!-- commands -->
[cmd-add]: ./command-line.md#add
[cmd-exec]: ./command-line.md#exec
[cmd-install]: ./command-line.md#install
[cmd-lock]: ./command-line.md#lock
[cmd-pull]: ./command-line.md#pull
[cmd-remove]: ./command-line.md#remove
[cmd-run]: ./command-line.md#run
[cmd-upgrade]: ./command-line.md#upgrade

<!-- environment -->
[env-ocx-global]: ./environment.md#ocx-global
[env-ocx-project]: ./environment.md#ocx-project

<!-- internal -->
[user-guide-global]: ../user-guide.md#global-toolchain
[in-depth-project-composition]: ../in-depth/project.md#running-composition-order
